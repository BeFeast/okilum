# Authenticated supervisor discovery and tree ownership

Design slice for #588, pending one review before implementation. It covers how the
controller finds the live supervisor's generation and a trustworthy process identity,
and what "the owned tree has exited" means on macOS. The Stop authority itself is
unchanged: only the journal token authorizes a stop (see
[stop operations](sync-sidecar-stop-operations.md)). Discovery establishes *who is
listening*, never *what may be stopped*. Linux's service path is out of scope.

## Principles

- Nothing discovered is authority. A hint is a claim that is checked against an OS
  object before it is used, and a wrong or stale hint fails closed (no connection).
- No PID file, wire claim or public record establishes identity. A PID is used only
  immediately after a kernel report from a connected endpoint, to open a handle that is
  then verified and retained.
- The peer must be the same user, run the executable in `Binding.supervisor`, satisfy
  the signature policy below, and be the process that owns the connected endpoint.

## Generation hint

The supervisor already names its endpoint with a fresh random generation, so a client
needs the generation to connect. At startup, after creating the endpoint, the
supervisor publishes `endpoint.json` in the private state directory with the same
owner-only rules as the journal: `{ schema, generation, started }`, where `started` is
the supervisor's own process start time. It is replaced atomically and removed on a
clean exit; it is never part of the lifecycle envelope, carries no revision, and the
supervisor never writes `sidecar.json`. The controller reads it with the store's
`StateDir` helpers, builds the endpoint name from it, and treats a missing, malformed,
stale or unconnectable hint as "no live supervisor".

## Windows: connect, then verify

1. Open the private pipe by name (owner-only DACL, local-only, one instance; these
   server-side properties are already checked on creation and read-back).
2. `GetNamedPipeServerProcessId`, then `OpenProcess(QUERY_LIMITED_INFORMATION |
   SYNCHRONIZE)` at once. The kernel reports the PID of the pipe's server end, but the
   server could exit and its PID be reused between that report and `OpenProcess`, so the
   PID alone is never trusted. What closes this is step 3's start-time comparison with
   the hint (a reused PID belongs to a process that started later) together with the
   liveness and PID recheck in step 4 on the still-connected pipe.
3. On that handle: token user SID equals `Binding.owner`; the image path from
   `QueryFullProcessImageNameW` equals `Binding.supervisor` (canonical, case-insensitive);
   the process start time from `GetProcessTimes` equals the hint's `started`; the image
   file passes the signature policy.
4. Build `ProcessPeer::from_verified_process`, run the existing `verify_pipe`, and keep
   handle and pipe through the whole exchange.
5. The first request is `Status`. The reply's scope must equal the hint's generation;
   a Stop is then sent only for a journal operation whose scope names that generation.

Residual, to be measured in native acceptance rather than assumed away: the image file
could be renamed and replaced after launch. Mapped images cannot be written or deleted,
and the signature is checked on the path the OS reports at connect time.

## macOS: connect, then verify

The helper is launched by launchd (SMAppService). It binds a Unix-domain socket in the
private state directory named by generation, mode 0600, and the controller connects
after reading the hint. Paths longer than the platform's `sun_path` limit fail closed;
that limit is checked at preparation, not discovered at Stop time.

After connect the controller reads `LOCAL_PEERCRED` (effective uid equals the current
user) and the peer's audit token (`LOCAL_PEERTOKEN`), which includes the PID version, so
reuse cannot be mistaken for the same process. The signature check is
`SecCodeCreateWithAuditToken` plus `SecCodeCheckValidity` against a requirement built
from the signature policy, and the code's path must equal `Binding.supervisor`. The
audit token, not a PID, is what is retained through the exchange. The server side checks
the controller the same way before accepting a Stop.

## macOS: what "owned tree stopped" means

There is no Job Object. The supervisor spawns the runtime as the leader of a new
process group and owns: the root (waited and reaped), and every process still in that
group. `Stopped` means the root was reaped and the group has no member, checked after a
`SIGTERM`, a bounded wait and a `SIGKILL`. A process that deliberately leaves the group
(`setsid`, double fork) is not owned and cannot be certified. The runtime is launched
with the fixed argv (`--no-restart --no-upgrade`) and does not do that; the contract is
stated here so nothing treats it as a kill-on-close guarantee. As defence in depth, not
as authority, a live process of the same user whose image is exactly the pinned runtime
executable (`Runtime.location`, not merely something under its directory) and whose start
time is not earlier than the owned root's downgrades the answer to `Stopping`. A process
that predates the root, or runs any other image, never counts, so a reused directory or
an unrelated install cannot block removal. The downgrade fails closed and is reported
with that process's identity so it can be resolved; the controller never kills or adopts
it. A user who starts a second copy of the same executable by hand after the supervisor
started will see exactly this blocked state until it exits, which is the intended
conservative answer.

## Signature policy

Verification is an injected policy, never a default no-op, and no signing identity is
written into code or tests. Inputs, as decided for the release pipeline:

- macOS: the Team ID is the release-signing value (`OKILUM_SIGNING_TEAM_ID`, see
  [macOS auto-update](macos-auto-update.md)), read at release build time. The helper's
  bundle identifier is derived from the same build configuration as the app's, not
  pinned as a string: it is `com.befeast.okilum` today, and a rebrand will change it.
- Windows: the Authenticode policy is built on the certificate **subject plus the
  issuer chain** of the code-signing authority, never on a thumbprint, because the
  thumbprint changes at every yearly renewal. The certificate is not issued yet; its
  values are supplied when it exists.

Until then, unsigned development builds are accepted only behind a compile-time `cfg`
and a pinned digest of the staged binary; production builds cannot select that policy
at run time. Native acceptance of the Windows signature check follows the certificate;
the macOS one can come earlier.

## Acceptance (native, none of it established by Linux tests)

Windows: connect to the real pipe of a signed fixture; refuse a same-user process of the
wrong image, a stale hint, a hint whose start time differs, and a restricted token; the
positive control is the genuine fixture on the same endpoint. macOS: the same set with
audit-token reuse simulated by a replaced process, and the process-group stop with a
descendant that stays in the group (reaped) and one that leaves it (reported as not
owned, answer `Stopping`, never `Stopped`).

## Concrete macOS connection (#588, stage 7)

`macos::connection::NativeConnection::open` combines the verified current bundle,
existing private Unix store, saved installation/instance/device binding, and the
Unix connector with `SignedPeer` authentication. Both code requirements come from
the caller's build policy; the factory has no permissive peer or transport
fallback. Opening refuses an absent/mismatched lifecycle journal and never creates
state or registers/starts a service. Open it outside a controller transaction.

`inspect` revalidates the bundle, then uses the lock-free atomic hint read so a
controller can inspect while holding its own transaction. It returns `Absent`,
`Unreachable(reason)` or `Live(scope)` distinctly. Absent means no published hint,
not proof of process exit. Unreachable never silently becomes Stopped.

`request_stop` checks the current journal's exact revision/epoch-bound token in a
short transaction and releases the lock before authenticated IPC. One absolute
deadline spans bundle checks, journal wait and authenticated exchange; it is not
renewed between these phases or per byte. Filesystem/signature calls themselves
are synchronous observations, not forcibly interruptible OS calls. The supervisor
still revalidates authority itself. Changed generation, stale authority, incomplete
Stop or failed authentication remain errors. Success is a protocol acknowledgement
of the owned-tree stop; it does not confirm supervisor-process exit or authorize
service unregistration. The native lifecycle adapter must separately confirm exit
and the lifecycle controller must revalidate the current journal after the reply.
No PID killing or service-unregistration fallback exists in this connection.

Portable tests inject a framed transport and signature verifier to exercise these
boundaries, including inspection while the journal is locked. Hosted macOS opens
a real ad-hoc signed disposable app/helper through the concrete factory, observes
the absent hint without launching them, and refuses a wrong helper requirement.
Existing native audit-token/socket tests cover signed peer authentication. This
is not a native login/sleep/stop/uninstall acceptance claim; OwnedSupervisor,
pinned Syncthing preparation and desktop orchestration remain work under #588.

## Authenticated supervisor exit observation (#588, next slice)

`supervisor::ipc::macos_exit::ExitWatch` captures a kernel `EVFILT_PROC/NOTE_EXIT`
observation from the connected, expected signed supervisor. It authenticates UID,
audit token, code requirement and executable path before subscribing, then repeats
authentication and compares the full audit token after subscription. A PID reused
during capture cannot substitute a different process. There is no API accepting a
caller-supplied PID and no signal, process adoption, registration or unregistration.
The queue descriptor is close-on-exec and closes when the observer is dropped.

Waiting uses one caller-supplied absolute deadline, including across EINTR.
A timeout means exit was not confirmed; it does not assert liveness or authorize
cleanup. A confirmed event remains confirmed on repeat reads. This only certifies
the captured supervisor process exited: the lifecycle layer still needs an
authenticated owned-tree Stop acknowledgement and current durable authority before
unregistering. Missing sockets/hints and arbitrary process scans are not substitutes.

The macOS tests start only an owned subprocess of the test executable, connect over a
scratch private socket and prove that the intended helper fixture ran before
capturing it. A live-child timeout is the positive control before the exact child
exits; another test refuses the wrong signing requirement and confirms that dropping
the observer leaves the child alive. Cleanup kills/waits only the spawned child.
These are process-observer fixtures, not launchd/login/update or complete Sync QA.

A deterministic capture-boundary regression exits the child after event registration
and before the second signature check. It first verifies the original socket audit
token remains readable and unchanged, then requires capture refusal. The second
`SignedPeer` check performs a fresh `SecCodeCopyGuestWithAttributes` lookup using
`kSecGuestAttributeAudit`; token equality alone is never liveness evidence. This
exercises a stale connected-socket credential, not a forced PID-reuse stress run.
