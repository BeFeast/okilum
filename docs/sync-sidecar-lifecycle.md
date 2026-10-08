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

### Windows native acceptance

Hosted windows-2022 with Rust 1.99.0 (`x86_64-pc-windows-msvc`) verified:

- Private-directory owner/DACL and token SID: four passed, none ignored.
  https://github.com/BeFeast/tessera/actions/runs/37754095376
  The held handle prevents rename; releasing it allows rename; an existing shared
  directory is refused without changing its contents.
- Process tree and Task Scheduler transport: two passed, none ignored.
  https://github.com/BeFeast/tessera/actions/runs/37757252886
  A live parent and descendant are confirmed in the Job Object before explicit
  stop and kill-on-close. The disabled, uniquely named Scheduler task verifies
  registration, collision refusal, owner/definition refusal and owned removal.

Scheduler registration supplies the process user SID as security owner. The
runner's default owner was Administrators. Task comparison resolves account names
with LookupAccountNameW; compares owned fields without ordering; ignores only
RegistrationInfo URI/SecurityDescriptor and named default values (LeastPrivilege,
enabled logon trigger/task, unified scheduling, idle StopOnIdleEnd=true and
RestartOnIdle=false). Unknown fields, non-default privileges, changed execution
fields and extra actions remain mismatches. Recorded Windows XML and mutation
cases exercise this comparison on Linux.

The final combined `sidecar::` run passed 31 tests with none failed or ignored
on exact PR #768 source `78022224a824baa11a10f0d6c42fdd7bec19bfe0`:
https://github.com/BeFeast/tessera/actions/runs/37762178783 . Its run SHA
`aa3df9ca9b8f2abe4ff6f13987c159cbf77cb2e1` adds only CI LF preparation;
Rust/Cargo sources were verified identical. This predates the IPC protocol. They do not establish authenticated
interactive supervisor start/stop, a complete Windows locked journal, or shipped
Sync lifecycle acceptance. No personal Syncthing state is used.

## Supervisor IPC protocol foundation

The portable `supervisor::ipc` module defines one authenticated local exchange,
not a listening endpoint or executable supervisor. It is not wired into Reader,
Task Scheduler, SMAppService or updater hooks. Construction has no OS effects.
The only commands are Status and Stop: there is no Start, executable path, shell
command, REST credential or caller-supplied ownership token in the protocol.

Frames use a four-byte big-endian length and at most 4096 JSON bytes. The reader
checks the size before allocating the body; truncated frames, unknown fields,
unknown commands and unsupported versions fail closed. Installation and instance
UUIDs bind the exchange to durable state. A new random supervisor generation is
created for every server lifetime; each response must echo that scope and the
request UUID. Generation discovery must come from a verified native endpoint,
not a public PID file or unauthenticated first reply.

Both server and client authenticate the native peer before wire I/O. The client
also checks the request's installation/instance against its prepared Binding;
it accepts neither a mismatched reply nor Running as a Stop result. An I/O error
or lost reply is an error, never evidence of process exit. There are no automatic
client retries. The server rechecks durable stop intent before every Stop,
including repeated requests, and uses captured owned process/job handles. It
caches completed stop only for its lifetime; a lost response can be retried without
stopping twice. Stopping is an incomplete timeout result and cannot authorize
unregister/removal. Stopped must mean the entire owned tree was reaped.

Native integration must provide private endpoint creation/discovery, authenticated
peer identity (including the expected supervisor, not merely a claimed SID/UID),
a shared absolute deadline for authentication and all fragmented I/O, captured
process handles, and lock ordering for durable intent. The controller must not
wait for IPC while holding a journal lock that the supervisor needs to validate
intent. Runtime methods must remain bounded by the same hook budget. These are
required transport/runtime contracts; the traits do not implement or prove them.

Protocol tests exercise fragmented I/O, pre-effect authentication/scope/version
refusal, durable intent refusal with a positive Stop control, incomplete stop,
lost reply/idempotency, response correlation and malformed/oversized frames.
A real UnixStream pair tests framing only with an injected peer gate; it is not
native peer-authentication or macOS/Windows supervisor acceptance. Native IPC,
full updater integration and complete Sync acceptance remain open under #588.

### Windows pipe peer identity primitive

`supervisor::ipc::windows_peer::ProcessPeer` consumes an owned process handle
obtained by the native launch/signature ownership layer. It never opens a process
from a PID supplied by a pipe or wire message. The handle must grant query and
synchronize access; the process must be live and its primary token SID must equal
the prepared current-user SID. The guard keeps that process object open.

For a connected pipe, it checks file type and endpoint direction, obtains the
remote process ID through GetNamedPipeServerProcessId/GetNamedPipeClientProcessId,
and compares it with the captured process. It checks process liveness and token
ownership again after the query. A same-user but different process, wrong endpoint,
ordinary file or exited process fails. Pipe I/O is not performed by this primitive.

This is not a full authenticated transport: the caller still must verify the
expected executable/signature/installation, create an owner-private local-only
endpoint, retain the connected pipe through the exchange, authenticate discovery
and generation, enforce the absolute deadline, and confirm owned process exit.
In particular, no caller may feed an arbitrary remote pipe to this primitive
and treat a numerically matching PID as local identity. Production endpoint
construction must enforce local-only access; the transport is not connected yet.

Two Windows-gated fixtures use disposable UUID-named local pipes and owned helper
processes with cleanup on all return paths. One checks both directions plus SID,
role and file rejection. The other confirms an actual child connection, refuses
a different live process of the same user, accepts the captured child, then
refuses that handle after exit. The fixture's default token pipe DACL is not
production endpoint ACL acceptance.

The native run on windows-2022, Rust 1.99.0 MSVC passed 41 `sidecar::` tests with
zero failures or ignored tests, including both named pipe-peer fixtures:
https://github.com/BeFeast/tessera/actions/runs/37813567283 . Run/source SHA was
`b4a359be85e3bd62fbaccc1651fb1c9fee046380`, tree
`97c2613b4335630fb9acf1e589ee265a4401424d`, with no extra source/workflow commit.
The dispatch used package `tessera-sync-controller`, no features, filter
`sidecar::`, and one test thread. This confirms this peer-identity primitive,
not the remaining production transport, private endpoint ACL or Sync acceptance.

### Windows private server endpoint

`supervisor::ipc::windows_endpoint::PrivatePipe` explicitly creates one server
instance in the fixed local `\\.\pipe\Tessera-Sync-<installation>-<instance>-<generation>`
namespace. All three identifiers are non-nil UUIDs, not caller paths or hosts.
It requests FIRST_PIPE_INSTANCE, OVERLAPPED and PIPE_REJECT_REMOTE_CLIENTS with a
single instance limit. Creation does not connect or read/write, register a task,
start Syncthing or alter durable intent; only verified explicit-Enable preparation
may call it. The handle is non-inheritable and owned until drop.

Creation supplies the current process user as security owner and a protected DACL
with exactly one non-inheritable full-access grant to that SID. Read-back checks
owner, protection, ACE count/type/flags/mask/SID, server end, byte type and one
instance. A collision fails without opening, adopting, repairing, disconnecting
or replacing the existing server. The owner retains Windows ownership rights;
the DACL is not a security boundary against that owner or system administrators.
Peer identity/signature validation still provides the process ownership check.

Three native fixtures cover owner read/write and ACL read-back, non-inheritance,
collision refusal and recreation after all handles close; restricted-token
read/write denial with an ordinary-owner positive control on the same endpoint;
and a shared existing endpoint whose descriptor remains identical and whose
server remains usable after refusal. The restricted token test is not a separate
user logon test. Remote-client refusal is configured through the native creation
flag, not measured from another host. Authenticated discovery, client-side endpoint connection,
signature verification, absolute I/O deadline/cancellation, production transport
and executable supervisor integration remain outstanding.

Read-back requires exactly PIPE_SERVER_END | PIPE_REJECT_REMOTE_CLIENTS (`0x9`)
and one instance. A regression rejects client/message pipes, missing remote
rejection, unknown flags and zero/multiple/unlimited instance limits. Windows
Server 2022 returns the remote-rejection bit for both private and shared fixtures.
The SDK defines PIPE_REJECT_REMOTE_CLIENTS as `0x8`, but GetNamedPipeInfo's
published contract documents only end/type flags; this read-back behavior is
native evidence for the tested platform, not a documented cross-version promise.
Unexpected values fail closed. CreateNamedPipeW uses only supported creation
flags; access rights such as READ_CONTROL are not added to dwOpenMode.

The exact-source native run on windows-2022 passed 45 `sidecar::` tests with zero
failures or ignored tests, including all three endpoint fixtures, the metadata
regression and both peer-identity fixtures:
https://github.com/BeFeast/tessera/actions/runs/37822215299 . Run/source SHA was
`a3f200239f68aa449a7dfb7cba1199de4eb2afa0`, tree
`e3bc443677aebcd11a640c490411bb3ecece5c73`, with no extra source/workflow commit.
The dispatch used package `tessera-sync-controller`, default features, filter
`sidecar::`, and one test thread. Toolchain: rustc 1.99.0
(b940084d7 2026-09-28), x86_64-pc-windows-msvc, LLVM 23.1.1.
The restricted token received ERROR_ACCESS_DENIED for read/write; the ordinary
owner succeeded on the same endpoint. Shared-endpoint refusal preserved its
security descriptor and usable server. This validates the endpoint and peer
primitives, not production transport or complete Sync acceptance.


### Windows client connection primitive (native acceptance pending)

`PrivateClient::connect` performs one open of the fixed local scoped name, with
no wait/retry, fallback path or wire I/O. It requests an overlapped,
non-inheritable handle and SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION:
even a colliding server may identify the caller but must not receive execution
impersonation rights. It checks the owner-private protected DACL through the
connected handle, byte/client-end/remote-rejection metadata and one-instance
limit, then verifies the captured live server process. The pipe and process
handles remain owned together. Missing/busy/shared or wrong-process endpoints
fail; the code never repairs their descriptor or adopts their process.

This is deliberately not a `Transport` implementation. Scope discovery and the
expected process still require trusted launch/signature/installation validation.
CreateFileW and identity/security queries are synchronous: there is no claimed
absolute timeout from checks before/after those calls. Bounded overlapped I/O,
cancellation completion/lifetime handling, production transport and supervisor
wiring remain the next work. Client metadata read-back uses the same tested-server
expectation for the undocumented returned remote-rejection bit, excluding the
server-end bit; its Windows-native fixtures must pass before acceptance.
