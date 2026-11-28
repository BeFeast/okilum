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

The Tree and Now tabs, the View menu's Now / Tree command, and the Tree shortcut
in the Now footer save the selected mode per vault. Switching modes retains
the tree's expanded folders, cursor and scroll position. Search, Files and
Reveal in Tree (Cmd/Ctrl+Shift+L) show Tree temporarily. Areas is also
session-local. Reopening the vault restores the saved mode.

With **Now** saved, opening the vault by its root or restoring its windows on
launch opens the Feed note declared by `start.note` in `.okilum/vault.json`.
Set it to an existing file relative to the vault root, such as
`{"start":{"note":"Feed.md"}}`. If the target is absent or invalid, the usual
restore and discovery rules apply. **Tree** uses those rules regardless of
`start.note`. Explicit file opens, deep links, exact restores and single-file
windows keep their requested targets.

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
perform no filesystem IO. Projection and navigation state is derived or application-local. Network roots explicitly show their snapshot time and
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

## Inline actions (#3297)

When a vault publishes `@views/out/now.json`, Now shows the unified feed.
Reply includes live T3 questions from the exact project manager binding.
A single keyed question can offer quick options and a custom reply when the
server permits it. A request with several keyed questions can accept a reply
for each. The row also opens the manager for legacy requests and answer modes
that require the full T3 composer. An accepted response refreshes the pending
list. Failed responses leave the form and its text available.

A case response with a case answer target offers an inline decision. It sets
`case_status: active` and `updated`, then puts the decision at the top of History.
`+ Intention` in Available appends a dated bullet to the Intentions section of a
selected `state/<area>/@index.md`. Both use the Asia/Jerusalem date, and decisions
read the owner label from `@Schema/people.yaml`. Each write checks the target
revision captured when Now loaded, uses the same write admission and source
editor as explicit saves, and offers Undo while the saved revision matches.
Now reflects confirmed edits immediately. Its snapshot remains generator-owned.
