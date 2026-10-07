# Explicit Sync enrollment

Approved extension for #574; slice 1 is #585. This contract does not enable Sync
in the application. Reader and single-file mode remain local-only until the user
chooses **Enable Sync** in Settings → Sync. Syncthing transports canonical files;
indexes remain disposable, identity/grants/operation journals do not.

## Approved behavior

- After Enable Sync, a managed user background service continues after closing the
  window and starts at login. Logout/sleep/offline is not loss of the local vault.
  Linux packages offer Syncthing as an optional dependency; installation never starts it or registers
  any service/autostart. Registration starts only on Enable Sync; Disable/Remove
  unregister Tessera-owned services, preserving externally owned services. macOS and
  Windows bundle an unchanged sidecar with matching MPL notices/source access.
- Offer reuse of an existing daemon after confirming identity, path, and user
  consent. Never take over its lifecycle or replace its complete configuration.
  Multiple candidates, inaccessible configuration, or path ambiguity require an
  explicit choice. The selected endpoint's device ID must match durable state.
- A new client trusts the hub as introducer; the hub does not trust the client as
  introducer. Skip Introduction Removals is on; Auto Accept Folders is off.
  Existing fleet settings stay unchanged. These flags are not a fleet revoke.
- One-click enrollment accepts only an empty directory or an authenticated known
  replica with the same folder ID and canonical path. Unknown nonempty import is
  separate. Reject overlap with any daemon's existing folder, including symlink
  aliases. A folder ID alone is not proof of ownership or replica identity.
- Create a new folder paused, seed approved canonical ignore bytes and validate
  include dependencies, then receive initially as `receiveonly`. `.stignore` is
  local policy, not transported by Syncthing. Different existing policy stays
  stopped for review; never overwrite it automatically. `.claude/skills` is
  permitted; worktrees and `.tessera-index` are excluded. The small policy in the
  fixture demonstrates those shapes and is **not** the live canonical policy.
- Promote to `sendreceive` only after verified complete receipt and no local
  changes under serialized controller ownership. No automatic Revert/Override.
  Missing folder markers remain errors; never recreate a marker to hide a missing
  mount. The compatibility crate deliberately does not implement promotion.
- Pause is per folder, including all peers. Remove is cooperative local stop plus
  service/hub revocation, retaining local files. Reuse removes only recorded
  Tessera additions; previously configured sync may continue. Hub offline means
  removal pending. Durable deny/reconciliation prevents re-introduction at the
  hub, with an eventual-enforcement window, not guaranteed fleet isolation.
  Lost-device fleet revoke is separate; already copied data cannot be revoked.

## Authority and subsequent slices

Local managed cert/key, REST key, selected device identity, ownership records,
operation IDs and revocation tombstones live in private OS application state,
outside vault/index. REST keys never go in URLs, argv, diagnostics or service
responses. REST uses authenticated literal loopback with redirects and proxies
disabled. Sync transport policy is separately explicit; a loopback REST address
alone does not restrict listeners, discovery, relay or NAT traffic.

Browser passkey approval at the existing HTTPS origin produces a short-lived,
nonce/verifier-bound scoped device grant. Service chooses owner/vault/folder;
clients never supply a server path or arbitrary REST target. A host hub adapter is
the sole reader of the hub REST key. It offers scoped add/remove/status, not a
REST proxy. Operations persist requested → approved → hub_added → local_prepared
→ syncing → active with read-back and idempotent retry; removal has a durable
pending state. Service pairing and the isolated adapter are defined in [slice 2](sync-pairing.md);
  native local preparation and service lifecycle remain subsequent slices.

Settings must distinguish preparing/syncing/idle/paused/offline/error, report
conflict files and folder errors, and show device/path and managed/reused mode.
Idle is local state, not proof that all peers are connected or caught up. Event
cursor loss triggers fresh status/connection/error snapshots. Preserve both
conflict versions. Sync is not backup: sendreceive deletions propagate; hub
versioning does not protect local hub edits. No iCloud migration is authorized.

Live hub installation/socket mounts require a separate concrete backup/rollback
plan and explicit manager approval. Native GUI QA belongs to a separate QA
session; no desktop/platform behavior is inferred from a Linux container test.

## Slice 1 compatibility boundary

`tessera-sync` is a small independent crate consumed by the opt-in host adapter. It
uses scoped REST endpoints, authenticated identity/version inspection, explicit
paused folder creation, scoped folder patch/read-back, ignore read-back, scan and
status/error inspection. It never PUTs the whole configuration. The controller
must serialize enrollment, recheck paths immediately before writes, validate
identity before every resumed operation, and retain a journal of additions.
Generic JSON here mirrors the versioned upstream API; it is not a remotely
exposed authority boundary. Scope/ownership and service grants remain slice 2.

The real fixture runs unchanged **Syncthing 1.29.5 (hub)** against **2.1.6
(client)**. Client 2.1.6 is an explicit compatibility candidate, not a promise
that arbitrary future Linux package versions pass. Later controllers must
version-check and reject unsupported behavior; this does not upgrade the hub.
No source changes or system installation of Syncthing are involved.

Reproduce on an isolated Linux amd64 development machine with Rust, Python 3,
curl, tar and sha256sum (Tessera executors must use the authorized remote build
host and wrap the complete command in its build lock):

```sh
cargo test -p tessera-sync
bash scripts/sync-compatibility.sh
cargo fmt --all --check
cargo clippy --tests -p tessera-sync -- -D warnings
```

The script downloads the two pinned Linux amd64 archives, verifies their committed
SHA-256 sums and invokes an ignored integration test. The
test uses separate temporary homes/configs/identities/keys/vaults, loopback-only
REST and TCP listeners, no discovery/relay/NAT/upgrade, and reaps only children it
started. It never reads a user daemon/config or contacts the live hub. Port
allocation has the normal release/bind race; startup failure fails the run.

Evidence and limitations are in [the compatibility result](sync-compatibility.md).

The first Linux controller component is documented in [Linux lifecycle ownership](sync-linux-controller.md). It is not yet connected to Reader or Settings.


### Linux package boundary (#587)

The Arch package declares `syncthing: sync between devices` in `optdepends`, not
`depends`. Reader installation and use do not require Syncthing. Tessera installs
no service unit or package install hook; discovering the package does not run it.
Only explicit Enable Sync may prepare Tessera's private instance and register
its user service. Disable/Remove unregister that owned service. External reused
instances keep their lifecycle.

The controller currently accepts the tested client version 2.1.6 and reports an
unsupported package version before preparation if Arch ships a different one.
The dependency does not pin or downgrade the user's Syncthing package; support
for another version requires updating the isolated compatibility evidence first.
Arch package installation/lifecycle QA remains a native Linux check, separate
from the controller's CT141 systemd and fixture tests.

### Desktop Remove coordination

Remove first records terminal intent in the runtime, pairing, and folder journals.
It attempts local folder cleanup, stops/unregisters only the owned runtime, then
reconciles service revocation. An unavailable service cannot keep the owned daemon
running. Retry after restart revokes using saved credentials without starting the
service again. A reused daemon keeps running; its folder cleanup must complete
before claiming removal, and the UI must say external sync can continue.

If an owned daemon was already offline, private dormant configuration may remain;
local files remain in all cases. The removed runtime identity cannot be enabled
again through its old journal. Disable remains reversible and preserves identity;
a new connection after Remove requires a fresh explicit enrollment. The real
CT141 user-systemd fixture verifies stop-before-revoke during a simulated service
outage, later revocation, and no restart or certificate replacement on retry.

### Settings integration boundary

The Linux Sync section is a separate GPUI component, created only when the user
opens that section. Discovery and every blocking controller operation run on the
background executor. Its initial read cannot prepare/register/start Syncthing.
Explicit Enable persists the chosen HTTPS service, computer name, folder and
managed/reused runtime before effects. Browser approval shows a grouped matching
code. A bounded refresh continues pairing and scoped first receipt; local idle
never substitutes for the readiness predicate. Status distinguishes Preparing,
paused, hub offline, local errors and caught up with the hub; the last observed
hub connection remains available offline. It does not claim all computers are
online or that conflict copies have been resolved.

Disable keeps identity for re-enable. Remove attempts local stop independently of
remote availability and displays pending until required reconciliation completes.
Once removal completes, a private generation record retires that connection;
explicit new setup allocates new journals. Retired authority is never silently
reused and canonical files are never deleted. Unknown nonempty folders still
require a separately approved adoption flow. Per-generation records stay outside
the vault/index; no marker is added to the vault.

Native Linux light/dark before/after evidence and browser-to-daemon end-to-end QA
are required before publishing the Settings UI. Development still uses only the
CT141 sandbox. The optional QA trust certificate is compiled only into the
non-publishing settings harness; shipping builds use system certificate trust.
