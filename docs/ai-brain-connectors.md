# Saved alpha connectors and recovery

Ordinary Okilum exposes **Workspace → Connections** for CLIProxyAPI, Todoist and
T3. Enable the desired connector, enter its address and credential reference,
then **Check connections and fetch choices**. CLIProxyAPI returns its model
catalog; T3 returns existing projects and provider model choices. Selecting a
project also fills its environment; selecting a T3 model fills its provider
instance. Advanced fields remain visible for deployments without discovery.
**Save and connect** persists configuration and refreshes runtime adapters.
Returning to Project brain refreshes capabilities in the retained BrainView;
source and conversation editor entities are preserved.

Configuration is non-secret JSON under the backend operational directory,
`connector-settings.json`, schema `okilum-connectors/v1`. It stores the existing
`ApplicationConfig` plus Todoist's provider-reported account identity. Markdown,
exports and derived indexes do not contain these operational settings.

The existing `api_key_env`/`token_env` fields now accept:

- `NAME` or `env:NAME`: legacy backend process environment variable.
- `file:/absolute/path`: an existing external file containing one raw token.

Credential files are read as data, never evaluated or sourced as shell code. They
must be outside the canonical brain. **Reconnect saved settings** rereads file
references. Process environment values remain inherited; changing an external env
file does not change a running process's environment, so use a file reference for
refresh without backend restart. No secret-store platform is added. Tokens are
runtime-only; neither settings replies nor persisted configuration return values.

States distinguish unconfigured, configured (authentication untested), reachable,
disconnected, authentication-required and identity mismatch. Missing credentials
disable only that connector. Read/edit access and historical goals/results remain
available. Explicit discovery and account probes run outside the runner mutex;
slow/offline providers do not hold source access. Applying an asynchronous result
rechecks the previous configuration and provider identity before replacement.
Interrupted chats are marked during process recovery, and reconnect does not mark
an active chat interrupted or resend any prompt. Existing T3 operations reconcile
with their original identifiers after adapter replacement.

Todoist uses authenticated `GET /api/v1/user` to pin the provider-reported account
ID. The live route and nonempty string ID shape were verified read-only on
2026-09-06 using the existing credential reference. The probe uses the exact token
held by the runtime adapter: changing a credential file between reads cannot
approve one account and construct an adapter for another. A different account or
an unverifiable identity disables task operations. Restore the original account's
credential and reconnect. Merely retaining the same URL/account label is not
identity proof. Existing legacy work without a saved account pin must first verify
its original runtime credential; otherwise recovery fails closed.

Provider URL/account/project routing is deliberately pinned once any
goal owns external task, chat or stage history. This includes completed history,
so old task/thread origins cannot be rewritten by a global setting change. Initial
setup may be corrected before external work exists; credential references for the
same identity may be refreshed later. Model/provider-instance and mode selection
may change after all prepared/running/uncertain stages and chats are terminal
across every goal; this does not rewrite historical thread origins. To choose another established provider
target, use a separate managed workspace. Arbitrary account switching and history
migration are not claimed by this alpha.

`Check connections` reads `/models` for CLIProxyAPI and `/user` for Todoist. T3
uses existing authorization to obtain short-lived WebSocket tickets, then reads
`server.getConfig` and `orchestration.subscribeShell`; it creates no thread or
provider turn. Connector checks do not save configuration or imply successful task
execution. Unsupported discovery is explicit, with advanced field entry available.

## Ordinary backend preparation

`scripts/prepare-brain-service.py` emits a user-service definition, exact backend
argument manifest and installation/rollback plan into a chosen output directory.
It does not install/start/enable a service or edit a brain. The backend opens its
saved operational settings on startup and remains independent of the desktop.
The joined alpha installation chooses concrete brain/state roots, binary hashes,
credential references and persistent desktop forwarding before approval.

A remote desktop must reach the backend's loopback endpoint and the configured T3
browser origin through its reviewed persistent connection. Backend paths remain
backend identities, not local desktop file paths. No per-goal UUID entry or
one-off test launcher is part of ordinary use.

## Read-only T3 recovery preflight

The standalone `t3-recovery-preflight` example inspects an **existing** operational
root without opening a Runner. It reads `connector-settings.json` and `state.json`,
resolves only the candidate T3 credential reference, and uses the same discovery
as Check connections (ticket issuance, configuration and existing project reads).
It never saves/reconnects, recovers a journal, creates a thread/turn, starts a
provider job, or opens the canonical brain/index. No service deployment is needed.

Build it in the source checkout; use a cache outside `/tmp`:

```sh
scripts/vendor-setup.sh
scripts/vendor-setup.sh --verify
CARGO_TARGET_DIR="$HOME/.cache/okilum-recovery" cargo build --locked -p okilum-brain --example t3-recovery-preflight
```

Create a private candidate JSON containing the existing T3 settings, with only the
intended recovery fields changed. Synthetic shape (credential **reference** only):

```json
{
  "base_url": "http://127.0.0.1:21001",
  "token_env": "file:/private/credentials/t3-token",
  "environment_id": "saved-environment",
  "project_id": "saved-project",
  "model_instance_id": "saved-provider",
  "model": "saved-model",
  "runtime_mode": "approval-required",
  "interaction_mode": "default"
}
```

```sh
"$HOME/.cache/okilum-recovery/debug/examples/t3-recovery-preflight" \
  --operational /private/brain-state \
  --candidate /private/recovery/candidate.json
```

Stdout is a redacted `okilum-t3-recovery/v1` report: fixed reason codes and
booleans, never credentials, paths, project titles, conversation content or raw
provider errors. Exit 0 means an observation found a supported candidate; exit 2
means refusal or invalid/unreadable inputs. Neither authorizes live apply.

The report separates observed identity from existing target-guard admission:

| Observation | Recovery meaning |
|---|---|
| `same_target` | Environment and retained project match at the saved origin. |
| `transport_relocation` | Same observed environment/project at another origin; retained history still blocks changing the origin. |
| `environment_mismatch` | A matching project UUID does not prove environment continuity. |
| `project_mismatch` / `project_missing` | Candidate selects another project / discovery lacks the retained project. |
| `endpoint_unreachable` | Connection/DNS establishment failed, including a refused offline endpoint. |
| `authentication_required` | Credential unavailable or ticket authorization refused. |
| `discovery_unavailable` / `invalid_discovery` | Protocol, transport or response failure; not evidence that the service is offline. |

`retained_identity_unproven` rejects missing or conflicting routing pins on goals
with external history and mismatching retained T3 envelope/binding identities.
Unrelated empty goals do not need a routing pin; supported terminal model changes
do not change routing identity. Current and previous T3 dispatches are inspected;
prepared, running, uncertain and unknown nonterminal phases block a recovery
recommendation. The existing production save/reconnect guard is unchanged.

This is a saved-state diagnostic, not a view of the live backend's in-memory
state. No runner lock is acquired. Settings/journal bytes are read again after the
network probe; drift gives `snapshot_changed`. Equal reads cannot exclude changes
between reads or later changes, so the report is neither an atomic snapshot nor a
future apply token. It does not certify whole-journal startup replay or inspect
external execution outcomes. Original operation/stage/goal/thread IDs, receipts,
cursors and historical origins remain untouched.

There is no supported retained-history origin/environment migration in this
alpha. A blocked report calls for a separate product decision defining lineage
proof, mappings for every retained identity/origin, replay/correlation semantics,
and rollback before any migration implementation. Do not edit settings/journals,
delete history, create a replacement workspace to bypass the guard, or use a
proxy as an implicit identity migration. A future approved recovery package must
name exact prerequisites, immutable backup, apply, rollback and verification.
Source/fixture PASS is not desktop or live acceptance.

An explicit [future T3 target generation](ai-brain-t3-target-generations.md) flow
can select a distinct target for future stages while preserving historical routes.
It is separate from Save/Reconnect and from the identity-preserving recovery
preflight above; it does not claim that a new environment is the original one.
