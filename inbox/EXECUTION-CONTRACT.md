# Execution connectors: slice 2 contract and PR 0 findings (#510)

Status: T3 isolated live probe passed for an existing project workspace;
Maestro source contract blocked separately by BeFeast/maestro#1288.
Oleg approved proceeding with T3 while the Maestro executor supplies that API. This document does not enable execution in Inbox.
The detached API/web remain on CT119, LAN-only, with a fixture vault. Reader,
real vault access, Internet exposure, and production project execution are unchanged.

## Ownership and bridge boundary

Inbox owns captures, conversations, brief revisions, explicit project links and
operation intents. T3 owns threads/runs/questions; Maestro owns its workers and
approvals; Forgejo owns issues, PRs and builds. Reading or marking an item processed
never answers a question or authorizes execution.

A narrow bridge runs beside the execution sources and connects outbound to Inbox.
It accepts only configured instance/project/action identities, not arbitrary URLs,
shell commands or user-supplied workspace paths. Secrets stay outside payloads,
SQLite and logs. The bridge credential is distinct from the browser session.
The API cannot choose an actor from a request body. Both endpoints must bind each
operation to the authenticated owner and configured source scope.

Persist the following before an external call:

- Inbox operation ID, kind, owner, exact payload and payload fingerprint.
- Source instance ID, project ID, thread/request ID and observed question revision.
- For launch: exact brief revision, model/runtime settings, approved workspace
  identity, deterministic thread/create/message command IDs and message ID.
- Delivery state and external identities; these are durable operational data,
  never part of a deletable cache.

Same operation + same payload reuses the original intent. A changed payload is a
conflict, even if the source itself silently deduplicates by command ID alone.
Use queued/accepted/running/completed/failed/uncertain distinctly. An accepted
command is not a completed executor. Lost responses require source reconciliation;
never generate replacement launch IDs to make a retry appear successful.

## T3 protocol 2: confirmed narrow path

The live server advertises orchestration protocol 2. Authenticated reads use
`GET /api/orchestration/threads/{threadId}/bounded`; older timeline pages use the
history cursor. `runtimeRequests` and matching `user_input_request` turn items
supply native request identity, response capability, question IDs/options and status.
The bridge must retain/reconcile unresolved requests across bounded-history pages;
an absent request in a truncated response is not a withdrawal.

Commands use the existing authenticated `/ws?orchestrationProtocol=2` RPC transport,
`orchestration.dispatchCommand`. Store IDs before sending:

| Purpose | Command | Identity / guard |
|---|---|---|
| Create in an already prepared project workspace | `thread.create` | commandId, threadId, projectId, explicit model/runtime/interaction settings, branch/worktree binding, creation actor/source |
| Deliver brief | `message.dispatch` | independent commandId and messageId, same threadId, exact text, explicit queue-after-active mode |
| Answer a native question | `runtime-request.respond` | commandId, threadId, requestId, answers keyed by native question ID |

Create and brief delivery are separate durable phases. On recovery, locate the
persisted thread ID, verify its project/workspace, then reconcile the stored message
ID before resending the exact command. UI must show a prepared thread whose brief
has not yet been accepted. Reconcile after backup restore before releasing any queue.

Do not substitute ordinary thread text for a native question response. Require a
pending request with an advertised supported response capability; a resolved,
dismissed or unavailable request is not answerable. Preserve the displayed question
and revision; if source identity/content changes, refresh rather than target a new
question. Provider-specific async questions need their own acceptance case.

The high-level `t3_thread_launch` wrapper has no retry key. It is not the bridge's
replay primitive. The native V2 command path supports caller-held IDs. This probe
covers the existing pilot project root, not fresh worktree preparation: new-worktree
launch stays disabled until preparation, binding and crash recovery are separately
verified through app-owned workspace operations. No generic shell workaround.

## Isolated live probe, 2026-10-05

An empty, separately registered Inbox pilot project was used; no Tessera development
thread or real project was controlled. The probe inherited the current Codex model,
used plan/approval-required mode and instructed the executor to perform no file,
shell or external-service work. The only question was a choice between Blue/Green.

1. Repeated identical thread-create and brief-dispatch commands returned their
   original sequence acknowledgements; one thread and one run were observed.
2. A pending native question with a live response capability appeared. This is
   the positive control that the provider actually asked, rather than a silent run.
3. Sent Blue, deliberately discarded its acknowledgement, then read back the
   resolved request and exact answer through the source snapshot.
4. Repeated the same answer command: original acknowledgement, no second delivery.
5. Sent Green under a new command ID to the resolved request: source rejected it.
6. The original executor completed with `PILOT_RESULT:Blue`; the original run stayed
   the sole run. This demonstrates receipt by the executor, not just HTTP acceptance.

This simulates a lost acknowledgement at the caller, not a network partition or
server crash. Restart/restore and newly prepared worktrees remain required tests
in the implementation PRs; this observation does not claim those gates passed.

## Maestro: source dependency, not an Inbox workaround

Read-only inspection of fetched Maestro `origin/main` (2026-10-02) found fleet
observations and approval actions, but no native question/thread reply endpoint.
`approvalDecisionRequest` contains actor/reason; the authenticated actor overrides
the supplied actor. Approve/reject claims a pending approval once and rejects stale
or already claimed records. This is useful, but does not supply a caller's expected
question/action revision or Inbox operation identity for end-to-end reconciliation.
The old Brain adapter's expected payload does not add these fields to the source.

Needed before enabling Maestro write-back:

- Stable source question/thread identity and pending/answered/withdrawn observations.
- A reply/decision action binding that identity to the expected displayed revision.
- Stable caller operation identity, exact-payload conflict checking, and a read path
  to distinguish accepted delivery from an unresolved network outcome.
- A way to target an isolated pilot; no approval of live deployment/merge actions
  merely to demonstrate connectivity.

The dependency was reported to the manager before any changes to Maestro. No
foreign repository product files were changed. An Open original link is a fallback,
not acceptance of the promised answer-in-Inbox flow. Do not enable plain approval
POSTs as a substitute, or report the whole PR 0 gate green.

## Next gate

Merge this contract/evidence independently of enabling connectors. Proceed with
T3 through the approved sequence while Maestro remains disabled: durable
store/API, question bridge/web, explicit brief launch, Forgejo aggregation, project
screen and LAN pilot acceptance. Product code remains absent from this PR.


## New-worktree launch probe, 2026-10-06 (PR 3)

On the isolated seeded pilot repository, the native `orchestration.launchThread`
RPC prepared a new app-owned worktree at a pinned commit. Repeating the same
command/thread/message IDs returned `resumed: true`; the source retained exactly
one initial user message and one completed run. The executor returned the requested
`INBOX_LAUNCH_PILOT_READY` marker without file, shell or service actions. This closes
the native preparation/binding probe gap above, not Oleg's web acceptance gate.
PR 3 adds journal/restart/restore tests and the separately confirmed web launch.
Maestro remains disconnected regardless of the source API's availability.
