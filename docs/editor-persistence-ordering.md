# Editor draft and save ordering

Issue #2404 is the persistence/lifecycle slice of #1621. The input revision is
canonical. FileEditor retains its immutable Rope together with a shared immutable
base; copying a draft or a saving handle does not copy either note's bytes.
Contiguous UTF-8 is materialized at the worker persistence boundary. Recovery JSON
is backwards compatible, including authored BOM and CRLF bytes.

Each input mutation (including native silent Undo/Redo and IME mutations) replaces
the pending recovery task and assigns a monotonically increasing generation.
Recovery starts after a 300 ms pause. A writer owns only the generation captured
with its snapshot; it checks ownership before serialization and again under the
journal publication lock. Locks remain held while any writer/save copy exists.
No completion from an older input/session may report the latest draft protected.
Recovery errors retain the editor and are shown to the user; explicit Save retries.

Canonical saves use a separate shared serialization lock. A save first protects
its captured draft, then uses the existing exact-byte conflict/publication checks.
The next save adopts only a confirmed own publication's base, under that lock.
An older save request cannot publish after a newer completed save. Its recovery
acknowledgement can retire its own generation, using compare-and-exchange, but
cannot acquire ownership over newer typing. The UI acknowledges a successful save
by retaining its current draft with the confirmed saved base in a new generation.
Thus typing during a save survives both the save's initial and final journal writes.

Explicit Save captures the native revision directly, even before deferred mutation
subscribers run. Close, Keep Draft and Quit synchronously flush the latest revision
before releasing ownership; a failed flush prevents losing that editor. The
300 ms debounce is a crash-durability window, not a claim that unflushed keystrokes
survive power loss. Once the worker acknowledges a generation, restart restores
that exact text; a changed canonical file remains an explicit conflict.

Typing notifies editor entities. TableGrid no longer forwards every editor
notification to Reader. Outline workers share native snapshots and notify Reader
only when displayed outline labels change. Direction exemptions, rendered-block
reference definitions and footnote context are prepared after the recovery pause
on a worker, accepted only for the same editor/generation. Pending block context is
unresolved rather than borrowed from another revision. Context-menu source facts
are immutable Rope snapshots; full reference/code context is extracted on opening
the menu, not on every editor notification.

Regression coverage includes reverse recovery completion, canonical save races,
a late pre-save journal writer, typing during publication, failed recovery/retry,
close/quit before debounce, crash-style reopen and exact BOM/CRLF bytes. The
foreground allocator gate observes actual allocations, including the legacy
String path, at 25 KB, 130 KB and 1 MiB. It requires a bounded largest allocation
and total allocation count-bytes, plus a positive control that materializes the
same native input. The same test is overlaid on the pinned origin/main source for
a real negative-control run. Core revision counters independently establish no
flattening/scanning while queuing/copying shared snapshots. These are deterministic
work bounds, not end-to-end latency measurements or #2405 own-save receipts.
