# T3 question bridge (slice 2, PR 2b)

An optional Python process beside T3 connects outbound to the detached Inbox.
It supports only protocol-2 native user-input requests in explicitly allowlisted
pilot threads. It never creates threads, dispatches ordinary messages, executes
shell commands, prepares worktrees or connects Maestro. The LAN backend stays on
CT119; the real vault is not used. Nothing starts automatically on merge.

## Private operator configuration

Install Python 3.11+ and the pinned dependency in a dedicated environment:

```sh
python3 -m venv /opt/tessera-bridge/venv
/opt/tessera-bridge/venv/bin/pip install --require-hashes --only-binary=:all: -r inbox/bridge/requirements.txt
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
/opt/tessera-bridge/venv/bin/python inbox/bridge/t3_questions.py --config /private/config.json
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
