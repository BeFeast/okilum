//! Where a fact came from and which object it is about. Names are not
//! identities: the same namespace/name exists in many contexts, and a deleted
//! and recreated object keeps its name but gets a new UID.
use super::model::Meta;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Namespace,
    Pod,
    Deployment,
    ReplicaSet,
    Service,
    EndpointSlice,
    Event,
    HorizontalPodAutoscaler,
}

impl Kind {
    /// API group/version path prefix. Fixed per kind; never user input.
    pub(crate) fn api_prefix(self) -> &'static str {
        match self {
            Self::Namespace | Self::Pod | Self::Service | Self::Event => "/api/v1",
            Self::Deployment | Self::ReplicaSet => "/apis/apps/v1",
            Self::EndpointSlice => "/apis/discovery.k8s.io/v1",
            Self::HorizontalPodAutoscaler => "/apis/autoscaling/v2",
        }
    }
    pub(crate) fn plural(self) -> &'static str {
        match self {
            Self::Namespace => "namespaces",
            Self::Pod => "pods",
            Self::Deployment => "deployments",
            Self::ReplicaSet => "replicasets",
            Self::Service => "services",
            Self::EndpointSlice => "endpointslices",
            Self::Event => "events",
            Self::HorizontalPodAutoscaler => "horizontalpodautoscalers",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Namespace => "Namespace",
            Self::Pod => "Pod",
            Self::Deployment => "Deployment",
            Self::ReplicaSet => "ReplicaSet",
            Self::Service => "Service",
            Self::EndpointSlice => "EndpointSlice",
            Self::Event => "Event",
            Self::HorizontalPodAutoscaler => "HorizontalPodAutoscaler",
        }
    }
    pub fn namespaced(self) -> bool {
        self != Self::Namespace
    }
}

/// The resolved kubeconfig context a read runs against. The context name is
/// always passed explicitly, so a later `kubectl config use-context` elsewhere
/// cannot move an open investigation to another cluster.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Connection {
    pub context: String,
    pub cluster: Option<String>,
    pub server: Option<String>,
    /// The context's own default namespace, if it names one.
    pub default_namespace: Option<String>,
}

/// One collection source: context + namespace + kind. Freshness, failures and
/// cancellation are tracked per source, so a forbidden Events read never makes
/// the Pods of the same namespace look stale.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Source {
    pub context: String,
    pub cluster: Option<String>,
    pub server: Option<String>,
    pub namespace: Option<String>,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId {
    pub context: String,
    pub namespace: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sameness {
    Same,
    /// Same context, namespace, kind and name, different UID: deleted and created again.
    Recreated,
    Different,
}

impl ResourceId {
    pub fn of(source: &Source, meta: &Meta) -> Self {
        Self {
            context: source.context.clone(),
            namespace: meta.namespace.clone().or_else(|| source.namespace.clone()),
            kind: source.kind,
            name: meta.name.clone(),
            uid: meta.uid.clone(),
        }
    }
    pub fn compare(&self, other: &Self) -> Sameness {
        let same_name = self.context == other.context
            && self.namespace == other.namespace
            && self.kind == other.kind
            && self.name == other.name;
        match (same_name, self.uid == other.uid) {
            (true, true) => Sameness::Same,
            (true, false) => Sameness::Recreated,
            _ => Sameness::Different,
        }
    }
}
