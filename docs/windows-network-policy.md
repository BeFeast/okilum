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

T3 and Board use `okilum_http::local_network_builder`, which also sends a
private IP literal direct: RFC 1918, link-local, the CGNAT block Tailscale uses
(`100.64/10`), IPv6 unique-local/link-local and IPv4-mapped forms. That is the
same set T3 already accepts over plain HTTP. The reason is WinHTTP's bypass
rule: `<local>` ("bypass proxy for local addresses") matches only host names
without a period ([WinHttpOpen](https://learn.microsoft.com/en-us/windows/win32/api/winhttp/nf-winhttp-winhttpopen),
[netsh winhttp](https://learn.microsoft.com/en-us/windows/win32/winhttp/netsh-exe-commands)),
and `windows_proxy::bypasses` implements exactly that. An origin such as
`http://10.10.0.23:3773` or a tailnet `http://100.x.y.z:3773` therefore went to a
static corporate proxy (pre-#1944 too, via hyper-util's registry proxy) and,
with this slice, to whatever a PAC file answers. Such a proxy cannot route to
the user's home LAN or tailnet. Host names (for example a LAN name that resolves
to `10.x`) keep the system/PAC decision, because the DNS answer is not known
when the client is built. Feedback, updater and remote log keep the plain
`builder` (loopback only); macOS/Linux behaviour is unchanged for all of them.

Other origins keep the slice-1 fail-closed PAC behaviour: the gateway, flow or
reader is not constructed and the existing error path reports it. Long-lived
gateways resolve PAC once; reconnecting the T3 environment refreshes it.

The shared builder's 5-second connect default applies on every platform, so
each migrated client sets its own pre-#1944 connect budget explicitly (Board
issue reads: connect bounded only by their 10-second total).

`crates/okilum-http/tests/client_inventory.rs` pins this. It scans every
production source in `crates/` for raw `reqwest`/`tungstenite` client
construction and fails on any site not listed with a reason, or on a listed site
that disappeared. It also asserts that each migrated path still calls the shared
builder with its own connect budget, that T3/Board use the local-network
policy, and that `okilum-core`, `okilum-t3` and `okilum-shell` depend on
`okilum-http` outside dev-dependencies.

### Recorded exceptions

Deliberately outside the policy:
- Syncthing REST (`okilum-sync`), loopback only, `no_proxy`.
- Sync pairing (`okilum-sync-controller/pairing.rs`), pinned root, direct.
- macOS sidecar connect, loopback only, macOS only.
- AppImage updater, Linux only.
- `okilum-brain` (model providers, Todoist, retrieval, second T3 stack), linked
  into `okilum-cored`, not into the Windows package.

- `okilum-brain`'s second T3 stack keeps webpki roots and a direct socket.
  okilum-t3 now enables tungstenite's `native-tls` feature on Windows, so a
  Windows build that also links `okilum-brain` would see that feature too; its
  `client_tls_with_config(..., None)` would then pick SChannel through
  unification. It does not ship on Windows; if it ever does, it must adopt the
  slice-3 socket path below instead of relying on that.

## Slice 3: multi-origin clients, T3 WebSocket, typed causes

### Multi-origin clients: per-request proxy

| Client | Builder |
|---|---|
| Paste page title (`note_paste/fetch.rs`) | `per_request_builder` |
| Paste image download (`note_paste/download.rs`, one client for every host) | `per_request_builder` |
| Project logo (`okilum-t3/projects/icon.rs`) | `local_network_per_request_builder` |
| Board delivery feeds (`okilum-t3/board/delivery/read.rs`) | `local_network_per_request_builder` |

These clients reach hosts that are unknown when the client is built and may
follow redirects to another host. On Windows the builder installs a
`reqwest::Proxy::custom` hook; reqwest calls it for every connection,
redirect hops included. The hook runs the same WinHTTP lookup as the
single-origin builder (static, per-protocol, PAC, WPAD; system bypass list;
`NO_PROXY`) for that request's own origin and caches the answer per
`scheme://host:port` for five minutes. That is also how a network change
(VPN on/off) reaches the long-lived paste client. A failed lookup is cached for
30 seconds. An environment proxy (`HTTP(S)_PROXY`, `ALL_PROXY`) keeps reqwest's
own precedence, as before. TLS is SChannel, selected explicitly.

A discovery failure sends that one request direct and logs the cause without
the URL. That is fail-open, unlike the single-origin clients: these are
best-effort previews (title, image, logo, feed), reqwest's hook cannot return
an error, and browsers fall back to direct when a PAC script cannot be
fetched. Each client keeps its own User-Agent, connect budget, total budget
and redirect limit. Board delivery keeps 30 s, the reqwest blocking default it
always had.

### T3 WebSocket: system proxy and OS trust

`stream.rs` asks `okilum_http::local_network_socket_route` for the route.
On Windows that is the same decision as the ticket request's
`local_network_builder`: loopback and private IP literals direct; otherwise
the environment proxy, then the WinHTTP static/per-protocol/PAC/WPAD answer.
PAC sees `https://host/ws` only; the ticket query is stripped before
discovery. Discovery errors fail closed, as for the ticket request. For a
proxy route the client connects TCP to the proxy, sends `CONNECT host:port`
(no credentials), requires a 2xx reply and reads it byte by byte, so no byte
of the TLS handshake is consumed. TLS then runs end to end with T3 through the
tunnel: the proxy never sees the ticket or frames. Only `http://` proxies can
tunnel; a SOCKS or HTTPS proxy is refused with a proxy error, never bypassed.

TLS is chosen in code: on Windows `tungstenite::Connector::NativeTls`
(SChannel, Windows trust store, so a corporate TLS-inspection root is
trusted), enabled by okilum-t3's own Windows-only `native-tls` feature.
macOS/Linux keep rustls with bundled webpki roots and a direct socket.

### Typed causes

`okilum_http::classify` walks an error chain and returns `Certificate`,
`Proxy`, `ProxyAuthentication`, `Timeout` or `Connection` by type: reqwest
status/timeout, `io::Error` kinds and WinHTTP/Winsock/SChannel/CryptoAPI codes,
`TunnelError` and `ProxyDiscoveryFailed`. The remaining text fallback (TLS
libraries and hyper report certificate and tunnel failures only as text)
skips each `reqwest::Error`'s own message, which names the URL, so a host
called `ssl-gw.corp.example` no longer turns a refused connection into "The
TLS certificate is not trusted". `okilum_http::reason` uses it.

T3 native errors gain `StreamError::Network(cause)` with a T3-specific message
(untrusted certificate, proxy, HTTP 407, timeout). It is retried like
`Unreachable`, which remains for DNS failure and refusal. The ticket request,
proxy discovery, TCP connect, tunnel and TLS handshake all map through it.

NTLM/Kerberos (407) remains unsupported for every client: the message says
so, and no OS credentials are read or sent.

### Verification

Linux-run unit tests: a redirect from a proxied origin to a direct one (each
hop takes its own route, checked at both listeners), per-origin cache and
failure caching, private literals never reach discovery, CONNECT request and
byte-exact handoff, 407/403/malformed replies, header injection, route
selection and refusal of non-HTTP proxies, typed classification with a
URL-name control, T3 stall → `Network(Timeout)`. `client_inventory.rs` pins
the four per-request builders with their connect budgets and the WebSocket's
route, tunnel and SChannel markers, and that okilum-t3 enables tungstenite's
`native-tls` on Windows itself. No hosted TLS-inspection or installed-network
acceptance is claimed by these tests; the corporate fixture extension remains
part of the nightly full gate as described for slice 1.

None of these clients infer support from Cargo feature unification.
