# Managed sidecar lifecycle: macOS and Windows

Slice 4 of #574 (#588). This increment defines the controller contract and the
Task Scheduler/SMAppService adapters. It is deliberately not connected to Reader,
package installation or updater hooks. Linux tests exercise injected native ports;
they do not prove native login, signatures, process exit, update or Trash behavior.

## Intent and ownership

A controller handle has no effects. With no journal, snapshot/reconcile/Disable/
Remove cannot create state, register an agent/task or launch anything. Explicit
Enable accepts an already prepared, verified binding and persists Enabled before
registration. Reconciliation repeats payload and existing-device verification;
registration recovery cannot generate a replacement device identity. Re-enable
must use the same installation, user, instance, supervisor and state location.
Remove is terminal for that instance. Reuse belongs to the existing external-runtime
controller and never enters this managed registration path.

Disable and Remove persist intent before stopping anything. Failure or interrupted
callbacks leave a replayable intent. The platform must confirm that the supervisor
and its owned child exited before unregistering. Removal here is local: service/hub
revocation is a separate durable operation, never a blocking uninstall-hook call.
Local notes, certificate/config and state are not deleted by this controller.

The LockedJournal port requires an exclusive instance lock, atomic durable writes
and private user-owned storage outside installation/vault/index. Native filesystem
implementations are not supplied by this increment. Windows needs real owner DACLs
and file identity/locking; the Linux chmod implementation cannot become a no-op.

## Windows adapter

The generated task definition has an instance-specific name, the current SID in
both principal and login trigger, InteractiveToken and LeastPrivilege. There is
one explicit Exec action, no shell, password or REST key. Multiple instances are
ignored; execution has no fixed time limit; crash restarts are limited to three.
The working directory is private state, not Velopack's replaceable current folder.
The verified supervisor executable must be staged outside current by the native
payload/preparation layer, which runs only after Enable.

Read-back compares task owner and the entire definition structurally, ignoring
only XML formatting/comments. Extra actions, elevated privilege and modified fields
fail closed. Creation must use TASK_CREATE without replace/update. Each native
mutation must recheck the expected definition and owner; a name or PID is not an
ownership token. stop_owned must terminate/await the owned Job Object tree before
delete. The TaskApi port still needs COM bindings and the actual supervisor; this
increment does not invoke schtasks or PowerShell and cannot create a live task.

Exported Task Scheduler definitions may contain platform-added defaults. Native
acceptance must capture these and implement explicit semantic normalization if
needed; do not weaken the comparison to only executable/name or ignore unknown
fields to make acceptance pass.

## macOS adapter

Sync requires macOS 13+, independently of Reader's deployment target. Monterey 12
on Hedva can test Reader but cannot accept this Sync lifecycle. The static bundled
plist uses BundleProgram under Contents/MacOS and is installed under the app's
Contents/Library/LaunchAgents; placing it in the bundle is not registration. No
legacy user LaunchAgents/launchctl backend is added.

The SmApi port binds to one verified signed bundle and current owner. It maps
NotRegistered, Enabled, RequiresApproval and NotFound distinctly. RequiresApproval
never becomes Running, and a missing helper is not treated as unregistered. The
bridge must await unregister completion and verify process exit, not report success
when an asynchronous request was merely dispatched. SMAppService Enabled is a
registration status; supervisor liveness is checked separately.

The native Objective-C/Swift binding and supervisor are subsequent work. These
adapters cannot establish ownership for duplicate/moved bundles or guarantee cleanup
on Trash by themselves. Those are explicit native gates, not inferred properties.

## Validation and remaining delivery

Linux tests cover an inert installation with an Enable positive control; failed
journal flush; lost registration reply/restart; stop failure and recovery without
restart; terminal removal; changed identity/payload; task owner/privilege/collision;
XML path escaping; macOS approval, missing helper and the macOS 13 boundary.

Before native release, implement and test the native ports, locked state stores,
supervisors and update/rollback journals; stage the pinned upstream payload with
notices; sign/notarize distributions. Windows hooks must stay within their bounded
callbacks and leave durable recovery intent on timeout. Test close Reader/login,
sleep/offline, update without Reader, moved/duplicate/trashed app, child exit and
unchanged device identity on real macOS 13+ and Windows. Only synthetic vaults and
the isolated CT141 test hub are authorized. Production CT119 is not part of this work.
