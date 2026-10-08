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
and private user-owned storage outside installation/vault/index. The Unix implementation opens an already prepared private directory without
creating state and holds an exclusive lock on its directory descriptor. It reads
and replaces the journal relative to that descriptor, rejects symlinks/hardlinks,
checks owner/mode and bounded JSON, and flushes the temporary file before rename
and the directory before returning. Directory replacement or corrupt prior state
requires recovery rather than a reset. Preparation must persist the private
directory and its parent before passing it to the journal; it runs only after
explicit Enable. Linux filesystem tests exercise this implementation and the macOS
target compiles it, but macOS crash/power-loss acceptance remains native work.
The Windows preparation primitive creates a directory with a protected DACL
containing only an inheritable full-access grant to the current process-token SID.
Existing directories are checked, never repaired or adopted by changing their ACL.
Read-back uses an open handle to check owner, protected DACL, exact ACE and absence
of a reparse point. The handle excludes FILE_SHARE_DELETE while held. Parent paths
must already exist; no recursive permission changes are made. Native tests for
DACL and token SID validation ran on hosted Windows with Rust 1.99: three passed;
the combined preparation test failed because rename succeeded while the handle
was held. This is a handle-sharing guarantee, not a restriction of the owner's
full-access DACL. The handle now requests FILE_LIST_DIRECTORY as well as metadata
access, so it participates in sharing checks; metadata-only access did not enforce
the intended exclusion. The regression requires a sharing violation while held
and a successful rename after release. Native confirmation passed on hosted windows-2022 with Rust 1.99.0 MSVC:
all four DACL/token SID/preparation tests passed, none ignored, including rename
prevention while held, rename after release and shared-directory refusal.
Evidence: https://github.com/BeFeast/tessera/actions/runs/37754095376 (the run's
Rust source exactly matches the directory-guard candidate; only CI LF preparation
was added). This does not establish Task Scheduler or Job Object acceptance.
This is not yet the Windows locked journal, durable file replacement or full
ancestor/file-identity validation. The Unix journal must not become permission
no-ops on Windows.

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
delete. A target-gated COM transport now connects to the local Task Scheduler using the
current token, reads task XML/security descriptors and uses TASK_CREATE, Run, Stop
and DeleteTask. It does not invoke schtasks or PowerShell. Its COM apartment is
thread-bound and outlives the COM interfaces. Current-user SID is read from the process token with TOKEN_QUERY. Task-owner
SDDL is parsed by Win32 and converted to a canonical SID while its native allocation
remains alive; a missing owner is rejected. Neither identity is supplied by the
injected guard. Signed-payload verification, authenticated supervisor state and
process-handle capture/exit confirmation remain mandatory injected guards, not
default no-ops.
The transport is not connected to Reader or installer hooks.

Exported Task Scheduler definitions may contain platform-added defaults. Native
acceptance must capture these and implement explicit semantic normalization if
needed; do not weaken the comparison to only executable/name or ignore unknown
fields to make acceptance pass.

## Windows child ownership primitive

The target-gated `supervisor::windows::JobChild` launches the explicit executable
suspended, attaches it to an unnamed non-inherited Job Object, and only then
resumes its main thread. The job has kill-on-close enabled, so supervisor exit
terminates its remaining children. Failed attachment/resume terminates and waits
for the suspended child. No process is selected for termination by a reused PID.
Stop terminates the job and waits for both zero active job processes and root
process exit; a timeout must leave durable removal pending.

Launch arguments disable browser, self-restart and self-upgrade and specify
separate private config/data paths. Windows quoting, Unicode, spaces and trailing
backslashes are tested on Linux. The caller must verify immutable staged payload
and private-directory ownership before launch. This primitive is not yet a
supervisor executable, authenticated IPC server, TaskGuard implementation or
native runtime test. Windows acceptance must cover assignment/resume failure,
supervisor crash, descendants and timeout; cross-clippy cannot prove those effects.

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

The target-gated native transport uses objc2-service-management for actual
agentServiceWithPlistName, status, registerAndReturnError and
unregisterAndReturnError calls. Runtime macOS 13 checking precedes any SMAppService
class reference. Registration errors are treated as approval-pending only when an
independent status read confirms RequiresApproval. Apple documents that unregister
does not reap the process: the owned supervisor guard must stop/reap first, and
registration/liveness are checked again after unregister returns.

Signature/bundle ownership, authenticated supervisor IPC and stop/reap guards still
need native implementations. These transports alone cannot establish ownership for
duplicate/moved bundles or guarantee cleanup on Trash. Those remain native gates.

## Runtime update recovery

The portable update controller performs one persisted phase per call: stop the
owned instance, select the pre-staged verified candidate, start it, and check its
actual REST version and device identity. Failed candidate verification/start/health
selects rollback; rollback stops the instance, re-verifies the previous runtime and
selects it, then restarts only if lifecycle intent is still Enabled. Host methods
must enforce a shared hook deadline; this controller neither retries in a loop nor
downloads payloads in an installer callback.

Selection and start must be idempotent: a crash after an effect but before the
journal flush replays that phase. An existing pending update cannot be overwritten
by another candidate or reset by a stale retry. Completion records whether rollback
occurred. No method creates a new Syncthing identity or edits vault content.

On Unix, `update.json` uses the lifecycle directory lock and the same private,
atomic, flushed record writer. Each read overlays the current lifecycle intent and
verifies the original binding; a saved Enabled value cannot override later Remove.
The native runtime selector, health/version/identity probes, signed payload guards,
Windows update storage and actual Sparkle/Velopack callbacks remain unimplemented.
The state-machine tests are injected failure/restart tests, not native updater QA.

## Validation and remaining delivery

Linux tests cover an inert installation with an Enable positive control; failed
journal flush; lost registration reply/restart; stop failure and recovery without
restart; terminal removal; changed identity/payload; task owner/privilege/collision;
XML path escaping; macOS approval, missing helper and the macOS 13 boundary.

`scripts/check-sync-sidecar-bindings.sh` cross-checks the actual transport source
with clippy for aarch64-apple-darwin and x86_64-pc-windows-msvc on CT141. It uses a
small generated probe and seeds dependency resolution from the repository lockfile;
this avoids GPUI and unrelated native C dependencies. Install the two Rust 1.99.0
standard-library targets first, and invoke the script inside `tessera-build`.
Both target checks and the 54 Linux controller tests pass. This is type/lint
validation, not linking a signed application or executing either native API.
The cross-check includes target-gated tests: Windows descriptor alias/missing-owner
and process-token round-trip tests compile but still require execution on Windows.

Before native release, implement and test the remaining native ownership guards, Windows locked state store,
supervisors and update/rollback journals; stage the pinned upstream payload with
notices; sign/notarize distributions. Windows hooks must stay within their bounded
callbacks and leave durable recovery intent on timeout. Test close Reader/login,
sleep/offline, update without Reader, moved/duplicate/trashed app, child exit and
unchanged device identity on real macOS 13+ and Windows. Only synthetic vaults and
the isolated CT141 test hub are authorized. Production CT119 is not part of this work.

### Native process-tree and scheduler diagnostics

The Windows Job Object test passed on hosted windows-2022 / Rust 1.99 MSVC:
https://github.com/BeFeast/tessera/actions/runs/37754975122 . A live descendant
was confirmed inside the job before testing explicit termination and kill on
job close. This establishes that process-tree primitive only.

The disabled Task Scheduler fixture failed its owner/state assertion in that
same run. Its guard reports not-running unconditionally, so the assertion failure
identifies an owner mismatch; it does not establish which SID Windows selected.
The next diagnostic logs default and explicit registration owner SDDL/SID,
principal and native state on the same runner. The candidate supplies an explicit
owner SID at registration while retaining scheduler default access rules.
Scheduler acceptance and interactive supervisor execution remain unconfirmed.

The subsequent native diagnostic confirmed default owner `Administrators` and
explicit owner equal to the process user SID; the disabled state was correct.
https://github.com/BeFeast/tessera/actions/runs/37755638626 . The next failure was
parsing a decoded BSTR as UTF-8 while its XML declaration still said UTF-16.
Canonical comparison now removes the transport declaration before parsing,
retains content/attribute checks, and has a Linux Unicode regression test.
The next native run must still validate the complete registration/removal test.

The next run reached definition comparison and exposed Scheduler serialization:
https://github.com/BeFeast/tessera/actions/runs/37756234878 . The exact expected
and returned XML are retained as regression fixtures. Comparison now ignores
field order, RegistrationInfo URI/SecurityDescriptor (owner is checked separately),
and only the named default values: LeastPrivilege, enabled logon trigger/task,
unified scheduling enabled, idle StopOnIdleEnd=true and RestartOnIdle=false.
Non-default values and unknown fields remain significant; duplicate actions,
changed command/arguments/context/principal, privilege escalation and unknown
settings remain mismatches. Native account spellings in principal/logon trigger
are resolved with LookupAccountNameW to SID, never compared by username alone.
The recorded XML passes the portable comparison and mutation regression tests;
full native Scheduler acceptance remains pending the next dispatch.
