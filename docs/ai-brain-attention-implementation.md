# Bound attention API and transaction plan — issue #125

This backend slice follows the canonical inbox in #124 and the mobile P1 contract
in #123/#128. It supplies provider-independent, exact-item attention reading,
saved decisions and seen acknowledgements. Native #125 controls and the actual
ok-gobot transport follow this frozen interface; neither is claimed by API tests.

## Authority and ownership

The existing `workspace_attention` remains read-only and unchanged. Its effective
item selection is reused, including resolved-catchup filtering and explicit null
stage ownership. New operations never call Start, prepare, cancel, retry,
AcceptHuman, criterion evaluation, task completion or a provider. A reply saves
knowledge against an observed item; it cannot resolve a workflow blocker or erase
existing attention history.

The backend takes actor identity from `capabilities.actor`. Initially only native
source/channel requests are enabled. A Telegram caller is explicitly unsupported
until the configured trusted connector exists; source IDs are not LLM authority.
Source identity and 64 KiB text validation reuse #124 without changing its API.

Attention revision is SHA-256 of the exact item identity/content, owning goal
source revision, explicit nullable owning stage revision, exact nullable result
revision and allowed actions. Observation time and seen/delivery state are excluded.
A null stage remains null and has no inferred result or stage revision. Renaming
or changing a goal changes its revision and invalidates a new reply. Once a reply
is committed, retries return its original receipt even after the item changes.

## Frozen JSON interface v1

Use the existing workspace envelope and exact `expected_workspace` in native
clients. Fields below live beside `op`. Capability flags are `attention_read`,
`attention_reply` and `attention_ack`; mutations require the managed write boundary.
Unsupported flags disable controls; empty results never stand in for unsupported.

### Reading

`attention_list` accepts `channel` (default `native`), `limit` (default 50, 1–200)
and nullable opaque `cursor`. It returns:

```json
{
  "items": [{
    "goal_id": "UUID",
    "attention_id": "opaque bounded existing identity",
    "revision": "sha256:digest",
    "goal_title": "Title",
    "goal_status": "draft",
    "stage_id": null,
    "result_id": null,
    "kind": "decision",
    "message": "Exact current message",
    "allowed_actions": ["save_decision", "ack_seen"],
    "current": true,
    "seen": false,
    "seen_at": null,
    "actor_id": "configured actor",
    "channel": "native"
  }],
  "next_cursor": null,
  "complete": true,
  "generation": "digest",
  "observed_at": "UTC RFC3339",
  "delivery_cursor": 12
}
```

Final items expose only `ack_seen`. Blockers/decisions expose `save_decision` and
`ack_seen`. Other attention kinds have no mutable mobile actions. `seen` is scoped
to this actor, channel and exact revision; it never hides an item. Another actor or
channel remains unseen, and a new material revision starts unseen. No caller can
choose a different actor. `seen_at` is the first accepted acknowledgement time.

Items sort by `(goal_id, attention_id)` lexical order. The opaque cursor binds the
brain, actor/channel, current inventory generation and last item. Generation covers
item revision and channel seen state; changes return `attention_cursor_stale`.
The inventory is bounded at 10,000 items, with an explicit error above that limit.

`attention_get` accepts `goal_id`, `attention_id`, nullable `revision` and `channel`
(default `native`) and returns `{item, observed_at}`. A null revision means the
current item. Supplying a previously observed exact revision can read its retained
snapshot with `current: false`; allowed actions are empty for a superseded item.
Unknown/unobserved historical content is `attention_not_found`, never fabricated.

New attention reads durably retain newly observed exact snapshots before returning
`delivery_cursor`, which is the monotonically increasing workspace observation
sequence. Repeated reads of one revision reuse its event identity. This is a new
explicit API effect and does not change `workspace_attention` or goal selection.
Only observed revisions are promised as replayable events; obsolete intermediate
states that nobody observed are not invented as delivery work. Later #126 polls
current items and retains per-route delivery attempts/mappings separately. An API
observation, outbound delivery, and a human acknowledgement remain three events.

### Saving a decision

```json
{
  "op": "attention_reply",
  "operation_id": "durable UUID",
  "goal_id": "UUID",
  "attention_id": "exact observed identity",
  "expected_revision": "sha256:digest",
  "stage_id": null,
  "text": "Exact human reply",
  "source": {
    "channel": "native",
    "instance_id": "persistent client UUID",
    "account_id": "local",
    "actor_id": "configured actor",
    "chat_id": null,
    "topic_id": null,
    "message_id": "durable operation UUID",
    "update_id": "durable operation UUID",
    "uri": null
  }
}
```

`stage_id` is required, including explicit null, and must equal the observed item.
No filesystem path or independent external-key field is accepted. Source identity
and exact text are validated before allocating an intent or record. Response:

```json
{
  "decision_id": "UUID",
  "path": "records/decision-UUID.md",
  "revision": "sha256:committed Markdown",
  "received_at": "UTC RFC3339",
  "receipt": {
    "operation_id": "original operation UUID",
    "status": "committed",
    "request_sha256": "digest",
    "replayed": false
  },
  "item": {"goal_id": "UUID", "attention_id": "identity", "current": true}
}
```

`item` is the full fresh item shape above, or null if it is no longer current when
a committed receipt is replayed. The decision retains `record_type: decision`,
brain/id/goal ownership, explicit nullable stage ownership, observed attention ID
and revision, actor/source route, timestamp and original text body. Verification is
`unverified`: saving a reply does not prove its proposed facts or actual execution.
The decision may be retrieved as explicitly attributed knowledge, never accepted
as criterion evidence by this endpoint. Saving a decision does not automatically
acknowledge the notification; the channel may call `attention_ack` explicitly.

### Seen acknowledgement

`attention_ack` accepts the same operation/source identity and exact target fields
as reply, with no text. It returns `{receipt, item, acknowledged_at}`. It records
seen for `(brain, actor, channel, goal, attention, revision)` only. It does not hide
blockers, accept final evidence, mark a goal complete or acknowledge another
channel. A genuinely new acknowledgement operation against an already seen exact
revision returns the original seen time while retaining its own mutation receipt.

## Idempotency and conflicts

Durable operation ID and normalized upstream identity are reserved across inbox,
reply and acknowledgement actions within a brain. Reusing either identity with a
different action, target, source or exact text conflicts before side effects. Same
request/different operation ID retains an alias to the original receipt. Request
hash includes action, brain, source and all exact target fields; transport request
ID does not participate. Seen writes and replies use separate upstream events.

First resolve completed/pending receipts, then validate current authority for a
new operation under the backend owner lock. A known successful retry remains
successful if the item has since changed. A new wrong goal/stage/revision or
unsupported action returns a structured error without saving a decision or seen
state. Errors are `attention_invalid_request`, `attention_unsupported`,
`attention_not_found`, `attention_stale`, `attention_identity_conflict`,
`attention_cursor_stale` and `attention_projection_pending`. A stale error includes
`current` (the fresh item or null) so the client retains its draft and can show the
new state. Existing SourceStore collision/error details remain intact.

## Persistence and crash recovery

New workspace fields hold observed revision snapshots and sequence, operation
intents/aliases, and scoped seen receipts. These are operational data outside the
derived index and outside selected GoalState. Restoring canonical decision files
cannot reconstruct acknowledged deliveries or replay permissions.

A new reply assigns decision UUID/time, queues a create-only SourceWrite and
persists its complete intent in one workspace checkpoint before writing Markdown.
Reuse #124's create-only serializer/receipt finalization boundary rather than
building another transaction engine. Finalize the decision receipt in the same
checkpoint that removes its pending source write. Unknown response/restart resumes
the original pending source operation. An acknowledgement is one atomic journal
checkpoint containing its operation receipt and first scoped seen record; it does
not write or rewrite existing Markdown.

A durable operational `attention-enrollment.json` marker is written after the first
workspace checkpoint containing attention history and before any new API can
return. Normal pre-attention upgrades with neither marker nor journal initialize
empty history. A marker with a missing journal is explicit recovery, even when
only acknowledgements existed and no decision Markdown can reveal that history.
New attention capabilities/operations and captures are disabled in that condition;
existing `workspace_attention` and source reading remain available. Already retained
committed operation/alias receipts can still replay without writing new state;
unknown operations and new alias bindings remain disabled. Malformed markers or
unreadable journals fail closed rather than entering first-upgrade initialization.
A missing marker
with an intact journal is recreated without resetting receipts or observations.

No live #125 operations are enabled before a preserving fallback or verified
roll-forward path covers these new operational fields and pending decision
projection. The #124 fallback alone does not preserve #125 state. Rollback must
retain all current source, receipt and observation history.

## Ownership and acceptance

Backend owns `attention.rs`, `runtime/attention.rs`, workspace State/recovery hooks,
service routing, capability flags and scoped tests. Native and ok-gobot consumers
use the JSON contract and own their client drafts/durable outboxes; they do not
edit backend types. No GUI or bot runtime change is included in this backend slice.

Required tests: stable revision across reads/restart with positive material-change
control; no inferred stage for null ownership; stale/wrong-goal/wrong-stage/actor
rejection; final reply refusal with ack positive control; concurrent duplicates,
operation/action collisions and lost response; reply crash before/after source
write; ack restart and actor/channel isolation; observed history and cursor staleness;
no AcceptHuman/Start/provider/task/goal-state changes; exact decision export and
verified separation from actual completion. Retain native/API/phone verdicts at
their real verification surfaces.
