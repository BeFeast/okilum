# AI Brain POC — scope and acceptance

Approved direction and delivery foundation, 2026-09-05. This specification extends
[the product PRD](PRD.md); implementation is tracked separately. Foundation schemas
and adapter behavior are in [ai-brain/v1](ai-brain-contracts.md).

## Purpose and control boundary

The primary user is a person managing engineering, personal and household goals.
The everyday entrypoint is an inbox and attention overview. The core outcome is
thought → task → execution → knowledge without repeatedly assembling and copying
context between tools.

Okilum owns the goal and overall plan. Engines execute delegated stages and return
results. A Maestro stage owns its own workers and PR execution; Okilum chooses the
next step after its outcome or failure. This is a product ownership boundary, not
an instruction to duplicate an engine's scheduler.

Development of Okilum stays under external T3 Code control: brainstorming,
implementation steering, review, debugging and recovery. No dogfooding or daily
workflow migration at this stage. Okilum may drive a selected test goal inside
the POC; it does not manage its own development. A separate fixture brain, test
task project and thread are the proposed test setup, not already provisioned state.

## Selected vertical

1. Capture an inbox item in the actual Okilum UI and open its goal context.
2. Clarify the goal in native Okilum conversation through CLIProxyAPI, backed by
   the shared Markdown brain.
3. Create or associate a task through the Todoist connector. Todoist remains the
   authority for that task during transition.
4. Prepare a stage context: goal, current decisions, constraints, sources, previous
   result and next step. Create/open the correlated T3 thread and observe it.
5. Receive the actual outcome and evidence automatically, save them in Markdown
   with provenance and verification status, and evaluate the goal's criteria.
6. Notify the user of the final result, a blocker or a decision they must make.
   Ordinary progress belongs in history/digest.

There is no manual context copy/paste either into the thread or back into the brain.
Source editing plus the existing rendered preview is sufficient for this POC.
Live Preview remains an eventual Obsidian-replacement requirement.

The goal process must continue independently of desktop/laptop lifetime on an
always-available host. No host, model, deployment topology or time/token budget is
chosen by this contract. The runner is durable; the current cored stdio/snapshot
service is not already that runner. Reuse the existing in-process core and GPUI
reader instead of introducing a general workflow framework as a prerequisite.

## Data, autonomy and completion

Markdown and adjacent media are canonical. After eventual Todoist replacement,
durable goals/tasks/statuses/decisions/results are reconstructible from Markdown.
During transition, task projections in Markdown remain references to Todoist,
never a competing task authority. Repository-owned executable documents stay in
their repositories; context packets reference them rather than fork them.

AI may proactively save proposals/drafts and update knowledge/tasks within the
granted assignment without per-step reapproval. This does not invent authority
outside the assignment. Results save automatically with sources and verification
status; contradictions are surfaced. Independent source edits may merge;
intersecting versions must be preserved for resolution.

A stage finishing is not proof that its goal is done. Goal completion requires
the predefined outcome criteria and their evidence. Human acceptance is required
only when a criterion calls for it. A failed or unverifiable criterion stays
visible; the system must not invent a successful receipt.

## Acceptance

Choose one concrete test task and record its outcome criteria before the run.
Backend-only scripted success does not satisfy interactive acceptance.

| Scenario | Required observation |
|---|---|
| Capture and conversation | User operates real Okilum UI; inbox, goal, conversation and sources remain associated. |
| Connected task | Real Todoist identity/status are linked to the goal; local projection is not authoritative. |
| T3 handoff | Real thread receives the complete context packet; user can open that thread. |
| Return | Real correlated status/outcome/evidence returns automatically and is inspectable in saved Markdown. |
| Completion | Goal criteria are evaluated; final result/blocker/decision produces attention, not a fabricated success. |
| Editing | Source bytes/frontmatter/wikilinks are retained; source editor and rendered preview are distinct. |
| Context continuity | No manual copying of task context or results in either direction. |
| Desktop reconnect | Closing/reopening the UI leaves known work running; reconnect shows the same goal/stage/thread. |
| Runner recovery | Restart or event replay recovers existing external work without blind duplicate dispatch or duplicate receipts. |
| Missing evidence | A completed engine turn without required evidence leaves the criterion unmet. |
| Source conflict | Stale writes cannot silently overwrite a newer revision; base/current/proposed content remain recoverable. |

Record implementation, automated evidence and interactive acceptance separately.
When a check cannot be observed, report it as unverified.

## Broader requirements and sequencing

The broader first useful product requires exact portable Markdown+attachments
export preserving original bytes, and an AI-derived goal/project context packet
with summary, decisions, open questions and source references. Derived summaries
must be labeled and must not replace originals. Semantic search complements lexical
search and agent context retrieval; this does not imply constant semantic mining.
Mobile capture, attention, replies and decisions use ok-gobot; full mobile editing
is deferred. Their timing relative to this selected vertical remains open.

Future Todoist replacement must cover capture, recurrence, dates/time/reminders,
priorities, projects/sections and labels. Filters, subtasks and shared tasks are
not selected requirements. Keep Todoist initially; another free/trial provider is
a fallback only if the connector proves infeasible. The documented API supports
the connector; account-specific access and reminder plan limits require validation.

After the POC is tried, decide full scope and the sequence of Live Preview, exports,
semantic retrieval, ok-gobot, Maestro and task replacement. Do not silently require
them all to demonstrate the first vertical. Sync/history/rename/offline semantics,
runtime storage/host, model routing, and broader failure/autonomy policies remain
design decisions. Foundation contracts specify only what this vertical requires.
