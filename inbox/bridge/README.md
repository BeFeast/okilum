# T3 question and explicit launch bridge

An optional Python process beside T3 connects outbound to the detached Inbox.
It supports only protocol-2 native user-input requests in explicitly allowlisted
pilot threads. An independently enabled launch target permits explicit native
executor launches. It never executes shell commands, prepares worktrees itself
or connects Maestro. The LAN backend stays on
CT119; the real vault is not used. Nothing starts automatically on merge.

## Private operator configuration

Install Python 3.11+ and the pinned dependency in a dedicated environment:

```sh
python3 -m venv /opt/okilum-bridge/venv
/opt/okilum-bridge/venv/bin/pip install --require-hashes --only-binary=:all: -r inbox/bridge/requirements.txt
```

Create a mode-0700 state/config directory, mode-0600 config and two separate
mode-0600 JSON credential files containing `{"token":"…"}`. Provision the Inbox
credential with the scope described in `../README.md`; its project must exist.
T3's token currently has broader native authority, so it stays only beside T3,
never on CT119 or in the image. The adapter allowlist is an application boundary,
not a claim of server-enforced T3 token scopes. Start with the isolated pilot only.

```json
{
  "instance_id": "<configured persistent T3 instance identity>",
  "source_project_id": "<isolated T3 project ID>",
  "project_id": "<Inbox project UUID>",
  "thread_ids": ["<explicit pilot thread ID>"],
  "t3_url": "http://127.0.0.1:<T3 port>",
  "inbox_url": "https://inbox-qa.oklabs.uk",
  "t3_credential_file": "/private/t3.json",
  "inbox_credential_file": "/private/inbox.json",
  "state_file": "/private/state/bridge.db"
}
```

URLs come only from operator configuration; redirects and environment proxies
are disabled. Inbox requires HTTPS, T3 permits HTTP only on loopback. Credentials
are absent from output and durable operation payloads. Config/SQLite must be
included in an app-consistent private backup, separately from derived caches.

```sh
/opt/okilum-bridge/venv/bin/python inbox/bridge/t3_questions.py --config /private/config.json
```

`--once` performs observation/recovery only. A process lock prevents two instances
from using one journal. Do not run independent journals for the same pilot scope.
Rotation requires replacing credential files and restarting. No service unit or
live credential is installed by this PR.

## Delivery and recovery

The displayed revision hashes source identity, fields and response capability.
Native option values (including whitespace), single/multiple choices and custom
text are preserved; combining custom text and options is rejected. The current
pilot rejects optional questions and does not support answer attachments.
Fresh source identity/revision/capability are checked immediately before dispatch.
Local SQLite stores the exact immutable intent and deterministic command ID;
Inbox commits `uncertain` before source I/O. Transport never retries a command.
Lost acknowledgements reconcile the original request and exact native answers.

T3 currently persists `resolved` **before** its provider callback executes (see
`RuntimeRequestServiceV2.respond`). Thus both a command receipt and matching
resolved answers mean **accepted**, not delivered. This adapter never claims
provider receipt or completion from them. A pilot must additionally observe the
original executor's answer/result; a general durable provider-delivery signal
requires a source contract extension before that stronger UI status is enabled.

Every process start quarantines already queued operations as `uncertain` without
sending them. An older backup is not permission to reissue work. Keep unresolved
operations and reconcile from the source; do not delete their local journal or
create replacement IDs as recovery. Expired/missing requests after an attempted
send cannot prove rejection and remain uncertain.

Initial pilot limit: up to 20 explicit threads, complete bounded snapshots only.
Truncated/malformed snapshots stop the synchronization step; they never imply
withdrawal. Native cancelled/expired records do. Source cursor regression or
changed journal binding requires operator investigation, not automatic reset.
Older history traversal, thread discovery and source liveness in the web UI remain
follow-up work before expanding beyond the bounded pilot. The web question view
and deployed end-to-end pilot are the next PR; this adapter alone is not slice-2
acceptance.

Tests: `python3 -m unittest discover -s inbox/bridge -p 'test_*.py' -v`.


## Explicit launches

Copy `t3_launch.py` beside `t3_questions.py`. To enable launching, add
`launch_targets` to the private local config: an array of exact target snapshots
returned by the authenticated Inbox target endpoint. Each contains `project_id`,
`instance_id`, `source_project_id` and `target`; every field is checked against
operator configuration before source I/O. Keep the native T3 credential local.

T3 `orchestration.launchThread` receives durable command/thread/message IDs,
exact brief and pinned base commit, with app-owned worktree preparation. This is
not the non-idempotent high-level MCP launch wrapper. The bridge journals consent
and commits Inbox uncertainty before calling T3 once. A receipt means accepted;
only a matching source thread/message/run/worktree observation advances progress.
The launch branch is `inbox-<operation UUID>`; the resulting path comes from T3.

At startup, already queued work is quarantined. Lost acknowledgements, restart,
restore and missing source records trigger read-only reconciliation, never an
automatic replacement launch. A late preparation result may fill previously
unknown run/path fields; established identity cannot change. Large/truncated source
snapshots remain unconfirmed. Launches do not broaden the question thread allowlist;
question discovery for newly launched threads is a separate follow-up.

## Maestro adapter (#601)

`maestro.py` is a separate, disabled-by-default process. It never imports a T3
runtime token, starts workers, edits fleet configuration or connects automatically
on Inbox deploy. The reviewed source contract is `BeFeast/maestro` PR #1289,
commit `0812fd6159cf19b84fa84719aaafe84341c44f45`, `docs/inbox-bridge.md`.

Provision a private configuration and **two different scoped credentials** only
for an isolated pilot after the separately agreed Maestro deployment window:

```json
{
  "instance_id": "<durable identity returned by Maestro>",
  "source_project_id": "<stable Maestro project ID>",
  "project_id": "<Inbox project UUID>",
  "approval_actions": ["merge_pr"],
  "maestro_url": "https://operator-configured-maestro-endpoint",
  "inbox_url": "https://inbox-qa.oklabs.uk",
  "maestro_credential_file": "/private/maestro.json",
  "inbox_credential_file": "/private/inbox-maestro.json",
  "state_file": "/private/state/maestro.db"
}
```

Credential files contain `{"token":"…"}`, mode 0600; the state directory is 0700.
The source token principal must remain stable across rotation and have only this
project's `read`, `reply`, and explicit `approve:<action>`/`reject:<action>` grants.
The Inbox token belongs to its independent `maestro` scope. HTTP is allowed only
for a loopback source; other connections require HTTPS, without redirects/proxies.

Run `python3 inbox/bridge/maestro.py --config /private/config.json`. `--once`
observes/reconciles only; initial queued operations are never automatically sent.
A process lock prevents two processes sharing a journal. Never provision separate
journals for the same scope; include this durable SQLite DB in private backups.
Restore or missing operation receipts are not permission to send again.

Complete snapshots refresh source liveness. Truncated question snapshots rebuild
from the durable change feed from cursor zero, in bounded pages; malformed pages,
cursor regression, and instance changes fail closed. Missing items are not marked
withdrawn; their Inbox observation expires. Approval snapshots are re-read before
sending. The displayed consent revision wraps the exact opaque native revision and
capabilities, so permission changes invalidate a choice even without a native edit.
Only the original native revision is sent to Maestro's atomic guard.

The immutable request and native operation ID are saved before I/O. Inbox marks it
uncertain before the source POST; there are no transport retries. Lost responses
use principal-scoped operation lookup with the original ID. Returned identities,
answer/decision and approval target/hash are checked before accepting the receipt.
A question is only delivered after the worker's durable acknowledgement. A delivered
approval receipt means the decision was saved, never that an external action ran.
Rejected source revisions retain their exact operation receipt and require new
explicit consent; text answers never become approval decisions.

Tests use contract fixtures and a loopback HTTP fixture with a durable SQLite
receipt committed before a dropped response. They do not start Maestro or touch
any native worker/fleet. `test_maestro.py`, existing bridge tests, and the browser
`web/inbox/qa/maestro.mjs` exercise the isolated paths; live acceptance is a separate
gate requiring a manager-coordinated deployment window. The Rust `bridge_api`
integration test additionally runs this Python adapter against a real HTTP Inbox
and SQLite Store, recovers question/approval receipts after a journal restart,
and verifies one source dispatch per operation.

For Maestro `change_global_config` only, a native absent/null approval target means
an explicit global target. The adapter represents it as `{"scope":"global"}` in
Inbox and applies the same normalization when validating source receipts. Native
revision/hash and the outbound decision body remain unchanged. Null/missing
scoped targets, scalar/list targets, and mismatching receipt targets fail closed.

The native-worker follow-up is design-first in
[the #728 pilot contract and runbook](../../docs/maestro-native-inbox-pilot.md).
The accepted #601 dedicated consumer is distinct from an actual contained
harness worker. Endpoint admission, per-attempt credentials, durable native
consumption and scoped revocation require Maestro-owner implementation and a
separately approved maintenance window; this adapter does not provision them.
