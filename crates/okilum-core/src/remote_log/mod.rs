//! Vault-scoped Kibana sources and bounded, session-only history (#1441).
//! Source definitions contain no credentials. Live completeness is not inferred
//! from event timestamps, bounded compatibility checks, or history pagination.
pub mod discover;
pub mod link;
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
pub const LIVE_LIMITATION: &str = "Live is not available: a strategy that includes late arrivals throughout the selected period has not been verified. History remains available.";
pub const KQL_LIMITATION: &str = "Server-side KQL; general parity with Discover has not been established. Unsupported queries leave the previous results intact.";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: uuid::Uuid,
    pub name: String,
    pub endpoint: String,
    pub discover: discover::Discover,
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
        })
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
            endpoint.scheme() == "https"
                && endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "Saved source endpoint must be HTTPS without credentials, query or fragment"
        );
        self.discover.validate()
    }
}

pub fn vault_scope(vault: &Path) -> String {
    let digest = Sha256::digest(vault.as_os_str().as_encoded_bytes());
    format!("{digest:x}")
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
            if !display.contains_key("message") {
                if let Some(description) = display.get("description").cloned() {
                    display.insert("message".into(), description);
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
        Ok(Self {
            events,
            bytes,
            index,
            path: std::path::PathBuf::from(format!("kibana-{}.jsonl", source.id)),
        })
    }
    pub fn export(&self, out: &mut dyn std::io::Write) -> Result<()> {
        for event in &self.events {
            serde_json::to_writer(&mut *out, event.as_ref())?;
            out.write_all(b"\n")?;
        }
        Ok(())
    }
}
