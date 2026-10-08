# S3a: structural edits without whole-document Source fallback

Tracked in #781, continuing #754. This slice is not ready for merge until the
native interaction matrix passes, including the remaining link-click and
mouse-up displacement reported on beta 8503.

## Current implementation

An edit contained in one accepted block can synchronously classify at most
4 KiB and retain the other blocks' projection and styles. The global async
classifier remains authoritative. Global syntax, cross-block edits and larger
blocks still fall back conservatively; this is incomplete acceptance, not a
claim that all Enter paths are fixed. The byte cap is not a measured time bound.

`RetainedPresentation::prepare_reveal` returns an immutable `RevealSnapshot`.
Its projection and `is_raw(current, scope)` share the same validated selection
and composition. The shell independently validates explicit replacement and
merges it into composition before preparing the snapshot. `MappedProjection`
owns that snapshot, so the policy cannot drift from the displayed projection.
Stale source identity or invalid grapheme scope returns raw visibility.

## Native integration still required

The editor must bind the prepared decision to the same LayoutStamp and gesture
epoch as its projection. The #784 paint adapter must consume that decision,
not recreate reveal from the latest caret. A live selection/IME/replacement
intersection can force marker foreground raw without changing the pinned hit
map. Mouse-up alone must not recompose into different line geometry. The core
snapshot does not implement these native lifetime rules by itself.

The sync executor owns marker extraction and paint-only decoration. Its
metadata-only AST traversal must not change formatting acceptance, projection
maps, source advances or wrapping. No marker metadata is added by this slice.

## Validation

Core tests compare local structural classification with full classification,
check neighboring projected blocks, UTF-8/CRLF and conservative fallbacks.
Reveal tests cover frozen decisions across selection changes, IME, exact
revision identity and grapheme boundaries. The controlled native Enter probe
uses delayed classifier delivery and a failing baseline as positive control.
The full Reader/Wayland wide/narrow, light/dark, typing, click, drag and IME
matrix remains required before merge; existing probe evidence is only for the
bounded structural-edit path.
