# HTTPS log file adapter (#1390)

`okilum_core::log::http::HttpLog` is a read-only adapter for an append-only
HTTPS resource. It is independent of Kibana and does not implement SFTP.

A background owner calls `open` once and `poll` on its normal follow tick.
Each successful poll commits and returns at most 1 MiB of contiguous source
bytes. The owner must retain incomplete UTF-8/record fragments across polls,
append only returned bytes, and display `status()` even when no bytes arrived.
No source settings, body cache or credentials are written by the adapter.

Every request uses a bounded Range and identity encoding. A `200` response
stops with `RangeUnsupported` before consuming the body. A `206` must contain
an exact contiguous Content-Range and complete body; a `416` is idle only when
its total equals the committed cursor. Shrinkage stops explicitly. Redirects,
authentication failures and malformed ranges stop; none trigger a full download.
Transport/body interruptions, HTTP 429 and 5xx expose `Disconnected` with
1/2/4/8-second backoff. A fifth consecutive failure stops until explicit reopen.
A successful response resets the budget. Polling during backoff sends nothing.

The contract assumes append-only content. Same-size replacement or a rewrite
of already consumed bytes cannot be detected reliably without a server-side
append identity contract; file rotation reconciliation is not implemented.
Ordinary ETags are intentionally not pinned: appending changes a resource's
ETag and must not turn every legitimate append into a full-response If-Range.

## Desktop flow

Settings → Logs → HTTPS log files opens one vault-scoped session window.
Source name and a public HTTPS file URL can be saved under the existing durable
application state directory, partitioned by vault path. Saving/selecting a source
does not connect. URL credentials, query tokens and fragments are refused; this
flow has no authentication or credential-storage fallback.

Start explicitly opens the applied source on one background worker. Stop, source
replacement and window close prevent further polling or stale replies. Waiting is
cancelled immediately; an in-flight request finishes within the adapter's existing
10-second request deadline. It is not claimed to abort network IO immediately.
Disconnected/backoff and terminal Range/source-change errors remain visible.
Previously loaded rows remain visible after Stop or a terminal error.

The shared `log::stream::Assembler` joins transport byte chunks, including split
UTF-8 and CRLF, and publishes only newline-terminated valid UTF-8. It bounds raw
retention to 8 MiB, 20,000 physical lines and 64 KiB per physical line; each chunk
is at most 1 MiB. A bound/encoding error stops rather than silently evicting rows.
An unfinished final line is counted as pending, never presented as complete.
Assembler storage and immutable index snapshots lease the existing process-wide
log memory pool. Pool exhaustion is explicit. Event bytes stay in session memory;
only an explicit existing LogView export writes user-selected event output.

`LogView::stream` / `replace_stream` use the existing virtual rows, fields, details,
local filters, histogram, raw copy and export. Scope identifies one stream session;
record byte offsets preserve selections/marks across appends. This interface is
transport-independent; it does not implement Kubernetes/SFTP process ownership.

This completes the HTTPS desktop flow, not SFTP or all of #1390. Same-size source
rewrites remain undetectable under the adapter's append-only contract. Native
clipboard/visual and real HTTPS endpoint acceptance remain separate evidence.

## Verification

`http/protocol.rs` has dependency-free cursor/range/backoff regression tests.
`http/tests.rs` uses a loopback mock HTTP server, never a production endpoint:
it proves a partial body disconnect retains the cursor, backoff sends nothing,
recovery delivers appended records once, EOF remains idle, and ignoring Range
stops before downloading a full body. HTTPS trust/proxy behavior is inherited
from `okilum-http`; live TLS and GUI acceptance remain for the parent.
