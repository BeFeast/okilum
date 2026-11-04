//! Immutable preimages are also the recovery protocol for an interrupted apply.
//! Recovery recreates missing names only, never replaces a present version and
//! never resumes a decision against files that were not reviewed.
use super::*;

pub struct HistoryEntry {
    pub canonical: PathBuf,
    pub created: u64,
    pub versions: [RetainedVersion; 2],
    pub pending: bool,
    pub record: PathBuf,
    directory_identity: Option<(u64, u64)>,
    modes: [u32; 2],
}
#[derive(Default)]
pub struct History {
    pub entries: Vec<HistoryEntry>,
    pub warnings: Vec<String>,
}
pub(super) fn complete(record: &Path) -> Result<()> {
    marker(record, "complete", b"complete\n")
}
pub(super) fn refused(record: &mut PathBuf) -> Result<()> {
    if marker(record, "refused", b"refused\n").is_ok() {
        return Ok(());
    }
    // A blocked/partial marker must not turn a published decision into an
    // interrupted operation. Renaming the record itself is the fallback terminal
    // state; History still retains its bytes and never grants recreation authority.
    let terminal = record.with_extension("refused.json");
    renameat_with(
        rustix::fs::CWD,
        &*record,
        rustix::fs::CWD,
        &terminal,
        RenameFlags::NOREPLACE,
    )?;
    *record = terminal;
    File::open(record.parent().context("Missing recovery directory")?)?.sync_all()?;
    Ok(())
}
fn marker(record: &Path, extension: &str, contents: &[u8]) -> Result<()> {
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(record.with_extension(extension))?;
    file.write_all(contents)?;
    file.sync_all()?;
    File::open(record.parent().context("Missing recovery directory")?)?.sync_all()?;
    Ok(())
}
fn has_marker(record: &Path, extension: &str, contents: &[u8]) -> Result<bool> {
    let fd = match open(
        record.with_extension(extension),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let file = File::from(fd);
    ensure!(file.metadata()?.is_file(), "Invalid recovery marker");
    let mut bytes = Vec::new();
    file.take(32).read_to_end(&mut bytes)?;
    ensure!(bytes == contents, "Invalid recovery marker");
    Ok(true)
}

fn is_pending(record: &Path) -> Result<bool> {
    Ok(!record
        .file_name()
        .context("Missing record name")?
        .as_bytes()
        .ends_with(b".refused.json")
        && !has_marker(record, "refused", b"refused\n")?
        && !has_marker(record, "complete", b"complete\n")?
        && !has_marker(record, "recovered", b"recovered\n")?)
}

/// All records remain adoptable by previous-version retention (#2045). An
/// incomplete record must never be expired; this API does no cleanup.
pub fn history(directory: &Path, root: &Path) -> Result<History> {
    let mut history = History::default();
    if !directory.try_exists()? {
        return Ok(history);
    }
    let root = root.canonicalize()?;
    let fd = open(directory, flags(), Mode::empty())?;
    for (index, entry) in Dir::read_from(&fd)?.enumerate() {
        ensure!(
            index < 100_000,
            "Conflict history exceeds enumeration limit"
        );
        let entry = entry?;
        let name = entry.file_name().to_string_lossy();
        if !name.starts_with("sync-conflict-")
            || !name.ends_with(".json")
            || entry.file_type() != FileType::RegularFile
        {
            continue;
        }
        let record = directory.join(OsStr::from_bytes(entry.file_name().to_bytes()));
        let load = || -> Result<Option<HistoryEntry>> {
            let value = record_value(&record)?;
            let versions = retained_from_value(value.clone())?;
            if versions[0].root != root {
                return Ok(None);
            }
            let canonical = versions[0].path.clone();
            let created = value["created"].as_u64().unwrap_or(0);
            let pending = is_pending(&record)?;
            Ok(Some(HistoryEntry {
                canonical,
                created,
                versions,
                pending,
                record: record.clone(),
                directory_identity: serde_json::from_value(value["directory_identity"].clone())
                    .ok(),
                modes: [
                    value["this_mode"].as_u64().unwrap_or(0o600) as u32 & 0o777,
                    value["that_mode"].as_u64().unwrap_or(0o600) as u32 & 0o777,
                ],
            }))
        };
        match load() {
            Ok(Some(entry)) => history.entries.push(entry),
            Ok(None) => {}
            Err(error) => history.warnings.push(format!(
                "Cannot read {}: {error}. Recovery is retained.",
                record.display()
            )),
        }
    }
    history
        .entries
        .sort_by_key(|entry| std::cmp::Reverse(entry.created));
    Ok(history)
}

/// Explicit recovery action. The journal was fsynced before capture; publishing
/// each missing preimage with NOREPLACE makes retries and crashes idempotent.
/// Present files, including a successfully published decision, are left intact.
/// Keep-both proposals are never removed. Old records without directory identity
/// remain recoverable through History but cannot authorize path recreation.
pub fn recover_interrupted(directory: &Path, root: &Path) -> Result<Vec<String>> {
    let root = root.canonicalize()?;
    let history = history(directory, &root)?;
    let mut messages = history.warnings;
    for entry in history.entries.into_iter().filter(|entry| entry.pending) {
        let result = (|| -> Result<()> {
            let expected = entry
                .directory_identity
                .context("Missing directory identity; use Recover notes")?;
            require_local_retention_policy(&root)?;
            let fd = parent(&root, &entry.canonical)?;
            ensure!(
                identity(&fd)? == expected,
                "Source directory was replaced; use Recover notes"
            );
            for (index, version) in entry.versions.iter().enumerate() {
                ensure!(
                    version.path.parent() == entry.canonical.parent(),
                    "Unexpected recovery path"
                );
                let name = version.path.file_name().context("Missing recovery name")?;
                match rustix::fs::statat(&fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(_) => continue,
                    Err(rustix::io::Errno::NOENT) => {}
                    Err(error) => return Err(error.into()),
                }
                let mode = entry.modes[index];
                publish(&fd, &version.path, &version.bytes, mode)?;
            }
            ensure!(
                identity(&parent(&root, &entry.canonical)?)? == expected,
                "Source directory changed during recovery; originals remain in History"
            );
            marker(&entry.record, "recovered", b"recovered\n")?;
            Ok(())
        })();
        messages.push(match result {
            Ok(()) => format!("Recovered interrupted resolution for {}. Review the versions again; both originals remain in Note history.", entry.canonical.display()),
            Err(error) => format!("Recovery for {} needs attention: {error}. Both originals remain in Recover notes.", entry.canonical.display()),
        });
    }
    Ok(messages)
}

/// Called once by the primary Reader process, while its instance lock is held,
/// before path validation, warm snapshots, indexing or managed Sync startup.
/// Only canonical roots already authorized by durable resolution records qualify.
pub fn recover_startup(directory: &Path) -> Result<Vec<String>> {
    if !directory.try_exists()? {
        return Ok(Vec::new());
    }
    let fd = open(directory, flags(), Mode::empty())?;
    let mut roots = std::collections::BTreeSet::new();
    let mut messages = Vec::new();
    for (index, entry) in Dir::read_from(&fd)?.enumerate() {
        ensure!(
            index < 100_000,
            "Conflict recovery exceeds enumeration limit"
        );
        let entry = entry?;
        let name = entry.file_name().to_string_lossy();
        if !name.starts_with("sync-conflict-")
            || !name.ends_with(".json")
            || entry.file_type() != FileType::RegularFile
        {
            continue;
        }
        let record = directory.join(OsStr::from_bytes(entry.file_name().to_bytes()));
        // Completed/refused/repaired records carry no path-recreation authority.
        // Do not decode their potentially large byte arrays or touch their vaults
        // (which can have been intentionally deleted or disconnected since).
        match is_pending(&record) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(error) => {
                messages.push(format!(
                    "Cannot check {}: {error}; use Recover notes.",
                    record.display()
                ));
                continue;
            }
        }
        match retained_versions(&record) {
            Ok(versions) => {
                roots.insert(versions[0].root.clone());
            }
            Err(error) => messages.push(format!(
                "Cannot read {}: {error}; use Recover notes.",
                record.display()
            )),
        }
    }
    for root in roots {
        match recover_interrupted(directory, &root) {
            Ok(recovered) => messages.extend(recovered),
            Err(error) => messages.push(format!(
                "Recovery for {} needs attention: {error}; use Recover notes.",
                root.display()
            )),
        }
    }
    Ok(messages)
}
