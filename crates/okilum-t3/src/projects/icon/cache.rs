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

#[cfg(test)]
mod tests;
