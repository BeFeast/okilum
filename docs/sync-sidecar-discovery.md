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
   SYNCHRONIZE)` at once. The pipe is connected, so the server is alive and the PID
   cannot yet have been reused; liveness is rechecked after the checks below.
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
as authority, a live process of the same user whose image lies under the staged runtime
directory after a "stopped" group downgrades the answer to `Stopping`.

## Signature policy

Verification is an injected `SignaturePolicy`, never a default no-op: an Authenticode
signer pin on Windows and a Team ID plus identifier requirement on macOS. The concrete
pins depend on the release signing identities, which this repository does not hold, so
they are an input to the implementation, not something it can invent. A development
policy that accepts a staged unsigned build exists only behind a compile-time `cfg` and
a pinned digest of the staged binary; production builds cannot select it at run time.

## Acceptance (native, none of it established by Linux tests)

Windows: connect to the real pipe of a signed fixture; refuse a same-user process of the
wrong image, a stale hint, a hint whose start time differs, and a restricted token; the
positive control is the genuine fixture on the same endpoint. macOS: the same set with
audit-token reuse simulated by a replaced process, and the process-group stop with a
descendant that stays in the group (reaped) and one that leaves it (reported as not
owned, answer `Stopping`, never `Stopped`).
