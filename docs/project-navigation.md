# Vault projects and application navigation (#1472, #1474)

Approved decisions of 22 October 2026 supersede the former current-note Project
button and the typed-project section in the left sidebar.

A project is any vault folder containing `ops/project.yaml`, across every domain.
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

### Native paste follow-up (#1510 reopened)

Edit commands resolve an explicit responder within the focused WKWebView subtree,
never the surrounding GPUI window. The native Edit menu uses the same browser
router. A failed action delivery no longer consumes the shortcut unconditionally;
the fallback invokes the public WKWebView key-equivalent implementation, bypassing
Wry's child override without patching its class.

The manual native fixture creates a real NSApp, NSWindow and WKWebView with Wry's
child key-equivalent behavior. It pastes a fixture clipboard string into local HTML
through the production callback and native menu, asserts no host paste, and checks
that the host receives paste after teardown. It does not use T3 or production
browser storage. Oleg's sprint check of the actual pairing field is required before
merge; native clipboard, IME and full focus acceptance are not claimed by a selector
unit test or by merely compiling this fixture.

Hosted macOS could not establish the fixture's active/key application (positive
control failed before paste), so the real-NSApp fixture is excluded from required
CI by explicit manager disposition. It remains an ignored Rust test and requires
`OKILUM_NATIVE_GUI=1` in an active local GUI session. Run from the checkout:
`OKILUM_NATIVE_GUI=1 bash scripts/ci/check-manager-webview-paste.sh`.
Dispatch selector unit tests remain in required native CI. Oleg's actual-app
sprint verification is the premerge acceptance for the reopened paste bug.

### Sprint Cmd-key diagnostics and page fallback (#1510 stop-loss)

Sprint enables `OKILUM_SPRINT_KEYS=1` in its launcher by default. Diagnostics append
to `~/Library/Logs/okilum-sprint-keys.log`: application sendEvent, public native
key-equivalent implementations and their return values, menu candidates/actions/
targets, actual first-responder classes, WebKit keyDown and sendAction results.
Clipboard text, selected text and pairing tokens are never logged. Other builds
create no diagnostic log unless the environment flag is explicitly enabled.

The app-owned router observes the native dispatch attempt before deciding whether
to use a page fallback. A page probe records trusted edit events and editing state;
copy additionally checks the native pasteboard change counter. Observed native
handling logs `path=NATIVE` and does not invoke the fallback. Otherwise, while the
same page input remains focused, text paste uses `execCommand('insertText')`, image
paste sends a ClipboardEvent containing a DataTransfer PNG file, copy/cut transfers
the page selection to NSPasteboard (cut then deletes), and select-all/undo/redo use
execCommand. The native attempt runs synchronously on the original event path. Delayed page
fallback requires an unchanged document, target, selection and input revision;
subsequent input cancels it rather than reordering a late paste. Copy/cut excludes
password and CSS-masked secure fields. Logs identify
`path=PAGE_FALLBACK` and its outcome without recording content.

This expanded scope is explicitly requested by the manager after Oleg's failed
sprint. It is not validated by hosted GUI fixtures: Oleg must send the log from a
real Sprint run and verify Cmd+V/C/X/A/Z in pairing and chat before merge.

The page helper has a per-document generation, reports only context IDs/counters
to the native router, and fails closed if that state is absent after navigation or
script failure. Required macOS CI runs pure JavaScript guard regressions, including
secure copy/cut with an unchanged-clipboard control, navigation/focus changes,
subsequent typing and trusted native paste observation. These are unit tests,
not another hosted GUI fixture or evidence of Oleg's native paste acceptance.
`page_keys.js` is canonical; regenerate its Objective-C string with
`python3 scripts/dev/generate-manager-page-keys.py`. The unit test checks equality.
