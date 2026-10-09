//! Durable source preimages, distinct from the rebuildable index.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::fs::File;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_AGE: u64 = 30 * 24 * 60 * 60 * 1_000_000;
const MAX_VERSIONS: usize = 20;
const MAX_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct Preimage {
    pub note: PathBuf,
    pub created: u64,
    pub text: String,
    // Recorded before exchange. Pending records and changed displaced bytes are
    // protected even when the canonical save completed before a crash.
    pub pending: bool,
    pub displaced: PathBuf,
    // EXDEV keeps the identified inode beside the vault. Its completed JSON
    // snapshot still participates in retention; unexpected inode/bytes pin both.
    #[serde(default)]
    external_inode: Option<(u64, u64)>,
    #[serde(default)]
    prepared: Option<PathBuf>,
    // Proposed bytes are durable outside the vault before native publication.
    // Cleanup authority binds the staging name to the recorded inode AND bytes.
    #[serde(default)]
    prepared_snapshot: Option<String>,
    #[serde(default)]
    prepared_identity: Option<(u64, u64)>,
    // A completed Windows record owns a durable full-text snapshot in app state.
    // external_inode remains set only while checked vault-side cleanup is pending.
    #[serde(default)]
    snapshot_only: bool,
}

#[derive(Clone)]
pub struct Version {
    pub note: PathBuf,
    pub text: String,
    pub created: u64,
    pub label: String,
    pub protected: bool,
    pub link_move: bool,
}
#[derive(Default)]
pub struct Listing {
    pub versions: Vec<Version>,
    pub warnings: Vec<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
fn directory(drafts: &Path) -> PathBuf {
    drafts.join("source-history")
}
fn persist(path: &Path, value: &Preimage) -> Result<()> {
    let parent = path.parent().context("Missing history folder")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    crate::source_state::persist(path, &serde_json::to_vec(value)?)?;
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()?;
        File::open(parent.parent().context("Missing recovery folder")?)?.sync_all()?;
    }
    Ok(())
}
impl Preimage {
    #[cfg(any(unix, test))]
    pub(crate) fn begin(drafts: &Path, note: &Path, text: &str, backup: &Path) -> Result<PathBuf> {
        let path = directory(drafts).join(format!("{}.json", uuid::Uuid::new_v4()));
        persist(
            &path,
            &Self {
                note: note.to_owned(),
                created: now(),
                text: text.to_owned(),
                pending: true,
                displaced: backup.to_owned(),
                external_inode: None,
                prepared: None,
                prepared_snapshot: None,
                prepared_identity: None,
                snapshot_only: false,
            },
        )?;
        Ok(path)
    }
    #[cfg(windows)]
    pub(crate) fn begin_windows(
        drafts: &Path,
        note: &Path,
        text: &str,
        backup: &Path,
        prepared: &Path,
    ) -> Result<PathBuf> {
        // FileEditor pins the supplied Win32 path before canonicalizing note
        // identity. These may spell the same parent as C:\... and \\?\C:\...
        // (or a short-name alias). Bind both native ancestries before storing
        // canonical recovery paths; never authorize a redirected parent.
        let note_parent = note.parent().context("Missing source parent")?;
        let prepared_directory = crate::windows_files::Directory::open(
            prepared.parent().context("Missing prepared parent")?,
        )?;
        let canonical = crate::windows_files::Directory::open(note_parent)?;
        let backup_directory = crate::windows_files::Directory::open(
            backup.parent().context("Missing preimage parent")?,
        )?;
        ensure!(
            prepared_directory.identities()? == canonical.identities()?
                && backup_directory.identities()? == canonical.identities()?,
            "Prepared recovery parent changed"
        );
        let prepared = note_parent.join(prepared.file_name().context("Missing prepared filename")?);
        let backup = note_parent.join(backup.file_name().context("Missing preimage filename")?);
        ensure!(
            owned_prepared_name(note, &prepared),
            "Invalid prepared recovery identity"
        );
        let (_, bytes, info) = prepared_directory.read(prepared.file_name().unwrap())?;
        let snapshot = String::from_utf8(bytes).context("Prepared source is not UTF-8")?;
        let (volume, high, low) = crate::windows_files::identity(&info);
        let path = directory(drafts).join(format!("{}.json", uuid::Uuid::new_v4()));
        let entry = Self {
            note: note.to_owned(),
            created: now(),
            text: text.to_owned(),
            pending: true,
            displaced: backup,
            external_inode: None,
            prepared: Some(prepared),
            prepared_snapshot: Some(snapshot),
            prepared_identity: Some((u64::from(volume), u64::from(high) << 32 | u64::from(low))),
            snapshot_only: false,
        };
        persist(&path, &entry)?;
        Ok(path)
    }
    /// A failed/interrupted save keeps recovery, but its owned staging inode
    /// need not remain in a synced vault once the exact bytes are durable here.
    #[cfg(windows)]
    pub(crate) fn archive_prepared_windows(path: &Path) -> Result<()> {
        let mut entry = Self::load(path)?;
        let Some(prepared) = entry.prepared.as_ref() else {
            return Ok(());
        };
        let (Some(snapshot), Some(identity)) =
            (entry.prepared_snapshot.as_ref(), entry.prepared_identity)
        else {
            // Older records have no checked inode/snapshot. A path or matching
            // UUID alone is never authority to remove an existing sync copy.
            return Ok(());
        };
        ensure!(
            owned_prepared_name(&entry.note, prepared),
            "Unassigned prepared recovery is protected"
        );
        // undo_created validates regular file, single link, identity and bytes
        // while holding DELETE on that inode; it cannot unlink a racing path.
        match crate::windows_files::undo_created(prepared, identity, Some(snapshot.as_bytes())) {
            Ok(()) => {}
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        entry.prepared = None;
        entry.prepared_identity = None;
        persist(path, &entry)
    }
    #[cfg(windows)]
    pub(crate) fn finish_windows(
        path: &Path,
        directory: &crate::windows_files::Directory,
        expected_identity: (u64, u64),
    ) -> Result<()> {
        let mut entry = Self::load(path)?;
        let (_, bytes, info) = directory.read(
            entry
                .displaced
                .file_name()
                .context("Missing displaced filename")?,
        )?;
        ensure!(
            bytes == entry.text.as_bytes(),
            "Displaced source changed; recovery is protected"
        );
        // The JSON already contains the complete preimage. Persist acknowledgement
        // and the checked cleanup identity before removing any native recovery bytes.
        let (volume, high, low) = crate::windows_files::identity(&info);
        ensure!(
            (u64::from(volume), u64::from(high) << 32 | u64::from(low)) == expected_identity,
            "Displaced source was replaced; recovery is protected"
        );
        entry.external_inode = Some(expected_identity);
        entry.pending = false;
        entry.prepared = None;
        entry.prepared_snapshot = None;
        entry.prepared_identity = None;
        entry.snapshot_only = true;
        persist(path, &entry)?;
        // Cleanup errors retain the verified preimage and do not turn a durable
        // source save into a failure. Startup retries from this owned record.
        let _ = Self::clean_windows(path, &mut entry);
        Ok(())
    }
    #[cfg(windows)]
    fn clean_windows(path: &Path, entry: &mut Self) -> Result<()> {
        ensure!(
            !entry.pending && entry.prepared.is_none(),
            "Interrupted recovery is protected"
        );
        let expected = entry
            .external_inode
            .context("No checked cleanup identity")?;
        ensure!(
            entry.displaced.parent() == entry.note.parent()
                && entry
                    .displaced
                    .file_name()
                    .is_some_and(crate::vault::windows_preimage_name),
            "Unassigned recovery is protected"
        );
        // Legacy acknowledged records become complete app-state snapshots first.
        // Never remove a changed inode, symlink, hard link or changed preimage.
        let parent = crate::windows_files::Directory::open(
            entry
                .displaced
                .parent()
                .context("Missing recovery parent")?,
        )?;
        match parent.read(entry.displaced.file_name().unwrap()) {
            Ok((guard, bytes, info)) => {
                let (volume, high, low) = crate::windows_files::identity(&info);
                ensure!(
                    bytes == entry.text.as_bytes()
                        && (u64::from(volume), u64::from(high) << 32 | u64::from(low)) == expected,
                    "Changed recovery is protected"
                );
                entry.snapshot_only = true;
                persist(path, entry)?;
                drop(guard);
                crate::windows_files::undo_created(
                    &entry.displaced,
                    expected,
                    Some(entry.text.as_bytes()),
                )?;
            }
            Err(error)
                if entry.snapshot_only
                    && error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        entry.external_inode = None;
        persist(path, entry)
    }

    fn load(path: &Path) -> Result<Self> {
        let entry: Self = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            entry.note.is_absolute() && entry.displaced.is_absolute(),
            "Invalid history identity"
        );
        Ok(entry)
    }
    /// Archive the displaced inode through the editor's pinned directory. Its
    /// old absolute name may now resolve to an entirely different directory.
    #[cfg(unix)]
    pub(crate) fn finish_bound(path: &Path, source_directory: &File) -> Result<()> {
        Self::finish_bound_with(path, source_directory, |source, name, target, archived| {
            rustix::fs::renameat(source, name, target, archived).map_err(Into::into)
        })
    }
    #[cfg(unix)]
    fn finish_bound_with(
        path: &Path,
        source_directory: &File,
        rename: impl FnOnce(&File, &std::ffi::OsStr, &File, &std::ffi::OsStr) -> std::io::Result<()>,
    ) -> Result<()> {
        use std::{io::Read, os::unix::fs::MetadataExt};
        let mut entry = Self::load(path)?;
        let name = entry
            .displaced
            .file_name()
            .context("Missing displaced filename")?;
        let mut source = crate::file_editor::open_regular_at(source_directory, name)?;
        let metadata = source.metadata()?;
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes)?;
        ensure!(
            bytes == entry.text.as_bytes(),
            "Displaced source changed; recovery is protected"
        );
        let archive_directory = File::open(path.parent().context("Missing history folder")?)?;
        let archived = path.with_extension("source");
        match rename(
            source_directory,
            name,
            &archive_directory,
            archived.file_name().unwrap(),
        ) {
            Ok(()) => {
                source_directory.sync_all()?;
                archive_directory.sync_all()?;
                entry.displaced = archived;
            }
            Err(error) if error.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) => {
                // Keep the inode and its identity beside the original note.
                // If the folder moved, history remains protected and the JSON
                // still contains the exact preimage, never redirected bytes.
                entry.external_inode = Some((metadata.dev(), metadata.ino()));
            }
            Err(error) => return Err(error.into()),
        }
        entry.pending = false;
        persist(path, &entry)
    }

    #[cfg(all(test, unix))]
    fn finish(path: &Path) -> Result<()> {
        let entry = Self::load(path)?;
        let directory = File::open(entry.displaced.parent().context("Missing source folder")?)?;
        Self::finish_bound(path, &directory)
    }
    #[cfg(all(test, unix))]
    fn finish_with(
        path: &Path,
        rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<()> {
        let entry = Self::load(path)?;
        let directory = File::open(entry.displaced.parent().context("Missing source folder")?)?;
        Self::finish_bound_with(path, &directory, |_, _, _, _| {
            rename(&entry.displaced, &path.with_extension("source"))
        })
    }
}

#[cfg(windows)]
fn owned_prepared_name(note: &Path, prepared: &Path) -> bool {
    prepared.parent() == note.parent()
        && prepared
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(".tessera-save-")?.strip_suffix(".prepared"))
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id))
}

/// A changed inode is recovery, never an ordinary expirable history entry.
fn protected(entry: &Preimage) -> bool {
    if entry.snapshot_only {
        return entry.pending || entry.external_inode.is_some();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        entry.pending
            || fs::read(&entry.displaced).map_or(true, |b| b != entry.text.as_bytes())
            || entry.external_inode.is_some_and(|identity| {
                fs::symlink_metadata(&entry.displaced).map_or(true, |m| {
                    !m.is_file() || m.nlink() != 1 || (m.dev(), m.ino()) != identity
                })
            })
    }
    #[cfg(windows)]
    {
        entry.pending
            || crate::windows_files::read_file(&entry.displaced).map_or(true, |(_, bytes, info)| {
                let (volume, high, low) = crate::windows_files::identity(&info);
                bytes != entry.text.as_bytes()
                    || entry.external_inode.is_some_and(|expected| {
                        expected != (u64::from(volume), u64::from(high) << 32 | u64::from(low))
                    })
            })
    }
}
fn owned_displaced(record: &Path, entry: &Preimage) -> bool {
    if entry.snapshot_only {
        return !entry.pending && entry.external_inode.is_none();
    }
    if entry.external_inode.is_some() {
        entry.displaced.parent() == entry.note.parent()
            && entry
                .displaced
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".tessera-save-"))
    } else {
        entry.displaced == record.with_extension("source")
    }
}

/// Retry acknowledged preimages and snapshot-backed prepared staging for this
/// vault. Interrupted recovery bytes stay protected outside the vault; changed,
/// raced and unassigned synced inodes are never deleted.
#[cfg(windows)]
pub fn cleanup_windows(drafts: &Path, root: &Path) -> Result<Vec<String>> {
    let root = root.canonicalize()?;
    let folder = directory(drafts);
    let mut warnings = vec![];
    if !folder.exists() {
        return Ok(warnings);
    }
    for item in fs::read_dir(folder)? {
        let path = item?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        match Preimage::load(&path) {
            Ok(mut entry) if entry.note.starts_with(&root) => {
                if let Err(error) = Preimage::archive_prepared_windows(&path) {
                    warnings.push(format!("Prepared recovery retained: {error:#}"));
                }
                if !entry.pending && entry.external_inode.is_some() {
                    if let Err(error) = Preimage::clean_windows(&path, &mut entry) {
                        warnings.push(format!("Save recovery retained: {error:#}"));
                    }
                }
            }
            Ok(_) => {}
            Err(error) => warnings.push(format!("Unreadable history retained: {error:#}")),
        }
    }
    Ok(warnings)
}

/// History preview is bounded independently of recovery retention. An oversized
/// prepared file stays on disk and produces a warning instead of an allocation.
#[cfg(any(windows, test))]
fn prepared_text(file: fs::File) -> Result<String> {
    use std::io::Read;
    const TOO_LARGE: &str = "Prepared save is too large to preview; the recovery file is preserved";
    ensure!(file.metadata()?.len() <= MAX_BYTES as u64, TOO_LARGE);
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, TOO_LARGE);
    String::from_utf8(bytes).context("Prepared recovery is not UTF-8")
}

pub fn list(drafts: &Path, root: &Path) -> Result<Listing> {
    let mut result = Listing::default();
    let root = root.canonicalize()?;
    let history = directory(drafts);
    if !history.exists() {
        return Ok(result);
    }
    for item in fs::read_dir(history)? {
        let path = item?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        match Preimage::load(&path) {
            Ok(entry) if entry.note.starts_with(&root) => {
                let is_protected = protected(&entry);
                result.versions.push(Version {
                    note: entry.note.clone(),
                    text: entry.text.clone(),
                    created: entry.created,
                    label: if is_protected {
                        "Protected save recovery"
                    } else if entry.external_inode.is_some() {
                        "Before save · vault-side archive"
                    } else {
                        "Before save"
                    }
                    .into(),
                    protected: is_protected,
                    link_move: false,
                });
                #[cfg(windows)]
                if entry.pending {
                    if let Some(text) = entry.prepared_snapshot.as_ref() {
                        result.versions.push(Version {
                            note: entry.note.clone(),
                            text: text.clone(),
                            created: entry.created,
                            label: "Prepared save — protected".into(),
                            protected: true,
                            link_move: false,
                        });
                    }
                }
                #[cfg(windows)]
                if let Some(prepared) = entry.prepared.as_ref().filter(|path| {
                    path.parent() == entry.note.parent()
                        && path.file_name().is_some_and(|name| {
                            name.to_string_lossy().starts_with(".tessera-save-")
                        })
                }) {
                    let recovered = (|| -> Result<String> {
                        let directory =
                            crate::windows_files::Directory::open(prepared.parent().unwrap())?;
                        prepared_text(directory.open_file(prepared.file_name().unwrap())?)
                    })();
                    match recovered {
                        Ok(text) if entry.prepared_snapshot.as_ref() != Some(&text) => {
                            result.versions.push(Version {
                                note: entry.note.clone(),
                                text,
                                created: entry.created,
                                label: if entry.prepared_snapshot.is_some() {
                                    "Unexpected prepared version — protected"
                                } else {
                                    "Prepared save — protected"
                                }
                                .into(),
                                protected: true,
                                link_move: false,
                            })
                        }
                        Ok(_) => {}
                        Err(error)
                            if error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
                        Err(error) => result
                            .warnings
                            .push(format!("Cannot read prepared recovery: {error:#}")),
                    }
                }
                // A crash after the inode move but before acknowledgement leaves
                // the archived name discoverable without rewriting the record.
                if entry.snapshot_only && entry.external_inode.is_none() {
                    continue;
                }
                let displaced = if entry.displaced.exists() {
                    entry.displaced.clone()
                } else {
                    path.with_extension("source")
                };
                #[cfg(unix)]
                let recovered = fs::read_to_string(&displaced).map_err(anyhow::Error::from);
                #[cfg(windows)]
                let recovered =
                    (|| -> Result<String> {
                        let parent = crate::windows_files::Directory::open(
                            displaced.parent().context("Missing recovery folder")?,
                        )?;
                        prepared_text(parent.open_file(
                            displaced.file_name().context("Missing recovery filename")?,
                        )?)
                    })();
                match recovered {
                    Ok(text) if text != entry.text => result.versions.push(Version {
                        note: entry.note,
                        text,
                        created: entry.created,
                        label: "Unexpected displaced version — protected".into(),
                        protected: true,
                        link_move: false,
                    }),
                    Err(error)
                        if error
                            .downcast_ref::<std::io::Error>()
                            .is_none_or(|e| e.kind() != std::io::ErrorKind::NotFound) =>
                    {
                        result
                            .warnings
                            .push(format!("Cannot read displaced version: {error:#}"))
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(error) => result.warnings.push(format!(
                "Unreadable history {}: {error:#}; retained",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    result
        .versions
        .sort_by_key(|v| std::cmp::Reverse(v.created));
    Ok(result)
}

#[cfg(test)]
mod recovery_preview_tests {
    use super::*;

    #[test]
    fn prepared_snapshot_schema_preserves_exact_bytes_and_legacy_defaults() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let legacy = serde_json::json!({
            "note": root.join("note.md"), "created": 1, "text": "base\r\n",
            "pending": true, "displaced": root.join(".tessera-save-old.previous"),
            "prepared": root.join(".tessera-save-old.prepared")
        });
        let mut entry: Preimage = serde_json::from_value(legacy).unwrap();
        assert!(entry.prepared_snapshot.is_none() && entry.prepared_identity.is_none());
        let proposed = "\u{feff}# Proposed שלום e\u{301}\r\n";
        entry.prepared_snapshot = Some(proposed.into());
        entry.prepared_identity = Some((42, 73));
        let path = root.join("state/source-history/record.json");
        persist(&path, &entry).unwrap();
        let recovered = Preimage::load(&path).unwrap();
        assert_eq!(recovered.prepared_snapshot.as_deref(), Some(proposed));
        assert_eq!(recovered.prepared_identity, Some((42, 73)));
        assert!(recovered.pending);
        assert_eq!(recovered.text, "base\r\n");
    }

    #[test]
    fn windows_editor_prepared_recovery_size_guard_preserves_file() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().canonicalize().unwrap();
        let path = folder.join(".tessera-save-preview");
        let read = || {
            #[cfg(unix)]
            let file = fs::File::open(&path).unwrap();
            #[cfg(windows)]
            let file = crate::windows_files::Directory::open(&folder)
                .unwrap()
                .open_file(path.file_name().unwrap())
                .unwrap();
            prepared_text(file)
        };
        fs::write(&path, "Recovered שלום\r\n").unwrap();
        assert_eq!(
            read().unwrap(),
            "Recovered שלום\r\n",
            "read positive control"
        );
        let oversized = MAX_BYTES as u64 + 1;
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(oversized)
            .unwrap();
        assert!(read()
            .unwrap_err()
            .to_string()
            .contains("too large to preview"));
        assert_eq!(fs::metadata(&path).unwrap().len(), oversized);
        fs::write(&path, "Still recoverable").unwrap();
        assert_eq!(
            read().unwrap(),
            "Still recoverable",
            "file was not deleted or locked"
        );
        fs::write(&path, [0xff]).unwrap();
        assert!(read().unwrap_err().to_string().contains("not UTF-8"));
        assert_eq!(
            fs::read(&path).unwrap(),
            [0xff],
            "invalid bytes are retained"
        );
    }
}

/// Pending/corrupt/racing entries are deliberately excluded from all limits.
pub fn prune(drafts: &Path) -> Result<()> {
    prune_at(drafts, now())
}
fn prune_at(drafts: &Path, now: u64) -> Result<()> {
    let journal_cleanup = prune_clean_journals(drafts, now);
    let folder = directory(drafts);
    if !folder.exists() {
        return journal_cleanup;
    }
    let mut records = vec![];
    for item in fs::read_dir(&folder)? {
        let path = item?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        if let Ok(entry) = Preimage::load(&path) {
            // Native vault-side cleanup uses its checked DELETE handle, never a
            // path-based unlink. Pending/legacy inodes await explicit cleanup.
            #[cfg(windows)]
            if entry.external_inode.is_some() {
                continue;
            }
            // Only prune inodes owned by this record. Never trust an arbitrary
            // path from old/edited metadata as permission to delete a file.
            if !protected(&entry) && owned_displaced(&path, &entry) {
                records.push((path, entry));
            }
        }
    }
    records.sort_by_key(|(_, e)| std::cmp::Reverse(e.created));
    let mut counts = BTreeMap::new();
    let mut total = 0usize;
    for (path, entry) in records {
        let count = counts.entry(entry.note.clone()).or_insert(0usize);
        *count += 1;
        total = total
            .saturating_add(fs::metadata(&path)?.len() as usize)
            .saturating_add(if entry.snapshot_only {
                0
            } else {
                fs::metadata(&entry.displaced)?.len() as usize
            });
        if now.saturating_sub(entry.created) > MAX_AGE || *count > MAX_VERSIONS || total > MAX_BYTES
        {
            if protected(&entry) {
                continue;
            }
            // The JSON contains a full copy. Remove the inode first so a crash
            // still leaves identifiable source bytes in a protected record.
            if !entry.snapshot_only {
                match fs::remove_file(&entry.displaced) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                #[cfg(unix)]
                File::open(
                    entry
                        .displaced
                        .parent()
                        .context("Missing archive directory")?,
                )?
                .sync_all()?;
            }
            fs::remove_file(path)?;
        }
    }
    #[cfg(unix)]
    File::open(folder)?.sync_all()?;
    journal_cleanup
}

#[derive(Deserialize)]
struct RecoveryDraft {
    path: PathBuf,
    base: String,
    text: String,
}

// Acknowledged journals are not unsaved drafts. Expire old ones only while
// holding the same per-note lock used by editors and background draft writers.
fn prune_clean_journals(drafts: &Path, now: u64) -> Result<()> {
    use sha2::{Digest, Sha256};
    if !drafts.exists() {
        return Ok(());
    }
    let mut errors = vec![];
    for item in fs::read_dir(drafts)? {
        let path = item?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let result = (|| -> Result<()> {
            let _lock = match crate::file_editor::EditorLock::acquire(&path.with_extension("lock"))
            {
                Ok(lock) => lock,
                Err(error) if crate::file_editor::lock_busy(&error) => return Ok(()),
                Err(error) => return Err(error),
            };
            let Ok(bytes) = fs::read(&path) else {
                return Ok(());
            };
            let Ok(draft) = serde_json::from_slice::<RecoveryDraft>(&bytes) else {
                return Ok(());
            };
            let key = format!(
                "{:x}",
                Sha256::digest(draft.path.as_os_str().as_encoded_bytes())
            );
            if path.file_stem().is_none_or(|stem| stem != key.as_str()) || draft.text != draft.base
            {
                return Ok(());
            }
            let age = now.saturating_sub(
                fs::metadata(&path)?
                    .modified()?
                    .duration_since(UNIX_EPOCH)?
                    .as_micros() as u64,
            );
            if age > MAX_AGE {
                fs::remove_file(path)?;
                #[cfg(unix)]
                File::open(drafts)?.sync_all()?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            errors.push(error.to_string());
        }
    }
    ensure!(
        errors.is_empty(),
        "Some acknowledged journals were retained: {}",
        errors.join("; ")
    );
    Ok(())
}

/// Includes moved/deleted note drafts without canonicalizing the missing path.
pub fn drafts(drafts: &Path, root: &Path) -> Result<Listing> {
    let mut result = Listing::default();
    let root = root.canonicalize()?;
    if !drafts.exists() {
        return Ok(result);
    }
    for item in fs::read_dir(drafts)? {
        let path = item?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let loaded =
            (|| -> Result<_> { Ok(serde_json::from_slice::<RecoveryDraft>(&fs::read(&path)?)?) })();
        match loaded {
            Ok(draft)
                if draft.path.starts_with(&root)
                    && draft.text != draft.base
                    && fs::read_to_string(&draft.path).map_or(true, |disk| disk != draft.text) =>
            {
                let created = fs::metadata(&path)?
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;
                result.versions.push(Version {
                    note: draft.path,
                    text: draft.text,
                    created,
                    label: "Unsaved draft — protected".into(),
                    protected: true,
                    link_move: false,
                });
            }
            Ok(_) => {}
            Err(error) => result.warnings.push(format!(
                "Unreadable draft {}: {error:#}; retained",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    Ok(result)
}

/// Link-move history and whole-operation rollback read identical preimages.
pub fn move_preimage(operation: &crate::link_rewrite::Operation, original: &str) -> Result<String> {
    Ok(operation
        .files
        .get(original)
        .context("Missing operation preimage")?
        .before
        .clone())
}

pub fn move_versions(state: &Path, root: &Path) -> Result<Listing> {
    let operations = crate::link_rewrite::Operation::list(state, root)?;
    let mut listing = Listing {
        versions: vec![],
        warnings: operations.warnings,
    };
    for path in operations.operations {
        let op = crate::link_rewrite::Operation::load(&path)?;
        let created = fs::metadata(&path)?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        for original in op.files.keys() {
            let mapped = crate::link_rewrite::moved_path(original, &op.from, &op.to);
            let current = if mapped == *original {
                original
            } else if op.complete
                || (!op.root.join(original).exists() && op.root.join(&op.to).exists())
            {
                &mapped
            } else if op.root.join(original).exists() && !op.root.join(&op.to).exists() {
                original
            } else {
                listing.warnings.push(format!("The interrupted move {} → {} has ambiguous paths. Use Recover link moves; no note history was guessed.", op.from, op.to));
                continue;
            };
            listing.versions.push(Version {
                note: op.root.join(current),
                text: move_preimage(&op, original)?,
                created,
                label: format!("Before link move: {} → {}", op.from, op.to),
                protected: !op.complete,
                link_move: true,
            });
        }
    }
    Ok(listing)
}

/// The manifest owns the entire group: no member is expired separately.
pub fn prune_moves(state: &Path, root: &Path) -> Result<()> {
    prune_moves_at(state, root, now())
}
fn prune_moves_at(state: &Path, root: &Path, now: u64) -> Result<()> {
    let root = root.canonicalize()?;
    let directory = state.join("link-moves");
    if !directory.exists() {
        return Ok(());
    }
    for item in fs::read_dir(directory)? {
        let path = item?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(_guard) = crate::link_rewrite::Operation::lock(&path) else {
            continue;
        };
        let Ok(op) = crate::link_rewrite::Operation::load(&path) else {
            continue;
        };
        let modified = fs::metadata(&path)?
            .modified()?
            .duration_since(UNIX_EPOCH)?
            .as_micros() as u64;
        if op.root == root && (op.complete || op.reverted) && now.saturating_sub(modified) > MAX_AGE
        {
            fs::remove_file(&path)?;
            #[cfg(unix)]
            File::open(path.parent().unwrap())?.sync_all()?;
        }
    }
    Ok(())
}

pub fn restore(
    editor: &mut crate::file_editor::FileEditor,
    reviewed: &str,
    text: &str,
) -> Result<()> {
    ensure!(
        !editor.dirty(),
        "Save or discard unsaved edits before restoring a version"
    );
    ensure!(
        editor.current()? == reviewed,
        "The note changed after preview; open history again"
    );
    // Refresh the exact conflict baseline only after checking the preview.
    editor.refresh_from_disk()?;
    ensure!(
        editor.current()? == reviewed && editor.text() == reviewed,
        "The note changed during restore"
    );
    editor.set_text(text.to_owned())?;
    ensure!(
        editor.save()? == crate::file_editor::Save::Saved,
        "The note changed during restore; recovered text is protected as a draft"
    );
    Ok(())
}

pub fn save_copy(root: &Path, relative: &Path, drafts: &Path, text: &str) -> Result<()> {
    let _guard = crate::file_editor::FileEditor::reserve_destination(&root.join(relative), drafts)?;
    crate::note_files::create_with_source(root, relative, text.as_bytes())
}

/// Old releases did not record which note a displaced inode belonged to.
/// Offer exact copies, but never guess its owner or delete an unassigned file.
pub fn legacy_preimages(drafts: &Path, root: &Path) -> Result<Listing> {
    let mut listing = Listing::default();
    let root = root.canonicalize()?;
    // A recorded prepared version is already presented by list(), including
    // changed bytes/warnings. Do not call the same inode "unassigned" as well.
    #[cfg(windows)]
    let assigned: std::collections::HashSet<_> = fs::read_dir(directory(drafts))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|item| Preimage::load(&item.path()).ok())
        .filter(|entry| entry.note.starts_with(&root))
        .filter_map(|entry| {
            entry
                .prepared
                .filter(|p| owned_prepared_name(&entry.note, p))
        })
        .collect();
    #[cfg(not(windows))]
    let _ = drafts;
    for item in walkdir::WalkDir::new(&root).follow_links(false) {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                listing.warnings.push(format!("Recovery scan: {error}"));
                continue;
            }
        };
        if !item.file_type().is_file()
            || !item
                .file_name()
                .to_string_lossy()
                .starts_with(".tessera-save-")
        {
            continue;
        }
        #[cfg(windows)]
        if assigned.contains(item.path()) {
            continue;
        }
        match fs::read_to_string(item.path()) {
            Ok(text) => listing.versions.push(Version {
                note: item.path().to_owned(),
                text,
                created: item
                    .metadata()?
                    .modified()?
                    .duration_since(UNIX_EPOCH)?
                    .as_micros() as u64,
                label: "Unassigned displaced source — save a copy to inspect".into(),
                protected: true,
                link_move: false,
            }),
            Err(error) => listing.warnings.push(format!(
                "Cannot read {}: {error}; retained",
                item.path().display()
            )),
        }
    }
    Ok(listing)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::file_editor::{FileEditor, Save};
    #[test]
    fn alias_paths_discover_canonical_history_and_broken_journal_does_not_block_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("note.md"), "base").unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let state = root.join("drafts");
        let mut editor = FileEditor::open(&alias.join("note.md"), &state).unwrap();
        editor.set_text("new".into()).unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
        drop(editor);
        assert_eq!(list(&state, &alias).unwrap().versions[0].text, "base");
        let record = fs::read_dir(directory(&state))
            .unwrap()
            .flatten()
            .find(|e| e.path().extension().is_some_and(|e| e == "json"))
            .unwrap()
            .path();
        fs::write(state.join("unreadable.json"), "{").unwrap();
        fs::create_dir(state.join("unreadable.lock")).unwrap();
        assert!(prune_at(&state, now() + MAX_AGE + 1000).is_err());
        assert!(
            !record.exists(),
            "failed journal cleanup must not stop normal history expiry"
        );
        assert!(state.join("unreadable.json").exists());
    }

    #[test]
    fn cross_device_fallback_keeps_the_pinned_inode_when_the_parent_is_replaced() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let other = root.path().join("other");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&other).unwrap();
        let backup = original.join(".tessera-save-exdev");
        fs::write(&backup, "base").unwrap();
        fs::write(other.join(".tessera-save-exdev"), "base").unwrap();
        let metadata = fs::metadata(&backup).unwrap();
        let pinned = File::open(&original).unwrap();
        let record = Preimage::begin(
            &root.path().join("state"),
            &original.join("note.md"),
            "base",
            &backup,
        )
        .unwrap();
        Preimage::finish_bound_with(&record, &pinned, |_, _, _, _| {
            fs::rename(&original, root.path().join("moved"))?;
            symlink(&other, &original)?;
            Err(std::io::Error::from_raw_os_error(
                rustix::io::Errno::XDEV.raw_os_error(),
            ))
        })
        .unwrap();
        let entry = Preimage::load(&record).unwrap();
        assert_eq!(entry.external_inode, Some((metadata.dev(), metadata.ino())));
        assert!(
            protected(&entry),
            "redirected path must pin retention, even with matching bytes"
        );
        assert_eq!(
            fs::metadata(root.path().join("moved/.tessera-save-exdev"))
                .unwrap()
                .ino(),
            metadata.ino()
        );
        assert_eq!(
            fs::read_to_string(other.join(".tessera-save-exdev")).unwrap(),
            "base"
        );
    }

    #[test]
    fn cross_filesystem_history_is_bounded_and_changed_inodes_remain_protected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join("drafts");
        fs::create_dir_all(&state).unwrap();
        let note = root.join("note.md");
        let backup = root.join(".tessera-save-xdev");
        fs::write(&backup, "before").unwrap();
        let record = Preimage::begin(&state, &note, "before", &backup).unwrap();
        Preimage::finish_with(&record, |_, _| {
            Err(std::io::Error::from_raw_os_error(
                rustix::io::Errno::XDEV.raw_os_error(),
            ))
        })
        .unwrap();
        assert!(!Preimage::load(&record).unwrap().pending);
        assert!(!list(&state, &root).unwrap().versions[0].protected);
        fs::write(&backup, "late external bytes").unwrap();
        prune_at(&state, now() + MAX_AGE + 1).unwrap();
        assert!(record.exists());
        assert_eq!(fs::read_to_string(&backup).unwrap(), "late external bytes");
        fs::write(&backup, "before").unwrap();
        prune_at(&state, now() + MAX_AGE + 1).unwrap();
        assert!(!record.exists());
        assert!(!backup.exists());
    }

    #[test]
    fn missing_archive_is_protected_without_blocking_other_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join("drafts");
        fs::create_dir_all(&state).unwrap();
        let mut records = vec![];
        for name in ["missing", "normal"] {
            let backup = root.join(format!(".tessera-save-{name}"));
            fs::write(&backup, name).unwrap();
            let record = Preimage::begin(&state, &root.join("note.md"), name, &backup).unwrap();
            Preimage::finish(&record).unwrap();
            records.push(record);
        }
        fs::remove_file(records[0].with_extension("source")).unwrap();
        prune_at(&state, now() + MAX_AGE + 1).unwrap();
        assert!(records[0].exists());
        assert!(!records[1].exists());
        assert_eq!(list(&state, &root).unwrap().versions[0].text, "missing");
    }

    #[test]
    fn acknowledged_drafts_expire_but_active_and_dirty_journals_survive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join("drafts");
        fs::write(root.join("note.md"), "base").unwrap();
        let mut editor = FileEditor::open(&root.join("note.md"), &state).unwrap();
        editor.set_text("saved".into()).unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
        let journal = fs::read_dir(&state)
            .unwrap()
            .flatten()
            .find(|e| e.path().extension().is_some_and(|e| e == "json"))
            .unwrap()
            .path();
        // Each write replaces the journal and its mtime. Derive the injected
        // cleanup clock from that revision, never from how fast the test runs.
        let expired_time = || {
            fs::metadata(&journal)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64
                + MAX_AGE
                + 1
        };
        prune_at(&state, expired_time()).unwrap();
        let journals = || {
            fs::read_dir(&state)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|e| e == "json"))
                .count()
        };
        assert_eq!(
            journals(),
            1,
            "active editor holds its acknowledged journal"
        );
        editor.set_text("unsaved".into()).unwrap();
        drop(editor);
        prune_at(&state, expired_time()).unwrap();
        assert_eq!(journals(), 1, "unsaved drafts never age out");
        let mut editor = FileEditor::open(&root.join("note.md"), &state).unwrap();
        editor.reload().unwrap();
        drop(editor);
        prune_at(&state, expired_time()).unwrap();
        assert_eq!(journals(), 0, "inactive acknowledged journals expire");
    }

    #[test]
    fn move_retention_keeps_partial_groups_and_uses_the_rollback_preimage() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join("state");
        fs::create_dir_all(state.join("link-moves")).unwrap();
        for (name, complete) in [("done", true), ("partial", false)] {
            let value = serde_json::json!({"root":root,"from":"old.md","to":"new.md",
                "files":{"old.md":{"before":"original 🧠", "after":"new"}},
                "complete":complete,"reverted":false});
            fs::write(
                state.join("link-moves").join(format!("{name}.json")),
                value.to_string(),
            )
            .unwrap();
        }
        fs::write(root.join("old.md"), "new note at old name").unwrap();
        let versions = move_versions(&state, &root).unwrap();
        assert_eq!(versions.versions.len(), 2);
        assert!(versions.versions.iter().all(|v| v.link_move));
        assert!(versions
            .versions
            .iter()
            .any(|v| !v.protected && v.note == root.join("new.md")));
        assert!(versions.versions.iter().all(|v| v.text == "original 🧠"));
        let lock =
            crate::link_rewrite::Operation::lock(&state.join("link-moves/done.json")).unwrap();
        prune_moves_at(&state, &root, now() + MAX_AGE + 1000).unwrap();
        assert!(
            state.join("link-moves/done.json").exists(),
            "active revert cannot expire"
        );
        drop(lock);
        prune_moves_at(&state, &root, now() + MAX_AGE + 1000).unwrap();
        assert!(!state.join("link-moves/done.json").exists());
        assert!(state.join("link-moves/partial.json").exists());
    }

    #[test]
    fn exact_history_restore_conflict_and_restore_itself_is_versioned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let note = root.join("Привет 🧠.md");
        let original = "\u{feff}---\r\ntitle: e\u{301}\r\n---\r\n[[Заметка]]\r\n";
        fs::write(&note, original).unwrap();
        let drafts = root.join("state/editor-drafts");
        let mut editor = FileEditor::open(&note, &drafts).unwrap();
        editor.set_text("new version".into()).unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
        let listing = list(&drafts, &root).unwrap();
        assert_eq!(listing.versions.len(), 1);
        assert_eq!(listing.versions[0].text.as_bytes(), original.as_bytes());
        assert!(!listing.versions[0].protected);
        assert!(!fs::read_dir(&root).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with(".tessera-save-")));
        fs::write(&note, "external").unwrap();
        assert!(restore(&mut editor, "new version", original).is_err());
        assert_eq!(fs::read_to_string(&note).unwrap(), "external");
        restore(&mut editor, "external", original).unwrap();
        assert_eq!(fs::read(&note).unwrap(), original.as_bytes());
        assert!(list(&drafts, &root)
            .unwrap()
            .versions
            .iter()
            .any(|v| v.text == "external"));
        editor.set_text("dirty".into()).unwrap();
        assert!(restore(&mut editor, original, "replacement").is_err());
        assert_eq!(editor.text(), "dirty");
    }

    #[test]
    fn moved_deleted_drafts_recover_as_complete_exclusive_copies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let note = root.join("old.md");
        fs::write(&note, "base").unwrap();
        let state = root.join("state/editor-drafts");
        let mut editor = FileEditor::open(&note, &state).unwrap();
        let text = "\u{feff}несохранённое 🧠e\u{301}\r\n";
        editor.set_text(text.into()).unwrap();
        drop(editor);
        fs::rename(&note, root.join("moved.md")).unwrap();
        let listing = drafts(&state, &root).unwrap();
        assert_eq!(listing.versions.len(), 1);
        save_copy(
            &root,
            Path::new("Recovered.md"),
            &state,
            &listing.versions[0].text,
        )
        .unwrap();
        assert_eq!(
            fs::read(root.join("Recovered.md")).unwrap(),
            text.as_bytes()
        );
        assert!(save_copy(&root, Path::new("Recovered.md"), &state, "bad").is_err());
        assert!(
            save_copy(&root, Path::new("old.md"), &state, "bad").is_err(),
            "orphaned draft prevents destination reuse"
        );
        assert!(save_copy(&root, Path::new("../outside.md"), &state, text).is_err());
        assert_eq!(drafts(&state, &root).unwrap().versions[0].text, text);
    }

    #[test]
    fn retention_limits_normal_versions_but_protects_incomplete_and_racing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join("drafts");
        fs::create_dir_all(&state).unwrap();
        let note = root.join("note.md");
        fs::write(&note, "note").unwrap();
        let mut records = vec![];
        for n in 0..25 {
            let backup = root.join(format!(".tessera-save-{n}"));
            fs::write(&backup, format!("version {n}")).unwrap();
            let record = Preimage::begin(&state, &note, &format!("version {n}"), &backup).unwrap();
            Preimage::finish(&record).unwrap();
            records.push(record);
        }
        let pending_backup = root.join(".tessera-save-pending");
        fs::write(&pending_backup, "possible proposed bytes").unwrap();
        let pending = Preimage::begin(&state, &note, "protected base", &pending_backup).unwrap();
        let racing = Preimage::load(&records[0]).unwrap();
        fs::write(&racing.displaced, "late external write").unwrap();
        fs::write(directory(&state).join("corrupt.json"), "{").unwrap();
        prune(&state).unwrap();
        assert_eq!(
            list(&state, &root)
                .unwrap()
                .versions
                .iter()
                .filter(|v| !v.protected)
                .count(),
            MAX_VERSIONS
        );
        prune_at(&state, now() + MAX_AGE + 1).unwrap();
        let listing = list(&state, &root).unwrap();
        assert_eq!(listing.versions.len(), 4); // two protected base/displaced pairs
        assert!(listing
            .versions
            .iter()
            .any(|v| v.text == "late external write"));
        assert!(pending.exists());
        assert!(directory(&state).join("corrupt.json").exists());
        assert_eq!(listing.warnings.len(), 1);
    }

    #[test]
    fn interrupted_archive_keeps_both_base_and_displaced_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let drafts = root.join("drafts");
        fs::create_dir_all(&drafts).unwrap();
        let note = root.join("note.md");
        let backup = root.join(".tessera-save-interrupted");
        fs::write(&backup, "unexpected").unwrap();
        let record = Preimage::begin(&drafts, &note, "base", &backup).unwrap();
        fs::rename(&backup, record.with_extension("source")).unwrap();
        prune_at(&drafts, now() + MAX_AGE + 1).unwrap();
        let listing = list(&drafts, &root).unwrap();
        assert_eq!(listing.versions.len(), 2);
        assert!(listing.versions.iter().all(|v| v.protected));
        assert!(listing.versions.iter().any(|v| v.text == "unexpected"));
    }
}

#[cfg(all(test, windows))]
mod windows_cleanup_tests {
    use super::*;
    use crate::{
        file_editor::{FileEditor, Save},
        windows_files::{Directory, Replacement},
    };
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let drafts = temp.path().join("state/editor-drafts");
        (temp, root, drafts)
    }
    // Simulate a historical completed save or a crash between acknowledgement
    // and cleanup. Native publication is real; only the durable marker is staged.
    fn staged(
        root: &Path,
        drafts: &Path,
        name: &str,
        completed: bool,
        snapshot: bool,
    ) -> (PathBuf, PathBuf) {
        let note = root.join(name);
        fs::write(&note, "preimage שלום\r\n").unwrap();
        let dir = Directory::open(root).unwrap();
        let plan = dir
            .prepare_replace(
                note.file_name().unwrap(),
                b"preimage \xd7\xa9\xd7\x9c\xd7\x95\xd7\x9d\r\n",
                b"saved\r\n",
            )
            .unwrap()
            .unwrap();
        let backup = plan.preimage_path().to_owned();
        let record = Preimage::begin_windows(
            drafts,
            &note,
            "preimage שלום\r\n",
            &backup,
            plan.prepared_path(),
        )
        .unwrap();
        assert!(matches!(plan.commit().unwrap(), Replacement::Saved { .. }));
        if completed {
            let (_, _, info) = dir.read(backup.file_name().unwrap()).unwrap();
            let (v, h, l) = crate::windows_files::identity(&info);
            let mut entry = Preimage::load(&record).unwrap();
            entry.pending = false;
            entry.prepared = None;
            entry.prepared_snapshot = None;
            entry.prepared_identity = None;
            entry.external_inode = Some((u64::from(v), u64::from(h) << 32 | u64::from(l)));
            entry.snapshot_only = snapshot;
            persist(&record, &entry).unwrap();
        }
        (record, backup)
    }
    fn prepared(root: &Path, drafts: &Path, name: &str) -> (PathBuf, PathBuf) {
        let note = root.join(name);
        fs::write(&note, "base\r\n").unwrap();
        let directory = Directory::open(root).unwrap();
        let plan = directory
            .prepare_replace(
                note.file_name().unwrap(),
                b"base\r\n",
                "proposed שלום\r\n".as_bytes(),
            )
            .unwrap()
            .unwrap();
        let prepared = plan.prepared_path().to_owned();
        let record =
            Preimage::begin_windows(drafts, &note, "base\r\n", plan.preimage_path(), &prepared)
                .unwrap();
        assert_eq!(
            Preimage::load(&record)
                .unwrap()
                .prepared_snapshot
                .as_deref(),
            Some("proposed שלום\r\n")
        );
        (record, prepared)
    }
    #[test]
    fn windows_history_prepared_win32_and_verbatim_parent_aliases_share_cleanup_identity() {
        let (_temp, root, drafts) = fixture();
        let ordinary = PathBuf::from(
            root.to_str()
                .unwrap()
                .strip_prefix("\\\\?\\")
                .expect("canonical Windows prefix positive control"),
        );
        assert_ne!(ordinary, root, "fixture exercises different path spellings");
        let note = root.join("Alias.md");
        fs::write(&note, "base").unwrap();
        let directory = Directory::open(&ordinary).unwrap();
        let plan = directory
            .prepare_replace(std::ffi::OsStr::new("Alias.md"), b"base", b"proposed")
            .unwrap()
            .unwrap();
        let staging = plan.prepared_path().to_owned();
        assert_ne!(staging.parent(), note.parent());
        let record =
            Preimage::begin_windows(&drafts, &note, "base", plan.preimage_path(), &staging)
                .unwrap();
        let entry = Preimage::load(&record).unwrap();
        assert_eq!(entry.prepared.as_ref().unwrap().parent(), note.parent());
        assert_eq!(entry.displaced.parent(), note.parent());
        drop(plan);
        Preimage::archive_prepared_windows(&record).unwrap();
        assert!(!staging.exists());
        assert!(list(&drafts, &ordinary)
            .unwrap()
            .versions
            .iter()
            .any(|v| v.text == "proposed" && v.protected));
        assert_eq!(fs::read_to_string(&note).unwrap(), "base");
    }

    #[test]
    fn windows_history_prepared_crash_recovery_moves_to_app_state_without_losing_bytes() {
        let (_temp, root, drafts) = fixture();
        let (record, prepared) = prepared(&root, &drafts, "Crash.md");
        assert!(prepared.exists(), "crash leaves a real staging inode");
        assert!(
            legacy_preimages(&drafts, &root)
                .unwrap()
                .versions
                .is_empty(),
            "assigned staging is not an unassigned duplicate"
        );
        assert!(cleanup_windows(&drafts, &root).unwrap().is_empty());
        assert!(!prepared.exists());
        assert!(Preimage::load(&record).unwrap().pending);
        let history = list(&drafts, &root).unwrap();
        assert_eq!(history.versions.len(), 2);
        assert!(history.versions.iter().all(|v| v.protected));
        assert!(history.versions.iter().any(|v| v.text == "base\r\n"));
        assert!(history
            .versions
            .iter()
            .any(|v| v.text == "proposed שלום\r\n" && v.label == "Prepared save — protected"));
        assert_eq!(
            fs::read_to_string(root.join("Crash.md")).unwrap(),
            "base\r\n"
        );
        prune_at(&drafts, now() + MAX_AGE + 1).unwrap();
        assert_eq!(
            list(&drafts, &root).unwrap().versions.len(),
            2,
            "pending snapshots never expire"
        );
        assert!(
            cleanup_windows(&drafts, &root).unwrap().is_empty(),
            "restart is idempotent"
        );
    }
    #[test]
    fn windows_history_prepared_cleanup_refuses_changed_replaced_locked_and_reparse_files() {
        let (_temp, root, drafts) = fixture();
        let (_, changed) = prepared(&root, &drafts, "Changed.md");
        fs::write(&changed, "unexpected proposed bytes").unwrap();
        let (_, replaced) = prepared(&root, &drafts, "Replaced.md");
        let replacement = root.join("replacement.tmp");
        fs::write(&replacement, "proposed שלום\r\n").unwrap();
        fs::rename(&replacement, &replaced).unwrap();
        let (_, locked) = prepared(&root, &drafts, "Locked.md");
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&locked)
            .unwrap();
        let (_, reparse) = prepared(&root, &drafts, "Reparse.md");
        let outside = root.parent().unwrap().join("outside.txt");
        fs::write(&outside, "outside — not recovery").unwrap();
        fs::remove_file(&reparse).unwrap();
        std::os::windows::fs::symlink_file(&outside, &reparse)
            .expect("native reparse positive control");
        assert_eq!(cleanup_windows(&drafts, &root).unwrap().len(), 4);
        assert_eq!(
            fs::read_to_string(&changed).unwrap(),
            "unexpected proposed bytes"
        );
        assert!(replaced.exists() && locked.exists());
        assert!(fs::symlink_metadata(&reparse)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "outside — not recovery"
        );
        let history = list(&drafts, &root).unwrap();
        assert!(history
            .versions
            .iter()
            .any(|v| v.text == "unexpected proposed bytes"
                && v.label == "Unexpected prepared version — protected"));
        assert_eq!(
            history
                .versions
                .iter()
                .filter(|v| v.label == "Prepared save — protected" && v.text == "proposed שלום\r\n")
                .count(),
            4
        );
        assert!(!history
            .versions
            .iter()
            .any(|v| v.text.contains("outside —")));
        assert!(legacy_preimages(&drafts, &root)
            .unwrap()
            .versions
            .is_empty());
        drop(lock);
        assert_eq!(cleanup_windows(&drafts, &root).unwrap().len(), 3);
        assert!(!locked.exists());
    }
    #[test]
    fn windows_history_prepared_legacy_other_vault_and_replaced_parent_are_not_cleaned() {
        let (_temp, root, drafts) = fixture();
        let (record, legacy) = prepared(&root, &drafts, "Legacy.md");
        let mut entry = Preimage::load(&record).unwrap();
        entry.prepared_identity = None;
        entry.prepared_snapshot = None;
        persist(&record, &entry).unwrap();
        let unknown = root.join(format!(".tessera-save-{}.prepared", uuid::Uuid::new_v4()));
        fs::write(&unknown, "unassigned synced bytes").unwrap();
        let (_, owned) = prepared(&root, &drafts, "Other.md");
        let other = root.parent().unwrap().join("other-vault");
        fs::create_dir(&other).unwrap();
        assert!(cleanup_windows(&drafts, &other).unwrap().is_empty());
        assert!(owned.exists());
        let nested = root.join("nested");
        fs::create_dir(&nested).unwrap();
        let (_, old_prepared) = prepared(&nested, &drafts, "Parent.md");
        let moved = root.join("moved");
        fs::rename(&nested, &moved).unwrap();
        fs::create_dir(&nested).unwrap();
        fs::write(&old_prepared, "proposed שלום\r\n").unwrap();
        assert_eq!(cleanup_windows(&drafts, &root).unwrap().len(), 1);
        assert_eq!(
            fs::read_to_string(&old_prepared).unwrap(),
            "proposed שלום\r\n"
        );
        assert!(moved.join(old_prepared.file_name().unwrap()).exists());
        assert!(legacy.exists() && unknown.exists());
        assert!(!owned.exists(), "owned staging positive control");
        let legacy_listing = legacy_preimages(&drafts, &root).unwrap();
        assert!(legacy_listing
            .versions
            .iter()
            .any(|v| v.text == "unassigned synced bytes"));
        assert_eq!(
            list(&drafts, &root)
                .unwrap()
                .versions
                .iter()
                .filter(|v| v.text == "proposed שלום\r\n")
                .count(),
            3
        );
    }
    #[test]
    fn windows_history_acknowledgement_refuses_replaced_preimage() {
        let (_temp, root, drafts) = fixture();
        let (record, backup) = staged(&root, &drafts, "Note.md", false, false);
        let dir = Directory::open(&root).unwrap();
        let (_, _, info) = dir.read(backup.file_name().unwrap()).unwrap();
        let (v, h, l) = crate::windows_files::identity(&info);
        let expected = (u64::from(v), u64::from(h) << 32 | u64::from(l));
        let replacement = root.join("another.tmp");
        fs::write(&replacement, "preimage שלום\r\n").unwrap();
        fs::rename(&replacement, &backup).unwrap();
        assert!(Preimage::finish_windows(&record, &dir, expected).is_err());
        assert!(Preimage::load(&record).unwrap().pending);
        assert_eq!(fs::read_to_string(&backup).unwrap(), "preimage שלום\r\n");
        assert!(cleanup_windows(&drafts, &root).unwrap().is_empty());
        assert!(backup.exists());
    }
    #[test]
    fn windows_history_completed_saves_keep_snapshots_without_vault_preimages() {
        let (_temp, root, drafts) = fixture();
        let note = root.join("Note.md");
        fs::write(&note, "base\r\n").unwrap();
        let mut editor = FileEditor::open(&note, &drafts).unwrap();
        for n in 0..25 {
            editor.set_text(format!("revision {n} שלום\r\n")).unwrap();
            assert_eq!(editor.save().unwrap(), Save::Saved);
            assert!(!fs::read_dir(&root)
                .unwrap()
                .any(|e| crate::vault::windows_preimage_name(&e.unwrap().file_name())));
        }
        assert_eq!(fs::read_to_string(&note).unwrap(), "revision 24 שלום\r\n");
        let history = list(&drafts, &root).unwrap();
        assert_eq!(history.versions.len(), MAX_VERSIONS);
        assert!(history.versions.iter().all(|v| !v.protected));
        assert!(history
            .versions
            .iter()
            .any(|v| v.text == "revision 23 שלום\r\n"));
        prune_at(&drafts, now() + MAX_AGE + 1).unwrap();
        assert!(list(&drafts, &root).unwrap().versions.is_empty());
        assert_eq!(fs::read_to_string(&note).unwrap(), "revision 24 שלום\r\n");
    }
    #[test]
    fn windows_history_startup_cleans_acknowledged_and_interrupted_cleanup_only() {
        let (_temp, root, drafts) = fixture();
        let (legacy_record, legacy) = staged(&root, &drafts, "Legacy.md", true, false);
        let (pending_record, pending) = staged(&root, &drafts, "Pending.md", false, false);
        let (missing_record, removed) = staged(&root, &drafts, "Removed.md", true, true);
        let entry = Preimage::load(&missing_record).unwrap();
        crate::windows_files::undo_created(
            &removed,
            entry.external_inode.unwrap(),
            Some(entry.text.as_bytes()),
        )
        .unwrap();
        let unknown = root.join(format!(".tessera-save-{}.previous", uuid::Uuid::new_v4()));
        fs::write(&unknown, "unassigned sync copy").unwrap();
        assert!(cleanup_windows(&drafts, &root).unwrap().is_empty());
        assert!(!legacy.exists());
        assert!(!Preimage::load(&legacy_record).unwrap().pending);
        assert!(Preimage::load(&legacy_record).unwrap().snapshot_only);
        assert!(Preimage::load(&missing_record)
            .unwrap()
            .external_inode
            .is_none());
        assert!(pending.exists() && Preimage::load(&pending_record).unwrap().pending);
        assert_eq!(
            fs::read_to_string(&unknown).unwrap(),
            "unassigned sync copy"
        );
        let history = list(&drafts, &root).unwrap();
        assert_eq!(history.versions.len(), 4);
        assert_eq!(history.versions.iter().filter(|v| v.protected).count(), 2);
        assert!(history
            .versions
            .iter()
            .any(|v| v.text == "saved\r\n" && v.protected));
        assert!(cleanup_windows(&drafts, &root).unwrap().is_empty());
    }
    #[test]
    fn windows_history_cleanup_refuses_changed_replaced_locked_and_reparse_preimages() {
        let (_temp, root, drafts) = fixture();
        let (_, changed) = staged(&root, &drafts, "Changed.md", true, false);
        fs::write(&changed, "unexpected displaced version").unwrap();
        let (_, replaced) = staged(&root, &drafts, "Replaced.md", true, false);
        let replacement = root.join("replacement.tmp");
        fs::write(&replacement, "preimage שלום\r\n").unwrap();
        fs::rename(&replacement, &replaced).unwrap();
        let (_, locked) = staged(&root, &drafts, "Locked.md", true, false);
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&locked)
            .unwrap();
        let (_, reparse) = staged(&root, &drafts, "Reparse.md", true, false);
        let target = root.join("outside.txt");
        fs::write(
            &target,
            "different symlink target — must not become recovery",
        )
        .unwrap();
        fs::remove_file(&reparse).unwrap();
        std::os::windows::fs::symlink_file(&target, &reparse)
            .expect("native symlink positive control");
        let warnings = cleanup_windows(&drafts, &root).unwrap();
        assert_eq!(
            warnings.len(),
            4,
            "each refused cleanup reports retained recovery"
        );
        assert_eq!(
            fs::read_to_string(&changed).unwrap(),
            "unexpected displaced version"
        );
        assert!(replaced.exists() && locked.exists());
        assert!(fs::symlink_metadata(&reparse)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "different symlink target — must not become recovery"
        );
        let history = list(&drafts, &root).unwrap();
        // A changed displaced source contributes BOTH its recorded snapshot and
        // its unexpected bytes. Four cleanup refusals are five recovery versions.
        assert_eq!(history.versions.len(), 5);
        assert!(history.versions.iter().all(|v| v.protected));
        let changed_versions: Vec<_> = history
            .versions
            .iter()
            .filter(|v| v.note == root.join("Changed.md"))
            .collect();
        assert_eq!(changed_versions.len(), 2);
        assert!(changed_versions
            .iter()
            .any(|v| v.text == "preimage שלום\r\n"));
        assert!(changed_versions
            .iter()
            .any(|v| v.text == "unexpected displaced version"
                && v.label == "Unexpected displaced version — protected"));
        for name in ["Replaced.md", "Locked.md", "Reparse.md"] {
            let versions: Vec<_> = history
                .versions
                .iter()
                .filter(|v| v.note == root.join(name))
                .collect();
            assert_eq!(versions.len(), 1, "{name} has its original snapshot only");
            assert_eq!(versions[0].text, "preimage שלום\r\n");
        }
        assert!(!history
            .versions
            .iter()
            .any(|v| v.text.contains("different symlink target")));
        assert!(
            history
                .warnings
                .iter()
                .any(|w| w.starts_with("Cannot read displaced version:")),
            "refused symlink preview must be surfaced, not silently followed"
        );
        drop(lock);
        assert_eq!(cleanup_windows(&drafts, &root).unwrap().len(), 3);
        assert!(!locked.exists());
        assert!(changed.exists() && replaced.exists());
    }
    #[test]
    fn windows_history_unassigned_names_and_other_vault_records_are_never_removed() {
        let (_temp, root, drafts) = fixture();
        let (_, backup) = staged(&root, &drafts, "OtherVault.md", true, false);
        let other = root.parent().unwrap().join("other-vault");
        fs::create_dir(&other).unwrap();
        assert!(cleanup_windows(&drafts, &other).unwrap().is_empty());
        assert!(backup.exists());
        let (record, backup) = staged(&root, &drafts, "ForgedName.md", true, false);
        let arbitrary = root.join("user.previous");
        fs::rename(&backup, &arbitrary).unwrap();
        let mut entry = Preimage::load(&record).unwrap();
        entry.displaced = arbitrary.clone();
        persist(&record, &entry).unwrap();
        assert_eq!(cleanup_windows(&drafts, &root).unwrap().len(), 1);
        assert_eq!(fs::read_to_string(&arbitrary).unwrap(), "preimage שלום\r\n");
        assert!(arbitrary.exists());
    }
}

#[cfg(test)]
mod displaced_listing_tests {
    use super::*;
    #[test]
    fn changed_displaced_version_is_listed_beside_protected_snapshot() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let drafts = fixture.path().join("drafts");
        let note = root.join("Changed.md");
        fs::write(&note, "saved").unwrap();
        let displaced = root.join(format!(".tessera-save-{}.previous", uuid::Uuid::new_v4()));
        fs::write(&displaced, "original snapshot").unwrap();
        Preimage::begin(&drafts, &note, "original snapshot", &displaced).unwrap();
        fs::write(&displaced, "unexpected displaced version").unwrap();
        let listing = list(&drafts, &root).unwrap();
        assert!(listing.warnings.is_empty());
        assert_eq!(listing.versions.len(), 2);
        assert!(listing
            .versions
            .iter()
            .all(|v| v.note == note && v.protected));
        assert!(listing
            .versions
            .iter()
            .any(|v| v.text == "original snapshot"));
        assert!(listing
            .versions
            .iter()
            .any(|v| v.text == "unexpected displaced version"
                && v.label == "Unexpected displaced version — protected"));
        assert_eq!(
            fs::read_to_string(&displaced).unwrap(),
            "unexpected displaced version"
        );
    }
}
