# Linked Maestro work — observation contract

P1 [issue152](https://git.oklabs.uk/BeFeast/okilum/issues/152). Backend implementation
contract; native integration and live alpha delivery have separate acceptance.
This is **linked existing work**, not a complete Maestro execution adapter.
Okilum still owns goals and outcome criteria; Maestro owns its queue, worker
attempts and PR execution. Linking invokes no Maestro command and creates no
Okilum stage, StartEnvelope, ResultRecord or criterion evaluation.

## Configuration and capabilities

The existing connector settings accept optional `maestro`:

```json
{
  "base_url": "http://maestro.example.test:8786",
  "instance_id": "01000000-0000-4000-8000-000000000151",
  "token_env": null,
  "ui_origin": "https://maestro-ui.example.test"
}
```

`base_url` must be an explicit HTTP(S) origin, without a path, userinfo, query or
fragment. `instance_id` is a stable configured UUID. `token_env`, when present,
uses the existing credential-reference resolver (environment variable or file
reference outside the brain). No credential values enter records. Select an
origin reachable from the backend; original issue/PR URLs remain the provider's
HTTP(S) links. A loopback origin is not automatically a desktop deep link.

`ui_origin` is an optional, explicit, nonsecret HTTP(S) origin reachable from the
desktop. It is independent of the backend API origin and excluded from provider
identity. There is no automatic host rewrite. Maestro's fleet source defines
project `dashboard_url` as `/project/<name>` and approval `dashboard_url` as
`/approvals?id=<approval ID>`; legacy configured project dashboards are ignored by
Maestro. Only those original routes, exactly matching the selected project or
approval identity, are resolved against `ui_origin`. Missing configuration,
mismatched routes, credentials or another origin yield unavailable/null UI links;
original issue/PR links remain available. Changing the UI origin does not create
new canonical observations.

Capabilities include `maestro_observation`, `maestro_link` and
`maestro_control:false`. The existing Connections Check performs GET-only
Maestro discovery and returns choices under `choices.maestro`. Configuration
cannot remove/change the instance or origin of any active link; unlink first.
Changing credentials for the same instance does not change linked identities.

The client only issues GET `/api/v1/fleet`. Redirects are refused, including
same-origin redirects, rather than following a changed target. Connect timeout
is 3 seconds, total timeout 5 seconds, response limit 16 MiB, maximum 256 projects and
8192 workers/approvals, 256 attempts and 256 KiB normalized payload per issue.
Missing stable project IDs are counted as unsupported. Nothing is silently chosen
by title. Missing attempt generation/start identity cannot appear as proven live
work. Every historical or live worker requires a matching repository identity;
a missing/mismatched repository is refused rather than attached to another project. Full logs and private execution paths are not fetched or copied.

## Native application API

These are existing versioned local JSON-lines service commands, with the normal
`schema:"ai-brain/v1"`, request `id` and `expected_workspace` envelope. All goal
ownership is explicit; commands never use the currently selected desktop goal.

### Discover

```json
{"op":"maestro_discover"}
```

Reply has `schema:"okilum-maestro-observation/v1"`, `instance`, `observed_at`,
`refreshed_at`, `unsupported_projects`, `controls_enabled:false` and `projects`.
Each supported project has `project_id`, `name`, `repo`, `paused`, optional
`dashboard_url`, `stale` and
`issues:[{issue,selection_guard}]`. An issue contains number/title/URL, normalized
attempts and read-only approval summaries. Approvals include an optional
`dashboard_url` bound to the approval's exact ID. The first picker supports only issues
already represented by Maestro's snapshot, not arbitrary issue search.

`selection_guard` hashes the configured instance, exact project identity and
selected issue projection. It excludes unrelated projects and telemetry counters.
Missing/stale targets cannot be linked. Discovery does not retain a link, modify
Markdown or change any provider state.

### Link existing work

```json
{
  "op": "maestro_link",
  "operation_id": "02000000-0000-4000-8000-000000000152",
  "goal_id": "03000000-0000-4000-8000-000000000152",
  "selection_guard": "sha256:the-exact-discovery-guard",
  "project_id": "04000000-0000-4000-8000-000000000152",
  "project_name": "example-project",
  "repo": "example/project",
  "issue_number": 42
}
```

The backend fetches a fresh snapshot outside its owner mutex, then revalidates
configuration, the goal's durable link history and the selected guard under that
mutex before the local commit. A link+unlink completed while the GET was pending
invalidates that late request; it cannot resurrect observation. Exact operation
receipts are still replayed before creating any new link.
A changed selection returns an error and requires rediscovery; it never attaches
another row/issue. Only one active link per goal is allowed. Link operation IDs
are durable: an exact retry returns the original receipt even while Maestro is
offline; the same ID with changed input is refused.

Receipt: `{operation_id,link_id,goal_id,linked:true}`. The receipt acknowledges a
local link, not acceptance of remote work. The initial semantic observation is
retained through the same existing source/journal mechanism; an interrupted
canonical write remains recoverable and cannot authorize a remote command.

### Inspect and stop observing

```json
{"op":"maestro_get","goal_id":"03000000-0000-4000-8000-000000000152"}
```

Reply: `{schema,goal_id,link,history,controls_enabled:false,recovery_required,source_paths}`.
`source_paths` contains the active `link` record path and an `observations` mapping
from observation IDs to canonical record paths.
`link` is the active link or null; history retains earlier inactive links. A link
contains exact instance/project/issue identity, `status`, last successful local
and remote observation times, optional error, immutable observation IDs and
latest projection and optional `project_url` for Open in Maestro. Source snapshots
also expose this as `maestro`. A null URL means that action is unavailable; the
native client must not invent a URL from a backend loopback origin.

```json
{
  "op":"maestro_unlink",
  "operation_id":"05000000-0000-4000-8000-000000000152",
  "goal_id":"03000000-0000-4000-8000-000000000152",
  "expected_link_id":"06000000-0000-4000-8000-000000000152"
}
```

Receipt: `{operation_id,link_id,goal_id,unlinked:true}`. This stops local observation
and preserves prior canonical/operational history. It never stops the worker,
pauses the Maestro project or removes a provider label. Exact retries are stable;
wrong-goal/stale-link operations fail.

## Observation, evidence and restart

[Use a saved observation in Context](ai-brain-maestro-observation-context.md)
retains exact historical evidence, including inactive links, through explicit
Source staging and subsequent Build/Review.

A dedicated backend observer polls every 15 seconds, independently of desktop
connections. It captures settings and active link identities under the owner
mutex, releases it for HTTP/credential resolution, then reacquires it to apply a
matching response. Relink, unlink or settings replacement invalidates the old
response. One fleet response serves all active links for the configured instance.

The provider supplies snapshots, not a replay cursor. Only newer provider snapshot
times can update evidence. Repeated snapshots still update local connection and
freshness state, so an unchanged provider timestamp cannot hide stale data. Reused slots/new generations are retained as different
attempt evidence. Snapshot errors, missing/mismatched project identity, stale data
or disappearance mean unknown/disconnected with previous evidence retained, never
not_started/succeeded. Last successful observation remains inspectable.

A semantic change is keyed by exact link, paused state and normalized issue/attempt/
approval state. Provider age/runtime counters and changing explanation/summary
prose do not create new observations or alerts. Observation IDs are deterministic
UUIDs derived from link identity, a durable transition sequence and semantic
identity. Consecutive identical snapshots/restarts do not append duplicate
knowledge; A→B→A creates a new transition and unseen Attention item even when the
earlier A was acknowledged. Repeated connection failures similarly share one
occurrence until recovery, after which a recurrence receives a new occurrence.
The journal caps links at 256, link operations at 2048,
observations per link at 512 and total retained observation journal at 32 MiB.
Capacity failure preserves previous evidence and is visible as a retention error.

Canonical records use existing managed revision-aware source transactions:
`maestro-link-<uuid>.md` and `maestro-observation-<uuid>.md` under the configured
record directory. Poll timestamps/current network errors remain operational.
Saved observations contain source links and structured attempt status with
`verification:unverified`; transient provider explanations and approval proposal
prose are omitted from canonical observations. A goal's original stages, criteria
and task authority remain unchanged. An observed landed PR is not verified goal
completion; no exact commit/build claim is manufactured from a worker status.

Attention shows current connection blockers, worker blockers, read-only approval
decisions and observed outcomes. Local Attention acknowledgement is not a Maestro
approval. Original approvals are opened in Maestro; the connector has no decision
or control methods. Maestro may change proposal content under the same approval
ID without exposing a payload revision. Updated summary/freshness remain visible
in the operational projection, but this connector does not claim revision-aware
proposal notifications or persist that prose as canonical evidence. Routine work remains available in the linked history.

## Recovery and compatible fallback

The Runner journal retains links, local operation receipts, observation identities
and pending canonical writes outside all disposable indexes. A separate enrollment
marker and canonical receipt inventory detect a binary that discarded these new
journal fields. Such a journal is reported as requiring recovery, not an empty
workspace that can silently relink/recreate work.

After first link enrollment, rollback requires a same-schema maintenance build
that preserves these records. Its only feature-disable change is
`maestro_links::NEW_LINKS_ENABLED=false`: existing observations/history, unlink,
GET reconciliation, settings and source journal behavior remain supported. An
older executable unaware of the new journal/settings is not a compatible fallback.
Preparing/verifying that build precedes any live alpha enrollment.

No live Maestro action is necessary for fixture testing or deployment acceptance.
The Okilum Maestro row remains paused. Full Maestro stage dispatch, context
transfer, start idempotency, cancellation and guarded approval control remain
separate work. The current Maestro approval API must gain an atomic expected
payload/revision guard before Okilum can promise approval of the exact displayed
proposal; a stable approval ID alone is insufficient.

A fixture verifier checks the actual enabled and maintenance executables against
one temporary brain, including saved settings, offline operation replay, continued
GET observation, disabled new enrollment and unlink/restart. It uses no live
provider or installed service:

```bash
python3 scripts/verify-maestro-maintenance.py --enabled /path/to/enabled/okilum-cored --maintenance /path/to/maintenance/okilum-cored
```
