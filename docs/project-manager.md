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

Executors/tree/launch/model UI (#1419–1420), isolated onboarding and integrated
acceptance (#1421–1422) remain open. Scheduled-task model following is unsupported;
a thread model change must not imply that task models followed it. The active
manager must not migrate before the agreed T3 scheduler fix and acceptance.
