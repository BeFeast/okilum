//! Bounded structured reads. Each read is one `kubectl get --raw=<path>` with a
//! fixed API path built from a `Kind`, validated names and a server-side
//! `limit`; there is no general command runner and nothing is ever written.
use super::classify::{self, Failure};
use super::discovery::{self, valid_namespace, valid_object_name, Contexts};
use super::identity::{Connection, Kind, ResourceId, Source};
use super::model::{self, Meta, ResourceVersion};
use super::transport::{Cancel, Kubectl, Limits, Transport};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

pub const DEFAULT_LIST_LIMIT: u32 = 500;
pub const MAX_LIST_LIMIT: u32 = 1000;
/// Individually undecodable items are reported, up to this many reasons.
pub const MAX_REJECTED: usize = 20;

pub trait Resource: Sized {
    const KIND: Kind;
    fn decode(value: Value) -> Result<Self, String>;
    fn meta(&self) -> &Meta;
}

macro_rules! resource {
    ($type:ty, $kind:ident, $decode:path) => {
        impl Resource for $type {
            const KIND: Kind = Kind::$kind;
            fn decode(value: Value) -> Result<Self, String> {
                $decode(value)
            }
            fn meta(&self) -> &Meta {
                &self.meta
            }
        }
    };
}
resource!(model::Namespace, Namespace, model::namespace);
resource!(model::Pod, Pod, model::pod);
resource!(model::Deployment, Deployment, model::deployment);
resource!(model::ReplicaSet, ReplicaSet, model::replica_set);
resource!(model::Service, Service, model::service);
resource!(model::EndpointSlice, EndpointSlice, model::endpoint_slice);
resource!(model::Event, Event, model::event);
resource!(
    model::HorizontalPodAutoscaler,
    HorizontalPodAutoscaler,
    model::hpa
);

/// A value plus where and when it was read. `observed_at` is taken when the
/// response arrived; it is this source's freshness, not the cluster's clock.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation<T> {
    pub source: Source,
    pub observed_at: SystemTime,
    pub value: T,
}

impl<T: Resource> Observation<T> {
    pub fn id(&self) -> ResourceId {
        ResourceId::of(&self.source, self.value.meta())
    }
}

impl<T: Resource> Observation<List<T>> {
    pub fn item_id(&self, item: &T) -> ResourceId {
        ResourceId::of(&self.source, item.meta())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    /// The server had more than `limit` items; only the first page is held.
    Truncated {
        remaining: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejected {
    pub index: usize,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct List<T> {
    pub items: Vec<T>,
    /// The list's opaque resourceVersion.
    pub resource_version: Option<ResourceVersion>,
    pub completeness: Completeness,
    /// Items that were present but could not be decoded (first `MAX_REJECTED`).
    pub rejected: Vec<Rejected>,
    pub rejected_total: usize,
}

impl<T> List<T> {
    /// A successful read that found nothing, as opposed to a failed read.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.rejected_total == 0
    }
    pub fn is_complete(&self) -> bool {
        self.completeness == Completeness::Complete && self.rejected_total == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListQuery {
    /// Server-side page size, 1..=MAX_LIST_LIMIT. Only one page is read.
    pub limit: u32,
    /// Equality label selector (`matchLabels`), e.g. a Deployment's selector.
    pub labels: BTreeMap<String, String>,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_LIST_LIMIT,
            labels: BTreeMap::new(),
        }
    }
}

fn valid_label_part(s: &str, max: usize) -> bool {
    s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && s.bytes().next().is_none_or(|b| b.is_ascii_alphanumeric())
        && s.bytes().last().is_none_or(|b| b.is_ascii_alphanumeric())
}

fn label_selector(labels: &BTreeMap<String, String>) -> Result<Option<String>, Failure> {
    if labels.is_empty() {
        return Ok(None);
    }
    let mut parts = Vec::new();
    for (key, value) in labels {
        let (prefix, name) = match key.rsplit_once('/') {
            Some((prefix, name)) => (Some(prefix), name),
            None => (None, key.as_str()),
        };
        let ok = !name.is_empty()
            && valid_label_part(name, 63)
            && prefix.is_none_or(|p| !p.is_empty() && valid_object_name(p))
            && valid_label_part(value, 63);
        if !ok {
            return Err(Failure::invalid(format!(
                "Invalid label selector {key}={value}"
            )));
        }
        parts.push(format!("{key}={value}"));
    }
    Ok(Some(parts.join(",")))
}

/// The fixed API path for a kind. Inputs are validated before this is called.
pub(crate) fn api_path(
    kind: Kind,
    namespace: Option<&str>,
    name: Option<&str>,
    params: &[(&str, String)],
) -> String {
    let mut path = kind.api_prefix().to_string();
    if let Some(namespace) = namespace {
        path.push_str("/namespaces/");
        path.push_str(namespace);
    }
    path.push('/');
    path.push_str(kind.plural());
    if let Some(name) = name {
        path.push('/');
        path.push_str(name);
    }
    if !params.is_empty() {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in params {
            query.append_pair(key, value);
        }
        path.push('?');
        path.push_str(&query.finish());
    }
    path
}

#[derive(Deserialize)]
struct ApiList {
    kind: Option<String>,
    #[serde(default)]
    metadata: ApiListMeta,
    #[serde(default)]
    items: Option<Vec<Value>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ApiListMeta {
    resource_version: Option<String>,
    #[serde(rename = "continue")]
    continue_token: Option<String>,
    remaining_item_count: Option<u64>,
}

pub(crate) fn decode_list<R: Resource>(bytes: &[u8], limit: u32) -> Result<List<R>, Failure> {
    let api: ApiList = serde_json::from_slice(bytes).map_err(|e| Failure::Malformed {
        detail: format!("{} list: {e}", R::KIND.name()),
    })?;
    let expected = format!("{}List", R::KIND.name());
    if api.kind.as_deref() != Some(expected.as_str()) {
        return Err(Failure::Malformed {
            detail: format!(
                "expected {expected}, got {:?}",
                api.kind.unwrap_or_default()
            ),
        });
    }
    let mut values = api.items.unwrap_or_default();
    let mut truncated = api
        .metadata
        .continue_token
        .as_deref()
        .is_some_and(|c| !c.is_empty());
    // A server that ignores `limit` is still bounded here, and visibly so.
    if values.len() > limit as usize {
        values.truncate(limit as usize);
        truncated = true;
    }
    let (mut items, mut rejected, mut rejected_total) = (Vec::new(), Vec::new(), 0);
    for (index, value) in values.into_iter().enumerate() {
        match R::decode(value) {
            Ok(item) => items.push(item),
            Err(reason) => {
                rejected_total += 1;
                if rejected.len() < MAX_REJECTED {
                    rejected.push(Rejected { index, reason });
                }
            }
        }
    }
    Ok(List {
        items,
        resource_version: api
            .metadata
            .resource_version
            .filter(|v| !v.is_empty())
            .map(ResourceVersion),
        completeness: if truncated {
            Completeness::Truncated {
                remaining: api.metadata.remaining_item_count,
            }
        } else {
            Completeness::Complete
        },
        rejected,
        rejected_total,
    })
}

pub(crate) fn decode_object<R: Resource>(bytes: &[u8]) -> Result<R, Failure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| Failure::Malformed {
        detail: format!("{}: {e}", R::KIND.name()),
    })?;
    let kind = value["kind"].as_str().unwrap_or_default();
    if kind != R::KIND.name() {
        return Err(Failure::Malformed {
            detail: format!("expected {}, got {kind:?}", R::KIND.name()),
        });
    }
    R::decode(value).map_err(|detail| Failure::Malformed { detail })
}

/// The typed read adapter. Every call is independent, bounded and cancellable;
/// nothing runs until a caller asks for a specific read.
pub struct Client<T: Transport = Kubectl> {
    transport: T,
    limits: Limits,
}

impl Default for Client<Kubectl> {
    fn default() -> Self {
        Self::new(Kubectl::default(), Limits::default())
    }
}

impl<T: Transport> Client<T> {
    pub fn new(transport: T, limits: Limits) -> Self {
        Self { transport, limits }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    fn fetch(&self, args: Vec<String>, cancel: &Cancel) -> Result<(Vec<u8>, SystemTime), Failure> {
        if cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        let raw = self
            .transport
            .run(&args, self.limits, cancel)
            .map_err(|e| classify::spawn_failure(&e))?;
        let at = SystemTime::now();
        classify::outcome(raw, &self.limits).map(|bytes| (bytes, at))
    }

    fn api_args(&self, connection: &Connection, path: String) -> Result<Vec<String>, Failure> {
        discovery::validate_context(&connection.context)?;
        Ok(vec![
            format!("--context={}", connection.context),
            format!(
                "--request-timeout={}s",
                self.limits.request_timeout.as_secs().max(1)
            ),
            "get".into(),
            format!("--raw={path}"),
        ])
    }

    fn source(connection: &Connection, namespace: Option<&str>, kind: Kind) -> Source {
        Source {
            context: connection.context.clone(),
            cluster: connection.cluster.clone(),
            server: connection.server.clone(),
            namespace: namespace.map(str::to_string),
            kind,
        }
    }

    /// Contexts from the merged kubeconfig, redacted by kubectl. Read only.
    pub fn contexts(&self, cancel: &Cancel) -> Result<Contexts, Failure> {
        let args = discovery::CONFIG_VIEW_ARGS.map(String::from).to_vec();
        let (bytes, _) = self.fetch(args, cancel)?;
        discovery::parse_contexts(&bytes)
    }

    /// Namespace listing. `Forbidden` here is common and does not prevent
    /// namespaced reads of a typed namespace (see `choose_namespace`).
    pub fn namespaces(
        &self,
        connection: &Connection,
        query: &ListQuery,
        cancel: &Cancel,
    ) -> Result<Observation<List<model::Namespace>>, Failure> {
        self.list_at(connection, None, query, &[], cancel)
    }

    /// One bounded page of a namespaced kind.
    pub fn list<R: Resource>(
        &self,
        connection: &Connection,
        namespace: &str,
        query: &ListQuery,
        cancel: &Cancel,
    ) -> Result<Observation<List<R>>, Failure> {
        if !R::KIND.namespaced() {
            return Err(Failure::invalid(
                "Use namespaces() for cluster-scoped kinds",
            ));
        }
        self.list_at(connection, Some(namespace), query, &[], cancel)
    }

    /// Events whose `involvedObject.uid` is this object's UID, so events of an
    /// older object with the same name are not mixed in.
    pub fn related_events(
        &self,
        connection: &Connection,
        id: &ResourceId,
        query: &ListQuery,
        cancel: &Cancel,
    ) -> Result<Observation<List<model::Event>>, Failure> {
        if id.context != connection.context {
            return Err(Failure::invalid(
                "The object belongs to a different context than this connection",
            ));
        }
        if id.uid.is_empty()
            || id.uid.len() > 64
            || !id
                .uid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Failure::invalid("Invalid object UID"));
        }
        let namespace = id
            .namespace
            .as_deref()
            .ok_or_else(|| Failure::invalid("Related events need a namespaced object"))?;
        let selector = format!("involvedObject.uid={}", id.uid);
        self.list_at(
            connection,
            Some(namespace),
            query,
            &[("fieldSelector", selector)],
            cancel,
        )
    }

    fn list_at<R: Resource>(
        &self,
        connection: &Connection,
        namespace: Option<&str>,
        query: &ListQuery,
        extra: &[(&str, String)],
        cancel: &Cancel,
    ) -> Result<Observation<List<R>>, Failure> {
        if !(1..=MAX_LIST_LIMIT).contains(&query.limit) {
            return Err(Failure::invalid(format!(
                "List limit must be 1..={MAX_LIST_LIMIT}"
            )));
        }
        if let Some(namespace) = namespace {
            if !valid_namespace(namespace) {
                return Err(Failure::invalid("Invalid Kubernetes namespace"));
            }
        }
        let mut params = vec![("limit", query.limit.to_string())];
        if let Some(selector) = label_selector(&query.labels)? {
            params.push(("labelSelector", selector));
        }
        params.extend(extra.iter().cloned());
        let path = api_path(R::KIND, namespace, None, &params);
        let (bytes, observed_at) = self.fetch(self.api_args(connection, path)?, cancel)?;
        Ok(Observation {
            source: Self::source(connection, namespace, R::KIND),
            observed_at,
            value: decode_list(&bytes, query.limit)?,
        })
    }

    /// One object by name. `NotFound` is a failure kind, not an empty result.
    pub fn get<R: Resource>(
        &self,
        connection: &Connection,
        namespace: &str,
        name: &str,
        cancel: &Cancel,
    ) -> Result<Observation<R>, Failure> {
        let namespace = R::KIND.namespaced().then_some(namespace);
        if let Some(namespace) = namespace {
            if !valid_namespace(namespace) {
                return Err(Failure::invalid("Invalid Kubernetes namespace"));
            }
        }
        if !valid_object_name(name) {
            return Err(Failure::invalid(format!("Invalid {} name", R::KIND.name())));
        }
        let path = api_path(R::KIND, namespace, Some(name), &[]);
        let (bytes, observed_at) = self.fetch(self.api_args(connection, path)?, cancel)?;
        Ok(Observation {
            source: Self::source(connection, namespace, R::KIND),
            observed_at,
            value: decode_object(&bytes)?,
        })
    }
}

/// Builds the existing log connector's request for one container of an
/// observed Pod. It only builds: selecting a row never starts kubectl.
pub fn log_request(
    observation: &Observation<model::Pod>,
    container: &str,
    previous: bool,
    follow: bool,
    tail_lines: u32,
) -> Result<crate::log::kubernetes::Request, Failure> {
    let pod = &observation.value;
    let found = pod.container(container).ok_or_else(|| Failure::NotFound {
        detail: format!("container {container:?} is not in Pod {:?}", pod.meta.name),
    })?;
    if previous && !found.previous_instance_observed() {
        return Err(Failure::NotFound {
            detail: format!("no previous instance of container {container:?} was observed"),
        });
    }
    let namespace = pod
        .meta
        .namespace
        .clone()
        .or_else(|| observation.source.namespace.clone())
        .ok_or_else(|| Failure::invalid("Pod namespace unknown"))?;
    let request = crate::log::kubernetes::Request {
        context: Some(observation.source.context.clone()),
        namespace,
        pod: pod.meta.name.clone(),
        container: container.to_string(),
        previous,
        follow,
        tail_lines,
    };
    request
        .args()
        .map_err(|e| Failure::invalid(e.to_string()))?;
    Ok(request)
}

// ---------- Retained observations ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staleness<'a> {
    /// The latest read of this source failed; the value shown is older.
    Failed {
        failure: &'a Failure,
        at: SystemTime,
    },
    /// No failure, but the value is older than the caller's freshness bound.
    Aged { age: Duration },
}

#[derive(Debug, PartialEq)]
pub enum View<'a, T> {
    /// Never read (or only cancelled reads).
    Unread,
    Fresh(&'a Observation<T>),
    Stale {
        last: &'a Observation<T>,
        why: Staleness<'a>,
    },
    /// Failed, with nothing earlier to show.
    Failed {
        failure: &'a Failure,
        at: SystemTime,
    },
}

/// The retained state of one `Source`. A failure keeps the previous value as
/// stale instead of erasing it; a cancelled read changes nothing; a value from
/// another source, or older than the one held, is refused.
#[derive(Clone, Debug)]
pub struct Slot<T> {
    source: Source,
    latest: Option<Observation<T>>,
    failure: Option<(Failure, SystemTime)>,
}

impl<T> Slot<T> {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            latest: None,
            failure: None,
        }
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// Returns whether the result changed this slot.
    pub fn record(&mut self, result: Result<Observation<T>, Failure>, at: SystemTime) -> bool {
        match result {
            Ok(observation) => {
                if observation.source != self.source
                    || self
                        .latest
                        .as_ref()
                        .is_some_and(|held| observation.observed_at < held.observed_at)
                {
                    return false;
                }
                self.latest = Some(observation);
                self.failure = None;
                true
            }
            Err(Failure::Cancelled) => false,
            Err(failure) => {
                self.failure = Some((failure, at));
                true
            }
        }
    }

    pub fn view(&self, now: SystemTime, max_age: Duration) -> View<'_, T> {
        match (&self.latest, &self.failure) {
            (None, None) => View::Unread,
            (None, Some((failure, at))) => View::Failed { failure, at: *at },
            (Some(last), Some((failure, at))) => View::Stale {
                last,
                why: Staleness::Failed { failure, at: *at },
            },
            (Some(last), None) => {
                let age = now.duration_since(last.observed_at).unwrap_or_default();
                if age > max_age {
                    View::Stale {
                        last,
                        why: Staleness::Aged { age },
                    }
                } else {
                    View::Fresh(last)
                }
            }
        }
    }
}
