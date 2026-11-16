//! Reads for the Workspace, off the render path. Every read is one call of the
//! typed adapter (#3009): bounded, cancellable, explicit context. Nothing runs
//! because a row is selected; only Load/Refresh and Open logs read.
use super::model::{Data, Scope};
use okilum_core::kubernetes::{
    Cancel, Client, Connection, Contexts, Deployment, EndpointSlice, Event, Failure,
    HorizontalPodAutoscaler, Kubectl, List, ListQuery, Namespace, Observation, Pod, ReplicaSet,
    Service, Ticket, Transport,
};
use std::sync::Arc;
use std::time::SystemTime;

type Read<T> = Result<Observation<List<T>>, Failure>;

/// Object-safe view of `Client<T>`, so tests can supply a synthetic transport.
pub(crate) trait Reader: Send + Sync + 'static {
    fn contexts(&self, cancel: &Cancel) -> Result<Contexts, Failure>;
    fn namespaces(&self, c: &Connection, cancel: &Cancel) -> Read<Namespace>;
    fn pods(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<Pod>;
    fn deployments(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<Deployment>;
    fn replica_sets(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<ReplicaSet>;
    fn services(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<Service>;
    fn slices(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<EndpointSlice>;
    fn events(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<Event>;
    fn hpas(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<HorizontalPodAutoscaler>;
    fn pod(
        &self,
        c: &Connection,
        ns: &str,
        name: &str,
        cancel: &Cancel,
    ) -> Result<Observation<Pod>, Failure>;
}

impl<T: Transport + Send + Sync + 'static> Reader for Client<T> {
    fn contexts(&self, cancel: &Cancel) -> Result<Contexts, Failure> {
        Client::contexts(self, cancel)
    }
    fn namespaces(&self, c: &Connection, cancel: &Cancel) -> Read<Namespace> {
        Client::namespaces(self, c, &ListQuery::default(), cancel)
    }
    fn pods(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<Pod> {
        self.list(c, ns, &ListQuery::default(), cancel)
    }
    fn deployments(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<Deployment> {
        self.list(c, ns, &ListQuery::default(), cancel)
    }
    fn replica_sets(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<ReplicaSet> {
        self.list(c, ns, &ListQuery::default(), cancel)
    }
    fn services(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<Service> {
        self.list(c, ns, &ListQuery::default(), cancel)
    }
    fn slices(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<EndpointSlice> {
        self.list(c, ns, &ListQuery::default(), cancel)
    }
    fn events(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<Event> {
        self.list(c, ns, &ListQuery::default(), cancel)
    }
    fn hpas(&self, c: &Connection, ns: &str, cancel: &Cancel) -> Read<HorizontalPodAutoscaler> {
        self.list(c, ns, &ListQuery::default(), cancel)
    }
    fn pod(
        &self,
        c: &Connection,
        ns: &str,
        name: &str,
        cancel: &Cancel,
    ) -> Result<Observation<Pod>, Failure> {
        self.get(c, ns, name, cancel)
    }
}

pub(crate) fn production() -> Arc<dyn Reader> {
    Arc::new(Client::<Kubectl>::default())
}

/// Tickets are taken on the UI thread before a Load starts, so a slow older
/// Load can never replace a newer one's results (Slot read-start order).
pub(crate) struct Tickets {
    pods: Ticket,
    deployments: Ticket,
    replica_sets: Ticket,
    services: Ticket,
    slices: Ticket,
    events: Ticket,
    hpas: Ticket,
}

impl Tickets {
    pub(crate) fn take(data: &Data) -> Self {
        Self {
            pods: data.pods.begin(),
            deployments: data.deployments.begin(),
            replica_sets: data.replica_sets.begin(),
            services: data.services.begin(),
            slices: data.slices.begin(),
            events: data.events.begin(),
            hpas: data.hpas.begin(),
        }
    }
}

pub(crate) enum Loaded {
    Namespaces(Read<Namespace>),
    Pods(Ticket, Read<Pod>),
    Deployments(Ticket, Read<Deployment>),
    ReplicaSets(Ticket, Read<ReplicaSet>),
    Services(Ticket, Read<Service>),
    Slices(Ticket, Read<EndpointSlice>),
    Events(Ticket, Read<Event>),
    Hpas(Ticket, Read<HorizontalPodAutoscaler>),
    Done,
}

impl Loaded {
    /// Applies one result to the slots of `data`; returns whether it changed.
    pub(crate) fn apply(self, data: &mut Data) -> bool {
        let now = SystemTime::now();
        match self {
            Self::Pods(t, r) => data.pods.record(t, r, now),
            Self::Deployments(t, r) => data.deployments.record(t, r, now),
            Self::ReplicaSets(t, r) => data.replica_sets.record(t, r, now),
            Self::Services(t, r) => data.services.record(t, r, now),
            Self::Slices(t, r) => data.slices.record(t, r, now),
            Self::Events(t, r) => data.events.record(t, r, now),
            Self::Hpas(t, r) => data.hpas.record(t, r, now),
            Self::Namespaces(_) | Self::Done => false,
        }
    }
}

/// One Load: the namespace listing (optional, may be forbidden) and one page
/// of each kind, in order, each result sent as soon as it arrives. Stops at
/// the first cancelled read; an auth failure stops the rest too, since every
/// further read would ask the same credential plugin again.
pub(crate) fn load(
    reader: Arc<dyn Reader>,
    scope: Scope,
    tickets: Tickets,
    cancel: Cancel,
    typed_namespace: bool,
) -> async_channel::Receiver<Loaded> {
    let (sender, receiver) = async_channel::unbounded();
    std::thread::spawn(move || {
        let c = &scope.connection;
        let ns = scope.namespace.as_str();
        let stop = |failure: Option<&Failure>| {
            cancel.is_cancelled()
                || matches!(
                    failure,
                    Some(
                        Failure::Cancelled
                            | Failure::AuthExpired { .. }
                            | Failure::InteractiveLogin { .. }
                            | Failure::Configuration { .. }
                    )
                )
        };
        macro_rules! step {
            ($variant:ident, $ticket:expr, $read:expr) => {{
                let result = $read;
                let failed = result.as_ref().err().cloned();
                if sender
                    .send_blocking(Loaded::$variant($ticket, result))
                    .is_err()
                    || stop(failed.as_ref())
                {
                    let _ = sender.send_blocking(Loaded::Done);
                    return;
                }
            }};
        }
        if typed_namespace {
            let result = reader.namespaces(c, &cancel);
            let failed = result.as_ref().err().cloned();
            // A denied listing is ordinary; only auth or cancel stops here.
            if sender.send_blocking(Loaded::Namespaces(result)).is_err()
                || (stop(failed.as_ref()) && !matches!(failed, Some(Failure::Forbidden { .. })))
            {
                let _ = sender.send_blocking(Loaded::Done);
                return;
            }
        }
        step!(Pods, tickets.pods, reader.pods(c, ns, &cancel));
        step!(
            Deployments,
            tickets.deployments,
            reader.deployments(c, ns, &cancel)
        );
        step!(
            ReplicaSets,
            tickets.replica_sets,
            reader.replica_sets(c, ns, &cancel)
        );
        step!(Services, tickets.services, reader.services(c, ns, &cancel));
        step!(Slices, tickets.slices, reader.slices(c, ns, &cancel));
        step!(Events, tickets.events, reader.events(c, ns, &cancel));
        step!(Hpas, tickets.hpas, reader.hpas(c, ns, &cancel));
        let _ = sender.send_blocking(Loaded::Done);
    });
    receiver
}

/// kubeconfig context names only (`kubectl config view`, redacted). No
/// cluster request and no credential plugin.
pub(crate) fn contexts(
    reader: Arc<dyn Reader>,
) -> async_channel::Receiver<Result<Contexts, Failure>> {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = sender.send_blocking(reader.contexts(&Cancel::new()));
    });
    receiver
}

/// One fresh read of the Pod by name, to compare its UID with the pinned one
/// immediately before `kubectl logs` (which addresses the Pod by name only).
pub(crate) fn verify(
    reader: Arc<dyn Reader>,
    connection: Connection,
    namespace: String,
    pod: String,
    cancel: Cancel,
) -> async_channel::Receiver<Result<Observation<Pod>, Failure>> {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = sender.send_blocking(reader.pod(&connection, &namespace, &pod, &cancel));
    });
    receiver
}
