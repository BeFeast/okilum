# Vault windows (#551)

Opening a full vault through Open folder, Recent, `--vault`, or native Open With
focuses its existing window. Canonical directory identity includes symlink aliases.
A pending opening reserves that identity before inventory preparation. Opening a
Markdown file inside an open full vault navigates that window, preserving #560.
Different vaults have separate windows; a standalone file remains a quick viewer.

File → New Window (also in the Reader menu) explicitly duplicates the active full
vault: Cmd+Shift+N on macOS, Ctrl+Shift+N elsewhere. Each window has independent
navigation and source editing. New Window waits for the initial baseline instead
of starting another index build. The duplicate shares the recursive watcher,
mutable graph baseline, search index and cache lease. Session workers outlive any
one window; completed updates reach the surviving windows. Watcher overflow uses
one full reconciliation and keeps the last usable snapshot meanwhile.

Repeated desktop launches send open intents to the existing Reader process through
an authenticated local endpoint protected by an OS file lock in application state.
A crashed process releases the lock automatically. Managed/Brain launches are
separate. Failure to contact a live owner is reported instead of starting a second
Reader that would race its index. No document data is sent through this endpoint.

Native acceptance on Linux: repeat folder/recent/CLI opens, then explicit duplicate,
independent navigation, external edit visible in both windows, close the first
window during changes, and open a different vault. Repeat native handler/shortcut
checks on macOS and Windows. Linux diagnostics:
`~/.local/state/tessera/reader-diagnostic.log` (or absolute `$XDG_STATE_HOME`).
