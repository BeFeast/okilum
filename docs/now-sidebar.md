# Now sidebar, slice 1 (#1927)

The left panel defaults to **Now**. Its header switches between Now and the
existing **Tree**, also with Cmd+Shift+Backslash on macOS or
Ctrl+Shift+Backslash elsewhere. Reveal in Tree (Cmd/Ctrl+Shift+L) explicitly
selects Tree and reveals the open file. Plain mode switching retains the tree
object, expanded folders, cursor and scroll handle; it never reveals a new row.
The mode persists per vault in application state. Single-folder viewers and
project mode retain their existing tree surface.

Now shows recent project activity, changes grouped by day and place, existing
pins and a nine-domain activity strip. Project candidates come from manifest
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
delivery on Mac and Linux Sprint. Oleg's daily use for two weeks determines
whether Now stays the default. Delivery alone does not prove human acceptance.
Unread state, origin filters, Waiting for me, review queues, project Now mode,
right-panel changes and Collections belong to later slices.
