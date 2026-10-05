# AI Brain isolated acceptance preparation

Preparation for #61, 2026-09-05. This document contains no live task, thread,
provider request or successful acceptance receipt. The product requirements remain
[the POC scope](ai-brain-poc.md) and [ai-brain/v1](ai-brain-contracts.md).

Development, review and recovery stay in external T3 Code. This fixture does not
make Tessera responsible for its own development or migrate daily work. Executing
this isolated synthetic task through the actual Tessera UI, Todoist and T3 can
satisfy the POC's functional acceptance. A mocked or backend-only scripted run
cannot. No additional personal-project run is required by this procedure.

## Inputs and isolated placement

- [Goal template](../fixtures/ai-brain-poc/goal.md): one initial thought, one
  conversation constraint, and an explicit review criterion.
- [Workshop brief](../fixtures/ai-brain-poc/sources/workshop-brief.md) and
  [supplies](../fixtures/ai-brain-poc/sources/supplies.md): the only initial brain
  source notes. Their frontmatter, wikilinks and distinct markers help distinguish
  raw source from rendered/transformed text.

Use a newly allocated test brain with an existing empty owned-records directory,
and a separate durable operational directory. Copy only `sources/*.md` into that
brain. Do not copy the goal template as `goal-*.md`: canonical goals must be created
by the backend, and existing owned goals without their journal require recovery.
Keep the acceptance oracle in this document outside the engine's source context.

Record a fresh brain ID, source paths and SHA-256 revisions in the run receipt.
All cooperating writes must use the managed source store. Live personal notes and
the development checkout are not fixture storage. Do not delete operational state
to reset a failed run; preserve it for external recovery and use a new isolated run.

## Configuration receipt required before a live attempt

| Field | Current value |
|---|---|
| Exact application/backend build and source commit | Unselected |
| Always-available backend host and process lifetime | Unselected |
| UI host and connection to that backend | Unselected |
| Brain ID, fixture root and operational directory | Unallocated |
| CLIProxyAPI endpoint and selected model | Unselected |
| Todoist account instance and dedicated test project ID | Unselected |
| T3 instance, test project/root and thread-opening route | Unselected |
| Secret loader references | Bind locally; never put values in the receipt |
| Concrete live operations and cleanup policy | Not authorized by this document |

When these values and the build exist, record exact startup/connection commands
and provider operations for review. Do not substitute guessed CLI options or a
currently empty adapter registry. Verify the real application API before binding
this fixture to it. The prepared runtime JSON-lines service alone is not evidence
that the complete UI/provider vertical is connected.

The intended provider mutation scope is one identifiable test task, one dedicated
T3 thread/turn with the prepared context, and a Markdown result in the fixture.
This is a proposed scope, not permission to invoke those operations now. Automatic
closing/deletion of the Todoist task is optional and must be explicitly included
in the concrete run's operations; the task remains authoritative in Todoist.

## Planned application API binding

The application lane's frozen `docs/ai-brain-application-api.md` was inspected on
2026-09-05. It is a planned integration surface; check the actual build's
`capabilities` before treating it as available. The UI, not the operator typing
opaque IDs, owns these calls.

| UI action | Application contract observation |
|---|---|
| Check connected capabilities | `capabilities`: chat, Todoist, T3 and source writes enabled; use its actual local actor for review |
| Select the two notes | `source_list`, then `chat_start` with the goal ID and selected `source_paths` |
| Follow native conversation | `chat_get` reaches `complete`; partial/interrupted text is not a completed reply |
| Create/associate test task | `task_create` or `task_link`; use `task_reconcile` for an indeterminate existing mutation |
| Prepare the handoff | `stage_prepare` freezes the selected sources and conversation with the actual ID of criterion C1; existing `start` initiates it |
| Inspect returned artifact | `snapshot.source_paths.result` and `result` using `result_id`; a correlated `thread_url` opens the real thread |
| Independently review this fixture's artifact | Open the saved result and evidence; compare its actual contents with the oracle below |
| Record required human acceptance | Existing `accept_human` with the actual C1 ID, actor/time and reviewed artifact reference, only after the oracle passes |

Actual T3 outcomes may arrive with empty `criterion_evaluations` and unverified
evidence. Because this fixture deliberately uses `requires_human:true`, independently
review the artifact and then record `accept_human`. The backend retains a canonical
human receipt and re-evaluates the saved result; `criterion_evaluate` is for nonhuman
criteria and must not be called for this C1. Generic project approval or a provider's
claimed review cannot replace the actual artifact review. C1 is the procedure's
label; when the UI generates its ID, retain and use that actual ID in observations.

## Observable interactive run

1. Capture the template's thought in the actual Tessera inbox. In the native
   conversation, establish the planning-only constraint and criterion `C1`.
   Starting the thought is user input; carrying source context/results by hand
   between Tessera and T3 is a failed context-continuity check.
2. Select both source notes through Tessera. Inspect the source view and preview;
   confirm the raw source retains frontmatter and wikilinks. Retain the actual
   selected revisions and resulting context packet revision.
3. Through the connector, create or associate the selected test task. Record its
   real account-scoped ID and observed status. A local Markdown task reference
   or successful HTTP fixture is insufficient.
4. Dispatch from Tessera to the dedicated real T3 stage. Open its real thread from
   Tessera. Using external T3 for observation, verify the received context contains
   the goal, criterion, constraint, both source references/revisions and the facts
   needed for the plan. Both source markers must be traceable in the prepared
   source context. Do not paste missing context into the thread.
5. During the stage, close/reopen only the Tessera UI. The same backend goal,
   operation and T3 thread/turn must continue. Record evidence of that identity
   before and after reconnect, not merely that the window reopened.
6. Inspect the automatically returned result in Tessera and its saved Markdown.
   It must identify the same goal/stage/operation and both sources. Do not manually
   ingest a manufactured outcome or copy the T3 reply into a source file.
7. Before review, `C1` must remain unmet and the result must retain its actual
   verification status. Independently review the saved artifact against the oracle
   below. On a passing review, record explicit `accept_human` for C1 with the actual
   reviewer/time and the reviewed artifact reference. If any required value is
   wrong or unsupported, leave C1 unmet and record the discrepancy. Confirm the
   canonical human receipt and resulting criterion evaluation exist, and completion
   follows the actual review without a second T3 execution.
8. Check that a decision/final result appears in attention, while ordinary progress
   does not generate a notification for every step. Record the actual notification
   surface/transport; do not assume mobile delivery.

Backend restart, lost acknowledgement, duplicate events and stale-source conflicts
must also be exercised with their owned regression fixtures. A live restart drill
requires a named isolated backend process and recovery commands in the run receipt;
do not restart a shared T3 service or unrelated host to simulate failure.

## Independent artifact oracle

The reviewer checks the saved result, not the engine's assertion that it checked
itself. Equivalent clear wording is acceptable; the values and distinctions matter.

| Observation | Expected result | Source |
|---|---|---|
| Batteries to pack / buy | Pack 4; buy 0; 2 remain in stock | Both notes |
| Markers to pack / buy | Pack 2 total; buy 1 additional | Both notes |
| Tape to pack / buy | Pack 1 roll; buy 1 roll | Both notes |
| Shopping cost | 4 credits; within the 5-credit limit | Both notes |
| Shopping deadline | 2026-09-11, 18:00 local time | Workshop brief |
| Departure / workshop start | 09:10 / 09:30 local time on 2026-09-12 | Workshop brief |
| Provenance | Both original note references and retained revisions are inspectable | Actual run context/result |
| Scope | A plan exists; no claim that shopping/packing actually happened | Goal constraint |

A missing/wrong value, unsupported citation or result absent from Markdown is a
failed criterion. Preserve the returned artifact and ask for correction; do not
rewrite the criterion to match it or mark the goal complete. Do not fill evidence
fields with planned checks, empty source URLs, fabricated timestamps or a generic
approval of the project. Physical shopping/packing remains outside this goal.

## Acceptance receipt

Save the actual build/config references, correlated IDs, criterion definitions,
source/context revisions, result Markdown path/revision, verification evidence and
observed attention/reconnect behavior. Include the reviewer and time only after
the review occurred. Record failures and limitations alongside the successful
observations. Distinguish mocked/scripted implementation checks from the actual
live interactive fixture receipt; only the latter can establish the complete
UI/provider acceptance described here.

Until those observations exist, the status is **prepared, not live-verified**.
