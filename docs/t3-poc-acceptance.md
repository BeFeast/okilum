# T3 POC acceptance on the real server (#1422)

Part of #1415. The POC checks run against the live T3 server at
`https://t3.oklabs.uk`, using only the isolated test area and test manager from
#1421. They never touch a production manager. Each check uses the same
`okilum_t3` code as the Project tab: OAuth/PKCE, `reconnect::connect`,
`chat::Conversation`, `executors::Snapshot`, `launch::{Request, progress::Tracker}`,
`models::Target` and `McpGateway`.

## Runner

`crates/okilum-t3/examples/poc_acceptance.rs` runs one phase per invocation and
prints one JSON object per check:

```sh
export OKILUM_ACCEPTANCE_TOKEN=$(mktemp -d)/token   # created 0600 by connect
poc_acceptance https://t3.oklabs.uk <area> connect          # prints AUTHORIZE_URL; callback by loopback or pasted on stdin
poc_acceptance https://t3.oklabs.uk <area> model <instance> <model>
poc_acceptance https://t3.oklabs.uk <area> chat
poc_acceptance https://t3.oklabs.uk <area> drop-resend
poc_acceptance https://t3.oklabs.uk <area> launch <role> <worktree> <branch> [instance/model]
poc_acceptance https://t3.oklabs.uk <area> executors
poc_acceptance https://t3.oklabs.uk <area> revoked <script that revokes the session on the server>
```

- **Approval.** The browser step was done headlessly. A short-lived T3 pairing
  code (`t3 auth pairing create --ttl 10m`, default scopes) went to T3's own
  approval API, `POST /oauth/mcp/decision` with
  `{"_tag":"pairing-code","access":"full-access"}`. The returned
  `redirectTo` reached the runner's loopback listener.
- **Token storage.** No OS keychain was used on the run host. The token lived
  only in the disposable 0600 file and was revoked on the server at the end.
  Shell keychain storage is covered by the native Manager acceptance (#1788,
  #2516).
- **Connection drop.** `drop-resend` sends through a loopback HTTP → HTTPS
  forwarder. The forwarder passes the `t3_thread_send` request to T3, waits for
  T3's reply, then closes the client socket without returning it.

## Results — 2026-11-18

Runner source `6ccf65db` (chat prompt lengthened in `96a6c154`). Release build
on the build queue, slot 3, clippy `-D warnings` clean. Run host: maestro, over
LAN through `https://t3.oklabs.uk`. T3 `0.1.10-nightly.20261116.4761`.

| # | Item | Result | Evidence (key values) |
|---|---|---|---|
| 1 | Connect via OAuth | **PASS** | PKCE + dynamic registration, scope `orchestration:read orchestration:operate settings:write source-control:write`, 30-day expiry; reconnect from the saved credential without a browser (`issued_token = None`, `Connected`). Approving with `auto` access was refused for the full-access manager (`runtime_mode_escalation_denied`), as designed. Keychain: see above. |
| 2 | Chat: history, send, long reply | **PASS, except "growing"** | History: 7 items, 1 page. Send: `delivery: started`. A 600-line reply (5 219 chars) was read in full and settled after 26.6 s. **The reply was never seen growing:** with the `claudeAgent` and `cursor` drivers, T3's `t3_thread_read` shows an assistant message only once it is `completed`. A raw MCP probe every 150 ms saw no `running` assistant item and no partial text (`probe_stream*.log`). The Codex driver, which streams, could not be checked: `codex/gpt-6.1-sol` is rate-limited until 20.11, `codex/claude-sonnet-5` failed and `codex/gemini-3.8-flash-high` hung (interrupted). |
| 3 | Executors tree | **PASS** | Registered executor `mcp:launch:3e7f…`: status `completed`, itemCount 16, PR list `[]`, 0 pending questions, Open in T3 `t3code://threads/<env>/<thread>`; no unregistered threads. |
| 4 | Launch through the manager | **PASS** | `requested → accepted` at 0.2 s, `preparing` at 27.8 s, `running` at 44.7 s. The executor thread was matched by the request marker. The manager recorded it in the registry with the `request_id`. The executor wrote `result.md` on branch `qa1422-202611180526`, in T3's managed worktree directory rather than the requested path (see note). |
| 5 | Change the manager's model | **PASS** | `codex/gpt-6.1-sol (high/default)` → `claudeAgent/claude-sonnet-5-5`. The configure read-back and a separate `t3_thread_configure` read both show the new model. Three more changes read back the same way. The original selection was restored afterwards and read back. |
| 6 | Revoke on the server → "revoked" | **PASS** | `t3 auth session revoke <session>`. The first request after it, 17 ms later with no retry, was refused (`invalid_mcp_credential`). `GatewayStatus::Revoked` maps to the Project tab text "Your T3 connection was revoked. Reconnect to continue." |
| 7 | Drop during send, resend | **PASS** | The first send was lost after T3 accepted it (`Unreachable`, "connection closed before message completed"). Sending the same `clientRequestId` twice more returned the same `messageId` both times. One copy is in the timeline. The manager project still has 1 thread before and after. |
| 8 | Codex manager read `_AgentContract` | **PASS (earlier)** | #1421 setup receipt. |

Note on item 4: the request carries a worktree path, but T3's `t3_thread_launch`
`workspaceStrategy` places the worktree under its own managed directory. The
branch is honored. Okilum follows the launch by marker and T3 status, never by
path, so the tracker is unaffected.

The full JSONL output, probe logs and build receipts are kept in the project
brain under `Dev/Areas/okilum-t3-test/evidence/1422-2026-11-18/`.
