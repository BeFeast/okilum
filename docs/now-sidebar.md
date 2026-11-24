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
counts and collapse controls. In a vault with a lifecycle layout summary in
`.okilum/vault.json`, In progress groups compact context rows by the
typed record's area, kind and status. Each group initially shows four contexts
and an expandable +N preview. The current context stays in that compact set and
is highlighted. Rows use short names, a status dot, note activity or recency,
and M for a bound manager. Context declarations use a typed project or area
note, a project manifest, or an index explicitly declaring a legacy project.

The older 14-day tail starts collapsed. Changes are grouped by day and then by
the same three fields on each note. Both sections display `—` for each missing
field. Changes includes notes under any ordinary folder, even when their
records have no lifecycle fields. Shims, hidden dot-directories, resource
forks, schema and operational bookkeeping stay outside activity. Fifty or more
edits within ten minutes in the same context or parent folder become a batch
row in their group. Each day initially shows at most eight groups.

Pinned uses the existing pin list. A fixed footer shows the file count and a
Tree shortcut. Freshness says `live · snapshot HH:MM` for local vaults and
`NFS · snapshot HH:MM` for network vaults. Both offer Rescan and an actions
menu. A terminal context stays in Changes when recent, and leaves In progress.
A vault without a layout summary keeps its original domain and place groups.

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

## Waiting for me (#2346)

**Waiting for me** sits above In progress. Its rows are the pending questions
T3 reports for each discovered project's bound manager thread
(`t3_pending_request`), never file times or activity. Each row keeps the exact
source: project folder, T3 environment, manager project, manager thread and
pending request ID (shown in the tooltip). At most 20 rows per thread are listed;
the rest is one explicit `+N more` row. The text of each listed question is
read with `t3_pending_request` `read` from the same thread; the row states that
question (the first one, with `(+N more)` when a request asks several), then its
project and freshness. A question whose text cannot be read stays a row titled
*Question · text unavailable*; a failed text read keeps the confirmed text, and a
withdrawn question drops it. T3's pending-request list does not include
approvals, so approvals are not rows. A row opens its project; entering the
project opens that manager thread, where the question is answered.
A row opens only while its question and manager binding are still current; a
rebound or removed manager never keeps the old thread's questions.

A read runs on project discovery, Rescan and when Now is shown, never per frame
and never from Tree. A failed read keeps the last confirmed rows, marked stale
with their confirmation time; an unreachable environment or an incomplete
manifest is its own warning row, and the status line says how many sources are
unavailable. Replies are accepted only for the vault root and read generation
that dispatched them, so a reply for a previous vault, a superseded read, or one
arriving after Now was disabled is discarded. Disabling Now (or Agents) revokes
the in-flight network permission before the next request, forgets the rows and
writes nothing to the vault.

P0 badges are not part of this slice and are not inferred from activity. Unread
state, origin filters, review queues, project Now mode, right-panel changes and
Collections belong to later slices.
