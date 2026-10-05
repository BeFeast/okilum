# Explicit Inbox planning — issue #140

P1 contract for [#140](https://git.oklabs.uk/BeFeast/tessera/issues/140).
A saved thought can become a planned goal without copying its context. The
original Inbox record remains unchanged. Native UI and backend use this contract;
the trusted Telegram connector does not gain planning authority.

## User-visible flow

Open a saved thought → **Plan this thought** → review the proposed title, original
thought and at least one observable result criterion → **Create goal and open
discussion**. Creating a goal does not call the LLM, create/link a task, prepare a
stage or Start an engine. Existing explicit controls still own those actions.
Existing origin-linked goals offer **Open goal**; making another goal requires
another explicit form submission. Repeating delivery is not another submission.
Unsaved source/chat/goal drafts remain attached to their original context.

## Native command

Use the existing bound native `ai-brain/workspace-v1` envelope and mandatory
exact `expected_workspace` (the legacy `ai-brain/v1` helper remains supported).
`inbox_plan` is a separate capability for new writes; retained exact requests may
still attempt recovery when this capability is false. An unsupported backend
rejects the command without discarding the client request.

```json
{
  "op":"inbox_plan",
  "operation_id":"durable-client-uuid",
  "capture_id":"canonical-capture-uuid",
  "expected_capture_revision":"sha256:exact-revision",
  "title":"Reviewed goal title",
  "criteria":[{"id":"durable-criterion-uuid","description":"Observable result","requires_human":false}],
  "source":{"channel":"native","instance_id":"persistent-client-uuid","account_id":"local","actor_id":"configured-actor","chat_id":null,"topic_id":null,"message_id":"durable-client-uuid","update_id":"durable-client-uuid","uri":null}
}
```

No client goal ID, source path override, task/engine parameter or provider request
is accepted. Backend allocates the stable goal ID after validating the canonical
capture and retaining the complete immutable intent. Criteria are required, with
nonempty descriptions and unique IDs; no implicit placeholder or passed criterion.
Native request journal records all IDs and exact title/criteria before transport.
Client-only display/navigation metadata is removed from the transport envelope.

Success is `{receipt,goal_id,origin}`. Receipt uses the existing
`{operation_id,status:"committed",request_sha256,replayed}` shape. No snapshot or
implicit goal selection is returned. UI requests the exact goal snapshot only
after a matching receipt and only while the originating navigation still applies;
otherwise it offers a link to the created goal.

`inbox_get` gains `planned_goals:[{goal_id,title}]`, derived from retained canonical
origin relationships. An absent field from an older backend means planning links
are unavailable, not evidence that no goal exists.

## Canonical origin

Goal's flatten-preserved metadata `origin_inbox` is the exact returned origin:

```json
{
  "schema":"ai-brain/inbox-origin-v1",
  "brain_id":"canonical-brain-uuid",
  "capture_id":"canonical-capture-uuid",
  "path":"records/inbox-capture-uuid.md",
  "revision":"sha256:exact-revision",
  "text":"Original exact thought body",
  "source_snapshot":{},
  "operation_id":"durable-client-uuid",
  "planned_by":{},
  "planned_at":"UTC RFC3339 timestamp"
}
```

`source_snapshot` is the full existing SourceSnapshot, with exact original bytes,
frontmatter and source provenance; `planned_by` is the validated native SourceIdentity
from the command. Backend reads these from the verified capture, never from client
preview text. The canonical goal body links to the capture. Neither planning nor
subsequent goal progress changes the Inbox body, provenance or status.

Chat and visible context preparation include the retained origin as **Original
thought — operator input, unverified**. This is not a retrieval evidence citation.
The origin appears in the displayed packet before review, counts toward its byte
limit, and is never appended invisibly at Start. Origin identity participates in
freshness for goals that have an origin, while no-origin legacy hash behavior
stays compatible. This slice does not edit or refresh the pinned origin.

## Durable operations and errors

Workspace-owned plan operations retain normalized request/source binding, stable
goal identity and original source snapshot before canonical projection. Reuse
Runner mutation/persist/pending_writes and SourceStore receipts. Same operation
and same request return the same goal across restart, lost response, and source
changes after commitment. A reused operation/source identity with changed content
or target returns `inbox_plan_operation_conflict`.

Matching operation/alias lookup precedes the current source revision check.
`inbox_plan_source_changed` means the exact request has never been retained or
committed and the capture revision no longer matches. This is the only response
that permits explicit archival of the rejected request while keeping its draft.
Refreshing the Inbox and reviewing the current version is required before a new
planning operation; no implicit rebinding. The error need not include `current`.

`inbox_plan_invalid_request` covers invalid criteria/input.
`inbox_plan_recovery_required` covers recovery/receipt inventory problems. Retained
projection failures and unknown transport replies never become source_changed;
they keep the exact request for recovery. Errors remain structured with their
original code/message. Native recovery is isolated in its own shared-client journal
namespace so capture, attention and planning receipts cannot acknowledge each other.

## Compatibility and validation

New workspace state requires a preserving fallback or tested roll-forward path
before enabling live writes. Never rewind canonical records or operation journals
on rollback. Byte preservation alone is insufficient: a fallback must honor the
same retained origin in chat, visible context and freshness hashes, or explicitly
disable those affected actions for origin-linked goals. Ordinary goals remain
available; capability/recovery UI must never imply that origin-blind execution is
supported. Exact export retains Inbox and goal/origin without derived-index
support; a Markdown-only restore keeps the relationship but cannot fabricate
missing operational receipts.

Validate original Inbox byte equality; required criteria and wrong-workspace/source
rejections; every persist/projection/acknowledgement crash boundary; duplicate and
conflicting request identities; desktop restart recovery; selection changes and
unrelated drafts during replies; linked-goal reopening; provider-free creation;
visible unverified origin in chat and both stage-context paths; no hidden append
or forged citation; exact export and preserving fallback. Native acceptance covers
the thought→form→goal→discussion/context transition. Existing execution controls
remain separately tested; no synthetic evidence is actual phone acceptance.
