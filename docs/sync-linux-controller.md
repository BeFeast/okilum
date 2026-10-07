# Linux Sync controller (#587)

The first part of this slice supplies an unconnected service-lifecycle library,
`tessera-sync-controller`. Reader startup, Settings and package installation do
not call it. Discovery/reuse, daemon preparation, pairing, folder enrollment,
package dependencies and the native Settings screen follow separately. This is
not yet user-facing Enable Sync.

## Managed lifecycle boundary

The caller explicitly chooses Enable and provides a **dedicated, already prepared
private Syncthing home** and the package executable's absolute path. Preparation
must configure loopback REST, the explicit transport policy and a fresh identity
without launching a default-config daemon. It must reject an externally owned
installation: reuse has a separate controller path and never calls this lifecycle
manager. This library validates the prepared files, not Syncthing configuration
semantics or version compatibility.

The state directory is private durable application state, outside the vault and
disposable indexes. Its parent must exist. Constructing a handle, reading desired
state or disabling a never-enabled instance creates no files and invokes no
service commands. First Enable creates the journal and a random owned service ID.
The journal binds the home, executable, unit directory and certificate digest.
Re-enable retains that identity; replacement is an explicit error.

Each operation takes an exclusive state lock. Requested enabled/disabled state is
committed atomically with file/directory fsync before systemd calls. Unit files are
published complete, without replacing an occupied path. The service invokes the
absolute executable directly, with systemd argument/specifier escaping and no
REST key in argv. It runs Syncthing with browser/upgrades disabled and a private
umask; user login owns the service lifetime, not the reader window. Closing
Tessera does not stop an explicitly enabled unit.

Disable stops and disables only the journal's unit, verifies its file has not
changed, removes it and reloads the user manager. Stop failure retains the file
and durable disabled intent for retry. Externally changed units, unexpected loaded fragments or drop-in overrides fail closed;
absence of a unit file does not by itself prove a running service stopped.
Unit and lock opens reject symlinks and foreign owners. Held unit descriptors and
inode/content checks surround systemd operations; temporary files are swept only
within their journal scope. systemd offers no atomic filesystem compare-and-act:
concurrent external administration must be coordinated, and detected races return
an error rather than a successful receipt. This is not a sandbox against hostile
processes running as the same OS user.
Certificates, daemon data and local vault files are never deleted.

A successful lifecycle operation is not proof of authenticated REST readiness,
folder enrollment or synchronization. The next controller layer must observe the
expected daemon identity/version and report pending/error rather than assuming
that systemd accepting a start means the vault is ready. `reconcile` retries
durable service intent when the controller is explicitly reopened; this crate
does not install an extra background retry mechanism while Sync is disabled.

## Evidence and remaining gates

Six unit tests cover no effects before Enable, service-positive/disabled-negative
controls, identity-preserving re-enable, interrupted stop recovery and rejection
of replaced identity/unit files, symlink locks and a unit replaced during start. The opt-in integration test uses a fresh
Syncthing 2.1.6 identity with loopback listeners and discovery/relay/NAT disabled,
plus a uniquely named real systemd user service. It verifies readiness before
absence, stop/unregistration, re-enable and unchanged certificate bytes, then
cleans up its own service. It never contacts CT119 or reuses a normal user daemon.

From the complete repository workspace on the isolated authorized development host:

```sh
~/bin/tessera-build cargo fmt -p tessera-sync-controller --check
~/bin/tessera-build cargo clippy --locked -p tessera-sync-controller --tests -- -D warnings
~/bin/tessera-build cargo test --locked -p tessera-sync-controller
TESSERA_SYNC_CLIENT=/path/to/pinned/syncthing-2.1.6 \
  ~/bin/tessera-build cargo test --locked -p tessera-sync-controller --test linux_service -- --ignored
```

A container user-manager test is not native desktop/login QA. Before a later UI
PR, follow the project's ten UI rules and attach Linux light/dark before/after
screenshots from the manager's QA sub-session. Final slice acceptance also needs
real login/update behavior, reuse preservation, empty first receive, known
replica/path checks, folder Pause, offline Remove, conflicts and the Linux beta.

## Read-only discovery and offline preparation (#587 increment)

`daemon::configuration_paths` inspects only known XDG locations and user-owned
Syncthing process arguments/environment; it never launches a daemon. Each process
must resolve independently. `discover` authenticates loopback REST inventories,
records the certificate fingerprint and device ID, and deduplicates paths.
Unavailable candidates prevent selection/enrollment instead of being treated as
an empty inventory. Missing/unmounted folder paths are errors. A later mutation
must reconnect through the recorded identity; only client 2.1.6 is supported.
Inventory may include the compatibility hub version 1.29.5, without granting
permission to change it. This is discovery infrastructure, not automatic reuse.

Explicit Enable may call `prepare` for a new private controller directory. Its
intent is durable before offline `generate`; the receipt binds the executable,
loopback endpoints, certificate and device ID. Repeating an interrupted prepare
preserves the identity. Default folders are removed before any service launch;
discovery/relay/NAT/upgrade/browser/reporting are disabled. Preparation does not
register a unit or launch a serving process. Existing homes without the matching
intent are never adopted.

## Native HTTPS pairing and recovery

`pairing::Service` uses an HTTPS origin with no redirects/proxy and strict trust.
An explicit private test CA is supported for the CT141 sandbox; there is no
accept-invalid-TLS option. Credentials are separate from browser sessions and
never appear in the public status snapshot. Approval URLs, registration identity,
vault scope and connection descriptors are validated before use.

`enrollment::Enrollment` persists the session and origin before Start, and an
exchange intent before attempting Exchange. After restart it tries grant status
first, recovering a committed grant even when the original exchange response was
lost or the exchange expired. The first grant binds its vault UUID; the first
ready descriptor binds folder/hub/address/ignore policy. Later changes fail closed.
Remove persists terminal intent before contacting the service and never exchanges
again. Network failure retains the grant and pending removal for retry. Cancelling
before a possible grant (including a confirmed unapproved exchange with no older
uncertain attempt) is reported as setup cancellation, not hub revocation. If an
exchange may have reached the server but status/revocation cannot establish its
outcome, removal remains pending; credentials are retained for reconciliation.

These APIs remain unconnected to Reader. Folder additions/reuse, receive-complete
promotion, package dependency and the separate Settings Sync component follow in
subsequent increments of #587; this increment does not close the issue.

Validation: 14 unit tests cover service ownership and pairing restart/removal/scope;
the ignored `linux_service` test uses an actual isolated 2.1.6 process and user
systemd, including offline preparation/retry and read-only authenticated discovery.
The ignored `sandbox_pairing` test checks trusted versus untrusted TLS, exact Start
retry and absence of grant authority before browser approval against CT141. It does
not replace real browser/passkey QA or claim a completed desktop enrollment.

The CT141 sandbox now uses a dedicated local CA (`service/tls/ca.pem`) and a separate
server certificate with `CA:FALSE`, serverAuth and localhost SANs. Native test trust
uses the CA; no host-wide trust change is required. All fixtures remain synthetic.

The replacement review identified three preparation/discovery durability gaps:
all are addressed before merge. A completed preparation now rechecks the package
version and binds its executable hash, so an upgrade at the same path cannot
bypass compatibility checks. `/proc` discovery includes a running `syncthing
(deleted)` executable after package replacement. Private state initialization
syncs the directory and its parent before any journal-authorized network request,
including retries after interrupted initialization. This is the filesystem
ordering guarantee; the tests do not simulate physical power loss.

The real Linux regression test replaces the package after preparation, rejects
both an unsupported version and changed same-version bytes, restores the original
package, and verifies that unlinking the running executable does not hide its
custom-home configuration. User-service disable/re-enable still preserves identity.

## Scoped folder enrollment and external reuse (next increment)

`folder::FolderController` binds a HubReady grant to the selected authenticated
2.1.6 daemon, canonical destination and approved hub descriptor. It requires a
complete fresh inventory: unavailable daemons, overlapping paths and unknown
nonempty destinations prevent enrollment. A known replica must belong to the
selected daemon, share the approved hub and already have the approved ignore
policy. Reuse preserves its configuration rather than replacing it.

A new folder is created paused and receive-only. Ignore policy is written and
read back before unpausing. The durable journal precedes each owned mutation,
so replay does not create a second folder or adopt a changed destination. A new
hub record uses one-way introduction; existing hub configuration is preserved.
Pause affects only the selected folder. Remove persists terminal intent even
when the daemon is offline, removes only owned folder/device configuration and
retains all canonical files. For reuse, Remove restores any controller-owned
pause change once, then relinquishes authority; repeated Remove cannot override
subsequent external changes. It reports that external synchronization remains.
Service grant revocation and managed service shutdown remain separate operations
that the desktop orchestrator must reconcile.

The isolated test uses a real 1.29.5 hub and 2.1.6 client with loopback-only
transport, independent identities and temporary data. It verifies real content
receipt, index exclusions, restart replay, reuse preservation, folder pause,
offline removal recovery and survival of unrelated configuration and local files.
No production hub is contacted. Folder status exposes unavailable paused-folder
errors as null, rather than claiming zero errors.

This increment deliberately leaves a new folder receive-only. Automatic promotion
requires proof that the hub index was received, including for an empty vault;
idle plus zero pending items alone is insufficient. Completion/promotion, durable
connection/conflict reporting and Reader orchestration remain subsequent work.

The folder journal also retains the last authenticated observation of a live hub
connection. Settings can read that Unix timestamp while REST is offline, without
starting a process or registering a service. It means connection observed, not
synchronization completed. A real fixture verifies that restart/offline reads
retain it and failed status requests do not erase it.
