# Linux Manager stream transport (#1787, slice 2)

The native Linux client uses T3's orchestration protocol v2. This slice adds
the transport boundary in `okilum-t3::stream`; the following projection slice
will connect it to the virtualized feed. The current UI still uses MCP. The
macOS embedded web UI keeps its existing session and connection behavior.

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
composer draft. Action drafts and composer keystrokes notify their own entities,
without re-rendering the cached message history.
