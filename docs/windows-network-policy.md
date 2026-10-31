# Windows network policy — #1944 slice 1

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
cover real async requests and no-follow behavior. Windows acceptance must use the
existing isolated hosted TLS/root/static-proxy/PAC fixture with both negative
(untrusted CA and bundled-root/direct legacy) and positive controls and retained
CONNECT receipts. Crate-only core Windows tests must use the production builder.
No hosted evidence or installed-network acceptance is claimed by this design.
Under the current networking-only scope there is no VM 150 install/update gate.

Future slices decide per-request proxy refresh for multi-origin clients,
WebSocket CONNECT plus OS trust (both T3 stacks), and typed WinHTTP/SChannel
error classification. They must not infer support from feature unification.
