//! Triage and Service path (#3011): two lenses over the same Workspace `Data`,
//! selection, inspector and log dock. Pure data, no GPUI and no reads: both
//! are computed from the slots the Workspace already holds.
//! docs/kubernetes-workspace.md §Lenses describes the rules kept here.
use super::model::{self, Data, Key, Row, Tone};
use okilum_core::kubernetes::{
    Container, ContainerKind, Deployment, EndpointSlice, Kind, Pod, Service, State, View,
};
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lens {
    Workspace,
    Triage,
    ServicePath,
}

impl Lens {
    pub(crate) const ALL: [Lens; 3] = [Lens::Workspace, Lens::Triage, Lens::ServicePath];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Triage => "Triage",
            Self::ServicePath => "Service path",
        }
    }
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Triage => "triage",
            Self::ServicePath => "service-path",
        }
    }
}

// ---------- Triage ----------

/// The rules Triage applies, shown verbatim in the view. Nothing else makes a
/// row appear, and nothing here clears a row because a Pod became Ready.
pub(crate) const RULES: [&str; 6] = [
    "A container waits with a reason other than ContainerCreating or PodInitializing.",
    "A container's current instance terminated with a non-zero exit code.",
    "A regular or sidecar container runs but is not ready.",
    "A container's last termination was non-zero (kept while lastState is reported, even after it is Ready again).",
    "A Pod's phase is Failed, Unknown or Pending.",
    "A Deployment reports unavailable replicas or Available=False.",
];

/// Ordered for investigation: what fails now, then what is not ready, then
/// what recovered but keeps a prior failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Category {
    CurrentFailure,
    NotReady,
    Recovered,
}

impl Category {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CurrentFailure => "Failing now",
            Self::NotReady => "Not ready",
            Self::Recovered => "Recovered · prior failure kept",
        }
    }
    pub(crate) fn tone(self) -> Tone {
        match self {
            Self::CurrentFailure => Tone::Danger,
            Self::NotReady | Self::Recovered => Tone::Warning,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Finding {
    /// Index into `RULES`.
    pub rule: usize,
    pub detail: String,
    /// The object's own timestamp for this finding, when it reports one.
    pub at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    pub key: Key,
    pub title: String,
    pub subtitle: String,
    pub category: Category,
    pub findings: Vec<Finding>,
}

impl Observation {
    fn latest(&self) -> Option<&str> {
        self.findings.iter().filter_map(|f| f.at.as_deref()).max()
    }
    pub(crate) fn row(&self) -> Row {
        Row {
            key: self.key.clone(),
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            status: self.category.label().into(),
            tone: self.category.tone(),
            ready: None,
            restarts: None,
            reasons: self
                .findings
                .iter()
                .map(|f| match &f.at {
                    Some(at) => format!("{} · {at}", f.detail),
                    None => f.detail.clone(),
                })
                .collect(),
        }
    }
}

/// Denominators and counters for the summary. Restarts are a cumulative
/// counter since each container was created, never an incident count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Summary {
    pub pods: usize,
    pub pods_ready: usize,
    pub pods_with_observations: usize,
    pub restarts: Option<u32>,
    pub complete: bool,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Triage {
    /// Pods were never read in this scope.
    Unread,
    /// The Pods read failed with nothing earlier: attention is unknown.
    Unknown(String),
    Ready {
        summary: Summary,
        observations: Vec<Observation>,
        observed_at: SystemTime,
        /// Set when the shown Pods are older than a failed read.
        stale: Option<String>,
    },
}

fn terminated(state: &State) -> Option<(String, Option<String>)> {
    match state {
        State::Terminated {
            reason,
            exit_code,
            signal,
            finished_at,
            ..
        } if *exit_code != 0 => {
            let reason = reason.as_deref().unwrap_or("Terminated");
            let text = match signal {
                Some(signal) => format!("{reason} (exit {exit_code}, signal {signal})"),
                None => format!("{reason} (exit {exit_code})"),
            };
            Some((text, finished_at.clone()))
        }
        _ => None,
    }
}

fn container_observation(pod: &Pod, c: &Container) -> Option<Observation> {
    fn raise(c: Category, category: &mut Option<Category>) {
        *category = Some(category.map_or(c, |old| old.min(c)));
    }
    let mut findings = Vec::new();
    let mut category = None::<Category>;
    match &c.state {
        Some(State::Waiting {
            reason: Some(reason),
            ..
        }) if !["ContainerCreating", "PodInitializing"].contains(&reason.as_str()) => {
            findings.push(Finding {
                rule: 0,
                detail: format!("Waiting: {reason}"),
                at: None,
            });
            raise(Category::CurrentFailure, &mut category);
        }
        Some(state @ State::Terminated { .. }) => {
            if let Some((text, at)) = terminated(state) {
                findings.push(Finding {
                    rule: 1,
                    detail: format!("Current instance: {text}"),
                    at,
                });
                raise(Category::CurrentFailure, &mut category);
            }
        }
        Some(State::Running { .. })
            if c.ready == Some(false)
                && matches!(c.kind, ContainerKind::Regular | ContainerKind::Sidecar) =>
        {
            findings.push(Finding {
                rule: 2,
                detail: "Running, not ready".into(),
                at: None,
            });
            raise(Category::NotReady, &mut category);
        }
        _ => {}
    }
    if let Some((text, at)) = c.last_state.as_ref().and_then(terminated) {
        findings.push(Finding {
            rule: 3,
            detail: format!("Last termination: {text}"),
            at,
        });
        raise(Category::Recovered, &mut category);
    }
    let category = category?;
    let restarts = c.restart_count.map_or_else(
        || "restarts unknown".into(),
        |n| format!("{n} restarts (cumulative)"),
    );
    Some(Observation {
        key: Key::Container {
            pod: pod.meta.name.clone(),
            pod_uid: pod.meta.uid.clone(),
            container: c.name.clone(),
        },
        title: c.name.clone(),
        subtitle: format!(
            "{} · {} · {restarts}",
            pod.meta.name,
            model::kind_label(c.kind)
        ),
        category,
        findings,
    })
}

fn pod_observation(pod: &Pod) -> Option<Observation> {
    let phase = pod.phase.as_deref()?;
    let category = match phase {
        "Failed" | "Unknown" => Category::CurrentFailure,
        "Pending" => Category::NotReady,
        _ => return None,
    };
    let detail = match (&pod.reason, &pod.message) {
        (Some(r), Some(m)) => format!("Phase {phase}: {r} — {m}"),
        (Some(r), None) => format!("Phase {phase}: {r}"),
        _ => format!("Phase {phase}"),
    };
    Some(Observation {
        key: Key::object(Kind::Pod, &pod.meta),
        title: pod.meta.name.clone(),
        subtitle: format!("Pod · {}", pod.node.as_deref().unwrap_or("no node")),
        category,
        findings: vec![Finding {
            rule: 4,
            detail,
            at: None,
        }],
    })
}

fn deployment_observation(d: &Deployment) -> Option<Observation> {
    let mut findings = Vec::new();
    let unavailable = d.unavailable.unwrap_or(0);
    if unavailable > 0 {
        findings.push(Finding {
            rule: 5,
            detail: format!(
                "{unavailable} of {} replicas unavailable",
                d.replicas.map_or_else(|| "?".into(), |r| r.to_string())
            ),
            at: None,
        });
    }
    let available_false = d
        .conditions
        .iter()
        .find(|c| c.kind == "Available" && c.status == "False");
    if let Some(c) = available_false {
        findings.push(Finding {
            rule: 5,
            detail: format!(
                "Available=False{}",
                c.reason
                    .as_ref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            ),
            at: c.last_transition.clone(),
        });
    }
    if findings.is_empty() {
        return None;
    }
    Some(Observation {
        key: Key::object(Kind::Deployment, &d.meta),
        title: d.meta.name.clone(),
        subtitle: "Deployment".into(),
        category: if available_false.is_some() {
            Category::CurrentFailure
        } else {
            Category::NotReady
        },
        findings,
    })
}

pub(crate) fn triage(data: &Data) -> Triage {
    let (observation, stale) = match data.pods.view(SystemTime::now(), Duration::MAX) {
        View::Unread => return Triage::Unread,
        View::Failed { failure, .. } => {
            return Triage::Unknown(format!(
                "Pods could not be read ({}). Attention is unknown, not zero.",
                model::failure_kind(failure)
            ))
        }
        View::Fresh(o) => (o, None),
        View::Stale { last, why } => (
            last,
            match why {
                okilum_core::kubernetes::Staleness::Failed { failure, .. } => Some(format!(
                    "The latest Pods read failed ({}); these observations are from {}.",
                    model::failure_kind(failure),
                    model::clock(last.observed_at)
                )),
                okilum_core::kubernetes::Staleness::Aged { .. } => None,
            },
        ),
    };
    let pods = &observation.value.items;
    let mut observations = Vec::new();
    let mut flagged = std::collections::BTreeSet::new();
    for pod in pods {
        let before = observations.len();
        observations.extend(pod_observation(pod));
        observations.extend(
            pod.containers
                .iter()
                .filter_map(|c| container_observation(pod, c)),
        );
        if observations.len() > before {
            flagged.insert(pod.meta.uid.as_str());
        }
    }
    if let Some(deployments) = model::latest(&data.deployments) {
        observations.extend(
            deployments
                .value
                .items
                .iter()
                .filter_map(deployment_observation),
        );
    }
    // Stable order: category, newest reported time, then name. Selection is
    // by key, so re-sorting on refresh never changes what is selected.
    observations.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| b.latest().cmp(&a.latest()))
            .then_with(|| a.subtitle.cmp(&b.subtitle))
            .then_with(|| a.title.cmp(&b.title))
    });
    let pods_ready = pods
        .iter()
        .filter(|p| {
            let (ready, total) = model::pod_ready(p);
            total > 0 && ready == total
        })
        .count();
    let restarts = pods
        .iter()
        .filter_map(model::pod_restarts)
        .reduce(|a, b| a.saturating_add(b));
    Triage::Ready {
        summary: Summary {
            pods: pods.len(),
            pods_ready,
            pods_with_observations: flagged.len(),
            restarts,
            complete: observation.value.is_complete(),
        },
        observations,
        observed_at: observation.observed_at,
        stale,
    }
}

pub(crate) fn triage_rows(data: &Data, query: &str) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    match triage(data) {
        Triage::Ready { observations, .. } => observations
            .iter()
            .map(Observation::row)
            .filter(|row| {
                query.is_empty()
                    || [&row.title, &row.subtitle, &row.status]
                        .iter()
                        .any(|s| s.to_lowercase().contains(&query))
                    || row
                        .reasons
                        .iter()
                        .any(|r| r.to_lowercase().contains(&query))
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---------- Service path ----------

/// What an EndpointSlice endpoint points at, by `targetRef` UID.
#[derive(Debug, PartialEq)]
pub(crate) enum Target<'a> {
    Pod(&'a Pod),
    /// A Pod UID the latest Pods read does not contain: deleted, replaced, or
    /// beyond a partial list. Never matched by name.
    MissingPod {
        name: String,
        uid: Option<String>,
        pods_complete: bool,
    },
    Other {
        kind: String,
        name: String,
    },
    None,
}

#[derive(Debug, PartialEq)]
pub(crate) struct PathEndpoint<'a> {
    pub addresses: Vec<String>,
    pub ready: Option<bool>,
    pub serving: Option<bool>,
    pub terminating: Option<bool>,
    pub target: Target<'a>,
    /// A target Pod whose labels the Service selector no longer matches.
    pub not_selected: bool,
}

#[derive(Debug, PartialEq)]
pub(crate) struct PathSlice<'a> {
    pub slice: &'a EndpointSlice,
    pub endpoints: Vec<PathEndpoint<'a>>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Routing<'a> {
    /// EndpointSlices were not readable (forbidden, failed, not read): the
    /// endpoints are unknown, never zero.
    Unknown(String),
    Known {
        slices: Vec<PathSlice<'a>>,
        /// The slice list was a first page or had undecodable items.
        partial: bool,
        /// Shown slices are older than a failed EndpointSlices read.
        stale: Option<String>,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) struct ServicePath<'a> {
    pub service: &'a Service,
    /// `port/proto → targetPort` per Service port.
    pub ports: Vec<String>,
    pub routing: Routing<'a>,
    /// Pods the selector matches that are not an endpoint of any shown slice:
    /// membership is still changing, or the Pod was never admitted.
    pub selected_not_endpoints: Vec<&'a Pod>,
}

impl ServicePath<'_> {
    pub(crate) fn endpoint_counts(&self) -> Option<(usize, usize)> {
        match &self.routing {
            Routing::Known { slices, .. } => {
                let all = slices.iter().flat_map(|s| &s.endpoints);
                let total = all.clone().count();
                let ready = all.filter(|e| e.ready == Some(true)).count();
                Some((ready, total))
            }
            Routing::Unknown(_) => None,
        }
    }
}

pub(crate) fn service_path<'a>(data: &'a Data, service: &'a Service) -> ServicePath<'a> {
    let ports = service
        .ports
        .iter()
        .map(|p| {
            format!(
                "{}{}/{} → {}",
                p.name.as_ref().map(|n| format!("{n} ")).unwrap_or_default(),
                p.port,
                p.protocol.as_deref().unwrap_or("TCP"),
                p.target_port.as_deref().unwrap_or("same port")
            )
        })
        .collect();
    let pods_observation = data.pod_observation();
    let pods_complete = pods_observation.is_some_and(|o| o.value.is_complete());
    let routing = match data.slices.view(SystemTime::now(), Duration::MAX) {
        View::Unread => Routing::Unknown("EndpointSlices not read".into()),
        View::Failed { failure, .. } => Routing::Unknown(format!(
            "EndpointSlices could not be read ({})",
            model::failure_kind(failure)
        )),
        View::Fresh(o) | View::Stale { last: o, .. } => {
            let stale = match data.slices.view(SystemTime::now(), Duration::MAX) {
                View::Stale {
                    why: okilum_core::kubernetes::Staleness::Failed { failure, .. },
                    ..
                } => Some(format!(
                    "The latest EndpointSlices read failed ({}); membership is from {}.",
                    model::failure_kind(failure),
                    model::clock(o.observed_at)
                )),
                _ => None,
            };
            let slices =
                o.value
                    .items
                    .iter()
                    .filter(|s| s.service.as_deref() == Some(service.meta.name.as_str()))
                    .map(|slice| PathSlice {
                        slice,
                        endpoints: slice
                            .endpoints
                            .iter()
                            .map(|e| {
                                let target = match &e.target {
                                    None => Target::None,
                                    Some(t) if t.kind != "Pod" => Target::Other {
                                        kind: t.kind.clone(),
                                        name: t.name.clone(),
                                    },
                                    Some(t) => match t.uid.as_deref().and_then(|uid| {
                                        data.pods().iter().find(|p| p.meta.uid == uid)
                                    }) {
                                        Some(pod) => Target::Pod(pod),
                                        None => Target::MissingPod {
                                            name: t.name.clone(),
                                            uid: t.uid.clone(),
                                            pods_complete,
                                        },
                                    },
                                };
                                let not_selected = matches!(target, Target::Pod(pod)
                                if !service.selector.is_empty()
                                    && !model::selects(&service.selector, &pod.meta.labels));
                                PathEndpoint {
                                    addresses: e.addresses.clone(),
                                    ready: e.ready,
                                    serving: e.serving,
                                    terminating: e.terminating,
                                    target,
                                    not_selected,
                                }
                            })
                            .collect(),
                    })
                    .collect::<Vec<_>>();
            Routing::Known {
                slices,
                partial: !o.value.is_complete(),
                stale,
            }
        }
    };
    let selected_not_endpoints = match &routing {
        Routing::Known { slices, .. } => model::pods_selected_by(data, service)
            .into_iter()
            .filter(|pod| {
                !slices.iter().flat_map(|s| &s.endpoints).any(
                    |e| matches!(e.target, Target::Pod(target) if target.meta.uid == pod.meta.uid),
                )
            })
            .collect(),
        Routing::Unknown(_) => Vec::new(),
    };
    ServicePath {
        service,
        ports,
        routing,
        selected_not_endpoints,
    }
}

/// The Service a path lens is anchored on, by UID.
pub(crate) fn find_service<'a>(data: &'a Data, key: &Key) -> Option<&'a Service> {
    let Key::Object {
        kind: Kind::Service,
        uid,
        ..
    } = key
    else {
        return None;
    };
    model::latest(&data.services)?
        .value
        .items
        .iter()
        .find(|s| &s.meta.uid == uid)
}

/// The Deployment that owns a Pod through a ReplicaSet with matching UIDs.
pub(crate) fn owning_deployment<'a>(data: &'a Data, pod: &Pod) -> Option<&'a Deployment> {
    let chain = model::owner_chain(data, &pod.meta);
    let uid = chain.iter().find_map(|link| match &link.key {
        Some(Key::Object {
            kind: Kind::Deployment,
            uid,
            ..
        }) => Some(uid.clone()),
        _ => None,
    })?;
    model::latest(&data.deployments)?
        .value
        .items
        .iter()
        .find(|d| d.meta.uid == uid)
}

#[cfg(test)]
#[path = "lenses_tests.rs"]
pub(crate) mod tests;
