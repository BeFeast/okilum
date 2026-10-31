//! Disposable last-known icon metadata. Loading never contacts T3 or the logo
//! host; a separately scheduled refresh replaces only confirmed answers.
use super::{Counterpart, CounterpartRefresh};
use crate::projects::Project;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Binding {
    folder: PathBuf,
    environment: String,
    origin: String,
    executor: Option<String>,
    checkout: Option<String>,
}
impl Binding {
    fn new(project: &Project, origins: &BTreeMap<String, String>) -> Option<Self> {
        let environment = project.environment.clone()?;
        Some(Self {
            folder: project.folder.clone(),
            origin: origins.get(&environment)?.clone(),
            environment,
            executor: project.appearance.executor_project.clone(),
            checkout: project.appearance.source_checkout.clone(),
        })
    }
    fn path(&self, cache: &Path) -> PathBuf {
        let key = Sha256::digest(serde_json::to_vec(self).expect("icon binding serializes"));
        let key: String = key.iter().map(|b| format!("{b:02x}")).collect();
        cache.join("counterparts-v1").join(format!("{key}.json"))
    }
}
#[derive(Serialize, Deserialize)]
struct Entry {
    binding: Binding,
    counterpart: Counterpart,
}

/// Read at discovery time, before displaying rows or starting network work.
pub fn load(
    cache: &Path,
    projects: &[Project],
    origins: &BTreeMap<String, String>,
) -> BTreeMap<PathBuf, Counterpart> {
    let mut links = BTreeMap::new();
    for project in projects {
        let Some(binding) = Binding::new(project, origins) else {
            continue;
        };
        let path = binding.path(cache);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= 64 * 1024) {
            continue;
        }
        let Some(mut entry) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Entry>(&bytes).ok())
        else {
            continue;
        };
        if entry.binding != binding || entry.counterpart.environment != binding.environment {
            continue;
        }
        if entry
            .counterpart
            .logo
            .as_deref()
            .is_some_and(|path| !super::usable_logo(path))
        {
            entry.counterpart.logo = None;
        }
        links.insert(project.folder.clone(), entry.counterpart);
    }
    links
}

/// A network error keeps the previous entry. A successful removal/reset is
/// authoritative. Runs on the refresh worker, never on the UI thread.
pub fn store(
    cache: &Path,
    projects: &[Project],
    origins: &BTreeMap<String, String>,
    refresh: &CounterpartRefresh,
) -> anyhow::Result<()> {
    use std::io::Write as _;
    for project in projects {
        if !refresh.confirmed.contains(&project.folder) {
            continue;
        }
        let Some(binding) = Binding::new(project, origins) else {
            continue;
        };
        let path = binding.path(cache);
        let Some(counterpart) = refresh.links.get(&project.folder) else {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            continue;
        };
        std::fs::create_dir_all(path.parent().unwrap())?;
        let bytes = serde_json::to_vec(&Entry {
            binding,
            counterpart: counterpart.clone(),
        })?;
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            drop(file);
            std::fs::rename(&temp, &path)
        })();
        let _ = std::fs::remove_file(&temp);
        result?;
    }
    Ok(())
}

/// Remove metadata no project of `vault` can load any more: its folder was
/// renamed or deleted, or its binding (environment, T3 origin, executor,
/// checkout) changed. `projects` must be the whole discovery of `vault`.
///
/// Only local facts decide; T3 is never asked, so an offline or failed
/// refresh cannot clear an icon. Kept on purpose:
/// - entries outside `vault` (another vault sharing this cache);
/// - a folder that is not discovered but still has a project manifest
///   (a broken manifest, a folder discovery does not walk, a symlinked
///   second vault);
/// - a project whose environment has no known origin: its binding cannot
///   be resolved, so it is not declared obsolete.
///
/// Returns how many entries were removed. Blocks; run it off the UI thread.
pub fn collect(
    cache: &Path,
    vault: &Path,
    projects: &[Project],
    origins: &BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    let directory = cache.join("counterparts-v1");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let current: BTreeMap<&Path, &Project> =
        projects.iter().map(|p| (p.folder.as_path(), p)).collect();
    let mut removed = 0;
    for file in entries {
        let path = file?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json")
            || !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= 64 * 1024)
        {
            continue;
        }
        // Unreadable entries are left alone: they are not loaded either, and
        // the folder they name is unknown.
        let Some(entry) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Entry>(&bytes).ok())
        else {
            continue;
        };
        let folder = entry.binding.folder.as_path();
        if !folder.starts_with(vault) {
            continue;
        }
        let obsolete = match current.get(folder) {
            Some(project) if project.environment.is_none() => true,
            Some(project) => {
                Binding::new(project, origins).is_some_and(|binding| binding != entry.binding)
            }
            None => !has_manifest(folder),
        };
        if !obsolete {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(removed)
}

/// True unless the manifest is known to be gone; an unreadable folder keeps
/// its entry.
fn has_manifest(folder: &Path) -> bool {
    ["project.toml", "project.yaml"].iter().any(|name| {
        !matches!(
            std::fs::symlink_metadata(folder.join("ops").join(name)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound
        )
    })
}

#[cfg(test)]
mod tests;
