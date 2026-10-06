//! Vault-scoped derived caches. Leases keep live Readers and workers out of LRU
//! eviction; the tiny registry contains no note contents or durable session state.
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{ensure, Context as _, Result};

const RETAINED_VAULTS: usize = 3;

#[derive(Debug)]
pub(crate) struct Lease {
    pub root: PathBuf,
    pub path: PathBuf,
    _lock: fs::File,
}

fn cache_name(path: &Path) -> Option<&str> {
    path.file_name()?
        .to_str()
        .filter(|name| name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn usage_file(parent: &Path, name: &str) -> Result<fs::File> {
    let registry = parent.join(".usage");
    fs::create_dir_all(&registry)?;
    ensure!(
        fs::symlink_metadata(&registry)?.is_dir(),
        "Invalid Reader cache registry"
    );
    let path = registry.join(name);
    if let Ok(meta) = fs::symlink_metadata(&path) {
        ensure!(meta.is_file(), "Invalid Reader cache usage file");
    }
    Ok(fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

impl Lease {
    pub fn acquire(path: PathBuf, root: &Path) -> Result<Self> {
        super::reader_loading::validate_external_cache(&path, root)?;
        let name = cache_name(&path).context("Invalid managed Reader cache name")?;
        let parent = path.parent().context("Reader cache has no parent")?;
        let lock = usage_file(parent, name)?;
        // Eviction holds this exclusive lock only while renaming the directory,
        // never during recursive cleanup. Multiple readers share a vault lease.
        lock.lock_shared()?;
        lock.set_modified(SystemTime::now())?;
        fs::create_dir_all(&path)?;
        Ok(Self {
            root: root.to_owned(),
            path,
            _lock: lock,
        })
    }

    /// Called after first-document publication on the background worker.
    pub fn prune(&self) -> Result<Vec<PathBuf>> {
        let parent = self.path.parent().context("Reader cache has no parent")?;
        // Two collectors must not both subtract the same oldest cache from
        // stale counts and evict more than needed. Acquisition uses per-vault
        // locks, so this background-only gate never stalls an opening Reader.
        let maintenance = usage_file(parent, "retention")?;
        if maintenance.try_lock().is_err() {
            return Ok(Vec::new());
        }
        let mut candidates = Vec::new();
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_prefix(".evicted-"))
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            {
                // Finish a cleanup interrupted after its atomic retirement.
                fs::remove_dir_all(&path)?;
                continue;
            }
            let Some(name) = cache_name(&path) else {
                continue;
            };
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let usage = parent.join(".usage").join(name);
            let age = match fs::symlink_metadata(&usage) {
                Ok(meta) if meta.is_file() => meta.modified()?,
                Ok(_) => continue,
                // Only caches registered by this lifecycle participate. An
                // unrelated hash-named directory must never be treated as ours.
                Err(_) => continue,
            };
            candidates.push((age, path));
        }
        candidates.sort();
        let mut retained = candidates.len();
        let mut removed = Vec::new();
        for (age, path) in candidates {
            if retained <= RETAINED_VAULTS {
                break;
            }
            if path == self.path {
                continue;
            }
            let name = cache_name(&path).unwrap();
            let Ok(lock) = usage_file(parent, name) else {
                continue;
            };
            if lock.try_lock().is_err() {
                continue;
            }
            // A vault reopened after enumeration must not be evicted using its
            // old age, even if its last Reader has already closed again.
            let current = lock.metadata()?.modified()?;
            if current > age {
                continue;
            }
            if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            let discarded = parent.join(format!(".evicted-{}", uuid::Uuid::new_v4()));
            if fs::rename(&path, &discarded).is_err() {
                continue;
            }
            drop(lock);
            // Canonical/durable files are never under this managed namespace.
            fs::remove_dir_all(&discarded)?;
            retained -= 1;
            removed.push(path);
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(base: &Path, number: usize) -> Lease {
        let root = base.join(format!("vault-{number}"));
        fs::create_dir_all(&root).unwrap();
        Lease::acquire(base.join("reader").join(format!("{number:064x}")), &root).unwrap()
    }

    #[test]
    fn lru_keeps_three_vaults_and_skips_live_shared_leases() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        let first = lease(base, 1);
        let also_first = lease(base, 1);
        for n in 2..=4 {
            let entry = lease(base, n);
            let usage = base.join("reader/.usage").join(format!("{n:064x}"));
            fs::OpenOptions::new()
                .write(true)
                .open(usage)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(n as u64))
                .unwrap();
            fs::write(entry.path.join("reader-startup.json"), "derived cache").unwrap();
        }
        // Make the pinned vault the oldest: protection must come from the lock.
        first._lock.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        let latest = lease(base, 4);
        let removed = latest.prune().unwrap();
        assert_eq!(removed, [base.join("reader").join(format!("{:064x}", 2))]);
        assert!(first.path.exists() && latest.path.exists());
        drop(first);
        assert!(
            also_first.path.exists(),
            "independent shared lease positive control"
        );
        let fifth = lease(base, 5);
        assert!(fifth
            .prune()
            .unwrap()
            .iter()
            .all(|path| path != &also_first.path));
        drop(also_first);
        let sixth = lease(base, 6);
        assert!(sixth
            .prune()
            .unwrap()
            .iter()
            .any(|path| path == &base.join("reader").join(format!("{:064x}", 1))));
        assert!(
            base.join("vault-1").exists(),
            "canonical vault never removed"
        );
    }

    #[test]
    fn interrupted_retirement_is_reclaimed_on_the_next_background_prune() {
        let temp = tempfile::tempdir().unwrap();
        let latest = lease(temp.path(), 1);
        let parent = latest.path.parent().unwrap();
        let retired = parent.join(format!(".evicted-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&retired).unwrap();
        fs::write(retired.join("reader-snapshot.json"), "derived old bank").unwrap();
        let unrelated = parent.join(".evicted-unrelated");
        fs::create_dir(&unrelated).unwrap();
        assert!(latest.prune().unwrap().is_empty());
        assert!(!retired.exists() && unrelated.exists() && latest.path.exists());
    }

    #[test]
    fn concurrent_retention_is_skipped_without_blocking_or_over_eviction() {
        let temp = tempfile::tempdir().unwrap();
        for n in 1..=4 {
            drop(lease(temp.path(), n));
        }
        let latest = lease(temp.path(), 4);
        let maintenance = usage_file(latest.path.parent().unwrap(), "retention").unwrap();
        maintenance.try_lock().unwrap();
        assert!(latest.prune().unwrap().is_empty());
        drop(maintenance);
        assert_eq!(
            latest.prune().unwrap().len(),
            1,
            "positive control: idle collector prunes exactly one of four caches"
        );
    }

    #[test]
    fn reopening_updates_lru_and_unrelated_or_symlink_entries_are_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        for n in 1..=3 {
            let entry = lease(base, n);
            entry
                ._lock
                .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(n as u64))
                .unwrap();
        }
        drop(lease(base, 1));
        let unrelated = base.join("reader/unrelated");
        fs::create_dir(&unrelated).unwrap();
        let unregistered = base.join("reader").join("b".repeat(64));
        fs::create_dir(&unregistered).unwrap();
        fs::write(
            unregistered.join("canonical.md"),
            "do not evict unknown data",
        )
        .unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                base.join("vault-1"),
                base.join("reader").join("a".repeat(64)),
            )
            .unwrap();
        }
        let newest = lease(base, 4);
        assert_eq!(
            newest.prune().unwrap(),
            [base.join("reader").join(format!("{:064x}", 2))]
        );
        assert!(
            unrelated.exists()
                && unregistered.join("canonical.md").exists()
                && base.join("vault-1").exists()
        );
    }
}
