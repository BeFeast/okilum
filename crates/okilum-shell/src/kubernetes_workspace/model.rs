//! The Workspace model (#3010) over the typed read adapter (#3009): rows,
//! relations, grouped events and pinned log targets. Pure data: no GPUI, no
//! processes. docs/kubernetes-workspace.md describes the rules kept here.
use okilum_core::kubernetes::{
    Connection, Container, ContainerKind, Deployment, EndpointSlice, Event, Failure, Health,
    HorizontalPodAutoscaler, Kind, List, Meta, Observation, Pod, ReplicaSet, Service, Slot, Source,
    Staleness, State, View,
};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// A source older than this is shown as aged (still shown, never hidden).
pub(crate) const FRESH_FOR: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    Pods,
    Containers,
    Deployments,
    Services,
}

impl Section {
    pub(crate) const ALL: [Section; 4] = [
        Section::Pods,
        Section::Containers,
        Section::Deployments,
        Section::Services,
    ];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pods => "Pods",
            Self::Containers => "Containers",
            Self::Deployments => "Deployments",
            Self::Services => "Services",
        }
    }
}

/// The explicit context and namespace every read of one Load uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    pub connection: Connection,
    pub namespace: String,
}

impl Scope {
    pub(crate) fn source(&self, kind: Kind) -> Source {
        Source {
            context: self.connection.context.clone(),
            cluster: self.connection.cluster.clone(),
            server: self.connection.server.clone(),
            namespace: Some(self.namespace.clone()),
            kind,
        }
    }
    pub(crate) fn label(&self) -> String {
        format!("{} / {}", self.connection.context, self.namespace)
    }
}

/// Retained observations of one scope, one slot per source. A failed read
/// keeps the older value as stale; it never empties the list.
pub(crate) struct Data {
    pub scope: Scope,
    pub pods: Slot<List<Pod>>,
    pub deployments: Slot<List<Deployment>>,
    pub replica_sets: Slot<List<ReplicaSet>>,
    pub services: Slot<List<Service>>,
    pub slices: Slot<List<EndpointSlice>>,
    pub events: Slot<List<Event>>,
    pub hpas: Slot<List<HorizontalPodAutoscaler>>,
}

pub(crate) fn latest<T>(slot: &Slot<T>) -> Option<&Observation<T>> {
    match slot.view(SystemTime::now(), Duration::MAX) {
        View::Fresh(last) | View::Stale { last, .. } => Some(last),
        View::Unread | View::Failed { .. } => None,
    }
}

fn items<T>(slot: &Slot<List<T>>) -> &[T] {
    latest(slot).map_or(&[], |o| o.value.items.as_slice())
}

impl Data {
    pub(crate) fn new(scope: Scope) -> Self {
        Self {
            pods: Slot::new(scope.source(Kind::Pod)),
            deployments: Slot::new(scope.source(Kind::Deployment)),
            replica_sets: Slot::new(scope.source(Kind::ReplicaSet)),
            services: Slot::new(scope.source(Kind::Service)),
            slices: Slot::new(scope.source(Kind::EndpointSlice)),
            events: Slot::new(scope.source(Kind::Event)),
            hpas: Slot::new(scope.source(Kind::HorizontalPodAutoscaler)),
            scope,
        }
    }
    pub(crate) fn pods(&self) -> &[Pod] {
        items(&self.pods)
    }
    pub(crate) fn pod_observation(&self) -> Option<&Observation<List<Pod>>> {
        latest(&self.pods)
    }
}

// ---------- Freshness per source ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Good,
    Warning,
    Danger,
    Neutral,
}

pub(crate) fn clock(at: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(at)
        .format("%H:%M:%S")
        .to_string()
}

/// One source's state for the status line: when it was read, whether it is
/// complete, and why it is stale or failed. Text, not only color.
pub(crate) fn freshness<T>(slot: &Slot<List<T>>, now: SystemTime) -> (String, Tone) {
    let completeness = |list: &List<T>| {
        let mut notes = Vec::new();
        if let okilum_core::kubernetes::Completeness::Truncated { remaining } = list.completeness {
            notes.push(match remaining {
                Some(n) => format!("first page, {n} more"),
                None => "first page only".into(),
            });
        }
        if list.rejected_total > 0 {
            notes.push(format!("{} undecodable", list.rejected_total));
        }
        if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        }
    };
    match slot.view(now, FRESH_FOR) {
        View::Unread => ("not read".into(), Tone::Neutral),
        View::Fresh(o) => {
            let tone = if o.value.is_complete() {
                Tone::Good
            } else {
                Tone::Warning
            };
            (
                format!("{}{}", clock(o.observed_at), completeness(&o.value)),
                tone,
            )
        }
        View::Stale {
            last,
            why: Staleness::Aged { .. },
        } => (
            format!(
                "{} (aged){}",
                clock(last.observed_at),
                completeness(&last.value)
            ),
            Tone::Warning,
        ),
        View::Stale {
            last,
            why: Staleness::Failed { failure, .. },
        } => (
            format!(
                "stale since {}: {}",
                clock(last.observed_at),
                failure_kind(failure)
            ),
            Tone::Warning,
        ),
        View::Failed { failure, .. } => (failure_kind(failure).into(), Tone::Danger),
    }
}

/// Short failure names. Forbidden is not "empty", auth is not "down".
pub(crate) fn failure_kind(failure: &Failure) -> &'static str {
    match failure {
        Failure::Forbidden { .. } => "forbidden",
        Failure::AuthExpired { .. } => "sign-in expired",
        Failure::InteractiveLogin { .. } => "interactive sign-in required",
        Failure::NotFound { .. } => "not served by this cluster",
        Failure::Unavailable { .. } => "unavailable",
        Failure::Configuration { .. } => "kubeconfig problem",
        Failure::InvalidRequest { .. } => "invalid request",
        Failure::Malformed { .. } => "unexpected response",
        Failure::Oversized { .. } => "response too large",
        Failure::TimedOut { .. } => "timed out",
        Failure::Cancelled => "cancelled",
        Failure::Failed { .. } => "failed",
    }
}

/// Why a list has no rows: a successful empty read, a failure, or no read yet.
pub(crate) fn empty_reason<T>(slot: &Slot<List<T>>, what: &str, scope: &Scope) -> String {
    match slot.view(SystemTime::now(), Duration::MAX) {
        View::Unread => format!("{what} not read yet."),
        View::Failed { failure, .. } => {
            format!("{what} could not be read: {}", failure.summary())
        }
        View::Fresh(o) | View::Stale { last: o, .. } if o.value.is_empty() => format!(
            "No {} in {} (a successful read that found nothing).",
            what.to_lowercase(),
            scope.label()
        ),
        View::Fresh(_) | View::Stale { .. } => "Nothing matches the filter.".into(),
    }
}

// ---------- Containers and Pods ----------

/// kubectl's READY counts regular containers and native sidecars.
fn counted(c: &Container) -> bool {
    matches!(c.kind, ContainerKind::Regular | ContainerKind::Sidecar)
}

pub(crate) fn kind_label(kind: ContainerKind) -> &'static str {
    match kind {
        ContainerKind::Init => "init",
        ContainerKind::Sidecar => "sidecar",
        ContainerKind::Regular => "container",
        ContainerKind::Ephemeral => "ephemeral",
    }
}

/// Ready containers of the counted kinds, out of how many. Unreported
/// statuses count as not ready, as kubectl does.
pub(crate) fn pod_ready(pod: &Pod) -> (usize, usize) {
    let counted: Vec<_> = pod.containers.iter().filter(|c| counted(c)).collect();
    let ready = counted.iter().filter(|c| c.ready == Some(true)).count();
    (ready, counted.len())
}

/// Sum over the counted containers that report a restart count; `None` when
/// none reports one, which is unknown rather than zero.
pub(crate) fn pod_restarts(pod: &Pod) -> Option<u32> {
    pod.containers
        .iter()
        .filter(|c| counted(c))
        .filter_map(|c| c.restart_count)
        .reduce(|a, b| a.saturating_add(b))
}

fn terminated(reason: &Option<String>, exit_code: i32, signal: Option<i32>) -> String {
    let reason = reason.as_deref().unwrap_or("Terminated");
    match signal {
        Some(signal) => format!("{reason} (exit {exit_code}, signal {signal})"),
        None => format!("{reason} (exit {exit_code})"),
    }
}

pub(crate) fn state_text(state: &State) -> String {
    match state {
        State::Waiting { reason, .. } => reason.clone().unwrap_or_else(|| "Waiting".into()),
        State::Running { .. } => "Running".into(),
        State::Terminated {
            reason,
            exit_code,
            signal,
            ..
        } => terminated(reason, *exit_code, *signal),
    }
}

/// This container's own named reasons: its current waiting/terminated reason
/// and its last termination. Nothing comes from the Pod or a sibling.
pub(crate) fn container_reasons(c: &Container) -> Vec<String> {
    let mut reasons = Vec::new();
    match &c.state {
        Some(State::Waiting {
            reason: Some(reason),
            ..
        }) => reasons.push(reason.clone()),
        Some(State::Terminated {
            reason,
            exit_code,
            signal,
            ..
        }) if *exit_code != 0 => reasons.push(terminated(reason, *exit_code, *signal)),
        _ => {}
    }
    if let Some(State::Terminated {
        reason,
        exit_code,
        signal,
        ..
    }) = &c.last_state
    {
        reasons.push(format!("last: {}", terminated(reason, *exit_code, *signal)));
    }
    reasons
}

const BENIGN_WAITING: [&str; 2] = ["ContainerCreating", "PodInitializing"];

pub(crate) fn container_tone(c: &Container) -> Tone {
    match c.health() {
        Health::Failed { .. } => Tone::Danger,
        Health::Waiting { reason } => match reason.as_deref() {
            Some(r) if BENIGN_WAITING.contains(&r) => Tone::Warning,
            None => Tone::Warning,
            Some(_) => Tone::Danger,
        },
        Health::Running { ready } => {
            let unready = ready == Some(false) && c.kind != ContainerKind::Ephemeral;
            let recovered = matches!(
                c.last_state,
                Some(State::Terminated { exit_code, .. }) if exit_code != 0
            );
            if unready || recovered {
                Tone::Warning
            } else {
                Tone::Good
            }
        }
        Health::Completed => match c.kind {
            ContainerKind::Init => Tone::Good,
            _ => Tone::Neutral,
        },
        Health::Unreported => Tone::Neutral,
    }
}

pub(crate) fn container_status(c: &Container) -> String {
    match c.health() {
        Health::Unreported => "Not reported".into(),
        Health::Running { ready: Some(false) } if c.kind != ContainerKind::Ephemeral => {
            "Running, not ready".into()
        }
        Health::Running { .. } => "Running".into(),
        Health::Waiting { reason } => reason.unwrap_or_else(|| "Waiting".into()),
        Health::Completed => "Completed".into(),
        Health::Failed { reason, exit_code } => terminated(&reason, exit_code, None),
    }
}

fn worst(a: Tone, b: Tone) -> Tone {
    let rank = |t| match t {
        Tone::Danger => 3,
        Tone::Warning => 2,
        Tone::Good => 1,
        Tone::Neutral => 0,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

pub(crate) fn pod_status(pod: &Pod) -> String {
    if pod.meta.deleting_since.is_some() {
        return "Terminating".into();
    }
    match (&pod.phase, &pod.reason) {
        (Some(phase), Some(reason)) => format!("{phase} ({reason})"),
        (Some(phase), None) => phase.clone(),
        (None, _) => "Unknown phase".into(),
    }
}

pub(crate) fn pod_tone(pod: &Pod) -> Tone {
    if pod.meta.deleting_since.is_some() {
        return Tone::Neutral;
    }
    let containers = pod
        .containers
        .iter()
        .map(container_tone)
        .fold(Tone::Neutral, worst);
    match pod.phase.as_deref() {
        Some("Failed") => Tone::Danger,
        Some("Succeeded") => worst(Tone::Neutral, containers),
        Some("Pending") => worst(Tone::Warning, containers),
        _ => worst(Tone::Good, containers),
    }
}

/// Reasons of a Pod, each with the container it belongs to, so a sidecar
/// never appears to share its neighbour's failure.
pub(crate) fn pod_reasons(pod: &Pod) -> Vec<String> {
    let mut reasons: Vec<String> = pod.reason.iter().cloned().collect();
    for c in &pod.containers {
        for reason in container_reasons(c) {
            reasons.push(format!("{}: {reason}", c.name));
        }
    }
    reasons
}

// ---------- Rows ----------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    Object {
        kind: Kind,
        name: String,
        uid: String,
    },
    Container {
        pod: String,
        pod_uid: String,
        container: String,
    },
}

impl Key {
    pub(crate) fn object(kind: Kind, meta: &Meta) -> Self {
        Self::Object {
            kind,
            name: meta.name.clone(),
            uid: meta.uid.clone(),
        }
    }
    pub(crate) fn uid(&self) -> &str {
        match self {
            Self::Object { uid, .. } => uid,
            Self::Container { pod_uid, .. } => pod_uid,
        }
    }
    pub(crate) fn section(&self) -> Section {
        match self {
            Self::Container { .. } => Section::Containers,
            Self::Object {
                kind: Kind::Deployment | Kind::ReplicaSet,
                ..
            } => Section::Deployments,
            Self::Object {
                kind: Kind::Service | Kind::EndpointSlice,
                ..
            } => Section::Services,
            Self::Object { .. } => Section::Pods,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub key: Key,
    pub title: String,
    pub subtitle: String,
    pub status: String,
    pub tone: Tone,
    /// Readiness fraction, kept apart from restarts and reasons.
    pub ready: Option<String>,
    pub restarts: Option<u32>,
    pub reasons: Vec<String>,
}

impl Row {
    fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let contains = |s: &str| s.to_lowercase().contains(query);
        contains(&self.title)
            || contains(&self.subtitle)
            || contains(&self.status)
            || self.reasons.iter().any(|r| contains(r))
    }
}

fn owner_label(meta: &Meta) -> Option<String> {
    meta.owners
        .iter()
        .find(|o| o.controller)
        .or(meta.owners.first())
        .map(|o| format!("{} {}", o.kind, o.name))
}

pub(crate) fn pod_row(pod: &Pod) -> Row {
    let (ready, total) = pod_ready(pod);
    let subtitle = [owner_label(&pod.meta), pod.node.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    Row {
        key: Key::object(Kind::Pod, &pod.meta),
        title: pod.meta.name.clone(),
        subtitle,
        status: pod_status(pod),
        tone: pod_tone(pod),
        ready: Some(format!("{ready}/{total}")),
        restarts: pod_restarts(pod),
        reasons: pod_reasons(pod),
    }
}

pub(crate) fn container_row(pod: &Pod, c: &Container) -> Row {
    let image = c.image.clone().or_else(|| c.status_image.clone());
    let subtitle = [
        Some(pod.meta.name.clone()),
        Some(kind_label(c.kind).into()),
        image,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    Row {
        key: Key::Container {
            pod: pod.meta.name.clone(),
            pod_uid: pod.meta.uid.clone(),
            container: c.name.clone(),
        },
        title: c.name.clone(),
        subtitle,
        status: container_status(c),
        tone: container_tone(c),
        ready: match (c.kind, c.ready) {
            (ContainerKind::Ephemeral, _) | (_, None) => None,
            (_, Some(true)) => Some("ready".into()),
            (_, Some(false)) => Some("not ready".into()),
        },
        restarts: c.restart_count,
        reasons: container_reasons(c),
    }
}

/// `readyReplicas` is omitted by the API server when zero; it is unknown only
/// while the controller has not reported status at all.
fn replicas_reported(d: &Deployment) -> bool {
    d.observed_generation.is_some() || d.status_replicas.is_some()
}

pub(crate) fn deployment_row(d: &Deployment) -> Row {
    let reported = replicas_reported(d);
    let ready = match (reported, d.replicas) {
        (true, desired) => Some(format!("{}/{}", d.ready.unwrap_or(0), desired.unwrap_or(1))),
        (false, Some(desired)) => Some(format!("?/{desired}")),
        (false, None) => None,
    };
    let mut reasons: Vec<String> = d
        .conditions
        .iter()
        .filter(|c| c.status != "True")
        .map(|c| match &c.reason {
            Some(reason) => format!("{}: {reason}", c.kind),
            None => format!("{} {}", c.kind, c.status),
        })
        .collect();
    let unavailable = d.unavailable.unwrap_or(0);
    if unavailable > 0 {
        reasons.push(format!("{unavailable} unavailable"));
    }
    let available_false = d
        .conditions
        .iter()
        .any(|c| c.kind == "Available" && c.status == "False");
    let tone = if !reported {
        Tone::Neutral
    } else if available_false {
        Tone::Danger
    } else if unavailable > 0 || !reasons.is_empty() {
        Tone::Warning
    } else {
        Tone::Good
    };
    Row {
        key: Key::object(Kind::Deployment, &d.meta),
        title: d.meta.name.clone(),
        subtitle: format!(
            "updated {} · available {}",
            d.updated.unwrap_or(0),
            d.available.unwrap_or(0)
        ),
        status: if reported {
            "Reported".into()
        } else {
            "No status yet".into()
        },
        tone,
        ready,
        restarts: None,
        reasons,
    }
}

/// Endpoint readiness from EndpointSlices. Without a slice read it is
/// unknown: a forbidden or failed read never means zero endpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Endpoints {
    Known {
        ready: usize,
        total: usize,
        partial: bool,
    },
    Unknown(String),
}

pub(crate) fn service_endpoints(data: &Data, service: &Service) -> Endpoints {
    let Some(observation) = latest(&data.slices) else {
        return Endpoints::Unknown(match data.slices.view(SystemTime::now(), Duration::MAX) {
            View::Failed { failure, .. } => failure_kind(failure).into(),
            _ => "not read".into(),
        });
    };
    let mut ready = 0;
    let mut total = 0;
    for slice in observation
        .value
        .items
        .iter()
        .filter(|s| s.service.as_deref() == Some(service.meta.name.as_str()))
    {
        for endpoint in &slice.endpoints {
            total += 1;
            if endpoint.ready == Some(true) {
                ready += 1;
            }
        }
    }
    Endpoints::Known {
        ready,
        total,
        partial: !observation.value.is_complete(),
    }
}

pub(crate) fn service_row(data: &Data, s: &Service) -> Row {
    let ports = s
        .ports
        .iter()
        .map(|p| {
            format!(
                "{}/{}",
                p.port,
                p.protocol.clone().unwrap_or_else(|| "TCP".into())
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let subtitle = [
        s.kind.clone(),
        s.cluster_ip.clone(),
        (!ports.is_empty()).then_some(ports),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let (status, tone, ready) = match service_endpoints(data, s) {
        Endpoints::Unknown(why) => (format!("Endpoints unknown ({why})"), Tone::Neutral, None),
        Endpoints::Known {
            ready,
            total,
            partial,
        } => {
            let more = if partial { "+" } else { "" };
            let tone = if s.selector.is_empty() {
                Tone::Neutral
            } else if ready == 0 {
                if partial {
                    Tone::Warning
                } else {
                    Tone::Danger
                }
            } else if ready < total {
                Tone::Warning
            } else {
                Tone::Good
            };
            (
                format!("{ready}{more} ready endpoints"),
                tone,
                Some(format!("{ready}/{total}{more}")),
            )
        }
    };
    Row {
        key: Key::object(Kind::Service, &s.meta),
        title: s.meta.name.clone(),
        subtitle,
        status,
        tone,
        ready,
        restarts: None,
        reasons: if s.selector.is_empty() {
            vec!["no selector".into()]
        } else {
            Vec::new()
        },
    }
}

pub(crate) fn rows(data: &Data, section: Section, query: &str) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    let rows: Vec<Row> = match section {
        Section::Pods => data.pods().iter().map(pod_row).collect(),
        Section::Containers => data
            .pods()
            .iter()
            .flat_map(|pod| pod.containers.iter().map(move |c| container_row(pod, c)))
            .collect(),
        Section::Deployments => items(&data.deployments)
            .iter()
            .map(deployment_row)
            .collect(),
        Section::Services => items(&data.services)
            .iter()
            .map(|s| service_row(data, s))
            .collect(),
    };
    rows.into_iter().filter(|r| r.matches(&query)).collect()
}

// ---------- Resolving a selection ----------

pub(crate) enum Resolved<'a> {
    Pod(&'a Pod),
    Container(&'a Pod, &'a Container),
    Deployment(&'a Deployment),
    ReplicaSet(&'a ReplicaSet),
    Service(&'a Service),
    /// The selected UID is no longer observed. A same-named object with a
    /// different UID is a replacement, offered but never substituted.
    Gone {
        replacement: Option<Key>,
    },
}

fn find_meta<'a, T>(list: &'a [T], meta: impl Fn(&T) -> &Meta, uid: &str) -> Option<&'a T> {
    list.iter().find(|item| meta(item).uid == uid)
}

pub(crate) fn resolve<'a>(data: &'a Data, key: &Key) -> Resolved<'a> {
    let replacement = |kind: Kind, name: &str| -> Option<Key> {
        let meta = match kind {
            Kind::Pod => data.pods().iter().map(|p| &p.meta).find(|m| m.name == name),
            Kind::Deployment => items(&data.deployments)
                .iter()
                .map(|d| &d.meta)
                .find(|m| m.name == name),
            Kind::ReplicaSet => items(&data.replica_sets)
                .iter()
                .map(|r| &r.meta)
                .find(|m| m.name == name),
            Kind::Service => items(&data.services)
                .iter()
                .map(|s| &s.meta)
                .find(|m| m.name == name),
            _ => None,
        }?;
        Some(Key::object(kind, meta))
    };
    match key {
        Key::Object { kind, name, uid } => {
            let found =
                match kind {
                    Kind::Pod => find_meta(data.pods(), |p| &p.meta, uid).map(Resolved::Pod),
                    Kind::Deployment => find_meta(items(&data.deployments), |d| &d.meta, uid)
                        .map(Resolved::Deployment),
                    Kind::ReplicaSet => find_meta(items(&data.replica_sets), |r| &r.meta, uid)
                        .map(Resolved::ReplicaSet),
                    Kind::Service => {
                        find_meta(items(&data.services), |s| &s.meta, uid).map(Resolved::Service)
                    }
                    _ => None,
                };
            found.unwrap_or_else(|| Resolved::Gone {
                replacement: replacement(*kind, name),
            })
        }
        Key::Container {
            pod,
            pod_uid,
            container,
        } => match find_meta(data.pods(), |p| &p.meta, pod_uid) {
            Some(found) => match found.container(container) {
                Some(c) => Resolved::Container(found, c),
                None => Resolved::Gone { replacement: None },
            },
            None => Resolved::Gone {
                replacement: data
                    .pods()
                    .iter()
                    .find(|p| &p.meta.name == pod && p.container(container).is_some())
                    .map(|p| Key::Container {
                        pod: p.meta.name.clone(),
                        pod_uid: p.meta.uid.clone(),
                        container: container.clone(),
                    }),
            },
        },
    }
}

// ---------- Relations ----------

/// One related object. Ownership comes only from `ownerReferences` whose UID
/// matches an observed object; label selection and endpoint membership are
/// separate relations with their own wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub kind_name: String,
    pub name: String,
    /// Present when the object is observed with exactly this UID.
    pub key: Option<Key>,
    pub note: Option<String>,
}

fn owned_by(meta: &Meta, uid: &str) -> bool {
    meta.owners.iter().any(|o| o.uid == uid)
}

pub(crate) fn owners(data: &Data, meta: &Meta) -> Vec<Link> {
    meta.owners
        .iter()
        .map(|o| {
            let key = match o.kind.as_str() {
                "ReplicaSet" => find_meta(items(&data.replica_sets), |r| &r.meta, &o.uid)
                    .map(|r| Key::object(Kind::ReplicaSet, &r.meta)),
                "Deployment" => find_meta(items(&data.deployments), |d| &d.meta, &o.uid)
                    .map(|d| Key::object(Kind::Deployment, &d.meta)),
                _ => None,
            };
            let note = key
                .is_none()
                .then(|| "not observed with this UID; shown as recorded, not linked".to_string());
            Link {
                kind_name: o.kind.clone(),
                name: o.name.clone(),
                key,
                note,
            }
        })
        .collect()
}

/// Pod → ReplicaSet → Deployment, following observed owner UIDs only.
pub(crate) fn owner_chain(data: &Data, meta: &Meta) -> Vec<Link> {
    let mut chain = owners(data, meta);
    let mut next = chain.iter().find_map(|l| l.key.clone());
    let mut guard = 0;
    while let Some(Key::Object { kind, uid, .. }) = next.take() {
        guard += 1;
        if guard > 4 {
            break;
        }
        let parent_meta = match kind {
            Kind::ReplicaSet => {
                find_meta(items(&data.replica_sets), |r| &r.meta, &uid).map(|r| &r.meta)
            }
            _ => None,
        };
        if let Some(parent_meta) = parent_meta {
            let parents = owners(data, parent_meta);
            next = parents.iter().find_map(|l| l.key.clone());
            chain.extend(parents);
        }
    }
    chain
}

pub(crate) fn replica_sets_of<'a>(data: &'a Data, deployment: &Deployment) -> Vec<&'a ReplicaSet> {
    items(&data.replica_sets)
        .iter()
        .filter(|r| owned_by(&r.meta, &deployment.meta.uid))
        .collect()
}

pub(crate) fn pods_owned_by<'a>(data: &'a Data, uid: &str) -> Vec<&'a Pod> {
    data.pods()
        .iter()
        .filter(|p| owned_by(&p.meta, uid))
        .collect()
}

pub(crate) fn pods_of_deployment<'a>(data: &'a Data, deployment: &Deployment) -> Vec<&'a Pod> {
    replica_sets_of(data, deployment)
        .into_iter()
        .flat_map(|r| pods_owned_by(data, &r.meta.uid))
        .collect()
}

/// Equality selection, as a Service selects Pods. Not ownership.
pub(crate) fn selects(
    selector: &BTreeMap<String, String>,
    labels: &BTreeMap<String, String>,
) -> bool {
    !selector.is_empty() && selector.iter().all(|(k, v)| labels.get(k) == Some(v))
}

pub(crate) fn pods_selected_by<'a>(data: &'a Data, service: &Service) -> Vec<&'a Pod> {
    data.pods()
        .iter()
        .filter(|p| selects(&service.selector, &p.meta.labels))
        .collect()
}

pub(crate) fn services_selecting<'a>(data: &'a Data, pod: &Pod) -> Vec<&'a Service> {
    items(&data.services)
        .iter()
        .filter(|s| selects(&s.selector, &pod.meta.labels))
        .collect()
}

/// EndpointSlice entries whose target reference names this Pod by UID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Membership {
    pub slice: String,
    pub service: Option<String>,
    pub ready: Option<bool>,
    pub serving: Option<bool>,
    pub terminating: Option<bool>,
}

pub(crate) fn memberships(data: &Data, pod_uid: &str) -> Vec<Membership> {
    items(&data.slices)
        .iter()
        .flat_map(|slice| {
            slice
                .endpoints
                .iter()
                .filter(|e| {
                    e.target
                        .as_ref()
                        .is_some_and(|t| t.uid.as_deref() == Some(pod_uid))
                })
                .map(move |e| Membership {
                    slice: slice.meta.name.clone(),
                    service: slice.service.clone(),
                    ready: e.ready,
                    serving: e.serving,
                    terminating: e.terminating,
                })
        })
        .collect()
}

pub(crate) fn hpa_for<'a>(
    data: &'a Data,
    deployment: &Deployment,
) -> Option<&'a HorizontalPodAutoscaler> {
    items(&data.hpas)
        .iter()
        .find(|h| h.target.kind == "Deployment" && h.target.name == deployment.meta.name)
}

// ---------- Events ----------

/// Events of one involved object with one type and reason. The involved
/// object is always named: a Deployment view also lists its Pods' events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EventGroup {
    pub involved: String,
    pub involved_uid: Option<String>,
    pub kind: String,
    pub reason: String,
    pub message: String,
    pub count: i64,
    pub last: Option<String>,
}

pub(crate) fn event_groups(data: &Data, uids: &[&str]) -> Vec<EventGroup> {
    let mut groups: Vec<EventGroup> = Vec::new();
    for event in items(&data.events) {
        let Some(uid) = event.regarding.uid.as_deref() else {
            continue;
        };
        if !uids.contains(&uid) {
            continue;
        }
        let kind = event.kind.clone().unwrap_or_else(|| "Normal".into());
        let reason = event.reason.clone().unwrap_or_else(|| "(no reason)".into());
        let last = event
            .last_seen
            .clone()
            .or_else(|| event.event_time.clone())
            .or_else(|| event.first_seen.clone());
        let count = i64::from(event.count.unwrap_or(1).max(1));
        let message = event.message.clone().unwrap_or_default();
        match groups.iter_mut().find(|g| {
            g.involved_uid.as_deref() == Some(uid) && g.kind == kind && g.reason == reason
        }) {
            Some(group) => {
                group.count += count;
                if last > group.last {
                    group.last = last;
                    group.message = message;
                }
            }
            None => groups.push(EventGroup {
                involved: format!("{}/{}", event.regarding.kind, event.regarding.name),
                involved_uid: Some(uid.to_string()),
                kind,
                reason,
                message,
                count,
                last,
            }),
        }
    }
    groups.sort_by(|a, b| b.last.cmp(&a.last).then(a.involved.cmp(&b.involved)));
    groups
}

// ---------- Log targets ----------

/// What an explicit Open logs pins: context, namespace, the Pod by name and
/// UID, container and instance. Selecting rows never changes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogTarget {
    pub context: String,
    pub cluster: Option<String>,
    pub server: Option<String>,
    pub namespace: String,
    pub pod: String,
    pub pod_uid: String,
    pub container: String,
    pub previous: bool,
}

impl LogTarget {
    pub(crate) fn new(
        scope: &Scope,
        pod: &Pod,
        container: &str,
        previous: bool,
    ) -> Result<Self, String> {
        let found = pod
            .container(container)
            .ok_or_else(|| format!("Container {container} is not in Pod {}", pod.meta.name))?;
        if previous && !found.previous_instance_observed() {
            return Err(format!(
                "No previous instance of {container} was observed (no last termination)."
            ));
        }
        Ok(Self {
            context: scope.connection.context.clone(),
            cluster: scope.connection.cluster.clone(),
            server: scope.connection.server.clone(),
            namespace: pod
                .meta
                .namespace
                .clone()
                .unwrap_or_else(|| scope.namespace.clone()),
            pod: pod.meta.name.clone(),
            pod_uid: pod.meta.uid.clone(),
            container: container.to_string(),
            previous,
        })
    }
    pub(crate) fn connection(&self) -> Connection {
        Connection {
            context: self.context.clone(),
            cluster: self.cluster.clone(),
            server: self.server.clone(),
            default_namespace: None,
        }
    }
    pub(crate) fn instance(&self) -> &'static str {
        if self.previous {
            "Previous instance"
        } else {
            "Current instance"
        }
    }
    pub(crate) fn short_uid(&self) -> &str {
        short_uid(&self.pod_uid)
    }
    pub(crate) fn label(&self) -> String {
        format!(
            "{} · {}/{}/{} · Pod UID {} · {}",
            self.context,
            self.namespace,
            self.pod,
            self.container,
            self.short_uid(),
            self.instance()
        )
    }
    pub(crate) fn in_scope(&self, scope: &Scope) -> bool {
        self.context == scope.connection.context && self.namespace == scope.namespace
    }
}

pub(crate) fn short_uid(uid: &str) -> &str {
    uid.get(..8).unwrap_or(uid)
}

/// The Pod a name refers to now, compared with the pinned UID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UidCheck {
    Same,
    Replaced {
        uid: String,
    },
    Missing,
    /// The current data is another scope, or Pods were not read.
    Unknown,
}

/// Against a fresh single-Pod read (before a log session starts).
pub(crate) fn check_fetched(target: &LogTarget, fetched: &Observation<Pod>) -> UidCheck {
    if fetched.source.context != target.context || fetched.value.meta.name != target.pod {
        return UidCheck::Unknown;
    }
    if fetched.value.meta.uid == target.pod_uid {
        UidCheck::Same
    } else {
        UidCheck::Replaced {
            uid: fetched.value.meta.uid.clone(),
        }
    }
}

/// Against the latest Pods list of the shown scope (while a session runs or
/// after it ended). Detects a replacement the session itself cannot see.
pub(crate) fn check_listed(target: &LogTarget, data: &Data) -> UidCheck {
    if !target.in_scope(&data.scope) {
        return UidCheck::Unknown;
    }
    let Some(observation) = data.pod_observation() else {
        return UidCheck::Unknown;
    };
    match observation
        .value
        .items
        .iter()
        .find(|p| p.meta.name == target.pod)
    {
        Some(pod) if pod.meta.uid == target.pod_uid => UidCheck::Same,
        Some(pod) => UidCheck::Replaced {
            uid: pod.meta.uid.clone(),
        },
        None if observation.value.is_complete() => UidCheck::Missing,
        None => UidCheck::Unknown,
    }
}

// ---------- Manifest (read-only, typed fields only) ----------

struct Yaml(String);

impl Yaml {
    fn line(&mut self, depth: usize, text: impl AsRef<str>) {
        for _ in 0..depth {
            self.0.push_str("  ");
        }
        self.0.push_str(text.as_ref());
        self.0.push('\n');
    }
    fn field(&mut self, depth: usize, key: &str, value: Option<impl std::fmt::Display>) {
        if let Some(value) = value {
            self.line(depth, format!("{key}: {}", quote(&value.to_string())));
        }
    }
    fn map(&mut self, depth: usize, key: &str, map: &BTreeMap<String, String>) {
        if map.is_empty() {
            return;
        }
        self.line(depth, format!("{key}:"));
        for (k, v) in map {
            self.line(depth + 1, format!("{}: {}", quote(k), quote(v)));
        }
    }
}

fn quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@+".contains(c))
        && !matches!(s, "true" | "false" | "null" | "yes" | "no");
    if plain {
        s.to_string()
    } else {
        format!("{s:?}")
    }
}

fn metadata(y: &mut Yaml, meta: &Meta) {
    y.line(0, "metadata:");
    y.field(1, "name", Some(&meta.name));
    y.field(1, "namespace", meta.namespace.as_ref());
    y.field(1, "uid", Some(&meta.uid));
    y.field(1, "resourceVersion", Some(&meta.resource_version.0));
    y.field(1, "generation", meta.generation);
    y.field(1, "creationTimestamp", meta.created.as_ref());
    y.field(1, "deletionTimestamp", meta.deleting_since.as_ref());
    y.map(1, "labels", &meta.labels);
    if !meta.owners.is_empty() {
        y.line(1, "ownerReferences:");
        for o in &meta.owners {
            y.line(2, format!("- kind: {}", quote(&o.kind)));
            y.field(3, "name", Some(&o.name));
            y.field(3, "uid", Some(&o.uid));
            y.field(3, "controller", Some(o.controller));
        }
    }
}

fn state_yaml(y: &mut Yaml, depth: usize, key: &str, state: &Option<State>) {
    let Some(state) = state else {
        return;
    };
    y.line(depth, format!("{key}:"));
    match state {
        State::Waiting { reason, message } => {
            y.line(depth + 1, "waiting:");
            y.field(depth + 2, "reason", reason.as_ref());
            y.field(depth + 2, "message", message.as_ref());
        }
        State::Running { started_at } => {
            y.line(depth + 1, "running:");
            y.field(depth + 2, "startedAt", started_at.as_ref());
        }
        State::Terminated {
            reason,
            message,
            exit_code,
            signal,
            started_at,
            finished_at,
        } => {
            y.line(depth + 1, "terminated:");
            y.field(depth + 2, "reason", reason.as_ref());
            y.field(depth + 2, "exitCode", Some(exit_code));
            y.field(depth + 2, "signal", signal.as_ref());
            y.field(depth + 2, "message", message.as_ref());
            y.field(depth + 2, "startedAt", started_at.as_ref());
            y.field(depth + 2, "finishedAt", finished_at.as_ref());
        }
    }
}

fn container_yaml(y: &mut Yaml, depth: usize, c: &Container) {
    y.line(depth, format!("- name: {}", quote(&c.name)));
    let d = depth + 1;
    y.field(d, "image", c.image.as_ref());
    if !c.resources.requests.is_empty() || !c.resources.limits.is_empty() {
        y.line(d, "resources:");
        y.map(d + 1, "requests", &c.resources.requests);
        y.map(d + 1, "limits", &c.resources.limits);
    }
}

fn status_yaml(y: &mut Yaml, depth: usize, c: &Container) {
    y.line(depth, format!("- name: {}", quote(&c.name)));
    let d = depth + 1;
    if !c.status_reported {
        y.line(d, "# no status reported");
        return;
    }
    y.field(d, "ready", c.ready);
    y.field(d, "started", c.started);
    y.field(d, "restartCount", c.restart_count);
    y.field(d, "image", c.status_image.as_ref());
    y.field(d, "imageID", c.image_id.as_ref());
    state_yaml(y, d, "state", &c.state);
    state_yaml(y, d, "lastState", &c.last_state);
}

pub(crate) const MANIFEST_NOTE: &str = "Read-only. Okilum keeps typed fields only: environment values, exec probe commands and secrets are never retained, so this is not the server's complete object.";

pub(crate) fn manifest(resolved: &Resolved) -> Option<String> {
    let mut y = Yaml(String::new());
    match resolved {
        Resolved::Pod(pod) | Resolved::Container(pod, _) => {
            y.line(0, "apiVersion: v1");
            y.line(0, "kind: Pod");
            metadata(&mut y, &pod.meta);
            y.line(0, "spec:");
            y.field(1, "nodeName", pod.node.as_ref());
            for (key, kinds) in [
                (
                    "initContainers",
                    &[ContainerKind::Init, ContainerKind::Sidecar][..],
                ),
                ("containers", &[ContainerKind::Regular][..]),
                ("ephemeralContainers", &[ContainerKind::Ephemeral][..]),
            ] {
                let list: Vec<_> = pod
                    .containers
                    .iter()
                    .filter(|c| kinds.contains(&c.kind))
                    .collect();
                if list.is_empty() {
                    continue;
                }
                y.line(1, format!("{key}:"));
                for c in list {
                    container_yaml(&mut y, 2, c);
                    if c.kind == ContainerKind::Sidecar {
                        y.line(3, "restartPolicy: Always");
                    }
                }
            }
            y.line(0, "status:");
            y.field(1, "phase", pod.phase.as_ref());
            y.field(1, "reason", pod.reason.as_ref());
            y.field(1, "podIP", pod.pod_ip.as_ref());
            y.field(1, "qosClass", pod.qos_class.as_ref());
            for (key, kinds) in [
                (
                    "initContainerStatuses",
                    &[ContainerKind::Init, ContainerKind::Sidecar][..],
                ),
                ("containerStatuses", &[ContainerKind::Regular][..]),
                (
                    "ephemeralContainerStatuses",
                    &[ContainerKind::Ephemeral][..],
                ),
            ] {
                let list: Vec<_> = pod
                    .containers
                    .iter()
                    .filter(|c| kinds.contains(&c.kind))
                    .collect();
                if list.is_empty() {
                    continue;
                }
                y.line(1, format!("{key}:"));
                for c in list {
                    status_yaml(&mut y, 2, c);
                }
            }
        }
        Resolved::Deployment(d) => {
            y.line(0, "apiVersion: apps/v1");
            y.line(0, "kind: Deployment");
            metadata(&mut y, &d.meta);
            y.line(0, "spec:");
            y.field(1, "replicas", d.replicas);
            y.line(1, "selector:");
            y.map(2, "matchLabels", &d.selector.match_labels);
            y.line(1, "# spec.template is not retained by the read adapter");
            y.line(0, "status:");
            y.field(1, "observedGeneration", d.observed_generation);
            y.field(1, "replicas", d.status_replicas);
            y.field(1, "readyReplicas", d.ready);
            y.field(1, "availableReplicas", d.available);
            y.field(1, "updatedReplicas", d.updated);
            y.field(1, "unavailableReplicas", d.unavailable);
        }
        Resolved::ReplicaSet(r) => {
            y.line(0, "apiVersion: apps/v1");
            y.line(0, "kind: ReplicaSet");
            metadata(&mut y, &r.meta);
            y.line(0, "spec:");
            y.field(1, "replicas", r.replicas);
            y.line(0, "status:");
            y.field(1, "replicas", r.status_replicas);
            y.field(1, "readyReplicas", r.ready);
            y.field(1, "availableReplicas", r.available);
        }
        Resolved::Service(s) => {
            y.line(0, "apiVersion: v1");
            y.line(0, "kind: Service");
            metadata(&mut y, &s.meta);
            y.line(0, "spec:");
            y.field(1, "type", s.kind.as_ref());
            y.field(1, "clusterIP", s.cluster_ip.as_ref());
            y.map(1, "selector", &s.selector);
            if !s.ports.is_empty() {
                y.line(1, "ports:");
                for p in &s.ports {
                    y.line(2, format!("- port: {}", p.port));
                    y.field(3, "name", p.name.as_ref());
                    y.field(3, "protocol", p.protocol.as_ref());
                    y.field(3, "targetPort", p.target_port.as_ref());
                    y.field(3, "nodePort", p.node_port);
                }
            }
        }
        Resolved::Gone { .. } => return None,
    }
    Some(y.0)
}

#[cfg(test)]
#[path = "model_tests.rs"]
pub(crate) mod tests;
