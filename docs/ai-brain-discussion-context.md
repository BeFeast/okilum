# Discussion context retained per turn

Draft contract for [#227](https://git.oklabs.uk/BeFeast/okilum/issues/227), based on
`8dacfa57b162b0f39951971322c223086b5ae48f`. Contract review precedes implementation.
This extends the existing [application conversation](ai-brain-application-api.md)
using the [goal context brief](ai-brain-goal-context-brief.md). It does not prepare
stages, change reviewed packets, start engines, enable providers or change completion.

## Context selection and authority

`chat_start` retains its current goal, explicit message, complete conversation
history, manual source paths and original Inbox input. It resolves saved inputs
through `runtime/goal_brief.rs` under the existing goal/transaction owner. Reuse
its validated decision ownership, latest saved result, verification, citations,
remaining criteria and omissions; do not independently select the current stage's
last result. A newly selected stage without a result therefore leaves the previous
same-goal outcome eligible. An invalid latest stage/result remains an omission,
without silently substituting an older outcome.

Admission order is mandatory current goal/criteria and transcript, retained original
Inbox input, explicit manual sources in selection order, latest saved result, then
saved decisions in the brief's existing order. No transcript message, manual source
or original thought is silently shortened or displaced to fit automatic context.
An interrupted assistant prefix retains its existing explicit interrupted label.

Manual inputs must be readable exact UTF-8 sources in the configured brain. Automatic
saved context is strictly same-goal. Preserve deliberately selected cross-goal and
unowned manual sources, with their exact provenance and explicit unverified-reference
status; they are not automatically promoted into this goal's saved context. Preserve
the retained original Inbox snapshot and provenance as unverified operator input.
A manually selected later revision of its path can be a separately labelled
unverified reference; it never replaces the original or becomes verified evidence.
Other goal-independent Inbox records are not automatically imported.

Deduplicate only `(brain_id, path, revision)` and retain all inclusion reasons
(`manual`, `saved_decision`, `latest_result`, `original_inbox`). Identical text at
different identities/revisions remains separate. Exact original-input duplicates
are attributed to the retained original. No semantic merging or inferred supersession.
Manual selections remain selected even when they overlap an automatic input.

The existing provider reference-context framing remains in force. Populate
`ChatContext.decisions` and `previous_result` with attributed exact saved input
data, and include the derived remaining criteria/omissions alongside the original
goal constraints. `ChatContext.sources` contains the admitted source references.
Saved text is reference data, never a new instruction authority. Decision replies
and original Inbox input stay unverified; recorded outcome verification is displayed
as recorded, not upgraded by Discussion or model output. No credential enters a receipt.

## New explicit request limits

The old manual-source/history path has no aggregate request limit. The following
limits are new pre-dispatch policy, not a claim about prior behavior:

| Boundary | Limit / behavior |
| --- | --- |
| Actual provider JSON request body | 256 KiB, including model, fixed framing, JSON escaping, all messages, reference metadata and omissions |
| Current user message / manual sources | Governed by the actual 256 KiB body cap; no additional 64 KiB per-item policy. Invalid UTF-8 rejects the send; bounded reads may reject a source already larger than the entire request budget |
| Manual paths | 32 unique paths; preserve input order |
| Original Inbox | Existing 64 KiB bound; malformed/oversized retained origin rejects the send |
| Saved inputs | Existing brief bounds: 1,024 decision candidates, 8 KiB per exact citation, 20 inputs; at most 48 KiB of serialized automatic input contributions |
| Omission display | At most 32 detailed path/code entries, plus exact remaining counts by code and `details_complete: false` when abbreviated |
| Retained receipt envelope | 8 MiB serialized JSON and 64 captured turns per conversation; never evict older receipts |
| Canonical conversation read | 128 MiB bounded read; reserve 64 MiB before dispatch for the existing 8 MiB assistant-output limit, YAML escaping and readable-body duplication |

Build and measure the complete serialized provider body, not just citation bytes.
Automatic admission must account for duplicated fields/escaping in its actual
request contribution and the total 256 KiB body. Keep the mandatory brief summary
(remaining criteria, completeness and bounded omission counts) even if no saved
source fits. If mandatory goal/history/original/manual data plus that summary
cannot fit, return `chat_context_limit` before adding the user message, changing
selection/status, checkpointing a request or contacting the provider. The draft
remains with the client; an explicit new conversation or smaller manual selection
is possible. Never silently trim history. Envelope exhaustion has the same rule.
Measure the final candidate YAML plus readable body before committing; reject if
it exceeds 64 MiB, leaving the stated output reserve. Preserve the existing
8 MiB assistant limit and the ability to checkpoint its terminal response.
Verify the serializer's conservative reserve
with maximum allowed output including escaped/control-character content and final
projection/readback; all successful new projections must fit the 128 MiB read bound.
Older writers that produce larger legacy files receive explicit unavailable context
on the new reader, never a fabricated or silently evicted history.

Automatic candidates that fail source/admission bounds become visible omissions;
no partial source excerpt is fabricated. Inventory overflow produces a bounded
`brief_inventory_limit` omission and an explicit unavailable brief, with the current
goal's criteria conservatively remaining; do not consume an incomplete inventory
as complete. Other brief read/validation failure is similarly explicit and cannot
import a guessed owner or result. No provider call is needed to assemble the context.

## Persistence seam and older writers

Do not add authoritative fields to typed `application::Journal`, `Conversation`,
`Message` or nested `ChatContext`: older serde readers discard unknown fields when
rewriting them. `Conversation.request` remains the compatible latest-request field
and is not a historical receipt source.

Store an additive **top-level canonical conversation YAML key** `discussion_context`
with schema `tessera-discussion-context/v1`, brain/goal/conversation IDs and a list
of immutable turn receipts. `Runner::queue_with_operation` merges existing
top-level YAML metadata before replacing fields supplied by the old typed record;
an older writer therefore preserves this unknown key. No second chat store,
new record type, State schema or enrollment journal is introduced.

Each receipt contains a UUID `turn_id`, zero-based `user_message_index` in the
existing `messages` array, SHA256 of the exact user text and ordered role/text
transcript prefix through that index, created time, configured actor, exact goal
revision, brief generation/availability, included inputs, remaining criteria,
omissions and limits. Freeze the exact goal title, full original criterion objects,
all constraints, next-step framing and other mandatory context fields actually
used to build this request; a goal revision/hash alone cannot display that history.
Each included input retains its exact UTF-8 source text,
brain/path/revision/locator, inclusion reasons, kind/title, actor/source identity,
recorded time and verification where applicable. Missing metadata is null/unknown,
never invented. Retained Inbox provenance is explicitly separate from evidence.

The receipt retains the model, `request_format: discussion-context/v1`, SHA256
of the actual serialized provider JSON body (`request_sha256`), and a deterministic
`receipt_sha256` over the receipt excluding that final digest. Store the structured
context once per turn; derive the presentation and provider reference fields from
that same object. Do not copy the full transcript or provider body into every
receipt: the existing transcript prefix is bound by index/hash, while the existing
latest `Conversation.request` remains compatible. Storage grows with retained
context, not with repeated cumulative transcript copies. Hashing uses actual frozen
body bytes and defined struct serialization, not an arbitrary reserialized JSON
map. These are integrity/binding checks, not proof that the provider received it.

Load/validate the existing envelope from canonical Markdown before appending a
new receipt. Ordinary streaming/recovery projections preserve the existing top-level
envelope opaquely even when it is invalid; they must not block unrelated maintenance
or erase it while updating the compatible conversation fields. Bind every receipt to brain, goal, conversation
and the original user index/text/prefix; old-writer-added user turns receive no
receipt. Preserve previous receipt objects unchanged; never rebuild from current
notes, a fresh brief, the overwritten latest request or assistant output.
Missing envelope means historical context unavailable. Corrupt, mismatched or
future-version data stays preserved and is explicitly unavailable; it blocks adding
new captured turns to that conversation until resolved, without disabling unrelated
goals, source readback or existing task/engine maintenance.

Use the existing `checkpoint_application` transaction to persist the running
conversation and queued canonical projection containing the appended envelope.
Return success and dispatch only after persistence and `flush_writes` succeed.
On failure, retain the existing durable pending-write/recovery behavior and do not
dispatch. Old maintenance can flush the same opaque `SourceWrite` bytes. A crash
after commit but before/after network submission leaves a retained request intent
and the existing interrupted recovery status; it does not establish delivery or
authorize automatic retry. A later explicit user send creates its own new receipt.
Streaming delta/terminal updates never revise the receipt.

Factor the existing ChatClient body construction into one pure preparation helper.
Build/hash the body once before the checkpoint, retain those bytes for this dispatch
in memory, and add a streaming entrypoint consuming them unchanged, retaining current
idle/cancellation/checkpoint behavior. Do not regenerate context in the provider
thread. Existing proposal `run_frozen` behavior is outside this change.

The canonical envelope is editable source, not tamper-proof storage. Validate its
digest, identity and transcript binding; invalid/missing historical data is shown
as unavailable rather than silently regenerated. New writers preserve it on all
conversation projections. Older writers may replace the readable body rendering,
but must retain the entire envelope in YAML. Actual binary compatibility proof is
required; the merge behavior observed in source alone is insufficient.

## API and native display

Capability `discussion_context: true` advertises this contract. `chat_start` keeps
its existing request fields and adds `turn_id` to its successful response.
`chat_get` accepts optional `context_turn_id` (UUID). Existing fields remain intact;
default polling returns at most 64 captured receipt summaries plus a constant-sized
`context_history` fallback, and an explicit expansion returns
one frozen snapshot without source re-reads or a fresh brief:

```json
{
  "id": "conversation-uuid",
  "goal_id": "goal-uuid",
  "status": "complete",
  "messages": [{"role":"user","text":"What comes next?"}],
  "turn_contexts": [{
    "user_message_index": 0,
    "turn_id": "turn-uuid",
    "availability": "available",
    "reason": null,
    "saved_input_count": 2,
    "manual_input_count": 1,
    "omission_count": 0
  }],
  "context_history": {"unrecorded_turn_count":0,"unrecorded_reason":"historical_context_unavailable"},
  "context_snapshot": null
}
```

For `context_turn_id`, `context_snapshot` is the matching receipt's presentation
projection: turn/message binding, goal revision, actor/time/model, request/receipt
hashes, included input objects and exact text, remaining criteria, omissions and
limits. It does not copy the full transcript or claim that the historical provider
body can be regenerated by a newer prompt formatter. An unknown/cross-conversation
turn ID is refused; invalid
stored data returns an unavailable state with no fabricated empty snapshot.
Only captured turns appear in `turn_contexts` (at most 64). For every user row absent
from that list, the client displays unavailable context using the constant-sized
`context_history: {unrecorded_turn_count: usize, unrecorded_reason: string}` fallback.
The count covers all absent user rows. The reason is `historical_context_unavailable`
for valid/missing envelopes, or the actual validation/read failure reason for an
unavailable envelope. No per-legacy-message summary is emitted: legacy transcript
length must not amplify new polling metadata. Do not relabel absence as empty context.

The expanded response uses this exact shape (IDs/hashes below are placeholders):

```json
{
  "context_snapshot": {
    "availability": "available",
    "reason": null,
    "snapshot": {
      "schema": "tessera-discussion-turn/v1",
      "brain_id": "brain-uuid",
      "conversation_id": "conversation-uuid",
      "turn_id": "turn-uuid",
      "user_message_index": 0,
      "user_message_sha256": "hex64",
      "transcript_prefix_sha256": "hex64",
      "created_at": "2026-09-08T00:00:00Z",
      "actor_id": "configured operator",
      "goal": {
        "id": "goal-uuid",
        "revision": "sha256:hex64",
        "title": "Exact goal title",
        "criteria": [{"id":"criterion-id","description":"Exact criterion","requires_human":false}]
      },
      "constraints": ["Exact original constraint"],
      "next_step": "Clarify the goal and prepare its next stage",
      "brief": {"availability":"available","generation":"hex64","reason":null},
      "inputs": [{
        "id": "record-uuid",
        "kind": "decision",
        "title": "Saved Attention reply",
        "brain_id": "brain-uuid",
        "owner_goal_id": "goal-uuid",
        "path": "records/decision-record-uuid.md",
        "revision": "sha256:hex64",
        "locator": "L1-L20",
        "text": "Exact canonical UTF-8 source including frontmatter",
        "reasons": ["saved_decision"],
        "actor_id": "saved actor",
        "source_identity": {"channel":"native","instance_id":"instance-uuid","account_id":"saved actor","actor_id":"saved actor","chat_id":null,"topic_id":null,"message_id":"message-uuid","update_id":"update-uuid","uri":null},
        "received_at": "2026-09-08T00:00:00Z",
        "verification": "unverified"
      }],
      "remaining_criteria": [{"id":"criterion-id","description":"Exact criterion","requires_human":false}],
      "omissions": {"items":[],"counts":{},"total":0,"details_complete":true},
      "model": "configured-model",
      "request_format": "discussion-context/v1",
      "request_sha256": "hex64",
      "limits": {"request_max_bytes":262144,"request_bytes":4096,"automatic_max_bytes":49152,"automatic_bytes":1024},
      "receipt_sha256": "hex64"
    }
  }
}
```

`context_snapshot: null` means no expansion was requested. Unavailable expansion
is `{ "availability": "unavailable", "reason": "context_invalid", "snapshot": null }`;
the same unavailable reason appears in `context_history`. Defined unavailable reasons:
`historical_context_unavailable`, `context_invalid`, `context_version_unsupported`,
`context_binding_mismatch`, `context_source_unavailable`, `context_source_oversized`.
Captured summary counts are integers. Unavailable rows have no summary or counts,
never misleading zeroes.
Available counts count unique input identities having the applicable reason; one
input may count as both manual and saved while its content is included once.

Input `id`, `owner_goal_id`, `locator`, `actor_id`, `source_identity` and
`received_at` are nullable. `source_identity`, when available, is the existing
`inbox::SourceIdentity` object (channel/instance/account/actor, chat/topic,
message/update and URI), preserved from the validated canonical source. A saved
decision must have that provenance; null is for inputs without a validated source
identity, not permission to accept a decision lacking provenance.
`kind` is `manual|decision|discussion-decision|result|inbox`; `verification` is
`unverified|verified|failed|unknown`. Manual-only references are `unverified` even
if their raw text claims verification. `omissions.items` holds `{path: string|null,
code: string}`; `counts` maps each code to its full count and `total` includes
abbreviated entries. Brief unavailability uses a null generation and explicit reason.

Native Discussion shows a compact context row under each user message and expands
the selected immutable snapshot using `chat_get`. Label it "Context for this
request", not proof of network delivery; current conversation/response state is
separate. Show source provenance, unverified status, original input, remaining
criteria and omissions. Reopen/source changes preserve the historical display.
Without the capability, the composer must not promise automatic saved context.

## Implementation ownership and acceptance gates

- Backend owner: `application.rs`, new `discussion_context.rs` and tests;
  narrow typed/internal reuse in `runtime/goal_brief.rs`; `chat.rs` body preparation;
  service/protocol wiring, crate module registration and API docs. No shell edits.
- Shell owner: `crates/okilum-shell/src/brain.rs` and new
  `brain/discussion_context.rs`; isolated native fixture and provider capture.
- Reviewer/root approve this contract and API before functional edits.

Focused tests must prove same-goal reply/outcome continuity with a new empty stage;
automatic other-goal exclusion and preserved explicit cross-goal manual references;
manual/original precedence and exact-revision dedup; real
serialized budgets/invalid UTF-8/inventory omissions; receipt equality after note
changes, later turns and reopen; missing/corrupt/future envelopes; all persistence
fault boundaries and no network before a durable projection; interrupted/uncertain
requests never auto-retry. Capture the actual provider body, verify its exact hash
against the stored receipt, and compare its context with the native expanded
snapshot. Prove cumulative limits and that receipt storage does not duplicate
the growing transcript; preserve all earlier snapshots when another send is refused.

Use frozen old enabled/maintenance binaries on disposable brains to prove new→old
continue/recover/unrelated task checkpoint→new preserves existing envelope bytes,
new turns get unavailable history, and pending canonical projections survive old
recovery. Do not substitute a mocked serde roundtrip for this gate or restore old
journals over newly retained turns. One native synthetic flow demonstrates saved
Attention reply/outcome → Discussion request → reopen with visible exact context
and unchanged task/stage/completion counts. Normal CI, reviewed artifact publication
and isolated native cleanup remain required; no live alpha/bot/linux-reference-host/T3/Maestro work.

## Starting another conversation (#242)

Native Discussion offers **New conversation** within the selected goal, including
when the current conversation reaches its retained-turn or request-history limit.
It opens an explicitly labelled empty history and carries the unsent question and
manual source selection. This local draft creates no canonical conversation and
makes no provider request. Only explicit **Send** invokes the existing `chat_start`
with a null conversation ID; the acknowledged ID becomes the selected conversation.
Old messages and context receipts remain unchanged and selectable. No transcript
is trimmed, copied or summarized into the new conversation; ordinary saved goal
inputs and source-reference rules continue to apply.

The explicit draft mode belongs to its goal and survives foreground/background
refresh and switching away and back within this desktop session. New and Send
respect running/in-flight requests, pending capture, note review and source guards.
A matched backend `chat_context_limit` rejection proves pre-dispatch refusal and
leaves the draft available for an explicit new conversation.

A lost acknowledgement of a null-ID request is different: the backend assigns the
conversation ID, so the client cannot prove ownership from the newest conversation
or matching text. It retains a goal-bound unconfirmed-send block while allowing
read-only inspection of saved histories. Refresh, history selection and goal
switching never silently release that block or resend the request. After inspecting
saved conversations, the user can explicitly choose **I checked saved conversations**.
It clears only the current goal's transient send guard, preserving the draft,
manual sources and selected history (or empty new draft). It makes no RPC/provider
call and does not claim an acknowledgement or retry of the original request. The
notice keeps the original delivery unknown; any subsequent Send is a separate
explicit action. This transient block and unsent draft are desktop-session state, not a durable retry journal;
restarting the desktop does not prove that an uncertain request was never accepted.
Saved conversations/receipts remain canonical and reopen through the existing API.

The new unconfirmed-send guard applies to explicit **New conversation** only.
Existing conversations and the original implicit first-conversation path retain
their prior send/error behavior; this change does not retrofit a durable or
idempotent dispatch protocol onto those paths.

The approved [durable send recovery contract](ai-brain-discussion-send-recovery.md)
tracks the next bounded API/outbox extension in [#244](https://git.oklabs.uk/BeFeast/okilum/issues/244).
It is specified but not implemented; the session-only behavior above remains the current behavior.

## Explicit user decisions

The same-goal brief may include a [saved Discussion decision](ai-brain-discussion-decision.md).
New turns retain one exact citation per path/revision, with original user attribution
and `unverified` authority. Manual pins merge with automatic input by the existing
identity rules. The automatic copy is suppressed in the originating conversation
while that exact user turn remains in its transcript; explicit manual pins survive.
Existing frozen turn/envelope schemas and previously retained inputs are not rewritten.

[Manual-only decision reuse](ai-brain-discussion-decision-reuse.md) removes only the future automatic brief contribution. Existing transcript text and prior frozen turn receipts remain exact. Explicit manual references may still include the current saved source; old-revision pins need normal stale review. A policy change does not send a provider request or erase previous inputs.
