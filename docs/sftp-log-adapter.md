# SFTP log adapter (#1390)

`okilum_core::log::sftp` implements read-only SFTP v3 using the existing `ssh`
executable, SSH config aliases and ssh-agent. It invokes the SFTP subsystem,
never a remote shell command. The only file operations are OPEN with
SSH_FXF_READ, positional READ, FSTAT (file size) and CLOSE; ending a session
kills and reaps its owned SSH child. No credential/configuration files are
written. Batch authentication, existing known-host trust and connect/response
deadlines are explicit.

## Follow contract

`Follow<T: Transport>` is polled outside the UI thread; the caller owns
retry/backoff and cancellation between polls. `Records` holds complete
newline-terminated records as raw bytes (UTF-8 split across reads preserved),
`Pending` means bytes arrived without completing a record, `CaughtUp` means a
read reached EOF, `Disconnected` means the connection failed and the cursor is
unchanged.

- **One connection.** A follow keeps one SSH/SFTP connection and one handle
  while they stay healthy. Growth is read at the cursor, at most 32 KiB per
  request, together with the 4 KiB of consumed bytes in front of it; a
  mismatch in that overlap (truncation or rewrite in place) is
  `ContinuityLost` in the same request, so a new generation is never emitted
  as growth. Each byte of the file is transferred about once.
- **Rotation.** At EOF the next poll closes the handle and opens the path again
  on the same connection, then re-reads the retained proof window: the last
  64 KiB of consumed bytes plus any unfinished record. A file rotated away,
  truncated or rewritten inside that window is `ContinuityLost`; existing rows
  stay and a restart is explicit. SFTP v3 has no portable file identity, so an
  unread rotated-away file cannot be recovered, and a rewrite of older bytes
  outside the window is outside the append-only contract (the HTTPS adapter
  checks no consumed bytes at all).
- **Reconnect.** After a failure the transport is dropped; the next poll opens
  a new connection and proves the same window before reading on, so no record
  is duplicated or lost, including the rest of a partial record.
- **Tail start.** `Follow::tail(n)` begins at the first full line of the last
  `n` bytes when FSTAT reports a larger file (one byte earlier is read: if it
  is a newline, nothing is cut). The desktop flow uses 1 MiB, so a large log is
  followed instead of filling the loaded-history budget with old lines.
- **Bounds.** Memory is the proof window plus the unfinished record; a record
  without a newline beyond 8 MiB is `LimitReached`, never evicted or skipped.
  Each SFTP response has a 15-second deadline.
- URLs accept an SSH config alias and an absolute, optionally percent-encoded
  path. Userinfo, explicit ports and IPv6 literal hosts are refused; use SSH
  config aliases for custom users, ports, proxies and IPv6. Trust must already
  exist.

## Desktop source flow

Logs → **SFTP logs…** opens a vault-scoped source window. Enter
`sftp://host-alias/absolute/path/app.log`, select Follow or a finite snapshot,
then Start. Opening the window does not connect. Authentication and known-host
trust remain in existing OpenSSH configuration; the UI never asks for a password
or changes host keys. Inputs and loaded rows are session-only.

The worker feeds complete lines into the shared leased stream assembler and the
ordinary LogView, with local filters, selection and export. History stops at
8 MiB / 20,000 lines or a 64 KiB line. A file larger than 1 MiB starts at a
line in its last 1 MiB; the status line names the start offset. Errors and
continuity loss retain the previously loaded rows.

A dropped connection reconnects automatically once the session has read
successfully. The worker keeps the same `Follow`, so each retry re-proves the
retained window and appends only unseen bytes, including the rest of a partial
record: no duplicate or missing records. While the connection is down the status
reads `Disconnected: … Reconnecting in Ns (retry k of 4)` in the error colour
with Stop enabled, and the loaded rows stay as they are. Backoff matches the
HTTPS adapter (1/2/4/8 s, cancellable by Stop); the fifth consecutive failure
stops with rows retained, and any successful poll resets the budget. A failure
before the first successful read (host alias, trust, authentication) is reported
at once without retrying. Rotation recovery still needs a new session; there is
no gap-free rotation claim.

Stop and window close set a session-specific cancellation flag. The worker checks
it before every positional operation, including each proof read; it does
not finish an entire multi-read poll after cancellation. An already running
OpenSSH exchange still waits for the connector's existing response deadline.
The UI shows Stopping until actual cleanup completes. An exchange already in
flight is not interrupted. Offline fake/UI tests are separate from real
SSH/native acceptance.

## Tests

`sftp/tests.rs` (core, injectable transport): a dropped growth read retries the
exact cursor, partial UTF-8 survives a drop and short reads, rotation,
truncation and copytruncate stop without emitting the new generation, rewrites
inside the proof window are caught on reopen and at the cursor, a 10 MB log is
followed on one connection with each byte read about once, tail start keeps an
exact line boundary, FSTAT and wire framing. `reader_log_sftp/worker_tests.rs`
covers reconnect/backoff, Stop, and the tail start message. These are
synthetic evidence, not homelab/native acceptance.
