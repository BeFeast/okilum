//! Context and namespace resolution. Reads `kubectl config view -o json`
//! (kubectl's redacted form, never `--raw`) and keeps only context, cluster,
//! server and namespace names: no users, tokens, certificates or exec settings.
use super::classify::Failure;
use super::collect::{List, Observation};
use super::identity::Connection;
use super::model::Namespace;
use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeContext {
    pub name: String,
    pub cluster: Option<String>,
    pub server: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contexts {
    /// kubeconfig `current-context`, read only; nothing here changes it.
    pub current: Option<String>,
    pub contexts: Vec<KubeContext>,
}

fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiConfig {
    #[serde(rename = "current-context")]
    current_context: Option<String>,
    #[serde(deserialize_with = "nullable")]
    contexts: Vec<ApiNamedContext>,
    #[serde(deserialize_with = "nullable")]
    clusters: Vec<ApiNamedCluster>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiNamedContext {
    name: String,
    context: ApiContext,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiContext {
    cluster: Option<String>,
    namespace: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiNamedCluster {
    name: String,
    cluster: ApiCluster,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiCluster {
    server: Option<String>,
}

pub(crate) const CONFIG_VIEW_ARGS: [&str; 4] = ["config", "view", "--output=json", "--raw=false"];

pub(crate) fn parse_contexts(bytes: &[u8]) -> Result<Contexts, Failure> {
    let api: ApiConfig = serde_json::from_slice(bytes).map_err(|e| Failure::Malformed {
        detail: format!("kubeconfig view: {e}"),
    })?;
    let contexts = api
        .contexts
        .into_iter()
        .filter(|c| !c.name.is_empty())
        .map(|c| {
            let server = c.context.cluster.as_ref().and_then(|name| {
                api.clusters
                    .iter()
                    .find(|cl| &cl.name == name)
                    .and_then(|cl| cl.cluster.server.clone())
            });
            KubeContext {
                name: c.name,
                cluster: c.context.cluster,
                server,
                namespace: c.context.namespace.filter(|n| !n.is_empty()),
            }
        })
        .collect();
    Ok(Contexts {
        current: api.current_context.filter(|c| !c.is_empty()),
        contexts,
    })
}

impl Contexts {
    /// The requested context, or kubeconfig's current one captured now and
    /// pinned by name from then on.
    pub fn resolve(&self, requested: Option<&str>) -> Result<Connection, Failure> {
        let name = match requested.map(str::trim).filter(|r| !r.is_empty()) {
            Some(name) => name,
            None => self
                .current
                .as_deref()
                .ok_or_else(|| Failure::Configuration {
                    detail: "no current-context is set; choose a context".into(),
                })?,
        };
        validate_context(name)?;
        let context = self
            .contexts
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| Failure::Configuration {
                detail: format!("context {name:?} is not in the kubeconfig"),
            })?;
        Ok(Connection {
            context: context.name.clone(),
            cluster: context.cluster.clone(),
            server: context.server.clone(),
            default_namespace: context.namespace.clone(),
        })
    }
}

pub(crate) fn validate_context(name: &str) -> Result<(), Failure> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(Failure::invalid("Invalid Kubernetes context name"));
    }
    Ok(())
}

/// RFC 1123 label, as Kubernetes requires for namespaces.
pub fn valid_namespace(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

/// RFC 1123 subdomain, the shape of Pod, Deployment, Event... names.
pub fn valid_object_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamespaceOrigin {
    /// Typed by the user; no namespace listing was attempted.
    TypedUnchecked,
    /// Typed by the user and present in a complete namespace listing.
    TypedListed,
    /// Typed by the user; namespace listing was denied or failed, so it is
    /// used as typed and namespaced reads decide whether access exists.
    TypedListingDenied(Failure),
    /// Typed by the user; a complete listing succeeded and does not contain it.
    TypedNotListed,
    /// Typed by the user; the listing was partial (limit reached).
    TypedListingPartial,
    /// The context's namespace from kubeconfig.
    Context,
    /// No namespace typed or configured: kubectl's own default.
    KubectlDefault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceChoice {
    pub namespace: String,
    pub origin: NamespaceOrigin,
}

/// Decides the namespace for namespaced reads. A denied namespace listing does
/// not block a typed namespace; it is recorded so the UI can say so.
pub fn choose_namespace(
    connection: &Connection,
    typed: Option<&str>,
    listing: Option<&Result<Observation<List<Namespace>>, Failure>>,
) -> Result<NamespaceChoice, Failure> {
    let typed = typed.map(str::trim).filter(|t| !t.is_empty());
    let Some(typed) = typed else {
        return Ok(match &connection.default_namespace {
            Some(ns) if valid_namespace(ns) => NamespaceChoice {
                namespace: ns.clone(),
                origin: NamespaceOrigin::Context,
            },
            _ => NamespaceChoice {
                namespace: "default".into(),
                origin: NamespaceOrigin::KubectlDefault,
            },
        });
    };
    if !valid_namespace(typed) {
        return Err(Failure::invalid(
            "A namespace is a lowercase DNS label (a-z, 0-9, '-', at most 63)",
        ));
    }
    let origin = match listing {
        None => NamespaceOrigin::TypedUnchecked,
        Some(Err(failure)) => NamespaceOrigin::TypedListingDenied(failure.clone()),
        Some(Ok(observation)) => {
            let list = &observation.value;
            if list.items.iter().any(|n| n.meta.name == typed) {
                NamespaceOrigin::TypedListed
            } else if list.is_complete() {
                NamespaceOrigin::TypedNotListed
            } else {
                NamespaceOrigin::TypedListingPartial
            }
        }
    };
    Ok(NamespaceChoice {
        namespace: typed.to_string(),
        origin,
    })
}
