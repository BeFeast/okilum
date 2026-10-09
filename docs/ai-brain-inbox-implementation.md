# Canonical inbox implementation plan — issue #124

This bounded plan freezes the interface for parallel backend and native UI work
following [P1 scope](ai-brain-mobile-capture-attention.md). Implementation begins
after PR #119 acceptance/merge. Current R4 acceptance and dedicated alpha update
take priority. No production bot or transport change is part of #124.

## Existing code to reuse

- `runtime.rs::Runner::mutation` restores the last persisted in-memory journal
  after an error; it does not rewind acknowledged source bytes.
- `persist` atomically writes and fsyncs workspace `state.json`.
- `pending_writes`, `flush_writes` and SourceStore's per-operation receipts already
  recover a crash after a canonical write but before removing the pending intent.
- `create_goal_inner` demonstrates queue → persist → flush → acknowledge. It is
  not an inbox implementation: it requires criteria and selects a goal.
- `checkpoint_application` is goal-owned; do not store workspace inbox receipts
  there. Add explicit workspace state instead.
- `path(kind,id)` uses `<records_dir>/<kind>-<UUID>.md`. Reuse this format for
  `inbox-<UUID>.md`; SourceStore deliberately does not create parent directories.
  This supersedes the initially proposed nested inbox directory without adding
  a second directory/write mechanism.
- `export_exact` drains pending projections and exports all canonical files under
  the existing SourceStore lock. Add coverage for inbox records; do not implement
  a second export path.
- Native `BrainView::capture` currently submits `create_goal` with title/criteria.
  Keep that explicit create-goal flow. New thought capture/list/read is a separate
  mode at the existing inbox/attention entrypoint, not a renamed goal form.

## Frozen backend/native interface v1

Requests use the existing workspace envelope and mandatory exact
`expected_workspace` in the native client. Fields below live beside `op`; they
are not placed inside a second request object. UUIDs use canonical lowercase
hyphenated spelling. All times returned by the backend are UTC RFC3339.

Advertise `inbox_read: true` and `inbox_capture: <managed write enabled>` separately.
Read/list work with unavailable providers and never select/modify a goal. Capture
requires the managed write boundary; unsupported/unmanaged is an explicit error.
Native checks capability before enabling the action.

### Capture

```json
{
  "op": "inbox_capture",
  "operation_id": "<UUID retained across retries>",
  "text": "<original UTF-8 text>",
  "source": {
    "channel": "native",
    "instance_id": "<persistent client instance UUID>",
    "account_id": "local",
    "actor_id": "<configured local actor>",
    "chat_id": null,
    "topic_id": null,
    "message_id": "<same durable operation UUID>",
    "update_id": "<same durable operation UUID>",
    "uri": null
  }
}
```

The backend derives the external key from the structured source identity; do not
accept an additional independent opaque external-key field that could disagree.
For the later Telegram adapter, `channel: telegram`, configured connector instance,
account, trusted sender/chat/topic/message/update IDs replace the native values.
All upstream IDs are bounded strings, not integers subject to JSON precision loss.
`uri` is an optional original-reference URI, not an instruction to fetch content.
The later connector transport supplies trusted actor/instance context. This local
API does not grant an LLM authority to choose another actor or connector identity.

Text must contain a non-whitespace character and be at most 65,536 UTF-8 bytes.
Keep exact text bytes, including CRLF, leading/trailing whitespace and a missing
final newline. Source identifiers are 1–256 UTF-8 bytes when non-null; optional URI
is at most 2,048 bytes. Reject control characters in identity fields and unknown
source channels. Schema errors occur before assigning a record or writing intent.
Native actor must match the backend's configured local actor. Telegram capture is
not advertised/enabled until a configured trusted connector context exists.

Successful `data`:

```json
{
  "capture_id": "<UUID>",
  "path": "<records_dir>/inbox-<UUID>.md",
  "revision": "sha256:<digest of committed Markdown>",
  "received_at": "<UTC time fixed by first accepted intent>",
  "receipt": {
    "operation_id": "<original operation UUID>",
    "status": "committed",
    "request_sha256": "<normalized request digest>",
    "replayed": false
  }
}
```

A retry returns the original committed identity/revision/time, with `replayed:
true`; it does not claim that a subsequently edited file still has that revision.
Use get to inspect current canonical source. A path missing after a previously
committed receipt is not recreated by replay. No goal/task/conversation is created.

### List and get

`inbox_list` takes `limit` (default 50, range 1–200) and nullable opaque `cursor`.
It returns `{items, next_cursor, complete, observed_at, generation}`. Each item is
`{capture_id,path,revision,received_at,status,title}`. Status is `captured` in v1;
title is a bounded display derivation from the first nonempty body line, never a
rewrite of original text. Return items in capture-ID lexical order. The cursor
binds brain identity, inventory generation and last ID. A changed inventory returns
`inbox_cursor_stale`, not an incomplete page presented as a complete current list.

List discovers canonical `inbox-<UUID>.md` files, validates exact brain/type/id/path
and UTC receipt time, and derives its generation from sorted path/revision pairs.
Set a 10,000-item inventory limit consistent with current brain budgets; exceeding
it is explicit. Invalid or unreadable canonical inbox records fail the listing;
never silently drop one. Inbox knowledge remains discoverable without its delivery
receipts. Restoring canonical files without operational receipts does not authorize
upstream replay or new mutations; that recovery boundary must stay explicit.

`inbox_get` takes `capture_id` and returns `{item, text, source}`, where `text` is the exact original body
(preserving CRLF, whitespace and absent final newline) and `source` is the
existing exact `SourceSnapshot` including base64 bytes. It validates identity and
does not allocate a download session. Native may use existing `source_preview`
for rendered reading. `source_read` and existing revision-aware editing/export
remain reusable; no special inbox editor or promotion action is required in #124.

Errors retain the standard `{code,message,conflict?}` envelope. Inbox variants:
`inbox_invalid_request`, `inbox_unsupported`, `inbox_identity_conflict`,
`inbox_not_found`, `inbox_record_invalid`, `inbox_cursor_stale`,
`inbox_projection_pending`. Preserve structured SourceStore conflicts for path
collision/write failure. An unknown response outcome retains the client operation
identity and draft; the UI offers recovery/retry of that exact request.

## Durable transaction and recovery

Add `#[serde(default)] inbox_operations` to workspace State, outside GoalState.
Each accepted intent keeps original operation ID, source identity/external key,
request digest, assigned capture UUID, fixed received time, expected committed
revision and SourceStore operation ID. Keep committed metadata compact; canonical
text need not be duplicated in every later state checkpoint. Pending source bytes
already live in the existing `pending_writes` journal. No receipt belongs under
`derived/` or a selected goal's application state.

The normalized request digest covers brain, exact text bytes and every source
identity field, excluding transport request ID. Both operation-ID reuse and
external-key reuse compare this digest. Reusing either identity with different
content fails before mutation. A different operation ID with the same external key
and digest resolves to the original receipt; retain a compact alias binding so
that the second operation ID cannot later be reused for another request.

Under the existing backend mutex/process owner:

1. Validate request, write boundary and trusted local actor; resolve any existing
   operation/external key before assigning new IDs. Successful repeats return their
   original receipt. Pending repeats continue only their original source operation.
2. For a new key assign capture ID/time and a create-only SourceWrite with
   `expected_revision: null`. Do not call generic `queue` unchanged: it reads and
   updates a pre-existing record. Reuse its serialization pattern through a narrow
   create-only record helper, and treat any colliding path as a preserved conflict.
3. Insert the inbox intent and the SourceWrite into the same workspace state, then
   persist/fsync once before any canonical write.
4. Flush via the existing SourceStore receipt path. Finalize the inbox receipt in
   the same checkpoint that removes its completed pending write. If a crash occurs
   between write and checkpoint, SourceStore replay returns the original receipt;
   startup/retry finalizes the same inbox intent, never generates new IDs.
5. Return committed only after canonical projection and receipt are durable. A
   collision or pending write failure remains visible and recoverable; never drop
   intent, overwrite another note, or return a successful capture from metadata alone.

Opening the backend recovers persisted pending writes as today, then reconciles
inbox intent state from retained SourceStore receipts. Reuse the existing injection
hook and add only the missing receipt-finalization boundary; do not build another
transaction engine. `with_goal`, primary goal, provider state and dispatch guards
must remain unchanged by workspace inbox operations.

## Retrieval, compatibility and export

Add a dedicated classification for exact canonical `record_type: inbox` plus
matching brain/id/path. It may be goal-independent and must yield no retrieval
chunks/citations in v1. Apply this classification before the usual goal-owner
requirement; every other canonical record retains its current fail-closed owner
rule. A random record lacking `goal_id` cannot masquerade as an inbox item through
a path prefix alone. Forged citations to inbox records fail, with saved result and
ordinary note positive controls.

The inbox addition changes durable workspace state: current #122 maintenance
fallback does not preserve inbox_operations and is not sufficient after the first
capture. Before enabling live writes, prepare an explicitly compatible fallback
that preserves those receipts and refuses inbox controls it cannot validate, or
use a verified roll-forward-only recovery path. No source rewind. This requirement
belongs to #124's live rollout, not to the already prepared #119 update.

Exact export includes inbox Markdown once, with unchanged body/provenance. Export
must wait for pending projections or report their failure. Derived-index deletion,
backend restart and goal selection changes cannot erase captures or receipts.

## Parallel ownership and acceptance

Backend owner: new `crates/okilum-brain/src/inbox.rs` for public types/validation,
`runtime/inbox.rs` for workspace transaction methods (or a bounded runtime section),
`runtime.rs` State/recovery hooks, `service.rs` operation routing before goal routing,
`application.rs` capability advertisement, `retrieval.rs` explicit classification,
and backend integration tests. Update the application API from this frozen contract.

Native UI owner: new `crates/okilum-shell/src/brain/inbox_ui.rs`, minimal `brain.rs`
state/dispatch integration, inbox/attention navigation and UI tests. Keep source,
conversation and goal drafts intact. Do not edit backend types/service/runtime;
consume the JSON interface above through the existing request batching mechanism.

Native retains an outbound capture request before sending it in per-client,
per-brain durable state, clearing it only after a matching committed receipt. A
restart/lost reply must reuse the exact operation/source key. Inbox pagination,
selection and delayed response guards use capture/workspace identity, not goal
selection. A new capture must appear/read from Okilum without changing the active
goal; existing explicit goal creation keeps its observable-criteria requirement.

Backend tests cover every transaction crash boundary, concurrent duplicates,
external-key/operation-ID mismatches, create-only collisions, source edits/deletion
after commit, restart/alias persistence, malformed inbox ownership, export exactness
and zero changes to current goals/providers/dispatch. Native tests cover provider
absence, unsupported older backend, draft survival, replay recovery, pagination
invalidation and late responses after changing brain/capture. The native acceptance
run shows a new thought, its exact readable source and unchanged selected goal.
A fake Telegram client does not constitute real-phone acceptance; that remains #127.
