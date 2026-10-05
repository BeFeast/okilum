# Persisted AI proposals — issue #166

**Status: proposed P1 implementation contract.** This document defines the next
slice; it does not enable jobs, provider requests, source enrollment or deployment.
Track work in [#166](https://git.oklabs.uk/BeFeast/tessera/issues/166); goal context
continuity from [#160](https://git.oklabs.uk/BeFeast/tessera/issues/160) is a prerequisite.
The [approved autonomy boundary](ai-brain-poc.md#data-autonomy-and-completion)
permits proactive drafts. Existing task, engine and completion authority remains.

## Outcome and scope

A committed Inbox thought, Attention reply or engine result produces one visible,
attributed, cited draft of a useful next action. Reopen/restart retains that draft
and its disposition. **Use draft** opens the existing planning/context form with
editable values, preserving manual pins and guidance. The actual form submission
records adoption only after a durable goal/context receipt exists.

Generation, inspection, rejection, snooze and adoption cannot create a Todoist
task, start T3, control Maestro, satisfy a criterion or complete a goal. Ordinary
proposals appear in the attention overview without a push per proposal. Mobile
proposal commands, autonomous multi-stage continuation, semantic supersession,
whole-brain scans and daily-work migration are outside this slice.

## Committed triggers and stable identity

| Trigger | Eligible event | Owner |
| --- | --- | --- |
| `inbox_saved` | First committed canonical capture | Brain; goal remains null |
| `decision_saved` | Committed saved Attention reply | Exact recorded goal |
| `result_saved` | Committed sourced engine result | Exact recorded goal |

A source-create intent alone is not a committed trigger. Partial Chat tokens,
progress polls, brief refreshes, proposal writes, ordinary source edits and index
rebuilds cannot recursively trigger proposals. Capture exact record ID, path,
revision and original received time after source durability. Persist activation's
cursor before accepting new events; enabling the feature does not backfill history.

Proposal identity is the tuple `(brain_id, trigger_kind, record_id, source_revision,
policy_version)`. Encode each UTF-8 field as its decimal byte length, a colon,
then those bytes; concatenate the five fields in that order and SHA-256 the result.
Record IDs are canonical UUID strings; kinds are the fixed values above; policy
version is a positive integer's canonical decimal representation. Cross-language
vectors, including Unicode and delimiter-containing fields, are required. Do not
hash ambient JSON map serialization: Cargo feature unification can change key order.

Persist the proposal ID/trigger intent before provider dispatch. Repeated delivery
of that tuple returns the same identity and disposition. Rejection cannot regenerate
unchanged input. Policy/model changes do not replay old triggers. Explicit Retry
creates a new durable attempt under the same proposal, never another trigger.
Ordinary external source edits make the existing proposal stale; they do not trigger
a fresh suggestion. A genuinely new committed record is a new trigger.

## Sources and model settings

Capture the exact trigger and bounded existing Citation values. Goal-bound input
uses the visible goal brief, preserving actor/source provenance, verification,
remaining criteria and omissions. An unplanned Inbox body is explicitly scoped
trigger input, not a fabricated knowledge-search citation. Do not infer another
goal from similarity or silently widen scope. Reuse
[retrieval/context freshness](ai-brain-retrieval-context.md#retrieval-boundary).

A versioned, workspace-owned policy selects the existing saved Chat provider/model
and credential reference through [connector settings](../crates/tessera-brain/src/settings.rs).
Freeze non-secret provider/model identity and exact input revisions into each
attempt; resolve credentials at dispatch. Never copy credentials into the proposal,
model response or export. Missing configuration is visible `unavailable`, not an
implicit model/provider fallback. Reuse bounded transport/cancellation from
[Chat](../crates/tessera-brain/src/chat.rs); run outside the Runner lock.

Initial limits: one running attempt per brain, 32 queued trigger intents, 20 exact
citations, 48 KiB cited bytes and 64 KiB total input. Oversized/incomplete input is
visible and is not silently truncated into a confident draft. A full queue retains
its discovery cursor before the unqueued event and shows backlog; no trigger is
acknowledged and lost. Provider output must be bounded to 32 KiB and validate a
fixed response shape: action/title, proposed criteria, rationale, open questions,
and references to captured citation IDs only. Generated content stays unverified.

## Canonical drafts and durable operations

Store each generated draft as canonical Markdown `proposal` under the configured
records directory. Its metadata retains proposal/brain/goal IDs, exact trigger,
attempt/model identity, generation time, `verification: unverified`, proposed fields
and existing citations. Preserve the original generated payload when the operator
edits the destination form. Disposition history (actor, time, revision, rejection,
snooze deadline or adopted target) is canonical too, so exact Markdown export does
not lose user decisions. Source updates use normal revision guards/conflict retention.

Keep trigger/attempt/adoption receipts and cursor state outside the deletable index.
Reuse atomic write/fsync/cancel/restart techniques in
[context jobs](../crates/tessera-brain/src/context_jobs.rs), not its export-job schema:
exports currently require a reviewed goal packet and cannot represent unplanned
Inbox input. Use a create-only source operation retained before writing the draft;
crashes between projection and receipt must recover the same canonical record.

Attempt states: `queued`, `running`, `draft`, `failed`, `interrupted`, `stale`.
User disposition is separate: `unreviewed`, `snoozed`, `rejected`, `adopted`.
Record `running` before network access. Unresolved running attempts become
`interrupted` after restart, without automatically resending a possibly accepted
provider request. Retry is explicit and operation-ID guarded, available only for failed/interrupted
attempts whose frozen sources and pinned provider/model identity are still valid.
It does not regenerate rejected/draft/stale proposals or refresh their context;
regeneration from changed input is outside this first API. Credential recovery may
resolve the same saved reference again. Exactly-once remote provider execution is
not promised. Cancellation/rejection wins a late response.
Timeout, authentication failure or malformed output leaves failure plus original
sources, never fabricated success. Sanitize provider errors before display/storage.

## Native API and adoption boundary

Proposed commands use the existing exact `ai-brain/workspace-v1` guard:

| Command | Required data / effect |
| --- | --- |
| `proposal_list` | Owner filter, bounded page/cursor; derived status and backlog |
| `proposal_get` | Proposal ID; exact source, revision, input citations and attempts |
| `proposal_retry` | Operation ID, proposal ID/revision, authenticated actor; one new attempt |
| `proposal_disposition` | Operation ID, proposal ID/revision, reject or snooze-until; durable receipt |
| `proposal_adopt` | Operation ID, exact proposal revision and reviewed destination-form payload; target receipt |

Reads do not select, prepare or dispatch work. **Use draft** only opens the existing
form; closing it leaves the proposal unadopted. Reject/Snooze do not modify the
original trigger or cancel an engine. Validate workspace, actor and the operation binding first. An exact committed
operation replay returns its original receipt before later source/proposal freshness
checks; a reused operation ID with changed payload fails. New operations validate
ownership and exact current input revisions before mutation, sharing the existing
workspace operation-ID reservation boundary. Unknown/lost responses retain the exact
client request for reconciliation, not an invented replacement operation.

Adoption needs a small durable coordination seam, not a claim that existing APIs
are already sufficient. Inbox adoption uses the existing idempotent
[inbox_plan](ai-brain-inbox-planning.md#native-command) request and records its goal
receipt. Context adoption wraps the existing context form/preparation path, retaining
its target packet identity and complete payload before writes: current
[context_prepare](../crates/tessera-brain/src/application.rs) allocates a fresh packet
per call and cannot itself deduplicate a lost adoption response. Extend its internal
creation seam to reuse the operation-bound target; do not retry arbitrary packet
creation. This extension is part of implementation slice A below.

Canonical adoption records link the successful original goal/packet receipt.
Crash recovery reconciles that receipt before marking `adopted`; it cannot create a
second goal/packet. Preserved pins are merged by exact path/revision without replacing
manual guidance. Changed sources/goal/proposal require renewed form inspection.
Adopting a context draft does not review it: ordinary explicit context review and
stage preparation remain separate, and Start keeps its existing authority boundary.

## Delivery and preserving rollback

This docs-only PR authorizes no implementation or enrollment. Implement in order,
with concrete source review and fixture acceptance at each boundary:

| Slice | Deliverable | Exit evidence |
| --- | --- | --- |
| A — identities/storage/adoption | Trigger cursor, canonical draft/disposition, durable receipts, native wire types and adoption seam; fake provider only | Replay/crash/ownership/adoption tests; no duplicate destination |
| B — bounded generation | Saved provider references, queued worker, cancellation, stale/error handling | Positive provider-call control; interrupted/late-response/failure tests |
| C — existing native forms | Visible cited proposal, inspection/edit/reject/snooze/retry, preserved pins/guidance | Actual GUI flow through destination receipt; cancel leaves unadopted |
| D — integrated acceptance | Isolated three-trigger flow, exact export/reopen and maintenance proof | Native receipt and actual producer/reader binary compatibility |

Before first enrollment, define a dedicated preserving maintenance build: generation
and new adoption may be disabled, but existing drafts/dispositions, pending receipts
and completed target identities remain readable/recoverable. Older unsupported
binaries must refuse before modifying enrolled state. Never restore an old backup
over newer acknowledged knowledge. Test current-writer→maintenance-reader and the
reverse with the actual artifact feature graphs, including goal/review hashes,
citations, pins and original guidance. Source-schema equality alone is insufficient.

## Required acceptance

1. All three positive committed triggers yield one visible draft; delivery replay,
   reconnect, restart and index rebuild neither lose events nor duplicate drafts.
2. Two similar goals and an unplanned Inbox item stay isolated; provenance,
   unverified status, missing context and backlog are visible.
3. Crash before/after provider dispatch and canonical projection retains identity;
   interrupted requests do not auto-resend, and cancellation/rejection beats late output.
4. Provider failure/malformed output remains failure; unchanged rejected input does
   not regenerate. Retry preserves original attempt history and exact input identity.
5. Existing-form editing preserves pins/guidance; cancel does not adopt. Lost
   adoption response and restart recover one original goal/packet receipt.
6. Source/proposal changes block stale generation/adoption; nothing is silently
   refreshed inside a reviewed packet or treated as verified completion evidence.
7. Exact export/reopen retains drafts and canonical dispositions. Deleting indexes
   loses no receipts; maintenance recovery passes the real binary matrix above.
8. Native end-to-end receipt distinguishes implemented/tested/built/installed behavior
   and proves no task creation, T3 start, Maestro control or goal completion. Every
   absence assertion includes a positive control that would detect the forbidden action.

## A3 implementation boundary — issue176

A3 builds on the existing A1 journal and A2 committed feed. Explicit fixture
activation adds `required_proposal_drafts: tessera-proposal-drafts/v1` to the source
binding before extending the A1 journal. Unsupported A2 source handles reject that
binding at fresh open and on later writes from an already-open upgraded handle.
An unenrolled brain gains no proposal state, draft or provider call. Enrollment
remains a Rust test fixture seam; there is no product switch or automatic migration.

A fixture attempt first persists an attributed, **pending and unverified** canonical
proposal through a retained create-only source operation. This record has no
`generated` fields or generation timestamp and does not claim provider success.
Validated fixture output fills the generated fields through a revision-guarded
projection. The result becomes readable only when its exact source receipt exists.
Reject or Snooze appends canonical history without changing original generated
fields. Reject while running frees the running slot and wins any late completion.
Startup interrupts unresolved running attempts without dispatching work.

The Runner owns the same A1 Store for its entire lifetime. Reads neither reopen it
nor interrupt attempts. Source projections, their original bytes and receipts, and
disposition requests remain in that journal outside derived indexes. Source/goal
or proposal changes are reported as stale; new dispositions require current
revisions. Exact committed replay returns the original receipt even after later
source changes or snooze expiry. Operation IDs and upstream identities share the
Inbox/Attention/planning reservation boundary in both directions.

Only `proposal_list`, `proposal_get` and `proposal_disposition` are implemented,
with the exact `ai-brain/workspace-v1` guard. A null owner filter means unplanned
Inbox proposals; an explicit goal selects only that goal. The service exposes read
and disposition capabilities only for enrolled workspaces. Retry, provider dispatch,
adoption and native proposal controls remain unavailable.

The fixed output fields are title, proposed criteria, rationale, open questions and
captured citation IDs. Malformed/oversized output persists only a fixed failure code;
raw provider errors are not exported. A3 captures trigger bytes, exact citations,
goal revision and explicit omissions. **B must additionally freeze the complete
visible goal brief and actual bounded prompt before real dispatch**; a goal revision
is not permission to reread a changed live brief. No real provider integration is
claimed here.

Canonical Markdown is bounded to slightly below 1 MiB, including frontmatter,
body duplication and disposition history, with space reserved for interruption
metadata. `proposal_get` and citation freshness reads use explicit byte limits;
this is within the existing 1 MiB retrieval reader and 8 MiB SourcePreview reader.
The separate reviewed-context 256 KiB limit does not define proposal records.
The 16 MiB operational journal reserves restart projection bytes and pending source
receipts before source writes. History exceeding a bound refuses before mutation.
Exact archive export retains original canonical proposal bytes and history; it does
not export operational secrets or substitute for the retained operational journal.

Current/maintenance artifact verification is required before merge. Both artifacts
must preserve draft/disposition operations and pending projection recovery. Their
new Maestro links and Inbox plans may differ by existing feature posture. Actual
unsupported A2 refusal and maintenance→current→maintenance preservation are fixture
checks, not production enrollment or native GUI acceptance.

## A4 internal context adoption — issue181

The context-only coordinator extends the same A1 Store. Explicit fixture enrollment
first adds `required_proposal_adoption: tessera-proposal-adoption/v1` to the source
binding, then the optional `adoption_enabled` journal marker. Preceding A3 writers
refuse the new binding, including already-open source handles. Startup can finish
an interrupted enrollment. Unenrolled brains retain their previous serialized shape.
There is no service command, native control, provider dispatch or Inbox destination
in this slice. The new-operation entrypoint is compiled only for Rust fixtures;
actual backends retain recovery for already accepted transactions.

An accepted request binds the exact actor, operation/external identity, proposal
revision, goal revision and complete inspected form to one frozen create-only target.
Its base packet ID/revision must still match the selected context. Existing manual
guidance must remain verbatim in the form, and existing pins retain their exact
citation identity and source revision. Preparation never substitutes a new template.
The accepted target remains explicitly unreviewed; adoption cannot dispatch it.

Before writing the target, the Store persists the full request, original target
bytes, original pointer expectation and precomputed canonical Adopted bytes. Its
16 MiB bound reserves the additional canonical projection and all remaining receipt
and pointer metadata before target creation. Both the operation ID and external
identity participate in the existing Inbox/Attention/planning reservation namespace.
A pending adoption reserves the proposal revision against Reject/Snooze or another
adoption. Completed adoption is terminal in this internal slice.

Recovery applies only the retained source operation, records its original receipt,
then projects canonical Adopted history with the exact context ID/path/revision.
Generated fields, captured sources and earlier history remain unchanged. After that
projection receipt, it patches only `reviewed_packet_id` in the original goal's
current Application state under an expected-previous comparison. Other fields and
a newer pointer survive. A lost pointer response may recover as `already_selected`;
that outcome is recorded once and later exact replay returns it unchanged. The
receipt always identifies the originally created target, even when a newer context
selection is preserved. Goal routing is restored after each operation.

Exact committed replay precedes current source/goal/proposal freshness. Changed
payload under a retained identity refuses. A conflicting manual projection keeps
the original target and pending source operation for explicit recovery; it does
not overwrite either version, allocate a replacement, or block ordinary capture
and other goals. Proposal detail reports an unfinished adoption as pending.

Fixture verification covers every durable cut from intent through target replace,
source acknowledgement, canonical replace/receipt and pointer checkpoint; one target
survives each reopen. Separate tests cover source edits after success, competing
identities/actors, stale base input, preserved pins/guidance, newer pointer and other
goal state, explicit review separation, exact journal capacity, malformed retained
forms and preceding-writer refusal. An actual current/maintenance binary matrix is
still required before any live enrollment; these fixtures do not enable production
adoption or establish native GUI acceptance.

## A5 internal Inbox goal adoption — issue187

An Inbox-owned generated proposal can be adopted into one original goal through a
retained child in the existing Inbox-plan journal. Its parent transaction stays in
A1. The proposal remains Inbox-owned and unverified; canonical history adds an
`adopted_inbox_goal` destination with exact goal/path/revision and child operation
ID. Existing PR186 context-adoption bytes and ordinary planning behavior remain
unchanged. Enrollment and new adoption are Rust fixtures only; no public/native
command or provider transport is added.

The accepted parent freezes the complete inspected request, original capture bytes,
operator SourceIdentity, title/criteria, child operation ID, goal ID, timestamp,
origin metadata and exact create-only goal SourceWrite before any child registration.
The child uses a separate operation ID and the **same** original SourceIdentity.
Because source external identity excludes operation ID, a private delegation witness
must bind both IDs and the original request digest to the retained A1 parent. The
existing child journal records that binding; it is validated against the owned
Store for every recovery. Public planning cannot supply a witness or claim aliases
for the delegated child. Exact parent replay precedes the child's namespace check.

The child stages its intent, writes only the retained target, then durably records
its original source receipt and goal membership. That source operation is owned by
the delegated intent and stays outside shared `pending_writes`: a manual conflict
must not block unrelated capture. A goal is added to the workspace only after its
source receipt, preserving existing selection or the ordinary first-goal rule.
Parent acknowledgement retains the exact child outcome/source receipt; only then
can the canonical adopted proposal projection commit. No task/context/stage/engine
operation follows adoption.

Startup checks parent/child row identities, exact bindings and original source
requests/receipts before any proposal recovery mutation. A lost source response can
reconcile the retained operation; current Markdown alone cannot prove commitment.
A conflicting manual goal/proposal edit remains local and retains the original child
and target. A missing/malformed delegated inventory refuses before recovery writes.
The A1 16 MiB bound reserves exact future projection bytes and serialized child
outcome plus bounded receipt growth before accepting a parent.

The additional source requirement is
`required_proposal_inbox_adoption: tessera-proposal-inbox-adoption/v1`. It fences
preceding context-only writers at fresh open and on an upgraded existing handle's
cached or new write. Unenrolled optional fields remain omitted. Preserving
maintenance with new Inbox planning disabled still finishes an accepted parent,
including a parent-only crash before the child exists. It never rereads a changed
capture or allocates another target. A compiled internal fixture checks the fresh
adoption gate in both feature postures; an unsupported public command is not proof
of internal capability.

Acceptance covers durable cuts from parent intent through child/source/parent and
canonical receipt, changed captures after acceptance, source edits after committed
replay, namespace aliases, owner routing, manual conflict isolation, corrupted
bindings/receipts, preceding writer refusal and both rejected/admitted capacity.
Actual current/maintenance artifact evidence is required before integration. It
remains distinct from full release/native-form acceptance and live enrollment.

## B1 first-attempt provider generation — issue191

An explicitly fixture-enrolled brain can now consume the committed feed through a
single backend-owned worker and the saved Chat provider. There is no public
activation switch, Retry or adoption command. Ordinary brains remain unenrolled.
The worker runs independently of desktop lifetime and releases the Runner mutex
before credential resolution and HTTP. New generation is controlled by
`runtime/proposal_generation.rs::NEW_GENERATION_ENABLED`; preserving maintenance
sets this constant false while retaining journal/source recovery and inspection.

Before network access, the existing A1 attempt retains `GenerationInput`: the
versioned exact UTF-8 HTTP JSON body, saved non-secret Chat settings reference,
complete visible goal brief and exact canonical goal source. The body includes
readable original trigger and goal text; source snapshots retain original bytes.
Its digest covers the exact string passed to the shared Chat client's raw-body
entrypoint. Startup validates its model identity, source digests, owners/revisions
and equality of sent content with retained provenance. No implicit Chat system
message is added to this body. External goal/source changes make work stale;
recognized Snooze projections preserve deadline/history and can accept a valid
response. Reject wins late completion. Each attempt owns its async runtime and client; both are dropped before the
single worker takes another attempt, so local HTTP connection tasks cannot remain
suspended after Reject or output overflow. This does not claim remote cancellation.

Input accounting is the complete serialized request-body byte length, including
JSON escaping, instructions and model settings: at most 64 KiB. The existing
20-citation/48 KiB cited-text bounds also apply. Output accumulation is limited to
32 KiB in the existing SSE parser, with a 30-second header/idle deadline and a
120-second total deadline. Oversized or incomplete input never silently loses
context to fit. A preflight failure retains its original trigger/attempt and a
bounded issue in A1, visible with queued entries through the same owner-scoped
pagination as canonical drafts. A changed trigger is stale; another invalid input
is failed. These preflight entries do not claim a canonical generated record.
They do not prevent subsequent eligible inputs from progressing.

Missing Chat settings/credentials, invalid configuration, malformed response and
provider failure remain visible failures. A possibly sent running attempt becomes
interrupted after restart and is not automatically retried. A projection conflict
retains its original operation while independent proposals continue recovery.
The worker also performs this recovery when preparation/final publication fails
and no local request remains, preventing orphaned running state from occupying the
queue indefinitely. Original operational receipts remain outside indexes; exact
Markdown export preserves generated drafts and dispositions, not the operational
journal needed for uncertain recovery.

The `required_proposal_generation: tessera-proposal-generation/v1` source binding
precedes any retained generation extension. Preceding source writers refuse it,
including writers opened before enrollment. Startup rejects generation inputs or
preflight issues without this binding. Optional fields are omitted on unenrolled
brains. Every journal commit while generation is running reserves 3 MiB beyond
the existing worst-case interrupted projection/receipt image for bounded result
publication. Capacity refusal happens before provider dispatch; the retained queue
is not acknowledged away. Compatibility acceptance uses the actual full native
feature graph and a preserving dispatch-disabled artifact; source tests alone do
not establish live enrollment or native/provider acceptance.

## B2 explicit Retry — issue192

`proposal_retry` accepts the original workspace guard, actor/SourceIdentity,
operation ID, proposal/owner and exact inspected revision. It is available only
for failed/interrupted canonical proposals with complete saved provider input and
unreviewed/snoozed disposition. Preflight-only failures, stale inputs, changed
settings, active or successful attempts and terminal dispositions cannot schedule
another request. Source read/storage uncertainty remains an error with the exact
client request retained. No prompt is rebuilt and no model/reference is changed.

The existing A1 journal retains a typed Retry receipt and its canonical source
operation. Acceptance archives the complete previous attempt and failure in the
canonical `attempt_history`, assigns one new attempt ID and publishes queued state.
The archive records the time of Retry acceptance, not an invented time of remote
failure. At most 32 archived attempts are admitted, additionally constrained by
canonical and journal byte limits. Accepted retries and initial triggers share the
32-item queue and the same single worker. Queued retries reserve result/recovery
capacity before acknowledgment. The worker transitions the retained retry to
running and sends the original frozen HTTP body. Reject also interrupts queued
retries before they can dispatch; Snooze preserves its existing meaning/history.

Receipt replay returns the original accepted attempt IDs/revision before later
freshness checks. Cross-feature operation and external identities share the same
reservation namespace. An exact never-accepted request whose inspected revision,
disposition, input or eligibility has changed can instead receive a durable typed
`not_applied` reservation. That receipt cannot later become an accepted Retry and
also replays exactly. Unknown storage or missing sources are never converted into
that proof. Pending accepted projections recover their original source operation,
with conflicts local to their proposal.

The new source requirement is
`required_proposal_retry: tessera-proposal-retry/v1`, installed before retaining the
first Retry operation. Generation-only writers refuse it, including cached source
handles. Preserving maintenance disables `NEW_RETRIES_ENABLED` and
`NEW_GENERATION_ENABLED`; it still recovers and replays accepted receipts and
canonical attempt history. Queued accepted attempts remain queued without HTTP;
unresolved running attempts become interrupted and never resend automatically.
Normal unenrolled brains retain their previous serialized form.


## C public adoption forms — issue194

The guarded `proposal_adopt` command accepts a tagged `destination: context|inbox`
with the complete original coordinator request. `proposal_adopt` capability is
available only in managed workspaces already enrolled for proposal drafts. Explicit
submission enables the original destination coordinator; opening a form has no
backend mutation. No ordinary workspace is enrolled for proposal generation.

The receipt schema is `tessera-proposal-adopt/v1`, binding the exact workspace and
request to `committed_context`, `committed_inbox`, or `not_applied`. Completed and
accepted pending operations reconcile before current source/selection checks.
Unsupported maintenance creation remains disabled through its existing posture;
accepted operations and terminal receipts remain readable and recoverable.

Before enrolling a coordinator, new requests validate the destination's actual
sources, exact retained manual guidance/pins, and the full hypothetical journal
including target, canonical projection and completion reserves. A positively
observed invalid edited form or capacity limit may produce a durable `not_applied`
receipt. Such receipts reserve the shared operation/external identity namespace,
require the source fence `required_public_proposal_adoption:
tessera-proposal-adopt/v1` before their first write, and survive a later change in
eligibility. Unknown source/storage errors and uncertain accepted operations never
become terminal outcomes. A full journal unable to retain even a terminal receipt
leaves the request pending for recovery.

### Native Use draft

**Use draft** opens the existing Inbox planning form or the original goal's Context
form. It loads that goal's saved context before applying suggestions, even when
another goal was selected. Proposed content remains unverified. A dirty manual
form requires an explicit local choice; editing a suggestion keeps the complete
previous form separately, including guidance, query, scope, search mode, selected
excerpts and pins. Cancel restores that form without creating a goal or packet.

Context submission preserves saved manual guidance and exact pins, validates the
edited form before retaining it, and creates an unreviewed packet through the
original adoption coordinator. Ordinary context build/review/export and stage use
remain unavailable while the suggestion form is open. Inbox submission keeps the
existing human-confirmation criteria controls. Neither action creates external
tasks, starts an engine, verifies a criterion or completes a goal.

The native outbox retains the complete submitted request before transport. Unknown
delivery remains recoverable against that exact operation after reconnect or
restart. A typed durable `not_applied` receipt closes delivery but retains the
submitted fields for inspection and editing; it does not silently refresh proposal,
goal or base-context revision expectations. If the saved context has since become
stale, the retained submission remains editable while saving is unavailable. A
successful receipt identifies the original destination independently of the current
selected goal or newer context selection.

## Ordinary workspace suggestion controls — issue200

The `suggestions_get` and `suggestions_set` commands require the exact
`ai-brain/workspace-v1` guard. The ordinary Connections surface uses the existing
saved Chat provider/model and credential reference; inspecting settings does not
contact a provider. `available` describes local configuration/credential readiness,
not network reachability. Pause remains available without provider configuration.

`suggestions_get` returns `tessera-suggestions/v1`, with `mode`
(`disabled`, `enabled`, `paused`), monotonic settings `revision`, `enrolled`,
`queued`/`running` counts, nullable `backlog`, `can_change`, and
`provider: {available, model, message}`. Queue counts are materialized intents;
retained source-feed overflow is indicated separately by backlog.

`suggestions_set` accepts exactly `{operation_id, expected_revision, enabled}`.
Its `tessera-suggestions-outcome/v1` response binds the full original `workspace`
and `request`; `status: committed` carries a `tessera-suggestions-receipt/v1`
`receipt` and null `reason`. The receipt retains workspace, request, actor,
resulting settings revision, enabled flag and replay flag. `status: not_applied`
carries null receipt and a stable `reason` (`provider_unavailable`,
`revision_changed`, `identity_conflict`). Unsupported operations and uncertain IO
remain errors and must not retire a pending native request. A mismatched workspace
also retains the original pending operation until its original endpoint is restored.

Provider/revision refusals are durable terminal reservations under the control
fence. They do not activate the source feed, change settings revision, or write
canonical Markdown. A delayed duplicate refused Enable cannot become eligible
after configuration changes; a new explicit command requires a new operation ID.
Operation IDs share the existing Inbox, Attention, planning and proposal mutation
namespace. Identity collisions are refused from their existing immutable owner.
Exact accepted or refused replay precedes current configuration/revision checks.
The recorded actor remains immutable attribution: changing the saved Review actor
cannot turn an already-applied original request into a refusal. Changed request
content under that ID is refused.

The source binding `required_suggestions_control: tessera-suggestions-control/v1`
precedes any optional Runner control state. Old opens and cached source writers
refuse that fence. A fence-only interrupted setup is disabled until its original
explicit command is retried. Pending activation persists before the existing
preparing/active source-feed enrollment, draft capability and generation capability;
only the final durable settings receipt permits dispatch. Recovery resumes those
same retained identities after restart. The no-backfill boundary is durable feed
activation, not the initial click or response time. Resume never resets its epoch,
cursors, attempts or original source receipts. A first Pause on an unenrolled brain
retains only control state and its receipt; it does not activate a source feed.
Existing legacy fixtures retain their previous behavior until an explicit control
command, including preservation of an already-enabled baseline on a refusal.

Pause gates new worker selection/dispatch only. New committed events remain in
the existing bounded queue/backlog. An already-started attempt may finish and
publish its original unverified draft; pause does not make it stale or cancel
transport. A restart preserves uncertainty as interrupted without automatic
resend. Explicit Retry can still accept its original frozen input while paused
and stays queued until Resume; source/settings validation remains unchanged.
Inspection, disposition, adoption, existing receipt recovery and source writes
keep their existing authority while paused.

The native client retains exact pending settings requests through lost responses
and restart. It retires only a bound committed/not-applied outcome, then refreshes
`suggestions_get`: an old replayed Enable receipt must not render the current
workspace as enabled after a newer Pause. No provider/model or credentials are
copied into the control command. No task, T3/Maestro execution or completion is
implied by enabling generation or adopting a suggestion.

Preserving maintenance understands the new fence/control journal and disables
`runtime/suggestions.rs::NEW_SETTINGS_ENABLED` plus the existing generation and
creation constants. It still reconstructs activation, pending accepted operations,
terminal reservations and original receipts; it never resets to an older backup.
Ordinary unenrolled workspaces omit the new optional Runner field entirely.
