//! Read-only association with an explicitly selected, authenticated existing replica.
use crate::{
    daemon::{self, DaemonIdentity},
    private,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Hold the original directory open so replacement cannot recycle its inode while
/// authenticated daemon inspection is in flight. Canonical paths alone are not
/// filesystem identities.
pub(super) struct VaultRoot(std::fs::File);
impl VaultRoot {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let root = Self(
            std::fs::File::open(path)
                .with_context(|| format!("open vault root {}", path.display()))?,
        );
        root.verify(path)?;
        Ok(root)
    }

    pub(super) fn identity(&self) -> Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let metadata = self.0.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(super) fn verify(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let current = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspect vault root {}", path.display()))?;
        let original = self
            .0
            .metadata()
            .with_context(|| format!("inspect original vault root {}", path.display()))?;
        ensure!(
            current.is_dir()
                && original.is_dir()
                && current.dev() == original.dev()
                && current.ino() == original.ino(),
            "vault root must remain the same real directory"
        );
        Ok(())
    }
}

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
    /// Persisted inspection witness. Legacy records need explicit fresh consent.
    #[serde(default)]
    pub root_identity: Option<(u64, u64)>,
    /// Native schema-2 witness. The raw pair remains an in-process descriptor guard.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_root_identity: Option<okilum_write_authority::filesystem_identity::Identity>,
}

/// No state is created, no daemon is started, and every REST operation is GET.
/// The inventory must include all known daemons, including custom configurations.
pub fn inspect_existing(configs: &[PathBuf], vault: &Path) -> Result<ExistingVault> {
    let root = VaultRoot::open(vault)?;
    let path = vault.canonicalize()?;
    root.verify(&path)?;
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
    root.verify(vault)?;
    root.verify(&path)?;
    use std::os::unix::fs::MetadataExt;
    let metadata = root.0.metadata()?;
    Ok(ExistingVault {
        root_identity: Some((metadata.dev(), metadata.ino())),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        stable_root_identity: Some(okilum_write_authority::filesystem_identity::from_file(
            &root.0,
        )?),
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

pub(super) fn same_replica(a: &ExistingVault, b: &ExistingVault) -> bool {
    a.daemon == b.daemon && a.folder_id == b.folder_id && a.path == b.path
}
pub(super) fn same_binding(a: &ExistingVault, b: &ExistingVault) -> bool {
    if !same_replica(a, b) {
        return false;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let (Some(a), Some(b)) = (&a.stable_root_identity, &b.stable_root_identity) {
        return a == b;
    }
    // Old Sync records have no birthtime or filesystem UUID. Never infer
    // continuity from only an inode after a reboot; fresh explicit consent can
    // upgrade that incomplete witness below. Ordinary same-boot refresh can
    // still upgrade the existing (dev, ino) proof without new authority.
    a.root_identity.is_some() && a.root_identity == b.root_identity
}

pub(super) fn same_binding_with_consent(saved: &ExistingVault, fresh: &ExistingVault) -> bool {
    if same_binding(saved, fresh) {
        return true;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if saved.stable_root_identity.is_some() {
        return false;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if saved.stable_root_identity.is_none()
        && fresh.stable_root_identity.is_some()
        && same_replica(saved, fresh)
        && saved.root_identity.is_some_and(|(_, inode)| {
            fresh
                .root_identity
                .is_some_and(|(_, current)| inode == current)
        })
    {
        return true;
    }
    saved.root_identity.is_none() && same_replica(saved, fresh)
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
            same_binding_with_consent(&saved, &fresh),
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if saved.stable_root_identity.is_none() && fresh.stable_root_identity.is_some() {
            let _lock = private::lock(&self.state)?;
            let file = self.state.join("existing.json");
            let current: ExistingVault = serde_json::from_slice(&private::read(&file)?)?;
            ensure!(
                current == saved,
                "saved replica association changed during migration"
            );
            private::write(&file, &serde_json::to_vec(&fresh)?)?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[cfg(target_os = "linux")]
    fn linux_vault(path: &Path) -> Result<ExistingVault> {
        let root = VaultRoot::open(path)?;
        Ok(ExistingVault {
            daemon: DaemonIdentity {
                config_file: path.with_file_name("fixture.xml"),
                device_id: "fixture".into(),
                certificate_hash: "fixture".into(),
            },
            version: "v2.1.7".into(),
            folder_id: "fixture".into(),
            path: path.to_owned(),
            label: "fixture".into(),
            paused: false,
            folder_type: "sendreceive".into(),
            peers: vec![],
            root_identity: Some(root.identity()?),
            stable_root_identity: Some(okilum_write_authority::filesystem_identity::from_file(
                &root.0,
            )?),
        })
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sync_2710_linux_replica_survives_device_renumbering_and_refuses_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vault = temp.path().join("vault");
        fs::create_dir(&vault)?;
        let current = linux_vault(&vault)?;
        let mut stored = serde_json::to_value(&current)?;
        stored["root_identity"][0] = serde_json::json!(60);
        stored["stable_root_identity"][0] = serde_json::json!(60);
        let mut fresh = serde_json::to_value(&current)?;
        fresh["root_identity"][0] = serde_json::json!(64);
        fresh["stable_root_identity"][0] = serde_json::json!(64);
        let stored: ExistingVault = serde_json::from_value(stored)?;
        let fresh: ExistingVault = serde_json::from_value(fresh)?;
        assert_ne!(stored.root_identity, fresh.root_identity);
        assert!(same_binding(&stored, &fresh));
        assert!(same_binding_with_consent(&stored, &fresh));

        for index in [1, 2, 3, 4] {
            let mut replaced = serde_json::to_value(&fresh)?;
            replaced["stable_root_identity"][index] = match index {
                3 => serde_json::json!(vec![9; 16]),
                _ => serde_json::json!(999),
            };
            let replaced = serde_json::from_value(replaced)?;
            assert!(
                !same_binding(&stored, &replaced),
                "changed identity field {index}"
            );
            assert!(!same_binding_with_consent(&stored, &replaced));
        }
        fs::rename(&vault, temp.path().join("previous"))?;
        fs::create_dir(&vault)?;
        let replaced = linux_vault(&vault)?;
        assert!(same_replica(&current, &replaced));
        assert!(!same_binding(&current, &replaced));
        assert!(!same_binding_with_consent(&current, &replaced));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sync_2710_legacy_pair_upgrades_with_fresh_consent_after_renumbering() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vault = temp.path().join("vault");
        fs::create_dir(&vault)?;
        let fresh = linux_vault(&vault)?;
        let mut value = serde_json::to_value(&fresh)?;
        value
            .as_object_mut()
            .unwrap()
            .remove("stable_root_identity");
        let mut legacy: ExistingVault = serde_json::from_value(value)?;
        assert!(
            same_binding(&legacy, &fresh),
            "same-boot migration keeps the old proof"
        );
        legacy.root_identity.as_mut().unwrap().0 += 1024;
        assert!(
            !same_binding(&legacy, &fresh),
            "legacy pair has no birthtime/UUID proof"
        );
        assert!(same_binding_with_consent(&legacy, &fresh));
        // Durable share grants carry a second copy of the source witness. The
        // same explicit attachment must upgrade it without widening the grant.
        let mut grant = super::super::transfer::ShareGrant::new(
            uuid::Uuid::new_v4(),
            &legacy,
            "approved-receiver",
        );
        let before = serde_json::to_value(&grant)?;
        assert!(grant.validate(&fresh, "approved-receiver").is_err());
        assert!(grant.refresh_source(&fresh, false).is_err());
        assert!(grant.refresh_source(&fresh, true)?);
        grant.validate(&fresh, "approved-receiver")?;
        let after = serde_json::to_value(&grant)?;
        for key in ["peer", "vault", "receiver"] {
            assert_eq!(before[key], after[key]);
        }
        assert!(grant.validate(&fresh, "other-receiver").is_err());
        legacy.root_identity.as_mut().unwrap().1 += 1;
        assert!(!same_binding_with_consent(&legacy, &fresh));
        legacy.root_identity = fresh.root_identity;
        legacy.daemon.device_id = "other-device".into();
        assert!(!same_binding_with_consent(&legacy, &fresh));
        Ok(())
    }

    #[test]
    fn saved_inspection_cannot_authorize_a_replaced_root_or_legacy_identity() -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir()?;
        let vault = temp.path().join("vault");
        fs::create_dir(&vault)?;
        fs::write(vault.join("note.md"), b"old notes")?;
        let metadata = fs::metadata(&vault)?;
        let inspected = ExistingVault {
            daemon: DaemonIdentity {
                config_file: temp.path().join("fixture.xml"),
                device_id: "fixture".into(),
                certificate_hash: "fixture".into(),
            },
            version: "v2.1.7".into(),
            folder_id: "fixture".into(),
            path: vault.clone(),
            label: "fixture".into(),
            paused: false,
            folder_type: "sendreceive".into(),
            peers: vec![],
            root_identity: Some((metadata.dev(), metadata.ino())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            stable_root_identity: None,
        };
        assert!(same_binding(&inspected, &inspected));
        fs::rename(&vault, temp.path().join("previous"))?;
        fs::create_dir(&vault)?;
        fs::write(vault.join("note.md"), b"different notes")?;
        let metadata = fs::metadata(&vault)?;
        let mut replaced = inspected.clone();
        replaced.root_identity = Some((metadata.dev(), metadata.ino()));
        assert!(same_replica(&inspected, &replaced));
        assert!(!same_binding(&inspected, &replaced));
        let mut legacy = inspected.clone();
        legacy.root_identity = None;
        assert!(!same_binding(&legacy, &inspected));
        assert_eq!(
            fs::read(temp.path().join("previous/note.md"))?,
            b"old notes"
        );
        assert_eq!(fs::read(vault.join("note.md"))?, b"different notes");
        Ok(())
    }
    #[test]
    fn inspection_rejects_symlink_root_before_discovery_and_preserves_bytes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vault = temp.path().join("vault");
        fs::create_dir(&vault)?;
        fs::write(vault.join("note.md"), b"local bytes")?;
        let root = VaultRoot::open(&vault)?;
        root.verify(&vault)?; // Positive control: the actual directory is admitted.
        let alias = temp.path().join("alias");
        symlink(&vault, &alias)?;
        let error = inspect_existing(&[], &alias).unwrap_err();
        assert!(error.to_string().contains("same real directory"));
        assert_eq!(fs::read(vault.join("note.md"))?, b"local bytes");
        Ok(())
    }

    #[test]
    fn inspection_root_guard_rejects_replacement_directory_and_symlink() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vault = temp.path().join("vault");
        let previous = temp.path().join("previous");
        fs::create_dir(&vault)?;
        fs::write(vault.join("note.md"), b"original bytes")?;
        let root = VaultRoot::open(&vault)?;
        root.verify(&vault)?;
        let canonical = vault.canonicalize()?;
        fs::rename(&vault, &previous)?;
        fs::create_dir(&vault)?;
        fs::write(vault.join("note.md"), b"replacement bytes")?;
        assert_eq!(vault.canonicalize()?, canonical);
        assert!(root.verify(&vault).is_err());
        assert_eq!(fs::read(previous.join("note.md"))?, b"original bytes");
        assert_eq!(fs::read(vault.join("note.md"))?, b"replacement bytes");
        fs::rename(&vault, temp.path().join("replacement"))?;
        symlink(&previous, &vault)?;
        assert!(root.verify(&vault).is_err());
        assert_eq!(fs::read(vault.join("note.md"))?, b"original bytes");
        Ok(())
    }
}
