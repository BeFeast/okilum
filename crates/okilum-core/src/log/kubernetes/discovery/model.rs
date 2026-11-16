use super::*;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Namespace,
    Pod,
    Deployment,
    ReplicaSet,
    Service,
    EndpointSlice,
    Event,
    Hpa,
}
impl Kind {
    pub(super) fn resource(self) -> &'static str {
        match self {
            Self::Namespace => "namespaces",
            Self::Pod => "pods",
            Self::Deployment => "deployments",
            Self::ReplicaSet => "replicasets",
            Self::Service => "services",
            Self::EndpointSlice => "endpointslices",
            Self::Event => "events",
            Self::Hpa => "horizontalpodautoscalers",
        }
    }
    pub(super) fn api_prefix(self) -> &'static str {
        match self {
            Self::Namespace | Self::Pod | Self::Service | Self::Event => "/api/v1",
            Self::Deployment | Self::ReplicaSet => "/apis/apps/v1",
            Self::EndpointSlice => "/apis/discovery.k8s.io/v1",
            Self::Hpa => "/apis/autoscaling/v2",
        }
    }
    fn wire(self) -> (&'static str, &'static str) {
        match self {
            Self::Namespace => ("v1", "Namespace"),
            Self::Pod => ("v1", "Pod"),
            Self::Deployment => ("apps/v1", "Deployment"),
            Self::ReplicaSet => ("apps/v1", "ReplicaSet"),
            Self::Service => ("v1", "Service"),
            Self::EndpointSlice => ("discovery.k8s.io/v1", "EndpointSlice"),
            Self::Event => ("v1", "Event"),
            Self::Hpa => ("autoscaling/v2", "HorizontalPodAutoscaler"),
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub name: String,
    #[serde(default)]
    pub namespace: Option<String>,
    pub uid: String,
    /// An opaque server token: never parsed, ordered or used as a wall clock.
    pub resource_version: String,
    pub generation: Option<i64>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub owner_references: Vec<OwnerReference>,
    pub creation_timestamp: Option<String>,
    pub deletion_timestamp: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerReference {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub uid: String,
    pub controller: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub connection: Connection,
    pub namespace: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub uid: String,
}
#[derive(Clone, Debug)]
pub struct Resource {
    pub metadata: Metadata,
    pub facts: Facts,
}
impl Resource {
    pub fn identity(&self, connection: &Connection) -> Identity {
        Identity {
            connection: connection.clone(),
            namespace: self.metadata.namespace.clone(),
            kind: self.facts.kind(),
            name: self.metadata.name.clone(),
            uid: self.metadata.uid.clone(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum Facts {
    Namespace,
    Pod(Pod),
    Deployment(Workload),
    ReplicaSet(Workload),
    Service(Service),
    EndpointSlice(EndpointSlice),
    Event(KubeEvent),
    Hpa(Hpa),
}
impl Facts {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Namespace => Kind::Namespace,
            Self::Pod(_) => Kind::Pod,
            Self::Deployment(_) => Kind::Deployment,
            Self::ReplicaSet(_) => Kind::ReplicaSet,
            Self::Service(_) => Kind::Service,
            Self::EndpointSlice(_) => Kind::EndpointSlice,
            Self::Event(_) => Kind::Event,
            Self::Hpa(_) => Kind::Hpa,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerRole {
    Regular,
    Init,
    Ephemeral,
}
#[derive(Clone, Debug)]
pub struct Container {
    pub role: ContainerRole,
    pub spec: ContainerSpec,
    /// Missing status is unknown. Pod phase/reason never fills a container status.
    pub status: Option<ContainerStatus>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSpec {
    pub name: String,
    pub image: Option<String>,
    #[serde(default)]
    pub resources: Resources,
    pub readiness_probe: Option<Probe>,
    pub liveness_probe: Option<Probe>,
    pub startup_probe: Option<Probe>,
    pub restart_policy: Option<String>,
    pub target_container_name: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Resources {
    #[serde(default)]
    pub requests: BTreeMap<String, String>,
    #[serde(default)]
    pub limits: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub initial_delay_seconds: Option<i64>,
    pub period_seconds: Option<i64>,
    pub timeout_seconds: Option<i64>,
    pub success_threshold: Option<i64>,
    pub failure_threshold: Option<i64>,
    pub termination_grace_period_seconds: Option<i64>,
    // Probe handlers retain their structured API fields, including named ports/headers.
    pub exec: Option<ExecProbe>,
    pub http_get: Option<HttpProbe>,
    pub tcp_socket: Option<SocketProbe>,
    pub grpc: Option<GrpcProbe>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct ExecProbe {
    pub command: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Port {
    Number(i32),
    Name(String),
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpProbe {
    pub host: Option<String>,
    pub path: Option<String>,
    pub port: Port,
    pub scheme: Option<String>,
    #[serde(default)]
    pub http_headers: Vec<HttpHeader>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct SocketProbe {
    pub host: Option<String>,
    pub port: Port,
}
#[derive(Clone, Debug, Deserialize)]
pub struct GrpcProbe {
    pub port: i32,
    pub service: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatus {
    pub name: String,
    pub ready: Option<bool>,
    pub started: Option<bool>,
    pub restart_count: Option<u64>,
    pub image: Option<String>,
    pub image_id: Option<String>,
    pub container_id: Option<String>,
    pub state: Option<ContainerState>,
    pub last_state: Option<ContainerState>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct ContainerState {
    pub waiting: Option<Waiting>,
    pub running: Option<Running>,
    pub terminated: Option<Terminated>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Waiting {
    pub reason: Option<String>,
    pub message: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Running {
    pub started_at: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminated {
    pub exit_code: i32,
    pub signal: Option<i32>,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub container_id: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Pod {
    pub phase: Option<String>,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub node_name: Option<String>,
    pub conditions: Vec<Condition>,
    pub containers: Vec<Container>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub last_transition_time: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selector {
    #[serde(default)]
    pub match_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub match_expressions: Vec<SelectorExpression>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct SelectorExpression {
    pub key: String,
    pub operator: String,
    #[serde(default)]
    pub values: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Workload {
    pub spec: WorkloadSpec,
    pub status: Option<WorkloadStatus>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct WorkloadSpec {
    pub replicas: Option<i32>,
    pub selector: Selector,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadStatus {
    pub observed_generation: Option<i64>,
    pub replicas: Option<i32>,
    pub ready_replicas: Option<i32>,
    pub available_replicas: Option<i32>,
    pub updated_replicas: Option<i32>,
    pub unavailable_replicas: Option<i32>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Service {
    pub spec: ServiceSpec,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub selector: Option<BTreeMap<String, String>>,
    pub cluster_ip: Option<String>,
    #[serde(default)]
    pub cluster_ips: Vec<String>,
    pub external_name: Option<String>,
    #[serde(default)]
    pub ports: Vec<ServicePort>,
    pub publish_not_ready_addresses: Option<bool>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServicePort {
    pub name: Option<String>,
    pub port: i32,
    pub target_port: Option<Port>,
    pub protocol: Option<String>,
    pub node_port: Option<i32>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointSlice {
    pub address_type: String,
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub ports: Vec<EndpointPort>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub addresses: Vec<String>,
    #[serde(default)]
    pub conditions: EndpointConditions,
    pub target_ref: Option<ObjectReference>,
    pub node_name: Option<String>,
    pub zone: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EndpointConditions {
    pub ready: Option<bool>,
    pub serving: Option<bool>,
    pub terminating: Option<bool>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointPort {
    pub name: Option<String>,
    pub port: Option<i32>,
    pub protocol: Option<String>,
    pub app_protocol: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectReference {
    pub api_version: Option<String>,
    pub kind: Option<String>,
    pub namespace: Option<String>,
    pub name: Option<String>,
    pub uid: Option<String>,
    pub resource_version: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KubeEvent {
    pub involved_object: ObjectReference,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub count: Option<i64>,
    pub first_timestamp: Option<String>,
    pub last_timestamp: Option<String>,
    pub event_time: Option<String>,
    pub series: Option<EventSeries>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSeries {
    pub count: i64,
    pub last_observed_time: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Hpa {
    pub spec: HpaSpec,
    pub status: Option<HpaStatus>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HpaSpec {
    pub scale_target_ref: ScaleTarget,
    pub min_replicas: Option<i32>,
    pub max_replicas: i32,
    /// Versioned metric payloads are evidence, not computed utilization or healthy zero.
    #[serde(default)]
    pub metrics: Vec<Metric>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleTarget {
    pub api_version: Option<String>,
    pub kind: String,
    pub name: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HpaStatus {
    pub observed_generation: Option<i64>,
    pub current_replicas: Option<i32>,
    pub desired_replicas: Option<i32>,
    pub last_scale_time: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub current_metrics: Vec<Metric>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Metric {
    #[serde(rename = "type")]
    pub kind: String,
    // The discriminant is typed; Kubernetes' evolving per-type structures are retained losslessly.
    #[serde(flatten)]
    pub fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePod {
    spec: PodSpec,
    #[serde(default)]
    status: PodStatus,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PodSpec {
    containers: Vec<ContainerSpec>,
    #[serde(default)]
    init_containers: Vec<ContainerSpec>,
    #[serde(default)]
    ephemeral_containers: Vec<ContainerSpec>,
    node_name: Option<String>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PodStatus {
    phase: Option<String>,
    reason: Option<String>,
    message: Option<String>,
    #[serde(default)]
    conditions: Vec<Condition>,
    #[serde(default)]
    container_statuses: Vec<ContainerStatus>,
    #[serde(default)]
    init_container_statuses: Vec<ContainerStatus>,
    #[serde(default)]
    ephemeral_container_statuses: Vec<ContainerStatus>,
}
fn pod(value: serde_json::Value) -> Result<Pod, serde_json::Error> {
    let wire: WirePod = serde_json::from_value(value)?;
    let mut containers = Vec::new();
    for (role, specs, statuses) in [
        (
            ContainerRole::Regular,
            wire.spec.containers,
            wire.status.container_statuses,
        ),
        (
            ContainerRole::Init,
            wire.spec.init_containers,
            wire.status.init_container_statuses,
        ),
        (
            ContainerRole::Ephemeral,
            wire.spec.ephemeral_containers,
            wire.status.ephemeral_container_statuses,
        ),
    ] {
        let spec_names: std::collections::BTreeSet<_> =
            specs.iter().map(|s| s.name.as_str()).collect();
        let status_names: std::collections::BTreeSet<_> =
            statuses.iter().map(|s| s.name.as_str()).collect();
        if spec_names.len() != specs.len()
            || status_names.len() != statuses.len()
            || !status_names.is_subset(&spec_names)
        {
            return Err(serde_json::Error::io(invalid(
                "Ambiguous container identity",
            )));
        }
        for spec in specs {
            let status = statuses.iter().find(|s| s.name == spec.name).cloned();
            containers.push(Container { role, spec, status });
        }
    }
    Ok(Pod {
        phase: wire.status.phase,
        reason: wire.status.reason,
        message: wire.status.message,
        node_name: wire.spec.node_name,
        conditions: wire.status.conditions,
        containers,
    })
}

pub(super) fn parse(
    source: &Query,
    started_at: SystemTime,
    bytes: &[u8],
) -> Result<Observation, Failure> {
    let bad = || {
        Failure::new(
            FailureKind::InvalidJson,
            "Invalid or mismatched Kubernetes JSON (source bytes not retained)",
        )
    };
    if bytes.len() > MAX_JSON_BYTES {
        return Err(Failure::new(
            FailureKind::Limit,
            "Discovery JSON byte limit exceeded",
        ));
    }
    let mut value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| bad())?;
    let data = if let Query::Resolve {
        context,
        kubeconfig,
    } = source
    {
        let contexts = value["contexts"].as_array().ok_or_else(bad)?;
        let selected: Vec<_> = contexts
            .iter()
            .filter(|v| v["name"].as_str() == Some(context))
            .collect();
        if selected.len() != 1 {
            return Err(bad());
        }
        let selected = &selected[0]["context"];
        let cluster = selected["cluster"].as_str().ok_or_else(bad)?;
        let clusters = value["clusters"].as_array().ok_or_else(bad)?;
        let matches: Vec<_> = clusters
            .iter()
            .filter(|v| v["name"].as_str() == Some(cluster))
            .collect();
        if matches.len() != 1 {
            return Err(bad());
        }
        let server = matches[0]["cluster"]["server"].as_str().ok_or_else(bad)?;
        let default_namespace = selected["namespace"]
            .as_str()
            .unwrap_or("default")
            .to_string();
        namespace_name(&default_namespace).map_err(|_| bad())?;
        Data::Connection(Connection {
            context: context.clone(),
            kubeconfig: kubeconfig.clone(),
            cluster: cluster.into(),
            server: server.into(),
            user: selected["user"].as_str().map(str::to_string),
            default_namespace,
        })
    } else {
        let kind = source.kind().expect("resource query");
        let (api, wire_kind) = kind.wire();
        let get = matches!(source, Query::Get { .. });
        let mut partial = Vec::new();
        let resource_version = value["metadata"]["resourceVersion"]
            .as_str()
            .map(str::to_string);
        let values = if get {
            vec![value]
        } else {
            // kubectl may wrap chunked results in a generic v1/List.
            let list_kind = format!("{wire_kind}List");
            if !((value["apiVersion"] == api && value["kind"] == list_kind)
                || (value["apiVersion"] == "v1" && value["kind"] == "List"))
            {
                return Err(bad());
            }
            if value["metadata"]["continue"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            {
                partial.push(PartialReason::ContinueToken);
            }
            let values = value["items"].as_array_mut().ok_or_else(bad)?;
            let mut values = std::mem::take(values);
            if values.len() > MAX_ITEMS {
                partial.push(PartialReason::ItemLimit);
                values.truncate(MAX_ITEMS);
            }
            values
        };
        let mut items = Vec::new();
        for value in values {
            if value["apiVersion"] != api || value["kind"] != wire_kind {
                return Err(bad());
            }
            let metadata: Metadata =
                serde_json::from_value(value["metadata"].clone()).map_err(|_| bad())?;
            if metadata.uid.is_empty()
                || metadata.resource_version.is_empty()
                || dns(&metadata.name).is_err()
                || (kind != Kind::Namespace && metadata.namespace.as_deref() != source.namespace())
            {
                return Err(bad());
            }
            if let Query::Get { name, .. } = source {
                if metadata.name != *name {
                    return Err(bad());
                }
            }
            let facts = match kind {
                Kind::Namespace => Facts::Namespace,
                Kind::Pod => Facts::Pod(pod(value).map_err(|_| bad())?),
                Kind::Deployment => {
                    Facts::Deployment(serde_json::from_value(value).map_err(|_| bad())?)
                }
                Kind::ReplicaSet => {
                    Facts::ReplicaSet(serde_json::from_value(value).map_err(|_| bad())?)
                }
                Kind::Service => Facts::Service(serde_json::from_value(value).map_err(|_| bad())?),
                Kind::EndpointSlice => {
                    Facts::EndpointSlice(serde_json::from_value(value).map_err(|_| bad())?)
                }
                Kind::Event => {
                    let event: KubeEvent = serde_json::from_value(value).map_err(|_| bad())?;
                    if let Query::RelatedEvents { uid, .. } = source {
                        if event.involved_object.uid.as_ref() != Some(uid) {
                            return Err(bad());
                        }
                    }
                    Facts::Event(event)
                }
                Kind::Hpa => Facts::Hpa(serde_json::from_value(value).map_err(|_| bad())?),
            };
            items.push(Resource { metadata, facts });
        }
        Data::Resources {
            items,
            resource_version,
            partial,
        }
    };
    Ok(Observation {
        source: source.clone(),
        started_at,
        observed_at: SystemTime::now(),
        data,
        diagnostic: String::new(),
        diagnostic_truncated: false,
    })
}
