# SFTP log adapter (#1390): bounded implementation slice

`okilum_core::log::sftp` implements read-only SFTP v3 using the existing `ssh`
executable, SSH config aliases and ssh-agent. It invokes the SFTP subsystem,
never a remote shell command. The only file operation is OPEN with SSH_FXF_READ
and positional READ; ending a session drops its handle and kills/reaps its owned
SSH child. No credential/configuration files are written. Batch authentication,
existing known-host trust and connect/response deadlines are explicit.

## Integration requirement

The parent must connect `Target::parse` and `Follow<OpenSsh>` to its remote-source
open/follow worker. This slice does not add a menu, URL routing, native window,
or change the independent HTTP/Kibana adapters. Call `poll` outside the UI thread;
the caller owns retry/backoff and cancellation between polls. Publish
`Disconnected` while retrying, preserve displayed rows and append only `Records`.
`Pending` means that bytes arrived without completing a newline-delimited record;
`CaughtUp` means this read reached EOF. Records are raw bytes, with UTF-8 split
across reads preserved; normal log parsing remains the consumer's responsibility.

Each poll opens the current path, verifies the entire previously consumed prefix
and then reads at most 32 KiB of growth. Failure before receiving a complete reply
leaves the cursor and pending record untouched. Prefix verification also handles
short SFTP reads. The cursor includes partial records, so they are neither lost
nor emitted twice across retries. All remote reads in a poll use the same handle.

## Explicit limits and unfinished acceptance

This is a conservative implementation slice, not completion of #1390:

- A changed consumed prefix (rotation, truncation or rewrite) produces terminal
  `ContinuityLost`. Keep existing rows and require explicit restart. SFTP v3 has no
  portable file identity and cannot recover an unread rotated-away file. Identical
  consumed prefixes cannot distinguish generations; concurrent rewrites during a
  poll are not a consistent snapshot. Seamless rotation with an independently
  proven no-loss oracle remains unimplemented.
- The adapter retains at most 8 MiB of consumed source bytes in memory and then
  returns `LimitReached`; it never silently evicts the proof or skips records.
  The consumer must show incomplete/stopped state. There is no disk cache.
- Reopening and rereading the prefix on every poll is deliberately conservative
  and expensive. This is not a large-log/performance acceptance result. Each
  response has a 15-second deadline, not an aggregate poll deadline. Poll-level
  cancellation, scalable continuity proof and persistent connections remain work.
- URLs accept an SSH config alias and absolute, optionally percent-encoded path.
  Userinfo, explicit ports and IPv6 literal hosts are not accepted; use SSH config
  aliases for custom users, ports, proxies and IPv6. Trust must already exist.
- Local tests use an injectable transport and byte fixtures only. Actual OpenSSH
  subsystem execution, homelab disconnect/reconnect, native disconnected display,
  full crate compilation and platform acceptance have not been run.

## Desktop source flow

Logs → **SFTP logs…** opens a vault-scoped source window. Enter
`sftp://host-alias/absolute/path/app.log`, select Follow or a finite snapshot,
then Start. Opening the window does not connect. Authentication and known-host
trust remain in existing OpenSSH configuration; the UI never asks for a password
or changes host keys. Inputs and loaded rows are session-only.

The worker feeds complete lines into the shared leased stream assembler and the
ordinary LogView, with local filters, selection and export. History stops at
8 MiB / 20,000 lines or a 64 KiB line. Errors and continuity loss retain the
previously loaded rows. Retry/rotation recovery starts a new session explicitly;
there is no automatic retry or gap-free rotation claim.

Stop and window close set a session-specific cancellation flag. The worker checks
it before every positional operation, including each prefix proof read; it does
not finish an entire multi-read poll after cancellation. An already running
OpenSSH exchange still waits for the connector's existing response deadline.
The UI shows Stopping until actual cleanup completes. Persistent connections,
scalable prefix proof and interruption inside an active exchange remain open.
Offline fake/UI tests are separate from real SSH/native acceptance.

## Lightweight regression command

No Cargo graph, SSH connection or daemon is needed:

```sh
rustc --edition 2021 -D warnings -C linker=/usr/bin/gcc --test \
  crates/okilum-core/src/log/sftp.rs -o /tmp/okilum-1390-sftp-tests
/tmp/okilum-1390-sftp-tests
```

The nine controls cover dropped growth replies, failed prefix verification,
short reads, split UTF-8, rotation/truncation/same-length and interior rewrites,
explicit history limits, URL/command isolation and bounded SFTP response framing,
IDs, DATA lengths, EOF and failure distinction. These are synthetic evidence,
not homelab/native acceptance. The full core crate is left to the parent's
canonical shared builders.
