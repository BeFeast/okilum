//! Exact UTF-8 file editing, independent of the Brain protocol.
//! Drafts live in application state; atomic exchanges retain displaced files next
//! to the note until archived into durable history; racing versions stay protected.
#[cfg(unix)]
mod directory;
#[cfg(windows)]
#[path = "file_editor/directory_windows.rs"]
mod directory;
use anyhow::{bail, Context, Result};
#[cfg(unix)]
pub(crate) use directory::open_regular_at;
use directory::Directory;
#[cfg(unix)]
use rustix::fs::{renameat_with, RenameFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

/// Releases the advisory lock synchronously, even if a fork/dup still holds
/// the same open-file description. Share ownership with Arc, not cloned files.
pub struct EditorLock {
    // On Windows the exclusive sharing mode is held by ownership alone.
    #[cfg_attr(windows, allow(dead_code))]
    file: File,
    #[cfg(unix)]
    owner_process: u32,
}
#[cfg(unix)]
fn flock_retry_interrupted(
    file: &File,
    operation: rustix::fs::FlockOperation,
) -> rustix::io::Result<()> {
    loop {
        let result = rustix::fs::flock(file, operation);
        if result != Err(rustix::io::Errno::INTR) {
            return result;
        }
    }
}
impl EditorLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        #[cfg(windows)]
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path)?;
        #[cfg(unix)]
        flock_retry_interrupted(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)?;
        Ok(Self {
            file,
            #[cfg(unix)]
            owner_process: std::process::id(),
        })
    }
}
impl Drop for EditorLock {
    fn drop(&mut self) {
        // A forked child must not release its parent's live lock when disposing
        // inherited Rust state. The acquiring process owns explicit unlock.
        #[cfg(unix)]
        if self.owner_process == std::process::id() {
            let _ = flock_retry_interrupted(&self.file, rustix::fs::FlockOperation::Unlock);
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Draft {
    path: PathBuf,
    base: String,
    text: String,
}

pub struct FileEditor {
    directory: Directory,
    draft: Draft,
    journal: PathBuf,
    lock: Arc<EditorLock>,
    writes: Arc<JournalWrites>,
}

#[derive(Default)]
struct JournalWrites {
    generation: AtomicU64,
    commit: Mutex<()>,
}

/// A supersedable background journal write. The note lock remains held until
/// every queued write ends; a new session can never race an old journal writer.
pub struct DraftWrite {
    draft: Draft,
    journal: PathBuf,
    writes: Arc<JournalWrites>,
    generation: u64,
    _lock: Arc<EditorLock>,
}
impl DraftWrite {
    pub fn persist(self) -> Result<()> {
        if self.writes.generation.load(Ordering::Acquire) != self.generation {
            return Ok(());
        }
        #[cfg(unix)]
        let parent = self.journal.parent().unwrap();
        #[cfg(unix)]
        {
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(&serde_json::to_vec(&self.draft)?)?;
            temp.as_file().sync_all()?;
            let _commit = self.writes.commit.lock().unwrap();
            if self.writes.generation.load(Ordering::Acquire) != self.generation {
                return Ok(());
            }
            temp.persist(&self.journal)?;
            File::open(parent)?.sync_all()?;
        }
        #[cfg(windows)]
        {
            let _commit = self.writes.commit.lock().unwrap();
            if self.writes.generation.load(Ordering::Acquire) != self.generation {
                return Ok(());
            }
            crate::source_state::persist(&self.journal, &serde_json::to_vec(&self.draft)?)?;
        }
        Ok(())
    }
}

pub(crate) fn lock_busy(error: &anyhow::Error) -> bool {
    #[cfg(unix)]
    {
        error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::WOULDBLOCK)
    }
    #[cfg(windows)]
    {
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| matches!(e.raw_os_error(), Some(32 | 33)))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Save {
    Saved,
    Conflict,
}

impl FileEditor {
    /// Read-only recovery discovery: never acquire an editor or rewrite a draft.
    pub fn has_unsaved_draft(path: &Path, state: &Path) -> Result<bool> {
        let path = path.canonicalize()?;
        let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let bytes = match fs::read(state.join(format!("{key}.json"))) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let draft: Draft = serde_json::from_slice(&bytes)?;
        Ok(draft.path == path
            && draft.text != draft.base
            && draft.text != fs::read_to_string(path)?)
    }

    /// Reserve a not-yet-existing destination without adopting an old draft.
    /// The caller holds this lock through rename, then releases it before opening
    /// the moved note in a new editor. No journal is deleted or rewritten.
    pub fn reserve_destination(path: &Path, state: &Path) -> Result<EditorLock> {
        let path = path
            .parent()
            .context("Destination has no folder")?
            .canonicalize()?
            .join(path.file_name().context("Destination has no filename")?);
        Self::reserve_path(path, state)
    }

    /// Reserve a descendant of a directory destination which does not exist yet.
    /// The directory move validates its real destination parent separately.
    pub(crate) fn reserve_future_destination(
        root: &Path,
        relative: &Path,
        state: &Path,
    ) -> Result<EditorLock> {
        anyhow::ensure!(
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Invalid future destination"
        );
        Self::reserve_path(root.canonicalize()?.join(relative), state)
    }

    fn reserve_path(path: PathBuf, state: &Path) -> Result<EditorLock> {
        fs::create_dir_all(state)?;
        let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let lock = EditorLock::acquire(&state.join(format!("{key}.lock")))
            .context("The destination has an active editor or recovery writer")?;
        match fs::read(state.join(format!("{key}.json"))) {
            Ok(bytes) => {
                let draft: Draft = serde_json::from_slice(&bytes)?;
                if draft.path != path || draft.text != draft.base {
                    bail!("The destination has a saved recovery draft. Choose another name; the draft has been preserved");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(lock)
    }

    pub fn open(path: &Path, state: &Path) -> Result<Self> {
        #[cfg(unix)]
        let meta = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        if !meta.is_file() || meta.nlink() != 1 {
            bail!("Editing requires a regular file without symlinks or hard links");
        }
        // Pin the original Windows path before canonicalization can resolve a
        // junction or symlink. Canonicalization must never authorize redirection.
        #[cfg(windows)]
        let directory = Directory::open(path.parent().context("Missing source folder")?)?;
        #[cfg(windows)]
        let (_, original) = directory.read(path.file_name().context("Missing source filename")?)?;
        let path = path.canonicalize()?;
        #[cfg(unix)]
        let directory = Directory::open(path.parent().context("Missing source folder")?)?;
        fs::create_dir_all(state)?;
        let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let lock = EditorLock::acquire(&state.join(format!("{key}.lock")))
            .context("This note is already being edited in another window")?;
        let journal = state.join(format!("{key}.json"));
        let (base, opened) = directory.read(path.file_name().unwrap())?;
        #[cfg(unix)]
        anyhow::ensure!(
            (meta.dev(), meta.ino()) == (opened.dev(), opened.ino()),
            "The source file changed while opening; reopen it"
        );
        #[cfg(windows)]
        anyhow::ensure!(
            original == opened,
            "The source file changed while opening; reopen it"
        );
        directory.validate(path.parent().unwrap())?;
        let draft = if journal.exists() {
            let draft: Draft = serde_json::from_slice(&fs::read(&journal)?)?;
            if draft.path != path {
                bail!("Recovery draft belongs to another file");
            }
            if draft.text == draft.base {
                Draft {
                    path,
                    text: base.clone(),
                    base,
                }
            } else {
                draft
            }
        } else {
            Draft {
                path,
                text: base.clone(),
                base,
            }
        };
        Ok(Self {
            directory,
            draft,
            journal,
            lock: Arc::new(lock),
            writes: Arc::default(),
        })
    }
    /// Canonical identity held by this editor, for root-bound mutation callers.
    pub fn path(&self) -> &Path {
        &self.draft.path
    }
    pub fn text(&self) -> &str {
        &self.draft.text
    }
    pub fn dirty(&self) -> bool {
        self.draft.text != self.draft.base
    }
    pub fn current(&self) -> Result<String> {
        self.directory.validate(self.draft.path.parent().unwrap())?;
        Ok(self.directory.read(self.draft.path.file_name().unwrap())?.0)
    }
    /// Update the UI-owned snapshot without performing filesystem I/O.
    pub fn queue_text(&mut self, text: String) -> DraftWrite {
        self.draft.text = text;
        self.pending_write()
    }
    pub fn set_text(&mut self, text: String) -> Result<()> {
        self.queue_text(text).persist()
    }
    fn pending_write(&self) -> DraftWrite {
        DraftWrite {
            draft: self.draft.clone(),
            journal: self.journal.clone(),
            writes: self.writes.clone(),
            generation: self.writes.generation.fetch_add(1, Ordering::AcqRel) + 1,
            _lock: self.lock.clone(),
        }
    }
    fn persist(&self) -> Result<()> {
        self.pending_write().persist()
    }
    /// Refresh only a clean buffer. Dirty source is never replaced by a watcher.
    pub fn refresh_from_disk(&mut self) -> Result<Save> {
        let current = self.current()?;
        if self.dirty() {
            return Ok(
                if current == self.draft.base || current == self.draft.text {
                    Save::Saved
                } else {
                    Save::Conflict
                },
            );
        }
        if current != self.draft.base {
            let mut write = self.pending_write();
            write.draft.base = current.clone();
            write.draft.text = current.clone();
            // Keep the conflict baseline paired with the visible source until
            // recovery persistence succeeds. A failed refresh must not authorize
            // saving the old visible text over the external version.
            write.persist()?;
            self.draft.base = current.clone();
            self.draft.text = current;
        }
        Ok(Save::Saved)
    }

    pub fn reload(&mut self) -> Result<()> {
        let current = self.current()?;
        self.draft.base = current.clone();
        self.draft.text = current;
        self.persist()
    }
    /// Explicit Keep mine uses the exact version displayed in the conflict UI.
    pub fn keep_mine(&mut self, reviewed: &str) -> Result<Save> {
        if self.current()? != reviewed {
            return Ok(Save::Conflict);
        }
        self.draft.base = reviewed.to_owned();
        self.save()
    }
    pub fn save(&mut self) -> Result<Save> {
        self.save_before_exchange(|| {})
    }
    #[cfg(unix)]
    fn save_before_exchange(&mut self, before_exchange: impl FnOnce()) -> Result<Save> {
        if !self.dirty() {
            self.directory.validate(self.draft.path.parent().unwrap())?;
            return Ok(Save::Saved);
        }
        self.persist()?;
        self.directory.validate(self.draft.path.parent().unwrap())?;
        let (current, metadata) = self.directory.read(self.draft.path.file_name().unwrap())?;
        if current == self.draft.text {
            self.directory.validate(self.draft.path.parent().unwrap())?;
            self.draft.base = current;
            self.persist()?;
            return Ok(Save::Saved);
        }
        if current != self.draft.base {
            return Ok(Save::Conflict);
        }
        let parent = self.draft.path.parent().unwrap();
        let directory = &self.directory.file;
        let (mut temp, name) = self.directory.temporary()?;
        temp.set_permissions(metadata.permissions())?;
        temp.write_all(self.draft.text.as_bytes())?;
        temp.sync_all()?;
        let backup = parent.join(name);
        directory.sync_all()?;
        let history = crate::source_history::Preimage::begin(
            self.journal.parent().unwrap(),
            &self.draft.path,
            &self.draft.base,
            &backup,
        )?;
        self.directory.validate(parent)?;
        before_exchange();
        renameat_with(
            directory,
            backup.file_name().unwrap(),
            directory,
            self.draft.path.file_name().unwrap(),
            RenameFlags::EXCHANGE,
        )?;
        directory.sync_all()?;
        let displaced =
            open_regular_at(directory, backup.file_name().unwrap()).and_then(|mut file| {
                // A racing writer may not have synced its replacement yet.
                // Protect that inode before either archiving or rolling back.
                file.sync_all()?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok(bytes)
            });
        if !displaced.is_ok_and(|bytes| bytes == self.draft.base.as_bytes()) {
            // A racing replacement is restored, and the displaced version from
            // this second exchange also remains on disk. Never delete either.
            renameat_with(
                directory,
                backup.file_name().unwrap(),
                directory,
                self.draft.path.file_name().unwrap(),
                RenameFlags::EXCHANGE,
            )?;
            directory.sync_all()?;
            return Ok(Save::Conflict);
        }
        crate::source_history::Preimage::finish_bound(&history, directory)?;
        self.directory.validate(parent)?;
        self.draft.base = self.draft.text.clone();
        self.persist()?;
        // Cleanup is best effort; a retention error never reverses a saved note.
        let _ = crate::source_history::prune(self.journal.parent().unwrap());
        Ok(Save::Saved)
    }
    #[cfg(windows)]
    fn save_before_exchange(&mut self, before_exchange: impl FnOnce()) -> Result<Save> {
        if !self.dirty() {
            self.directory.validate(self.draft.path.parent().unwrap())?;
            return Ok(Save::Saved);
        }
        self.persist()?;
        self.directory.validate(self.draft.path.parent().unwrap())?;
        let current = self.current()?;
        if current == self.draft.text {
            self.draft.base = current;
            self.persist()?;
            return Ok(Save::Saved);
        }
        let directory = self.directory.pin()?;
        let Some(plan) = directory.prepare_replace(
            self.draft.path.file_name().unwrap(),
            self.draft.base.as_bytes(),
            self.draft.text.as_bytes(),
        )?
        else {
            return Ok(Save::Conflict);
        };
        let history = crate::source_history::Preimage::begin_windows(
            self.journal.parent().unwrap(),
            &self.draft.path,
            &self.draft.base,
            plan.preimage_path(),
            plan.prepared_path(),
        )?;
        before_exchange();
        match plan.commit()? {
            crate::windows_files::Replacement::Conflict => return Ok(Save::Conflict),
            crate::windows_files::Replacement::Saved { .. } => {}
        }
        crate::source_history::Preimage::finish_windows(&history, &directory)?;
        self.draft.base = self.draft.text.clone();
        self.persist()?;
        let _ = crate::source_history::prune(self.journal.parent().unwrap());
        Ok(Save::Saved)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn replaced_parent_or_ancestor_refuses_save_reload_and_undo_without_losing_drafts() {
        use std::os::unix::fs::symlink;
        for ancestor in [false, true] {
            for redirect in [false, true] {
                for undo in [false, true] {
                    let root = tempfile::tempdir().unwrap();
                    let folder = root.path().join("folder");
                    let sibling = root.path().join("sibling");
                    let relative = if ancestor { "child/note.md" } else { "note.md" };
                    let path = folder.join(relative);
                    let other = sibling.join(relative);
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::create_dir_all(other.parent().unwrap()).unwrap();
                    fs::write(&path, "base").unwrap();
                    let state = root.path().join("state");
                    let mut editor = FileEditor::open(&path, &state).unwrap();
                    if undo {
                        editor.set_text("saved edit".into()).unwrap();
                        assert_eq!(editor.save().unwrap(), Save::Saved);
                    }
                    let baseline = if undo { "saved edit" } else { "base" };
                    let draft = if undo { "base" } else { "unsaved edit 🧠" };
                    editor.set_text(draft.into()).unwrap();
                    fs::write(&other, baseline).unwrap();
                    let moved = root.path().join("moved");
                    fs::rename(&folder, &moved).unwrap();
                    if redirect {
                        symlink(&sibling, &folder).unwrap();
                    } else {
                        fs::create_dir_all(path.parent().unwrap()).unwrap();
                        fs::write(&path, baseline).unwrap();
                    }
                    assert!(editor.current().is_err());
                    assert!(editor.refresh_from_disk().is_err());
                    assert!(editor.reload().is_err());
                    assert!(editor.keep_mine(baseline).is_err());
                    assert!(editor.save().is_err());
                    assert_eq!(editor.text(), draft);
                    assert_eq!(fs::read_to_string(&other).unwrap(), baseline);
                    assert_eq!(fs::read_to_string(&path).unwrap(), baseline);
                    assert_eq!(fs::read_to_string(moved.join(relative)).unwrap(), baseline);
                    if redirect {
                        fs::remove_file(&folder).unwrap();
                    } else {
                        fs::remove_dir_all(&folder).unwrap();
                    }
                    fs::rename(moved, folder).unwrap();
                    drop(editor);
                    let mut recovered = FileEditor::open(&path, &state).unwrap();
                    assert_eq!(recovered.text(), draft);
                    assert_eq!(recovered.save().unwrap(), Save::Saved);
                    assert_eq!(fs::read_to_string(&path).unwrap(), draft);
                }
            }
        }
    }

    #[test]
    fn parent_swap_at_exchange_cannot_redirect_note_or_preimage_archiving() {
        use std::os::unix::fs::symlink;
        for redirect in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let folder = root.path().join("folder");
            let sibling = root.path().join("sibling");
            fs::create_dir(&folder).unwrap();
            fs::create_dir(&sibling).unwrap();
            let path = folder.join("note.md");
            fs::write(&path, "base").unwrap();
            fs::write(sibling.join("note.md"), "base").unwrap();
            let original_inode = fs::metadata(&path).unwrap().ino();
            let state = root.path().join("state");
            let mut editor = FileEditor::open(&path, &state).unwrap();
            editor.set_text("mine".into()).unwrap();
            let moved = root.path().join("moved");
            let result = editor.save_before_exchange(|| {
                // Even after the last identity check, all I/O must stay bound.
                let backup = fs::read_dir(&folder)
                    .unwrap()
                    .flatten()
                    .find(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with(".tessera-save-")
                    })
                    .unwrap()
                    .file_name();
                fs::rename(&folder, &moved).unwrap();
                if redirect {
                    symlink(&sibling, &folder).unwrap();
                } else {
                    fs::create_dir(&folder).unwrap();
                    fs::write(&path, "base").unwrap();
                }
                // A decoy with matching bytes must not be moved into history.
                fs::write(folder.join(&backup), "base").unwrap();
            });
            assert!(
                result.is_err(),
                "stale visible path cannot report a successful save"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), "base");
            assert_eq!(fs::read_to_string(sibling.join("note.md")).unwrap(), "base");
            assert_eq!(fs::read_to_string(moved.join("note.md")).unwrap(), "mine");
            let archived = fs::read_dir(state.join("source-history"))
                .unwrap()
                .flatten()
                .find(|e| e.path().extension().is_some_and(|x| x == "source"))
                .unwrap()
                .path();
            assert_eq!(fs::metadata(&archived).unwrap().ino(), original_inode);
            assert_eq!(fs::read_to_string(archived).unwrap(), "base");
            assert!(fs::read_dir(&folder).unwrap().flatten().any(|e| e
                .file_name()
                .to_string_lossy()
                .starts_with(".tessera-save-")));
            let draft: Draft = serde_json::from_slice(&fs::read(&editor.journal).unwrap()).unwrap();
            assert_eq!(draft.text, "mine");
            assert_eq!(draft.base, "base");
        }
    }

    #[test]
    fn replacing_an_ancestor_is_detected_even_when_the_note_parent_inode_is_retained() {
        let root = tempfile::tempdir().unwrap();
        let ancestor = root.path().join("ancestor");
        let parent = ancestor.join("parent");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join("note.md");
        fs::write(&path, "base").unwrap();
        let mut editor = FileEditor::open(&path, &root.path().join("state")).unwrap();
        editor.set_text("mine".into()).unwrap();
        let old_parent = fs::metadata(&parent).unwrap().ino();
        let moved = root.path().join("moved");
        fs::rename(&ancestor, &moved).unwrap();
        fs::create_dir(&ancestor).unwrap();
        fs::rename(moved.join("parent"), &parent).unwrap();
        assert_eq!(fs::metadata(&parent).unwrap().ino(), old_parent);
        assert!(editor.save().is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "base");
        assert_eq!(editor.text(), "mine");
    }

    #[test]
    fn a_non_utf8_racing_write_is_restored_by_the_bound_exchange() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("note.md");
        fs::write(&path, "base").unwrap();
        let mut editor = FileEditor::open(&path, &root.path().join("state")).unwrap();
        editor.set_text("mine".into()).unwrap();
        assert_eq!(
            editor
                .save_before_exchange(|| fs::write(&path, [0xff]).unwrap())
                .unwrap(),
            Save::Conflict
        );
        assert_eq!(fs::read(path).unwrap(), [0xff]);
        assert!(editor.dirty());
    }

    #[test]
    fn moved_parent_without_replacement_is_refused_and_remains_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("folder");
        fs::create_dir(&folder).unwrap();
        let path = folder.join("note.md");
        fs::write(&path, "base").unwrap();
        let state = root.path().join("state");
        let mut editor = FileEditor::open(&path, &state).unwrap();
        editor.set_text("mine".into()).unwrap();
        let moved = root.path().join("moved");
        fs::rename(&folder, &moved).unwrap();
        assert!(editor.save().is_err());
        assert_eq!(fs::read_to_string(moved.join("note.md")).unwrap(), "base");
        fs::rename(&moved, &folder).unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
        assert_eq!(fs::read_to_string(path).unwrap(), "mine");
    }

    #[test]
    fn lossless_and_recovery() {
        for original in [
            "\u{feff}---\r\ntitle: Привет 🧠\r\n---\r\nbody\r\n".to_owned(),
            "body without newline".into(),
            "大きい 🧠\r\n".repeat(100_000),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("note.md");
            fs::write(&path, &original).unwrap();
            let state = dir.path().join("state");
            let mut editor = FileEditor::open(&path, &state).unwrap();
            assert_eq!(editor.save().unwrap(), Save::Saved);
            assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
            let proposed = original.replacen(
                if original.contains("body") {
                    "body"
                } else {
                    "🧠"
                },
                "edited",
                1,
            );
            editor.set_text(proposed.clone()).unwrap();
            drop(editor);
            let mut recovered = FileEditor::open(&path, &state).unwrap();
            assert_eq!(recovered.text(), proposed);
            assert_eq!(recovered.save().unwrap(), Save::Saved);
            assert_eq!(fs::read(&path).unwrap(), proposed.as_bytes());
        }
    }
    #[test]
    fn conflicts_and_atomic_exchange_preserve_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        fs::write(&path, "base").unwrap();
        let mut editor = FileEditor::open(&path, &dir.path().join("state")).unwrap();
        editor.set_text("mine".into()).unwrap();
        fs::write(&path, "external").unwrap();
        assert_eq!(editor.save().unwrap(), Save::Conflict);
        assert_eq!(fs::read_to_string(&path).unwrap(), "external");
        assert_eq!(editor.keep_mine("base").unwrap(), Save::Conflict);
        assert_eq!(editor.keep_mine("external").unwrap(), Save::Saved);
        // An open handle sees the complete previous inode after atomic exchange.
        let mut old = File::open(&path).unwrap();
        editor.set_text("next".into()).unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut old, &mut text).unwrap();
        assert_eq!(text, "mine");
        editor.set_text("last".into()).unwrap();
        assert_eq!(
            editor
                .save_before_exchange(|| {
                    fs::write(&path, "race").unwrap();
                })
                .unwrap(),
            Save::Conflict
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "race");
        assert!(fs::read_dir(dir.path()).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with(".tessera-save-")
            && fs::read(e.path()).unwrap() == b"last"));
    }
    #[test]
    fn stale_background_writes_cannot_replace_newer_or_saved_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("state");
        fs::write(&path, "base").unwrap();
        let mut editor = FileEditor::open(&path, &state).unwrap();
        let old = editor.queue_text("deleted section".into());
        let latest = editor.queue_text("undo restored section".into());
        latest.persist().unwrap();
        old.persist().unwrap();
        let draft: Draft = serde_json::from_slice(&fs::read(&editor.journal).unwrap()).unwrap();
        assert_eq!(draft.text, "undo restored section");
        let obsolete = editor.queue_text("obsolete".into());
        editor.set_text("saved".into()).unwrap();
        editor.save().unwrap();
        obsolete.persist().unwrap();
        drop(editor);
        assert_eq!(FileEditor::open(&path, &state).unwrap().text(), "saved");
    }
    fn assert_note_locked(path: &Path, state: &Path) {
        let error = FileEditor::open(path, state)
            .err()
            .expect("another owner must still hold the note");
        assert_eq!(
            error.downcast_ref::<rustix::io::Errno>(),
            Some(&rustix::io::Errno::WOULDBLOCK),
            "{error:#}"
        );
    }

    #[test]
    fn immediate_reopen_releases_lock_despite_an_inherited_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        let state = directory.path().join("state");
        fs::write(&path, "base").unwrap();
        let mut editor = FileEditor::open(&path, &state).unwrap();
        editor.set_text("durable draft".into()).unwrap();
        // dup and fork inheritance share the same kernel open-file description.
        // Keep that descriptor alive across close/reopen, without any timing race.
        let inherited = editor.lock.file.try_clone().unwrap();
        assert_note_locked(&path, &state);
        drop(editor);
        let reopened = FileEditor::open(&path, &state).unwrap();
        assert_eq!(reopened.text(), "durable draft");
        drop(inherited);
        assert_note_locked(&path, &state);
    }

    #[test]
    fn pending_draft_retains_exclusion_until_it_finishes_then_reopens_immediately() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        let state = directory.path().join("state");
        fs::write(&path, "base").unwrap();
        let mut editor = FileEditor::open(&path, &state).unwrap();
        let inherited = editor.lock.file.try_clone().unwrap();
        let write = editor.queue_text("last queued draft".into());
        drop(editor);
        assert_note_locked(&path, &state);
        write.persist().unwrap();
        let reopened = FileEditor::open(&path, &state).unwrap();
        assert_eq!(reopened.text(), "last queued draft");
        drop(inherited);
        assert_note_locked(&path, &state);
    }

    #[test]
    fn completed_exchange_before_journal_acknowledgement_recovers_without_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("state");
        // Stress the exact close/reopen scenario without retries or sleeps.
        for _ in 0..128 {
            fs::write(&path, "base").unwrap();
            let mut editor = FileEditor::open(&path, &state).unwrap();
            editor.set_text("proposed".into()).unwrap();
            drop(editor);
            fs::write(&path, "proposed").unwrap();
            let mut editor = FileEditor::open(&path, &state).unwrap();
            assert_eq!(editor.save().unwrap(), Save::Saved);
            assert!(!editor.dirty());
        }
    }
    #[test]
    fn saved_journal_never_replaces_a_new_disk_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("state");
        fs::write(&path, "base").unwrap();
        let mut editor = FileEditor::open(&path, &state).unwrap();
        editor.set_text("saved".into()).unwrap();
        editor.save().unwrap();
        drop(editor);
        fs::write(&path, "external after close").unwrap();
        let editor = FileEditor::open(&path, &state).unwrap();
        assert_eq!(editor.text(), "external after close");
        assert!(!editor.dirty());
    }
    #[test]
    fn destination_reservation_protects_orphaned_drafts_and_active_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target.md");
        let state = dir.path().join("drafts");
        // Exercise both editor and reservation handoffs with a live dup, as when
        // a concurrent subprocess inherits the open-file description during fork.
        for _ in 0..128 {
            fs::write(&path, "base").unwrap();
            let mut editor = FileEditor::open(&path, &state).unwrap();
            let inherited_editor = editor.lock.file.try_clone().unwrap();
            editor.set_text("orphaned unsaved draft".into()).unwrap();
            fs::remove_file(&path).unwrap();
            let error = FileEditor::reserve_destination(&path, &state)
                .err()
                .unwrap();
            assert_eq!(
                error.downcast_ref::<rustix::io::Errno>(),
                Some(&rustix::io::Errno::WOULDBLOCK)
            );
            drop(editor);
            let error = FileEditor::reserve_destination(&path, &state)
                .err()
                .unwrap();
            assert!(
                error.to_string().contains("saved recovery draft"),
                "{error:#}"
            );
            fs::write(&path, "base").unwrap();
            let mut editor = FileEditor::open(&path, &state).unwrap();
            assert_eq!(editor.text(), "orphaned unsaved draft");
            drop(inherited_editor);
            assert_note_locked(&path, &state);
            assert_eq!(editor.save().unwrap(), Save::Saved);
            let inherited_saved_editor = editor.lock.file.try_clone().unwrap();
            drop(editor);
            fs::remove_file(&path).unwrap();
            let reservation = FileEditor::reserve_destination(&path, &state).unwrap();
            let inherited_reservation = reservation.file.try_clone().unwrap();
            fs::write(&path, "moved source").unwrap();
            drop(inherited_saved_editor);
            assert_note_locked(&path, &state);
            drop(reservation);
            let reopened = FileEditor::open(&path, &state).unwrap();
            assert_eq!(reopened.text(), "moved source");
            drop(inherited_reservation);
            assert_note_locked(&path, &state);
        }
    }

    #[test]
    fn invalid_utf8_and_concurrent_editor_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("state");
        fs::write(&path, [0xff]).unwrap();
        assert!(FileEditor::open(&path, &state).is_err());
        assert_eq!(fs::read(&path).unwrap(), [0xff]);
        fs::write(&path, "base").unwrap();
        let _editor = FileEditor::open(&path, &state).unwrap();
        assert!(FileEditor::open(&path, &state).is_err());
    }
    #[test]
    fn durable_unicode_draft_survives_sigabrt() {
        use std::os::unix::process::ExitStatusExt;
        const KEY: &str = "TESSERA_CRASH_DRAFT_TEST";
        const DRAFT: &str = "\u{feff}# Черновик 🧠e\u{301}\r\n[[Заметка|ссылка]] unsaved\r\n";
        if let Some(directory) = std::env::var_os(KEY) {
            let directory = PathBuf::from(directory);
            let mut editor =
                FileEditor::open(&directory.join("note.md"), &directory.join("state")).unwrap();
            // Same queued durable write used by the native editor; no Save or Drop.
            editor.queue_text(DRAFT.into()).persist().unwrap();
            std::process::abort();
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, "base\r\n").unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_editor::tests::durable_unicode_draft_survives_sigabrt",
                "--nocapture",
            ])
            .env(KEY, directory.path())
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(6), "child must reach SIGABRT");
        assert_eq!(fs::read_to_string(&path).unwrap(), "base\r\n");
        let mut editor = FileEditor::open(&path, &directory.path().join("state")).unwrap();
        assert_eq!(editor.text(), DRAFT);
        assert!(editor.dirty());
        fs::write(&path, "external\r\n").unwrap();
        assert_eq!(editor.save().unwrap(), Save::Conflict);
        assert_eq!(editor.text(), DRAFT);
        assert_eq!(fs::read_to_string(&path).unwrap(), "external\r\n");
    }
    #[test]
    fn failed_refresh_preserves_the_old_conflict_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("drafts");
        fs::write(&path, "original").unwrap();
        let mut editor = FileEditor::open(&path, &state).unwrap();
        fs::write(&path, "external").unwrap();
        // A directory at the journal destination reliably rejects atomic persist,
        // including when tests run with elevated filesystem permissions.
        fs::create_dir(&editor.journal).unwrap();
        assert!(editor.refresh_from_disk().is_err());
        assert_eq!(editor.text(), "original");
        assert!(!editor.dirty());
        fs::remove_dir(&editor.journal).unwrap();
        editor
            .set_text("local after failed refresh".into())
            .unwrap();
        assert_eq!(editor.save().unwrap(), Save::Conflict);
        assert_eq!(fs::read_to_string(path).unwrap(), "external");
    }

    #[test]
    fn watcher_refresh_updates_only_clean_source_and_preserves_dirty_draft() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("drafts");
        fs::write(&path, "original").unwrap();
        let mut editor = FileEditor::open(&path, &state).unwrap();
        fs::write(&path, "external clean 🧠").unwrap();
        assert_eq!(editor.refresh_from_disk().unwrap(), Save::Saved);
        assert_eq!(editor.text(), "external clean 🧠");
        assert!(!editor.dirty());
        editor.set_text("my unsaved Привет".into()).unwrap();
        fs::write(&path, "external conflict").unwrap();
        assert_eq!(editor.refresh_from_disk().unwrap(), Save::Conflict);
        assert_eq!(editor.text(), "my unsaved Привет");
        assert_eq!(editor.save().unwrap(), Save::Conflict);
        drop(editor);
        assert!(FileEditor::has_unsaved_draft(&path, &state).unwrap());
        assert_eq!(
            FileEditor::open(&path, &state).unwrap().text(),
            "my unsaved Привет"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "external conflict");
    }
}

#[cfg(all(test, windows))]
mod windows_editor_tests {
    use super::*;
    #[test]
    fn windows_editor_unacknowledged_commit_retains_history_and_draft() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let note = root.join("Note.md");
        let state = fixture.path().join("drafts");
        fs::write(&note, "base\r\n").unwrap();
        let mut editor = FileEditor::open(&note, &state).unwrap();
        editor.set_text("proposed שלום\r\n".into()).unwrap();
        let directory = editor.directory.pin().unwrap();
        let plan = directory
            .prepare_replace(
                note.file_name().unwrap(),
                b"base\r\n",
                editor.text().as_bytes(),
            )
            .unwrap()
            .unwrap();
        let record = crate::source_history::Preimage::begin_windows(
            &state,
            &note,
            "base\r\n",
            plan.preimage_path(),
            plan.prepared_path(),
        )
        .unwrap();
        let preimage = plan.preimage_path().to_owned();
        assert!(matches!(
            plan.commit().unwrap(),
            crate::windows_files::Replacement::Saved { .. }
        ));
        // Simulate acknowledgement loss: no history finish or clean draft write.
        drop(directory);
        drop(editor);
        assert!(record.exists() && preimage.exists());
        let mut recovered = FileEditor::open(&note, &state).unwrap();
        assert!(recovered.dirty());
        assert_eq!(recovered.text(), "proposed שלום\r\n");
        assert_eq!(recovered.save().unwrap(), Save::Saved);
        assert!(!recovered.dirty());
        let listing = crate::source_history::list(&state, &root).unwrap();
        assert!(listing
            .versions
            .iter()
            .any(|v| v.text == "base\r\n" && v.protected));
    }
    #[test]
    fn windows_editor_racing_save_keeps_disk_draft_and_prepared_recovery() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let note = root.join("Note.md");
        let state = fixture.path().join("drafts");
        fs::write(&note, "base").unwrap();
        let mut editor = FileEditor::open(&note, &state).unwrap();
        editor.set_text("mine".into()).unwrap();
        assert_eq!(
            editor
                .save_before_exchange(|| {
                    fs::write(root.join("external.md"), "external").unwrap();
                    fs::rename(root.join("external.md"), &note).unwrap();
                })
                .unwrap(),
            Save::Conflict
        );
        assert_eq!(fs::read_to_string(&note).unwrap(), "external");
        assert!(editor.dirty());
        drop(editor);
        let recovered = FileEditor::open(&note, &state).unwrap();
        assert_eq!(recovered.text(), "mine");
        assert!(recovered.dirty());
        let history = crate::source_history::list(&state, &root).unwrap();
        assert!(history
            .versions
            .iter()
            .any(|v| v.text == "base" && v.protected));
        assert!(fs::read_dir(&root).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".raced")));
    }
}
