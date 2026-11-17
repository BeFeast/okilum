# Kubernetes Workspace (#3010)

Slice 2 of 4 of the Kubernetes investigation workspace (epic #3008). A native
GPUI window over the typed read adapter (docs/kubernetes-read-adapter.md,
#3009) and the existing Kubernetes log worker and LogView
(docs/kubernetes-log-connector.md). It follows the approved Workspace
reference (01-workspace.html): scope strip, searchable resource list,
inspector, log dock and a status line, with the GUI V2 neutral tokens, Noto
Sans for UI text and Cascadia Code for identifiers and logs. Triage and
Service path are slice 3.

Code: `crates/okilum-shell/src/kubernetes_workspace.rs` and
`kubernetes_workspace/` (`model.rs` pure model, `backend.rs` reads,
`inspector.rs`, `dock.rs`). Entry: Logs section → **Kubernetes workspace…**;
**Kubernetes logs…** (the explicit-target panel) is unchanged.

## What runs when

| Action | kubectl |
|---|---|
| Opening the window | `config view --output=json --raw=false` (context names only; no cluster request, no credential plugin) |
| Load / Refresh | one bounded `get --raw` page each of Pods, Deployments, ReplicaSets, Services, EndpointSlices, Events, HPAs; plus the namespace listing when a namespace was typed |
| Selecting a row, a section, a tab, a relation link, typing in the filter, the `J`/`K`/`/` keys | nothing |
| Open logs / Previous logs / Start | one `get --raw` of the pinned Pod (UID check), then `kubectl logs` of that target |

Every read passes the resolved context explicitly. Reads stop at the first
auth, interactive sign-in or kubeconfig failure, which is shown at once in the
banner (an MFA/browser step is surfaced, never deferred). A forbidden source
fails only itself.

## Scope

Context is a choice from kubeconfig (`current-context` preselected, never
changed). The namespace is typed; empty uses the context's namespace or
kubectl's `default`. A denied namespace listing does not block a typed
namespace; the banner says how the namespace was confirmed. Choosing another
context or namespace changes nothing until Load. A scope change replaces the
shown data and clears the selection; it never touches the log dock.

## List

Sections: Pods, Containers, Deployments, Services, each with its count; the
filter matches name, status, named reasons and images. Rows show readiness
fractions, restart counts and named reasons as separate facts. A Pod's reasons
carry their container's name (`api: last: OOMKilled (exit 137)`), so a sidecar
never appears to share its neighbour's failure; Container rows show only
their own state, readiness, restarts and last termination. READY counts
regular containers and native sidecars, as kubectl does. Status uses text and
a dot, not color alone.

Empty states are distinct: not read yet, a failed read (with the failure
kind), and a successful read that found nothing.

## Inspector

- **Overview**: Pod facts and per-container cards (state, *Last state* labelled
  as container status, not a Kubernetes Event, image, Open logs / Previous
  logs); container details with image ID, requests/limits and probes (exec
  probes show only their argument count); Deployment replicas, conditions,
  HPA facts (missing metrics are *unknown*, a failed HPA read is *unknown*,
  not "no HPA"), owned ReplicaSets and Pods; Service ports, selector and
  endpoints.
- Relations are separate and labelled: **Owned by** follows `ownerReferences`
  only when an observed object has exactly that UID (an unobserved or
  mismatched UID is shown as recorded, never linked); **Selected by** is label
  selection; **EndpointSlice membership** matches `targetRef.uid`. A Service
  never owns Pods. Unreadable EndpointSlices make endpoints *unknown*, never
  zero. Relation links select the object and switch the section.
- **Events**: Kubernetes Events grouped by involved object, type and reason
  (count summed, newest message), each naming its involved object. A
  Deployment includes its ReplicaSets' and Pods' events. Events are selected
  by UID, so an earlier object with the same name is not mixed in.
- **Manifest**: read-only YAML of the typed fields the adapter retains. It is
  not the server's complete object: environment values, exec commands,
  secrets and a Deployment's pod template are not retained.

Selection is by UID. When a later read no longer has that UID, the inspector
says so and offers a same-named replacement without substituting it.

## Log dock

Three things are independent: the selected resource, the pinned session and
the viewport.

- **Open logs** pins context, namespace, Pod name and UID, container and
  current/previous instance. Previous is offered only when the observation
  shows a last termination.
- Before kubectl starts, one fresh read of the Pod compares its UID with the
  pinned one. A different UID stops there (*Not started: Pod replaced*); nothing
  reads logs by that name.
- `kubectl logs` addresses the Pod by name, not UID. The dock states when the
  UID was verified, and after each Refresh compares the pinned UID with the
  latest Pods read: a replacement or disappearance is shown as a warning; the
  session is not silently stopped.
- Selecting rows, switching sections and changing context/namespace keep the
  session and its loaded rows. A different target while a session runs or
  holds rows asks to **Retarget** (or keep the current one); the old rows stay
  until the new session sends data, labelled with the target they came from.
- Start/Stop control receiving. **Follow bottom** only keeps the newest row in
  view; scrolling up pauses it while rows keep arriving, and the viewport
  stays where the reader left it. **Stream new lines** (`--follow`) applies to
  the next start.
- States: Verifying, Receiving, Stopping, Ended (kubectl exited by itself),
  Stopped, Stopped at a loading limit (8 MiB / 20,000 lines / a 64 KiB line /
  invalid UTF-8; the worker now reports these as `Truncated` instead of a
  generic failure), Disconnected (a streamed session failed after data),
  Failed, Not started: Pod replaced. kubectl stderr stays a bounded diagnostic
  line, never a log row. Filter, marks, export and the parser are the
  ordinary LogView's.

## Lenses: Triage and Service path (#3011)

A switch at the start of the filter bar selects **Workspace**, **Triage** or
**Service path**. They are views over the same data, selection, inspector and
log dock: switching reads nothing, keeps the selection and the inspector tab,
and never touches the pinned session. Relations in both lenses select objects
in the shared inspector; logs start only from its explicit Open logs /
Previous logs, with the usual retarget confirmation.

### Triage

Rows are reviewable observations, one per container, Pod or Deployment, each
listing the rules it matches with the object's own timestamp when it reports
one. The rules are shown in the view:

1. A container waits with a reason other than ContainerCreating or
   PodInitializing.
2. A container's current instance terminated with a non-zero exit code.
3. A regular or sidecar container runs but is not ready.
4. A container's last termination was non-zero. This stays while the kubelet
   reports `lastState`, also after the container is Ready again.
5. A Pod's phase is Failed, Unknown or Pending.
6. A Deployment reports unavailable replicas or Available=False.

Order: failing now, not ready, recovered with a prior failure kept; then the
newest reported time; then name. Selection is by object key (container =
Pod UID + name), so a refresh that reorders rows keeps the selected one. There
is no acknowledge or dismiss lifecycle: a row leaves only when a later read no
longer matches a rule. If the selected object no longer matches, it stays
selected and the band says it is not marked resolved.

The summary states its scope, the Pods read time and denominators: Pods
Ready `r / n` (readiness, not application health; `n` is marked "first page"
for a truncated list), Pods with observations `a of n`, Restarts as a
cumulative counter (not incidents) and Metrics "Not read" (no metrics API is
used). A failed Pods read with nothing earlier is "attention unknown, not
zero"; a failed read after an earlier one keeps those observations and says
they are stale.

### Service path

Choose a Service in the list. The band shows:

- **Service**: selector (or "no selector: endpoints are managed outside this
  Service") and each `port/protocol → targetPort`.
- **EndpointSlices** of that Service (`kubernetes.io/service-name`), with
  ready/total, address type and ports. An unreadable EndpointSlices source is
  *unknown, not zero*; a readable list without a slice is *zero endpoints*;
  a first page or undecodable items are marked partial; a failed read after
  an earlier one is marked stale.
- **Endpoints** with addresses and ready/serving/terminating, and their
  `targetRef`, matched by UID: a target Pod with its status and its
  containers, or a target that is not among the shown Pods. What that means
  follows the Pods source: "deleted or replaced" only after a complete,
  current Pods read; "the Pods read was partial" for a truncated list;
  "Pods are stale" after a failed read with earlier data; otherwise "unknown"
  (Pods not read, or denied: EndpointSlices can be readable while Pods are
  forbidden). Names are never used to match. An endpoint Pod the selector no longer matches, and
  Pods the selector matches that are not endpoints, are listed as changing
  membership.
- **Ownership**, labelled separately from routing: Deployment → ReplicaSet →
  Pod through `ownerReferences` with matching UIDs, and the Deployment's HPA
  facts (or "HPA unknown (not readable)").

The path stays on its Service while its Pods and containers are inspected. A
Service that disappears from a later read is reported, not replaced by a
same-named one. There is no general graph canvas.

## Keys

Only while the resource list has focus: `J`/`↓` next, `K`/`↑` previous, `L`
current logs of the selected container (or a Pod's only regular container),
`/` focus the filter. Inputs and the LogView keep their own bindings. There is
no Workspace command palette in this slice.

## Verification

Synthetic only: no cluster, kubeconfig or credentials, and no real kubectl.
`kubernetes_workspace/model_tests.rs` covers rows, readiness/restarts/reasons
per container, a recovered Ready Pod keeping its previous OOMKill beside a
sidecar with zero restarts, UID-matched ownership, selection vs membership vs
unknown endpoints, grouped events and earlier-UID exclusion, log target UID
checks, selection by UID, empty vs forbidden vs unread, filtering and the
typed manifest. `kubernetes_workspace/tests.rs` drives the window with a
synthetic transport and a process-free log worker: opening reads only
contexts; selection, sections, scope changes and keys never run kubectl or
retarget; Open logs verifies the UID and starts with the exact argv; a
replaced Pod starts nothing; a replacement after start is disclosed; Follow
bottom and receiving are independent; retarget needs confirmation and keeps
old rows until new data; forbidden vs empty; interactive sign-in stops reads;
layout regions in light and dark.

`kubernetes_workspace/lenses_tests.rs` covers the Triage rules, order and
denominators (a recovered Ready Pod keeps its OOMKilled; restarts are
cumulative; a healthy namespace has none; unread/denied/stale differ; keys
survive reordering) and the Service path (ports, ready/total, a missing target
by UID, selected-but-not-endpoint, endpoint-no-longer-selected, zero vs
unknown vs stale endpoints, no selector, ownership through UIDs). The GPUI
tests drive both lenses: a stable Triage selection across refresh, a row that
stops matching is not resolved silently, Previous logs from Triage; path
clicks select in the shared inspector, read nothing and never pin logs; the
explicit pin and retarget; layout of both lenses in light and dark after the
app's own theme path, with inspector tab clicks after the switch.

Not established by these tests: a real API server, an exec credential plugin,
native visual review on macOS/Windows/Linux builds, accessibility and
performance with large namespaces. Those are native acceptance (#3012).
