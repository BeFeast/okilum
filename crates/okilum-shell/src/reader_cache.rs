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
                        okilum_core::vault::display_path(&path)
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
                    okilum_core::vault::display_path(&discarded)
                ));
            }
            retained -= 1;
            report.removed.push(path);
        }
        Ok(report)
    }
}

// Immutable search generations inside one vault cache. A family is one 64-hex
// name: `generations/<name>` (directory-rename publication),
// `generations/<name>.repairs/<child>` (complete-marker publication on Windows,
// incremental checkpoints and repairs) and `attempts/<name>.<uuid>` staging.
// Every family is derived data; canonical notes never live under the cache.
const PINS: &str = "generation-pins";
const RETIRED: &str = "retired-generations";
/// Persisted hints that can name a generation for the next Reader open.
const REFERENCES: [&str; 3] = [
    "reader-startup.json",
    "reader-snapshot.json",
    "reader-delta.json",
];

fn generation_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn lock_file(path: &Path) -> Result<fs::File> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        ensure!(meta.is_file(), "Invalid search generation lock file");
    }
    Ok(fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

fn pin_directory(base: &Path) -> Result<PathBuf> {
    let pins = base.join(PINS);
    fs::create_dir_all(&pins).with_context(|| {
        format!(
            "Create search generation pin directory {}",
            okilum_core::vault::display_path(&pins)
        )
    })?;
    ensure!(
        fs::symlink_metadata(&pins)?.is_dir(),
        "Invalid search generation pin directory"
    );
    Ok(pins)
}

/// A live Reader or worker's claim on one generation family, held for as long
/// as it may open, read, fork or publish that family. Other processes see the
/// claim as an exclusively locked `<name>.<uuid>` file; a crashed holder's lock
/// is released by the OS and its file is reclaimed by the next collection.
#[derive(Debug)]
pub(crate) struct GenerationPin {
    path: PathBuf,
    file: Option<fs::File>,
}

impl GenerationPin {
    /// Take the pin before the first look at the family. The shared gate is
    /// held only while the pin file is created, so a collector that retires
    /// the family either finished before (the family is gone) or sees the pin.
    pub fn acquire(base: &Path, name: &str) -> Result<Self> {
        ensure!(generation_name(name), "Invalid search generation name");
        let pins = pin_directory(base)?;
        let gate = lock_file(&pins.join("gate"))?;
        gate.lock_shared()?;
        let path = pins.join(format!("{name}.{}", uuid::Uuid::new_v4()));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        if let Err(error) = file.try_lock() {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(anyhow::anyhow!("Lock search generation pin: {error}"));
        }
        drop(gate);
        Ok(Self {
            path,
            file: Some(file),
        })
    }
}

impl Drop for GenerationPin {
    fn drop(&mut self) {
        // Close before removal: Windows cannot delete a file this handle holds.
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug, Default)]
pub(crate) struct GenerationReport {
    pub retired: Vec<String>,
    pub retained: usize,
    /// Why this run left every family in place; a later run retries.
    pub skipped: Option<&'static str>,
    pub cleanup_errors: Vec<String>,
}

/// Identity of one persisted hint file. Hints are replaced atomically, so an
/// unchanged identity means unchanged contents.
type HintRevision = Option<okilum_core::vault::warm::SourceRevision>;

fn hint_revision(path: &Path) -> Result<HintRevision> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(_) => Ok(Some(okilum_core::vault::warm::SourceRevision::read(path)?)),
    }
}

type References = (std::collections::BTreeSet<String>, Vec<HintRevision>);

/// Every generation named by a persisted hint, or `None` when a hint was
/// replaced while reading. An unreadable or undecodable hint means the
/// collector cannot prove what the next open needs: fail rather than treat it
/// as referencing nothing.
fn persisted_references(base: &Path) -> Result<Option<References>> {
    #[derive(serde::Deserialize)]
    struct Hint {
        #[serde(default)]
        search_generation: Option<String>,
    }
    let mut names = std::collections::BTreeSet::new();
    let mut revisions = Vec::new();
    for file in REFERENCES {
        let path = base.join(file);
        let before = hint_revision(&path)?;
        if before.is_some() {
            let reader = std::io::BufReader::new(fs::File::open(&path)?);
            let hint: Hint = serde_json::from_reader(reader)
                .with_context(|| format!("Decode search generation hint {file}"))?;
            names.extend(hint.search_generation);
            if hint_revision(&path)? != before {
                return Ok(None);
            }
        }
        revisions.push(before);
    }
    Ok(Some((names, revisions)))
}

fn generation_families(base: &Path) -> Result<std::collections::BTreeMap<String, Vec<PathBuf>>> {
    let mut families = std::collections::BTreeMap::<String, Vec<PathBuf>>::new();
    for (directory, separator) in [("generations", ".repairs"), ("attempts", ".")] {
        let entries = match fs::read_dir(base.join(directory)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            // Symlinks and files are never ours to retire.
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let family = if directory == "generations" && generation_name(name) {
                name
            } else {
                match name.split_once(separator) {
                    Some((family, rest))
                        if generation_name(family)
                            && ((directory == "generations" && rest.is_empty())
                                || (directory == "attempts"
                                    && uuid::Uuid::parse_str(rest).is_ok())) =>
                    {
                        family
                    }
                    _ => continue,
                }
            };
            families
                .entry(family.to_owned())
                .or_default()
                .push(entry.path());
        }
    }
    Ok(families)
}

/// Retire generation families that no persisted hint names and no live Reader
/// or worker pins, in this or any other process. Runs after publication and
/// checkpoint persistence; never blocks a Reader for longer than a few renames.
pub(crate) fn collect_generations(base: &Path) -> Result<GenerationReport> {
    collect_generations_with(
        base,
        &mut || {},
        &mut |from, to| fs::rename(from, to),
        &mut |path| fs::remove_dir_all(path),
    )
}

fn collect_generations_with(
    base: &Path,
    // Test seam between the unlocked hint read and the gated recheck.
    after_scan: &mut impl FnMut(),
    retire: &mut impl FnMut(&Path, &Path) -> std::io::Result<()>,
    cleanup: &mut impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<GenerationReport> {
    let mut report = GenerationReport::default();
    if !fs::symlink_metadata(base).is_ok_and(|meta| meta.is_dir()) {
        return Ok(report);
    }
    let pins = pin_directory(base)?;
    // One collector at a time; another run already covers this cache.
    let collector = lock_file(&pins.join("collector"))?;
    if collector.try_lock().is_err() {
        report.skipped = Some("another collection is running");
        return Ok(report);
    }
    let retired = base.join(RETIRED);
    fs::create_dir_all(&retired)?;
    ensure!(
        fs::symlink_metadata(&retired)?.is_dir(),
        "Invalid retired search generation directory"
    );
    // Finish cleanup interrupted after an atomic retirement. Retired names are
    // unreachable from any Reader, so this needs no pin or gate.
    let mut discarded = Vec::new();
    for entry in fs::read_dir(&retired)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok())
        {
            discarded.push(entry.path());
        }
    }
    retire_unused(
        base,
        &pins,
        &retired,
        &mut report,
        &mut discarded,
        after_scan,
        retire,
    )?;
    for path in discarded {
        if let Err(error) = cleanup(&path) {
            report.cleanup_errors.push(format!(
                "Remove retired search generation {}: {error}",
                okilum_core::vault::display_path(&path)
            ));
        }
    }
    Ok(report)
}

/// Atomically retire unreferenced, unpinned families into `retired`. Leaves
/// `report.skipped` set when this run cannot prove a family unused.
fn retire_unused(
    base: &Path,
    pins: &Path,
    retired: &Path,
    report: &mut GenerationReport,
    discarded: &mut Vec<PathBuf>,
    after_scan: &mut impl FnMut(),
    retire: &mut impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    const HINTS_CHANGED: &str = "persisted search hints changed";
    let families = generation_families(base)?;
    report.retained = families.len();
    let Some((referenced, revisions)) = persisted_references(base)? else {
        report.skipped = Some(HINTS_CHANGED);
        return Ok(());
    };
    let candidates: Vec<_> = families
        .iter()
        .filter(|(name, _)| !referenced.contains(*name))
        .collect();
    after_scan();
    if candidates.is_empty() {
        return Ok(());
    }
    let gate = lock_file(&pins.join("gate"))?;
    if gate.try_lock().is_err() {
        // A Reader is pinning right now; never make it wait for cleanup.
        report.skipped = Some("a Reader is pinning a generation");
        return Ok(());
    }
    let pinned = live_pins(pins)?;
    let current = REFERENCES
        .iter()
        .map(|file| hint_revision(&base.join(file)))
        .collect::<Result<Vec<_>>>()?;
    if current != revisions {
        // A checkpoint persisted after the scan; its hint may name a family
        // whose pin was released before the gate. Retry on the next run.
        report.skipped = Some(HINTS_CHANGED);
        return Ok(());
    }
    for (name, members) in candidates {
        if pinned.contains(name) {
            continue;
        }
        let mut complete = true;
        for member in members {
            let target = retired.join(uuid::Uuid::new_v4().to_string());
            // Each member is retired atomically. An open handle (e.g. Windows
            // antivirus or a Reader without pins) keeps it in place.
            match retire(member, &target) {
                Ok(()) => discarded.push(target),
                Err(error) => {
                    complete = false;
                    report.cleanup_errors.push(format!(
                        "Retire search generation {}: {error}",
                        okilum_core::vault::display_path(member)
                    ));
                }
            }
        }
        if complete {
            report.retired.push(name.clone());
            report.retained -= 1;
        }
    }
    drop(gate);
    Ok(())
}

/// Families claimed by a live holder. A pin whose lock can be taken belongs to
/// a holder that exited or crashed; it is removed here, under the gate.
fn live_pins(pins: &Path) -> Result<std::collections::BTreeSet<String>> {
    let mut live = std::collections::BTreeSet::new();
    for entry in fs::read_dir(pins)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some((name, id)) = file_name.to_str().and_then(|name| name.split_once('.')) else {
            continue;
        };
        if !generation_name(name) || uuid::Uuid::parse_str(id).is_err() {
            continue;
        }
        let path = entry.path();
        let stale = match fs::OpenOptions::new().write(true).open(&path) {
            Ok(file) => {
                let stale = file.try_lock().is_ok();
                drop(file);
                stale
            }
            // Removed by its holder after enumeration.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            // Unknown state is a live claim, never permission to delete.
            Err(_) => false,
        };
        if stale {
            let _ = fs::remove_file(&path);
        } else {
            live.insert(name.to_owned());
        }
    }
    Ok(live)
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

    fn name(n: usize) -> String {
        format!("{n:064x}")
    }

    /// Complete-marker publication: the only layout Windows uses for builds
    /// and the only layout incremental checkpoints use on every platform.
    fn publish_marker(base: &Path, n: usize) -> PathBuf {
        let child = base
            .join("generations")
            .join(format!("{}.repairs", name(n)))
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&child).unwrap();
        fs::write(child.join("meta.json"), "derived index").unwrap();
        fs::write(child.join("complete"), b"1").unwrap();
        child
    }

    /// Directory-rename publication used by full builds outside Windows.
    fn publish_renamed(base: &Path, n: usize) -> PathBuf {
        let directory = base.join("generations").join(name(n));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("complete"), b"1").unwrap();
        directory
    }

    /// Atomic replacement, as the warm cache writers persist hints.
    fn hint(base: &Path, file: &str, generation: Option<usize>) {
        fs::create_dir_all(base).unwrap();
        let mut temp = tempfile::NamedTempFile::new_in(base).unwrap();
        std::io::Write::write_all(
            &mut temp,
            &serde_json::to_vec(&serde_json::json!({
                "schema": 2,
                "entries": [{"path": "note.md"}],
                "search_generation": generation.map(name),
            }))
            .unwrap(),
        )
        .unwrap();
        temp.persist(base.join(file)).unwrap();
    }

    fn families(base: &Path) -> std::collections::BTreeSet<String> {
        generation_families(base).unwrap().into_keys().collect()
    }

    fn set(numbers: &[usize]) -> std::collections::BTreeSet<String> {
        numbers.iter().copied().map(name).collect()
    }

    #[test]
    fn repeated_checkpoints_stay_bounded_while_an_older_reader_can_fork() {
        use okilum_core::search::{SearchDocument, Searcher};
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        // Warm open: startup and source bank name the full build.
        publish_renamed(&base, 0);
        hint(&base, "reader-startup.json", Some(0));
        hint(&base, "reader-snapshot.json", Some(0));
        let mut older_reader = None;
        let mut retired = 0;
        for n in 1..=10 {
            let pin = GenerationPin::acquire(&base, &name(n)).unwrap();
            let child = publish_marker(&base, n);
            if n == 2 {
                // A second Reader opens this checkpoint and keeps searching it.
                fs::remove_file(child.join("meta.json")).unwrap();
                fs::remove_file(child.join("complete")).unwrap();
                Searcher::build_documents(
                    &[SearchDocument {
                        path: "note.md".into(),
                        title: "Note".into(),
                        text: "olderreader".into(),
                    }],
                    &child,
                )
                .unwrap()
                .finish_build()
                .unwrap();
                fs::write(child.join("complete"), b"1").unwrap();
                let reader_pin = GenerationPin::acquire(&base, &name(2)).unwrap();
                older_reader = Some(Searcher::open(&child).unwrap().pinned(reader_pin));
            }
            hint(&base, "reader-delta.json", Some(n));
            drop(pin);
            let report = collect_generations(&base).unwrap();
            assert!(report.cleanup_errors.is_empty() && report.skipped.is_none());
            retired += report.retired.len();
        }
        assert!(retired >= 7, "positive control: checkpoints were retired");
        assert_eq!(families(&base), set(&[0, 2, 10]));
        let older = older_reader.unwrap();
        assert_eq!(older.search("olderreader", 5).unwrap().len(), 1);
        let fork = older.fork_session().unwrap();
        assert_eq!(fork.search("olderreader", 5).unwrap().len(), 1);
        drop(older);
        assert_eq!(collect_generations(&base).unwrap().retired, [name(2)]);
        assert_eq!(fork.search("olderreader", 5).unwrap().len(), 1);
        assert_eq!(families(&base), set(&[0, 10]));
        assert!(
            fs::read_dir(base.join(PINS))
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains('-')),
            "released pins leave no files"
        );
        assert!(fs::read_dir(base.join(RETIRED)).unwrap().next().is_none());
    }

    #[test]
    fn concurrent_checkpoints_never_lose_the_persisted_generation() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let temp = tempfile::tempdir().unwrap();
        let base = Arc::new(temp.path().join("cache"));
        publish_marker(&base, 0);
        hint(&base, "reader-delta.json", Some(0));
        let done = Arc::new(AtomicBool::new(false));
        let collector = {
            let (base, done) = (base.clone(), done.clone());
            std::thread::spawn(move || {
                let (mut runs, mut retired) = (0, 0);
                while !done.load(Ordering::Acquire) || runs < 3 {
                    let report = collect_generations(&base).unwrap();
                    assert!(report.cleanup_errors.is_empty());
                    retired += report.retired.len();
                    runs += 1;
                    let value: serde_json::Value =
                        serde_json::from_slice(&fs::read(base.join("reader-delta.json")).unwrap())
                            .unwrap();
                    let current = value["search_generation"].as_str().unwrap();
                    assert!(
                        families(&base).contains(current),
                        "persisted generation {current} must survive collection"
                    );
                }
                retired
            })
        };
        // The checkpoint writer is a separate pin holder, as another process is.
        for n in 1..=60 {
            let pin = GenerationPin::acquire(&base, &name(n)).unwrap();
            publish_marker(&base, n);
            hint(&base, "reader-delta.json", Some(n));
            drop(pin);
        }
        done.store(true, Ordering::Release);
        let retired = collector.join().unwrap();
        assert!(retired > 0, "positive control: collection ran concurrently");
        let report = collect_generations(&base).unwrap();
        assert!(report.skipped.is_none());
        assert_eq!(families(&base), set(&[60]));
    }

    #[test]
    fn hint_published_after_the_scan_defers_collection() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        publish_marker(&base, 1);
        hint(&base, "reader-delta.json", Some(1));
        // Published and unpinned before the collector's gate, persisted after
        // its unlocked scan: the old hint read alone would retire it.
        publish_marker(&base, 2);
        let report = collect_generations_with(
            &base,
            &mut || hint(&base, "reader-delta.json", Some(2)),
            &mut |from, to| fs::rename(from, to),
            &mut |path| fs::remove_dir_all(path),
        )
        .unwrap();
        assert_eq!(report.skipped, Some("persisted search hints changed"));
        assert_eq!(families(&base), set(&[1, 2]));
        assert_eq!(
            collect_generations(&base).unwrap().retired,
            [name(1)],
            "positive control: the next run retires the superseded hint"
        );
        assert_eq!(families(&base), set(&[2]));
    }

    #[test]
    fn crashed_pins_are_reclaimed_but_live_pins_from_any_holder_are_kept() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        hint(&base, "reader-startup.json", None);
        for n in 1..=3 {
            publish_marker(&base, n);
        }
        // A holder that crashed leaves an unlocked pin file behind.
        let pins = pin_directory(&base).unwrap();
        let crashed = pins.join(format!("{}.{}", name(1), uuid::Uuid::new_v4()));
        fs::write(&crashed, "").unwrap();
        let live = GenerationPin::acquire(&base, &name(2)).unwrap();
        let report = collect_generations(&base).unwrap();
        assert_eq!(report.retired, [name(1), name(3)]);
        assert!(!crashed.exists());
        assert_eq!(families(&base), set(&[2]));
        drop(live);
        assert_eq!(collect_generations(&base).unwrap().retired, [name(2)]);
    }

    #[test]
    fn undecodable_hint_retires_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        publish_marker(&base, 1);
        publish_marker(&base, 2);
        hint(&base, "reader-snapshot.json", Some(1));
        fs::write(base.join("reader-startup.json"), "{\"search_generation\":").unwrap();
        assert!(collect_generations(&base).is_err());
        assert_eq!(families(&base), set(&[1, 2]));
        hint(&base, "reader-startup.json", Some(1));
        assert_eq!(
            collect_generations(&base).unwrap().retired,
            [name(2)],
            "positive control: a readable hint permits collection"
        );
    }

    #[test]
    fn open_handles_and_cleanup_failures_never_invalidate_the_current_cache() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        let current = publish_marker(&base, 1);
        publish_renamed(&base, 2);
        publish_marker(&base, 3);
        hint(&base, "reader-delta.json", Some(1));
        let interrupted = base.join(RETIRED).join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&interrupted).unwrap();
        fs::write(interrupted.join("meta.json"), "half-removed").unwrap();
        // Windows refuses to move a directory with an open descendant handle;
        // cleanup can be refused by antivirus. Neither touches the current cache.
        let report = collect_generations_with(
            &base,
            &mut || {},
            &mut |from, to| {
                if from.ends_with(name(2)) {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "open descendant handle",
                    ))
                } else {
                    fs::rename(from, to)
                }
            },
            &mut |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "antivirus cleanup hold",
                ))
            },
        )
        .unwrap();
        assert_eq!(report.retired, [name(3)]);
        assert_eq!(report.retained, 2);
        assert_eq!(report.cleanup_errors.len(), 3, "{report:?}");
        assert_eq!(families(&base), set(&[1, 2]));
        assert!(current.join("complete").is_file() && interrupted.exists());
        let report = collect_generations(&base).unwrap();
        assert_eq!(report.retired, [name(2)]);
        assert!(report.cleanup_errors.is_empty());
        assert!(fs::read_dir(base.join(RETIRED)).unwrap().next().is_none());
        assert_eq!(families(&base), set(&[1]));
        assert!(current.join("complete").is_file());
    }

    #[test]
    fn busy_gate_or_collector_skips_without_waiting() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        publish_marker(&base, 1);
        let pins = pin_directory(&base).unwrap();
        let gate = lock_file(&pins.join("gate")).unwrap();
        gate.lock_shared().unwrap();
        let report = collect_generations(&base).unwrap();
        assert_eq!(report.skipped, Some("a Reader is pinning a generation"));
        drop(gate);
        let collector = lock_file(&pins.join("collector")).unwrap();
        collector.try_lock().unwrap();
        let report = collect_generations(&base).unwrap();
        assert_eq!(report.skipped, Some("another collection is running"));
        drop(collector);
        assert_eq!(collect_generations(&base).unwrap().retired, [name(1)]);
    }

    #[test]
    fn only_owned_family_layouts_are_collected() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("cache");
        publish_marker(&base, 1);
        let attempt = base
            .join("attempts")
            .join(format!("{}.{}", name(1), uuid::Uuid::new_v4()));
        fs::create_dir_all(&attempt).unwrap();
        let legacy = base.join("attempts").join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&legacy).unwrap();
        let unrelated = base.join("generations").join("unrelated");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(
            base.join("generations").join(name(9)),
            "a file, not a family",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(temp.path(), base.join("generations").join(name(8))).unwrap();
        fs::write(base.join("reader-primary.json"), "{}").unwrap();
        let report = collect_generations(&base).unwrap();
        assert_eq!(report.retired, [name(1)]);
        assert!(
            !attempt.exists(),
            "crashed staging of the family is reclaimed"
        );
        assert!(legacy.exists() && unrelated.exists());
        assert!(base.join("generations").join(name(9)).is_file());
        #[cfg(unix)]
        assert!(temp.path().exists() && base.join("generations").join(name(8)).exists());
        assert!(base.join("reader-primary.json").exists());
    }
}
