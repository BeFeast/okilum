# Manager onboarding (#2653, variant B)

The Manager panel and Settings → T3 share one origin-bound connection flow.
The normal path is Sign in to T3 → approve in the browser → Connect manager →
Create link → Copy → return to Okilum. T3 itself is unchanged.

Connect manager opens `<configured-origin>/settings/connections`. Only that
explicit action arms clipboard inspection, and inspection runs when an Okilum
window becomes active. A copied `<same-origin>/pair#token=…` is exchanged once
for `orchestration:read orchestration:operate`. Scheme, host and effective port
must match; foreign origins, unrelated paths and ambiguous fragments are ignored.
A bare code has no verifiable origin, so only Details accepts it manually.
Clipboard content is compared again before clearing; unrelated/newer content
is retained. The pair link is never saved or included in errors/Debug output.
Only the resulting native credential is saved in the OS credential store, under
the origin's `/ws` key, independently of the MCP OAuth `/mcp` credential.

Each connection state has a plain sentence and one primary action. Waiting and
connecting disable sends; transient live-stream failures retain history/drafts
and retry automatically, with Retry now available. Expired credentials offer
Reconnect rather than another MCP sign-in. Thread ids, server URL, local-time
expiry and safe last errors live behind Details, alongside Copy details and the
manual link/code fallback. Connected returns to normal chat.

Protocol evidence was checked read-only against installed T3
`v0.1.7-nightly.20261031.4004` (`54c5f2524`):

- `apps/web/src/routes/_chat.settings.connections.tsx:8` declares the pathless
  `_chat` layout's `/settings/connections` route.
- `apps/web/src/components/settings/pairingUrls.ts:4` sets `/pair` and delegates
  fragment encoding to `packages/shared/src/remote.ts:167` (`#token=…`).
- `apps/server/src/auth/PairingGrantStore.ts:254` defines the optional manual
  pairing code as 12 characters from `23456789ABCDEFGHJKLMNPQRSTUVWXYZ`.
- `apps/server/src/auth/EnvironmentAuth.ts:953` narrows an exchanged credential
  to the requested scopes.
- The installed `t3 auth pairing create --help` confirms `--base-url` produces
  a ready `/pair#token=…` link. Production code never invokes that CLI.

Hosted tests cover the state mapping, same-origin recognition and foreign-link
controls, duplicate activation, conditional clipboard clearing, scope narrowing,
OS-store failure, native/MCP isolation and redacted failure diagnostics. Native
beta verification remains a separate acceptance step.
