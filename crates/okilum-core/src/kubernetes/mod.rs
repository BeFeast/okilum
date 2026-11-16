//! Typed, read-only Kubernetes discovery for the investigation workspace (#3009).
//!
//! Contract: docs/kubernetes-read-adapter.md. Every read is one bounded
//! `kubectl` invocation with a fixed argv (`config view` or `get --raw=<path>`),
//! the explicit context and no shell. JSON is decoded into typed facts that keep
//! their source, observation time, UID and opaque resourceVersion. Nothing
//! writes kubeconfig or cluster resources, and nothing runs until a caller asks.
//! The log connector (`crate::log::kubernetes`) is unchanged and shares the
//! kubectl resolution in `transport::kubectl_command`.

mod classify;
mod collect;
mod discovery;
mod identity;
mod model;
mod transport;

pub use classify::Failure;
pub use collect::{
    log_request, Client, Completeness, List, ListQuery, Observation, Rejected, Resource, Slot,
    Staleness, View, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT, MAX_REJECTED,
};
pub use discovery::{
    choose_namespace, valid_namespace, valid_object_name, Contexts, KubeContext, NamespaceChoice,
    NamespaceOrigin,
};
pub use identity::{Connection, Kind, ResourceId, Sameness, Source};
pub use model::{
    Condition, Container, ContainerKind, Deployment, Endpoint, EndpointSlice, Event, Health,
    HorizontalPodAutoscaler, Meta, MetricFact, Namespace, ObjectRef, OwnerRef, Pod, Probe,
    ProbeHandler, Probes, ReplicaSet, ResourceVersion, Resources, Service, ServicePort, SlicePort,
    State,
};
pub use transport::{
    kubectl_command, Cancel, Cleanup, End, Kubectl, Limits, Raw, Transport, DEFAULT_MAX_STDOUT,
    KUBECTL, MAX_STDERR,
};

#[cfg(test)]
mod tests;
