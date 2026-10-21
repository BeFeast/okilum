# Project manager chat (#1418, #1415)

The Project control in a vault Reader window opens the manager of the area
containing the current note. It reuses the existing `okilum-t3` OAuth MCP
`ManagerGateway` and the implementation from closed, unmerged PR1427. The approved
contract is the vault spec `specs/2026-10-18-okilum-t3-managers.md`, including its
18 October decision24 limitation. Okilum remains a remote control; the manager
and its executors run in T3. Ordinary notes remain available when Project closes.

The nearest area's `ops/project.yaml` must already contain the explicit version1
`t3` binding with environment alias, manager project/thread and executor project.
An absent binding displays “not connected”; Okilum neither creates T3 resources
nor guesses a newer manager. A changed binding gets a separate view. Drafts for
an unchanged binding survive note navigation and closing/reopening Project.

For an already-bound area, enter its T3 HTTPS origin and choose Connect. The
existing authorization-code/PKCE flow opens the system browser; this is an
explicit user action. Tokens stay in OS credential storage; only alias/origin
are saved in application state. Polling loads history and refreshes growing
replies by item ID, including text chunks. Before enabling Send, the server must
confirm that the manager belongs to the manifest's project. Expired/revoked
credentials stop polling and disable sends with a reconnect explanation. An
unreachable server preserves the previous history and retained messages while
polling retries; other failures are shown explicitly.

A send persists its operation identity outside the rebuildable index before
network dispatch. An interrupted response leaves an unconfirmed operation;
explicit Retry reuses that identity, including after restart. An acknowledged
operation is not resent. If preparing a new intent fails, its composer text is
kept even when an older operation has identical text. A receipt means accepted
or queued, not that the manager completed the task. Questions stay in T3, with an
Open in T3 action using the server's actual environment identity.

This slice adds the production Manager view in the default application. The
existing non-publishing `project-ui-harness` remains a test-only fake gateway;
its historical frames do not certify OAuth, send or reconnect against a live
server. Current runtime/human acceptance is pending. No production manager,
vault, token or T3 resource is migrated by this change.

Executor launch/model UI (#1420), isolated onboarding and integrated
acceptance (#1421–1422) remain open. Scheduled-task model following is unsupported;
a thread model change must not imply that task models followed it. The active
manager must not migrate before the agreed T3 scheduler fix and acceptance.

## Executors (#1419)

The Executors segment reads the area’s existing `ops/thread-ids.md` current
registry and projects statuses from its bound executor T3 project. It supports
one `## Current threads` or `## Текущие threads` section (a dated suffix is fine),
one table beginning `Role | Thread` or `Роль | Thread`, and one inline
backtick-quoted `mcp:` ID per row. The sole Manager row must match the explicit
manifest manager. History sections are ignored; duplicate/ambiguous IDs, missing
manager, unsupported format, registries over256KiB or over128 executors fail
visibly instead of selecting a partial registry or guessing ownership.

Registered executors remain under the manager even when T3’s `parentThreadId`
is null. Each matching row shows run status, item count, all linked PRs, pending
question count and Open in T3 with the actual server identity. A registry entry
missing from the bound T3 project is shown as missing. Other T3 threads appear
in an explicit unregistered group with ownership/question status not established.
A failed question read is unknown, never zero. PR actions accept HTTP(S) web
URLs without embedded credentials.

The existing serial manager poll refreshes this segment while it is selected;
closing Project deactivates that poller. Metadata/registry errors retain the last
executor projection and manager draft with an error. No area registry, manager
binding, T3 resource or provider setting is written by this read-only slice.
Runtime/live OAuth acceptance and ordinary human UI QA remain pending.

### Thread model controls (#1420, next production slice)

The Manager header and registered, matched executor rows offer **Model**. Opening
it reads the live T3 catalog and the exact thread configuration. Selecting a model
is local; **Apply thread model** rechecks the thread's bound project and current
catalog before calling T3, then shows the configuration read back from T3. A queued
or successful mutation acknowledgement alone is not confirmation. A mismatched
readback, permission error, revoked token or failed request is visible without
changing the manager draft or automatically retrying the mutation. **Refresh
models** explicitly reads the current configuration after an uncertain result.
Unregistered or missing executor rows do not offer mutation controls.

Reselecting the current provider/model preserves its option overrides. Selecting
a different model clears those overrides and requests T3 defaults. The UI does
not edit model options in this slice; strict readback differences remain explicit.
The last successful read is labelled **Last confirmed readback**, rather than
asserting that another client cannot change the thread afterwards.

**Scheduled tasks do not follow this model change.** This restriction is displayed
beside the controls and remains visible after success or failure. Manager migration
and scheduler acceptance remain blocked under decision24. Launch requests through
the manager are the next part of #1420; this slice does not create T3 resources.
Real OAuth/model mutation and human UI acceptance remain unverified by these tests.

### Executor requests through the manager (#1420)

**Request executor** collects a role, a UTF-8 brief file relative to the selected
vault area, an absolute worktree path on the Unix T3 server, a branch, and either
the bound repo project's live default model or an explicit live-catalog override.
The actual bound T3 project's title/root is shown. Local device paths are never
used to open/create remote worktrees. Briefs are limited to 64 KiB; missing,
invalid, escaping or oversized briefs and changed area bindings reject the request
without truncation. Inputs and the independent manager chat draft are retained.

Before sending, Okilum rechecks the manager's project, the bound executor project,
current model catalog and on-disk area binding. It captures the complete brief and
request identity in the existing durable message journal outside the index, then
queues the request to the manager. Okilum does not call `t3_thread_launch` directly.
An unconfirmed send blocks a second intent; use the retained message's **Retry**
in Manager, which keeps the original clientRequestId and complete request payload.
After confirmed delivery a separate new request requires an explicit user action.

A delivered message means **launch requested**, not manager acceptance or a
running executor. The UI says so. The manager is instructed to reconcile ambiguous
launch responses, register the actual thread and report its lifecycle in chat;
the registered executor list shows actual T3 states. Automatic correlation uses only the explicit Launch request column described
below. Queue receipts, titles and parentThreadId never establish this relation.
Scheduled-task model following, manager migration and live integrated acceptance
remain pending/blocked as described above. #1420 is not closed by request queuing.


### Launch lifecycle correlation (#1420)

The existing current registry table may include one optional **Launch request**
column. The manager records the exact `okilum-launch-<UUID>` from the request,
in inline backticks, on the resulting executor row; Manager and older rows leave
it blank. No registry write or T3 resource creation is performed by the UI.
Malformed/duplicate request identities, multiple such columns or a request ID on
the Manager row reject that refresh. Legacy registries remain valid and show
requests as awaiting explicit correlation.

A matching registry row is the explicit manager acknowledgement of that launch.
If its thread is missing from the bound T3 project, the UI says **Manager accepted ·
registered** with execution status unavailable. Once listed, that exact thread's
T3 status supplies preparing/running/waiting/completed/failed/cancelled/interrupted
or idle; idle and unknown statuses are never called completed. Duplicate retained
request IDs and manager/project mismatches show an error rather than selecting a
thread. Open executor in T3 uses that registered ID and the actual environment.
No pre-registration acknowledgement is invented: before the manager writes the
correlation the state remains Requested, even if message delivery is confirmed.

The existing single Project poll refreshes launch state while Manager is selected,
including after reopening from durable request records. This path reads the
registry and project thread listing without issuing executor question calls;
the Executors segment still loads question indicators normally. A refresh failure
keeps the last rows with **Last observed** and an explicit stale warning. Closing
Project stops future polls. Failed delivery remains a separate, visible receipt
state and never triggers another launch. Existing pre-correlation requests may
need their manager to add the known request ID; do not create a new request to
obtain a status. Live launch/OAuth, scheduler following, manager migration and
human runtime acceptance remain unverified/blocked; no whole-epic acceptance claim.
