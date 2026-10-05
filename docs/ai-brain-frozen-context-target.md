# Frozen context target preparation — issue179

The private `context::frozen_target` helper prepares one immutable destination for
a future adoption coordinator. It does not persist a journal, reserve an operation,
write a source, change a selected packet or mark a proposal adopted. No API/CLI or
production caller invokes it in this slice. The ordinary context creation and
review/hash functions are unchanged.

Preparation accepts the complete inspected form: goal identity and expected source
revision, query, scope, exact citations, pinned citation IDs, and final edited
manual guidance. It checks current goal/source ownership, scope, revision, citation
provenance and the existing64KiB guidance-plus-excerpts budget. The entire
canonical Markdown, including scope/citation metadata, must also fit the existing
context reader's256KiB limit; the serialized frozen wrapper is capped at1MiB. Guidance is preserved
byte-for-byte, including whitespace and CRLF. Pin identities and citation order are
preserved; the helper never replaces them with an inferred selection or generated
text. A context packet remains explicitly unreviewed after preparation.

The helper allocates the packet ID, SourceWrite operation ID and UTC timestamp
once. Its result holds the exact create-only SourceWrite bytes, source revision,
packet/goal identity and timestamp. Only preparation and checked restore construct
the private target wrapper; there is no public arbitrary target-ID/SourceWrite
input. Restore validates owner, path, bounds and metadata consistency without a
Runner, clock, ID generator, or current source/goal reads. It does not reserialize
canonical Markdown; the original base64 content survives serialization/reopen even
when the source, goal, time or serde_json feature graph changes.

The future coordinator must bind this object to a durable adoption intent and
workspace operation reservation *before* writing it. Exact SourceStore request and
receipt reconciliation must precede canonical adopted projection. An already
committed exact operation replays its original target receipt before checking later
freshness. New operations still require fresh sources/proposal/form inspection.
This helper alone provides none of those transaction or idempotence guarantees.
It must not replay an older whole application journal or overwrite a newer selected
packet pointer after a lost response.

Tests prove frozen bytes/IDs/time survive source and goal changes; new preparation
refuses stale or cross-owned input; pin/citation identities and manual text are
retained; existing context readers accept the target while requiring ordinary
explicit review; malformed restored ownership/bytes/review flags refuse; and the
combined guidance/excerpt budget is enforced. No native test is required for this
unused internal helper. Full release feature-graph maintenance compatibility remains
a gate before any production adoption/enrollment.
