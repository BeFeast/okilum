# Guarded decisions for linked Maestro approvals

Refs #236. P1 implementation contract extending existing Maestro observation and
local operation recovery. Provider wire is Maestro `docs/guarded-approval-api.md`,
SHA-256 `bd9d520195e83c8c9b1e22e4cba40a568a2aa10cd98146413990652d0d44b12f`.
Only advertised JSON-backed `merge_pr` review is eligible. Missing/invalid guarded
review or capability remains observation-only; no unguarded POST fallback.

## Implementation seams

- `maestro.rs`: bounded, strict provider expected/review/receipt types; retain raw
  exact review and immutable receipt separately from display/execution status.
  POST uses encoded exact approval ID and project query, unchanged expected,
  actor/reason and explicit approve/reject. No transport retry.
- `maestro_operations.rs`, `runtime/maestro_operations.rs`: extend the existing
  typed request and journal with `approval_decision`; bind operation UUID, goal,
  original link ID, configured instance, displayed review, verb and reason.
  Persist the exact intention before I/O. Every persisted pending decision is
  conservatively possibly sent: later 4xx, disabled sends or missing GET proof
  cannot reject or archive it. Only a matching original receipt commits it.
  If no intention exists before the send boundary, an atomic rejected tombstone
  with `rejection.never_sent: true` may prove no send began and prevent a delayed
  first begin. Pending/committed state always wins that race. Native archives
  only this exact durable never-sent proof, never an error string.
  Existing pending/committed/rejected
  meanings remain; only a matching provider receipt proves committed. A provider
  rejection decision is a committed decision receipt, not a failed local request.
- `service.rs`: network outside owner mutex; new sends/retries require the exact
  active original link and endpoint. Before the first send, review project
  UUID/name/repo and target.issue must equal the linked project/repo/issue, in
  addition to original goal/link and configured instance/origin equality.
  Read-only reconciliation uses retained
  original identity and remains available after unlink and in maintenance.
  Late replies commit only to the original goal/link history. Unknown outcomes
  remain pending; there is no automatic POST retry and no remote abandonment.
- `runtime/maestro_links.rs`: additive decision receipts in existing goal-owned
  Maestro history/source projection. Execution status is observation, never a
  Okilum ResultRecord, criterion acceptance or stage completion.
- Native `maestro_ui.rs` and `native_outbox.rs`: explicit exact review (repo,
  issue, PR, full head, summary/risk/evidence), retained request before send,
  exact matching receipt and explicit retry/GET reconciliation. Goal/workspace
  switches cannot retarget a request. Existing link/unlink branches become
  exhaustive; approval requests never inherit local-abandon semantics.

## Durable fields and fallback

Use the existing `maestro_journal.operations` and
`maestro-links-enrollment.json`; do not introduce a second operation store.
Before an approval intention or decision projection is written, add versioned
`maestro_approval_decisions: 1` to the existing enrollment marker, preserving
`maestro_operation_dispositions: 1`. New readers accept legacy markers; legacy
readers reject the additional identity field instead of silently dropping state.
Link/unlink enrollment must preserve the newer marker on later operations.
An explicit retained request may enroll a never-sent refusal when capability
disappeared after review. Missing-capability browse/review alone never enrolls.

Native decision outbox entries use `tessera-maestro-outbox/v2` within the existing
Maestro directory; current readers accept v1 link/unlink and v2 decisions.
Previous readers refuse v2 entries. Request, done and rejection markers retain
exact immutable bytes. A sent uncertain decision is never locally abandoned.

Build a same-schema maintenance binary before any real enrollment. It disables
all new approval POSTs, including explicit retry, while retaining pending records,
matching receipt replay, GET reconciliation, observation and existing unlink/
local-operation recovery. A disabled send does not reject an already uncertain
remote decision. Existing pre-control binaries are downgrade-refusal controls,
not fallback candidates. No live enrollment, provider deployment or rollout is
part of this implementation.

## Evidence

For a configured managed Maestro connection, `maestro_observation`, `maestro_link`
and `maestro_control` remain true in both enabled and guarded maintenance builds.
Only `maestro_approval_send` changes from true to false with
`NEW_DECISIONS_ENABLED=false`. The #237 update helper incorrectly expected
`maestro_control=false`, so it rejected both healthy binaries; mocked health
expectations had missed the actual configured capability contract. Do not infer
approval-send permission from `maestro_control` or apply this matrix to an
unconfigured connection or the separate `NEW_LINKS_ENABLED=false` profile.
`scripts/verify-maestro-maintenance.py --guarded` checks and records these actual
RPC capability subsets before they are reused as update evidence. Its negative
control changes only `maestro_control` to false in a copy of the enabled response
and requires the same capability assertion to reject it.

Use isolated provider fixtures for exact POST and receipt matching, stale review,
opposite decision, lost reply, reminted IDs, disabled capability, unlink/late
reply, restart and maintenance. Prove zero automatic POSTs with a positive request
control. Native verifies review/readability, explicit decision, separate execution
status, retained uncertainty and goal ownership. Preserve ordinary Maestro
observation, link/unlink, Source and Context behavior. Fake provider/executor only.
