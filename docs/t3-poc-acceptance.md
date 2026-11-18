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
  T3's reply, keeps its status and JSON-RPC result (never headers), then closes
  the client socket without returning it. The check passes only when that
  withheld reply was HTTP 2xx with a `messageId` and no error, the resends
  return the same `messageId`, the timeline holds one copy, and the manager
  project gains no thread.
- **Revoke.** The script revokes the session with `t3 auth session revoke`
  and fails unless the session is then missing from `t3 auth session list`.
  Only T3's explicit `invalid_mcp_credential` refusal
  (`GatewayError::InvalidCredential`) passes, not any 401.

## Results — 2026-11-18

Two runs from maestro over LAN to `https://t3.oklabs.uk`, T3
`v0.1.10-nightly.20261116.4761` (`t3-version.txt`). Each runner build ran on the
build queue: `cargo build --locked --release` plus
`cargo clippy --locked -p okilum-t3 --example poc_acceptance -- -D warnings`,
both clean (`checks.sh`, `build*.log`).

- **Run 1**, runner `6ccf65db`, later `96a6c154` (only the chat prompt: 120 →
  600 numbers; poll every 200 ms instead of 400 ms): connect, model, chat,
  launch, executors, drop, revoke.
- **Run 2**, runner `aa7cf5bf`, after the cross-family review tightened the
  pass criteria: connect, executors, drop, revoke again (`run2/`).

| # | Item | Result | Evidence |
|---|---|---|---|
| 1 | Connect via OAuth | **PASS** | PKCE + dynamic registration. A full-access approval gave scope `orchestration:read orchestration:operate settings:write source-control:write` with a 30-day expiry. Reconnecting from the saved credential needed no browser (`reconnect_without_browser: true`, `Connected`), in both runs. An earlier `auto`-access approval also connected (`connect-auto-access.jsonl`), but T3 then refused its model change on the full-access manager (`runtime_mode_escalation_denied`, `model-auto-access-refused.jsonl`), as designed. Keychain: see above. |
| 2 | Chat: history, send, long reply | **PASS, except "growing" (not observed)** | 120-number run (`chat-120-lines.jsonl`): history 7 items in 1 page; reply 1 037 chars, settled at 18.2 s. 600-number run (`chat.jsonl`): history 16 items in 1 page, `delivery: started`, reply 5 219 chars first seen at 21.8 s already `completed`, settled at 26.6 s. Both runs fail the growing criterion: it needs two length increases, and only one completed observation was seen. A raw MCP probe from the latest cursor every 150 ms (`probe_stream.log` claudeAgent, `probe_stream_cursor.log` Cursor driver) saw the assistant item appear once, already `completed` at 3 443 chars, with no `running` assistant item and no shorter text. **With these two drivers, T3's `t3_thread_read` exposes a reply only once it is complete.** The Codex driver was not observed: `codex/claude-sonnet-5` failed after 9.8 s with no reply (`manager-runs.tsv` ordinal 10, no reason recorded); `codex/gemini-3.8-flash-high` stayed `running` with no assistant item from 05:31:06 until we interrupted it at 05:36:29 (ordinal 12); `codex/gpt-6.1-sol` failed in 2.2 s (ordinal 13). That fits the 17.11 rate-limit probe in the model policy (Sol 429 until 20.11), but this run recorded no failure reason. **Open:** re-check growth on a working Codex model. |
| 3 | Executors tree | **PASS** | Registered executor `mcp:launch:3e7f…`: listed, status `completed`, itemCount 16, PR list `[]`, 0 pending questions with no question error, Open in T3 `t3code://threads/<env>/<thread>`, no unregistered threads (run 2 requires all of these). |
| 4 | Launch through the manager | **PASS** | `launch.jsonl`: accepted at 184 ms, preparing at 27 789 ms (sampled again at 41 612 ms), running at 44 658 ms. `running` requires the executor thread's first timeline line to be this request's marker. The registry gained the executor row with request id `okilum-launch-9a319e76-…` (`registry-thread-ids.md`). The executor wrote `result.md` ("Isolated executor reached running") on branch `qa1422-202611180526` (`executor-worktree.txt`, `executor-thread.json`), in T3's managed worktree directory rather than the requested path (see note). |
| 5 | Change the manager's model | **PASS** | `model.jsonl`: `codex/gpt-6.1-sol (high/default)` → `claudeAgent/claude-sonnet-5-5`, confirmed by the configure read-back and a separate `t3_thread_configure` read. Three more changes read back the same way (`model-2..4.jsonl`). The original selection was restored and read back (`restore-model.log`). |
| 6 | Revoke on the server → "revoked" | **PASS** | Run 2: the script revoked the session and confirmed it was gone from the list. The first request afterwards, 23 ms later with no retry, failed with `InvalidCredential`, and the status became `Revoked` (run 1: 17 ms). The runner does not render text. In the Project tab, `status_note` (`okilum-shell/src/project.rs`) maps `Revoked` to "Your T3 connection was revoked. Reconnect to continue." |
| 7 | Drop during send, resend | **PASS** | Run 2 `drop.jsonl`: the withheld reply was HTTP 200, no error, `delivery: started`, with `messageId …okilum-f2993838…`. The client saw `Unreachable` ("connection closed before message completed"). Two resends with the same `clientRequestId` returned that same `messageId`. One copy in the timeline; manager project threads 1 → 1. |
| 8 | Codex manager read `_AgentContract` | **PASS (earlier, not re-run)** | #1421 setup receipt (#1421 comment 34555). |

Note on item 4: the request carries a worktree path, but T3's `t3_thread_launch`
`workspaceStrategy` placed the worktree under T3's own managed directory. The
branch was honored. Okilum follows the launch by marker and T3 status, never by
path, so the tracker is unaffected.

The full JSONL output, probe logs, run history and build logs are in the project
brain under `Dev/Areas/okilum-t3-test/evidence/1422-2026-11-18/` (run 2 in
`run2/`).
