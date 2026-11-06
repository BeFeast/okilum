# Manager onboarding (#2653, variant B)

The Manager panel and Settings → T3 share one origin-bound connection flow.
The normal path is Sign in to T3 → approve in the browser → Connect manager →
Create link → Share → Copy link → return to Okilum. T3 itself is unchanged.

Connect manager opens `<configured-origin>/settings/connections#authorized-clients`.
In the installed T3 release the legacy anchor redirects to `/settings/access`
and reveals Authorized clients; on versions with an unsplit Connections page it
selects that same section. Only that
explicit action arms clipboard inspection, and inspection runs when an Okilum
window becomes active. A copied `<same-origin>/pair#token=…` is exchanged once
for `orchestration:read orchestration:operate`. Scheme, host and effective port
must match; foreign origins, unrelated paths and ambiguous fragments are ignored.
A bare code has no verifiable origin, so only Details accepts it manually.
Clipboard content is compared again before clearing; unrelated/newer content
is retained. The pair link is never saved or included in errors/Debug output.
On Wayland, keyboard focus can arrive before the clipboard Selection offer.
Activation therefore allows the platform event pump to finish and retries for
at most 750 ms, only while the window is active and the same explicit Connect
request is still armed. Cancelling, starting another flow or closing the window
invalidates the pending read. The focus regression delivers the link after the
activation callback, matching this native event ordering.
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
  `_chat` layout's `/settings/connections` route; `settingsSearch.ts:1221,1228`
  redirects its `authorized-clients` anchor to `/settings/access`. The route at
  `routes/_chat.settings.access.tsx:8` renders `ConnectionsSettings` with access
  controls (`ConnectionsSettings.tsx:288,1166`).
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

## CLI fallback (#2523)

Settings and Manager Details share a Copy command control for the configured
server. The supported `t3 auth pairing create` invocation creates a five-minute,
one-time grant limited to orchestration read/operate, printing a ready pair link.
Run it on the T3 host in bash/zsh, under the server account and matching data
directory (`--base-dir` for a custom installation). The user explicitly pastes
its output in Details; Okilum does not execute a shell or mint a server token.
The URL is shell-quoted, including IPv6 and non-default ports. Normal onboarding
continues to use the browser flow above.

One shared authorization for both MCP and native transport is not implemented:
MCP tokens remain rejected outside `/mcp`, and their scopes cannot mint native
pairing grants. An app-only change must not elevate that token. The existing
first-run and browser grant flow are shared across projects; that does not claim
that the two server credential audiences are unified. The server dependency is
tracked explicitly in [#2673](https://git.oklabs.uk/BeFeast/okilum/issues/2673).
