//! Explicit Linux runtime reuse. Discovery and selection never start or stop a daemon.
use super::*;
use anyhow::{ensure, Context as _};
use okilum_sync_controller::daemon::{self, DaemonIdentity, Discovery};

/// Shared by managed installation and uninstall's live-service refusal. This
/// directory is shared with other apps and must never be recursively purged.
pub(crate) fn managed_service_directory() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("user service location unavailable")?;
    ensure!(
        config.is_absolute(),
        "absolute user service location required"
    );
    Ok(config.join("systemd/user"))
}

#[derive(Clone)]
pub(super) struct Choice {
    pub identity: DaemonIdentity,
    pub folders: usize,
    pub managed: bool,
    pub selected: bool,
    pub network: Option<okilum_sync_controller::network_policy::State>,
}
pub(super) struct Runtime {
    pub inventory: Discovery,
    pub configs: Vec<PathBuf>,
    pub choices: Vec<Choice>,
}
/// An explicit Share click on an existing replica opens inspection/consent;
/// it must never try to prepare a second daemon over that replica.
pub(super) fn route_share(operation: Operation, runtime: &Runtime) -> anyhow::Result<Operation> {
    let Operation::Share(path) = &operation else {
        return Ok(operation);
    };
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve vault to share {}", path.display()))?;
    let managed_selected = runtime
        .choices
        .iter()
        .any(|choice| choice.selected && choice.managed);
    let marker = canonical.join(".stfolder");
    let marked = match std::fs::symlink_metadata(&marker) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect Syncthing marker {}", marker.display()))
        }
    };
    let external = runtime.inventory.candidates.iter().any(|candidate| {
        candidate
            .folders
            .iter()
            .any(|(_, folder)| folder == &canonical)
            && !runtime
                .choices
                .iter()
                .any(|choice| choice.managed && choice.identity == candidate.identity)
    });
    if external || marked && !managed_selected {
        return Ok(Operation::Inspect(path.clone()));
    }
    Ok(operation)
}
pub(super) fn state_path(profile: &Path) -> anyhow::Result<PathBuf> {
    // Reader owns the profile and may legitimately create it with mode 0755.
    // Only Sync's child directory must be private; never chmod an existing profile.
    let sync = profile.join("sync");
    ensure_private_sync_directory(&sync)?;
    Ok(sync.join("direct"))
}
fn selection_path(state: &Path) -> anyhow::Result<PathBuf> {
    Ok(state
        .parent()
        .context("Sync state parent unavailable")?
        .join("runtime.json"))
}
pub(super) fn inspect(state: &Path, configs: &[PathBuf]) -> anyhow::Result<Runtime> {
    let mut configs = configs.to_vec();
    let managed = state
        .parent()
        .context("Sync state parent unavailable")?
        .join("managed/prepared/syncthing/config.xml");
    if managed.try_exists()? && !configs.contains(&managed) {
        configs.push(managed.clone());
    }
    let inventory = daemon::discover(&configs);
    let saved = selection_path(state)?;
    let selected: Option<DaemonIdentity> = if saved.try_exists()? {
        ensure!(
            std::fs::symlink_metadata(&saved)?.is_file(),
            "Sync runtime selection replaced"
        );
        Some(serde_json::from_slice(&std::fs::read(saved)?)?)
    } else {
        None
    };
    let managed_identity = if inventory
        .candidates
        .iter()
        .any(|c| c.identity.config_file == managed)
    {
        Some(daemon::managed_identity(
            managed.parent().unwrap().parent().unwrap(),
        )?)
    } else {
        None
    };
    let mut choices = inventory
        .candidates
        .iter()
        .filter(|c| daemon::compatible_external_version(&c.version))
        .map(|c| Choice {
            identity: c.identity.clone(),
            folders: c.folders.len(),
            managed: managed_identity.as_ref() == Some(&c.identity),
            selected: false,
            network: None,
        })
        .collect::<Vec<_>>();
    let chosen = if let Some(selected) = selected {
        let choice = choices
            .iter_mut()
            .find(|c| c.identity == selected)
            .context(
                "Selected Sync connection is unavailable or changed; restore it and refresh",
            )?;
        choice.identity.connect()?;
        choice.selected = true;
        Some(choice.identity.config_file.clone())
    } else if choices.len() == 1
        && inventory.unavailable.is_empty()
        && choices[0].identity.config_file == managed
    {
        choices[0].selected = true;
        Some(choices[0].identity.config_file.clone())
    } else {
        None
    };
    if let Some(choice) = choices.iter_mut().find(|c| c.selected) {
        choice.network = Some(okilum_sync_controller::network_policy::read(
            &choice.identity,
        )?);
    }
    Ok(Runtime {
        configs: chosen.map(|p| vec![p]).unwrap_or_else(|| configs.clone()),
        inventory,
        choices,
    })
}
pub(super) fn connection_error(runtime: &Runtime) -> Option<String> {
    if runtime.choices.iter().any(|choice| choice.selected)
        || runtime.inventory.candidates.is_empty() && runtime.inventory.unavailable.is_empty()
    {
        None
    } else {
        Some(user_error(&anyhow::anyhow!(
            "Syncthing connection requires explicit selection"
        )))
    }
}
pub(super) fn select(
    state: &Path,
    runtime: &mut Runtime,
    path: &Path,
    snapshot: &Snapshot,
) -> anyhow::Result<()> {
    ensure!(
        snapshot.peers.is_empty() && snapshot.attached.is_empty() && !snapshot.preparing,
        "Sync connection is already in use; keep its existing identity"
    );
    let identity = runtime.inventory.select(path)?.identity.clone();
    identity.connect()?;
    let file = selection_path(state)?;
    let mut temporary = tempfile::NamedTempFile::new_in(file.parent().unwrap())?;
    use std::io::Write;
    temporary.write_all(&serde_json::to_vec(&identity)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(file)?;
    runtime.configs = vec![identity.config_file.clone()];
    for choice in &mut runtime.choices {
        choice.selected = choice.identity == identity;
    }
    Ok(())
}
pub(super) fn check_destination(runtime: &Runtime, path: &Path) -> anyhow::Result<()> {
    let path = destination::canonical_path(path)?;
    let selected = runtime
        .configs
        .first()
        .context("Choose a Sync connection")?;
    runtime.inventory.folders()?;
    for candidate in &runtime.inventory.candidates {
        if &candidate.identity.config_file == selected {
            continue;
        }
        for (_, other) in &candidate.folders {
            ensure!(
                !path.starts_with(other) && !other.starts_with(&path),
                "destination overlaps another Syncthing folder"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn isolated_profile_keeps_sync_local_without_changing_reader_permissions() -> anyhow::Result<()>
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir()?;
        let first = root.path().join("first");
        let second = root.path().join("second");
        for p in [&first, &second] {
            std::fs::create_dir(p)?;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
        }
        assert_eq!(state_path(&first)?, first.join("sync/direct"));
        assert_eq!(state_path(&second)?, second.join("sync/direct"));
        assert_eq!(std::fs::metadata(first)?.mode() & 0o777, 0o755);
        assert_eq!(
            std::fs::metadata(second.join("sync"))?.mode() & 0o777,
            0o700
        );
        Ok(())
    }
    #[test]
    fn selecting_one_runtime_does_not_hide_another_vault_overlap() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let vault = root.path().join("external-vault");
        std::fs::create_dir(&vault)?;
        let selected = root.path().join("selected.xml");
        let runtime = Runtime {
            configs: vec![selected],
            choices: vec![],
            inventory: Discovery {
                unavailable: vec![],
                candidates: vec![daemon::Candidate {
                    identity: DaemonIdentity {
                        config_file: root.path().join("other.xml"),
                        device_id: "other".into(),
                        certificate_hash: "other".into(),
                    },
                    folders: vec![("external".into(), vault.clone())],
                    version: "v2.1.7".into(),
                }],
            },
        };
        assert!(check_destination(&runtime, &vault).is_err());
        let free = root.path().join("free");
        std::fs::create_dir(&free)?;
        check_destination(&runtime, &free)?;
        Ok(())
    }
}

/// Pairing entry can prepare only after a complete, empty external inventory.
/// Unavailable, incompatible and unselected external instances cannot imply absence.
pub(super) fn prepare_for_pairing(
    runtime: Runtime,
    prepare: impl FnOnce() -> anyhow::Result<Runtime>,
) -> anyhow::Result<Runtime> {
    if runtime.inventory.candidates.is_empty() && runtime.inventory.unavailable.is_empty() {
        prepare()
    } else {
        Ok(runtime)
    }
}

/// Explicit pairing consent only; never called by inspect/Read or selection failure.
pub(super) fn prepare_managed(
    state: &Path,
    configs: &[PathBuf],
    check: impl Fn() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let parent = state.parent().context("Sync state parent unavailable")?;
    let snapshot_file = state.join("direct.json");
    // Existing association is not permission to change runtime identity.
    if snapshot_file.try_exists()? {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&snapshot_file)?)?;
        ensure!(
            value["peers"].as_object().is_some_and(|v| v.is_empty())
                && value["attached"].as_array().is_some_and(|v| v.is_empty())
                && value["outbound"].is_null()
                && value["invitation"].is_null(),
            "Sync connection is already in use"
        );
    }
    let units = managed_service_directory()?;
    let identity = okilum_sync_controller::linux_bootstrap::connect(
        true,
        &parent.join("managed"),
        &units,
        configs,
        &check,
    )?;
    check()?;
    let file = selection_path(state)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temporary.write_all(&serde_json::to_vec(&identity)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&file)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod guided_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn guided_first_run_prepares_only_with_complete_absence_of_existing_syncthing(
    ) -> anyhow::Result<()> {
        let runtime = |unavailable, candidates| Runtime {
            configs: vec![],
            choices: vec![],
            inventory: Discovery {
                unavailable,
                candidates,
            },
        };
        assert!(
            connection_error(&runtime(vec![], vec![])).is_none(),
            "fresh preparation is not an existing-instance error"
        );
        let called = std::cell::Cell::new(false);
        prepare_for_pairing(runtime(vec![], vec![]), || {
            called.set(true);
            Ok(runtime(vec![], vec![]))
        })?;
        assert!(
            called.get(),
            "positive control: fresh pairing invokes bundled preparation implicitly"
        );
        called.set(false);
        let external = daemon::Candidate {
            identity: DaemonIdentity {
                config_file: PathBuf::from("/external/config.xml"),
                device_id: "external".into(),
                certificate_hash: "external".into(),
            },
            folders: vec![],
            version: "v1.29.5".into(),
        };
        let existing = prepare_for_pairing(runtime(vec![], vec![external]), || {
            called.set(true);
            anyhow::bail!("must not prepare")
        })?;
        assert!(!called.get());
        assert!(
            connection_error(&existing)
                .unwrap()
                .starts_with("No compatible Syncthing"),
            "positive control: an unselected existing instance keeps the actionable error"
        );
        assert!(
            existing.choices.is_empty(),
            "external inventory never grants implicit selection"
        );
        prepare_for_pairing(
            runtime(vec![PathBuf::from("/unavailable.xml")], vec![]),
            || {
                called.set(true);
                anyhow::bail!("must not prepare")
            },
        )?;
        assert!(!called.get(), "unavailable inventory is not absence");
        Ok(())
    }
}

#[cfg(test)]
mod destination_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn sync_new_destination_overlap_is_checked_before_leaf_creation() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let other = root.path().join("other-vault");
        std::fs::create_dir(&other)?;
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&other, &alias)?;
        let runtime = Runtime {
            configs: vec![root.path().join("selected.xml")],
            choices: vec![],
            inventory: Discovery {
                unavailable: vec![],
                candidates: vec![daemon::Candidate {
                    identity: DaemonIdentity {
                        config_file: root.path().join("other.xml"),
                        device_id: "other".into(),
                        certificate_hash: "other".into(),
                    },
                    folders: vec![("other".into(), other.clone())],
                    version: "v2.1.6".into(),
                }],
            },
        };
        for path in [other.join("Notes"), alias.join("Notes")] {
            assert!(check_destination(&runtime, &path).is_err());
            assert!(!path.exists());
        }
        let free = root.path().join("Notes");
        check_destination(&runtime, &free)?;
        assert!(
            !free.exists(),
            "preflight does not create the proposed destination"
        );
        Ok(())
    }
}
