# AI Brain application API (frozen POC contract)

This additive JSON-lines API uses request `{schema:"ai-brain/v1",id,op,...}`
and response `{schema,id,ok:true,data}` or
`{schema,id,ok:false,error:{code,message,conflict?}}`.
IDs in this description are values supplied programmatically, never product form fields.

| Command | Additional request fields | Data |
|---|---|---|
| capabilities | none | `{chat:bool,todoist:bool,t3:bool,source_write:bool,actor:string}` |
| source_list | none | `{sources:[{path,title}]}` (readable relative Markdown paths) |
| source_preview | `path,content_base64?:string` | `{path,revision,preview_revision,markdown,assets:[{url,content_base64,media_type}],links:[{url,target,status,candidates:[{path,title}]}]}` |
| snapshot | none | Existing Snapshot plus `conversations:[{id,path,status}]`, `task:null\|TaskView`, `pending_task_operation_id:null\|string`, `thread_url:null\|string`, `source_paths:{goal,stage,result}` (nullable relative paths) |
| chat_start | `goal_id,message,source_paths:[string],conversation_id:null\|string` | `{conversation_id,turn_id,status:"running"}` |
| chat_get | `conversation_id,context_turn_id?:UUID` | `{id,goal_id,path,status,messages:[{role,text}],partial,error:null\|string,turn_contexts,context_history,context_snapshot}` |
| task_create | `goal_id,operation_id,content` | TaskReply |
| task_link | `goal_id,task_id,picker_session_id?` | TaskReply; optional frozen Inbox selection |
| todoist_inbox_list | `goal_id,session_id?` | Cumulative bounded Inbox page; exact workspace guard required |
| task_refresh | `goal_id` | TaskReply |
| task_reconcile | `operation_id` | TaskReply |
| stage_prepare | `goal_id,conversation_id:null\|string,source_paths:[string],criterion_ids:[string],next_step` | Snapshot |
| criterion_evaluate | `result_id,criterion_id,evidence_ids:[string],status,evaluated_by,evaluated_at` | Snapshot |
| result | `result_id` | Existing ResultRecord (fixes outer request ID collision) |

TaskView is `{task_id,content,status,url}`. TaskReply is
`{status:"accepted"|"rejected"|"indeterminate",task:null|TaskView,error:null|string}`.
Task creation durably saves a provider mutation before network transmission.
The same UUID and payload reconciles that exact command; changed payload reuse fails.
A later explicit task selection supersedes an older create intent: reconciling the
older operation retains its receipt without relinking the goal.
Task observations remain subordinate to Todoist. Unknown/unavailable is not completed.

Chat status is `running|complete|interrupted|error`. Roles are `user|assistant`.
The backend owns streaming independently of the socket. Poll chat_get every second.
The first text fragment is checkpointed immediately; subsequent fragments are
coalesced at a 4 KiB threshold (up to 4,099 bytes at a UTF-8 boundary), with a
500 ms checkpoint timer even while the
provider is idle. Scheduling or storage contention may delay persistence. A process
crash can lose the uncheckpointed batch (up to 4,099 bytes); the last
saved partial remains recoverable as interrupted. Complete/error/interrupted terminal
events always checkpoint the full accumulated text, including the final batch,
before the terminal state becomes visible. Checkpoint timers do not reset the
provider's independent idle timeout.
Partial text is excluded from completed messages. Running jobs become interrupted
on backend restart and are never implicitly retried. The next explicit chat_start
may continue the conversation. A simultaneous message to a running conversation fails.
Actual selected source snapshots and full prior transcript are persisted before network.

Capability `discussion_context: true` adds bounded same-goal saved Attention replies
and the latest saved outcome automatically. Explicit manual references can belong to
another goal and remain unverified. Every new turn stores its immutable structured
context in preserved top-level canonical conversation metadata before dispatch.
`chat_get` returns at most 64 captured `turn_contexts` summaries plus the constant-sized
`context_history` unavailable fallback for all unrecorded user rows; `context_turn_id` expands one
stored snapshot without re-reading current inputs. Missing, corrupt or unsupported
history is explicitly unavailable. Request, cumulative receipt and canonical-size
limits reject before changing the conversation; complete manual input and history
are never silently truncated. See [Discussion context contract](ai-brain-discussion-context.md)
for exact fields, limits, integrity checks and older-writer preservation.

stage_prepare allocates stage/context/operation IDs, uses configured T3 target, and
freezes selected source contents and the conversation. Existing start initiates work;
the backend owns reconcile and poll afterwards. The snapshot thread_url is the actual
configured T3 web route. Engine completion alone does not verify criteria.

criterion_evaluate requires `passed|failed|unverified`, saved evidence IDs, an actual
reviewer and UTC RFC3339 time. It records explicit review of selected saved evidence
and the retained criterion definition. Human criteria use existing accept_human,
with `{criterion_id,actor,observed_at,source:{uri,revision,locator}}`.
The configured local actor comes from capabilities. A local review may reference
`brain-review://goal/<goal-id>/criterion/<criterion-id>` as its source URI.

Snapshot attention projects current criterion decisions from retained history:
completion hides the earlier unmet-criteria decision, and reopening or invalidating
completion hides its old success notice. Stage-owned attention from a predecessor
is retained in `attention_history`, not projected as a current action for its
successor. `attention_stage_ids` maps each owned attention ID to its stage ID.
The history is additive and immutable; a repeated message in another stage has
its own identity. Legacy entries without ownership are associated with the prior
stage only for recognized lifecycle messages. Unknown legacy decisions remain
current. A retained terminal result resolves known transport/engine-running
blockers; unrelated current decisions stay visible. Neither filtering nor opening
details changes a task, dispatch, result, or human acceptance receipt.
The UI uses this effective goal status to replace confirmation controls with the
completed state while keeping the saved result accessible.

Existing create_goal, source_read, source_write, goal_source, prepare_stage, start,
reconcile, poll, ingest and accept_human retain their existing structures. All
returned snapshots receive the additive application fields above.

## Explicit provider configuration

The opt-in backend accepts `--config /absolute/path/providers.json`. Configuration
is operator setup, not a field in the product goal/chat flow. Example (all values
are placeholders, and credentials are environment references only):

```json
{
  "actor": "Oleg",
  "chat": {
    "base_url": "http://127.0.0.1:8317/v1",
    "model": "configured-model",
    "api_key_env": "TESSERA_CHAT_KEY"
  },
  "todoist": {
    "base_url": "https://api.todoist.com/api/v1/",
    "instance_id": "personal-todoist",
    "token_env": "TESSERA_TODOIST_TOKEN"
  },
  "t3": {
    "base_url": "http://127.0.0.1:3773",
    "token_env": "TESSERA_T3_SESSION_TOKEN",
    "environment_id": "configured-environment",
    "project_id": "configured-project",
    "model_instance_id": "configured-provider",
    "model": "configured-model",
    "runtime_mode": "approval-required",
    "interaction_mode": "default"
  }
}
```

A provider may be null. Configuration does not pair accounts, install services,
request a live model response, or issue tasks. Runtime credentials are never
serialized. Stable non-secret provider identity is retained to prevent silently
rerouting unfinished work to another account/project. The existing `--managed-brain`
cooperating-writer declaration is still required for direct source writes.

The runner journal stores frozen chat requests, partial text, Todoist commands and
receipts, alongside pending canonical Markdown projections. It uses the same process
lock and source journal as goals/stages/results. Operational storage must remain
outside the brain and disposable index. Markdown conversation/result/context bodies
are readable without interpreting their typed YAML metadata.

## Native source preview

`source_preview` takes an existing root-relative source path and optional unsaved
UTF-8 draft bytes as base64. `revision` identifies the unchanged canonical source;
`preview_revision` identifies the rendered draft. Preprocessing and note resolution
reuse the existing core reader, preserving callout syntax and code literal boundaries.
Resolved links use `tessera://open/`, ambiguous links `tessera://ambiguous/`;
metadata also identifies unresolved targets without guessing a candidate.

Local images are delivered as bytes under opaque `tessera-asset://<sha256>` URLs.
A remote GUI never receives a backend `file://` path to read. Relative image paths
are normalized within the brain before SourceStore traversal; escapes and symlink
traversal return an unavailable image reference. HTTP(S) references remain references;
the backend does not fetch remote images. Preview never writes canonical source.

POC preview budgets are 8 MiB of source/draft UTF-8, 8 MiB per local image and
32 MiB of unique image bytes per reply. Oversize sources fail explicitly; unavailable
or oversize images have an opaque unavailable reference. These reads are bounded
on the opened SourceStore file descriptor, including concurrent file growth. The
source editor's exact read/write contract is unchanged. Each preview builds a
metadata-only resolver; the first POC uses the small isolated brain fixture, with
debounced asynchronous requests from the GUI. Large-vault incremental indexing is
a later performance step, not claimed by this preview implementation.

### T3 terminal response evidence

A dedicated T3 thread is correlated through the full snapshot's single exact user
message ID and text, then the server-assigned user `createdAt` and matching turn
`requestedAt`. The earlier client command timestamp is not the server acceptance
time. Before a turn is assigned, `thread_url` can already open the persisted
submitted attempt; this URL does not claim that execution or correlation succeeded.

A completed turn must expose its declared final assistant message with the same
turn ID, nonempty text and `streaming: false` before Tessera records an outcome.
An available checkpoint/diff is retained. An absent checkpoint is explicit
`engine_checkpoint_unavailable` evidence with `unverified` status: non-git planning
projects may never create one. The first terminal receipt freezes only the evidence
available then. A later checkpoint does not rewrite that receipt; future evidence
enrichment requires a separate event. Goals requiring file/diff verification stay
unmet until their actual criteria have evidence. Engine success alone never passes
criteria or supplies human acceptance.

## Source conflict inspection and explicit resolution

`source_write` accepts an optional top-level `base` containing the exact loaded
`SourceSnapshot`. Existing clients may omit it. The source store validates the
base's brain/path/schema and bytes against `expected_revision` before retaining
it with the operation. A stale revision still returns a structured conflict;
independent edits are not automatically merged in this POC.

Read `source_conflict` with `{brain_id,path,conflict_id}` to inspect the retained
`base` (nullable), `proposed` SourceSnapshot and latest `current` SourceSnapshot
(nullable if deleted), plus the original structured `conflict`. This is read-only
and validates the requested brain/path/operation identity. The durable operation
retains the bytes observed when the conflict occurred even if current changes
again. Missing base never fabricates history.

The native editor retains its draft, displays base/current read-only, and offers
**Save resolved draft**. That deliberate action submits the edited draft with the
displayed current snapshot/revision and a new operation ID. A further concurrent
write causes another visible conflict; it does not silently overwrite the newer
source. Identical retries after a lost acknowledgement retain their frozen
operation ID, including during resolution. Original conflicts remain inspectable
through the API after resolution or backend restart. The current UI does not list
historical conflict IDs after reopening: in-session inspection/resolution is the
POC UI scope, and unsaved draft restoration across desktop restart is not claimed.
Deleted/non-UTF-8 current
sources remain preserved conflicts; this text editor does not silently recreate
or normalize them. The existing Reader renderer previews the resolution draft.

Automatic independent-edit merging remains a later vision requirement. This POC
provides explicit revision-guarded resolution and does not claim automatic merge.

## Alpha goal ownership and sequential stages

`create_goal` accepts multiple unique goals and returns the created goal's snapshot.
`snapshot` adds `goals`, `stages`, `selected_source_paths` and
`selected_conversation_id`. Goal-owned requests carry `goal_id`; it is optional on
`snapshot`, `goal_source`, `start`, `reconcile`, `poll`, `criterion_evaluate` and
`accept_human` for legacy clients. Omitting it always addresses the original
primary goal, never the most recently selected desktop goal. Selection is a read
of `snapshot` with that goal ID; it invokes no adapter and changes no journal.
`goal_selection` explicitly saves `{goal_id, source_paths, conversation_id}` for
revisiting that goal. Sources must exist and the conversation must belong to it.

Task operation reconciliation routes by its retained operation ID; chat reads and
stream callbacks route by the conversation's immutable goal owner. The backend
continues/reconciles all started goals independently of desktop selection. It never
implicitly starts a prepared goal. A task operation UUID cannot be reused for a
different goal. Late provider receipts cannot relink another goal's task.

`stage_prepare` accepts optional `previous_result_id`. Preparing a follow-up is an
explicit action naming the latest retained result; its predecessor must have a
terminal outcome. The new context freezes that result and preserves predecessor
context, dispatch, result and evidence. At most one stage is active per goal.
New stages require new IDs and never overwrite predecessor records. Existing human
acceptance remains historical and does not automatically accept the new result.
Late predecessor events are retained as history, never projected onto the new
stage. Duplicate terminal outcomes do not create additional result receipts.

Persistence is an additive adaptation: the original POC goal/application/dispatch
fields remain at their original JSON locations. `primary_goal_id` anchors that
legacy default; `other_goals` stores independent journals with the same shape,
and `previous_stages` retains earlier dispatch/verification snapshots. Every
journal write normalizes the original goal back to the top-level slot, including
writes issued while another goal is being handled. No canonical Markdown or
accepted evidence is migrated or rewritten merely on opening. Missing operational
state with owned goals remains an explicit recovery error. Opening never dispatches
work; the backend driver only reconciles already-started durable operations.

Native goal selection preserves per-goal source/conversation selections and
in-session compose/next-step drafts. An unsaved source draft blocks goal changes
until the user saves or discards it. Reopening can navigate all persisted goals;
restoring unsaved desktop drafts after a crash remains outside this alpha slice.

## Workspace attention projection

Capability `workspace_attention: true` advertises the additive read-only
`workspace_attention` operation. It uses the normal workspace identity guard and
returns `{schema, workspace, observed_at, goal_count, running_goal_count, items}`.
Each item contains `attention_id`, `goal_id`, `goal_title`, `goal_status`, `kind`,
`message`, nullable `stage_id`, nullable `current_stage_id` and nullable `result_id`.
`stage_id` is the recorded attention owner; legacy or goal-wide attention without
an owner stays `null`. `current_stage_id` identifies the stage current at observation.
`result_id` is the latest result of that stage only when its ID matches the recorded
owner. These fields never infer ownership from the desktop's selected goal.

The projection uses the same current-attention filtering as a selected-goal
application snapshot. Superseded stages, resolved runtime blockers and stale
criterion transition messages remain in per-goal history, not this list. Completed
goal results and unresolved decisions retain distinct kinds (`final`, `decision`,
`blocker`). No acknowledgement, provider polling, task refresh, dispatch, selection
write or journal mutation occurs. Failure to read any goal fails the operation;
a partial list must not masquerade as an empty or complete workspace overview.

`observed_at` is UTC RFC3339 time when this backend projection was assembled under
the service owner mutex. It is not a provider refresh or source freshness claim.
`running_goal_count` counts durable `running`, `submitting` and `indeterminate`
phases, including goals other than the selected one; it is not a provider health
check. Clients retain the observation time and show failed refreshes as stale or
unavailable. Old backends without the capability are unsupported, not empty.

A click carries the observed goal/attention/stage identity. Clients re-read that
goal and verify ownership before displaying current actions; a superseded item
can open its retained history/result, but must not be retargeted to the successor
stage. Late responses are ignored after a newer request, selection or workspace
change. Workspace refresh itself never selects a goal or discards an editor draft.

## Indexed retrieval and reviewed context (milestone #113)

The additive contract and acceptance boundaries are in
[indexed retrieval and reviewed context](ai-brain-retrieval-context.md).
These operations retain the `ai-brain/v1` envelope above.

| Command | Additional request fields | Data |
|---|---|---|
| brain_index_status | none | index status, generation, freshness and model metadata |
| brain_index_rebuild | none | explicit rebuild/index status |
| brain_search | `query,mode,scope:{goal_id,mode?,path_prefix?,include_paths?,exclude_paths?},limit?,max_excerpt_bytes?` | `{hits:[Citation],index:{generation,observed_at,status,model}}` |
| context_prepare | `goal_id,query,scope:SearchScope,citations:[Citation],pinned_citation_ids?:[string]` | `{packet:ReviewedPacket}` |
| context_revise | `goal_id,packet_id,expected_revision,text` | `{packet:ReviewedPacket}` |
| context_get | `goal_id,packet_id?` | `{packet:ReviewedPacket|null,export_job?:ExportJob|null}` |
| context_export_start | `goal_id,packet_id,packet_revision` | `{job_id,status}` |
| context_export_get | `goal_id,job_id` | `{job_id,status,markdown?,error?}` |
| context_export_cancel | `goal_id,job_id` | export job cancellation/status |
| context_export_prepare | `goal_id,job_id` | existing `DownloadReady` shape, then existing download chunks/release |

Top-level `mode` is `lexical|semantic|hybrid`. `scope.goal_id` is the required
target goal. `scope.mode` is `project|goal`, defaulting to `project`: the selected
brain's saved knowledge, optionally restricted by `path_prefix`. This does not
create a new project identity. Include/exclude paths
are brain-relative Markdown paths; `path_prefix` is a relative directory scope.
Exclusion wins. Goal-only scope rejects other-goal owned records even if explicitly
included. Explicit brain/project scope may reuse other goals' saved outcomes and
decisions with owner/status/date metadata; raw operational stage/session records
remain excluded. Referenced knowledge never transfers target-goal ownership. `limit` is at most 20. `max_excerpt_bytes` defaults to 4096, with an
8192 hard maximum; excerpt boundaries are exact whole source lines, not generated
text. Search never selects sources implicitly. The native selected citation set
persists across queries until the user changes it; excluded citations are not
silently re-added to the packet by a later search.

`Citation` contains `citation_id,path,revision,locator,start_line,end_line,excerpt`,
optional `lexical_score,semantic_score`, `rank` and metadata
`{record_type?,owner_goal_id?,status?,verification?,observed_at?}`. `locator` is a string and line
numbers are one-based. Model metadata identifies the actual embedding model and
immutable digest/dimensions. Search exposes lexical-only degradation explicitly.

`ReviewedPacket` contains `id,revision,goal_id,goal_revision,query,scope,text,citations,
created_at,reviewed,reviewed_content_sha256` and optional `pinned_citation_ids`.
`text` is editable task guidance. Citation excerpts remain separate immutable
originals. The UI displays both, and both enter the consumer payload. Pin IDs must
be a subset of selected citation IDs and do not bypass scope or revision guards.

`context_prepare` persists the full scope and validates its target goal, path
restrictions and allowed knowledge types. It rereads sources and validates exact revision,
locator/line excerpt and derived citation ID. It creates an unreviewed packet.
`context_revise` checks `expected_revision`, revalidates sources and records the
explicit user review with `reviewed=true` and a hash of guidance plus citations.
Review is durable, not merely an enabled client button. Source edits invalidate
approval; edited guidance requires a new revision/review checkpoint.

`stage_prepare` adds optional `reviewed_packet:{id,revision}`. This path requires
a reviewed packet belonging to the goal, an exact packet revision and fresh
source revisions. It consumes that packet without hidden full source snapshots or
unreviewed conversation text. The dispatch carries guidance once in `next_step`,
exact citations once in `extra.source_excerpts`, and only `{id,revision}` in
`extra.reviewed_packet`. Omitting the reference retains the older selected-full-
source behavior only for a goal with no saved reviewed-context packet. Once a
packet exists, omission is rejected rather than silently reverting to that path.
Existing stage guards and explicit Start remain required. Start revalidates the
packet; later source changes do not block polling/reconciliation of already
accepted work, whose original context stays frozen. Generic stage guidance edits
cannot replace a reviewed packet: discard, review and prepare its new revision.
A reviewed guidance-only packet may prepare T3 without citations or a task binding;
it explicitly records that no source evidence was selected.

AI export requires the same reviewed identity/revision and freshness checks, and
at least one selected citation. Guidance-only export fails with an explanation.
Its job status is `running|complete|interrupted|error|stale`. It consumes the packet
without new retrieval and validates emitted citation references against the frozen
citation set. The generated Markdown and provenance are available through the
existing bounded download machinery. Provider failure does not mutate a canonical
source or dispatch a T3 stage. Exact archive export remains independent.

Export jobs are durable under operational `context-export-jobs/`. `context_get`
restores the latest job matching the target goal and packet identity. Backend
restart changes a running job to `interrupted`; no provider request is replayed.
Cancellation interrupts only its job. Generation runs outside the backend/Runner
mutex. A transport completion is not authority: only a validated generated package
and renewed source/packet checks make a job `complete`. Download preparation checks
freshness again. Job IDs and ephemeral download `export_id` values are distinct.

AI archives contain the reviewed guidance, generated Markdown and exact selected
excerpts, with original paths, source revisions, line ranges and excerpt hashes.
They omit unselected portions of the original notes, even when two selected
excerpts share a note. Exact workspace export remains the separate full Markdown
and attachment archive. Failed/cancelled new jobs do not overwrite retained older
completed packages; no partial result is advertised as a downloadable package.

The backend reads nonsecret embedding settings from operational
`retrieval-settings.json`: `{base_url,model,digest,dimensions}`. The selected model
is Ollama `qwen3-embedding:0.6b`, digest
`ac6da0dfba84a81fdbfbaf330198c33cd77c4cdfc53e8bc50eb581914a15621d`,
with 1024 dimensions. This is operator setup, not per-task input. The backend
checks the provider model digest before and after each embedding request and
uses `num_ctx=16384` with `truncate=false` to cover the accepted whole-line byte
budget, and validates finite, normalized vectors. Embedding batches contain at
most eight passages and 8192 combined UTF-8 bytes, with a 180-second request
deadline; a whole accepted passage is never truncated to fit a batch. Context size is part of the
cache model identity. A failed batch retains already valid vectors under that
identity while explicitly reporting incomplete semantic availability. Disposable cache generations have content
checksums and vector-shape checks; invalid caches are rebuilt from Markdown.

The brain lexical baseline quotes Unicode word tokens and joins them with OR for
natural-language questions. The ordinary Reader query parser is unchanged.
Hybrid ranking sums reciprocal one-based ranks (`k=0`) across available methods,
with a stable citation ID tie-break. Raw lexical and cosine scores remain separate.
This replaces initial equal RRF60, whose near-uniform contributions promoted weak
dual-method tail matches above strong semantic-only evidence in the small corpus.
Independent receipts retain the original failed run and informed rerun; no query
weights, threshold changes or parameter search were used.


## Canonical inbox (issue #124)

`inbox_read` advertises provider-independent `inbox_list` and `inbox_get`.
`inbox_capture` advertises writes only when the selected brain is managed and its
original operational receipts are available. All three operations are workspace
operations: they retain selected goal, provider, conversation and dispatch state.
They use the existing envelope and `expected_workspace` identity guard.

The [inbox implementation contract](ai-brain-inbox-implementation.md) freezes the
complete request/response shapes, field limits, error codes and pagination.
Capture accepts exact original text and a structured native source identity, with
actor matching `capabilities.actor`. A durable operation UUID and original source
identity must survive client retries/restarts. The native API rejects Telegram
source identity. The separate scoped connector entrypoint below provides trusted
Telegram provenance only when explicitly configured.

A committed receipt reports the original Markdown identity, revision and received
time. Repeated identical requests return that receipt even if the canonical note
was subsequently edited/deleted; replay never rewrites it. Current bytes come from
get/read. `inbox_get` returns `{item, text, source}`: `text` is the exact original
body decoded by the canonical backend parser, preserving CRLF, whitespace and a
missing final newline; `source` remains the complete Markdown `SourceSnapshot`.
Native reading can display the thought directly and expose full Markdown on request. Different content under either operation or upstream update identity
returns `inbox_identity_conflict`. An unknown response retains the client's draft
and operation; it is not permission to submit with a new identity.

Records use `records/inbox-<UUID>.md` (respecting configured records_dir). Original
body bytes remain unchanged, including CRLF and absent final newline. Canonical
inbox records are listable after restoring files without receipts, but capture is
then disabled with an explicit recovery boundary. Operational receipts, aliases
and pending source writes remain outside the derived index. Exact export drains
pending projections and includes each source once. The index schema advances to
v3 for strict inbox classification; inbox items cannot become retrieval citations.

This changes operational state. The #122 fallback does not preserve inbox receipts
and must not be used after enabling inbox writes. Live rollout requires a verified
compatible fallback or explicit tested roll-forward recovery, plus native inbox
acceptance. Source rewinds are not a recovery strategy.

## Mobile capture and attention implementation status

[The P1 design contract](ai-brain-mobile-capture-attention.md) defines goal-independent
Markdown inbox capture, durable upstream idempotency, exact attention replies and
LLM-independent ok-gobot controls. Canonical inbox (#124) and bound attention
(#125) operations are implemented, as described above and below. The scoped
connector transport (#134) is opt-in and documented below. The ok-gobot client
and real-phone acceptance remain separate tracked work; backend and synthetic
transport tests do not establish phone acceptance. Existing workspace attention
remains read-only.


## Bound attention decisions and seen state (issue #125)

The [bound attention contract](ai-brain-attention-implementation.md) defines the
complete `attention_list`, `attention_get`, `attention_reply` and `attention_ack`
JSON shapes, field limits, ownership rules, error codes and recovery boundaries.
Capabilities `attention_read`, `attention_reply` and `attention_ack` advertise
availability; ordinary capability queries remain pure.

New reads retain exact observed revisions in operational history and return a
stable delivery observation cursor. Existing `workspace_attention` stays read-only.
Item revisions bind exact goal/stage/result sources and allowed actions, preserve
null stage ownership, and exclude volatile time or channel seen state. Historical
get returns only retained observed snapshots with no actions when superseded.

Reply writes one create-only, unverified, goal-owned decision with the original
text and exact observed target. Acknowledgement marks only the configured actor,
source channel and item revision seen. Neither endpoint accepts criteria, starts
an engine, completes tasks/goals, resolves blockers or hides workflow attention.
Operation/upstream identities are reserved across capture, reply and acknowledgement;
a duplicate committed request returns its original receipt before rechecking
current authority. `attention_stale` means a new request did not commit and carries
`current` (the fresh item or null); clients retain the original text/target and
require explicit review before submitting a new operation against a new revision.

The durable attention enrollment marker distinguishes a first upgrade from missing
acknowledgement/observation history without a Markdown projection. Recovery disables
new attention operations/captures while preserving existing workspace/source reads
and already retained committed receipt replay. Malformed markers or unreadable
journals fail closed. No real Telegram support is advertised by these native-only
operations. Live #125 adoption requires a preserving fallback covering its history,
seen state, intents and pending decision source projections; the #124 fallback
alone is insufficient.


## Scoped trusted Telegram connector (issue #134)

The [connector wire and configuration contract](ai-brain-connector-implementation.md)
defines a separate `ai-brain/connector-v1` JSON-lines listener enabled only by the
operational `--connector-config` file. It binds loopback and shares the native
backend's owner mutex. An operator-managed fixed tunnel is a separate deployment
step; this source change provisions neither credentials nor tunnel nor bot runtime.

Each request authenticates a configured connector, sender, exact brain and allowed
chat/topic route before any operation. The server owns actor, account, source
instance and `telegram` channel. Request bodies cannot override source authority;
missing credentials never fall through to the privileged native API. The allowlist
contains only capabilities, inbox capture/list/get and bound attention list/get/
reply/ack. Configuring this listener does not relax native source validation.

Capture and attention mutations reuse the existing journal/SourceStore transaction,
replay and recovery logic. Attention observations and seen state use the Telegram
channel; native seen state remains independent. Stage ownership remains explicitly
nullable. Engine control, arbitrary source writes, criterion acceptance and task/
goal completion remain outside connector scope. Isolated listener tests verify
permitted capture/reply and rejection without state changes; live-phone and bot
release acceptance are not claimed by these tests.

## Linked Maestro work

The optional GET-only Maestro connector exposes `maestro_discover`, `maestro_link`,
`maestro_get` and `maestro_unlink`; see the exact [observation contract](ai-brain-maestro-observation.md).
Linking is a local goal association with existing external work, not a stage start.
It preserves existing T3 execution and never completes criteria or controls workers.

## Guarded Maestro approval decisions

The additive `maestro_control` capability exposes fresh, ephemeral
`maestro_approval_review` with `goal_id`, `expected_link_id` and `approval_id`.
`maestro_approval_send` additionally indicates that this build permits sends;
provider capability and exact linked review still gate each request.
`maestro_approval_decision` captures `operation_id`, `goal_id`,
`expected_link_id`, configured `instance`, exact `review`, `decision`
(`approved` or `rejected`), `actor` and `reason`. It persists the request before
one guarded POST and never retries automatically. `maestro_approval_reconcile`
accepts `goal_id` and `operation_id` and uses GET only, including after unlink.

These commands reuse the existing operation disposition and native outbox.
Pending means possibly sent. Only an exact original provider receipt commits a
decision; generic abandonment cannot resolve uncertainty. An authoritative
`rejection.never_sent: true` tombstone may retire an absent request before the
send boundary. Original receipt actor/reason and execution status remain separate:
a recorded decision never implies successful execution or goal completion.
See [guarded approval control](ai-brain-maestro-control.md) for exact scope,
enrollment, maintenance and downgrade behavior.

## Existing Todoist Inbox task selection

[Todoist Inbox picker](ai-brain-todoist-inbox-picker.md) defines the additive
`todoist_inbox_picker` capability, explicit bounded pagination, frozen-selection
linking, transient session ownership and outside-lock provider reads. Known-ID
linking remains compatible. Task views additionally expose the persisted
`provider`, `instance_id` and `goal_id`; Todoist remains task authority.

## Save a Discussion answer as an ordinary note

The native [Discussion note flow](ai-brain-discussion-notes.md) reuses `chat_get`,
an actual canonical `source_read`, create-only `source_write` and current-source
readback. It adds no backend operation or operational record type. The note is
ordinary goal-owned Markdown with captured source provenance and unverified
assistant attribution. Existing exact write replay and revision checks apply;
no Context preparation, proposal adoption or provider mutation is implicit.

**Sources → New note** uses the same create-only `source_write` and validated
readback without `chat_get` or an origin read. Its target is the current goal,
workspace and endpoint, independent of conversation selection. The required title
and optional exact body produce ordinary Markdown with only `type: Note` and
`goal_id` frontmatter; an empty body is valid and receives no placeholder or
assistant provenance. It retains the same immutable request, collision and
navigation/close recovery semantics. It adds no backend API or draft store.

## Explicit future T3 target selection

`t3_target_get`, `t3_target_prepare` and `t3_target_adopt` require the exact
`ai-brain/workspace-v1` workspace guard. The [generation contract](ai-brain-t3-target-generations.md)
defines typed review guards, request-bound committed/not-applied outcomes,
immutable history and client retry semantics. These operations do not submit
provider work; a new stage and ordinary explicit Start remain separate actions.
