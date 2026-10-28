# Project Board — Sprint slice 1 (#1767)

The shared Board model is upstreamed for #1860. The native project-center UI
described below remains in `sprint/board`; this model change does not activate it
in main. Native UI follow-ups are tracked separately.

Entering a project opens a native Board in its existing center pane. Notes and
Board share that center. Returning with **Board** retains the executor filter,
selected card and scroll. The existing right-hand Manager and its draft stay in
place. Note opens still use the Reader's ordinary revision/save/refusal path;
only a successfully accepted Markdown document hides the Board.

The stage is an explicit selection, independent of whether issues are open or
closed. An issue card shows its expected result and executor, expands in place,
and opens explicit issue/PR/build links or related area notes. Escape closes its
details. The horizontal lane scroll is local to the center.

**Discuss with Manager · draft** appends a separate block to the existing native
Manager composer. It preserves existing text and uses an atomic input insertion,
so normal composer Undo removes that block. It opens the existing right panel;
it never sends, prepares a journal intent, assigns an executor or dispatches work.
The event is refused after switching away from the originating project.

## Explicit binding

`ops/board.yaml` is an optional companion to `ops/project.yaml`. The latter's T3
and project fields are preserved. The Board's HTTPS repository must match the
manifest's `repo.host` and `repo.full_name`. Missing/invalid configuration is
visible; there is no directory-name inference or repository-wide issue query.

[The initial selection](examples/okilum-board.yaml) is a schema/test example of
the first fourteen-issue slice. The manager-maintained canonical `ops/board.yaml`
is authoritative and may select a newer explicit scope; never overwrite it with
this historical example. Stage and related note paths are relative to that area, must be
Markdown paths, and cannot escape through `..`, absolute paths or symlinks.

The version-1 binding contains `title`, `goal`, `repository`, `stage_note`,
`refresh_seconds` (default 30, configurable), and ordered `issues`. Each issue has
`number`, `expected`, `executor`, optional `notes`, `pull_requests` and `builds`
(label + HTTPS URL). An optional `execution` names both `thread_id` and `run_id`;
merely naming an executor does not establish that it is working on this issue.
Build links are explicit destinations, not verified inclusion or installation.

## Sources and honest states

Forgejo reads use the public API without any credential or GitHub request. Only
the selected issue endpoints are requested. T3 reads the bound executor project
through the already connected Manager gateway; it adds no connection, pairing or
Manager history poller. These requests run independently off the UI thread.

The Board reads on entry and Refresh, and periodically only while visible.
Overlapping requests coalesce. Hiding cancels its timer and invalidates pending
answers. Each cached Board belongs to one project; replies also carry owner and
generation identity, and cannot update a switched project. Configuration is
reread on refresh. A failed source retains its previous snapshot and successful
read time; partial Forgejo reads retain issues that failed. A different source's
success does not make that failed source fresh. The UI is silent while sources
are healthy or refreshing. A failed or stale
source shows one compact line with its last successful read time. Internal error
text and polling counters are not presented as ordinary Board content.

Open issues without an exact issue-to-run link stay in Queue. In progress needs
that exact run in the bound project's live listing. Closed issues stay in Checks:
merge/closure is neither available delivery nor Oleg acceptance. No delivery
placeholder or acceptance disclaimer is shown. Verified
feed/artifact correlation and human acceptance remain later slices; removing
empty placeholders does not establish those facts.

## Verification and acceptance

Targeted tests cover the exact fourteen-issue scope, closed without acceptance,
exact-run execution matching, independent and partial failure, retained snapshots,
coalescing and late answers. Native GPUI fixtures exercise the actual Manager
composer (append, Unicode, Undo, no send and project mismatch), hidden Board state,
and real Reader project → note → Board navigation with the retained Manager draft.
The Forgejo source is injected only in the offline navigation fixture; production
uses the public client. These tests do not establish Oleg's visual acceptance.

Linux and Mac Sprint are parallel delivery destinations. Mac webview draft
integration, delivery feeds and execution mutations remain outside this slice: Board
currently appends to the native composer, separately from the visible Mac webview.

## Board v2: subtractive presentation

The compact card contains its issue title/number, a small agent tag and one
existing stage-state line. The expected result and issue/PR/build/note/Discuss
actions are inside the expanded card. Related-note navigation retains the same one-action return to Board. Escape
still collapses the card. Refresh is automatic and silent.
There is no new source poll, execution inference, delivery claim or status mapping.
The human-attention lane, item/stage colours and richer live-agent presentation
wait for Oleg's visual mockup acceptance.

## Accepted look, push 1

The native layout follows the accepted 27 October Board v2 mockup. Type and
origin chips use the issue labels already returned by the public Forgejo read;
missing type metadata stays a neutral issue, not an invented bug/task. The theme's
danger, important, muted, information, warning and success roles supply the pink,
violet, slate and stage colours in both appearance modes. Native Lucide icons and
a steady indicator replace browser effects. Cards show the explicit expected
outcome, agent and state, and at most one PR link badge; a PR link never claims
that its CI passed or it merged. The expanded card retains notes and draft actions.

Columns have zero minimum width and equal flexible bases. The Verified toggle is
a narrow native icon/count strip with a full accessible label and tooltip; opening
it adds the fifth flexible column instead of forcing the first column off-screen.
`stage: ready` and `stage: verified` on an issue are optional, explicit human-owned
marks in `ops/board.yaml`. Issue closure alone still means Checks. No current
binding is marked ready/verified by this implementation. The optional `stuck: true`
marker supplies amber styling; elapsed-time status is the separate third push.
The attention list and live-agent gateway details remain pushes 2 and 3.

## Explicit attention list, push 2

The optional `attention` list in the same human-edited `ops/board.yaml` supplies
**Waiting for you** above the kanban. An empty or absent list renders no lane.
Each entry has `kind` (`decision`, `try` or `hands`), `title`, `subline` and `link`:

```yaml
attention:
  - kind: decision
    title: Choose the next slice
    subline: Read the proposed decision first
    link: decisions/next-slice.md
```

A link is either an area-relative Markdown note or a public HTTPS URL without
credentials. Notes open through the ordinary Reader path in the same center;
Board return, filter, selected card and existing draft are preserved. HTTPS opens
the explicit destination. This action never sends a message or starts a workflow.
No issue status, label, agent activity or build report creates attention entries.
The manager maintains this list; delivery does not prepopulate it with invented
next actions. Presentation/list edits keep the existing issue snapshot and card
selection. Repository or selected-issue identity changes still invalidate reads.

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
`/home/god/Obsidian Vault/Dev/Areas/okilum/ops/board.yaml`, field `attention`.
This change leaves that vault file untouched; the manager authors the real list.
Issue #1795 stays open until Oleg approves the resulting build.

## Live agent source, push 3

An issue's optional `agent_thread_id` names one exact thread from the already
connected gateway's bound-project listing. Missing `agent_thread_id` means no live status and no binding error, even when
there is an issue execution link. The executor name alone never resolves a live thread.
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
and retained snapshot. Agent activity does not promote an issue to working or
mark a build/issue accepted.

The manager may maintain the same canonical `ops/board.yaml`, for example:

```yaml
stuck_after_seconds: 1800
issues:
  - number: 1767
    expected: Project work is visible on the Board
    executor: Editing
    agent_thread_id: mcp:ccd5df8a-17e0-4e4c-8aae-cdffd5524537
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
older builds ignore that extra column, and `board.yaml` gains no incompatible field.
This stable issue-level mark changes when the executor changes task, never with
each run. Other cards assigned to the same busy executor remain in Queue. The
current registry supersedes stale card IDs for an exact matching executor role;
a literal ID must otherwise still belong to that registry. A missing or unreadable
registry does not fall back to an old settled ID. Live Working requires a unique,
unsettled current thread, an `activeRunId`, and preparing/starting/running/waiting
status. A queued future turn, latest completed run, display title or executor name
alone cannot establish current work. `execution` remains optional historical
context for cached progress text; column classification does not require it.
