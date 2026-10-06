//! Vault-scoped derived caches. Leases keep live Readers and workers out of LRU
//! eviction; the tiny registry contains no note contents or durable session state.
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{ensure, Context as _, Result};

const RETAINED_VAULTS: usize = 3;

#[derive(Debug, Default)]
pub(crate) struct PruneReport {
    pub removed: Vec<PathBuf>,
    pub cleanup_errors: Vec<String>,
}

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
        Ok(Self {
            root: root.to_owned(),
            path,
            _lock: lock,
        })
    }

    /// Candidate acquisition pins existing files but does not count a failed
    /// or cancelled open as recent. Only the accepted document marks usage.
    pub fn mark_published(&self) -> Result<()> {
        fs::create_dir_all(&self.path)?;
        let parent = self.path.parent().context("Reader cache has no parent")?;
        let name = cache_name(&self.path).unwrap();
        usage_file(parent, &format!("{name}.opened"))?.set_modified(SystemTime::now())?;
        Ok(())
    }

    /// Called after Ready publication, never in a source refresh.
    pub fn prune(&self) -> Result<PruneReport> {
        self.prune_with_cleanup(&mut |path| fs::remove_dir_all(path))
    }

    fn prune_with_cleanup(
        &self,
        cleanup: &mut impl FnMut(&Path) -> std::io::Result<()>,
    ) -> Result<PruneReport> {
        let parent = self.path.parent().context("Reader cache has no parent")?;
        // Two collectors must not both subtract the same oldest cache from
        // stale counts and evict more than needed. Acquisition uses per-vault
        // locks, so this background-only gate never stalls an opening Reader.
        let maintenance = usage_file(parent, "retention")?;
        if maintenance.try_lock().is_err() {
            return Ok(PruneReport::default());
        }
        let mut report = PruneReport::default();
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
                if let Err(error) = cleanup(&path) {
                    report.cleanup_errors.push(format!(
                        "Remove retired cache {}: {error}",
                        tessera_core::vault::display_path(&path)
                    ));
                }
                continue;
            }
            let Some(name) = cache_name(&path) else {
                continue;
            };
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let usage = parent.join(".usage").join(format!("{name}.opened"));
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
            let current =
                fs::symlink_metadata(parent.join(".usage").join(format!("{name}.opened")))?
                    .modified()?;
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
            if let Err(error) = cleanup(&discarded) {
                report.cleanup_errors.push(format!(
                    "Remove retired cache {}: {error}",
                    tessera_core::vault::display_path(&discarded)
                ));
            }
            retained -= 1;
            report.removed.push(path);
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(base: &Path, number: usize) -> Lease {
        let root = base.join(format!("vault-{number}"));
        fs::create_dir_all(&root).unwrap();
        let lease =
            Lease::acquire(base.join("reader").join(format!("{number:064x}")), &root).unwrap();
        lease.mark_published().unwrap();
        lease
    }

    #[test]
    fn lru_keeps_three_vaults_and_skips_live_shared_leases() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        let first = lease(base, 1);
        let also_first = lease(base, 1);
        for n in 2..=4 {
            let entry = lease(base, n);
            let usage = base.join("reader/.usage").join(format!("{n:064x}.opened"));
            fs::OpenOptions::new()
                .write(true)
                .open(usage)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(n as u64))
                .unwrap();
            fs::write(entry.path.join("reader-startup.json"), "derived cache").unwrap();
        }
        // Make the pinned vault the oldest: protection must come from the lock.
        usage_file(first.path.parent().unwrap(), &format!("{:064x}.opened", 1))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        let latest = lease(base, 4);
        let removed = latest.prune().unwrap().removed;
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
            .removed
            .iter()
            .all(|path| path != &also_first.path));
        drop(also_first);
        let sixth = lease(base, 6);
        assert!(sixth
            .prune()
            .unwrap()
            .removed
            .iter()
            .any(|path| path == &base.join("reader").join(format!("{:064x}", 1))));
        assert!(
            base.join("vault-1").exists(),
            "canonical vault never removed"
        );
    }

    #[test]
    fn failed_retired_cleanup_does_not_stop_eviction_or_retry() {
        let temp = tempfile::tempdir().unwrap();
        for n in 1..=4 {
            drop(lease(temp.path(), n));
        }
        let latest = lease(temp.path(), 4);
        let retired = latest
            .path
            .parent()
            .unwrap()
            .join(format!(".evicted-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&retired).unwrap();
        let report = latest
            .prune_with_cleanup(&mut |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "antivirus cleanup hold",
                ))
            })
            .unwrap();
        assert_eq!(
            report.removed.len(),
            1,
            "successful retirement still reduces active caches to three"
        );
        assert_eq!(
            report.cleanup_errors.len(),
            2,
            "both sweep and newly retired cleanup failures recorded"
        );
        assert!(latest.path.exists() && retired.exists());
        let report = latest.prune().unwrap();
        assert!(report.cleanup_errors.is_empty() && report.removed.is_empty());
        assert!(
            !retired.exists(),
            "retry completes cleanup without evicting more caches"
        );
    }

    #[test]
    fn candidate_acquisition_does_not_replace_a_recent_published_vault() {
        let temp = tempfile::tempdir().unwrap();
        for n in 1..=3 {
            drop(lease(temp.path(), n));
        }
        let root = temp.path().join("candidate");
        fs::create_dir(&root).unwrap();
        let candidate = Lease::acquire(
            temp.path().join("reader").join(format!("{:064x}", 4)),
            &root,
        )
        .unwrap();
        assert!(
            !candidate.path.exists(),
            "pinning an unaccepted open creates no cache directory"
        );
        let current = lease(temp.path(), 3);
        assert!(current.prune().unwrap().removed.is_empty());
        candidate.mark_published().unwrap();
        assert_eq!(
            candidate.prune().unwrap().removed.len(),
            1,
            "positive control: accepted fourth vault enters retention"
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
        assert!(latest.prune().unwrap().removed.is_empty());
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
        assert!(latest.prune().unwrap().removed.is_empty());
        drop(maintenance);
        assert_eq!(
            latest.prune().unwrap().removed.len(),
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
            usage_file(entry.path.parent().unwrap(), &format!("{n:064x}.opened"))
                .unwrap()
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
            newest.prune().unwrap().removed,
            [base.join("reader").join(format!("{:064x}", 2))]
        );
        assert!(
            unrelated.exists()
                && unregistered.join("canonical.md").exists()
                && base.join("vault-1").exists()
        );
    }
}
