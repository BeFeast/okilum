//! Durable source preimages, distinct from the rebuildable index.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
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
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    File::open(parent.parent().context("Missing recovery folder")?)?.sync_all()?;
    Ok(())
}
impl Preimage {
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
            },
        )?;
        Ok(path)
    }
    fn load(path: &Path) -> Result<Self> {
        let entry: Self = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            entry.note.is_absolute() && entry.displaced.is_absolute(),
            "Invalid history identity"
        );
        Ok(entry)
    }
    pub(crate) fn finish(path: &Path) -> Result<()> {
        Self::finish_with(path, |source, target| fs::rename(source, target))
    }
    fn finish_with(
        path: &Path,
        rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let mut entry = Self::load(path)?;
        ensure!(
            fs::read(&entry.displaced)? == entry.text.as_bytes(),
            "Displaced source changed; recovery is protected"
        );
        let archived = path.with_extension("source");
        // Preserve the inode, including writes through an already-open external
        // descriptor. On EXDEV the durable JSON is the archive copy, while the
        // original inode stays beside the note until its verified retention end.
        match rename(&entry.displaced, &archived) {
            Ok(()) => {
                File::open(entry.displaced.parent().context("Missing source folder")?)?
                    .sync_all()?;
                File::open(archived.parent().unwrap())?.sync_all()?;
                entry.displaced = archived;
            }
            Err(error) if error.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) => {
                let metadata = fs::symlink_metadata(&entry.displaced)?;
                ensure!(
                    metadata.is_file() && metadata.nlink() == 1,
                    "Displaced identity changed; recovery is protected"
                );
                entry.external_inode = Some((metadata.dev(), metadata.ino()));
                ensure!(
                    fs::read(&entry.displaced)? == entry.text.as_bytes(),
                    "Displaced bytes changed; recovery is protected"
                );
            }
            Err(error) => return Err(error.into()),
        }
        entry.pending = false;
        persist(path, &entry)
    }
}

/// A changed inode is recovery, never an ordinary expirable history entry.
fn protected(entry: &Preimage) -> bool {
    use std::os::unix::fs::MetadataExt;
    entry.pending
        || fs::read(&entry.displaced).map_or(true, |b| b != entry.text.as_bytes())
        || entry.external_inode.is_some_and(|identity| {
            fs::symlink_metadata(&entry.displaced).map_or(true, |m| {
                !m.is_file() || m.nlink() != 1 || (m.dev(), m.ino()) != identity
            })
        })
}
fn owned_displaced(record: &Path, entry: &Preimage) -> bool {
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
                // A crash after the inode move but before acknowledgement leaves
                // the archived name discoverable without rewriting the record.
                let displaced = if entry.displaced.exists() {
                    entry.displaced.clone()
                } else {
                    path.with_extension("source")
                };
                match fs::read_to_string(&displaced) {
                    Ok(text) if text != entry.text => result.versions.push(Version {
                        note: entry.note,
                        text,
                        created: entry.created,
                        label: "Unexpected displaced version — protected".into(),
                        protected: true,
                        link_move: false,
                    }),
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => result
                        .warnings
                        .push(format!("Cannot read displaced version: {error}")),
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
            .saturating_add(fs::metadata(&entry.displaced)?.len() as usize);
        if now.saturating_sub(entry.created) > MAX_AGE || *count > MAX_VERSIONS || total > MAX_BYTES
        {
            if protected(&entry) {
                continue;
            }
            // The JSON contains a full copy. Remove the inode first so a crash
            // still leaves identifiable source bytes in a protected record.
            match fs::remove_file(&entry.displaced) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            File::open(
                entry
                    .displaced
                    .parent()
                    .context("Missing archive directory")?,
            )?
            .sync_all()?;
            fs::remove_file(path)?;
        }
    }
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
                Err(error)
                    if error.downcast_ref::<rustix::io::Errno>()
                        == Some(&rustix::io::Errno::WOULDBLOCK) =>
                {
                    return Ok(())
                }
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
pub fn legacy_preimages(root: &Path) -> Result<Listing> {
    let mut listing = Listing::default();
    for item in walkdir::WalkDir::new(root).follow_links(false) {
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

#[cfg(test)]
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
