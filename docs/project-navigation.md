# Vault projects and application navigation (#1472, #1474)

Approved decisions of 22 October 2026 supersede the former current-note Project
button and the typed-project section in the left sidebar.

A project is any vault folder containing `ops/project.toml` (legacy `ops/project.yaml` while TOML is absent), across every domain.
Discovery does not require `Dev/Areas`, a repository, or a particular PARA category.
`kind` is `code`, `knowledge`, or `hybrid`; absence means code when `repo` is
present and knowledge otherwise. A minimal knowledge manifest needs no executor,
checkout, build, CI, or release configuration.

The application menu opens Reader, Inbox, Projects and Logs. The left sidebar
contains only Recent, Pinned and Folders. Its explicit collapse preferences stay
fixed while scrolling; ancestor folders that have scrolled out appear as sticky
rows inside the tree viewport. Old five-section restore snapshots migrate without
losing pins or recent notes. Legacy section names remain decodable in old layout
files, but have no sidebar rows or separate project collapse state.

Projects shows the project name, vault path and last refreshed manager status.
Unreadable manifests and ambiguous role registries remain visible errors.
The current `Manager` row in `ops/thread-ids.md` is authoritative, including
`— (not started)`; the manifest's `t3.manager.thread_id` is a fallback only when
the registry file is absent. A pointer alone is not proof of a running manager.
Status refresh groups listings by configured T3 environment and project.

Entering a project preserves the original tree, expansion, cursor, scroll and
panel choices, then scopes the tree and opens the manager on the right. Switching
A to B keeps that original snapshot. Leaving from the window header restores it.
Ctrl+Alt+P (Cmd+Alt+P on macOS) opens Projects or leaves the active project.
Switching vaults invalidates pending discovery/activation work and the snapshot.

On macOS, the independent manager panel uses a child WKWebView and the public T3
environment descriptor to construct the browser route on the configured origin.
Login stays inside an application-specific WKWebsiteDataStore; browser cookies
are never copied into application settings or MCP credentials. Escape and clicks
outside return native focus. Reader shortcut context is disabled while the browser
has focus. A failed descriptor request reports an unreachable state. Linux and
Windows keep the existing native manager surface and compile no webview dependency.
The note-outline modules are independent of this panel.

Start manager is explicit. It uses the manifest's manager T3 project and its root
workspace, with a first message pointing to the selected area's manager-state and
AGENTS files. Knowledge managers use the vault workflow; code and hybrid managers
retain the repository workflow. Missing T3 configuration is an error, never an
invitation to choose a project silently. A settled or missing manager must be
confirmed by a fresh listing before replacement.

Launching is non-idempotent. A durable intent outside the deletable index precedes
the request. An uncertain response blocks a second launch. A known created ID is
retained before writing the registry. Registry replacement uses FileEditor's
lossless revision-aware save and changes only the Manager cell. External edits,
binding changes and unsaved recovery drafts prevent a silent overwrite; a failed
registry save reports the already-created ID for reconciliation. This extends the
explicit writing phase, not the read-only reader protocol.

Required CI and one code review remain premerge gates. Ordinary UI acceptance is
human QA on a concrete Mac beta build; code and fixture tests do not establish
live OAuth, browser interaction or user acceptance. Manager rotation policy is
tracked separately in #1473.

### Native Manager input on macOS (#1510)

A panel-owned AppKit event monitor makes the clicked WKWebView first responder.
Its mouse-down notification clears GPUI logical focus and pending chords, so the
previously focused origin input cannot receive paste. Escape restores that prior
GPUI focus; outside clicks choose their own target.
While that view (or a descendant) is the key window's first responder, editing
shortcuts use native responder actions and all other keyboard events go directly
to WebKit. Paste keeps the original system pasteboard, including images; no
JavaScript clipboard emulation is involved. The bridge temporarily provides a
native Edit menu with nil-target responder actions and restores the original menu
on blur, hide, window change or teardown. Escape and outside clicks return first
responder to the GPUI parent. Native focus, rather than delayed DOM notifications,
controls suppression of Reader shortcuts.

The bridge uses public AppKit APIs and lives outside GPUI core and the vendored
webview. Hosted selector tests cover native edit command mapping. Oleg's Mac beta
acceptance must still check text/image paste, copy, select-all, undo/redo, IME,
T3 shortcuts, outside-click/Escape return and closing/reopening the panel.

### App-wide T3 connection in Settings (#1507)

Settings → T3 owns the address field and app-wide connection. The
Manager panel retains the native webview without an extra header; when disconnected it shows a
plain explanation and Open T3 settings button. Pairing stays inside T3.
The Settings address field submits with Enter or the app-styled Connect button. Names
without a scheme use HTTPS; IP:port (and localhost:port) use HTTP. Browser paths,
query strings and fragments are reduced to the origin, which is shown in the field
and alongside the transport rule. Credentials in addresses are rejected.

HTTP is allowed for loopback, RFC1918 IPv4, link-local, IPv6 unique-local,
and the 100.64/10 tailnet range, retaining the main transport policy. Public names and public IPs require HTTPS. This is a deliberate private
network transport exception; Okilum does not resolve arbitrary public names to
classify them as private.

Prefill uses optional `t3.origin` in the project manifest, then the manifest's
`t3.environment` alias in the device connection registry, then the last successfully
validated app-wide origin in app state. Explicit project manifest / registry
bindings override the shared origin without replacing the app-wide last success.
A successful Settings connection refreshes existing Manager panels across windows. A delayed prefill cannot replace typing.
No project list layout or category changes are part of this fix.


### Native editing follow-up (#1510, #1551)

The focused Manager webview owns Cmd+C/V/X/A/Z and Cmd+Shift+Z. Commands resolve
an explicit WebKit responder, then use the guarded page helper only if native
handling produced no edit. Non-Latin keyboard layouts use physical editing-key
positions while Latin mappings and modifier guards remain intact. Each return
from GPUI refreshes page context; navigation, focus changes and intervening input
cancel queued edits. Copy/cut excludes password and CSS-masked secure fields.

The page helper uses document/target identities and revision guards. Clipboard
content and key events are not logged. `page_keys.js` is canonical; regenerate its
Objective-C string with `python3 scripts/dev/generate-manager-page-keys.py`.
Required macOS CI runs pure page-guard and native dispatch unit regressions.
The real-NSApp fixture remains ignored/manual-only (`OKILUM_NATIVE_GUI=1`) because
hosted runners cannot establish an active/key application.

Native WebKit geometry comes from the actual Wry element. GPUI overlay bounds
are frame-scoped; the webview shrinks to uncovered space and restores when clear.
No GPUI core patch is used.

Oleg approved T3 visibility and Cmd+C/V in Sprint cfceebcc. This acceptance does
not establish image paste, IME, undo/redo, or all overlay scenarios. Sprint runner,
signing, reporting and self-check tooling are separate from this product change.

Oleg also approved attached-image removal in Sprint c1dc8dc6. Its init listener
preserves focus in an already active form while a button is pressed, after page
handlers run, so WebKit does not blur the composer and unmount the image X before
mouseup. Disabled/draggable buttons, other forms and cancelled events retain
normal behavior. Pointer diagnostics are not included.

### One authenticated connection per app (#1758)

Settings → T3 distinguishes a saved server address from an authenticated connection.
Sign in checks and reuses an origin-bound credential before asking for approval.
The resulting OS-keychain token serves every project alias on that origin;
explicit project overrides keep their own origin. Existing native panels reload
the shared credential after sign-in. Tokens and browser storage remain isolated
between Okilum and Okilum Sprint.

On macOS approval opens in an app-owned WebKit sign-in view using the exact
persistent store of the Manager panel. An already paired Manager session therefore
serves authorization too, and a new sign-in pairs the store used by future panels.
On other platforms the system browser opens; it cannot donate its browser cookies
to an embedded view. The separate Linux WS native-client audience is unchanged:
an MCP token is never treated as a native-session token.

The browser-return wait is ten minutes and shows its remaining time. Wait longer
extends it; after timeout it reopens the original loopback port. Paste callback URL
remains available after the listener closes: the original redirect URI, state,
issuer and PKCE verifier still govern the exchange. Wrong-origin/state/issuer and
duplicate parameters are rejected without exchanging a code. Cancel discards the
attempt; expired authorization codes require a new sign-in. Merely entering a
reachable address never displays “Signed in”.

Regression tests use a loopback fake T3 server and isolated app state. They do not
establish live pairing, WebKit interaction or Oleg's acceptance on a delivered build.
