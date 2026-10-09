# Independent Markdown merge preview

Approved bounded scope: [issue148](https://git.oklabs.uk/BeFeast/okilum/issues/148).
This is an incremental assistant toward automatic independent-edit merging, not
silent automatic save. The managed Source conflict surface exposes the explicit
preview/adopt flow; actual native acceptance is recorded separately.

## User flow and authority

On an existing source conflict, **Preview merged draft** computes a candidate
separately from the editor. Only a demonstrably independent text result enables
**Use merged draft**. Adoption must first persist the exact candidate through
[editor recovery](ai-brain-editor-recovery.md); only its matching durable
acknowledgement can replace protected local text. **Save resolved draft** remains
the existing explicit, revision-guarded canonical write.

Preview/adoption do not write canonical Markdown, call an LLM, complete a task,
start an engine or replay an uncertain Save. Original base/current/proposed conflict
versions remain recoverable. A clean text merge is not semantic verification.

## Pure helper interface

`brain/merge_preview.rs` supplies:

```rust
preview_merge(base: Option<&[u8]>, current: Option<&[u8]>, draft: &[u8])
    -> MergePreview
```

`MergePreview::Ready { text, current_edits, draft_edits }` contains exact candidate
UTF-8 text. `MergePreview::Manual { reason }` retains manual resolution. Reasons
distinguish missing base/current, unsupported UTF-8, input/work limits, ambiguous
alignment and overlapping edits. `ManualReason::message()` provides plain English
copy; the counts are helper diagnostics, not a new required user decision.

The helper accepts bytes only, performs no I/O, and owns no workspace or journal.
Before calling it, the native owner validates and freezes full workspace identity,
note path, conflict ID, exact base/current bytes and revisions, and the protected
local draft ID/generation/text. A pending uncertain Save cannot be bypassed by
computing or adopting a replacement. Use the latest protected local draft as the
proposal; retain the original rejected proposal in the existing conflict record.

Run computation on a background task. Late results are accepted only if every
frozen input still matches. A local edit, navigation, conflict refresh or another
workspace invalidates the preview instead of silently adopting stale text.

## Conservative merge policy

The helper uses pinned `similar`2.7.0 Myers diff with a deadline. It splits only
after LF, retaining LF/CRLF, BOM, Unicode and final-newline presence in the exact
tokens. No Markdown parsing, newline conversion or semantic merging occurs.

Equal complete sides and unchanged sides have exact byte-preserving results.
Otherwise, it combines disjoint base-line ranges and deduplicates identical edits
at one location. Different edits to one line remain a conflict even if different
words were changed. Deletion/replacement overlap, competing same-anchor insertions,
and insertions touching a changed range boundary remain manual.

Ambiguity is deliberately conservative: removed/replaced lines must occur only
once in the common base; insertion neighbors must be unique when present. Added
lines that already occur in the base are treated as possible moves/copies and
remain manual. This can refuse otherwise mergeable notes, particularly repeated
blank lines or boilerplate, but never guesses their intended alignment. Unrelated
repeated lines outside the edited ranges do not alone prevent a merge.

Each side is reconstructed from its reported edits and checked against its exact
input before combination. A coarse diff returned after the library deadline is
refused, never interpreted as demonstrated non-overlap. No conflict markers are
inserted into canonical text or automatically accepted drafts.

## Bounds and persistence

All three inputs and the merged output are individually limited to256KiB and each
input to4096 lines. The complete calculation has a100ms elapsed budget, checked
around the library diff and assembly. A refusal retains the existing manual editor,
which supports larger sources. There is no custom quadratic diff implementation.
Scheduling delays may cause a truthful work-limit refusal; they must not stall the
UI or be called successful merging.

After adoption, keep the same retained conflict and persist candidate text using
#145's compare-and-update semantics. A later explicit Save uses the exact displayed
current snapshot, its revision, candidate bytes and a new durably retained UUID.
A second writer causes another ordinary conflict; a lost response replays only the
same explicit Save through existing recovery. Do not automatically re-merge and
retry races. A recovered adopted draft remains protected independently of the
discardable preview calculation.

No source API, backend state/schema, runtime binary or preserving fallback change
is required. The existing managed-writer boundary remains binding; this helper
does not make external uncoordinated writes safe.

## Native UI validation

The Source conflict panel computes off the UI thread against one complete frozen
workspace/draft/conflict binding. Exact snapshot SHA values are verified before
merge computation. Changed bindings invalidate late results and candidate controls.
Adoption temporarily holds editing/navigation while compare-and-update protects the
candidate; storage refusal leaves the visible original draft intact. Successful
adoption retains original conflict evidence and leaves canonical Save explicit.

GPUI integration coverage checks computation without adoption, exact BOM/CRLF
durability before replacement, retained conflict/base, stale current invalidation,
uncertain-Save refusal and another window's newer-generation CAS refusal.
