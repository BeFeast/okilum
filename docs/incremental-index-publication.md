# Incremental index publication and retirement

Refs #2803. Canonical note writes, recovery, save ordering and index persistence
are unchanged. Source reads, graph/search updates and persistence already run
on the background executor; their aggregate time is not UI-thread latency.

An accepted incremental batch replaces immutable indexes on the foreground
executor. Replacing the last `Arc` also destroys its map/vector contents there.
For a project-sized vault, even a small body save could therefore free every
old title, task or project metadata row during UI publication. Retain the
previous published snapshots through the pointer swaps and release that group
on the background executor. The retirement group retains the cache lease until
the old searcher closes, so vault eviction cannot race delayed destruction.
The delayed Now publication also releases its
previous workbench snapshot and time-label map there. A superseded completion
or a completion whose Reader no longer exists also releases its unused index
and labels on the worker. This does not promise exclusive ownership:
other consumers can extend a snapshot's lifetime.

Quick-open inventory construction stays unchanged on the worker. Different
publishers can supply differently scoped inventories (shared-session publication
currently starts from notes alone), so a body-only batch must not assume that
an existing inventory already contains every file. The replaced inventory is
retired with the other snapshots, without changing which files are published.

The Reader regression measures actual allocator frees inside the existing
foreground-publication scope, with a 192-note vault, and asserts fewer than 128
frees. The bound is below the cost of destroying even one full two-string title
map. It requires the measured scope to fire, the changed H1 to publish, the old
title snapshot to expire, and canonical source to stay byte-identical. It also
requires the old inventory to expire after the body change and a new-file
control to publish every new file. The assertion must fail with the old production path.

`acceptance/save-no-lag.yaml` is a visible typing/save smoke. The runner now
supports generated multi-file fixtures and action-to-visible `max_ms` bounds.
The separate native flow below exercises Repair, canonical-byte checks and the
real language dropdown. Screenshot/OCR timing and allocator work bounds do not
by themselves establish a 16.7 ms input-to-frame budget.
Long-line shaping/highlighting limits already recorded on #2803 remain separate.

## Validation, 2026-11-23

The regression was first compiled against production baseline `3feba9093` with
only the new test added. It failed at the foreground destruction assertion:
1,391 frees in UI publication (10,307 allocated bytes, 1,352-byte maximum
allocation). The candidate passes that same assertion, including updated-title,
expired-old-snapshot, exact-source and new-file inventory controls. Both runs
used the same shared build slot in one session. These are allocator work counts,
not a claim about physical display latency or general long-line editing.

The delayed-retirement control queues a real identical-source watcher echo at
250 ms. At 501 ms only superseded Now callbacks run; the accepted callback is
not due until 750 ms. This excludes accepted UI observer/render work from that
measurement. Restoring the old completion ownership fails with 3,921 frees and
only 48 allocated bytes. The candidate must stay below the same 128-free bound
and expire a weak reference to the superseded index, so retention cannot hide
its destruction indefinitely.

The native flow on candidate `31464a75a` passed: open a 2 MiB malformed JSON,
Repair/preview/Cancel, Repair/preview/Apply, navigate to a 1,283-byte Markdown
note, type and save, then open the real language dropdown and press Down sixteen
times. Both repaired JSON content and exact Markdown bytes were checked again
after quitting. A persistent virtual-pointer client held the Wayland pointer
capability through launch; transient pointer injection alone did not reliably
activate the buttons. No product or shared runner hooks were added.

UI index publication measured 0.129–0.219 ms; changed-note worker indexing took
118–133 ms off the foreground executor. The small-note mutation-through-painted-
frame counters still reported 28 samples, mean 47.01 ms, maximum 100.73 ms, with
25 over 32 ms. These process-wide overlapping counters include painted-frame
work, but prevent claiming the original 16.7 ms budget. The tester issue must
remain open for residual latency and native/reporter acceptance. Menu traversal
was functionally verified, without a separate per-key latency bound.
