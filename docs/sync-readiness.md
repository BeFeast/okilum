# First-receive readiness (#587)

Approved direction: extend the pairing service and hub adapter with a scoped,
read-only readiness observation. No marker is written to a vault. Development
and proof use only the independent CT141 hub and synthetic data. The endpoint and explicit controller promotion are implemented in this local
increment; desktop orchestration is not connected and no production deployment
is implied. Until its acceptance gates pass, new folders
remain receive-only and Settings shows **Preparing**.

## Authority and endpoint

`POST /api/v1/sync/desktop/readiness` uses the existing desktop Bearer grant.
No browser Origin/Cookie or CORS support; no credentials in query strings/logs.
The request has no caller-selectable owner, vault, folder, device, path or REST
address. The service resolves the registration from the grant and allows only
`hub_ready`. Pending/revoked/removal_pending registrations cannot obtain a ready
observation. Unknown fields are rejected. Revocation is checked again after the
adapter response before returning it. Readiness confers no configuration mutation
capability and never issues a second grant.

The private socket adds `readiness` to the existing bounded adapter action enum.
Its request binds owner/vault/registration/device through the same mapping and
ownership journal as Add. Adapter rejection, scope mismatch or unavailable REST
returns pending/unavailable, never an empty successful snapshot. The adapter
verifies pinned hub version, certificate/device identity and configured folder
path before gathering observations. It cannot start, scan or modify an arbitrary
folder supplied by a client.

The response envelope contains registration/vault/folder/hub/client bindings,
protocol version, observation ID, expiry and an adapter-generation identifier.
The observation contains hub folder scan state, hub local index sequence, hub
startup identity (to invalidate observations across daemon restart), global/local
item and byte counts, pending items, errors and the approved client's connection
and remote folder state. Generation and observation IDs are service metadata,
not files inside the vault. An old observation must not become valid again after
adapter restart, daemon restart, index reset or scope change. The exact source
for daemon/index-generation detection is an explicit proof gate below.

## Client predicate and races

An observation is a short-lived boundary, not a claim that all peers are synced.
The client must independently authenticate its selected daemon and immutable
pairing scope, retain receive-only mode and approved ignores, and observe:

1. Hub folder is unpaused, fully scanned, idle, has no pending receive or errors,
   and the approved client has an established folder connection.
2. Client has received at least the observed hub sequence, is idle with no pending
   downloads/errors, and has finished its own scan with no receive-only changes.
3. A second scoped observation still has the same valid generation and boundary.
   A changed boundary, expiry, missing field or disconnect means retry Preparing.
4. Empty vaults require a freshly scanned empty hub and confirmed folder sharing;
   a missing client remote sequence alone never proves an empty remote vault.

Only after the proved predicate may the controller journal promotion intent and
PATCH its owned folder to sendreceive, with read-back and idempotent restart
recovery. Reused folders retain their original mode. Concurrent local writes or
hub changes during validation require explicit race tests; there is no atomic
transaction across the two REST services. The contract must not advertise a
stronger snapshot guarantee than the pinned versions can demonstrate. Failure to
prove a safe empty or reset case keeps it Preparing rather than weakening a gate.

## Evidence and acceptance

Upstream 1.29.5/2.1.6 index sender batches and REST completion implementations were
inspected on CT141. RemoteIndexUpdated describes a batch, not receipt of the whole
remote snapshot. Completion/idle with zero need can describe an uninitialized
client database; it is insufficient without independent hub evidence.

The ignored `readiness_boundaries_empty_and_large` controller fixture runs real
1.29.5/2.1.6 processes with fresh identities, loopback-only transport and temporary
vaults. It records the empty hub/client sequences, transfers 12,000 individually
verified notes across multiple batches, rejects the old empty observation for
the newer sequence, and observes a receive-only local edit without publishing it.
This fixture tests REST observability; separate endpoint and promotion regressions
exercise authority and durable transitions.

Before enabling promotion, add endpoint/adapter tests for wrong or revoked grants,
owner/vault/device substitution, offline/paused/scanning/error states, daemon and
adapter restarts, index reset, stale/expired observations, interrupted transfer,
large concurrent hub changes, local edits and promotion crash/retry. Empty vault
and index-generation semantics must be demonstrated directly, not inferred from
absence. No production CT119 deployment or socket mount belongs to this change;
that requires a separate daytime approved plan.

### CT141 observations (2026-10-07)

The 12,000-note fixture passed with hub boundary 12001 (notes plus directory),
client remote sequence 12001 and 12001 local items. Every note's contents matched.
During transfer the remote sequence already reached 12001 while thousands of
files were still needed: index receipt alone would have falsely declared ready.
A later local-only edit was detected and never appeared on the hub.

For a freshly empty vault the hub sequence is 0 and the client remote sequence
is null. The hub's scoped completion reports remoteState=valid. Pausing the
client changes that field to paused even though completion remains 100 percent;
resuming returns valid. This is a positive/negative control for empty-folder
sharing, not proof that completion percentage alone is usable. Both fixtures
leave the client receive-only. Endpoint authorization and restart/index reset evidence are recorded below;
client-side freshness validation and explicit promotion recovery are described below.


### Scoped observation endpoint implementation

The native POST accepts an empty JSON object and rejects extra fields. It resolves
scope from the grant, releases the auth mutex during private socket I/O, then
revalidates the same grant and hub_ready registration before returning data.
A removal committed while the adapter is responding prevents disclosure. The
adapter requires an active owned registration and existing share. It validates
hub identity/version/path, brackets sampling with hub startup/sequence/config
checks, and emits bounded selected counters without REST credentials or paths.
The response state is `observed`, never `ready`; missing counters remain null.
An offline/unstable adapter returns unavailable rather than an empty success.

The native HTTP regression passes wrong-grant, caller-supplied vault, browser
Origin and removal-during-socket-I/O cases, with a valid scoped observation as
positive control. The actual 1.29.5 fixture records a nonzero index, stops its own
synthetic daemon, deletes only its temporary index database and restarts it.
The new startup value differs even though sequence values may be reused. Reopening
the adapter changes adapter_generation independently. Wrong owner/unknown
registration fail; existing revoke and unrelated-configuration regressions pass.

This provides evidence for the generation fields used by the client validator.
The client must reject expired observations and changed generations before any
promotion. Known hub/global/local writes can still race separate REST calls;
concurrent-change acceptance and crash recovery must precede enabling promotion.

### Native observation validation

The native pairing client now parses scoped observations into in-memory receipts.
It validates protocol, registration/vault/device/hub/folder binding and a remaining
lifetime of at most 30 seconds. Each receipt also carries a monotonic deadline;
it is deliberately not serializable. Clock rollback cannot extend that deadline,
and controller restart requires new observations. Two distinct fresh receipts
must agree on owner, scope, adapter generation, daemon startup and hub counters.
Paused/disconnected/unknown state, missing counters, pending downloads, local
receive-only changes or mismatched totals fail the predicate.

`FolderController::first_receive_ready` obtains receipts through the saved
Enrollment credentials, scans the authenticated client between them, rechecks
folder ownership/configuration and ignores, and evaluates the interval. This is
an observational API, not automatic promotion. Reused folders cannot enter this
path. A concurrent external write after the last scan remains outside this
interval evidence. The explicit promotion operation below journals its transition;
the desktop does not invoke it yet.

Unit regressions cover scope, expiration, revocation state, replay of one receipt,
monotonic expiration, changed owner/generation/startup, partial index/content and
local edits. The empty real-daemon fixture also feeds its actual REST counters
through this same client parser and predicate, with pause/resume negative control.

The large native predicate experiment identified a pinned-version counter
incompatibility: hub 1.29.5 includes `SyntheticDirectorySize = 128` bytes per
non-deleted directory or symlink (`protocol.FileInfo.FileSize`), whereas the
2.1.6 fixture reports file bytes. Identical 12,000-note contents differed by 128
bytes for the single directory. The adapter now also supplies global directory
and symlink counts. The client subtracts their checked synthetic contribution
from the pinned hub's global byte count before comparing to 2.1.6; missing counts,
overflow or underflow fail closed. Hub-local versus hub-global bytes must still
match before normalization, and pending/local-change/index checks remain required.
This normalization is version-specific and must be revisited before upgrading
either pinned endpoint. A unit regression covers both directories and symlinks.


### Explicit promotion and recovery

`FolderController::promote` accepts a scoped observation source, serializes with
folder operations, and permits only its owned enrolled replica. It scans again
after the second service observation to detect local edits made while that
response was pending. A failed predicate leaves receive-only mode intact.
The journal records `Promoting` before changing the daemon to `sendreceive`, then
records `Active`. If restart finds receive-only mode, it requires fresh
observations; saved intent alone never authorizes a new PATCH. If the daemon
already committed the PATCH, matching ownership/configuration permits completion
of the journal without another mutation. Reused or removed replicas cannot enter
this transition.

The real CT141 1.29.5/2.1.6 folder fixture covers pause blocking promotion, a local
edit injected during the second observation (not published), interrupted intent
before PATCH with service unavailable, and lost journal completion after PATCH.
A new client file arriving at the hub after promotion is the positive control.
These tests do not claim an atomic snapshot across independent filesystems:
an external writer can still write after the final local scan. The endpoint,
controller, and large fixture are tested separately; desktop scheduling and a
full native HTTPS enrollment-to-promotion run remain integration work.
