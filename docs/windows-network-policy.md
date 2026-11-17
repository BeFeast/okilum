# Windows network policy — #1944

## Slice 1: remote log

The first adoption outside Feedback and Windows updater is remote log/Kibana.
`okilum-core` depends directly on `okilum-http`, so a standalone core Windows
build selects SChannel explicitly rather than inheriting Cargo features from
the shell. `async_builder(endpoint, version, timeout)` resolves the current
user's WinHTTP static/per-protocol/PAC/WPAD proxy for that source endpoint.
Discovery errors fail closed. Environment proxy precedence remains the existing
Feedback policy. The source transport must be recreated after changing network
settings to refresh its resolved PAC policy.

Kibana retains a 3-second connect timeout, 10-second request budget, cancellation,
response cap, sensitive API-key header and refusal of every HTTP redirect.
Because redirects are refused and routes remain on the same endpoint origin,
this slice needs no cross-host proxy cache. UI construction failures expose only
the safe network-policy reason; they do not include source URLs or credentials.
HTTP 407 explains that integrated NTLM/Kerberos proxy authentication is unsupported.
No proxy credentials are read from OS storage or added to requests.

macOS/Linux policy is unchanged. T3 HTTP/WebSocket, project icons/board, paste
fetch/download and What's new remain separate follow-ups; this slice makes no
claim about their proxy support. Sync pairing with pinned roots and explicitly
direct loopback transports must retain their distinct trust boundaries.

## Verification

Existing remote-log request/auth/redirect/cancellation fixtures also assert the
production client's User-Agent and safe HTTP 407 response. Shared builder tests
cover real async requests and no-follow behavior. Corporate-network acceptance in the nightly full gate should extend the
existing isolated hosted TLS/root/static-proxy/PAC fixture with both negative
(untrusted CA and bundled-root/direct legacy) and positive controls and retained
CONNECT receipts. Crate-only core Windows tests must use the production builder.
No hosted TLS-inspection or installed-network acceptance is claimed by these
unit tests. Under the 31 October process v2, this networking-only change has no
extra pre-merge native attempt; full/native checks belong to the Stable night.
Under the current networking-only scope there is no VM 150 install/update gate.

## Slice 2: T3 HTTP, Board and What's new

Every single-origin client in the Windows package now builds through
`okilum_http::builder` for its one origin:

| Client | Origin | Built |
|---|---|---|
| T3 MCP gateway (`McpGateway`) | T3 environment | per gateway |
| T3 session/native gateway (`SessionGateway`) | T3 environment | per gateway |
| T3 OAuth discovery/register/token | T3 environment (discovery refuses other origins) | per flow |
| T3 native pairing and WebSocket ticket (`stream::auth`) | T3 environment | per pairing / native client |
| T3 browser identity (`projects::browser_identity`) | T3 environment | per call |
| Board issues, live reader, issue peek | Forgejo repository origin | per read; live reader per repository |
| What's new notes | `updates.befeast.com` | per load |

`okilum-t3` depends on `okilum-http` directly, so a crate-only Windows test
build selects SChannel and WinHTTP exactly like `okilum.exe`. Each client keeps
its existing User-Agent, connect/request budgets and refusal of every redirect.
The live Board reader drops its client when the repository changes.

A loopback origin (`127.0.0.0/8`, `::1`, `localhost`) is always direct on
Windows and never runs WinHTTP discovery: a corporate proxy cannot route back to
this machine, and a local T3 should not fail because a PAC file is unreachable.
Other origins keep the slice-1 fail-closed PAC behaviour: the gateway, flow or
reader is not constructed and the existing error path reports it. Long-lived
gateways resolve PAC once; reconnecting the T3 environment refreshes it.

`crates/okilum-http/tests/client_inventory.rs` pins this. It scans every
production source in `crates/` for raw `reqwest`/`tungstenite` client
construction and fails on any site not listed with a reason, or on a listed site
that disappeared. It also asserts that each migrated path still calls the shared
builder and that `okilum-core`, `okilum-t3` and `okilum-shell` depend on
`okilum-http` outside dev-dependencies.

### Recorded exceptions

Deliberately outside the policy:
- Syncthing REST (`okilum-sync`), loopback only, `no_proxy`.
- Sync pairing (`okilum-sync-controller/pairing.rs`), pinned root, direct.
- macOS sidecar connect, loopback only, macOS only.
- AppImage updater, Linux only.
- `okilum-brain` (model providers, Todoist, retrieval, second T3 stack), linked
  into `okilum-cored`, not into the Windows package.

Open follow-ups, not covered by slice 2:
- Multi-origin clients: paste page title (`note_paste/fetch.rs`), paste image
  download (`note_paste/download.rs`, one client for every host), project logo
  (`projects/icon.rs`), Board delivery feeds (`board/delivery/read.rs`). They
  follow arbitrary URLs or cross-host redirects, so one per-origin PAC answer is
  wrong for them. They need per-request resolution (for example
  `reqwest::Proxy::custom` with a cached WinHTTP lookup) and a decision on
  fail-open versus fail-closed when that closure cannot return an error.
- T3 WebSocket (`stream.rs`, and `okilum-brain/t3.rs` if it ever ships on
  Windows): tungstenite has no proxy support and uses bundled webpki roots. It
  needs HTTP CONNECT through the resolved proxy and an OS-trust TLS connector.
  Until then the ticket request follows the system proxy and OS trust but the
  socket that spends it does not.
- Typed WinHTTP/SChannel error classification, and mapping
  `StreamError::Unreachable` to the real cause.

None of these may infer support from Cargo feature unification.
