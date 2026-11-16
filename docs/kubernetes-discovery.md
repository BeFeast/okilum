# Typed Kubernetes discovery (#3009)

This slice exposes `okilum_core::log::kubernetes::discovery` for the future
native investigation workspace (#3010). It changes no native layout. The
existing `kubernetes::Request`, `Session`, log worker, leased stream assembler
and LogView keep their current/previous/follow behavior. Selecting a resource
is a model operation and never opens logs or starts discovery implicitly.

## Connection and request contract

`Kubectl::read(Query, &AtomicBool)` is synchronous worker work, never render
work. A read resolves the installed `kubectl` through the inherited PATH by
default. A trusted GUI caller can supply an executable path and optional PATH;
that PATH reaches kubectl and its credential plugins. KUBECONFIG, HOME and other
authentication environment variables remain inherited. An optional kubeconfig
path is one argv element, not a shell command. No credential settings are
created or changed; stdin stays closed.

`Query::Resolve` requires an explicit context and reads only
`kubectl --context=<context> [--kubeconfig=<path>] config view --minify --output=json`.
It retains context, cluster name, server, user identity name and the context's
namespace (or `default`), never tokens, certificates or exec configuration.
It never runs `use-context`. The next slice must present the resolved source
before reading resources. A namespaced query always supplies its namespace;
a namespace listing is optional and a denied listing does not block a typed
namespace. Successful namespaced access is established by that read, not by
membership in a namespace picker.

`Query::List`, `Get` and `RelatedEvents` have a fixed kind enum: Namespace,
Pod, Deployment, ReplicaSet, Service, EndpointSlice, core Event and HPA.
Namespace is cluster-scoped; all others require an explicit namespace. The
argv uses explicit context and cluster flags, `--request-timeout=15s`, `get`
and one generated `--raw=<path>` for the kind's fixed API version. Namespaces
and optional names are validated path segments. Lists request one server page
with `limit=500`; related Events add only a URL-encoded UID field selector. Names and UIDs
cannot become arbitrary flags or commands. No shell, describe/table parsing,
resource mutation, raw command text, terminal, exec or port-forward exists.

Connection identity records a kubeconfig path and resolved endpoint, not a
cryptographic identity or immutable kubeconfig revision. External edits to the
same kubeconfig/context can change what its next invocation reaches. The UI
must re-resolve after connection changes and surface replacement/ambiguity;
it must not imply that the model locks external kubeconfig files.

## Evidence model

Every `Observation` contains its exact query/source, start/observation wall time
and typed data. Resource identity includes the resolved Connection, namespace,
kind, name and UID. `Metadata.resource_version` and list resourceVersion are
opaque strings: compare equality only; never parse or order them. A replacement
UID is another resource even when its name is unchanged. Observations from two
contexts cannot share a source state.

Pod facts separate regular, init and ephemeral containers. Native sidecars are
init containers with `spec.restart_policy == Some("Always")`. Each spec is
matched to status by name within its own role; absent status is unknown.
Duplicate or unmatched statuses fail parsing rather than choose a winner.
Current and last state retain waiting reason/message, running start time,
termination exit code/signal/reason/message/start/finish, readiness/started,
restart count, image/imageID/containerID, requests/limits and all supplied
startup/readiness/liveness probe facts. A recovered Running/Ready container
keeps its previous OOMKilled. A Pod reason never becomes a sidecar's reason or
restart count. Missing facts remain `None`, not healthy zero.

Workloads retain replica facts, conditions and selectors (including expression
selectors). Services retain selectors and ports; EndpointSlices retain target
references with UID and ready/serving/terminating conditions. Ownership,
selector matching and endpoint membership are separate evidence, not inferred
interchangeably. Events retain involvedObject UID and timestamps; an event
returned for another UID is refused. HPA retains target, desired/current
replicas, conditions and versioned metric payloads. Missing metrics do not
establish zero utilization or healthy autoscaling.

## Limits, freshness and failure

Each invocation has a 20 s whole-operation deadline, checked with cancellation
every 20 ms during transport and once after bounded JSON parsing. Parsing is
not preempted mid-document. Stdout is bounded to 8 MiB, stderr to 16 KiB plus an explicit
truncation flag. The existing Session adds at most 16 queued 16 KiB chunks
and one chunk per pipe. Typed JSON parsing has the standard serde recursion
bound. Its temporary tree and decoded model add bounded allocation overhead;
the byte ceiling is an input ceiling, not an exact total heap budget.

At most 500 resources are exposed. A longer list is `ItemLimit` partial; a
server continuation token is `ContinueToken` partial. The adapter never follows
that token, and fixed raw API reads prevent kubectl's automatic pagination.
The byte ceiling and deadline also bound servers that ignore the page limit;
there is no retry or continuation request in the adapter. A byte limit yields no new observation;
it does not turn a JSON prefix into usable complete evidence.

Successful empty Resources with no partial reasons is a known empty result.
Forbidden, Authentication (including expired credentials), InteractiveLogin,
NotFound, Unavailable, InvalidJson, Limit, Cancelled, Timeout and Io are separate failed attempts.
Structured Kubernetes Forbidden/Unauthorized Status has priority; otherwise
kubectl stderr classification is necessarily wording-dependent. Unknown
nonzero failures remain Unavailable and keep their bounded diagnostic. An
interactive credential/MFA renewal must be presented promptly using the
failure diagnostic; the adapter does not answer credential prompts or retry.
Successful stderr warnings are retained separately from resource facts.

`SourceState` retains the last successful observation and the latest failure
for exactly one query. Failure leaves old facts visible with stale status;
`is_stale(now, max_age)` also supports caller-selected source age. A successful
empty refresh replaces old rows. Monotonic read-start sequence prevents an
older read that finishes late from replacing a newer attempt, even if the
wall clock moves. Slice 2 must cancel superseded requests and retain independent
source states for Pods, Events, Services, HPA and each selected object/query.

Cancellation before spawn starts nothing. Cancellation, deadline, byte-limit,
read failure, parse completion and Drop close the receiver, terminate if
necessary, and reap the direct kubectl child through the existing `Session`.
They do not own credential-helper descendants. An escaped/helper process can
survive and hold a pipe/reader thread after cancellation; the caller does not
wait for those threads. Killing/reaping the direct child can still be delayed
by OS process state (for example uninterruptible I/O): the 20 ms poll interval
is not a hard cleanup latency guarantee. No process-group/job-object cleanup
is claimed.

## Next slice's log boundary and acceptance

Selection, discovery source and a pinned log collection source must be separate
native state. Only explicit Start builds/opens the existing log Request for an
explicit context, namespace, Pod name, container and previous/follow choice.
The connector remains name-addressed: a discovered UID does not pin what
`kubectl logs` reaches. The next slice must surface replacement races and never
silently fall back from missing previous logs to current logs.

Synthetic controls cover source/UID isolation, per-container history, all
resource projections, denied namespace listing with successful namespaced
access, malformed/oversized data, partial limits, stale retention, owned-child
cancellation/Drop/deadline, option injection and previous-instance absence.
The prescribed committed-HEAD fast gate compiles these tests with workspace
clippy; it does not execute Rust tests. Runtime regression execution, real
kubeconfig/exec-plugin GUI launch, real API behavior and native Workspace
acceptance are separate evidence. This slice installs nothing and performs no
corporate cluster request. No source/CI result establishes native acceptance.
