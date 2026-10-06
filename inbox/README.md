# Optional Inbox — slice 1 (#465)

This is an independent Cargo workspace so the Inbox server does not expand the
Reader dependency graph, root lockfile, or required workspace lint/test job.
It does not depend on `tessera-brain`, GPUI, or the Reader. The existing Reader
build and startup behavior are unchanged. Run commands from this directory:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

The API/auth step adds a loopback-only HTTP listener for the LAN TLS proxy.
The embedded mobile web shell supports passkey sign-in, capture and readback.
There is no AI provider call or vault write yet. Public exposure and
connection to a real vault remain owner-gated.

## Local administration and LAN deployment boundary

```sh
mkdir -m 700 /path/to/private-inbox-data
cargo run --locked -p tessera-inboxd -- bootstrap \
  --data-dir /path/to/private-inbox-data --origin https://inbox-qa.example.test
cargo run --locked -p tessera-inboxd -- serve \
  --data-dir /path/to/private-inbox-data --origin https://inbox-qa.example.test
```

`bootstrap` prints a one-time enrollment URL with a secret in its **fragment**.
Treat that output as a credential; do not pipe it into logs. The digest expires
in ten minutes and is consumed atomically when a verified passkey is saved.
Rotating bootstrap invalidates unfinished enrollments. Once enrolled, this command
refuses to replace the owner. No public signup, reset endpoint or password exists.
The web shell clears the fragment immediately and performs the WebAuthn browser ceremony.

The stable HTTPS origin and owner ID persist in SQLite. Changing the origin fails
explicitly; it cannot silently rebind a passkey to another RP. One server process
owns the directory lock. WebAuthn challenges last five minutes, are single-use and
remain only in memory. Sessions last seven days and are also memory-only; logout
revokes one session, and server restart revokes all sessions and pending challenges.
Captures and public passkey credentials survive restart. No private key or raw
bootstrap/session token is stored in SQLite. Only one initial passkey is supported
in this step; additional enrollment and operator-assisted recovery are deferred.

Deployment for this slice is **LAN QA only** in a dedicated development LXC on
DevBox, using Docker Compose; the proposed hostname is `inbox-qa.oklabs.uk`.
The backend must not be deployed on maestro. Production on Mimir is a later step.
The server-side connector will use a small fixture vault on the same dev server;
real-vault replication belongs to the separate sync track. NPM requires a separately
prepared restricted route to the loopback service; this PR provisions no LXC,
proxy, DNS, tunnel, fixture vault or firewall. Compose networking must retain the
loopback-only daemon boundary (for example, a proxy sharing its network namespace),
not change its listener to `0.0.0.0` as a shortcut. Use private persistent storage
outside the fixture vault for the database; no Reader dependency on this service.

## HTTP contract

All `/api/v1` reads require a session except WebAuthn ceremony endpoints. Every
POST also requires the exact configured `Origin`; JSON endpoints require JSON.
Cookies use `__Host-` names, Secure, HttpOnly, SameSite=Strict, Path=/ and no Domain.
Responses are no-store; no CORS permissions, token URL parameters or body logging.
Ceremony starts are limited globally to 30/minute with bounded in-memory flows.
The worker validates user verification, RP/origin and challenge through pinned
`webauthn-rs`, not a custom signature/login implementation.

- `POST auth/register/start {token}` → WebAuthn creation options + flow cookie;
  `POST auth/register/finish` takes the browser registration credential.
- `POST auth/login/start` → WebAuthn request options + flow cookie;
  `POST auth/login/finish` takes the browser assertion; success sets session cookie.
- `GET session` returns the authenticated owner; `POST auth/logout` revokes it.
- `POST items {operation_id,item_id,text}` captures exact text. The session supplies
  owner identity, and unknown body fields are rejected. Changed replay → 409.
- `GET items?after=0&limit=50` returns capture rows and a fixed `through` boundary;
  use `after=next_after&through=…` for subsequent pages, then omit `through` to
  catch new arrivals. `GET items/{id}` returns one item or 404.

HTTP middleware and SQLite calls run with a bounded body limit; blocking database
and WebAuthn work runs off the async reactor. Server errors never return provider,
credential or SQL details. There are no mutable-item or provider-action routes yet.

## Storage contract

`Store` is canonical Inbox data, not a search cache. The caller must provide a
private durable directory outside any vault. SQLite uses WAL, FULL synchronous
commits, foreign keys and a bounded busy timeout. Unknown schemas and corrupt
files fail explicitly; they are never reset to an empty Inbox.

The transport supplies the authenticated `OwnerId`; capture bodies do not choose
an owner. Within that owner, an operation UUID binds an item UUID and the exact
UTF-8 capture text. Replaying the same request returns the original timestamp and
item; changed content or identity conflicts. Item identity cannot be reused under
a different operation. Capture and operation are one transaction; there are no
external effects. Whitespace is validated but never normalized in stored text.

A paginated capture feed fixes an upper sequence boundary so captures arriving
during pagination can be read by the next request without skipping earlier rows.
This is append-only **capture** pagination, not a mutable item/change-log API.
Before adding edits or deletions, introduce transactional change events and
snapshot semantics. Goals, stages and execution orchestration are out of scope.

## Checks

Tests cover a lost response followed by process/store reopen, concurrent replay
on independent SQLite connections, same-key/different-content conflicts, owner
isolation, injected failure between the two inserts, stable pagination while new
captures arrive, invalid input and non-destructive schema/corruption errors.
The dedicated path-filtered `inbox-ci` job is non-blocking for Reader development;
Inbox PRs must pass it before merge.

## Mobile web and offline capture

`web/inbox` is a dependency-free runtime shell embedded in the daemon. After the
first verified sign-in, IndexedDB retains unsent captures and the last verified
owner. A capture is acknowledged locally only after its transaction commits.
Reconnect retries preserve operation/item IDs; only an exact server acknowledgement
removes the row. Auth errors and lost responses keep it; conflicts retain the text
for export without automatic retry. Multiple tabs reuse server idempotency.

The service worker caches an explicit shell allowlist, never API/session replies.
Previously synced items are fetched after sign-in, not retained for offline reading.
Browser data clearing can destroy unsent captures: export is available, including
before sign-out, and the UI states this limitation. Signing out clears remembered
identity, while retaining unsent rows for the same owner after the next sign-in.
No authentication secret is placed in local storage. Text is rendered literally.

Run `npm ci --ignore-scripts && npm test` in `web/inbox` for outbox persistence,
replay/conflict/ownership, WebAuthn encoding and service-worker boundary tests.
Static desktop/mobile preview checks layout only. Real phone/Mac passkey login,
installed-PWA offline capture and reconnect remain LAN HTTPS acceptance checks;
this intermediate PR is not the complete slice-1 demo.

## AI discussion (CLIProxyAPI)

Optional server flags: `--ai-endpoint https://proxy.example.test/v1/chat/completions
--ai-model MODEL --ai-credential-file /run/credentials/cliproxy-key`. The credential
file must be private and regular; provision through Infisical/systemd credentials,
not a repository file, DB setting or command-line key value. Without these flags,
capture/readback works and discussion returns an explicit unavailable response.

`GET/POST items/{id}/discussion` uses the same session/Origin boundary. POST accepts
`{operation_id,text}`; a transaction binds it to the selected item before any call.
An exact replay returns the existing turn without another provider request; changed
content conflicts. Only the original thought, completed exchanges and this question
are sent. No tools or vault data. The context is bounded to 100 turns /128 KiB;
limits fail explicitly instead of silently dropping history. One running call per
item and four globally; transport timeout 90 seconds, bounded response, no redirects
or application retries. Provider errors expose no raw body, endpoint or credential.

A background task saves the answer even if the browser closes. A server restart,
truncated reply or unknown provider outcome leaves `uncertain`; asking again is an
explicit new operation, never a hidden resend. The browser persists an unacknowledged
question identity before POST and can check/retry that same operation after reconnect.
An explicit Forget local retry action releases a rejected/stuck local intent while
keeping its text in the editor; it warns that server work is not cancelled and a
subsequent send creates a new request.
AI output remains a draft: this PR adds no publication authority or vault writes.
Deployment/backup constraints are in [deploy/PLAN.md](deploy/PLAN.md).

## Fixture publication and LAN Compose

With `--fixture-vault /path --vault-folder Projects ...`, the Linux daemon exposes
`GET destinations` and `GET/POST items/{id}/publications`. Each allowed folder is one
explicit direct child of the configured root. POST contains
`{operation_id,folder,filename,content}`; exact payload is durable before disk effects.
The web editor previews those bytes and asks before creating the Markdown file.
No original capture is changed or deleted. AI output alone cannot invoke publication.

The connector pins directories with no-follow descriptors, rejects traversal,
symlink directories/files and non-Markdown destinations, and never overwrites.
A hidden staging file is fully written/fsynced, journalled, then hard-linked
exclusively to the destination. After a lost acknowledgement, staging inode plus
exact bytes distinguish this operation from an unrelated file; identical content
under a different operation still conflicts. Published replay verifies bytes and
never recreates a deleted/edited file. A partial private stage is recreated only while its intent is still queued and it
has no other hard links. Missing/prepared stages or changed destinations conflict
explicitly; they never authorize an overwrite.
The retained publication record binds item, chosen destination and exact draft.

[Deployment plan](deploy/PLAN.md) describes the dedicated DevBox LXC, Compose,
fixture volumes, enrollment and consistent SQLite pre-PBS backup. Reader remains
local-only; real vault and public access require separate approval.

HTTP diagnostics emit one JSON line per request: generated request ID, normalized
method, static matched route template, status, duration and a fixed error code.
The same ID is returned as `X-Request-ID`. Bodies, query strings, raw paths/IDs,
headers, cookies, Origin values and provider credentials are never logged.

Publication names may include up to ten relative components within the selected
PARA root. Missing intermediate folders are created with no-follow traversal.
The editor suggests a name from the draft heading and adds `.md` when omitted.
Known first-attempt collisions are durable and terminal; choosing another name
creates a fresh operation and preserves the draft. Legacy prepared operations
with an unrelated target are shown as occupied with earlier delivery uncertain,
not retroactively claimed to have failed. Forget hides a conflict on this device;
it never deletes server history or vault files.

## Execution foundation (slice 2, PR 1a)

Authenticated same-origin draft endpoints:

- `GET/POST /api/v1/projects`: list/save project metadata. Listing accepts `after`
  (last UUID) and `limit` (1–100). Continue using `next_after` until an empty page;
  this is a live metadata list, not the append-only capture change feed.
- `GET /api/v1/projects/{id}`: current owner-scoped project.
- `POST /api/v1/briefs`: save a new immutable brief revision.
- `GET /api/v1/briefs/{id}/revisions/{revision}`: read exact saved draft bytes.

Saves contain operation UUID and expected revision (zero for creation). A stale
edit returns 409 with the current object; same-ID exact replay returns its original
response even after later edits. Brief identity stays in its original project.
An opaque `target_id` on a draft grants no authority: this foundation has no launch,
question ingest or dispatch endpoints and makes no source requests. Configured
source authorization and durable answer/launch operations follow separately.
Schema 6 stores project state, immutable brief history and mutation replay results
outside the vault/cache. Existing capture/discussion/publication data are retained.

### Question/reply journal (slice 2, PR 1b)

Trusted source adapters can observe a question with a monotonically increasing
observation sequence; opaque source revisions are never treated as numeric cursors.
The source identity and project cannot be rebound. Browser sessions can read
`GET /projects/{id}/questions`, `GET /questions/{id}` and
`GET /reply-operations/{id}` under `/api/v1`, and reserve an exact answer through
`POST /questions/{id}/reply`. The request contains operation/question UUIDs,
expected source revision, and answers keyed by the advertised field IDs. The
same session/Origin guards apply as for capture. No browser ingest or delivery
acknowledgement endpoint exists.

A reply atomically stores the displayed question and source identity plus exact
answers. A second operation cannot answer a reserved question; exact replay
returns the saved operation even after source revision changes. Delivery states
are queued, uncertain, accepted, delivered and rejected. Internal bridge code
must persist uncertain before I/O and reconcile through source identity after
restart; it cannot reset to queued. Only a definite source rejection releases a
reservation. A delivered reply stays reserved and does not imply completed work.
Question pages are a live owner/project-scoped view, not a source change feed;
absence on a page is never a withdrawal. Adapter authentication, durable cursor
integration, network dispatch and the web question UI are the next PR.

## Scoped bridge transport (slice 2, PR 2a)

The optional machine API is disabled unless `serve --bridge-credential-file PATH`
is supplied. Provision a regular mode-0600 JSON file outside the repo/image; never
put its contents in command arguments, logs or browser configuration:

```json
{
  "token": "<64 hex characters from 32 random bytes>",
  "scope": {
    "owner_id": "<existing Inbox owner UUID>",
    "project_id": "<existing Inbox project UUID>",
    "instance_id": "<configured T3 instance identity>",
    "source_project_id": "<isolated T3 project identity>",
    "ingest": true,
    "replies": true
  }
}
```

The owner must match this database. The configured project must already exist
before ingest. Browser project edits do not create or extend this permission.
This credential permits questions/reply reconciliation only, never executor
launch, shell/workspace operations, ordinary session APIs or Maestro access.
Rotate by replacing the external file and restarting; the old token stops working.
The current Compose deployment does not enable this transport.

Requests use `Authorization: Bearer …` over the LAN HTTPS endpoint, without
Origin or Cookie headers. Browser session credentials are not accepted here;
normal browser mutation routes continue to require session + matching Origin.
Payloads cannot select owner, source instance or source project authority.

- `POST /api/bridge/v1/questions`: `{question, sequence}`; scope checked before
  ingest, monotonic source observations and immutable identity enforced by store.
- `GET /api/bridge/v1/replies?after=0&limit=50`: scoped insertion-order pages,
  `{operations, next_cursor, has_more}`; maximum 100. Includes terminal operations.
- `GET /api/bridge/v1/replies/{operation_id}`: current exact intent and state;
  out-of-scope operations return the same 404 as missing operations.
- `POST /api/bridge/v1/replies/{operation_id}`:
  `{expected, next, delivery_id?, error_code?}`; guarded delivery transition,
  exact result replay and no transition back to queued. Invalid transitions: 409.

The list cursor is **not** a change-feed cursor. Retain unresolved operation IDs
and use individual lookups; replay a full scan after restart/restore. A missing
page/record never authorizes another external command. Before source I/O, persist
`queued → uncertain` and retain the original deterministic source command ID.
Only definite source evidence may advance to accepted/delivered/rejected; an
HTTP acknowledgement is not proof that the original executor consumed an answer.
This PR adds no source adapter or dispatcher. Source reconciliation, local bridge
recovery, question freshness and the web answer surface follow in PR 2b.

## Web question answers (slice 2, PR 2c)

Signed-in users see Questions for you, grouped by existing execution projects.
Opening a question is read-only. Sending is explicit and online-only: source
observations expire after 30 seconds on the server, and the open form requires a
fresh check after 20 seconds. Schema 8 timestamps trusted observations; migrated
questions start stale. Exact operation replay remains available after expiry.

The web client saves the exact consent payload under the authenticated owner in
local storage before sending. Reload/Check status reads that operation; an
unconfirmed send may only retry its saved ID and payload. It never uses the offline
capture outbox. Storage failure prevents sending. Refused requests can be cleared
explicitly while keeping the answer text; accepted/uncertain operations cannot be
cleared into a new send. Accepted is distinct from executor receipt.

The source bridge remains opt-in. This PR adds the web surface and freshness gate;
CT119 deployment and an actual T3 pilot must validate the combined flow before
claiming slice-2 acceptance. Maestro stays disconnected.

## Explicit T3 executor launch (slice 2, PR 3)

Schema 9 adds immutable launch operations. Browser clients save a brief, inspect
its exact revision and operator-provisioned target, then explicitly POST
`/api/v1/launches` with operation ID, brief ID/revision and target fingerprint.
Saving a brief never launches anything. The preview shows repository, pinned
commit, new-worktree policy, model/options and modes. Launches are online-only;
uncertain actions remain visible and never enter the capture outbox.

Targets are disabled by default. The private bridge credential may additionally
set `scope.launches: true` and `launch_targets: [Target]`, where Target contains
`id`, `label`, `repository`, `base_commit` (40-character lowercase commit),
`model_selection` (native instanceId/model/options), `runtime_mode` and
`interaction_mode`. Scope fixes owner, Inbox project and source project. Provision
only the isolated pilot; do not grant authority to Tessera development or a real
vault. GET `/api/v1/projects/{id}/launch-targets` returns immutable snapshots and
fingerprints. The bridge's separate local configuration must allow those exact
snapshots too; backend configuration alone cannot broaden native execution.

GET `/api/v1/launches/{operation_id}` and the paginated project launch list retain
thread, run and worktree identity through reloads. Exact request replay survives
target revocation; changed payloads and a second launch of the same brief revision
conflict. Machine `/api/bridge/v1/launches` supports scoped list/lookup/progress,
not creation. The browser's session/Origin protections remain mandatory. Retain
both server DB and bridge journal in consistent backups; do not erase uncertain
operations or create new IDs to recover a missing acknowledgement.


## Forgejo read-only overview (slice 2, PR 4)

The optional [collector](forgejo/README.md) projects accessible repositories,
open issues/PRs, commit status attempts and published releases into a replaceable
cache outside the vault. Configure a dedicated read-only credential and explicit
project/repository associations; no source token reaches the browser or Inbox API.
`GET /api/v1/forgejo` serves authenticated observations and computed freshness;
`?project=UUID` filters linked repositories and attaches separately identified
executor launches. This does not turn assignees or a launch's base commit into
claims about who authored a PR or the executor's current HEAD. The service stays
disabled until operator configuration is supplied; Maestro is unaffected.

## Project and results screen (slice 2, PR 5)

The responsive Project panel combines guarded status/next-step edits, source
questions (the existing reply dialog), executor identities/state/final output,
explicitly mapped Forgejo repositories, and publication/QA history. Assignees and
executors remain separate. Independent source errors retain observations with a
stale/unavailable label. The fixture pilot is not mapped to Tessera development.

Schema 10 adds durable `execution_outputs` and `execution_results`. The scoped
launch bridge stores the final non-streaming assistant message from the exact
completed run via `POST /api/bridge/v1/launches/{id}/output`; identity/content are
immutable and exact retries survive lost acknowledgements and restart. Browser
reads require the owner's session. This does not relaunch work or infer a build
from generated prose. T3 thread IDs/worktrees are displayed; a browser deep link
is not fabricated where no operator-provisioned public source route exists.

`POST /api/v1/results` records an explicitly **owner-reported** publication (or
failure), linked to a terminal launch, exact run and full commit. HTTPS result
link, platform, channel, version and QA text are required. This endpoint does not
publish or verify external deployment. Operation IDs replay exact records;
changed payloads conflict. `GET /api/v1/projects/{id}/results` returns append-only
history. A later failed report does not replace the last published result for
that exact platform/channel. Client retry journals are owner/project scoped;
clearing a saved request keeps text and does not delete a committed record.

Back up schema-10 DB and the bridge journal using SQLite online backup. Image
rollback must retain newer operations; old binaries reject newer schema rather
than resetting it. Maestro and broader execution remain gated separately.

## Passkeys and devices (#575)

Settings → Passkeys & devices lists the owner's keys and their last use. Adding a
key here, creating an invitation, approving a device, and revocation require a
passkey confirmation within five minutes. A synchronized Bitwarden/iCloud key is
one credential: revoking it revokes every synchronized copy and its sessions.
The last credential cannot be removed; enroll another working key first.

Add another device displays a five-minute invitation link. The new device creates
its own key and displays a comparison code. The owner compares that code on both
devices and explicitly approves it. Until approval the key cannot log in. A link
alone never creates a session. Invitations and ceremonies are memory-only and
expire on restart; ask for a new link if interrupted. Do not put links in logs or
issue comments. The browser strips the fragment immediately and does not persist it.

Schema 11 migrates the existing credential without changing the owner or Inbox
contents. Existing sessions expire on deployment (as before); sign in with the
same passkey. Before deployment keep an online SQLite backup plus the old image
and source. Rolling back to schema 10 requires stopping the service and restoring
that database backup as well as the previous image; do not downgrade the schema
number or re-run bootstrap on an enrolled owner.

`qa/devices.mjs` in web/inbox verifies real WebAuthn with two independent CDP virtual
authenticators and an isolated backend. Supply QA_BACKEND, QA_ENROLLMENT_FILE and
QA_CDP_ENDPOINT. It forwards requests to that backend without mocking API results;
never use the live database. It captures Settings and enrollment at 390/1280 in
light/dark and verifies approval-before-login, revoked sessions and last-key refusal.

## Maestro transport foundation (#601, PR 1)

The existing bridge credential remains T3 by default. An optional `maestro` object
in that private credential file has its own `token` and `scope`, with
`source_kind: "maestro"`, a pinned source instance/project, and explicit
`approval_actions` (for example `["merge_pr"]`). Its token must differ from T3's.
Maestro scopes cannot launch T3 executors. No Maestro credential or process is
installed by this foundation; the adapter/UI and isolated contract pilot follow.
A Maestro-only deployment may use this scope as the top-level credential.

Maestro question identity includes `worker_id` and the original generation/thread.
Approvals set `source.record_kind: "approval"` and contain typed `approval` data:
exact action, structured target, summary, risk, payload hash and optional target
state hash. Their sole `decision` field permits only explicit `Approve`/`Reject`
choices; free text cannot be interpreted as approval. `stop_worker` is unsupported.
The stored consent snapshot includes these details: changing them under the same
revision is rejected; an existing operation always replays its original snapshot.
T3's persisted source identity JSON is unchanged. No database migration is needed.

Live Maestro deployment/restart/fleet configuration requires a separately agreed
window. This transport foundation is not evidence of live Maestro integration.
