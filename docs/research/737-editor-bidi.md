# #737 — shared editor bidi geometry

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

## Problem and native diagnosis

The shared editor delegated caret and pointer positions to GPUI's line helpers.
Those helpers assume either ascending source indices or ascending X in the
platform glyph collection. Neither ordering holds for all RTL layouts.

A Linux native shaping probe, without projection or IME, produced these results
for `abc שלום xyz` at 16 px. The ASCII positive control round-tripped every
boundary correctly in the same process.

| UTF-8 boundary | Old caret X | Old hit-test at that X |
| --- | --- | --- |
| 4 | 33.84375 | 10 |
| 6 | 33.84375 | 10 |
| 8 | 33.84375 | 10 |
| 10 | 33.84375 | 10 |
| 12 | 69.25 | 12 |

The Hebrew glyphs themselves had distinct correct positions. For pure Hebrew,
source indices increased while X decreased, breaking the inverse hit test too.
Selection also assumed that the logical end was right of the start. This explains
both the stuck caret and the whole-word highlight for one selected Hebrew letter.

## Vendor repair boundary

GPUI core stays unmodified. Patch `0036-editor-bidi-geometry.diff` adds a geometry
adapter in the shared vendor editor:

- Group glyphs by source grapheme and sort visual extents independently of the
  platform's glyph iteration order. Resolve edge orientation with Unicode bidi
  levels. Cache the result alongside the shaped rows; plain LTR lines retain the
  existing fast path.
- Preserve `(source byte, directional affinity)` separately from soft-wrap
  affinity. A logical byte offset at an RTL/LTR boundary can own two X positions.
- Resolve pointer hits through the visual cell under the pointer. Visual arrows
  select the edge of the cell crossed, not an arbitrary equal-X neighbor. That
  distinction matters for Shift+Left across the final Hebrew letter.
- Draw selected cells as separate rectangles. A logical selection across mixed
  runs can be visually disjoint. Preserve a newline selection marker.
- Carry row affinity across soft wraps. At an unpainted adjacent row, retain the
  requested edge and resolve it after the row is laid out. Vertical hits retain
  their directional affinity; selection collapse compares visual endpoints.
- Normalize IME bounds so reversed endpoints do not create negative widths.
  Empty-range candidate bounds use the active caret affinity.

Source offsets remain UTF-8 bytes. The adapter never inserts bidi controls,
reorders canonical text, or disables projection to hide the issue. FileEditor,
revision checks and the disk-writing protocol are unchanged. Existing Undo/Redo
transactions still store source edits and selections, not visual ranks.

## Reproducible probes

With the full vendor patch stack applied and verified:

```sh
cargo test -p tessera-shell vendor_bidi_geometry::tests
cargo run -p tessera-shell --example bidi737
cargo run -p tessera-shell --example native_bidi737
```

`bidi737` imports the actual vendor module and asserts native glyph geometry for
ASCII, pure Hebrew, mixed English/Hebrew, niqqud, numbers, a ligature sample and
Cyrillic/Hebrew. It verifies logical pointer ownership as well as X. Three unit
regressions cover two positions at a directional boundary, crossed-cell ownership
and disjoint selection with an ASCII positive control. These tests are included
in normal shell CI because the vendored crate is not a root workspace member.

`native_bidi737` is an isolated real shared Editor widget. It observes editor
notifications and records source bytes, selection and cursor offsets. Ctrl+Alt+R
records state; Ctrl+Alt+L toggles projection. The provider refreshes after source
revision changes. Optional environment variables select a fixture text
(`TESSERA_BIDI_TEXT`), initial projection (`TESSERA_BIDI_LIVE`), dark theme
(`TESSERA_BIDI_DARK`) and a deterministic 10..12 selection (`TESSERA_BIDI_SELECT`).
The harness never reads or writes user notes.

Native Linux/X11 validation:

- Source and Live Preview traverse the first mixed line with offsets
  `0,1,2,3,4,10,8,6,4,13,14,15,16` while moving visually right.
- Shift+Left from 10 selects exactly 10..12 (`ם`). Left/Right collapse that selection
  to the corresponding visual endpoint. Replacement and Undo/Redo restore the
  exact source in both modes.
- A long wrapped paragraph reaches EOF in 241 visual arrows and returns in 241.
  A 22-line viewport case reaches EOF in 289 and returns in 289, without cycles
  or source changes.
- Paired Linux light/dark captures use the same widget, selected bytes and host
  session. Baseline was built without patch 0036 with the remaining stack verified;
  after was built with the complete verified stack. Baseline highlights the full
  word, after highlights only `ם`.

Evidence and drivers are under `~/.cache/tessera-qa/737/`. The first function-key
probe failed its positive control and is excluded from evidence. The successful
probe requires actual editor notifications, not merely a visible window.

## Merge gate and remaining acceptance

The PR must remain a draft until native acceptance in the complete Reader on
muninn: Source/Live Preview, light/dark, ru/he/en, fcitx5 preedit/commit/cancel,
candidate placement, wrapped-row selection, copy, exact save and Undo/Redo.
X11 key injection is not evidence of IME acceptance.

Within a multi-grapheme shaped ligature, caret positions use subdivision. Relevant
scripts/fonts need native coverage. Bidi resolution is over the displayed shaped
rows; paragraph-context behavior around soft wraps also belongs in acceptance.
Do not treat the local matrix as proof of complete Unicode bidi conformance.
