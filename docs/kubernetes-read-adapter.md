# Kubernetes typed read adapter (#3009)

Slice 1 of 4 of the Kubernetes investigation workspace (epic #3008). This is
the model and transport the native workspace (slice 2) builds on. It has no UI
of its own and changes no existing UI. Code: `crates/okilum-core/src/kubernetes/`
(`okilum_core::kubernetes`).

## What it runs

Every read is one `kubectl` process with a fixed argument vector and no shell:

| Read | argv |
|---|---|
| Contexts | `config view --output=json --raw=false` |
| List / get / related events | `--context=<ctx> --request-timeout=<n>s get --raw=<path>` |

`<path>` is built only from a `Kind` (fixed API group/version and resource),
a validated namespace (DNS label), a validated object name (DNS subdomain) and
encoded query parameters (`limit`, an equality `labelSelector`,
`fieldSelector=involvedObject.uid=<uid>` for events). No user text becomes a
flag, verb or path segment without that validation. The adapter never runs
`describe`, never parses table output and has no general command runner.
Nothing writes cluster resources or kubeconfig: there is no `use-context`,
`set-context` or credential write.

Kinds: Namespace (cluster scope), Pod, Deployment, ReplicaSet, Service,
EndpointSlice (`discovery.k8s.io/v1`), core/v1 Event and
HorizontalPodAutoscaler (`autoscaling/v2`; a cluster without it answers
`NotFound`).

## Connection, context and namespace

`Client::contexts` reads kubeconfig as kubectl redacts it and keeps only
context, cluster, server and namespace names; users, tokens, certificates and
exec settings are not retained. `Contexts::resolve(None)` captures the current
context once; from then on every read passes `--context=<name>` explicitly, so
a `kubectl config use-context` elsewhere cannot move an open investigation to
another cluster. `current-context` is never changed.

`choose_namespace` takes a typed namespace, the context's namespace, or
kubectl's `default`, and records where it came from (`NamespaceOrigin`). A
namespace listing is optional: when it is forbidden (common for namespaced
roles) the typed namespace is still used and namespaced reads decide whether
access exists (`TypedListingDenied`). A complete listing that lacks the typed
name is `TypedNotListed`; a partial one is `TypedListingPartial`.

## Model

Every value is an `Observation { source, observed_at, value }`. `Source` is
context + cluster + server + namespace + kind. `observed_at` is when the
response arrived on this machine. Each object keeps `Meta`: name, namespace,
UID, opaque `ResourceVersion` (equality only, never ordered or parsed),
generation, creation/deletion time, labels and owner references.

Identity is `ResourceId { context, namespace, kind, name, uid }`.
`ResourceId::compare` returns `Same`, `Recreated` (same context/namespace/kind/
name, new UID) or `Different` (including the same name, or even the same UID,
in another context). `related_events` refuses an object from another context
and selects events by UID, so events of a deleted predecessor with the same
name are not mixed in.

Pods list containers explicitly by `ContainerKind`: `Init`, `Sidecar` (init
container with `restartPolicy: Always`), `Regular`, `Ephemeral`. Statuses are
matched by name within the same list only. Each container keeps current and
last `State` (waiting reason/message; running start; terminated reason,
message, exit code, signal, start/finish), ready, started, restart count, spec
image, reported image and image ID, requests and limits, and startup/liveness/
readiness probes (HTTP path/port/scheme, TCP port, gRPC port/service; an exec
probe keeps only its argument count, never the command). `Container::health`
derives only from that container's own status; the Pod phase or a crashing
sibling never changes a healthy sidecar's health. A container without a
reported status is `Unreported`. `previous_instance_observed` is true only when
`lastState` is a termination.

Workloads keep replicas/ready/available/updated/unavailable, observed
generation, selector and conditions; ReplicaSets their `pod-template-hash` and
owner. Services keep type, cluster IP, selector and ports. EndpointSlices keep
their service (label), address type, endpoints (addresses, ready/serving/
terminating, node, target reference with UID) and ports. Events keep the
regarded object (with UID), type, reason, message, count, first/last/event time
and reporter. HPAs keep target, min/max/current/desired replicas, last scale,
conditions and current metric facts (`93%`, `930m`, ...).

## Bounds and outcomes

- Lists read one server page: `limit` 1–1000 (default 500). A `continue` token
  means `Completeness::Truncated { remaining }` (server `remainingItemCount`
  when given). A server that ignores `limit` is cut to it and marked truncated.
- Stdout is bounded (default 16 MiB); beyond it the process tree is ended and
  the result is `Failure::Oversized`, nothing from it is kept. Stderr keeps at
  most 16 KiB: its first 4 KiB and its newest bytes, so kubectl's final error
  line survives any amount of earlier warnings. Each invocation has a 45 s deadline and passes a 20 s
  `--request-timeout`.
- Items that fail to decode (for example without a UID or resourceVersion) are
  reported in `List::rejected` (first 20) and `rejected_total`, never dropped
  silently. `List::is_empty` is a successful read that found nothing;
  `is_complete` is false for truncated or partially rejected lists.
- `Failure` separates: `Forbidden`, `AuthExpired` (401, expired tokens, failing
  credential helper), `InteractiveLogin` (an exec plugin needs a terminal,
  browser, device code or MFA: surfaced at once so the user signs in outside
  Okilum), `NotFound`, `Unavailable` (kubectl missing from PATH, network, TLS,
  server overload), `Configuration` (unknown or unset context),
  `InvalidRequest` (refused before running kubectl), `Malformed` (exit 0 but
  not the expected JSON kind), `Oversized`, `TimedOut`, `Cancelled` and
  `Failed` (any other exit, with code and bounded stderr). The explicit server
  reason (`Error from server (Forbidden)`, `... to the server (Unauthorized)`,
  the last one printed) decides first; only client-side errors without one are
  matched on kubectl's wording, with quoted names ignored, so an object called
  `"unauthorized"` cannot turn `NotFound` into `AuthExpired`. An unknown
  message stays `Failed`, never success.

## Freshness, retention and cancellation

`Slot<T>` holds one source. A failure keeps the previous value as
`View::Stale { why: Failed { failure, at } }`; a value older than the caller's
bound is `Stale { why: Aged }`; a failure with nothing earlier is
`View::Failed`. A cancelled read changes nothing. A value from another source,
or older than the one held, is refused. Slots are per source, so a forbidden
Events read never marks the Pods of the namespace stale.

`Cancel` is shared with the caller. A pre-cancelled read runs nothing; a running
one ends within one poll interval (20 ms).

## Processes and credentials

kubectl is resolved by `kubectl_command()`, the same function the log
connector uses: the OS resolves `kubectl` against the PATH the application
received. Nothing rewrites PATH, KUBECONFIG or HOME, so kubeconfig exec
credential plugins behave as they do for the log connector. Stdin is closed:
an exec plugin that requires interactive input fails and is reported as
`InteractiveLogin`/`AuthExpired`.

Owned-child cleanup: the direct kubectl child is always reaped, including on
panic. On Unix it is spawned as the leader of its own process group and its
pipes are read nonblocking on the calling thread: no reader thread outlives a
read, and both pipe ends are closed when it returns, even if a helper
descendant still holds stderr. Completion is "stdout at EOF and kubectl
exited"; stderr is then drained to what kubectl wrote, without waiting for
descendants. On cancel, deadline or the stdout limit kubectl has not been
reaped yet, so the whole group is killed (even when kubectl itself has already
exited) and then kubectl is reaped. `Raw::cleanup` reports what happened.
Limits, surfaced here and in `Cleanup`:

- A credential-helper descendant that moved to another process group or session
  (`setsid`, daemonizing token caches) is not reached.
- When kubectl exits by itself nothing is signalled; descendants it leaves are
  not ended.
- On Windows only the direct kubectl child is killed (`DirectChildOnly`);
  helper descendants are not tracked. Pipes are read by one thread each; after
  kubectl exits stderr gets 250 ms to close, and a reader whose pipe a
  descendant holds open ends only when that descendant closes it.

## Log connector

`log::kubernetes` (docs/kubernetes-log-connector.md) is unchanged: current,
previous and follow behave as before. `log_request(observation, container,
previous, follow, tail)` only builds its `Request` from an observed Pod (explicit
context and namespace); it never opens a session, so selecting a row starts
nothing. Asking for `previous` when no previous termination was observed is a
`NotFound` failure before kubectl runs.

## Verification

Synthetic only (`crates/okilum-core/src/kubernetes/tests.rs`): no cluster,
kubeconfig or credentials. A fake transport covers argv shape, refused unsafe
input, redacted contexts, denied namespace listing with valid namespaced
access, cross-context duplicate names, recreated UID and UID-scoped events,
per-container status (failing app container beside a healthy sidecar, init,
ephemeral, unreported), previous-instance absence, workload/service/slice/HPA
facts, empty vs truncated vs rejected lists, malformed/oversized/timed-out/
cancelled output, stderr classification and stale retention. A local `sh`
fixture standing in for kubectl covers exact argv delivery (with a negative
control), bounded stderr, the stdout limit, the deadline, a descendant holding
stderr, and (Linux) cancel killing a process-group descendant.

Not established by these tests: behaviour against a real API server or a real
exec credential plugin, and any native UI (slice 2).
