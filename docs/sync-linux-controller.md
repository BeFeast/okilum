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
