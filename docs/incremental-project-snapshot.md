# Projects snapshot copying during an incremental save

Refs [editing lag](https://git.oklabs.uk/BeFeast/okilum/issues/2803).
Source baseline: `296196975b792998b15eb0161e5c69ba4c5f770b`.

`Reader::start_incremental` already executes canonical reads, graph/search work,
checkpoint persistence and retention on the background executor. However, before
spawning that worker it deep-clones `projects::Index` on the foreground executor.
That index contains a BTreeMap of every note's project/task/recency metadata and
a vector of projected project rows. Even one accepted small-note save therefore
copies metadata unrelated to that note on UI.

Capture the existing immutable `Arc<Index>` instead, and perform the same deep
clone within the existing worker before project mutations. The captured snapshot
is exactly the one previously copied synchronously. There is no await between
snapshot capture and task submission; other foreground events cannot alter its
immutable value. The worker's cancellation check, batch reconciliation, project
refresh, epoch/root publication guards, serialized persistence, canonical saves
and recovery remain unchanged. No new task or thread, new copy, index model or
save delay is introduced. This moves work; it does not make the deep clone
proportional to the changed note or remove all other vault-sized worker work.

The focused app regression launches the actual incremental path in a vault with
128 task notes and 256 project source files. Its existing worker gate separates
foreground dispatch from task execution. The allocation meter observes real
allocations, including those of the unchanged old production clone. An explicit
clone of that exact published index fires the positive control; foreground
launch must allocate less than that one complete copy. The bound derives from
work the control actually performs, not a wall-clock target. After releasing the
gate, the worker must publish updated project task counts and search data while
the captured published snapshot and canonical source retain their exact values.
The same test must fail with the original foreground clone restored and compiled.

No latency, native key-to-paint, RSS or attribution of the retained aggregate
64–67 ms highlighter phase is claimed. This is independent of the large JSON/XML
tools, their worker cancellation/size/stale guards, and the pending syntax-tree
coordinate representation limitation. No upstream or GPUI core change is needed.
