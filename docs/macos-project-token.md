# Native project gateway on macOS and Linux (#2520)

Board, Projects, project icon reads/save/reset and read-only thread summaries
share `reader_project_navigation::restore_gateway`. The UI thread reads the OS
credential store and a background task builds the gateway. The native bearer
uses `NativeToken::credential_key` (normalized origin `/ws`); MCP uses the
separate `Environment::credential_url` (`/mcp`). Neither credential is copied
into files, the index, browser storage or another audience.

An explicitly paired native bearer authorizes `/api/projects`, project mutations
and the protocol-v2 `/api/orchestration/shell` snapshot. Every request rechecks
origin, expiry and `orchestration:read`; mutations also require
`orchestration:operate`. Redirects are disabled. A missing native credential
allows MCP fallback. A malformed, expired, refused or insufficient native grant
remains an error; it never authorizes an operation through a broader MCP grant.
The shell does not read or send browser cookies, or probe `/api/auth/session`.
The HTTP contract and project mutation scopes were checked against T3 contract
revision `71a283aed2` (`environmentHttp.ts`, `project/http.ts`, `settings.ts`).

The Mac Manager now mounts the native ProjectView (#2522). The WKWebView,
its website-data-store adapter and focus bridge are removed from the app.
Settings OAuth opens the system browser on every platform. Old browser data
is left untouched; native pairing neither migrates nor revokes that session.
Open in T3 uses the OS handler or the configured server's web fallback.

A Mac with only the old webview session now shows the same native connect state
as Linux for Board/Projects. Existing Settings > T3 sign-in can connect MCP;
explicit native pairing stores its separate bearer through #2519's Keychain
path. Installing a newly paired native client also replaces the current Board gateway
immediately and refreshes the Reader's cached project/icon gateways; Board does
not need to be reopened first. Reopening a project reads
the saved credentials again without authorizing
or pairing automatically. It does not require a Manager webview to exist.

Tests exercise the production shell gateway over loopback HTTP with no MCP
credential and no webview: all five project/icon/summary operations must arrive
with a bearer and no Cookie header. Session regressions additionally install a
populated cookie-source spy and require zero reads, verify the MCP-only path,
and prevent read-only or rejected native grants from falling back for writes.
Both macOS and Linux run these tests; the macOS gate explicitly includes the
existing navigation, session and Board suites with non-empty filter controls.

Navigation fixtures use the canonical temporary root so Reader and project paths
share identity on macOS (`/var` is an alias of `/private/var`). Assertions and
product path handling stay the same.
