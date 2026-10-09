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
`~/.local/state/okilum/reader-diagnostic.log` (or absolute `$XDG_STATE_HOME`).

For QA beside a user's running Reader, set an absolute `OKILUM_STATE_DIR` (and
`--index-dir`): that instance then owns its own state, instance lock, drafts,
diagnostics and presentation config (`<dir>/config`) instead of forwarding to the
running Reader. This is the only per-user override on Windows.

## Window state integration (#592)

New Window snapshots the initiating Reader before attaching the shared vault.
The explicit document intent survives attachment; the existing store restores
layout/history/scroll once the document is ready. Appearance, font size and
reading width remain application preferences. Subsequent navigation and scrolling
are independent, and the active sibling owns persisted viewport state. This does
not add durable multi-window relaunch or change the preference schema.

On macOS, Windows and X11, a duplicate retains its source size/display with a 28 px cascade; on Linux/Wayland, placement and tiling belong to the compositor.
At work-area edges the position wraps to the visible origin; an oversized window
is fitted to the work area. Explicit duplication does not restore a saved slot's
maximized/fullscreen state. Normal launch still restores saved geometry.

Acceptance also covers light/dark Linux before/after, duplicate geometry near
screen edges and on a secondary display, explicit note versus an older empty
selection, global preference refresh in both windows, and independent history
and scroll after duplication.
