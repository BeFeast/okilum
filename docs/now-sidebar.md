# Now sidebar, slice 1 (#1927)

Second brain is an optional module (#2091, Oleg 2026-10-31). New vaults open
**Tree**, regardless of project discovery. The **Tree | Now** tabs remain visible
in vault mode; selecting Now explicitly enables the module for this vault and
makes it the saved view. Existing explicit mode choices survive migration.
Vault settings can disable Now without deleting its saved section state.

A vault with at least 100 prepared note-link edges or three date-only daily notes
gets one inline offer to make Now its default. This is a contextual hint from the
accepted snapshot, never a scan performed by rendering. **Use Now** enables it;
**Not now** remembers refusal for that vault across restarts. A plain note vault
and a project alone do not trigger the offer. Explicit activation or disabling
also answers the offer. The shared Settings module switches belong to #2092.

The header's icon tabs switch between Tree and Now, also with
Cmd+Shift+Backslash on macOS or Ctrl+Shift+Backslash elsewhere. Reveal in Tree
(Cmd/Ctrl+Shift+L) explicitly selects Tree and reveals the open file. Plain mode
switching retains the tree object, expanded folders, cursor and scroll handle;
it never reveals a new row. The mode persists per vault in application state.
Single-folder viewers and project mode retain their existing tree surface.

Now keeps the In progress, Changes and Pinned section headers visible, with
counts and collapse controls. In progress groups compact, single-line project
rows by domain (Dev, Music, AI / HomeLab, then remaining domains). Each group
initially shows four projects and an expandable +N preview. The current project
is retained in that compact set and highlighted; the current pinned note is also
highlighted. Rows use short project names, a status dot, meaningful-file activity
or recency, and M only when discovery has an actual manager binding. Document
headings do not rename projects. Paths stay in tooltips and Tree reveal.

The older 14-day tail starts collapsed. Changes are grouped by day and place;
Pinned uses the existing pin list. A fixed footer shows the file count and an
explicit Tree shortcut. Local freshness says `live · snapshot HH:MM`; network
freshness says `NFS · snapshot HH:MM`. Both offer Rescan and a compact action menu.
Project candidates come from manifest
discovery and children of Projects; terminal status/lifecycle values exclude a
unit from activity. Source mtime determines recency. Shims, transcripts,
dot-directories, resource forks, Pokutan, archive and operational bookkeeping
do not become activity. Fifty or more edits within ten minutes in a place are
represented as one batch row. Each day initially lists at most eight places.

The projection uses accepted source snapshots on background workers. Incremental
batches update its input rows; publication is debounced for 500 ms and guarded
by root, watcher generation and request sequence. Render and mode switching
perform no filesystem IO. All state is derived or application-local; Now never
writes vault files. Network roots explicitly show their snapshot time and
Rescan instead of claiming live updates.

Acceptance includes preserved Tree behavior, real-corpus counts and projection
timings (full build <=150 ms; 20-file batch <=10 ms on maestro and M4), and
beta delivery. Tree remains the default unless explicitly changed. Delivery
alone does not prove human acceptance.
P0 badges and Waiting for me share the later manager-state slice. Neither is
inferred from activity. Unread state, origin filters, review queues, project Now mode,
right-panel changes and Collections belong to later slices.
