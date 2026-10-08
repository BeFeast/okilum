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

## Native geometry prototype

The example now includes an isolated `bidi737/geometry.rs` adapter. It groups
shaped glyphs by source grapheme, sorts visual extents independently of platform
iteration order, and uses Unicode bidi levels to orient leading/trailing edges.
It preserves both candidates at a directional boundary and constructs selections
from visual cells rather than only two logical endpoints. Only a dev dependency
on the already locked `unicode-bidi` package is added; production is unchanged.

On Linux/X11, eight real shaped fixtures pass the probe assertions: ASCII, pure
Hebrew, Hebrew within English, English within Hebrew, Hebrew with niqqud, mixed
Hebrew/numbers, a Latin ligature sample, and Cyrillic with Hebrew. Specifically,
source boundaries 6/8/10 in `abc שלום xyz` have distinct decreasing X coordinates;
selecting 10..12 occupies only the last Hebrew letter; selecting 0..6 produces two
disjoint spans. Every visual cell has positive width and its nearest-edge hit test
returns the corresponding visual position. The ASCII control still round-trips.

This is **not** complete editor acceptance. Pointer hits at a directional boundary
still require choosing and retaining logical affinity; this probe deliberately
checks visual position rather than claiming both logical indices round-trip from
one X. Soft-wrapped rows must retain paragraph bidi context. Ligature subdivision
is provisional, and real caret positions/fallback behavior need further evidence.
Integration must replace the shared editor's caret, selection and movement paths
together; patching only `x_for_index` would leave wrong selection and clicks.

Trace: `~/.cache/tessera-qa/737/native-geometry.log`. Build and strict example clippy
logs are adjacent. No UI before/after or product fix is claimed by these assertions.

### Directional affinity and pointer ownership

The prototype now represents a caret as `(byte index, Before|After)`, independently
of the existing soft-wrap affinity. It resolves an X hit to the containing visual
cell before choosing its nearest edge. This fixes a weakness of the first probe:
globally nearest equal-X edges could return the right coordinate with the wrong
logical offset. Native assertions now verify the logical index as well as X for
both sides of every cell in all eight fixtures.

For `abc שלום xyz`, offset 4 has two distinct positions depending on affinity.
Visual Left/Right traverses increasing/decreasing X and retains edge ownership;
stepping back restores the prior visual position. The probe does not claim that
every equivalent equal-X logical state must be identical after a round trip.
Native run `native-affinity.log` exits successfully. This narrows the integration
contract but remains isolated: product clicks, selection, IME and wrapped-row
state have not yet been converted, and #737 is still open.
