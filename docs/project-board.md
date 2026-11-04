# Native project Board (#1767, #1859)

The optional [delivery projection](board-delivery.md) (#1768) joins explicit
platform/channel feeds, exact source/build inclusion and build-scoped scenario /
human evidence. It adds a compact summary without restoring the removed stage
header, and never publishes, installs or accepts a build.

The optional [project-note context](board-context.md) (#1769) adds explicit
attention, decisions and exact-build try notes, plus collapsed next-stage context.
Current stage outcome/scope stays in linked notes, preserving #1795.

The accepted Sprint Board UI and shared model now use the compatible Area
manifest readers. Its live status remains a projection; explicit in-app Board
authoring is described below. Editing a card never changes its Forgejo issue or
starts executor work.

## Live repository projection (#2161)

A repository-bound Board now discovers all open issues from that exact Forgejo
repository. Manual `issues` entries annotate titles/outcomes, agents and related
notes; they no longer determine membership. `attention` remains human-owned.
An optional `pinned = true` keeps a manual card after closure; explicit Ready /
Verified stages also retain their existing human-owned semantics. No manifest is
rewritten by discovery. Local Boards and local card persistence are unchanged.

On entry and every at most 30 seconds while visible, the existing background read
loads open issues and open PRs, and scans recently updated closed PRs for merges
in the last 24 hours. PR links come from current references or explicit card
links. Open PRs with pending/failed/unknown aggregate checks enter Checks; successful
checks retain In progress. Closing an issue removes its unpinned card on the next
successful read. A recent merge retains its issue and PR link for 24 hours. Can try
still requires the existing exact-source inclusion, available artifact and scenario
evidence; merge alone never claims delivery or acceptance.

Reads reuse the Board's explicitly bound public Forgejo repository client. There
is no separate tracker token in the existing desktop Board; Manager OAuth belongs
to its T3 origin and is never forwarded to Forgejo. No host or token is inferred
from the developer's environment. Reads reject redirects, share a 20-second
deadline and cap responses at 2 MiB and each list at 1000 entries. A Board-owned
ETag cache (at most 8 MiB of response payloads / 256 entries) revalidates responses on subsequent reads and survives hide/show;
changing repository discards cached responses and server backoff. HTTP 429/503
honours Retry-After (60 seconds when absent), retaining the last complete Board.
Refresh is clamped to 15–30 seconds to avoid fast-poll configurations flooding the
API while meeting the one-minute closure target on a healthy source. A failed/incomplete read retains
the last complete snapshot and reports its source error in one quiet line with
the last-success time; full details remain in the existing source disclosure.
Merge expiry also freezes at that last-success time during failure, so an offline
Board cannot silently empty itself. Generated cards identify missing manual annotations in the issue panel. Invalid or duplicate annotations stay visible and never supply
an arbitrary owner. Delivery refresh follows
the issue read so newly discovered PRs reach the existing delivery verifier in the
same refresh cycle. The earlier selected-scope descriptions below document the
historical Board slices; this live membership rule supersedes them.

Entering a project opens a native Board in its existing center pane. Notes and
Board share that center. Returning with **Board** retains the filters and search,
selected card and scroll. The existing right-hand Manager and its draft stay in
place. Note opens still use the Reader's ordinary revision/save/refusal path;
only a successfully accepted Markdown document hides the Board.

The stage is an explicit selection, independent of whether issues are open or
closed. An issue card shows its expected result and executor. Its right-hand issue
panel opens explicit issue/PR/build links or related area notes. Escape closes
the issue panel. The horizontal lane scroll is local to the center.

**Discuss with Manager · draft** appends a separate block to the existing native
Manager composer. It preserves existing text and uses an atomic input insertion,
so normal composer Undo removes that block. It opens the existing right panel;
it never sends, prepares a journal intent, assigns an executor or dispatches work.
The event is refused after switching away from the originating project.

## Explicit binding

`ops/board.toml` is an optional companion to `ops/project.toml`. The latter's T3
and project fields are preserved. When the project manifest names `repo.host` and `repo.full_name`, the Board's
HTTPS repository must match them. Otherwise Create board takes an explicit
Repository URL directly in its form; it does not guess a repository or silently
rewrite the project manifest (#2027). Missing/invalid configuration is
visible; there is no directory-name inference or repository-wide issue query.

[The initial selection](examples/okilum-board.toml) is a schema/test example of
the first fourteen-issue slice. The manager-maintained canonical `ops/board.toml`
is authoritative and may select a newer explicit scope; never overwrite it with
this historical example. Stage note paths are area-relative Markdown. Related notes may also be public
HTTPS links without credentials. Local notes cannot escape through `..`, absolute
paths or symlinks.

The version-1 binding contains `title`, `goal`, `repository`, `stage_note`,
`refresh_seconds` (default 30, configurable), and ordered `issues`. `stage_note`
is optional (missing/empty means none); when supplied it is an existing project
Markdown note and need not live under Planning (#2027). Each issue has
`number`, `expected`, `executor`, optional `notes`, `pull_requests` and `builds`
(label + HTTPS URL). An optional legacy `execution` names both `thread_id` and `run_id` for cached
progress context. Working classification uses the automatic evidence below;
merely naming an executor does not establish that it is working on this issue.
Build links are explicit destinations, not verified inclusion or installation.

## Sources and honest states

Forgejo reads use the configured public repository API without any credential or
GitHub request. Live discovery supersedes the original selected-issue reads. T3 reads the bound executor project
through the already connected Manager gateway; it adds no connection, pairing or
Manager history poller. These requests run independently off the UI thread.

The Board reads on entry and Refresh, and periodically only while visible.
Overlapping requests coalesce. Hiding cancels its timer and invalidates pending
answers. Each cached Board belongs to one project; replies also carry owner and
generation identity, and cannot update a switched project. Configuration is
reread on refresh. A failed source retains its previous snapshot and successful
read time; partial Forgejo reads retain the entire last complete membership. A different source's
success does not make that failed source fresh. The UI is silent while sources
are healthy or refreshing. A failed or stale
source shows one compact line with its last successful read time. Manifest failures show the exact file/parser detail in the problem line and app
log. Invalid cards show their issue number, field and error beside healthy cards;
they do not hide the entire Board. Transport counters remain hidden.

Open issues need a verified open PR reference with successful aggregate checks,
or current-task/active-run evidence to enter In progress. Otherwise they stay in
Queue or Checks as described above. Retained closed issues stay in Checks:
merge/closure is neither available delivery nor Oleg acceptance. No delivery
placeholder or acceptance disclaimer is shown. Verified
feed/artifact correlation and human acceptance remain later slices; removing
empty placeholders does not establish those facts.

## Verification and acceptance

Targeted tests cover the exact fourteen-issue scope, closed without acceptance,
open-PR/current-task evidence, independent and partial failure, retained snapshots,
coalescing and late answers. Native GPUI fixtures exercise the actual Manager
composer (append, Unicode, Undo, no send and project mismatch), hidden Board state,
and real Reader project → note → Board navigation with the retained Manager draft.
The Forgejo source is injected only in offline native-navigation fixtures;
loopback fake-Forgejo tests additionally exercise the real production HTTP client,
conditional headers, close/merge transitions, Retry-After and incomplete responses.
Production uses the same bounded public client. These tests do not establish Oleg's visual acceptance.

Linux and Mac Sprint are parallel delivery destinations. Mac webview draft
integration, delivery feeds and execution mutations remain outside this slice: Board
currently appends to the native composer, separately from the visible Mac webview.

## Board v2: subtractive presentation

List rows stay 38 px tall. Their 12/13 px text, group headers and shared type,
origin/epic and agent primitives use an explicit 1.5 em line box so Latin and
Cyrillic descenders fit completely. Title and state overflow still uses ellipsis;
line height does not expand rows or change viewport-only rendering (#2548).

### Default List (#2530)

Projects with no saved view choice open **Список**. The **Список / Доска**
control retains the existing kanban as an explicit choice. View and group
collapse preferences are saved per canonical project path in app-owned state,
outside the vault and the disposable index.

After the optional Ждёт тебя group, the list groups the unchanged Snapshot in this
order: В работе, Проверки,
Можно попробовать, Очередь, Проверено. Queue and Verified start collapsed.
All rows are 38 px high and use GPUI `uniform_list`; a cached projection and
row index rebuild only on source publications, filter/search or collapse
changes. Scrolling and resizing build only viewport rows, including when the
240-issue synthetic fixture's Queue is expanded.

Rows show the source title and number, theme-based type tag, at most two
origin/epic pills, existing state or exactly matched cached progress, a PR link
with its actual aggregate CI state, a bound agent avatar and source-relative
updated time. Unknown/future timestamps show no invented age; an unavailable
CI state stays grey. A live dot requires an active run. Active/waiting agents
with no update for four hours show amber «⚠ долго», with the existing tooltip
identifying the age as time since thread update, not measured run duration.

Up/Down selects visible issues and skips group headings and waiting rows. Space, Enter and row
click open the issue panel described below. With the panel open, Up/Down updates
its issue. Board → related note → Board retains selection, shared filters and
list scroll. Existing local cards,
invalid annotations, attention, delivery and project-note context remain reachable.
List rows introduce no new read, dispatch, membership, stage rule or delivery claim.
Native tests cover defaults, independent persisted project choices, collapse,
240-row viewport work, keyboard reachability and actual Reader note round trips.
The sibling attention and ordering issues remain independent.


### Issue panel (#2531)

Clicking a List row or kanban card, or Space/Enter on the selection, opens the
right dock and selects **Задача**, beside **Manager**, using gpui-kit tabs.
Filtering rows retains the selected issue panel, using the cached complete
membership projection. Switching tabs retains the issue and the existing Manager entity,
composer draft and Undo state. Escape or **Esc закрыть** removes the issue tab
and returns to Manager without changing its draft. The macOS native WebView is
hidden while the issue tab is selected, including its keyboard routing; its
entity and browser draft are retained. Project/source changes close
the panel and invalidate outstanding comment results. Local task details and
Edit/Discuss are available in the same dock, without inventing execution facts.

The panel uses theme tokens, a 17 px semibold title with a 23 px line box, source
labels, the bound live agent/avatar and exactly matched cached current step.
The source-age label identifies time since update; amber after four hours is
not a measured run duration. Stage, all linked PRs with their actual state/title
and CI indicator, related/stage notes, expected result, description, build links
and existing delivery details remain reachable. Literal same-repository issue
mentions from the bounded description preview appear as labelled links, without
inferring dependencies. **В beta** lists exact builds
only from ready Beta rows in the existing delivery projection; otherwise it
says **ещё нет**. Merge alone never claims delivery.

**Что было** combines source creation/PR timestamps, verified Beta publication
and a bounded comments excerpt. Opening an issue makes at most one extra public
Forgejo request (first 12 comments, 256 KiB, 10 seconds, no redirects or tokens).
It runs off the UI thread, with at most one request in flight; rapid selection
changes coalesce to the latest issue. Project, repository, issue and generation
must all match before publication. No per-frame request or all-issue detail read
is added. Missing history and comment errors remain visible; full history opens
through **Issue ↗**. The timeline shows at most 16 source events and a plain **Полная история — в Issue ↗** link. Empty **Связано** properties are omitted; property labels reserve a full 20 px line box.

**Обсудить с Manager** switches to Manager → Chat and uses the existing protected
append-to-draft path, with Undo. It never sends or creates a thread. Issue, PR,
agent, note and build actions keep their existing destinations. Native GPUI
regressions exercise click/Space, tabs, arrows, Escape/draft retention, stale
owner/generation rejection, and real Reader Discuss without a send.

### Shared filter bar (#2532)

Waiting rows remain visible independently of the stage chip and search filters.

The native header places the shared chip bar immediately after Список / Доска
and replaces the executor icon with one selected chip: Все,
От Олега (`from-oleg`), Баги (`bug`), От testers (`from-tester`), or Агенты сейчас.
The last chip requires a unique bound thread, an unsettled active run and this
issue's explicit current-task mapping; an agent's backlog is not current work.
Counts show each chip's matches within the current search and extra filters.
The selected chip, all selected labels and the optional executor intersect.
＋ Фильтр lists the snapshot's labels (including epic labels) and executors;
selected extra filters remain visible as removable chips even if a refresh no
longer contains that value.

`/` focuses the native search input. Title matching uses Unicode lowercase
(including Cyrillic); `#123` and `123` also match the exact issue number.
Escape clears search and returns focus to the Board. Zero matches show
«Ничего не найдено · Сбросить фильтр». Local tasks participate in Все and title
search; issue-only label/agent filters do not invent metadata for them.
Invalid annotation diagnostics remain visible independently of filtering.

Filter and search preferences are stored per canonical project path in app-owned
state, outside the vault and disposable index, and survive note navigation and
reopening. Both List and Board consume the narrowed issue binding before grouping.
List rows and group counts consume the same cached membership as kanban. The List
row index follows projection identity and collapse state, rebuilding on filter or
source changes rather than on each frame or scroll.
The cached projection owns chip counts, column counts and membership; unchanged
frames reuse it. Snapshot, filter and delivery changes invalidate it, with merge
expiry and delivery freshness still respected. No additional source polling is
introduced. This header supersedes the historical single executor control below.

### Compact Board (#2534)

**Доска** shares the List's cached projection and row primitives. Working,
Checks and Can try have 268 px lanes; Queue has 240 px. Lanes scroll horizontally
inside the center and virtualise their card streams. Snapshot/filter publications
rebuild the groups; ordinary scrolling and selection build only visible cards.
Lane, horizontal and Queue scroll handles belong to the project Board entity,
so switching views or returning from a note retains their offsets. Queue has a
bounded scroll body when context or card details leave less vertical space.
The Verified control remains a 34 px rail at the right edge and explicitly opens
its lane, including when empty. Existing empty-stage controls remain available.

Cards occupy an 88 px virtualised row including spacing; ten complete cards
fit a 1000 px fixture window without context disclosures. Each issue card has three rows: number/type/up to two origin/epic pills and the
bound agent avatar; a regular 13 px source title clamped to two 19.5 px lines; and
existing state or exactly matched cached progress beside the PR number/actual CI
dot. Active stale threads (four hours since their last source update) show amber
with ⚠; failed CI shows red. Missing CI remains unknown. Selection uses an accent
border and hover a subtle theme surface. Shared text primitives reserve full
18 px line boxes at 12 px text, including descenders. No mockup colour constants are used.

Queue builds just the first eight 30 px mini rows. **＋ ещё N · открыть списком**
switches to List, expands Queue, scrolls to its heading and saves that view choice.
Local cards and annotation problems participate in the same count and remain
reachable. Both views apply the shared #2535 priority order at publication time;
Queue retains the «от Олега сначала» hint. The source timestamp stays lossless for
List age labels while ordering compares its parsed time. Clicking a compact card or Queue mini row opens **Задача** beside **Manager**,
with outcome, links, notes and draft-only Discuss. Space/Enter opens the selection;
Up/Down updates the dock and Escape returns to Manager with its draft intact.
There is no in-place disclosure in either view. Native GPUI
regressions cover card geometry, two-line clamping, bounded Queue work and view
handoff, actual CI/live/stale evidence, virtualised lanes and Verified expansion.

The compact Board consumes the same cached filtered projection as List, including
column membership, counts and #2535 stage order. It never re-sorts cards during
render. Shared chips and `/` search remain above both views; Queue's remaining-count
action preserves the active filters when opening expanded List. Compact metadata
pills and mini rows retain #2548's full font line boxes.

No source poll, execution inference, membership/stage mapping or delivery claim
is added by either presentation.

### Waiting for you (#2533)

The **Ждёт тебя** group (#2533) is the first group in the default List view;
in Board view it is a strip above the columns. It contains Решить (explicit
`questions_note` Open bullets and existing unresolved context decisions), Попробуй
(existing exact-build scenario notes, build label and optional video), and Новое
(unanswered from-tester/from-oleg reports younger than 24 hours). The optional
`questions_note` belongs in `ops/board-context.toml`; see [the binding and rules](board-context.md#waiting-for-you-2533).
List uses distinct fixed-height waiting rows: kind, text, source/owner, one action
button, relative time when known, and an optional scenario video link. The group
collapses independently of stages; its state is saved per project in app state,
with older presentation preferences defaulting to expanded. Waiting rows share
the virtualised stream with stages and keep full text line boxes for descenders.
The Board strip renders up to four compact cards, with explicit paging.
Empty means no group; source failure retains the last snapshot and shows one quiet
line. Projection work runs on snapshot changes and reuses the existing refresh,
not a new per-frame scan or poller. Every waiting publication invalidates the
cached List row index; repaint and scrolling reuse it. A failed first source shows
the quiet status line without an empty group.
The delivery summary in either view and expanded cards omits tracks without a
build, hides platforms/channels with no data, and disappears if no build exists.
Last-known builds and their source diagnostics remain visible on refresh failure.
If a failed delivery source has no last-known build, its diagnostic remains on
the same quiet status line even though the table and empty waiting group are hidden.
Item/stage colours and richer live-agent
presentation retain their existing independent sources.

## Accepted look, push 1

The native layout follows the accepted 27 October Board v2 mockup. Type and
origin chips use the issue labels already returned by the public Forgejo read;
missing type metadata stays a neutral issue, not an invented bug/task. The theme's
danger, important, muted, information, warning and success roles supply the pink,
violet, slate and stage colours in both appearance modes. Native Lucide icons and
a steady indicator replace browser effects. Cards show the source title, agent and state, with the explicit expected
outcome inside the issue panel, and at most one PR link badge; a PR link never claims
that its CI passed or it merged. The issue panel retains notes and draft actions.

Once card sources have completed a successful read, empty columns
collapse into compact name/count controls above the lanes (#2219), leaving room for useful content. Each control expands its empty lane with the keyboard
or pointer; the empty lane offers Collapse. Explicit expansion is window-local,
not a persisted setting. A newly nonempty lane always opens. Initial loading is
not zero cards; refreshes retain the last completed layout decision. An all-empty
Board shows a short empty state and keeps Add card available.
The Verified toggle remains a narrow native icon/count strip with a full
accessible label and tooltip; opening it explicitly expands the fifth lane,
even at zero cards, instead of forcing the first column off-screen.
`stage: ready` and `stage: verified` on an issue are optional, explicit human-owned
marks in `ops/board.toml`. Issue closure alone still means Checks. No current
binding is marked ready/verified by this implementation. The optional `stuck: true`
marker supplies amber styling; elapsed-time status is the separate third push.
The attention list and live-agent gateway details remain pushes 2 and 3.

## Explicit attention list, push 2

The optional `attention` list in the same human-edited `ops/board.toml` supplies
**Waiting for you** above the kanban. An empty or absent list renders no lane.
Each entry has `kind` (`decision`, `try` or `hands`), `title`, `subline` and `link`:

```toml
[[attention]]
kind = "decision"
title = "Choose the next slice"
subline = "Read the proposed decision first"
link = "decisions/next-slice.md"
```

A link is either an area-relative Markdown note or a public HTTPS URL without
credentials. Notes open through the ordinary Reader path in the same center;
Board return, filter, selected card and existing draft are preserved. HTTPS opens
the explicit destination. This action never sends a message or starts a workflow.
No issue status, label, agent activity or build report creates attention entries.
The manager maintains this list; delivery does not prepopulate it with invented
next actions. Presentation/list edits keep the existing issue snapshot and card
selection. Only repository changes invalidate issue membership; changing manual annotations
keeps the live snapshot through an outage.

## No header block, #1795 correction

The Board begins with the explicit attention lane when non-empty, then the
columns. It renders no stage title, goal sentence, Refresh control or Stage note
action. None is relocated to a menu or icon. Those fields remain binding/context
metadata; they are not a stationary header. The single small agent dropdown sits
at the right end of the attention heading, or as an icon at the right end of the
column-header row when the list is empty. Other related issue notes remain explicit
card actions; a note equal to `stage_note` does not reintroduce that removed action. Expanded links/actions wrap in a compact row, and the PR badge has
its own intrinsic width. There is no full-width command strip.

The manager-owned attention source is
`Dev/Areas/okilum/ops/board.toml`, field `attention`.
This change leaves that vault file untouched; the manager authors the real list.
Issue #1795 completion follows the working process in [AGENTS.md](../AGENTS.md);
Oleg's build approval is not a development gate.

## Live agent source, push 3

On macOS, read-only Board threads use the Manager panel's existing persistent
T3 web session and `/api/orchestration/shell`, through the same session gateway
as Projects and icons (#1958). They do not require a second MCP credential.
The configured project filters the server snapshot; only the authoritative
active turn establishes current work. A project without an executor registry
has no live-agent source to query and displays no connection warning. A present
malformed registry remains a visible, specific diagnostic. Source failures show
their concrete reason on the existing status line and in the application log,
while retaining the last successful snapshot and its original timestamp.

The current executor registry resolves each card to a literal thread in the already
connected gateway's bound-project listing. An exact registry role supersedes a
stale `agent_thread_id`; otherwise the literal ID must still belong to the registry.
A thread title or unregistered executor display name never establishes ownership.
The agent tag opens T3 through the existing server identity, using its encoded
thread URL. Missing or ambiguous bound agents are reported, not matched by name.

Status is working / waiting / idle (an unknown server status remains unavailable).
The displayed age is explicitly **time since the thread's last update**, not a
measured run duration. Invalid or future timestamps do not fabricate an age.
Active/waiting agents become amber after `stuck_after_seconds` (default 1800),
while idle threads do not become stuck just because they have old updates.
A human `stuck: true` remains an explicit override. A steady native activity dot
uses no animation timer or additional frame loop.

No new gateway call or polling timer is added. The existing Board source listing
supplies status and `updatedAt`; it does not supply executor message text. A short
progress line is shown only if the Manager already has that exact thread/run in
its conversation cache and the issue has the same execution link. Other agent
timelines are not fetched, and thread titles are never relabelled as progress.
Unavailable progress is omitted. Source errors keep the existing one-line notice
and retained snapshot. Agent activity alone never promotes an entire backlog. Only its explicit current
task can use active-run evidence; neither branch marks a build/issue accepted.

The manager may maintain the same canonical `ops/board.toml`, for example:

```toml
stuck_after_seconds = 1800
[[issues]]
number = 1767
expected = "Project work is visible on the Board"
executor = "Editing"
# Optional literal thread ID; the current registry takes precedence.
```

This is an explicit binding example, not automatic registry/role resolution.

Manager updates the literal agent IDs only after push 3 is delivered on both Mac
and Linux. Older delivered bindings remain valid without the optional field. The
initial fourteen-issue proposal is historical and is not applied automatically.


## Automatic Working evidence, #1860

An open issue is **In progress** when a currently open PR in the configured
repository references its `#number`/issue URL or is explicitly linked by the card.
PR status is read on the existing off-thread refresh, independently of badge text;
closed/merged PRs cannot promote an open issue. A failed or incomplete bounded PR
read retains the previous source with its existing unavailable/stale notice.
Closed issues and explicit human delivery/acceptance stages retain their precedence.

Without an open PR, an optional **Current task** column in `ops/thread-ids.md`
marks the executor's one current issue, as a positive number or `#number`.
Blank/`—` means unknown. The existing registry remains valid without the column;
older builds ignore that extra column, and `board.toml` gains no incompatible field.
This stable issue-level mark changes when the executor changes task, never with
each run. Other cards assigned to the same busy executor remain in Queue. The
current registry supersedes stale card IDs for an exact matching executor role;
a literal ID must otherwise still belong to that registry. A missing or unreadable
registry does not fall back to an old settled ID. Live Working requires a unique,
unsettled current thread, an `activeRunId`, and preparing/starting/running/waiting
status. A queued future turn, latest completed run, display title or executor name
alone cannot establish current work. `execution` remains optional historical
context for cached progress text; column classification does not require it.

## Compatible manifest transition (#1859)

Both manifests select TOML first and legacy YAML only when TOML is absent.
Malformed, unreadable or symlinked TOML is an explicit error; it cannot silently
select an older YAML copy. Readers never convert files. See the
[conversion and rollout guide](area-manifests.md). Existing YAML remains usable
unchanged until the manager completes the rollout on every channel and OS.

## Board management (#1862)

A project without a Board shows a quiet empty state and a primary **Create
board** action. The inline form starts with the Area's project title and goal
(`goal`, then `product.outcome`, then `description`); a missing goal is filled by
the person creating the Board. Repository is an explicit form input, prefilled
from the project manifest when configured and never from a folder name. A known
project repository must still match. Stage note is optional: exactly one Planning
candidate may prefill it; with none or multiple candidates, None remains valid. Creation writes an empty `ops/board.toml` only when
no Board file exists. An empty created Board is valid and prompts its first card.

The Board's **Manage board** icon opens the same inline surface. Add a card with
an issue number, `#number`, or exact issue URL in this Board's repository; edit
its expected result, executor and stage; remove it; or change the title, goal and
optional stage note under Board details. Automatic stage follows source evidence; Ready
and Verified are explicit human overrides. A reference adds only an explicit
card; it does not create, close or edit an issue, send a Manager message, or
assign an executor. Removal is immediate and has one revision-checked Undo
inside management until the next write. Errors remain beneath their fields and
the draft stays available after a write conflict.

The management view names the exact active file and format. Existing
`board.yaml` files remain JSON-in-YAML with two-space indentation; existing TOML
keeps TOML, including unrelated card content and comments. When both files
exist, only the reader-selected TOML is edited and the YAML is explicitly marked
inactive. No operation writes both files or performs vault-wide conversion.
Until the manager completes rollout, removing the last legacy YAML card is
refused with an explanation: older Okilum builds would show “Board unavailable”.
Add another card first or wait for that Board's migration to TOML. All other
legacy edits/removes work; a TOML Board may become empty. This temporary guard
is removed with the [vault migration](area-manifests.md#one-shot-conversion).
Changes use the revision and format selected when editing began. A changed
project manifest, external Board edit or new preferred-format file refuses a
stale write; close/reopen management to reload. A new file is published atomically
without replacing a concurrent creator, and existing files use synced
same-directory atomic replacement. Local-note, repository and per-field
validation remain enforced. All file work runs off the UI thread.

## Create board and source diagnostics (#2027)

Create board includes a Repository input with a full credential-free HTTPS
Forgejo `owner/repository` example. Project manifest repository bindings retain
precedence: an explicit mismatch or partial binding is a field error. A project
without that binding uses the repository the user entered for this Board only.
Repository is fixed after creation so numeric issue references are never silently
retargeted. Revision checks still protect both the selected Board file and the
unchanged project manifest; saving never edits the latter.

Stage note is optional, with an explicit None choice. Existing Planning notes
are suggestions, not a creation prerequisite. Existing chosen notes and unrelated
manifest fields are preserved. An explicitly chosen note still cannot escape the
project, including through symlinks. Older reader builds may reject a new Board
with no stage note until this fix reaches their channel; this does not rewrite
existing Board bindings or create placeholder notes.

Registry Current task remains one issue number (`2020`, `#2020`, inline code) or
blank/dash. A compound queue/PR/history value is not guessed into a current task.
Its diagnostic names the actual registry file, physical line, role and exact cell,
and explains where to move the additional prose. Registry diagnostics do not
become a missing-project or repository error.

The browser-session Board route sends `x-t3-orchestration-protocol: 2`, required
by the T3 orchestration API header schema. Projects/auth use their existing
separate contract; a valid cookie alone does not satisfy the orchestration schema.
A TCP regression rejects the previous header-less request with 400 and accepts
the real client with the required header and same cookie. An authenticated native
server reproduction is separate; no cookie/token is copied for this diagnosis.

## Local empty Boards (#2033 slice 1)

Goal and Repository are now optional when creating a Board. An unbound Board
shows a quiet Local Board state with inactive PR/CI links; add Repository in
Manage board → Board details later. Explicitly supplied URLs retain validation
and known-project matching. A bound repository cannot be retargeted. No executor
or stage is required to create the empty Board. Local cards use an independent
sidecar and draft-only discussion; verified local Manager launch follows
separately. See [local project start](local-project-start.md).
