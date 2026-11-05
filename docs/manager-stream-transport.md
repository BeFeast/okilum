# Native Manager stream transport (#1787, #2519)

The native Manager uses T3's orchestration protocol v2. Linux mounts it today;
macOS compiles and tests the same implementation under `cfg(native_manager)`.
The Mac runtime still mounts WKWebView until #2522. Its existing session and
Board/Projects cookie transport remain unchanged (#2520 owns that migration).

## Authentication

MCP authorization and native-client pairing are separate audiences. T3's
`EnvironmentAuth.rejectMcpClientAudience` rejects MCP sessions outside `/mcp`;
`McpOAuth.test.ts` explicitly expects HTTP 401 from
`POST /api/auth/websocket-ticket` for an MCP OAuth token. The same rejection
strings are present in installed nightly `0.1.4-nightly.20261027.3783`.
The audited contract export is upstream `71a283aed2` (22 October); the binary
check corroborates the audience restriction, not every wire schema. Live
end-to-end acceptance and ongoing contract monitoring remain separate (#1789).

`NativeToken::pair` explicitly exchanges a one-time native pairing credential
at `/oauth/token` (RFC 8693), requesting only `orchestration:read` and
`orchestration:operate`. A server-narrowed read-only grant works. An unexpected
scope expansion is refused. This is not an automatic upgrade of an MCP
runtime-mode ceiling: the future native connection UI must explain the new
client's authority before pairing. No OAuth token is converted or overwritten.

Only the resulting native bearer belongs in OS credential storage. Its key is
the normalized origin's `/ws`, separate from MCP's `/mcp`; the payload includes
the origin and expiry. Pairing secrets and socket tickets are transient.
Expiry or a 401 produces an explicit error without opening a browser, retrying
authorization, or silently falling back to another credential. HTTP redirects
are refused and transport errors omit peer bodies and credential-bearing URLs.

## macOS pairing and Keychain (#2519)

`client_os` in the native pairing exchange comes from the Rust **build target**
(`linux`, `macos`, or `windows`), including cross-compiled builds. It does not
inspect the build host or report Linux for a Mac client.

The selected native Manager uses the Linux pairing lifecycle on macOS:
restore an existing native token, or display the pairing prompt; paste the
one-time T3 native pairing token and choose **Pair**. T3 exchanges it for a
separate read/operate client. Okilum saves the returned bearer before installing
the native connection. A failed Keychain write leaves the client disconnected
with an explicit save error; it does not fall back to a session-only bearer.
Reconnect reads that saved token. Expired/wrong-origin native tokens remain
visible errors requiring explicit pairing.

Both credential types use GPUI's platform credential API: `StoredToken` uses
`Environment::credential_url()` (`/mcp`), and `NativeToken` uses the same
origin's `/ws`. GPUI maps this API to macOS Security.framework Keychain items.
The existing MCP authorization writer remains shared with Linux; native pairing
writes only `/ws` and removes only its consumed `/ws/pairing` enrollment.
Sign out removes MCP and native bearer items on both native platforms. Pairing
does not read, convert, clear or revoke the WKWebView cookie session. A Mac with
only an old web session therefore needs explicit native pairing; that web
session continues to work independently. Tokens never enter project files,
preferences, the rebuildable index, diagnostic output or a socket URL.

`ProjectView::threads.enabled` is the native Manager selection. Retained Mac
views leave it false while WKWebView is mounted: they skip MCP Keychain reads
and do not automatically restore native credentials. When #2522 selects the
native panel, it must enable that same selection at construction; connection
completion and activation then restore the native token, and a missing token
opens the existing native pairing controls. No hidden native sign-in is started
behind the current Mac panel. Hosted tests explicitly select this future native
path; this issue does not switch the shipping panel.

The signed shipping bundle is `com.befeast.okilum` (Sprint has its separate
bundle identity). New items written by that app normally need no access prompt.
If macOS asks to access an existing item, the user sees **Okilum** requesting
Keychain access: choose **Always Allow** to remember trust for the installed
signed app, rather than **Allow** for just one access. With that remembered
trust and an unlocked login Keychain, subsequent restores need no further
access prompts (at most one initial trust prompt per installation). A locked
Keychain, a denied request, or a changed signing identity is an OS recovery
condition; Okilum does not bypass it or repeatedly authorize in the background.
The prompt's app identity comes from the signed bundle, not a token label.

Hosted GPUI unit tests use an injected in-memory credential-store double;
GPUI's stock test platform discards writes and always returns no credential.
They do **not** certify the runner's login Keychain or interactive access-dialog
behavior. They execute the production pairing/restore lifecycle, including
write failure, save-before-connect, independent MCP/native keys, one-time
credential cleanup, native restore selection and the missing-token prompt.
Stable signed-bundle Keychain ACL/dialog acceptance remains native tester
verification; no real user credential is used by CI.

## Subscription and projection ownership

Run `NativeClient::subscribe` and `ThreadSubscription::receive` on a background
executor. A subscription obtains a fresh, short-lived ticket through authenticated
HTTP, then upgrades `/ws?orchestrationProtocol=2&wsTicket=…`. HTTP 426 is an
explicit protocol incompatibility. The bearer never goes into the socket URL.
Native Manager directory reads also use WS v2 `orchestration.subscribeShell`.
The full directory is received before client-side project filtering. Both the
message budget and the maximum individual frame are bounded to 64 MiB, so an
unfragmented directory above the library's default 16 MiB frame limit can load.
Shell snapshots marked `resolvedRepositoryIdentityRoots` enrich project metadata
only: their empty thread arrays do not replace the scoped directory or advance
its structural sequence. An authoritative empty snapshot still clears threads.

The transport holds one live Effect RPC `orchestration.subscribeThread` request.
It handles protocol and WebSocket keepalives and acknowledges every accepted
chunk. `receive` validates the entire chunk before invoking its callback. That
callback must apply the batch atomically to its retained projection; only success
advances the resume cursor and sends ACK. A failed ACK after successful apply
retains the new cursor, while a rejected or malformed batch closes the connection
without advancing it. There is no hidden worker or unbounded queue.

Keep the opaque `ResumeCursor` only as long as its projection exists. It is bound
to origin and thread. Reconnect passes it as `afterSequence`; T3 supplies bounded
replay or replaces it with a snapshot. Duplicate events are suppressed, sequence
gaps are allowed, unknown domain-event payloads are retained for the reducer,
and a fresh snapshot may reset the sequence backwards. History cursors remain
opaque and `hasMoreHistory`/payload-budget flags remain visible. Compact
snapshots are not requested; reconstruction and older-history loading belong
to the projection slice.

Dropping/closing the subscription cancels that RPC, not the remote agent.
Implicit drop only shuts down the socket; it never writes protocol frames or
waits for the peer. Explicit `close` sends Interrupt/Close and must run off the
UI thread.
`CancelSubscription::cancel` shuts down a blocked socket immediately; the worker
then exits. The caller owns reconnect policy, view-generation checks and task
lifetime. No commands, send retries, approvals or question responses are added
in this slice.

## Verification

Loopback tests exercise real reqwest requests and WebSocket handshakes, not a
mock transport: pairing/scopes, separate keychain payloads, refused credentials,
required protocol query, multi-chunk ACKs, opaque history, duplicate/unknown
events, reconnect and snapshot replacement, atomic rejection, malformed batches,
immediate cancellation, keepalives and incompatible upgrades. No installed app
or live credential is used.

## Native Tier 1 actions (#1787, slice 4)

Linux's explicitly paired native client sends `orchestration.dispatchCommand`
over a separate bounded WS v2 RPC socket. It does not block the live subscription
or fall back to MCP when a native request fails. The macOS webview is unchanged.
Send starts an idle manager and queues behind an active run. Queue is explicit;
Steer is offered only when the active provider session advertises active steering,
and pins that run's identity. Interrupt also targets that exact run and holds its
queue. Runtime mode and model selection are inherited, never expanded by a send.

Pending approvals show the server's offered decisions (or Allow once/Decline
when options are absent). Questions support single/multiple choices, custom
answers where allowed, and message-mode dismissal. A resolved, inherited or
non-resumable request cannot grant a response control. Unsupported details,
attachments and all non-Tier-1 features remain in Open in T3.

Before network dispatch, each complete command is atomically published and synced
under the application's durable `t3-native-commands` directory, scoped by server
origin and bound thread. The earlier MCP message journal is preserved separately.
Lost responses retain the same command/message/target identities. Only explicit
Retry same action resends them; acknowledged commands return their saved sequence
without another dispatch. Acceptance is not completion. Reconnect loads retained
commands but never sends one automatically. Disk preparation failures keep the
composer draft. Action drafts and composer keystrokes notify their own entities without
rebuilding the retained history model. Inline request inputs may make GPUI
repaint the virtual list's visible rows; offscreen history stays unrendered.


### Interaction recovery (slice 7)

Request controls live in their matching local history row. Runtime capability,
request kind and source identity remain the authority; inherited/resolved or
ambiguous cards cannot grant controls. Approval context retains both prompt and
reason, advertised decisions and warnings. Missing approval options permit only
Allow once/Decline. Secret requests are not ordinary answer fields.

Live questions retain exact option values and multi-selection arrays. Message-mode
requests require strings on the server; multiple selected values become JSON-array
**text**, preserving unambiguous values without sending an unsupported array.
Pending answer drafts survive an unsynchronized reconnect and are cleared only by
confirmed resolution. A retained response disables further answers until projection
confirms it. Stop pins its run and holds queued work.

A stable per-thread file lock serializes durable preparation and receipt publication
across windows. Respond/Dismiss share one request identity; Stop has one identity
per run. Repeated clicks reuse the immutable operation, while a conflicting answer
fails visibly. An uncertain message blocks fresh sends until explicit retry; retry
keeps its command/message/thread/run IDs, including after reconnect. Acknowledged
commands return their saved receipt without network dispatch. Resolved request
and stale Stop retries are disabled. Message retries retain their original target
even when the active run changes, allowing server idempotency readback without
turning an old Steer into a new message. Missing or malformed receipts fail closed.

## Time-to-first-message diagnostics (#2518)

The native Manager records one `manager_ttfm` JSON event per panel open in
`reader-diagnostic.log`. All millisecond fields are monotonic offsets from the
same **panel open** (including asynchronous descriptor/binding lookup):

- `connect_start_ms`: descriptor/connect start, currently zero.
- `auth_ms`: Manager subscription's authenticated WebSocket-ticket HTTP reply.
- `connect_ms`: that same socket's WebSocket upgrade completed. Ticket auth
  precedes the upgrade in this transport; it is not a later WebSocket handshake.
- `shell_ms`: first accepted `subscribeShell` directory snapshot.
- `thread_ms`: first accepted `subscribeThread` Manager snapshot, independently
  of the later synchronized marker.
- `render_ms` and `open_to_first_message_ms`: first message row's visible paint
  callback, excluding virtual-list row construction/measurement and activity rows.
- `reused_connection`: the open reuses an existing paired `NativeClient` and
  retained feed. Closing currently pauses both subscriptions, so `true` does
  **not** claim that a physical WebSocket survived the close.

Shell and Manager subscriptions run in parallel; `shell_ms` need not precede
`thread_ms` or even the first paint. On a cold open, emission waits for both
snapshots while preserving the original paint timestamp. A warm open can paint
retained history immediately; phases not observed during that open are explicit
JSON `null`, never invented zero timings or timestamps from the previous open.
Each reopen has a fresh clock. Further frames do not log again; closing cancels
unfinished measurements and stale paint callbacks.

A cold open whose Manager snapshot never arrives emits no successful TTFM line.
An empty snapshot also cannot complete TTFM until a message actually paints.
The existing asynchronous diagnostic writer does disk I/O. This instrumentation
adds no RPCs and records only timings and the reuse flag: no tokens, origins,
thread identities or message content. Compare measurements on the same machine
in the same session; Linux and macOS measurements should each use their own
before/after pair (native macOS mounting follows #2522).
