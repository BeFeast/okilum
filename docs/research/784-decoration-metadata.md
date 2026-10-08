# Live Preview marker paint (#784)

Live Preview replaces unordered markers with • / ◦ / ▪ by nesting depth, draws
quote bars and thematic rules, and preserves the original shaped rows. Ordered
numbers, task checkbox source and RTL rows retain their existing rendering.
The source buffer, glyph advances, indentation, wrapping and row height are not
changed. Revealed markers use their original foreground.

## Metadata and frame ownership

A separate metadata-only extractor consumes the classifier's already guarded
Comrak AST. It does not change formatting traversal, Plan/Region, styles, links,
reasons or marker_scopes. Each marker and scope has grapheme-aligned endpoints;
markers are contained in scopes, and scopes may nest or be equal but never cross.
Invalid coordinates, ambiguous prefixes, overlapping markers or limits discard
the inventory. Ordered/task, frontmatter, setext and fenced-code cases are
classified from the AST and exact authored delimiters. Quotes behind a list
marker conservatively retain raw paint in this first slice.

Metadata is exposed only for the exact classified Snapshot. It is never remapped
with retained presentation after an edit. The bridge stores it with the immutable
projection and the shared RevealSnapshot. The shared core API was taken from
editing executor's ce6dcb1; only the required API and tests were imported, without
S3a's remap/local-parse implementation. Binding 0040 comes from cb28904 in #790.

Prepaint consumes its LastLayout's PinnedProjection, LayoutStamp, source and safety
ranges through marker_scope_is_raw. Selection/IME/replacement safety may only
force raw paint; there is no live-provider recomposition in paint. The painter
also verifies exact source before suppressing foreground. Missing/stale/invalid
pin data uses original foreground.

The optional 0041 seam partitions already-shaped visual rows using split_at.
Glyph backgrounds and selection keep their original paint pass. Invalid grapheme
or glyph-cluster boundaries, non-monotone glyph indices and RTL make the WHOLE
visual row raw, including replacement shapes. Existing whitespace visualization
and non-left alignment also retain original paint. Bullet replacements stay in
marker bounds. Quote continuation bars use only existing free indentation; if a
wrapped continuation has no free gutter, no bar is drawn over its text. Thematic
rules use the existing content width and row, with no additional block spacing.
No gpui-core modifications or second layout/shaping of body text are used.

## Native evidence

The isolated native_marker784 fixture runs on CT141/Xvfb with private HOME/XDG.
Before/after below compare raw foreground (F7 Source) and decorated Live Preview
on the SAME binary, source, viewport and machine. The fixture has no unrelated
concealed formatting. These are on/off geometry controls, not screenshots from a
separately built main binary. No user vault or system daemon is involved.

| Theme / width | Before | After | Anchor ΔY |
| --- | --- | --- | --- |
| light / wide | [raw](https://oklb.uk/amber-hound-9543) | [decorated](https://oklb.uk/rapid-koala) | 0 px |
| light / narrow | [raw](https://oklb.uk/lucky-bison) | [decorated](https://oklb.uk/gentle-stoat-1788) | 0 px |
| dark / wide | [raw](https://oklb.uk/snug-swan-0179) | [decorated](https://oklb.uk/merry-owl-2991) | 0 px |
| dark / narrow | [raw](https://oklb.uk/golden-fox) | [decorated](https://oklb.uk/merry-hare) | 0 px |

Wide is 1100×850; narrow is 660×920. Caret entry and the exercised drag selection
also gave ΔY=0. Scrolling moved the measured anchor by -144 px (positive control).
A typed positive-control edit changed the source hash; native Undo restored the
exact 494-byte fixture and SHA256
06cb08bbd16b651cf58daa9fb7bb1a6d570e31f69d898ca17e7e78a45cf09ea4.
The core FileEditor roundtrip separately verifies that saving canonical source
retains markers, Unicode and CRLF bytes. No end-to-end application Save UI claim
is made by this isolated editor harness.

Core tests cover grapheme endpoints, scope nesting, AST syntax boundaries, stale
metadata and projection on/off invariance. Bridge tests cover immutable pin policy,
selection/IME/replacement safety, invalid/stale stamps and delayed metadata.
The decoration784 shaping spike checks original glyph IDs/positions with Latin,
Cyrillic, ligatures, combining marks, emoji/ZWJ and raw RTL controls. The cumulative
vendor patch stack is verified before native builds.

Remaining acceptance: editing-executor seam review, broader S3a mouse-up/gesture
acceptance and Mac QA after publication. The measured click/drag result above is
limited to the fixture and does not claim completion of S3a. #588 stays paused.

### PR review: marker repaint cost

Marker-free, missing-pin, stale-pin and unsupported-alignment paths return before
reading canonical bytes or segmenting graphemes. Prepaint reuses the pinned
immutable source Arc. A per-editor, single-entry cache retains exact-source
validation and grapheme boundaries for that source identity and Arc; it never
caches reveal decisions or row geometry. A new Arc is checked against the Rope
without allocating a String before being admitted. Paint only checks the canonical
source generation, which also advances for silent IME mutations, against the
prepaint snapshot; it never copies or scans the document or queries a provider.
The cache regression includes a positive source-read control, panicking readers
on zero-work paths, pointer identity for reused boundaries, and wrong-byte
rejection despite an equal stamp. Existing per-row cluster/grapheme checks remain.
