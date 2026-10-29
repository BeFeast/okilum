# Project manager chat (#1418, #1415)

The 22 October [project navigation contract](project-navigation.md) supersedes
the current-note Project button and adds manager-only bindings for minimal
projects. The native transport and chat behavior below remain the fallback.

The approved Linux native-client work (#1787) is incremental. Its
[WS v2 transport boundary](manager-stream-transport.md) is separate from the
current MCP connection; feed projection and Tier 1 actions follow separately.

The Project control in a vault Reader window opens the manager of the area
containing the current note. It reuses the existing `okilum-t3` OAuth MCP
`ManagerGateway` and the implementation from closed, unmerged PR1427. The approved
contract is the vault spec `specs/2026-10-18-okilum-t3-managers.md`, including its
18 October decision24 limitation. Okilum remains a remote control; the manager
and its executors run in T3. Ordinary notes remain available when Project closes.

The nearest area's `ops/project.toml` (legacy `ops/project.yaml` while TOML is absent) must already contain the explicit version1
`t3` binding with environment alias, manager project/thread and executor project.
An absent binding displays “not connected”; Okilum neither creates T3 resources
nor guesses a newer manager. A changed binding gets a separate view. Drafts for
an unchanged binding survive note navigation and closing/reopening Project.

Settings → T3 owns the app-wide sign-in (#1758), described in the
[connection contract](project-navigation.md#one-authenticated-connection-per-app-1758).
It checks and reuses the origin-bound OS credential, offers authorization and
retains callback recovery after timeout. Mac sign-in shares the Manager panel's
app-specific WebKit store; other platforms use the system browser. Only origins
are saved in application state, and explicit project server overrides stay separate. Polling loads history and refreshes growing
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
the registered executor list shows actual T3 states. Automatic correlation of a
request to manager acceptance and its resulting thread is not implemented in this
slice, and must not be inferred from queue receipts, titles or parentThreadId.
Scheduled-task model following, manager migration and live integrated acceptance
remain pending/blocked as described above. #1420 is not closed by request queuing.

## MCP compatibility (#1759)

Each authenticated MCP connection discovers its scoped `tools/list` catalog once
(with bounded pagination). The catalog is reused by polling and refreshed after
reconnection or a dropped session. Missing `t3_environment_read`, `t3_thread_read`
or `t3_thread_send` gives a compatibility error naming the required tool before
any feature call. Missing optional tools disable only their feature with a named
explanation; no request is sent to a tool absent from that catalog.

Pending questions use `t3_pending_request` with `action: list`. An unavailable
question listing is shown as unavailable, never as a confirmed zero, while
manager history, project verification and message sending remain available.
Model reads use `t3_thread_configure` with only `threadId`; explicit model changes
still require confirmation and separate configuration readback. Brain-local
`t3_target_*` operations are unrelated to MCP tool names and remain unchanged.

The checked-in 27 October input-schema fixture comes from the live manager agent
catalog, not an Okilum OAuth `tools/list` capture. OAuth scopes can filter tools;
real connection and response-shape acceptance remain pending until verified with
an authorized paired client. Fixture tests do not claim live OAuth acceptance.

## Reconnect and token expiry (#1773)

The Manager **↻** control reads the saved credential for the entered origin and
reconnects with initialize and tools/list while that token is valid, including
the final three days. It does not start OAuth or replace the keychain entry after
a successful reconnect. Expired or missing credentials, or a 401 explicitly
reporting `invalid_mcp_credential`, start authorization after this explicit user
action. Network errors, generic 401 responses and missing tools remain visible
errors; they never silently start another pairing flow or discard the credential.
Changing the T3 origin cannot reuse or transmit the previous server's token.

**Sign in again** explicitly authorizes a new connection. The panel shows the
stored token's expiry in UTC; within three days this action is labelled **Renew**.
There is no refresh grant or automatic renewal. A successful new authorization
is saved before subsequent polling, so a later network error does not lose it.
Re-entering a project continues to reuse stored credentials without opening a
browser. Keychain errors remain visible; explicit sign-in can replace a damaged
credential after successful authorization.

When browser approval needs sign-in, the panel explains that a browser sign-in
lasts 30 days and offers **Open T3 in browser** for the entered origin. An already
signed-in browser can approve without a pairing code. The existing 180-second
OAuth callback path and paste-back follow-up (#1758) are separate work.

## Linux virtualized feed (#1787, slice 1)

The 27 October decision explicitly permits a Linux native T3 client for Tier 1.
This first slice keeps the existing MCP gateway, authorization, durable send
journal and question link. WebSocket v2 transport and native turn controls are
later slices. The macOS embedded T3 webview is unchanged.

On Linux the message history uses the vendored MessageScroller virtual list.
Only viewport rows and its fixed overdraw are rendered, including for thousands
of loaded messages. It follows the latest reply until the reader scrolls away;
new messages preserve that choice, and Jump to latest resumes tail following.
Growing replies invalidate only their affected row measurements. Retained send
receipts disappear when their message is projected; unconfirmed operations keep
their original Retry identity.

Poll workers receive the cursor and growing item identities instead of cloning
the loaded conversation. Successful reads apply only unequal items and notify
the panel only when displayed data or connection status changes. The composer
owns its input notifications, and the cached history entity stays independent
of keystrokes. Tests exercise a 5,000-message thread with bounded row rendering,
scroll and streaming behavior, unchanged polling, and real composer input; these
are regression evidence, not live Linux/Hyprland acceptance.

Vendor patch 0095 preserves selection registrations through GPUI cached paint
using an element-state lease that expires on unmount. It also makes clearing an
already-empty selection silent. This prevents selection cleanup from repeatedly
invalidating the cached feed while preserving selectable Markdown and Copy.
