//! Typed facts decoded from Kubernetes API JSON (never from table or describe
//! text). Only fields the investigation workspace shows are kept; Secrets,
//! environment values and exec probe commands are not retained.
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::BTreeMap;

/// Opaque server token. It is compared only for equality, never ordered or parsed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceVersion(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerRef {
    pub kind: String,
    pub name: String,
    pub uid: String,
    pub controller: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub name: String,
    pub namespace: Option<String>,
    pub uid: String,
    pub resource_version: ResourceVersion,
    pub generation: Option<i64>,
    pub created: Option<String>,
    pub deleting_since: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub owners: Vec<OwnerRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition {
    pub kind: String,
    pub status: String,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub last_transition: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectRef {
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    pub uid: Option<String>,
}

// ---------- Pods and containers ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContainerKind {
    /// `spec.initContainers` that run to completion before the regular ones.
    Init,
    /// `spec.initContainers` with `restartPolicy: Always` (native sidecars).
    Sidecar,
    Regular,
    /// `spec.ephemeralContainers` (debug containers).
    Ephemeral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Waiting {
        reason: Option<String>,
        message: Option<String>,
    },
    Running {
        started_at: Option<String>,
    },
    Terminated {
        reason: Option<String>,
        message: Option<String>,
        exit_code: i32,
        signal: Option<i32>,
        started_at: Option<String>,
        finished_at: Option<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resources {
    pub requests: BTreeMap<String, String>,
    pub limits: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeHandler {
    Http {
        path: Option<String>,
        port: String,
        scheme: Option<String>,
    },
    Tcp {
        port: String,
    },
    Grpc {
        port: i32,
        service: Option<String>,
    },
    /// The command itself is not retained (it may embed secrets).
    Exec {
        arguments: usize,
    },
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub handler: ProbeHandler,
    pub initial_delay_seconds: Option<i32>,
    pub period_seconds: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub success_threshold: Option<i32>,
    pub failure_threshold: Option<i32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probes {
    pub startup: Option<Probe>,
    pub liveness: Option<Probe>,
    pub readiness: Option<Probe>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub kind: ContainerKind,
    pub name: String,
    /// The image the spec asks for.
    pub image: Option<String>,
    /// The image the kubelet reports running, and its resolved digest/ID.
    pub status_image: Option<String>,
    pub image_id: Option<String>,
    /// False when the Pod status has no entry for this container yet.
    pub status_reported: bool,
    pub ready: Option<bool>,
    pub started: Option<bool>,
    pub restart_count: Option<u32>,
    pub state: Option<State>,
    pub last_state: Option<State>,
    pub resources: Resources,
    pub probes: Probes,
}

/// This container's own condition. It is derived only from this container's
/// status, never from the Pod phase or from a sibling container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    Unreported,
    Running {
        ready: Option<bool>,
    },
    Waiting {
        reason: Option<String>,
    },
    Completed,
    Failed {
        reason: Option<String>,
        exit_code: i32,
    },
}

impl Container {
    pub fn health(&self) -> Health {
        match &self.state {
            None => Health::Unreported,
            Some(State::Running { .. }) => Health::Running { ready: self.ready },
            Some(State::Waiting { reason, .. }) => Health::Waiting {
                reason: reason.clone(),
            },
            Some(State::Terminated { exit_code: 0, .. }) => Health::Completed,
            Some(State::Terminated {
                reason, exit_code, ..
            }) => Health::Failed {
                reason: reason.clone(),
                exit_code: *exit_code,
            },
        }
    }

    /// Whether the observation shows a previous terminated instance whose logs
    /// `kubectl logs --previous` could return.
    pub fn previous_instance_observed(&self) -> bool {
        matches!(self.last_state, Some(State::Terminated { .. }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pod {
    pub meta: Meta,
    pub phase: Option<String>,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub node: Option<String>,
    pub pod_ip: Option<String>,
    pub started: Option<String>,
    pub qos_class: Option<String>,
    pub conditions: Vec<Condition>,
    /// Init and sidecar first (spec order), then regular, then ephemeral.
    pub containers: Vec<Container>,
}

impl Pod {
    pub fn container(&self, name: &str) -> Option<&Container> {
        self.containers.iter().find(|c| c.name == name)
    }
}

// ---------- Workloads, networking, events, autoscaling ----------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace {
    pub meta: Meta,
    pub phase: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    pub meta: Meta,
    pub replicas: Option<i32>,
    pub selector: BTreeMap<String, String>,
    pub observed_generation: Option<i64>,
    pub status_replicas: Option<i32>,
    pub ready: Option<i32>,
    pub available: Option<i32>,
    pub updated: Option<i32>,
    pub unavailable: Option<i32>,
    pub conditions: Vec<Condition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaSet {
    pub meta: Meta,
    pub replicas: Option<i32>,
    pub selector: BTreeMap<String, String>,
    pub observed_generation: Option<i64>,
    pub status_replicas: Option<i32>,
    pub ready: Option<i32>,
    pub available: Option<i32>,
    pub template_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServicePort {
    pub name: Option<String>,
    pub protocol: Option<String>,
    pub port: i32,
    pub target_port: Option<String>,
    pub node_port: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub meta: Meta,
    pub kind: Option<String>,
    pub cluster_ip: Option<String>,
    pub selector: BTreeMap<String, String>,
    pub ports: Vec<ServicePort>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub addresses: Vec<String>,
    pub ready: Option<bool>,
    pub serving: Option<bool>,
    pub terminating: Option<bool>,
    pub node: Option<String>,
    pub target: Option<ObjectRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlicePort {
    pub name: Option<String>,
    pub port: Option<i32>,
    pub protocol: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointSlice {
    pub meta: Meta,
    /// From the `kubernetes.io/service-name` label.
    pub service: Option<String>,
    pub address_type: Option<String>,
    pub endpoints: Vec<Endpoint>,
    pub ports: Vec<SlicePort>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub meta: Meta,
    pub regarding: ObjectRef,
    pub kind: Option<String>,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub count: Option<i32>,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub event_time: Option<String>,
    pub reporter: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricFact {
    pub kind: String,
    pub name: Option<String>,
    pub current: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HorizontalPodAutoscaler {
    pub meta: Meta,
    pub target: ObjectRef,
    pub min_replicas: Option<i32>,
    pub max_replicas: Option<i32>,
    pub current_replicas: Option<i32>,
    pub desired_replicas: Option<i32>,
    pub last_scale: Option<String>,
    pub conditions: Vec<Condition>,
    pub metrics: Vec<MetricFact>,
}

// ---------- Decoding ----------

fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiMeta {
    name: Option<String>,
    namespace: Option<String>,
    uid: Option<String>,
    resource_version: Option<String>,
    generation: Option<i64>,
    creation_timestamp: Option<String>,
    deletion_timestamp: Option<String>,
    #[serde(deserialize_with = "nullable")]
    labels: BTreeMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    owner_references: Vec<ApiOwner>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiOwner {
    kind: String,
    name: String,
    uid: String,
    controller: Option<bool>,
}

fn meta(api: ApiMeta) -> Result<Meta, String> {
    let required = |value: Option<String>, field: &str| {
        value
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("metadata.{field} is missing"))
    };
    Ok(Meta {
        name: required(api.name, "name")?,
        namespace: api.namespace,
        uid: required(api.uid, "uid")?,
        resource_version: ResourceVersion(required(api.resource_version, "resourceVersion")?),
        generation: api.generation,
        created: api.creation_timestamp,
        deleting_since: api.deletion_timestamp,
        labels: api.labels,
        owners: api
            .owner_references
            .into_iter()
            .map(|o| OwnerRef {
                kind: o.kind,
                name: o.name,
                uid: o.uid,
                controller: o.controller.unwrap_or(false),
            })
            .collect(),
    })
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiCondition {
    #[serde(rename = "type")]
    kind: String,
    status: String,
    reason: Option<String>,
    message: Option<String>,
    last_transition_time: Option<String>,
}

fn conditions(list: Vec<ApiCondition>) -> Vec<Condition> {
    list.into_iter()
        .map(|c| Condition {
            kind: c.kind,
            status: c.status,
            reason: c.reason,
            message: c.message,
            last_transition: c.last_transition_time,
        })
        .collect()
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiObjectRef {
    kind: String,
    name: String,
    namespace: Option<String>,
    uid: Option<String>,
}

fn object_ref(r: ApiObjectRef) -> ObjectRef {
    ObjectRef {
        kind: r.kind,
        name: r.name,
        namespace: r.namespace,
        uid: r.uid,
    }
}

/// IntOrString and similar scalars as display text.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiPod {
    metadata: ApiMeta,
    spec: ApiPodSpec,
    status: ApiPodStatus,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiPodSpec {
    #[serde(deserialize_with = "nullable")]
    containers: Vec<ApiContainer>,
    #[serde(deserialize_with = "nullable")]
    init_containers: Vec<ApiContainer>,
    #[serde(deserialize_with = "nullable")]
    ephemeral_containers: Vec<ApiContainer>,
    node_name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiContainer {
    name: String,
    image: Option<String>,
    restart_policy: Option<String>,
    resources: ApiResources,
    liveness_probe: Option<ApiProbe>,
    readiness_probe: Option<ApiProbe>,
    startup_probe: Option<ApiProbe>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiResources {
    #[serde(deserialize_with = "nullable")]
    requests: BTreeMap<String, Value>,
    #[serde(deserialize_with = "nullable")]
    limits: BTreeMap<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiProbe {
    http_get: Option<ApiHttpGet>,
    tcp_socket: Option<ApiPortOnly>,
    grpc: Option<ApiGrpc>,
    exec: Option<ApiExec>,
    initial_delay_seconds: Option<i32>,
    period_seconds: Option<i32>,
    timeout_seconds: Option<i32>,
    success_threshold: Option<i32>,
    failure_threshold: Option<i32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiHttpGet {
    path: Option<String>,
    port: Value,
    scheme: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiPortOnly {
    port: Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiGrpc {
    port: i32,
    service: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiExec {
    #[serde(deserialize_with = "nullable")]
    command: Vec<Value>,
}

fn probe(api: Option<ApiProbe>) -> Option<Probe> {
    let api = api?;
    let handler = if let Some(h) = api.http_get {
        ProbeHandler::Http {
            path: h.path,
            port: scalar(&h.port).unwrap_or_default(),
            scheme: h.scheme,
        }
    } else if let Some(t) = api.tcp_socket {
        ProbeHandler::Tcp {
            port: scalar(&t.port).unwrap_or_default(),
        }
    } else if let Some(g) = api.grpc {
        ProbeHandler::Grpc {
            port: g.port,
            service: g.service,
        }
    } else if let Some(e) = api.exec {
        ProbeHandler::Exec {
            arguments: e.command.len(),
        }
    } else {
        ProbeHandler::Unknown
    };
    Some(Probe {
        handler,
        initial_delay_seconds: api.initial_delay_seconds,
        period_seconds: api.period_seconds,
        timeout_seconds: api.timeout_seconds,
        success_threshold: api.success_threshold,
        failure_threshold: api.failure_threshold,
    })
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiPodStatus {
    phase: Option<String>,
    reason: Option<String>,
    message: Option<String>,
    #[serde(rename = "podIP")]
    pod_ip: Option<String>,
    start_time: Option<String>,
    qos_class: Option<String>,
    #[serde(deserialize_with = "nullable")]
    conditions: Vec<ApiCondition>,
    #[serde(deserialize_with = "nullable")]
    container_statuses: Vec<ApiContainerStatus>,
    #[serde(deserialize_with = "nullable")]
    init_container_statuses: Vec<ApiContainerStatus>,
    #[serde(deserialize_with = "nullable")]
    ephemeral_container_statuses: Vec<ApiContainerStatus>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiContainerStatus {
    name: String,
    ready: Option<bool>,
    started: Option<bool>,
    restart_count: Option<u32>,
    image: Option<String>,
    #[serde(rename = "imageID")]
    image_id: Option<String>,
    state: Option<ApiState>,
    last_state: Option<ApiState>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiState {
    waiting: Option<ApiWaiting>,
    running: Option<ApiRunning>,
    terminated: Option<ApiTerminated>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiWaiting {
    reason: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiRunning {
    started_at: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiTerminated {
    exit_code: i32,
    signal: Option<i32>,
    reason: Option<String>,
    message: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

/// An empty `{}` state (as in `lastState` of a never-restarted container) is no state.
fn state(api: Option<ApiState>) -> Option<State> {
    let api = api?;
    if let Some(t) = api.terminated {
        Some(State::Terminated {
            reason: t.reason,
            message: t.message,
            exit_code: t.exit_code,
            signal: t.signal,
            started_at: t.started_at,
            finished_at: t.finished_at,
        })
    } else if let Some(r) = api.running {
        Some(State::Running {
            started_at: r.started_at,
        })
    } else {
        api.waiting.map(|w| State::Waiting {
            reason: w.reason,
            message: w.message,
        })
    }
}

fn quantities(map: BTreeMap<String, Value>) -> BTreeMap<String, String> {
    map.into_iter()
        .filter_map(|(k, v)| scalar(&v).map(|v| (k, v)))
        .collect()
}

/// Statuses are matched by name within the same container list only, so an
/// init container and a regular container never share a status.
fn containers(
    specs: Vec<ApiContainer>,
    mut statuses: Vec<ApiContainerStatus>,
    kind_of: impl Fn(&ApiContainer) -> ContainerKind,
    orphan: ContainerKind,
    out: &mut Vec<Container>,
) {
    for spec in specs {
        let status = statuses
            .iter()
            .position(|s| s.name == spec.name)
            .map(|at| statuses.remove(at));
        let kind = kind_of(&spec);
        out.push(container(kind, spec, status));
    }
    // A status without a spec entry is still a fact; keep it visible.
    for status in statuses {
        let spec = ApiContainer {
            name: status.name.clone(),
            ..Default::default()
        };
        out.push(container(orphan, spec, Some(status)));
    }
}

fn container(
    kind: ContainerKind,
    spec: ApiContainer,
    status: Option<ApiContainerStatus>,
) -> Container {
    let reported = status.is_some();
    let status = status.unwrap_or_default();
    Container {
        kind,
        name: spec.name,
        image: spec.image,
        status_image: status.image,
        image_id: status.image_id.filter(|id| !id.is_empty()),
        status_reported: reported,
        ready: status.ready,
        started: status.started,
        restart_count: status.restart_count,
        state: state(status.state),
        last_state: state(status.last_state),
        resources: Resources {
            requests: quantities(spec.resources.requests),
            limits: quantities(spec.resources.limits),
        },
        probes: Probes {
            startup: probe(spec.startup_probe),
            liveness: probe(spec.liveness_probe),
            readiness: probe(spec.readiness_probe),
        },
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiSelector {
    #[serde(deserialize_with = "nullable")]
    match_labels: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiWorkloadSpec {
    replicas: Option<i32>,
    selector: ApiSelector,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiWorkloadStatus {
    observed_generation: Option<i64>,
    replicas: Option<i32>,
    ready_replicas: Option<i32>,
    available_replicas: Option<i32>,
    updated_replicas: Option<i32>,
    unavailable_replicas: Option<i32>,
    #[serde(deserialize_with = "nullable")]
    conditions: Vec<ApiCondition>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiWorkload {
    metadata: ApiMeta,
    spec: ApiWorkloadSpec,
    status: ApiWorkloadStatus,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiServiceSpec {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "clusterIP")]
    cluster_ip: Option<String>,
    #[serde(deserialize_with = "nullable")]
    selector: BTreeMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    ports: Vec<ApiServicePort>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiServicePort {
    name: Option<String>,
    protocol: Option<String>,
    port: i32,
    target_port: Value,
    node_port: Option<i32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiService {
    metadata: ApiMeta,
    spec: ApiServiceSpec,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiEndpointSlice {
    metadata: ApiMeta,
    address_type: Option<String>,
    #[serde(deserialize_with = "nullable")]
    endpoints: Vec<ApiEndpoint>,
    #[serde(deserialize_with = "nullable")]
    ports: Vec<ApiSlicePort>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiEndpoint {
    #[serde(deserialize_with = "nullable")]
    addresses: Vec<String>,
    conditions: ApiEndpointConditions,
    node_name: Option<String>,
    target_ref: Option<ApiObjectRef>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiEndpointConditions {
    ready: Option<bool>,
    serving: Option<bool>,
    terminating: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiSlicePort {
    name: Option<String>,
    port: Option<i32>,
    protocol: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiEvent {
    metadata: ApiMeta,
    involved_object: ApiObjectRef,
    #[serde(rename = "type")]
    kind: Option<String>,
    reason: Option<String>,
    message: Option<String>,
    count: Option<i32>,
    first_timestamp: Option<String>,
    last_timestamp: Option<String>,
    event_time: Option<String>,
    source: ApiEventSource,
    reporting_component: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiEventSource {
    component: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiHpa {
    metadata: ApiMeta,
    spec: ApiHpaSpec,
    status: ApiHpaStatus,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiHpaSpec {
    scale_target_ref: ApiObjectRef,
    min_replicas: Option<i32>,
    max_replicas: Option<i32>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiHpaStatus {
    current_replicas: Option<i32>,
    desired_replicas: Option<i32>,
    last_scale_time: Option<String>,
    #[serde(deserialize_with = "nullable")]
    conditions: Vec<ApiCondition>,
    #[serde(deserialize_with = "nullable")]
    current_metrics: Vec<Value>,
}

fn metric(value: &Value) -> MetricFact {
    let kind = value["type"].as_str().unwrap_or_default().to_string();
    // "ContainerResource" -> "containerResource", "Pods" -> "pods", ...
    let mut chars = kind.chars();
    let key: String = chars
        .next()
        .map(|first| first.to_ascii_lowercase().to_string() + chars.as_str())
        .unwrap_or_default();
    let inner = &value[key.as_str()];
    let name = inner["name"]
        .as_str()
        .or_else(|| inner["metric"]["name"].as_str())
        .map(str::to_string);
    let current = &inner["current"];
    let current = if let Some(u) = current["averageUtilization"].as_i64() {
        Some(format!("{u}%"))
    } else {
        scalar(&current["averageValue"]).or_else(|| scalar(&current["value"]))
    };
    MetricFact {
        kind,
        name,
        current,
    }
}

fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| e.to_string())
}

pub(crate) fn namespace(value: Value) -> Result<Namespace, String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Api {
        metadata: ApiMeta,
        status: ApiPhase,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct ApiPhase {
        phase: Option<String>,
    }
    let api: Api = decode(value)?;
    Ok(Namespace {
        meta: meta(api.metadata)?,
        phase: api.status.phase,
    })
}

pub(crate) fn pod(value: Value) -> Result<Pod, String> {
    let api: ApiPod = decode(value)?;
    let mut out = Vec::new();
    let status = api.status;
    containers(
        api.spec.init_containers,
        status.init_container_statuses,
        |c| {
            if c.restart_policy.as_deref() == Some("Always") {
                ContainerKind::Sidecar
            } else {
                ContainerKind::Init
            }
        },
        ContainerKind::Init,
        &mut out,
    );
    containers(
        api.spec.containers,
        status.container_statuses,
        |_| ContainerKind::Regular,
        ContainerKind::Regular,
        &mut out,
    );
    containers(
        api.spec.ephemeral_containers,
        status.ephemeral_container_statuses,
        |_| ContainerKind::Ephemeral,
        ContainerKind::Ephemeral,
        &mut out,
    );
    Ok(Pod {
        meta: meta(api.metadata)?,
        phase: status.phase,
        reason: status.reason,
        message: status.message,
        node: api.spec.node_name,
        pod_ip: status.pod_ip,
        started: status.start_time,
        qos_class: status.qos_class,
        conditions: conditions(status.conditions),
        containers: out,
    })
}

pub(crate) fn deployment(value: Value) -> Result<Deployment, String> {
    let api: ApiWorkload = decode(value)?;
    Ok(Deployment {
        meta: meta(api.metadata)?,
        replicas: api.spec.replicas,
        selector: api.spec.selector.match_labels,
        observed_generation: api.status.observed_generation,
        status_replicas: api.status.replicas,
        ready: api.status.ready_replicas,
        available: api.status.available_replicas,
        updated: api.status.updated_replicas,
        unavailable: api.status.unavailable_replicas,
        conditions: conditions(api.status.conditions),
    })
}

pub(crate) fn replica_set(value: Value) -> Result<ReplicaSet, String> {
    let api: ApiWorkload = decode(value)?;
    let meta = meta(api.metadata)?;
    Ok(ReplicaSet {
        template_hash: meta.labels.get("pod-template-hash").cloned(),
        meta,
        replicas: api.spec.replicas,
        selector: api.spec.selector.match_labels,
        observed_generation: api.status.observed_generation,
        status_replicas: api.status.replicas,
        ready: api.status.ready_replicas,
        available: api.status.available_replicas,
    })
}

pub(crate) fn service(value: Value) -> Result<Service, String> {
    let api: ApiService = decode(value)?;
    Ok(Service {
        meta: meta(api.metadata)?,
        kind: api.spec.kind,
        cluster_ip: api.spec.cluster_ip,
        selector: api.spec.selector,
        ports: api
            .spec
            .ports
            .into_iter()
            .map(|p| ServicePort {
                name: p.name,
                protocol: p.protocol,
                port: p.port,
                target_port: scalar(&p.target_port),
                node_port: p.node_port,
            })
            .collect(),
    })
}

pub(crate) fn endpoint_slice(value: Value) -> Result<EndpointSlice, String> {
    let api: ApiEndpointSlice = decode(value)?;
    let meta = meta(api.metadata)?;
    Ok(EndpointSlice {
        service: meta.labels.get("kubernetes.io/service-name").cloned(),
        meta,
        address_type: api.address_type,
        endpoints: api
            .endpoints
            .into_iter()
            .map(|e| Endpoint {
                addresses: e.addresses,
                ready: e.conditions.ready,
                serving: e.conditions.serving,
                terminating: e.conditions.terminating,
                node: e.node_name,
                target: e.target_ref.map(object_ref),
            })
            .collect(),
        ports: api
            .ports
            .into_iter()
            .map(|p| SlicePort {
                name: p.name,
                port: p.port,
                protocol: p.protocol,
            })
            .collect(),
    })
}

pub(crate) fn event(value: Value) -> Result<Event, String> {
    let api: ApiEvent = decode(value)?;
    Ok(Event {
        meta: meta(api.metadata)?,
        regarding: object_ref(api.involved_object),
        kind: api.kind,
        reason: api.reason,
        message: api.message,
        count: api.count,
        first_seen: api.first_timestamp,
        last_seen: api.last_timestamp,
        event_time: api.event_time,
        reporter: api
            .reporting_component
            .filter(|c| !c.is_empty())
            .or(api.source.component),
    })
}

pub(crate) fn hpa(value: Value) -> Result<HorizontalPodAutoscaler, String> {
    let api: ApiHpa = decode(value)?;
    Ok(HorizontalPodAutoscaler {
        meta: meta(api.metadata)?,
        target: object_ref(api.spec.scale_target_ref),
        min_replicas: api.spec.min_replicas,
        max_replicas: api.spec.max_replicas,
        current_replicas: api.status.current_replicas,
        desired_replicas: api.status.desired_replicas,
        last_scale: api.status.last_scale_time,
        conditions: conditions(api.status.conditions),
        metrics: api.status.current_metrics.iter().map(metric).collect(),
    })
}
