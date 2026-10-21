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
