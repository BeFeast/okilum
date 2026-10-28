//! Shared authenticated daemon inspection. Callers supply the complete explicit
//! inventory; this module never starts a daemon or claims to discover OS processes.
//! Linux process discovery and preparation are separate target-gated operations.
use crate::private;
use anyhow::{ensure, Context, Result};
use okilum_sync::Syncthing;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use xmltree::Element;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{configuration_paths, prepare, Preparation};

pub const CLIENT_VERSION: &str = "v2.1.6";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DaemonIdentity {
    pub config_file: PathBuf,
    pub device_id: String,
    pub certificate_hash: String,
}
impl DaemonIdentity {
    /// Re-read the credential locally and authenticate the same instance.
    pub fn connect(&self) -> Result<Syncthing> {
        let config = read_config(&self.config_file)?;
        ensure!(
            fingerprint(&self.config_file)? == self.certificate_hash,
            "daemon certificate changed"
        );
        let api = Syncthing::connect(config.address, &config.key)?;
        api.verify_identity(&self.device_id)?;
        ensure!(
            api.version()?["version"] == CLIENT_VERSION,
            "unsupported Syncthing version"
        );
        Ok(api)
    }
}
pub struct Candidate {
    pub identity: DaemonIdentity,
    pub folders: Vec<(String, PathBuf)>,
    pub version: String,
}
pub struct Discovery {
    pub candidates: Vec<Candidate>,
    /// An inaccessible candidate cannot silently be treated as no existing daemon.
    pub unavailable: Vec<PathBuf>,
}
impl Discovery {
    pub fn select(&self, config: &Path) -> Result<&Candidate> {
        ensure!(
            self.unavailable.is_empty(),
            "resolve unavailable Syncthing configurations before enrollment"
        );
        let config = config.canonicalize()?;
        self.candidates
            .iter()
            .find(|c| c.identity.config_file == config)
            .context("explicit daemon selection required")
    }
    pub fn folders(&self) -> Result<Vec<(String, PathBuf)>> {
        ensure!(
            self.unavailable.is_empty(),
            "daemon inventory is incomplete"
        );
        Ok(self
            .candidates
            .iter()
            .flat_map(|c| c.folders.clone())
            .collect())
    }
}
/// The caller supplies explicit paths in addition to standard/proc discovery.
/// No credential, process, home, directory or service is created here.
pub fn discover(configs: &[PathBuf]) -> Discovery {
    let mut paths = BTreeSet::new();
    let mut result = Discovery {
        candidates: vec![],
        unavailable: vec![],
    };
    for path in configs {
        let inspected = (|| -> Result<Candidate> {
            let path = path.canonicalize()?;
            ensure!(paths.insert(path.clone()), "duplicate candidate");
            let config = read_config(&path)?;
            let api = Syncthing::connect(config.address, &config.key)?;
            let version = api.version()?["version"]
                .as_str()
                .context("missing version")?
                .to_owned();
            ensure!(
                [CLIENT_VERSION, "v1.29.5"].contains(&version.as_str()),
                "unsupported inventory version"
            );
            let id = api.identity()?["myID"]
                .as_str()
                .context("missing device identity")?
                .to_owned();
            let all = api.config()?;
            let folders = all["folders"]
                .as_array()
                .context("invalid daemon inventory")?
                .iter()
                .map(|f| -> Result<_> {
                    Ok((
                        f["id"].as_str().context("missing folder ID")?.to_owned(),
                        PathBuf::from(f["path"].as_str().context("missing folder path")?)
                            .canonicalize()?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Candidate {
                identity: DaemonIdentity {
                    device_id: id,
                    certificate_hash: fingerprint(&path)?,
                    config_file: path,
                },
                folders,
                version,
            })
        })();
        match inspected {
            Ok(candidate) => result.candidates.push(candidate),
            Err(_)
                if path.canonicalize().is_ok_and(|p| {
                    result
                        .candidates
                        .iter()
                        .any(|c| c.identity.config_file == p)
                }) => {}
            Err(_) => result.unavailable.push(path.clone()),
        }
    }
    result
}
struct RestConfig {
    address: SocketAddr,
    key: String,
}
fn read_config(path: &Path) -> Result<RestConfig> {
    let xml = Element::parse(private::read(path)?.as_slice())?;
    let gui = xml.get_child("gui").context("GUI configuration missing")?;
    ensure!(
        gui.attributes.get("tls").is_none_or(|s| s == "false"),
        "existing HTTPS REST requires explicit support"
    );
    let address: SocketAddr = gui
        .get_child("address")
        .and_then(Element::get_text)
        .context("REST address missing")?
        .parse()?;
    ensure!(
        address.ip().is_loopback() && address.port() != 0,
        "REST must be literal loopback"
    );
    let key = gui
        .get_child("apikey")
        .and_then(Element::get_text)
        .context("REST credential missing")?
        .into_owned();
    ensure!(!key.is_empty(), "REST credential missing");
    Ok(RestConfig { address, key })
}
fn fingerprint(config: &Path) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(private::public_file(&config.with_file_name("cert.pem"))?)
    ))
}
