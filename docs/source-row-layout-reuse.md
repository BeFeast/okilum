# Exact unchanged source-row layout reuse

Refs [editing lag](https://git.oklabs.uk/BeFeast/okilum/issues/2803) and
[large edit tools](https://git.oklabs.uk/BeFeast/okilum/issues/2804).
Baseline main: `4b316c9b25141bb8e4bc1454a08e4c918aaa4273`; gpui-kit
`928c3eb776a3d733d9b771f7dea27a6a79242ced`, unchanged app-lock GPUI 0.3.3.

## Proven cause and bounded correction

The retained whole-frame SourceShapeCache includes source generation and
presentation epoch. Editing a short third row invalidates that key and sends
all visible rows through layout_lines again. The retained virtual-frame probe
proved materialization and different backing layouts for unchanged 307,216-byte
Hebrew/emoji and 307,207-byte TAB rows. This is source work, not native latency.

Patch 0148 retains the existing whole-frame cache and its eligibility/budget.
On its miss, a complete unwrapped source row may reuse a layout from that same
previous visible set. Admission requires the same document, font/size, scale,
row metrics, wrapping policy, text alignment, content/gutter width, exact row
bytes and exact clipped row-relative TextRuns. RopeSlice equality compares
chunks against the already-owned shaped text without allocating another String.
Generation and presentation epoch alone are not row geometry inputs: source,
styles and all effective geometry inputs are checked instead. No hash-only or
identity-only admission is used.

Only full 0..row_len unwrapped, unscaled, non-hanging rows participate. Projected,
wrapped, masked, IME, whitespace-indicator and document-color paths retain their
existing layout logic. Display-row placement is refreshed, as is contextual
paragraph alignment/exemption. The existing paragraph-direction preparation may
still materialize a row when enabled; this patch does not claim to remove that
separate Markdown work. The plain-file reproduction disables that preparation.

The layout clone shares native ShapedLine backing and immutable bidi geometry
through Rc. It copies only small layout metadata in this eligible path; it
never clones glyph vectors or bidi cells. Geometry remains immutable and each
set_wrapped_lines replaces its geometry owner. No new source snapshot or cache
collection is retained: the same one previous visible-set cache and existing
painted layouts own the data. Non-visible rows are not accumulated. Necessary
residence/sharing is not an RSS claim.

## Controls

The real Reader fixture uses the proven two 300 KiB rows plus a short third row.
Per-editor opt-in diagnostics require the changed-row path and actual byte
materialization to fire, then assert that only the changed short row materializes
and unchanged rows reuse layouts. Source, one Undo/Redo, canonical save and real
Edit -> Read -> Edit reopening retain exact bytes. A separate fixture compares
strict far-right caret/hit geometry of actually reused rows to a cold effective-
color round trip at the same viewport. A deterministic InputHighlighter uses the
real TextRun composition seam and must fire its style callback; effective style
changes must invalidate decorated layouts. A language-name-only control did not
change effective styles and failed its fired-control assertion, so it is not
counted as evidence.
Viewport-size changes separately require fresh materialization. The initial
resize-round-trip geometry oracle failed identically on main and candidate:
TAB-row caret x changed 1274.5 -> 1290 px with identical source hit offsets.
That oracle changed scroll state, so it was replaced by the same-viewport
cold-color oracle, keeping exact absolute caret/hit equality without tolerance.
Both original FAILED controls are retained in the external evidence.

The compiled old-production negative restores main's complete vendor stack,
adding only the same observational diagnostic fields/materialization counter.
It does not implement reuse; its actual large-row materialization must fail the
new bound. This external negative variant is not shipped. The retained private
frame probe is adapted only to observe actual reused backing identities and
zero unchanged-row materialization; its old result is reused, not rerun as an
unrelated timing sweep. Normal builds use the same production path with opt-in
counters disabled.

## Remaining scope and upstream

Changed-row Unicode/TAB shaping, materialization, glyph copying/suffix replay,
visible style queries, offscreen longest-row shaping, XML performance,
Tree traversal/sharing, Parser/Rope owners and platform/native acceptance remain
separate unfinished work. No native/RSS/16 ms or aggregate 64–67 ms claim follows
from these controls.

The owning upstream is [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit),
base input layout/display_map. The ordered idempotent vendor diff requires no
core GPUI/tree-sitter API or source change. On upgrades retain the exact effective
key and immutable ownership tests. A future general upstream proposal would
carry the cache admission contract and geometry/actual-frame controls; no
upstream submission is made under this task.
