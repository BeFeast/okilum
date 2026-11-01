//! Read-only association with an explicitly selected, authenticated existing replica.
use crate::{
    daemon::{self, DaemonIdentity},
    private,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExistingVault {
    pub daemon: DaemonIdentity,
    pub version: String,
    pub folder_id: String,
    pub path: PathBuf,
    pub label: String,
    pub paused: bool,
    pub folder_type: String,
    pub peers: Vec<String>,
}

/// No state is created, no daemon is started, and every REST operation is GET.
/// The inventory must include all known daemons, including custom configurations.
pub fn inspect_existing(configs: &[PathBuf], vault: &Path) -> Result<ExistingVault> {
    let path = vault.canonicalize()?;
    ensure!(path.is_dir(), "vault must be a directory");
    let inventory = daemon::discover(configs);
    let folders = inventory.folders()?;
    let matches: Vec<_> = inventory
        .candidates
        .iter()
        .filter(|candidate| candidate.folders.iter().any(|(_, p)| p == &path))
        .collect();
    ensure!(
        matches.len() == 1,
        "explicit vault must belong to exactly one accessible daemon"
    );
    let candidate = matches[0];
    let ids: Vec<_> = candidate
        .folders
        .iter()
        .filter(|(_, p)| p == &path)
        .collect();
    ensure!(ids.len() == 1, "ambiguous replica folder identity");
    let id = &ids[0].0;
    for (other_id, other_path) in &folders {
        ensure!(
            (other_id == id && other_path == &path)
                || (!other_path.starts_with(&path) && !path.starts_with(other_path)),
            "vault overlaps another Syncthing folder"
        );
    }
    let api = candidate.identity.connect()?;
    let folder = api.folder(id)?;
    ensure!(
        Path::new(folder["path"].as_str().context("folder path missing")?).canonicalize()? == path,
        "replica path changed during inspection"
    );
    ensure!(folder["id"] == *id, "replica folder identity changed");
    let peers = folder["devices"]
        .as_array()
        .context("folder devices missing")?
        .iter()
        .map(|d| {
            d["deviceID"]
                .as_str()
                .map(str::to_owned)
                .context("invalid peer identity")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ExistingVault {
        daemon: candidate.identity.clone(),
        version: candidate.version.clone(),
        folder_id: id.clone(),
        path,
        label: folder["label"].as_str().unwrap_or(id).to_owned(),
        paused: folder["paused"]
            .as_bool()
            .context("folder pause state missing")?,
        folder_type: folder["type"]
            .as_str()
            .context("folder mode missing")?
            .to_owned(),
        peers,
    })
}

pub(super) fn same_binding(a: &ExistingVault, b: &ExistingVault) -> bool {
    a.daemon == b.daemon && a.folder_id == b.folder_id && a.path == b.path
}

/// Association itself confers no permission to change daemon configuration.
/// Its durable record is outside the canonical folder and disposable index.
pub fn attach_existing(
    state: &Path,
    inspected: &ExistingVault,
    consent: bool,
) -> Result<AttachedVault> {
    ensure!(consent, "explicit attach-existing consent required");
    ensure!(
        !state.starts_with(&inspected.path),
        "operational state must be outside the vault"
    );
    let fresh = inspect_existing(
        std::slice::from_ref(&inspected.daemon.config_file),
        &inspected.path,
    )?;
    ensure!(
        same_binding(inspected, &fresh),
        "inspected replica identity changed"
    );
    let _lock = private::lock(state)?;
    let file = state.join("existing.json");
    if file.try_exists()? {
        let saved: ExistingVault = serde_json::from_slice(&private::read(&file)?)?;
        ensure!(
            same_binding(&saved, &fresh),
            "saved replica association differs"
        );
    }
    private::write(&file, &serde_json::to_vec(&fresh)?)?;
    Ok(AttachedVault {
        state: state.to_owned(),
    })
}

#[derive(Clone, Debug)]
pub struct AttachedVault {
    state: PathBuf,
}
impl AttachedVault {
    pub fn load(state: PathBuf) -> Result<Self> {
        let attached = Self { state };
        attached.inspect()?;
        Ok(attached)
    }
    pub fn inspect(&self) -> Result<ExistingVault> {
        let saved: ExistingVault =
            serde_json::from_slice(&private::read(&self.state.join("existing.json"))?)?;
        let fresh = inspect_existing(std::slice::from_ref(&saved.daemon.config_file), &saved.path)?;
        ensure!(
            same_binding(&saved, &fresh),
            "saved replica association changed"
        );
        Ok(fresh)
    }
}

/// Configuration-path inventory only. No network request, process launch or state.
/// Errors, including inaccessible standard paths, are not converted to absence.
pub fn configuration_paths() -> Result<Vec<PathBuf>> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?);
    #[cfg(target_os = "linux")]
    {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        daemon::configuration_paths(&home, &state, &config)
    }
    #[cfg(target_os = "macos")]
    {
        let mut paths = Vec::new();
        for path in [
            home.join("Library/Application Support/Syncthing/config.xml"),
            home.join(".config/syncthing/config.xml"),
            home.join(".local/state/syncthing/config.xml"),
        ] {
            if path.try_exists()? {
                paths.push(path);
            }
        }
        Ok(paths)
    }
}
