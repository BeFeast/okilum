//! Vault-scoped Kibana sources and bounded, session-only history (#1441).
//! Source definitions contain no credentials. Live completeness is not inferred
//! from event timestamps, bounded compatibility checks, or history pagination.
pub mod chips;
pub mod discover;
#[cfg(any(test, debug_assertions, feature = "test-support"))]
#[doc(hidden)]
pub mod fake;
pub mod link;
pub mod live;
pub mod result;
mod rison;
#[cfg(test)]
mod tests;
pub mod transport;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

pub const PAGE_ROWS: usize = 200;
pub const MAX_ROWS: usize = 10_000;
pub const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;
pub const LIVE_LIMITATION: &str = "Live re-reads every page of the selected window, including late arrivals. Paging is not an immutable snapshot. Limits and partial results stop following explicitly; large windows have no five-second guarantee.";
pub const KQL_LIMITATION: &str = "Server-side KQL; general parity with Discover has not been established. Unsupported queries leave the previous results intact.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: uuid::Uuid,
    pub name: String,
    pub endpoint: String,
    pub discover: discover::Discover,
    /// Absent in legacy Discover-only sources; resolved on first Apply.
    #[serde(default)]
    pub index_pattern: Option<String>,
    #[serde(default = "default_time_field")]
    pub time_field: String,
    /// User-pinned scope fields. Keys are discovered, never dataset-specific.
    #[serde(default)]
    pub scope_fields: Vec<String>,
}
impl Source {
    pub fn import(name: &str, input: &str) -> Result<Self> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 128,
            "Enter a source name of at most 128 bytes"
        );
        let (endpoint, discover) = discover::import(input)?;
        Ok(Self {
            id: uuid::Uuid::new_v4(),
            name: name.trim().into(),
            endpoint,
            discover,
            index_pattern: None,
            time_field: default_time_field(),
            scope_fields: Vec::new(),
        })
    }
    pub fn direct(name: &str, endpoint: &str, pattern: &str, time_field: &str) -> Result<Self> {
        let time_field = if time_field.trim().is_empty() {
            default_time_field()
        } else {
            time_field.trim().into()
        };
        let source = Self {
            id: uuid::Uuid::new_v4(),
            name: name.trim().into(),
            endpoint: endpoint.trim().trim_end_matches('/').into(),
            index_pattern: Some(pattern.trim().into()),
            time_field: time_field.clone(),
            scope_fields: Vec::new(),
            discover: discover::Discover {
                data_view: pattern.trim().into(),
                query: discover::Query {
                    language: "kuery".into(),
                    query: String::new(),
                },
                filters: Vec::new(),
                columns: Vec::new(),
                sort: vec![discover::Sort {
                    field: time_field,
                    order: "asc".into(),
                }],
                time: discover::TimeRange {
                    from: "now-15m".into(),
                    to: "now".into(),
                },
                refresh_paused: true,
            },
        };
        source.validate()?;
        Ok(source)
    }
    /// The dataset context of a query, independent of filters and presentation.
    pub fn identity_scope(&self) -> Result<String> {
        Ok(serde_json::to_string(&(
            self.id,
            &self.endpoint,
            &self.discover.data_view,
            &self.index_pattern,
            &self.time_field,
        ))?)
    }
    /// Include the endpoint to prevent an edited source redirecting an existing key.
    pub fn credential_url(&self, vault: &Path) -> String {
        let scope = vault_scope(vault);
        let endpoint = Sha256::digest(self.endpoint.as_bytes());
        format!("okilum-kibana://{scope}/{}/{endpoint:x}", self.id)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 128,
            "Enter a source name of at most 128 bytes"
        );
        let endpoint = url::Url::parse(&self.endpoint).context("Invalid Kibana endpoint")?;
        ensure!(
            valid_endpoint_scheme(&endpoint)
                && endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "Saved source endpoint must be HTTPS without credentials, query or fragment"
        );
        ensure!(
            !self.time_field.is_empty() && self.time_field.len() <= 256,
            "Invalid time field"
        );
        if let Some(pattern) = &self.index_pattern {
            validate_pattern(pattern)?;
        }
        ensure!(
            self.scope_fields.len() <= 16
                && self
                    .scope_fields
                    .iter()
                    .all(|f| !f.is_empty() && f.len() <= 256)
                && self
                    .scope_fields
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == self.scope_fields.len(),
            "Pin at most 16 distinct scope fields of 1–256 bytes"
        );
        self.discover.validate()
    }
}

fn default_time_field() -> String {
    "@timestamp".into()
}
/// Production release builds require HTTPS. Test/debug HTTP requires literal loopback IPs.
pub(crate) fn valid_endpoint_scheme(url: &url::Url) -> bool {
    url.scheme() == "https"
        || (cfg!(any(test, debug_assertions, feature = "test-support"))
            && url.scheme() == "http"
            && url
                .host_str()
                .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback()))
}
pub(crate) fn validate_pattern(pattern: &str) -> Result<()> {
    ensure!(
        !pattern.is_empty()
            && pattern.len() <= 1024
            && !pattern.contains(['?', '#', '&', '/'])
            && !pattern.chars().any(char::is_whitespace),
        "Enter a valid index pattern"
    );
    Ok(())
}

pub fn vault_scope(vault: &Path) -> String {
    let digest = Sha256::digest(vault.as_os_str().as_encoded_bytes());
    format!("{digest:x}")
}

/// Machine-local vault identity carried by `okilum://logs` links. It derives
/// from the same canonical path that partitions saved sources, so resolving a
/// link never guesses a vault by name and never writes an identity file.
pub fn vault_id(vault: &Path) -> uuid::Uuid {
    let digest = Sha256::digest(vault.as_os_str().as_encoded_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// Stored under the existing durable application state directory, partitioned
/// by the canonical vault path. Event data and credentials never enter this file.
pub fn load_sources(state: &Path, vault: &Path) -> Result<Vec<Source>> {
    let path = state
        .join("remote-log-sources")
        .join(format!("{}.json", vault_scope(vault)));
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "Saved sources exceed 1 MiB");
    let sources: Vec<Source> =
        serde_json::from_slice(&bytes).context("Cannot decode saved Kibana sources")?;
    for source in &sources {
        source.validate()?;
    }
    Ok(sources)
}
pub fn save_source(state: &Path, vault: &Path, source: &Source) -> Result<()> {
    source.validate()?;
    let directory = state.join("remote-log-sources");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", vault_scope(vault)));
    // Read-modify-write is protected across vault windows and processes.
    let lock = directory.join(format!("{}.lock", vault_scope(vault)));
    let guard = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)?;
    guard.lock()?;
    let mut sources = load_sources(state, vault)?;
    if let Some(old) = sources.iter_mut().find(|old| old.id == source.id) {
        *old = source.clone();
    } else {
        ensure!(
            sources.len() < 32,
            "At most 32 sources per vault are supported"
        );
        sources.push(source.clone());
    }
    let bytes = serde_json::to_vec_pretty(&sources)?;
    ensure!(bytes.len() <= 1024 * 1024, "Saved sources exceed 1 MiB");
    let mut temp = tempfile::NamedTempFile::new_in(&directory)?;
    use std::io::Write;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(&path).map_err(|e| e.error)?;
    Ok(())
}

/// One immutable display snapshot. Original hits are shared across pages and
/// exported verbatim; display aliases never replace or truncate event fields.
pub struct EventWindow {
    pub events: Vec<Arc<serde_json::Value>>,
    pub bytes: Vec<u8>,
    pub index: crate::log::LogIndex,
    pub path: std::path::PathBuf,
    pub identity_scope: String,
    /// Observed source paths; counts describe loaded documents, not the whole index.
    pub fields: Vec<result::FieldInfo>,
    /// Exhaustive role identities, derived off the UI thread with the window.
    role_fields: Vec<(crate::log::Role, Option<String>)>,
}
impl EventWindow {
    pub fn new(events: Vec<Arc<serde_json::Value>>, source: &Source) -> Result<Self> {
        ensure!(events.len() <= MAX_ROWS, "Loaded event limit exceeded");
        let mut bytes = Vec::new();
        for event in &events {
            let mut display = serde_json::Map::new();
            if let Some(fields) = event["_source"].as_object() {
                display.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            if let Some(fields) = event["fields"].as_object() {
                for (key, value) in fields {
                    display.entry(key.clone()).or_insert_with(|| {
                        value
                            .as_array()
                            .filter(|a| a.len() == 1)
                            .map_or_else(|| value.clone(), |a| a[0].clone())
                    });
                }
            }
            display.insert("_event".into(), event.as_ref().clone());
            serde_json::to_writer(&mut bytes, &display)?;
            bytes.push(b'\n');
            ensure!(
                bytes.len() <= MAX_EVENT_BYTES * 4,
                "Loaded display snapshot exceeds 64 MiB; narrow the query"
            );
        }
        let index = crate::log::LogIndex::build(&bytes);
        let fields = result::describe_fields(&events);
        use crate::log::Role;
        let mut roles = [Role::Time, Role::Level, Role::Message, Role::Logger]
            .map(|role| (role, std::collections::BTreeSet::new()));
        for at in 0..index.len() {
            if let Some(record) = index.raw(&bytes, at).and_then(|raw| index.record(raw)) {
                for (role, keys) in &mut roles {
                    if let Some(field) = record.role(*role) {
                        keys.insert(field.key.clone());
                    }
                }
            }
        }
        let role_fields = roles
            .into_iter()
            .map(|(role, keys)| {
                let key = if keys.len() == 1 {
                    keys.into_iter().next()
                } else {
                    None
                };
                (role, key)
            })
            .collect();
        Ok(Self {
            fields,
            role_fields,
            events,
            bytes,
            index,
            path: std::path::PathBuf::from(format!("kibana-{}.jsonl", source.id)),
            identity_scope: source.identity_scope()?,
        })
    }
    /// None means absent or ambiguous, never a guessed first row's field.
    pub fn role_field(&self, role: crate::log::Role) -> Option<&str> {
        self.role_fields
            .iter()
            .find(|(kind, _)| *kind == role)?
            .1
            .as_deref()
    }
    pub fn export(&self, out: &mut dyn std::io::Write) -> Result<()> {
        for event in &self.events {
            serde_json::to_writer(&mut *out, event.as_ref())?;
            out.write_all(b"\n")?;
        }
        Ok(())
    }
}
