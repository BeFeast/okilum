# Vault windows (#551)

Opening a full vault through Open folder, Recent, `--vault`, or native Open With
focuses its existing window (a folder that is not yet a vault opens as a folder
view, #3098). Canonical directory identity includes symlink aliases.
A pending opening reserves that identity before inventory preparation. Opening a
Markdown file inside an open full vault navigates that window, preserving #560.
For a different vault, Settings → Appearance → Open another vault offers In this
window, In a new window, and Ask every time (the default). The choice applies to
folder selection, Recent, the vault menu, external links and native open intents.
The prompt has Remember my choice; Settings can change it later. Cancel leaves
the current vault intact. Replacement uses the same save guard as closing the
window: conflicts and failed saves prevent replacement. A standalone file remains
a quick viewer. Existing windows of the requested vault still receive focus.
Explicit New Window and startup window restoration bypass this preference (#1665).

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

For QA beside a user's running Reader, set an absolute `OKILUM_STATE_DIR`: that
instance then owns its own state, instance lock, drafts, diagnostics, presentation
config (`<dir>/config`) and derived vault caches (`<dir>/cache`) instead of
forwarding to the running Reader. This holds for every window and for the first-run
folder picker, so a second vault opened there is cached under `<dir>/cache` and its
retention (three vaults) only ever evicts caches of that instance (#1129). This is the
only per-user override on Windows. `--index-dir` is separate: it is the index of the
vault the instance is launched on and is not inherited by windows opened later.

## Known vaults in the name menu (#2002, slice 1)

The vault-name menu lists known full vaults with a check beside the current one
and an absolute path tooltip. Clicking a row follows the same destination policy
and save/conflict guard as Open folder. Open folder, Reveal and Vault settings
remain available; the project-name menu keeps its project actions.

The list uses the existing application-state `update-session.json` reading
history, outside the vault and disposable caches. Quick-viewer folders do not
become known vaults. Paths are checked independently in the background whenever
the menu opens: a missing/unmounted path is marked Unavailable and retained.
Opening the menu again rechecks a remounted path. No path probe runs in render.

Remove from list hides only the history list entry, including it in startup
recency suppression; it does not delete, rename or write the vault, or erase its
last reading position. The additive `hidden_roots` field is omitted while empty;
existing history remains readable without migration. Successfully opening/using
that vault again lists it as recent. A state-write error leaves the entry visible.
Application-wide list management is described in slice 2 below.

## Settings → Vaults (#2002, slice 2)

Vaults is an application-wide Settings section, separate from the open vault's
name, appearance and file settings in Vault. It uses the same known-vault list
as the name menu. Each row shows its name, full path, availability, current/default
markers and last successful opening time. Older records show Not recorded rather
than an inferred timestamp; opening time advances on a usable vault publication,
not every note navigation or a failed candidate.

Open in new window explicitly bypasses the destination preference; an existing
vault duplicates its shared session instead of merely focusing the old window.
Remove from list leaves canonical files and reading position intact and clears
that vault's default preference. A missing/unmounted path remains listed, can be
removed explicitly, and is rechecked by Refresh. Reads and writes run in the
background and failed state writes leave the visible list unchanged.

Make default/Clear default persist only in application state, without a migration.
The saved window set restores first; if no window can be restored, the default
precedes last-used fallback. Explicit CLI/OS/file/folder deliveries still supersede
startup. An unavailable default opens the existing start screen with its unavailable
notice, rather than silently selecting a different vault. No default keeps the
existing launch behavior.

## Recent vaults on the start screen (#3208)

Design: the «Welcome screen: interaction spec (combined design)» comment on
#3208 (layout A · Launcher, first launch from B, filter and ⌘1–9 from C, New
file). Code: `reader_welcome.rs` draws it, `reader_startup.rs` holds the list
and its actions, `reader_recent_vaults.rs` decides what is listed and why a
vault is unavailable.

**Layout.** Two panes under a transparent title strip: the actions on the
left (Open folder… primary, Open file…, New file, New vault…, then Sync),
each with a one-line description and its shortcut; the recents list on the
right with a filter field and a footer (key hints, Clear list). Each row has a
folder icon in the vault colour, the name over the path and a relative time.
Below 640 px the panes stack and the window scrolls; the start window can be
400 px wide. With nothing to list, a first-launch page replaces both panes.

**What is listed.** Folders opened as vaults (`known_vault_roots`), newest
first; unsaved untitled drafts (#1528) above them, at most three. Ten rows,
then Show all. A folder that only held a file opened on its own, or a folder
view, is not listed. In the user's own history (not an `OKILUM_STATE_DIR` QA
instance, not a test's state directory) folders nobody chooses as a vault are
hidden when recorded and pruned on every startup: the system temp folders, any
`qa-evidence` path, `~/Library/Application Support`, `~/Library/Caches`,
`~/Library/Logs`, `AppData`, `.cache`, `.local/state` and the trash. A hidden
folder keeps its last note but is neither listed nor reopened at startup.
iCloud Drive (`~/Library/Mobile Documents`) is not excluded. The history keeps
at most 30 vaults besides the default. `okilum --no-history` or
`OKILUM_NO_HISTORY=1` records nothing for that process.

**Removing.** Remove from list (row menu, ⌘⌫ on macOS, Delete elsewhere)
hides the entry, with a 6 s Undo toast. Clear list hides every listed vault
after a confirmation; drafts stay. Opening a hidden vault again lists it again.
Locate… forgets the old entry and opens the chosen folder, keeping the colour.

**Keyboard** (context `ReaderEntry`): ↑/↓, Home/End, PageUp/PageDown, Enter or
⌘↓ to open, ⌘1–9 / Ctrl+1–9 for the nth visible row, ⌘N / Ctrl+N for New
file, Esc clears the filter, Tab moves between actions, filter and list. A
printable key typed in a list goes into the filter (name, then path, then the
letters in order). Holding ⌘ / Ctrl for 500 ms shows the row numbers.

**Unavailable vaults.** Each listed vault is checked off the UI thread, one
thread per folder, with a 1.5 s deadline in all, when the screen appears and
when its window comes back to the front. The row stays in place and shows the
reason and one fix:

| Reason (macOS wording; Linux says «mounted») | Fix |
|---|---|
| `Network volume “X” isn’t connected` (the vault was on a network volume when last opened; Linux: `Network share “X” isn’t mounted`), `Volume “X” isn’t connected`, `Drive “Z:” isn’t connected`, `Network location \\server\share isn’t reachable` | Try again |
| `Network folder isn’t responding` (deadline passed) | Try again |
| `Folder was moved or deleted`, `This path is now a file, not a folder` | Locate… |
| `Okilum isn’t allowed to read this folder` | Allow access… (macOS) / Locate… |
| `Couldn’t open: <system message>` | Details… |

A folder that opens but cannot take edits is available: its row shows a lock
and `Read-only · <path>`. Paths are shortened by whole folders next to the
anchor (`~`, `/Volumes/<name>`, `C:\`, `\\server\share`, `smb://server/share`),
keeping the last folder and, while filtering, the one that matches.

**Visual evidence.** `welcome-native.yml` (push to `welcome-native/**`) builds
with the non-publishing `welcome-ui-harness` feature and captures every state
on Linux, macOS and Windows; `OKILUM_WELCOME_HARNESS` sets the filter, held
numbers, Clear list confirmation or keyboard focus without input automation.

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
