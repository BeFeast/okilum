# Source projection mapping foundation (#212)

This is the first child of [managed Live Preview](managed-live-preview.md), not a
Markdown classifier, native editor adapter or completed Live Preview mode. The
module is `tessera_core::source_projection`; no shell, source API or vendor call
uses it yet.

A `Snapshot` owns immutable full authored UTF-8 text, document identity and a
monotonic generation. A `Plan` binds that exact snapshot and contains explicitly
classified regions: `Source` for unsupported syntax, or a block with approved
conceal spans. The future classifier must establish those semantic ranges. This
mapping layer checks their coordinate and resource safety; it does not infer
whether arbitrary supplied spans are valid Markdown delimiters.

`project` returns a `Projection` or `SourceFallback`. Every rejection exposes the
**current exact Source**, including a stale plan from another snapshot. Unsupported
regions and uncovered bytes remain authored Source in place. There is no approximate
mapping, source normalization, renderer-to-source reconstruction or canonical write.

## Hard limits and validation

- Source and projected output: 256 KiB. Document identity: nonempty, at most 4,096 bytes.
- At most 4,096 total block and marker ranges. Input must already be sorted and
  disjoint; overlapping, inverted, empty or out-of-bounds ranges are rejected.
- At most 2,097,152 deterministic work units. Callers may lower this budget, never
  raise it. Byte scans/copies and fixed allowances for bounded range lookups consume
  fuel. This is a finite-work bound, not a measured native frame deadline.
- Every range endpoint must be an extended grapheme boundary in canonical UTF-8.
  CRLF is one boundary unit. Conceal spans cannot contain CR, LF or BOM.
- A conceal anchor must also be a grapheme boundary in the projected display.
  Removing syntax between two regional indicators, for example, must not join them
  into a flag and leave a caret position inside that new grapheme.

Plans and maps require both document/generation identity and exact source equality.
A reused generation with changed text is stale. A plan is not valid again merely
because later edits restore its old bytes with a newer generation. Source fallback
clones the immutable source reference instead of constructing another large buffer.

## Mapping and editing seam

Visible grapheme boundaries map directly. Hidden source boundaries collapse to a
single display anchor. At that anchor, `Bias::Left` means before all hidden bytes
and `Bias::Right` means after them. Adjacent conceal spans share one combined anchor.
For every valid source boundary, the display anchor's left/right source bounds
enclose it; either bound maps back to the same display position. Both bias choices
for every display boundary round-trip to that same display position.

`Active` carries canonical selection and composition ranges. All touched blocks
reveal their markers; an active caret on a shared block boundary reveals both
blocks. `reveal_selection` translates a visual selection with explicit endpoint
biases for rebuilding the projection before editing. A collapsed caret requires
one consistent bias and cannot accidentally expand over hidden source bytes.

`Snapshot::copy_source` copies exact authored spans. `replace_source` is a pure
canonical replacement that validates the expected snapshot and grapheme range,
preserves surrounding bytes, checks output size and increments generation without
wrapping. It returns a new snapshot; it does not apply it to a GUI buffer, file or
backend. Native selection, IME conversion, undo transaction ownership and actual
editor application remain later work. Mode/reveal updates must never become edits.

## Evidence and remaining stage gates

Tests exercise mapping equivalence classes and round trips, explicit bias values,
source clipboard and canonical edits, selection/composition reveal, stale identity,
malformed plans, hard bounds, source-only unsupported regions, grapheme joining,
Unicode/CRLF/BOM and final-newline preservation. They do not establish native IME,
caret movement, clipboard integration, UndoManager behavior or GUI latency.

A semantic classifier and real native caret/IME/undo evidence are still required
before stage 1 of #211 can be considered complete. The later adapter must preserve
the existing single canonical buffer, source recovery and #206 CAS/receipt rules.
