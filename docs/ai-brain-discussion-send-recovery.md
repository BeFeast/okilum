# Durable Discussion send correlation and recovery

Approved bounded implementation contract for [issue #244](https://git.oklabs.uk/BeFeast/okilum/issues/244), 2026-09-08. **Backend implementation and test boundaries: [evidence](ai-brain-discussion-send-backend-evidence.md). Shell/native and frozen old-writer acceptance are tracked separately.** This extends [Discussion context](ai-brain-discussion-context.md) and the [foundation source boundary](ai-brain-contracts.md) without changing their existing v1 formats. Source audit: `764c7fcd9cc7248de1ddf629905de035060dcc07`, merged as `7b29ef3c219e5891e34f0268427e6e67e42953ff` with the same tree.

## Scope and authority

For a separately named, capability-gated correlated send operation, use the desktop's durable operation UUID as **the SourceWrite UUID of that turn's initial conversation projection**. Reuse Runner's existing `pending_writes` and SourceStore's retained `RecoveryRecord`; add no second backend outbox or global operation scan. Subsequent streaming/final checkpoints keep their existing fresh write UUIDs. The desktop persists the original operation UUID, exact workspace/goal, nullable conversation target, message/manual-source selection and stable payload digest before its first network call, using the existing immutable native outbox/fsync primitive. Recovery is lookup-only and cannot route a retained entry back into send. Retain the pending operation and draft under their exact original owner. Reconnect or late replies must not silently switch the currently viewed workspace/goal; expose explicit navigation to the original owner.

The initial proposed conversation bytes carry a small, versioned adjacent metadata receipt containing operation UUID, original nullable target, brain/goal/actor binding, client-payload digest, resulting conversation/turn IDs and the frozen provider-body hash. Keep the existing `discussion_context` Turn/Envelope v1 unchanged. The original bytes are already retained in the SourceWrite journal; the new metadata does not duplicate transcript/provider bodies in another store. Current canonical metadata is useful for inspection, but **the original pending SourceWrite or retained RecoveryRecord is operational authority** even after later canonical edits/deletion.

## Verified source seam

- `source.rs:44` SourceWrite contains schema, operation_id, brain_id, path, expected_revision and content_base64. `source.rs:125` RecoveryRecord retains that full request, preimage, previous revision, optional receipt, optional conflict and divergent observations. No schema change to either type is needed.
- `runtime.rs:1134` queue_with_operation_checked currently generates a fresh UUID after constructing/bounding the projection. Add a narrow internal option for a caller-supplied initial Discussion operation UUID; all other callers retain generated IDs. `runtime.rs:917` checkpoint_discussion queues that exact SourceWrite, installs the application state, persists the Runner journal, then flushes canonical writes. Before SourceStore receives the operation, its complete identity/proposed bytes are already in durable pending_writes.
- `source.rs:1214` rejects a reused write UUID with different request/base and returns an existing receipt without rewriting. It durably stores intent/preimage before mutation, then records the write receipt after replacement+directory sync. `source.rs:1084` recovery_record(UUID) performs direct lookup under writer.lock and does not write/retry. The request payload remains retained as terminal receipt/conflict fields evolve.
- `service.rs:1643` currently prepares under the backend owner mutex and spawns the provider after successful preparation. Refactor its return into `NewlyCommitted(Prepared, Receipt)` versus `ExistingOperation(ReceiptOrPending)`; only the first successful new commit path can spawn. Return from SourceStore.write is not itself permission to spawn on a replay.

## Admission, lookup and ownership

Hold the existing backend owner mutex for classification and preparation. Before recomputing context or testing current conversation status, look for the requested UUID in current durable pending projections and the requested SourceStore journal file. Validate an existing operation against the original client digest and immutable owner before returning it; recorded UUID reuse for another goal/target/payload conflicts. No fresh conversation/turn ID is allocated on this branch. A prior operation must never become a new send because its conversation is now complete/running, sources changed, canonical metadata disappeared or the lookup cache is absent.

Only an actually new first-submission path may prepare and checkpoint. Lookup for a retained client outbox entry is a distinct read-only request: missing both pending projection and SourceStore record means **unknown**, not never accepted and not permission to submit again. Original network work may still be queued. A missing/deleted operational record cannot support an unconditional idempotent-resend guarantee; the client remains lookup-only. A deliberately new user submission uses a fresh UUID while retaining the unresolved original entry. Do not add a general tombstone/retirement service here.

Expose only a small validated typed response, never raw recovery bytes. `recovery_record` currently validates UUID syntax and JSON parsing but does not itself validate the returned request's brain/path/operation binding. The Discussion adapter must validate outer request schema and exact expected_workspace, requested canonical UUID versus stored request/metadata UUID, SourceWrite brain/path versus original goal/conversation, typed metadata version/payload digest, initial transcript/turn binding and stored request hash. If a WriteReceipt exists, validate its operation/path/previous_revision/revision against the preserved request and proposed-byte hash; a foreign source-edit operation ID or conflicting pending/journal records is an error. Use only consistent validated evidence. Contradictory receipt/conflict fields, or disagreement between Runner pending bytes and the SourceStore request, are explicit recovery errors. Any unavailable current conversation is reported separately, never reassigned. These are operation-provenance checks, not a new authentication subsystem.

## Status meanings and crash boundaries

| Durable evidence | Read-only result | Provider behavior |
| --- | --- | --- |
| Exact Runner pending SourceWrite; no terminal SourceStore receipt | Retained application intent; projection pending/unknown; original IDs available from validated bytes | No dispatch from lookup/replay |
| SourceStore record with neither receipt nor conflict | Projection intent retained; replacement may or may not have happened | No dispatch from lookup/replay |
| SourceStore conflict | Retained projection conflict; original attempt did not reach this service's post-checkpoint spawn | Preserve recovery; do not call it a clean rejected/unrecorded send |
| Valid SourceStore Written/Unchanged receipt | Original projection committed; return original conversation/turn/body hash | This does not prove provider delivery or completion; no replay spawn |
| No retained record, malformed/mismatched/over-budget/unavailable record | Unknown or explicit recovery error | Keep client outbox; no send |

A direct matching validation reply may declare `rejected_before_checkpoint` only before admission/persistence began, not based on generic text and not for projection conflict after application state was persisted. The desktop must persist that exact terminal reply before releasing its durable guard. Changed corrected input uses a new UUID. Server tombstones for every validation error are outside this slice.

Crash after Runner persist but before canonical flush preserves the same SourceWrite UUID/payload. Existing Runner recovery may complete the **canonical write** through its normal path; that is not an LLM retry. Read-only operation lookup itself must not invoke flush/write. Crash after canonical receipt but before provider spawn remains committed projection with dispatch unknown/interrupted; recovery never automatically sends it. Crash after spawn/before response is indistinguishable without additional provider evidence and receives the same conservative status. Current Application::recover already marks running conversations interrupted rather than resending. If existing startup fails because a canonical projection needs manual recovery, the desktop reports backend unavailable/unknown and retains the outbox.

## Bounded read requirement

Existing SourceStore.load uses unbounded fs::read. Reusing it unmodified would not meet a bounded transport. Add a narrow `recovery_record_bounded` equivalent that limits bytes while reading only the requested record, under the existing lock; do not globally change legacy reads or scan all UUID files. Validate Base64 and all lengths before exposing typed data.

For a normal initial Discussion projection, decoded proposed bytes are at most MAX_CANDIDATE (64 MiB), the source preimage read is at most MAX_CONVERSATION (128 MiB), and base is None. The two Base64 strings alone can therefore occupy 256 MiB. Freeze a serialized-record cap from `4*ceil(MAX_CANDIDATE/3) + 4*ceil(MAX_CONVERSATION/3) + bounded JSON/path metadata overhead`, with an exact serialized boundary fixture before choosing the final constant. A generic SourceStore record can have extra base/divergence history or unrelated larger inputs; those must yield a bounded explicit recovery error, never be silently truncated or treated as absent. Raw journal data never crosses native RPC; the response is only the small receipt/status. Pending projections are already loaded in Runner state, but their chat-specific proposed bytes still require exact candidate/metadata bounds.

## Compatibility

Keep legacy chat_start untouched and use a distinct new opcode/capability. The old tagged Command may ignore an unknown additional field on a known operation; optional operation_id on legacy chat_start is therefore unsafe. An old backend must reject the new opcode, and a retained new-client operation must never fall back to legacy send.

No new field is required in Application::Journal or Runner::State: old typed roundtrips would drop unknown fields, including on boot. No new field goes inside the deny_unknown_fields Turn/Envelope. Existing SourceWrite/RecoveryRecord formats preserve the caller UUID and exact proposed bytes across old enabled/maintenance writers. The adjacent typed metadata is contained in content_base64 and preserved by the established canonical metadata merge. Original operational journal records survive subsequent source writes and derived-index deletion. Verify these claims with exact frozen old binaries, including old recovery of pending source projections; prior #227 evidence does not by itself certify this change.

Required new contracts are the correlated send/lookup API, its small typed adjacent projection receipt, and a native Discussion outbox schema. SourceStore persistence format, main Runner state schema, provider protocol and v1 context envelope do not change. Full receipt requests already retained by SourceStore are operational state, not a disposable search index.

## Frozen wire contract

The names and field meanings below are shared by the backend and shell implementation. They are additive to the existing transport envelope, not optional fields on legacy chat_start. Each payload is a strict object: reject unknown fields and require explicitly nullable fields where listed.

- Capability: `discussion_send_recovery: true` means the backend implements this v1 send/lookup contract. Read-only lookup remains available without a configured provider. A new first send additionally requires existing `chat: true`; an already recorded operation is looked up before provider configuration/current context is consulted.
- Both operations require the existing `schema: "ai-brain/workspace-v1"`, fresh transport `id`, and exact `expected_workspace` guard. Its brain_id is the brain used in the digest below; absolute root/records_dir/managed are bound by the guard and durable local outbox, not copied into canonical receipt metadata.
- `op: "chat_send"` requires `operation_id`, `goal_id`, `expected_actor_id`, `conversation_id` (UUID or null), `message`, `source_paths` (array), and `request_sha256`.
- `op: "chat_send_get"` requires only `operation_id`, `goal_id`, `expected_actor_id`, and `request_sha256` beyond the envelope. These are the original retained lookup key; it carries no new message or source inputs and can never enter the send/preparation path.
- UUID fields are canonical lowercase UUID strings. A new first send must match expected_actor_id to the configured local actor. For an existing operation/lookup, bind it to the recorded original actor; a later configuration change does not rewrite its provenance. Actor is not a user-selectable authority.
- Normalize selected manual paths once before durable client publication: sort and deduplicate exact UTF-8 path strings using Rust String ordering. Backend requires this same sorted-unique representation, at most the existing 32-path limit; source path eligibility remains the existing backend policy. Preserve message UTF-8 exactly on the wire. Any existing UI trimming happens once before freezing the submission; keep the editable draft separately. Never trim/normalize while computing the digest or replaying.

### Stable client digest

`request_sha256` is lowercase SHA-256 hex of the UTF-8 bytes produced by `serde_json::to_vec` on this tuple in this exact order (a JSON array, compact with no whitespace or trailing newline):

```text
("tessera-discussion-send-request/v1", brain_id, goal_id,
 expected_actor_id, conversation_id: Option<String>, message, source_paths)
```

Null remains JSON null, paths remain an ordered array, and strings use serde_json escaping without Unicode normalization. The transport id, operation UUID, current source bytes, resolved context/provider model and workspace absolute root are not tuple elements. A fresh operation UUID can carry the same client payload intentionally; an accepted UUID cannot change that payload. The original resolved provider body has a separate `provider_request_sha256` captured during the one preparation.

Golden digest input (the backslash escapes are literal JSON bytes, not physical CR/LF):

```json
["tessera-discussion-send-request/v1","11111111-1111-4111-8111-111111111111","22222222-2222-4222-8222-222222222222","operator",null,"Question\r\n",["manual-reference.md"]]
```

Expected SHA-256:

```text
3765d691f98c285b8c7503373d06a5c74936c711161c83b4613fe45a7ced3152
```

Backend and shell tests must consume the shared [digest fixtures](fixtures/discussion-send-request-digests-v1.json). They include the CRLF example and a non-ASCII/control-character case with a combining accent that must not be normalized. The canonical_json string in each fixture gives the exact compact UTF-8 serialization; utf8_bytes and request_sha256 must match it.

### Retained projection metadata

The initial projection adds one typed `discussion_send` field adjacent to discussion_context. It describes the initial send represented by that exact SourceWrite; a later correlated send may replace the current canonical field, while the original SourceWrite journal retains its own exact bytes. It is not an ever-growing operation map.

```text
schema: "tessera-discussion-send/v1"
operation_id: UUID
brain_id: UUID
goal_id: UUID
actor_id: string
original_conversation_id: UUID | null
conversation_id: UUID
turn_id: UUID
request_sha256: lowercase SHA-256 hex
provider_request_sha256: lowercase SHA-256 hex
context_receipt_sha256: lowercase SHA-256 hex
```

All fields are required. Validate against the exact original conversation record and retained context turn in the proposed bytes, including message/source-derived client digest and turn receipt digest. Do not include the full-projection hash inside its own hashed content. Compute that separately from decoded SourceWrite content_base64. Existing writers can preserve this adjacent field without parsing it; context envelope v1 remains unchanged.

### Small operation result

Both operations return the following strict object inside the normal matching `ok: true, data: ...` envelope:

```text
schema: "tessera-discussion-send-result/v1"
operation_id: UUID
brain_id: UUID
goal_id: UUID
actor_id: string
request_sha256: lowercase SHA-256 hex
status: "projected" | "pending_projection" | "projection_conflict" | "unknown"
record: null | {
  original_conversation_id: UUID | null,
  conversation_id: UUID,
  turn_id: UUID,
  provider_request_sha256: lowercase SHA-256 hex,
  context_receipt_sha256: lowercase SHA-256 hex,
  source_path: relative string,
  projection_revision: "sha256:" + lowercase SHA-256 hex
}
source_receipt: null | existing WriteReceipt
```

All outer fields are required. `projected` requires a consistent validated SourceStore receipt and record. Pending/conflict results require a validated original record but have source_receipt null. A record with neither outcome is pending; a validated conflict maps to projection_conflict. Unknown has record and source_receipt null: its outer key echoes the requested owner/key, not a recovered identity. Contradictory receipt+conflict or mismatched evidence is an error, never a precedence guess. No status asserts provider delivery. SourceStore `Written` and `Unchanged` are both projection outcomes. Return no raw proposed/preimage/divergence bytes or provider body.

Malformed/foreign/corrupt/inconsistent/over-budget operation evidence returns a matching error with code `discussion_send_recovery_error`; accepted-UUID changed-payload/owner reuse returns `discussion_send_conflict`. Neither permits outbox deletion or a new send. A simple absence in both durable sources returns unknown.

Only a fully bound, digest-valid *new* send rejected before admission/checkpoint may use the terminal error:

```text
code: "discussion_send_rejected"
message: human-readable reason
operation_id: original UUID
brain_id: original UUID
goal_id: original UUID
request_sha256: original lowercase SHA-256 hex
recorded: false
```

This is inside `ok: false, error: ...` with the exact matching transport envelope. Unknown/malformed request envelopes and generic runtime/transport errors do not carry this terminal marker. A projection conflict after Runner persistence is not this rejection. The client persists a terminal matched reply before retiring its pending local guard; it never synthesizes the marker from error text.

### Native persistence contract

Use schema `tessera-discussion-outbox/v1` in a distinct native outbox scope. Retain the exact original guarded request/key plus the original editable draft and owner before dispatch. Publish immutable accepted/rejected terminal receipts separately using the existing durable primitive; unknown/pending/conflict and unconfirmed local terminal writes retain the operation. No shared mutable entry is rebound to the currently selected goal. An existing #242 inspection acknowledgement cannot delete a durable operation or fabricate a terminal receipt. A user's separately explicit new submission receives another UUID and leaves the unresolved original inspectable.

## Acceptance

1. Concurrent identical first submissions return one original conversation/turn with one provider dispatch. Replaying an intact accepted UUID bypasses current source re-resolution and provider spawn. Changed owner/target/payload or a foreign source-write UUID conflicts. Positive-control the dispatch counter with an intentionally distinct send.
2. Inject failure before Runner persist, after persist/before SourceStore intent, after source intent/before replacement, after replacement/before receipt, after receipt/before spawn and after spawn/before ACK. Desktop/backend restart retains exact IDs or explicit unknown and never automatically sends. Lost/failed local outbox publication or terminal-receipt write cannot discard pending state.
3. Current canonical metadata removal, canonical file deletion and later source edits still recover the original identity from an intact operational journal. Remove/corrupt/mismatch/oversize that journal (and separately simulate pending-only state): lookup returns validated retained status or unknown/error, makes zero provider calls and preserves the outbox. Missing lookup never calls admission/send. Compare pending and stored record if both exist; reject disagreement.
4. New -> old enabled continue/recover/unrelated checkpoint -> old maintenance boot/recovery -> new lookup preserves the original SourceWrite UUID/content/receipt and context snapshot. Delete only derived indexes and repeat read-only lookup. Unsupported capability has no legacy fallback. Test maximum normal serialized record, one-byte-over budget and divergent-history overflow through the actual bounded reader.
5. Native restart restores the pending draft/operation under its original workspace/goal and offers explicit navigation there; it does not silently change another current selection. Late replies cannot rebind it. Show committed projection separately from provider outcome. A new explicit user send uses another UUID and does not erase the old unknown receipt. Keep existing legacy send behavior and canonical limits unchanged.

The serialized request/receipt/digest contract above is frozen for both implementation owners. Freeze an exact maximum-record fixture before selecting the final bounded-read constant. Normal source review, exact-head CI, frozen package and isolated native/old-writer acceptance remain required. This contract does not authorize provider activation or live installation changes; the existing explicit New conversation UI remains a separate delivered slice.
