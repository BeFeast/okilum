//! Opt-in Syncthing compatibility boundary. No application lifecycle integration.
use anyhow::{bail, ensure, Context, Result};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue},
    Method,
};
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

/// This policy is a compatibility fixture, not the live vault's canonical policy.
/// Production enrollment must receive and compare a versioned approved policy.
pub const FIXTURE_IGNORES: &[&str] = &["/.tessera-index", "/.claude/worktrees", "/worktrees"];

pub struct Syncthing {
    client: Client,
    origin: String,
}

impl Syncthing {
    /// Literal loopback address only; never inherit proxy settings or follow redirects.
    pub fn connect(address: SocketAddr, key: &str) -> Result<Self> {
        ensure!(address.ip().is_loopback(), "REST must use loopback");
        ensure!(!key.is_empty(), "missing REST key");
        let mut headers = HeaderMap::new();
        let mut value = HeaderValue::from_str(key)?;
        value.set_sensitive(true);
        headers.insert("X-API-Key", value);
        Ok(Self {
            client: Client::builder()
                .default_headers(headers)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()?,
            origin: format!("http://{address}"),
        })
    }

    fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin));
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().context("Syncthing REST unavailable")?;
        ensure!(
            response.status().is_success(),
            "Syncthing REST returned {}",
            response.status()
        );
        let bytes = response.bytes()?;
        if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(serde_json::from_slice(&bytes)?)
        }
    }

    pub fn identity(&self) -> Result<Value> {
        self.request(Method::GET, "/rest/system/status", None)
    }
    pub fn version(&self) -> Result<Value> {
        self.request(Method::GET, "/rest/system/version", None)
    }
    pub fn verify_identity(&self, expected: &str) -> Result<()> {
        ensure!(
            self.identity()?["myID"].as_str() == Some(expected),
            "unexpected daemon identity"
        );
        Ok(())
    }
    pub fn config(&self) -> Result<Value> {
        self.request(Method::GET, "/rest/config", None)
    }
    pub fn folder(&self, id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/rest/config/folders/{}", segment(id)?),
            None,
        )
    }
    pub fn device(&self, id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/rest/config/devices/{}", segment(id)?),
            None,
        )
    }
    /// Scoped PATCH preserves fields not owned by this operation; never PUT /rest/config.
    pub fn patch_folder(&self, id: &str, changes: &Value) -> Result<Value> {
        self.request(
            Method::PATCH,
            &format!("/rest/config/folders/{}", segment(id)?),
            Some(changes),
        )?;
        let actual = self.folder(id)?;
        verify_fields(&actual, changes)?;
        Ok(actual)
    }
    pub fn add_device(&self, device: &Value) -> Result<()> {
        let id = segment(device["deviceID"].as_str().context("missing device ID")?)?;
        let config = self.config()?;
        if let Some(existing) = config["devices"]
            .as_array()
            .context("invalid devices config")?
            .iter()
            .find(|entry| entry["deviceID"] == id)
        {
            let fields = device.as_object().context("invalid device object")?;
            ensure!(
                fields
                    .iter()
                    .all(|(key, value)| existing.get(key) == Some(value)),
                "existing device differs; explicit reuse reconciliation required"
            );
            return Ok(());
        }
        self.request(Method::POST, "/rest/config/devices", Some(device))?;
        verify_fields(&self.device(id)?, device)?;
        Ok(())
    }
    /// New folder only. Existing replica enrollment must preserve its config.
    pub fn add_paused_folder(&self, folder: &Value) -> Result<()> {
        ensure!(folder["paused"] == true, "new folder must be paused");
        let id = folder["id"].as_str().context("missing folder ID")?;
        segment(id)?;
        ensure!(
            !self.config()?["folders"]
                .as_array()
                .context("invalid config")?
                .iter()
                .any(|f| f["id"] == id),
            "folder already exists; use verified replica"
        );
        self.request(Method::POST, "/rest/config/folders", Some(folder))?;
        verify_fields(&self.folder(id)?, folder)?;
        Ok(())
    }
    pub fn set_ignores(&self, id: &str, lines: &[&str]) -> Result<()> {
        ensure!(
            self.folder(id)?["paused"] == true,
            "ignores require paused folder"
        );
        let existing = self.ignores(id)?;
        let current = existing
            .get("ignore")
            .context("ignore response field missing")?;
        ensure!(
            current.is_null()
                || current.as_array().is_some_and(Vec::is_empty)
                || *current == json!(lines),
            "existing ignores differ; explicit policy review required"
        );
        self.request(
            Method::POST,
            &format!("/rest/db/ignores?folder={}", segment(id)?),
            Some(&json!({"ignore":lines})),
        )?;
        let observed = self.ignores(id)?;
        let actual = observed
            .get("ignore")
            .context("ignore read-back field missing")?;
        // Both pinned versions can encode an empty ignore list as null.
        ensure!(
            *actual == json!(lines) || (lines.is_empty() && actual.is_null()),
            "ignore read-back mismatch"
        );
        Ok(())
    }
    pub fn ignores(&self, id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/rest/db/ignores?folder={}", segment(id)?),
            None,
        )
    }
    pub fn status(&self, id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/rest/db/status?folder={}", segment(id)?),
            None,
        )
    }
    pub fn errors(&self, id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/rest/folder/errors?folder={}", segment(id)?),
            None,
        )
    }
    pub fn scan(&self, id: &str) -> Result<()> {
        self.request(
            Method::POST,
            &format!("/rest/db/scan?folder={}", segment(id)?),
            None,
        )?;
        Ok(())
    }
}

// Syncthing expands nested device defaults; compare requested object fields recursively.
fn verify_fields(actual: &Value, requested: &Value) -> Result<()> {
    match requested {
        Value::Object(fields) => {
            for (key, value) in fields {
                verify_fields(&actual[key], value)
                    .with_context(|| format!("read-back field {key}"))?;
            }
        }
        Value::Array(items) => {
            let observed = actual.as_array().context("read-back array missing")?;
            ensure!(observed.len() == items.len(), "read-back length mismatch");
            let mut remaining: Vec<_> = observed.iter().collect();
            for requested in items {
                let position = remaining
                    .iter()
                    .position(|actual| verify_fields(actual, requested).is_ok())
                    .context("read-back array entry mismatch")?;
                remaining.remove(position);
            }
        }
        _ => ensure!(actual == requested, "REST read-back mismatch"),
    }
    Ok(())
}

fn segment(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty()
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "invalid REST identifier"
    );
    Ok(value)
}

/// Call with paths from every discovered daemon, not just the chosen endpoint.
/// Canonicalization rejects nonexistent paths and resolves symlink aliases.
/// This preflight is not a lock; the controller must serialize enrollment and revalidate.
pub fn validate_destination(
    path: &Path,
    folder_id: &str,
    known: &[(String, PathBuf)],
) -> Result<Destination> {
    let path = path.canonicalize()?;
    ensure!(path.is_dir(), "destination must be a directory");
    let mut replica = false;
    for (id, existing) in known {
        let existing = existing.canonicalize()?;
        if id == folder_id {
            ensure!(path == existing, "known replica path changed");
            replica = true;
        } else if path.starts_with(&existing) || existing.starts_with(&path) {
            bail!("destination overlaps another folder");
        }
    }
    if replica {
        return Ok(Destination::KnownReplica(path));
    }
    ensure!(
        path.read_dir()?.next().is_none(),
        "unknown nonempty destination"
    );
    Ok(Destination::Empty(path))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Destination {
    Empty(PathBuf),
    KnownReplica(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destinations_are_explicit() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("vault");
        std::fs::create_dir(&path)?;
        assert!(matches!(
            validate_destination(&path, "vault", &[])?,
            Destination::Empty(_)
        ));
        std::fs::write(path.join("note.md"), "local")?;
        assert!(validate_destination(&path, "vault", &[]).is_err());
        let known = vec![("vault".into(), path.clone())];
        assert!(matches!(
            validate_destination(&path, "vault", &known)?,
            Destination::KnownReplica(_)
        ));
        assert!(validate_destination(&path, "other", &known).is_err());
        assert!(validate_destination(root.path(), "other", &known).is_err());
        let child = path.join("child");
        std::fs::create_dir(&child)?;
        assert!(validate_destination(&child, "other", &known).is_err());
        assert!(validate_destination(&child, "vault", &known).is_err());
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn destination_resolves_symlink_aliases() -> Result<()> {
        let root = tempfile::tempdir()?;
        let vault = root.path().join("vault");
        std::fs::create_dir(&vault)?;
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&vault, &alias)?;
        let known = vec![("vault".into(), vault)];
        assert!(validate_destination(&alias, "other", &known).is_err());
        assert!(matches!(
            validate_destination(&alias, "vault", &known)?,
            Destination::KnownReplica(_)
        ));
        assert!(validate_destination(&root.path().join("missing"), "new", &[]).is_err());
        Ok(())
    }

    #[test]
    fn endpoint_and_identifiers_fail_closed() {
        assert!(Syncthing::connect("192.0.2.1:8384".parse().unwrap(), "key").is_err());
        assert!(segment("a&folder=b").is_err());
        assert!(segment("../x").is_err());
        assert!(segment("..").is_err());
        assert!(segment(".").is_err());
        assert!(segment("vault-1").is_ok());
    }
}
