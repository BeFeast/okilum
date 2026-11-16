# Bounded grapheme lookup in bidi geometry

Refs [editing lag](https://git.oklabs.uk/BeFeast/okilum/issues/2803) and
[large edit tools](https://git.oklabs.uk/BeFeast/okilum/issues/2804).
Baseline: main `7c20139935f7305352705c4f99686849317023f2`, gpui-kit
`928c3eb776a3d733d9b771f7dea27a6a79242ced`, unchanged app-lock GPUI 0.3.3.

## Mechanism and bounded change

`input/editor/display_map/text_wrapper.rs::set_wrapped_lines` builds
`Geometry::new` for shaped rows when the line needs bidi geometry. The same
geometry adapter serves Reader selections. `Geometry::from_layout` first builds
sorted, unique grapheme boundaries, then coalesces platform glyphs into clusters.
Previously, each cluster filtered the complete boundary vector and allocated a
new vector, even when that cluster covered one grapheme. With G clusters and B
boundaries this lookup performed G × B predicate visits.

Patch 0147 borrows `boundaries[first..last]`, using partition points for `< start`
and `<= end`. This returns exactly the previous inclusive filtered sequence.
It removes the per-cluster vector and bounds lookup predicate visits to
`2 × G × (ceil(log2(B)) + 1)`. Existing glyph coalescing, visual sorting, bidi
levels, arithmetic, ligature subdivision, geometry cells and source offsets stay
unchanged. No threshold, cache policy, shaper, core API or Tree edit changes.
The existing provisional equal subdivision of ligatures is preserved, not newly
validated against native ligature carets by this patch.

## Controls and claim limits

Two test-only counters observe the actual cluster loop and partition predicates.
The 4,096-ASCII-grapheme + Hebrew/emoji test requires both counters to fire,
complete geometry, and the calculated per-cluster work bound. A compiled
old-production negative was run outside the repository with the full filter and
predicate instrumentation restored: 16,822,302 visits exceeded the 114,828 bound.
That negative-control variant does not ship in the patch stack. The differential
oracle retains the old implementation for small mixed RTL/LTR, combining, emoji
ZWJ, isolates, tabs/CRLF, fallback glyphs
and multi-grapheme clusters. It compares exact cells, caret positions, movement,
selection, hit-testing and boundary ownership; no geometry tolerance is added.

These are source-work and geometry controls. They do not establish native input
timing, explain the earlier timed-out virtual-frame control, attribute any of
the retained aggregate 64–67 ms to this function, or prove RSS/whole-issue
acceptance. Source materialization, glyph copying/suffix replay, correct full
Unicode/TAB shaping, Tree traversal/sharing and Parser/Rope ownership remain
separate residuals. XML functional worker acceptance has separate evidence;
XML rendering/performance and platform/native behavior remain unfinished.

## Upkeep and upstream plan

This is an ordered idempotent gpui-kit diff verified by `vendor-setup.sh`.
The owning upstream is [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit),
`crates/base/src/input/bidi_geometry.rs`; the adapter was introduced by Okilum's
patch stack. No gpui-pre or tree-sitter change is needed. On vendor upgrades,
retain the sorted-boundary invariant and differential controls. A future upstream
submission would carry the general geometry change and tests with its owning
adapter; no upstream issue or PR is opened under this task.
