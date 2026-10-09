# Discussion decision reuse settings

[#261](https://git.oklabs.uk/BeFeast/okilum/issues/261) adds one explicit automatic → manual-only transition. Implementation evidence and native acceptance are tracked in the issue; this document does not authorize installation. It does not add reactivation, semantic supersession, general search exclusion or Attention reply disposition.

Context → saved Discussion decision → Reuse settings shows the exact read-only user text. Stop adding automatically confirms the transition; Cancel leaves canonical bytes unchanged. Manual selection only remains discoverable through Inspect/Open original. Explicit Include in context is still available. Historical conversation text, reviewed packets, stages and results remain exact. A pin to the previous source revision becomes stale and needs explicit ordinary re-review; it is never silently deleted/refreshed.

## Canonical authority

The same `records/discussion-decision-<id>.md` file keeps its stable ID, brain/goal/actor, verification, timestamp, complete `discussion_origin` and exact body. Only the canonical single-line `record_type` token changes from `discussion-decision` to `discussion-decision-manual`; a deterministic top-level `reuse_disposition` block is appended immediately before the closing YAML delimiter. Noncanonical ambiguous layouts are refused without normalization.

The disposition contains schema `okilum-discussion-decision-reuse/v1`, mode `manual_only`, fresh review operation UUID, original actor, UTC time and exact predecessor revision. Removing that exact block and reversing the kind token reconstructs every predecessor byte. Its hash must equal the retained previous_revision; original #253 identity/provenance admission still applies. A known create journal must match the entire predecessor, not just selected fields. Portable canonical-only records validate the same bindings without inventing a journal acknowledgement. Disposition actor must equal original actor; first mutation also requires the configured local actor.

Complete source remains bounded to 8 KiB. A policy that cannot fit is refused before Save; text is never truncated. Unknown/contradictory kinds or policy versions never become automatic. This is one canonical transition record plus existing SourceStore receipts, not a new ledger.

## API and recovery

All commands require exact workspace routing and capability `discussion_decision_reuse`:

- `discussion_decision_reuse_get`: goal_id, decision_id; returns bounded current source, state/eligibility, exact text and citation. Optional operation_id requests only the bounded correlated terminal-conflict view for that exact goal/decision operation. It never uses unbounded source_conflict.
- `discussion_decision_reuse_write`: goal_id, decision_id, exact SourceWrite request and real SourceSnapshot base. The shared transform validates the exact delta. SourceStore CAS uses the reviewed revision; the source remains unchanged on conflict.

Each fresh explicit review uses a fresh UUID, retained before send in the existing EditorRecovery guarded request. No generic SourceWrite downgrade or automatic merge is allowed. Unknown Save remains protected through restart. A positively correlated terminal conflict can be discarded locally, followed by fresh review with a new UUID, including when external edits restore the same old source revision. No automatic replay occurs on startup.

Historical successful receipt lookup precedes pending projection flush, current source checks and current-actor eligibility. It binds the original exact request/base and workspace brain; returning it never recreates missing/changed current bytes. Current readback is separate. Pending records require coherent exact preimage/revision and no divergent observations. Journal reads are limited to 64 KiB before deserialization; current decision/conflict reads are limited to 8 KiB before allocation. A bounded preflight protects SourceStore's retained preimage under the existing managed-writer exclusion boundary; arbitrary concurrent external filesystem growth is outside that guarantee.

## Automatic consumers, export and compatibility

Manual-only records are absent from brief `inputs`. A separate UI summary contains only id/path/fixed status, at most 20 entries and 2 KiB; truncation is explicit. No body, citation or user-provided title enters that summary, because proposal generation serializes the whole brief. If no manual-only records exist, every new key is absent and the complete old brief JSON/generation stays exact.

Future automatic Discussion and newly captured proposal context omit the stopped source. A real policy change may cause existing proposal retry InputChanged; captured requests are not silently refreshed. Explicit manual selection remains under existing scope/revision/budget authority. Stopping reuse itself starts no provider/task/engine.

Exact Markdown archives preserve policy without operational journals. AI-package export remains an explicitly reviewed historical artifact. #253 readers reject the new kind automatically with or without create journals; their display may say unsupported. Earlier readers discover only `decision-` filenames. Binary rollback keeps policy bytes intact. Restoring pre-policy source is a separate explicit reversal of user data. Old editor recovery readers must preserve unsupported guarded drafts for the new reader instead of converting them into generic writes.
