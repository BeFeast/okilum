# #737 — shared editor bidi geometry

## Native localization (2026-10-08)

The standalone `bidi737` example shapes real text through the Linux GPUI platform
text system. It does not create a source projection, write a note, or use IME.
Run with an available native display:

```sh
cargo run -p tessera-shell --example bidi737
```

The ASCII positive control asserts that every boundary in `abcd` round-trips
through `x_for_index` and `closest_index_for_x`. It passes. On the same machine,
font and process, the mixed line `abc שלום xyz` produces these positions:

| UTF-8 boundary | Current caret X | Hit-test at that X |
| --- | --- | --- |
| 4 | 33.84375 | 10 |
| 6 | 33.84375 | 10 |
| 8 | 33.84375 | 10 |
| 10 | 33.84375 | 10 |
| 12 | 69.25 | 12 |

The Hebrew glyphs themselves are correctly positioned: source indices
`10, 8, 6, 4` have X coordinates `33.84375, 44.460938, 48.820313, 57.914063`.
The core `LineLayout::x_for_index` returns the first glyph whose source index is
at least the requested index. This assumes increasing source indices in visual
order and collapses these four boundaries onto one coordinate.

For a pure Hebrew line `שלום`, the native glyph collection instead has ascending
source indices and descending X coordinates. `closest_index_for_x` assumes
ascending X; it maps every tested glyph coordinate to source offset zero.
Therefore neither glyph collection order nor source order alone can define a
portable visual boundary map.

## Other shared-editor assumptions

- `input/editor/display_map/text_wrapper.rs` delegates caret positions and pointer
  hit-testing to those core methods, in both Source and Live Preview.
- `input/base/element.rs::layout_match_range` constructs selection from only the
  two endpoint carets and clamps `end_x` to at least `start.x + 6px`. Logical
  endpoints can be reversed in RTL; mixed-direction selections can require
  multiple disjoint visual rectangles. Swapping two endpoints is insufficient.
- `input/base/movement.rs::{left,right}` advances previous/next logical grapheme
  boundaries, not visual neighbors. Selection collapse has the same assumption.

This establishes a shared geometry fault independent of source projection and
consistent with manager's baseline reproduction. It is not a completed fix or
native acceptance evidence for a fix.

## Repair boundary and acceptance

Keep GPUI core unpatched. The vendor editor needs an explicit visual caret/cluster
map over each shaped display row, using Unicode bidi levels and actual glyph
cluster extents. Byte indices remain source/display indices, never visual ranks.

The map must support both caret affinities at directional boundaries, spatial
hit-testing, visual Left/Right and Shift selection, and per-cluster selection
rectangles. Cache it with the shaped row rather than rebuilding on pointer motion.
Source projection still owns source/display conversion; do not change source,
insert bidi controls, or disable Hebrew/projection to conceal the problem.

Before integration, prove the map with native glyphs for LTR, RTL and mixed lines,
including wrapped rows, combining marks, font fallback and ligatures. Preserve
logical Home/End/document behavior unless a separate specification changes it.
Then capture native Linux light/dark Source and Live Preview: Hebrew click,
Left/Right, Shift+arrows, drag selection, copy and exact save, with English and
Russian positive controls. IME preedit/cancel and candidate geometry must remain
correct. A vendor adapter that cannot satisfy these cases is not permission to
patch GPUI core or weaken acceptance.

Raw local trace: `~/.cache/tessera-qa/737/native-shaping.log`.
