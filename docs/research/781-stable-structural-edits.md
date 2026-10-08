# S3a: structural edits without whole-document Source fallback

Tracked in #781, continuing #754. This slice is not ready for merge until the
native interaction matrix passes, including the remaining link-click and
mouse-up displacement reported on beta 8503.

## Current implementation

An edit spanning a contiguous run of accepted blocks and whitespace gaps can
synchronously classify at most 4 KiB and retain the other blocks' projection
and styles. Unsupported source between accepted blocks prevents local reuse. The global async
classifier remains authoritative. Global syntax and edits crossing unsupported containers still fall back
conservatively. Context-local runs above 4 KiB stay raw only within the edited
run; validated outer blocks retain their mapped projection and styles. Repeated
structural edits preserve this local-context provenance until async adoption.
This is incomplete acceptance, not a claim that all Enter paths are fixed.
The byte cap is not a measured time bound.

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

## Paint policy binding (#784)

Patch 0040 adds `SourceProjection::marker_scope_is_raw`, defaulting to raw for
providers without a reveal policy. `MappedProjection` delegates to its owned
`RevealSnapshot`. `PinnedProjection::marker_scope_is_raw(layout, current,
scope, safety)` checks the full LayoutStamp, source stamp and bytes before
consulting that policy. Invalid scope or safety coordinates conservatively
return raw. Selection, composition and explicit replacement may force raw
foreground without recomposing the pinned projection.

The decoration integration point is `prepaint.last_layout.projection`, using
that same prepaint's source, layout stamp and captured safety ranges. Do not
fetch another projection/provider from live editor state at paint time. A
missing pin, mismatched epoch or invalid geometry must paint the original row.
The existing drag path retains the same projection Arc; its immutable reveal
policy now follows automatically. This seam does not fix mouse-up recomposition
or authorize independently reconstructing the gesture policy in the renderer.

## Mouse release presentation lease

Patch 0042 removes unconditional projection invalidation on mouse-up, including
replayed clicks released before their first frame. The final mouse selection is
synchronized while the gesture still pins the displayed projection. Release
then retains that geometry. A later keyboard/IME active-range change, explicit
IME replacement, changed source revision or disabling projection ends the lease.
The provider may adopt in the meantime, but cannot reinterpret the displayed hit
map merely because the mouse button was released.

The gpui-kit gesture regression checks release preserves projected text and epoch,
then uses keyboard movement as a positive control for deferred provider adoption.
This is not a claim of native wide/narrow anchor or full S3a acceptance; those
measurements remain required on the final integrated build.

## Local-context oversize and initial timing probe

A structural edit above 4 KiB no longer drops all outer projection solely because
of the local parser cap. Only the dirty run is raw, with no stale inline styles;
its validated outer ranges and source maps survive repeated edits before adoption.
Global syntax guards still win over this retention. This does not promise stable
geometry inside the dirty run when async classification eventually arrives.

The ignored `structural_edit_timing_probe` compares remap+projection with full
classification+projection on the same host, checking equal displayed text each
iteration. An initial unoptimized maestro run (100 samples, other builds active)
reported local/full median milliseconds: 1 block 0.099/0.146; 100 blocks
7.001/19.555; 600 blocks, 18,612 bytes 43.151/123.571; 600 blocks, 58,612 bytes
57.611/139.380. Local p95 for the last case was 87.425 ms (max 156.529 ms).
These are diagnostic debug measurements, not a release frame-budget PASS.
The parser cap alone does not bound whole-plan mapping/projection cost. A controlled
optimized run and isolation of projection rebuilding are still required before
claiming the synchronous path meets the interaction budget.
