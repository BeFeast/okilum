# AI Brain mobile capture and attention — proposed P1 contract

This document defines the next bounded implementation after [indexed retrieval and
reviewed context](ai-brain-retrieval-context.md) passes acceptance. It is a design
contract for review, not an implemented capability or approval to deploy the bot.
The broader mobile requirement comes from [the POC](ai-brain-poc.md). Development
and execution control remain external in T3 Code.

## User-visible result

A thought sent through ok-gobot becomes one durable Markdown inbox item without
inventing a goal, task or result criteria. The phone can display current blockers,
required decisions and final results, then save a reply against the exact item
shown. These controls work while the LLM provider is unavailable. Closing either
desktop does not lose the capture, notification intent or reply association. A
newly captured thought is visible and readable through Tessera's native inbox /
attention entrypoint; an API response or a file alone is not native acceptance.

P1 does not replace Todoist, add general scheduling or mobile editing, dispatch an
engine, evaluate criteria, or interpret a free-text reply as human acceptance.
Promotion from a thought into a goal remains an explicit planning action requiring
criteria; automatic promotion is outside this slice.

## Authority and canonical records

An inbox capture has a stable UUID, original text, received time, original channel
reference and source identity. It is canonical Markdown with `record_type: inbox`,
`brain_id`, `id`, `status: captured`, and `received_at` frontmatter; the original
text remains its readable body. Channel identifiers are nonsecret provenance,
not credentials. Exact exports include the record; AI summaries do not replace it.

Use the managed `<records_dir>/inbox-<id>.md` namespace, reserved explicitly by the
new capability. A pre-existing conflicting file is a conflict, never an overwrite.
This requires a deliberate goal-independent canonical record case in source
inventory and validation: the current retrieval code rejects canonical records
without a goal owner. Do not weaken that rule for other record kinds. Inbox items
are not retrieval evidence for existing goals in P1; list them through `inbox_list`.
No new live capture is enabled until inventory/export/ownership compatibility is
implemented and tested together. Older binaries unable to read this record kind
are not a valid fallback after its first acknowledged write.

A saved reply is a separate `decision` record owned by the exact goal, with the
observed attention identity/revision, original source route, actor identity and
text. Saving it does not assert that a blocker is resolved, a criterion passed,
or a goal completed. Existing knowledge remains intact; conflicts are retained.

Canonical Markdown carries durable human knowledge. External idempotency keys,
request receipts, delivery attempts/cursors, reply-message mappings and pending
projection intents live in durable operational storage, outside the derived index.
Removing/rebuilding the index must not replay a capture or a notification.

## Additive API sketch

The existing [application envelope](ai-brain-application-api.md) and brain identity
guard remain unchanged. New operations are advertised by capabilities rather than
inferred from a successful connection. Unsupported means unsupported, never empty.
Transport request `id` correlates one response; durable `operation_id` identifies
a mutation across retries and restarts.

| Operation | Request | Result |
|---|---|---|
| `inbox_capture` | `operation_id`, `external_key`, `text`, trusted source route | Durable capture identity, path, revision and receipt |
| `inbox_list` | bounded `limit`, opaque cursor | Captures and an explicit next cursor / complete flag |
| `inbox_get` | `capture_id` | Exact canonical source and original provenance |
| `attention_list` | bounded `limit`, opaque cursor | Current items with exact revision, allowed reply action and observation time |
| `attention_get` | `goal_id`, `attention_id` | Current exact item or retained superseded history |
| `attention_reply` | `operation_id`, `external_key`, `goal_id`, `attention_id`, `expected_revision`, exact nullable `stage_id`, `text`, trusted source route | Saved decision identity/receipt and fresh item state |
| `attention_ack` | durable operation identity, exact observed item revision | Acknowledgement receipt only; no workflow transition |

Concrete JSON field limits and error variants are frozen with the implementation
PR before a real bot connection. Use bounded UTF-8 text (initial target 64 KiB) and
bounded pagination; attachments/voice transcription are a subsequent slice.
No client-chosen filesystem path is accepted by capture/reply operations.

`external_key` is the tuple of configured connector instance, channel, account,
chat/topic and immutable upstream update/event ID. The instance/account/actor come
from the configured trusted connector and authenticated Telegram context, not LLM
arguments. Store a normalized request digest alongside the key: replaying identical
content returns the original receipt; reusing the key with different text, brain,
route or action returns a conflict. Edited Telegram messages do not rewrite an
acknowledged capture; an explicit future revision command will govern that case.

Persist the mutation intent and assigned record UUID before any canonical write.
Only acknowledge capture/reply after its exact canonical projection is durable.
Lost replies, a crash after projection, or two simultaneous duplicate requests
must converge on one record and one receipt. Unknown outcome is recoverable through
the same operation identity; a client must not allocate a replacement ID on retry.

## Exact attention binding

The existing `workspace_attention` is read-only and must retain that behavior.
The new attention revision is a deterministic digest of the item identity and
content plus its owning goal/stage/result revisions and actionable state, excluding
volatile observation timestamps. Null stage ownership remains null; never infer an
owner from the desktop selection or the latest stage.

Before saving a reply, re-read the same item under the backend owner lock and
compare revision/goal/stage ownership. Stale or superseded replies return current
state without creating a decision or attaching it to a successor. The client keeps
the unsent text so it can be reviewed against the new state. Duplicate successful
replies return the original receipt even if the item later changes.

P1 exposes `save_decision` only for replyable decisions/blockers. A final result may
be acknowledged; acknowledgement means seen, not verified. Persist seen state by
actor, channel and exact attention revision. Reopening or restarting must not
renotify an already seen final revision in that channel. Other clients retain the
item and can display a seen marker; one channel acknowledgement cannot silently
erase another client's view or another actor's unseen state. A new material item
revision has its own delivery/seen identity. Replies to final items,
criterion acceptance, stage preparation, Start, cancel and retry are unsupported
mobile actions unless a later explicit typed contract adds them. Saving a decision
may mark that decision request as answered in the channel view, but cannot erase
an unresolved workflow blocker or mutate the existing effective-attention history.

## Connector and transport

Implement a typed `internal/tessera` client in ok-gobot and use it from both tool
registry entries and deterministic slash/ForceReply handlers. Source inspection of
ok-gobot HEAD `696fd9491bd30bad371a75fe2ef0b982692415b6` found `ToolSchema`,
`ExecuteJSON`, `ChatScoped`, typed native service integrations and durable outbox
support. Its memory MCP integration is a server, not an established outbound client.
Config struct, schema, defaults/bindings and loader tests must change together.
Do not replace existing approval callback handlers when adding reply routing.

No generic shell/exec tool or per-call SSH is needed. For isolated P1 testing, a
fixed separately provisioned operator tunnel may carry the existing loopback
JSON-lines protocol. Production remote use requires a scoped authenticated
transport or equivalent operator-controlled isolation with fixed brain/actor
binding. Opening the current unauthenticated service on all interfaces is not a
connector design. Connectivity provisioning and bot deployment are separate
operational steps; this contract performs neither.

Reuse ok-gobot's commit-before-send outbox, adding a unique external attention
revision key and durable sent-message-to-attention mapping. Preserve forum topic,
user and reply IDs. Marking an event delivered must not acknowledge its workflow
meaning. Notify only blockers, decisions and final results; normal progress stays
in summaries. Retry visible delivery failures, with retained terminal failure.

Do not promise exactly-once Telegram delivery: a successful send with a lost
acknowledgement may be uncertain. Retain the attempt, report uncertainty, and never
lose or duplicate the underlying Tessera capture/decision. Delivery deduplication
and mutation idempotency are distinct guarantees.

## Decomposition and acceptance

1. **Canonical inbox and idempotency.** Implement inbox records, durable external
   identity receipts, list/get, crash recovery, index ownership exclusions and exact
   export coverage, plus native inbox discoverability/read access for goal-independent
   captures. Acceptance: concurrent duplicates, lost response, restart after
   each persistence boundary, and reused key/different payload. Positive control:
   a genuinely new update creates a second capture without creating any goal.
   Native acceptance shows that captured item in Tessera and opens its exact text;
   it does not require implicit promotion into a goal.
2. **Bound attention replies and delivery identities.** Add deterministic item
   revisions, save-decision receipts, explicit seen acknowledgements and durable
   event cursors/mappings. Acceptance: stale/wrong-goal/wrong-stage/wrong-user reply,
   successor-stage turnover, repeated valid reply, and backend restart. Assert no
   AcceptHuman/Start/task completion and no hidden live blocker.
3. **Typed ok-gobot connector.** Add configured client, tools and deterministic
   commands; integrate durable outbox and route mapping. Acceptance with fake
   transport/Telegram: provider down capture/list/reply, duplicate updates, delivery
   failure/restart, and existing command/callback/forum-topic regressions.
4. **Isolated end-to-end acceptance and packaging.** Use a marked test brain/chat
   route through the chosen provisioned transport: capture on phone, inspect exact
   Markdown, show one decision, save its bound reply, restart/reopen and recover the
   same result. Validate actual user-visible behavior separately from API tests.
   Synthetic bot transport tests while the user is unavailable do not count as
   real-phone acceptance; retain separate native/API/phone verdicts and complete
   each at its actual surface. Produce a reviewed immutable bot release through
   its existing pipeline when
   authorized; no ad hoc shtrudel build or production bot mutation during design.

Dependencies: slices 1 and 2 define backend contracts before slice 3 implementation;
slice 4 requires their tests and reviewed transport/deployment preparation. Current
retrieval acceptance and its preserving fallback take priority over this milestone.

## Tracking

[Milestone #123](https://git.oklabs.uk/BeFeast/tessera/issues/123) owns this scope:

- [Canonical inbox and idempotency #124](https://git.oklabs.uk/BeFeast/tessera/issues/124).
- [Bound replies and delivery identities #125](https://git.oklabs.uk/BeFeast/tessera/issues/125).
- [Typed ok-gobot integration #126](https://git.oklabs.uk/BeFeast/tessera/issues/126).
- [Isolated mobile acceptance and packaging #127](https://git.oklabs.uk/BeFeast/tessera/issues/127).

The [issue #124 implementation plan](ai-brain-inbox-implementation.md) freezes the
backend/native interface, create-only transaction and parallel file ownership.
