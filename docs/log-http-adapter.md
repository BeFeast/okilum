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

## Integration requirement

This packet exports the HTTP adapter only. The parent remote-file owner must
connect the URL-opening action, follow worker lifetime/cancellation, byte-to-log
snapshot assembly and visible connection status. The shipped Kibana UI is not
a generic remote-file UI and is unchanged. Consequently this packet alone does
not complete the interactive acceptance of #1390 or its SFTP acceptance.

## Verification

`http/protocol.rs` has dependency-free cursor/range/backoff regression tests.
`http/tests.rs` uses a loopback mock HTTP server, never a production endpoint:
it proves a partial body disconnect retains the cursor, backoff sends nothing,
recovery delivers appended records once, EOF remains idle, and ignoring Range
stops before downloading a full body. HTTPS trust/proxy behavior is inherited
from `okilum-http`; live TLS and GUI acceptance remain for the parent.
