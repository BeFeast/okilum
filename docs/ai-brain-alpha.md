# AI Brain alpha — one project, ordinary Tessera

Approved direction, 2026-09-05. This is the next bounded scope under the
[product PRD](PRD.md), following the [accepted POC](https://git.oklabs.uk/BeFeast/tessera/issues/61#issuecomment-11336).
Implementation is tracked in [alpha epic #79](https://git.oklabs.uk/BeFeast/tessera/issues/79).
This describes required outcomes, not implemented capabilities or an installation.

## Outcome and boundary

Open a separate copy of one project's Markdown brain from ordinary Tessera, browse
and edit its notes, and carry multiple thoughts through linked goals, conversation,
Todoist tasks, T3 stages and saved results. Reopening uses the saved workspace and
connections; evaluating the product no longer depends on a one-off POC launcher.
Export returns a portable, exact copy of that brain.

Development, brainstorming, execution supervision, review and recovery remain in
external T3 Code. The alpha may execute isolated acceptance goals. It does not take
over Tessera development, the original project or daily work. The selected fixture
is a small engineering-only copy of halenote materials, not its live repository or
Oleg's health records. This planning scope does not provision or migrate anything.

## User-visible acceptance

| Slice | Observable completion |
|---|---|
| One project workspace | Ordinary Tessera can open and return to the selected brain with an identifiable root and project label. Note navigation and links lead to that brain's files; unavailable or ambiguous targets remain explicit. Existing read-only vault use stays available. No test-specific launcher or operator-supplied goal IDs are needed. |
| Reusable inbox and goals | Capture two thoughts, select either goal and return to its own sources, conversation, task, criteria, stages and results. Switching and reopening cannot exchange or discard their identities. An explicit follow-up stage preserves its predecessor's result; at most one stage runs per goal. This is not a general scheduler. |
| Source editing and preview | Open exact Markdown, edit and save with the existing rendered preview. Frontmatter, wikilinks and unchanged bytes remain intact. Switching notes/goals must not silently drop a dirty draft: retain it or require an explicit save/discard choice. Writes remain limited to the selected writable brain. |
| Source conflicts | A second managed writer's independent or overlapping edit produces an inspectable conflict with retained draft/current and base when available. Resolve deliberately against the displayed current revision; a further change creates another conflict. Preview uses the existing Reader. No silent overwrite or invented base history. |
| Saved connector settings | CLIProxyAPI, Todoist and T3 have persistent configuration reachable from the ordinary UI, clear connection/authentication state and a concrete recovery action. Reopen retains non-secret settings and uses the configured credential reference. Missing/expired auth does not become an empty inbox, a completed task or a repeated provider command. |
| Continuous execution | Closing the desktop leaves backend-owned T3 work running. Reconnect/backend recovery returns the same goal, stage, thread and result without duplicate starts or receipts. Changing account/target settings cannot silently reroute unfinished work. Interrupted chat is shown honestly and is not implicitly resent. |
| Understandable goal flow | Capture → discuss → task → stage → result is navigable with original sources and the actual T3 thread accessible. Current blockers, required decisions and final outcomes are readable; routine progress and technical/historical evidence open in details. Engine success and verified goal completion remain distinct; a completed goal does not ask for the same human acceptance again. |
| Exact export | Export the selected brain's canonical Markdown and local attachments with original bytes and relative paths, plus a file/revision manifest. Extract into a fresh directory and inspect notes/media independently of Tessera's internal database. Missing/unreadable or concurrently changed files cause an explicit failure/retry instead of a false complete archive. |

Todoist remains task authority. A locally accepted planning artifact does not close
an actual errand. Criteria that require human review retain the real review actor,
revision and time; no automated acceptance stands in for that action.

The export contains canonical saved files, including goal/conversation/result
records. It excludes credentials and disposable indexes. Unsaved drafts and durable
execution journals are not represented as a portable knowledge archive: export is
not a running-process backup. Opening the extracted copy must not dispatch external
work or claim restored connector sessions. Links outside the selected root remain
external references; the manifest identifies unresolved local dependencies rather
than importing unrelated files. Archive packaging details remain an implementation
choice subject to these checks.

## Accepted baseline and deliberate limits

The POC proved the real task/T3 round trip, human review, desktop absence during an
active turn and native manual conflict resolution. See its
[final receipt](https://git.oklabs.uk/BeFeast/tessera/issues/61#issuecomment-11336).
[Source conflict issue #77](https://git.oklabs.uk/BeFeast/tessera/issues/77) and
[the application contract](ai-brain-application-api.md#source-conflict-inspection-and-explicit-resolution)
explicitly distinguish preserved versions and manual resolution from automatic
independent-edit merge. The alpha integrates that verified behavior; automatic
merge remains a vision requirement for a later slice. Historical conflicts remain
recoverable through the API; listing/restoring editor drafts after desktop restart
is also deferred. Dirty-draft navigation protection is required above, not a claim
of crash recovery.

Live Preview, AI-derived context export, semantic/vector retrieval, ok-gobot,
Maestro stages, Todoist replacement and full Obsidian migration are deferred.
Existing rendering/search work continues in its own issues; alpha does not quietly
promise every Obsidian feature. Multi-project orchestration, general sync/CRDT,
rename tracking, parallel-stage scheduling and new platforms are outside this
scope. Existing quality/performance gates remain binding.

## Delivery sequence

Each row is a small behavior issue with its own evidence; the final row joins them.
Do not use this document to mark implementation or live acceptance complete.

1. [Ordinary brain workspace/profile and note navigation (#80)](https://git.oklabs.uk/BeFeast/tessera/issues/80).
2. [Multiple goals and retained sequential stages, with inbox selection/recovery (#81)](https://git.oklabs.uk/BeFeast/tessera/issues/81).
3. [Source editor, preview and safe in-session conflict resolution in that workspace (#82)](https://git.oklabs.uk/BeFeast/tessera/issues/82).
4. [Persistent connector settings, auth state and reconnect behavior (#83)](https://git.oklabs.uk/BeFeast/tessera/issues/83).
5. [Integrated goal flow, current attention and expandable evidence/history details (#84)](https://git.oklabs.uk/BeFeast/tessera/issues/84).
6. [Exact Markdown and attachment export with independent extraction checks (#85)](https://git.oklabs.uk/BeFeast/tessera/issues/85).
7. [Joined halenote acceptance through ordinary Tessera (#86)](https://git.oklabs.uk/BeFeast/tessera/issues/86).

The workflow slice depends on goal identity and connector settings; source editing
and export build on the workspace boundary. The joined run requires all slices.
Implementation should extend existing source/reader, brain application, adapters and
runner contracts. Current code launches Reader and BrainView separately and owns a
single goal/task/stage per runner, so integration and multiple-goal ownership are
real work, not UI labels. The persistence layout, additive API/version changes and
normal backend connection/install path need a reviewed design in their owning
issues. Preserve existing POC record/journal identities and accepted results: no
silent reset, migration or redispatch. Any legacy-state migration needs an explicit
compatibility design. This scope does not preselect a new runtime or workflow framework.

## Halenote acceptance and evidence

Use a fresh, identified copy of engineering documentation: halenote's README,
ios/README and the dated 2026-09-03 iOS launch-crash handover. This is a historical
snapshot, not current halenote operational authority. Record selected source paths,
revisions and byte hashes when preparing the copy. Add a synthetic local
attachment and acceptance note for export/link coverage; keep expected results
outside the engine context. Do not copy clinical data, environment files, crash
logs or the full vault. No source file is edited in place.

One goal prepares an iOS rebuild/launch-readiness checklist grounded in those
sources; a second exercises goal separation and a deliberate follow-up. Distinguish
prepared advice from Mac/operator-dependent build, install and launch verification.
No actual halenote build/deploy or health API access is implied. The run includes
safe source switching, both conflict cases, auth/reconnect recovery and exact export.

Record build identity, fixture manifest, source revisions, goal/task/thread/result
identities and screenshots of the actual requested surfaces. Distinguish automated
regressions, agent-operated native checks and Oleg's observations. Record unchanged
original-source hashes and archive extraction verification. Prepare any concrete
install/provider mutation and cleanup plan separately before its live approval.
