# AI Brain foundation contracts — ai-brain/v1

Implementation contract for [the approved POC](ai-brain-poc.md), 2026-09-05.
**Specified, not yet implemented.** JSON below describes transport-neutral domain
values; these are not current cored commands or MCP tools. The existing reader
[protocol v0.3](PROTOCOL.md) is unchanged. New transport exposure must advertise
`ai-brain/v1`; incompatible schema changes require a new version.

## 1. Shared conventions and authority

- IDs are opaque UUID strings, allocated once before a record/operation is sent.
  The examples use readable labels in prose, but serialized IDs must be UUIDs.
- `brain_id` identifies one configured brain; its host-specific root is configured
  outside records. Paths are relative to that root. Reject absolute paths, `..`,
  and symlink traversal escaping it. Preserve existing file paths and bytes.
- Existing notes retain v0 path identity; do not insert IDs into unrelated notes.
  New Tessera-owned goal/stage/context/result documents carry stable record IDs.
  File moves do not change these IDs; duplicate IDs are an explicit conflict.
- Timestamps are UTC RFC3339 strings. Missing facts are `null` or an empty list
  as defined below, never guessed values. Schema-required fields remain present.
- Markdown is canonical for durable goal/decision/context/result records. Runtime
  state includes dispatch intents, adapter bindings, acknowledgements and cursors;
  it is durable operational data, not derived cache. Search/vector indexes are
  derived. Deleting an index must not delete operational state or dispatch history.
- Todoist is task authority during transition. Store its reference and observed
  status/time as a projection. A successful projection write does not complete,
  edit or recreate the remote task.

## 2. Exact source boundary

`SourceRead(brain_id, path)` returns `SourceSnapshot`:

```json
{
  "schema": "ai-brain/v1",
  "brain_id": "01000000-0000-4000-8000-000000000001",
  "path": "notes/example.md",
  "revision": "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
  "content_base64": "aGVsbG8=",
  "media_type": "text/markdown"
}
```

The revision is SHA-256 of exact bytes (`hello` in this example). No frontmatter
removal, link rewriting, newline normalization, Unicode normalization or formatting
occurs. A missing path returns `not_found`, not empty content. The source editor
decodes supported text losslessly; invalid UTF-8 remains available as exact bytes
and must not be overwritten through lossy text conversion. Render separately.
Existing MCP `read_note` is unsuitable: it strips frontmatter and rewrites links.

`SourceWrite` contains `schema`, `operation_id`, `brain_id`, `path`,
`expected_revision` and `content_base64`. `expected_revision: null` means create
only when the path is absent; otherwise it must equal the current byte revision.
Success returns `operation_id`, `path`, `previous_revision`, `revision` and
`outcome: written | unchanged`. An identical retry of the same operation returns
its recorded result; a reused operation ID with different input is `invalid_request`.

Serialize cooperating writes per path. Re-read and verify the expected revision
under that serialization boundary before committing. Persist the operation intent
and recoverable preimage; atomically replace the file only after the new content
is fully written. Acknowledgement follows a durably recorded outcome. A crash
between file replacement and acknowledgement is reconciled against the intent and
actual file revision, not blindly retried. Index updates follow the committed file.

A stale revision returns `conflict` with `path`, `expected_revision`,
`current_revision`, and a durable `conflict_id`. Keep base (when supplied/retained),
current and proposed bytes recoverable. Do not place conflict markers into the
canonical note as an implicit resolution. Resolution is another write with the
then-current expected revision. Independent edits may be merged only with an
available common base and demonstrated non-overlap; overlapping edits keep both
versions. Missing base means conflict, not invented merge history. The current POC
uses explicit resolution, not automatic merging. The application API can retain
an optional validated base snapshot and expose the preserved versions for review;
see [source conflict inspection](ai-brain-application-api.md#source-conflict-inspection-and-explicit-resolution).

Atomic rename alone is not compare-and-swap against an unrelated external editor.
The minimum supported POC commit boundary is a managed test brain with cooperating
writers serialized through the source store, or another explicitly enforced writer
exclusion mechanism. If that boundary cannot be established, retain the proposed
bytes as a conflict/proposal and do not replace the canonical file. Sequential
external edits between read and submit still produce revision conflicts. Arbitrary
unmanaged writers racing during commit are not supported by this minimum contract;
support requires an explicit exclusion or recoverable conflict mechanism. Document
the actual guarantee and never claim advisory locks exclude non-cooperating writers.

Read/write errors: `not_found`, `invalid_path`, `invalid_request`, `conflict`,
`io_error`, `indeterminate`. An indeterminate write retains its operation ID and
recovery record until reconciled. Do not report it as a successful save.

## 3. Durable record shapes

New owned Markdown documents have YAML frontmatter with `schema: ai-brain/v1`,
`record_type`, `id`, `brain_id`, and the typed fields below; the Markdown body is
human-readable content. Preserve unknown frontmatter keys on updates. Examples
show JSON projections of that same data, not a second authoritative JSON store.
Choose record paths through configured placement; do not impose a new vault layout.

### Goal

```json
{
  "schema": "ai-brain/v1", "record_type": "goal",
  "id": "02000000-0000-4000-8000-000000000001",
  "brain_id": "01000000-0000-4000-8000-000000000001",
  "title": "Explain a selected change",
  "status": "active",
  "criteria": [{"id": "C1", "description": "A cited explanation is saved", "requires_human": false}],
  "stage_ids": ["03000000-0000-4000-8000-000000000001"],
  "task_ref": {"provider": "todoist", "external_id": "example-task", "observed_status": "open", "observed_at": "2026-09-05T12:00:00Z"}
}
```

Goal status: `draft | active | blocked | completed | cancelled`.
`task_ref` is nullable; external IDs are opaque strings. Criterion IDs are stable
within the goal. `completed` requires a passing evaluation for each criterion,
including recorded human acceptance when `requires_human` is true. A criterion
definition change (`id`, `description`, or `requires_human`) invalidates an older
evaluation for it. `goal_revision` anchors the exact retained goal snapshot that
was evaluated. Before completing a goal, compare current criterion definitions
against that snapshot. Mechanical status/result-reference projection updates do
not invalidate evidence for unchanged criteria: writing `status: completed` must
not invalidate its own receipt. Missing evaluated snapshots prevent verification.

### Stage and context packet

```json
{
  "schema": "ai-brain/v1", "record_type": "stage",
  "id": "03000000-0000-4000-8000-000000000001",
  "brain_id": "01000000-0000-4000-8000-000000000001",
  "goal_id": "02000000-0000-4000-8000-000000000001",
  "engine": "t3", "status": "ready", "criterion_ids": ["C1"],
  "context_id": "04000000-0000-4000-8000-000000000001",
  "result_ids": []
}
```

Stage status: `ready | submitting | running | outcome_ready | blocked | completed |
cancelled`. `outcome_ready` means the engine supplied a result, not accepted goal
completion. Context documents use `record_type: context`, their own `id`,
`brain_id`, `goal_id`, `stage_id`, `goal_revision`, `goal`, `decisions`,
`constraints`, `sources`, `previous_result_id` (nullable), and `next_step`.
`goal` and `next_step` are strings; decisions/constraints are string lists.
Freeze the exact context revision for each dispatch. Later edits create a new
revision for a future dispatch; never silently mutate what a running stage received.

Source references are `{uri, revision, locator}`. URI may identify a brain-relative
file or an external repository/document; `revision` identifies exact bytes/commit
when known, otherwise `null`. `locator` is a heading/block/range hint or `null`.
Unknown revision explicitly limits reproducibility. A packet must retain source
references alongside any AI summary. External canonical docs are referenced, not
copied into a competing authoritative document.

### Result and evidence

```json
{
  "schema": "ai-brain/v1", "record_type": "result",
  "id": "05000000-0000-4000-8000-000000000001",
  "brain_id": "01000000-0000-4000-8000-000000000001",
  "goal_id": "02000000-0000-4000-8000-000000000001",
  "stage_id": "03000000-0000-4000-8000-000000000001",
  "operation_id": "06000000-0000-4000-8000-000000000001",
  "engine_ref": {"engine": "t3", "instance_id": "test-instance", "thread_id": "example-thread", "turn_id": "example-turn"},
  "outcome": "succeeded", "summary": "An explanation was produced.",
  "sources": [], "evidence": [], "verification": "unverified",
  "criterion_evaluations": [], "received_at": "2026-09-05T12:01:00Z"
}
```

Outcome: `succeeded | failed | cancelled | unknown`; verification:
`unverified | partial | verified | failed`. The example deliberately has a successful
engine outcome but no verification. Evidence entries contain `id`, `kind`,
`source` (the source-reference shape), `description`, `observed_at`, and `status`
(`unverified | passed | failed`). Criterion evaluations contain `criterion_id`,
`goal_revision`, `status` (`unverified | passed | failed`), `evidence_ids`,
`evaluated_by`, and `evaluated_at`; human acceptance, when required, is evidence
whose `kind` is `human_acceptance`. Never fabricate actor, time or evidence.

Allocate/persist a result ID when ingesting its external event identity; replays
reuse it. Persist result content before projecting `result_ids`/goal status, and
recover interrupted multi-file updates through the operational journal. No atomic
cross-file transaction is assumed. Contradictions create visible unresolved findings;
do not silently overwrite an earlier decision with the newest engine claim.

## 4. Runner and adapter boundary

Adapters implement transport-neutral `start`, `observe`, `reconcile` and capability
discovery. `cancel` is optional and must advertise support. `start` receives:
`schema`, `operation_id`, `goal_id`, `stage_id`, `context_id`, `context_revision`,
the prepared packet, and non-secret target identifiers. Persist this exact envelope
before network I/O. One logical dispatch has one operation ID across retries.

Start replies are `accepted` (with external binding), `rejected` (proven not
started), or `indeterminate` (may have started). An external binding includes
engine, instance and relevant thread/turn/task IDs; IDs are scoped to that instance.
An idempotency key is sent only when the provider supports its semantics. A lost
reply without provable idempotency requires reconciliation, not a fresh start.

Observe events carry `engine_ref`, `event_id`, `cursor` (nullable), `kind`,
`observed_at`, and `payload`; kinds are `status | outcome | attention`.
Deduplication keys include engine instance and event identity. If a provider lacks
stable event IDs, its adapter must establish a deterministic identity/reconciliation
strategy before replay is supported; timestamps alone are insufficient.

Correlate each event to the exact operation and external thread/turn binding;
thread identity alone must not attach an earlier turn's outcome to a later attempt.
Include the provider's event-ID scope (such as thread/stream) in deduplication keys.
Persist ordering/cursor per stream or reconcile an authoritative snapshot before
projecting ambiguous order. A stale/replayed `running` event cannot regress an
`outcome_ready` or completed stage. Keep raw event evidence even when its state
projection is rejected as stale; an unbound event cannot complete a stage.

Persist bindings, observed events and outcome intake before acknowledging receipt
or advancing a replay cursor. On startup recover incomplete intents first. Reconcile
returns `running | outcome_available | not_started | unknown` with its evidence.
Retry is allowed only for proven `not_started` or provider-proven idempotent replay.
`unknown` stays blocked with the original operation ID and a visible explanation.
Do not use a timeout, process absence or empty snapshot as proof of `not_started`.

For T3, use its typed WebSocket create/start and snapshot/event/replay primitives;
map actual status and outcome/diff evidence. Goal/thread mapping and extraction are
Tessera responsibilities. A web route exists; verify the chosen opening path rather
than assume native desktop deep links. For Todoist, keep remote task authority and
validate recurrence/reminder semantics for the selected operation. CLIProxyAPI serves
native conversation; model errors/fallback must remain observable.

Runner lifecycle cannot depend on the desktop connection. A reconnect gets the
current durable projection and resumes observation of existing work. Notifications
are correlated records of blockers, required decisions and final outcomes; progress
stays in history. Delivery retries must not repeat stage execution.

## 5. Foundation failure checks

Implementers must prove the relevant cases in their owning slices:

- Raw read/save preserves byte-for-byte frontmatter, links and line endings; a
  rendered `read_note` response is never substituted for raw source.
- A stale save preserves current/proposed versions; crash after replace before
  acknowledgement reconciles one write operation without losing its preimage.
- Goal/task/thread IDs remain associated across reconnect and runner restart.
- Lost start reply after the engine began does not dispatch a second stage.
- Duplicate outcome replay creates one result and does not advance goal completion
  without criterion evidence.
- Crash after result Markdown but before goal projection recovers the projection
  without creating another result or repeating external execution.
- Successful engine outcome with missing/failed evidence remains unverified/failed;
  required human acceptance cannot be inferred from generic approval of the project.
- Index deletion leaves operational journal/bindings intact. Lost operational state
  is a recovery incident: never reconstruct it as permission to replay remote work.

The POC's actual interactive acceptance remains in [scope and acceptance](ai-brain-poc.md).

## 6. Undispatched preparation correction — local workspace API

Issue [#98](https://git.oklabs.uk/BeFeast/tessera/issues/98) adds two guarded
operations to `ai-brain/workspace-v1` (the explicit legacy `ai-brain/v1` transport
also accepts them). Capabilities advertise `prepared_stage_edit: true` and
`guarded_start: true`. Clients connected to an older backend must not offer these
operations merely because they can display a prepared context.

A snapshot with a dispatch supplies `prepared_guard` with `stage_id`,
`stage_revision`, `context_id`, and `context_revision`. Revisions identify exact
saved Markdown bytes. `can_change_prepared` is true only for a never-dispatched
preparation, with no binding. An attempted but rejected Start (`not_started`),
uncertain Start, running stage or retained outcome cannot be revised/discarded.

```json
{
  "schema": "ai-brain/workspace-v1", "id": "client-request-id",
  "expected_workspace": {"brain_id":"configured-brain-uuid","root":"configured-root","records_dir":"records","managed":true},
  "op": "stage_revise", "goal_id": "goal-uuid", "operation_id": "stable-change-uuid",
  "expected": {"stage_id":"stage-uuid","stage_revision":"sha256:exact-stage-bytes","context_id":"context-uuid","context_revision":"sha256:exact-context-bytes"},
  "next_step": "The complete corrected instruction"
}
```

UUID placeholders above must be replaced with actual UUID strings. Copy `expected`
verbatim from the snapshot under review. `stage_discard` uses the same request
without `next_step`. A guarded `start` uses `goal_id` and the same `expected` object;
it retains the dispatch's existing operation ID rather than allocating a new one.
Snapshot guards also exist in `not_started`, so a proven-not-started retry can use
its current exact guard. `requires_guarded_start` states whether an unguarded legacy
Start is forbidden for this goal.

The change response is the normal application snapshot plus `prepared_change`:
`{operation_id, goal_id, action: revise|discard, previous: guard,
replacement: guard|null}`. Persist one change operation UUID and its exact request
before sending. An identical retry returns the same durable receipt plus a fresh
snapshot even if a later Start/change has advanced the goal. Reusing that UUID
with another goal, action, guard or text is rejected. The generic request `id` is
only transport correlation; it does not replace the operation UUID. A lost reply
is reconciled by repeating the exact change request, never by preparing or
starting another stage. No provider is contacted by either change operation.

### Identity and history

Revision changes only `next_step`. It creates new stage, context and dispatch IDs
derived from the change operation UUID, retaining the complete frozen packet:
goal snapshot/revision, decisions, constraints, source identities/revisions and
excerpts, conversation, prior result and target. It does not re-read/reselect
sources, rerun AI, edit criteria or refresh a task. Changed goal criteria reject
revision; discard followed by a new preparation can deliberately freeze new
criteria. Goal identity, criteria, task reference and existing history remain.

The preceding context file is immutable. Its stage retains its identity and
context link, and becomes `cancelled` with `prepared_disposition: superseded`,
`prepared_change_operation_id`, and `superseded_by_stage_id`. The replacement
stage links back with `supersedes_stage_id`. `goal.stage_ids` retains both;
operational history retains the exact old dispatch envelope. Native History must
be able to open a cancelled preparation's context without requiring a result.

Discard keeps the original context and stage, marks the stage `cancelled` with
`prepared_disposition: discarded`, and sets operational phase `discarded`.
The snapshot keeps the discarded dispatch solely as history/ancestry; Start is
blocked; neither the driver nor an explicit reconciliation request contacts an
engine for it. A later explicit preparation
must retain that packet's `previous_result_id`: null for a first stage, or the
same retained prior result for a follow-up. Thus both initial and follow-up
preparations can be replaced without inventing an outcome or losing prior review.

### Race, recovery and compatibility

The backend request mutex and single-owner Runner serialize Start/change. Every
change compares the exact stage/context guard before queuing any writes. If Start
wins, revision/discard fails without touching its packet. If revision/discard
wins, the old guarded Start fails before adapter I/O. Start without a guard stays
supported on unchanged legacy goals; after any prepared change for that goal it
fails closed. New clients always send a guard when the capability is present.

The change request, allocated identities, receipt, lifecycle projection and
pending Markdown writes are durably journaled before file replacement. Startup
finishes these exact pending writes before accepting more work. Partial projection
or lost acknowledgement cannot allocate another replacement. No atomic multi-file
transaction or rollback of acknowledged newer work is claimed.

A rejected change may return `error.prepared_change_recorded: false` only after
the backend re-reads its durable journal under exclusive ownership and proves that
the requested operation UUID has no entry. The client may then retain its draft,
clear the pending request and explicitly refresh/rebase. A recorded operation,
partial projection, unreadable journal or transport failure never carries this
marker: retain and retry the exact original request. Missing markers are not a
definitive rejection, even when the error message looks like a validation error.

New binaries read existing journals with an absent `prepared_changes` map as empty.
Edited preparations persist a narrow operational phase fence `prepared_edited`,
projected as ordinary `prepared` by the new snapshot API. Pre-#98 runners do not
recognize it as startable, so they cannot accidentally dispatch an edited packet
via their unguarded Start path. `discarded` is likewise not startable by old code.

**Running a pre-#98 backend on a journal after prepared changes is unsupported.**
Although its string phase parsing succeeds, its serializer drops unknown receipt
fields and cannot preserve the new replay contract. The phase fence reduces unsafe
Start behavior; it is not a downgrade migration. Preserve the current operational
journal and use a compatible backend/forward correction. Never restore an older
operational snapshot over newer acknowledged work. Reader/index rebuilds remain
independent of this durable operation history.

Automated verification covers initial/follow-up revise/discard, frozen source and
prior-result preservation, idempotent retries after restart and later Start, stale
guards, unchanged legacy journal loading, the old Start allowlist fence, both
edit-vs-start lock orderings, and recovery after each partial Markdown projection.
A real disposable cored process test exercises capability discovery and the JSON
commands with no provider configuration, then repeats requests after process
restart. These tests do not claim a native user acceptance run.
