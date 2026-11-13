//! Exact UTF-8 file editing, independent of the Brain protocol.
//! Drafts live in application state; atomic exchanges retain displaced files next
//! to the note until archived into durable history; racing versions stay protected.
#[cfg(unix)]
mod directory;
#[cfg(windows)]
#[path = "file_editor/directory_windows.rs"]
mod directory;
mod raw_transaction;
#[cfg(unix)]
use crate::unix_publish::flagged_rename as renameat_with;
use anyhow::{bail, Context, Result};
#[cfg(unix)]
pub(crate) use directory::open_regular_at;
use directory::Directory;
#[cfg(unix)]
use rustix::fs::RenameFlags;
/// An explicit inspection token. Cleanup accepts only the exact revision shown
/// by this token and never writes the public note. The editor lock stays held.
#[cfg(unix)]
pub struct InterruptedSave {
    inner: raw_transaction::RecoveryInspection,
}
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptedSaveState {
    OriginalPresent,
    ProposedPresent,
    ChangedExternally,
    /// Capture completed but create-only publication/restoration was interrupted.
    SourceMissing,
}
/// The public note was written, but its preimage changed before verification.
/// Distinct from Conflict: callers must not claim that nothing was published.
#[derive(Debug)]
pub struct PublishedSaveNeedsAttention;
impl std::fmt::Display for PublishedSaveNeedsAttention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The save was published, but recovery changed during verification. Your draft and recovery were retained; inspect the file before saving again.")
    }
}
impl std::error::Error for PublishedSaveNeedsAttention {}
/// Publication may have reached the drive, but completion was not confirmed.
/// Keep the durable draft and preimage; this error never authorizes rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnconfirmedSave {
    /// A failed rename reply or failed outcome probe cannot establish publication.
    UnknownPublication,
    /// Rename succeeded, but durability or recovery acknowledgement failed.
    Published,
}
impl std::fmt::Display for UnconfirmedSave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownPublication => "The drive did not confirm whether the save was published. Draft and recovery are retained; inspect the file before retrying.",
            Self::Published => "The save was published, but the drive or recovery did not confirm completion. Inspect the file before retrying.",
        })
    }
}
impl std::error::Error for UnconfirmedSave {}

#[cfg(unix)]
fn missing(error: &anyhow::Error) -> bool {
    error.chain().any(|e| {
        e.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT)
            || e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    })
}
use crate::source_projection::revision::Text;
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
/// Shown when the recovery folder itself cannot be used, so the reader is not
/// told that another window holds the note.
pub const RECOVERY_UNAVAILABLE: &str =
    "Okilum couldn’t prepare a recovery copy for this note. Check that it can write to its data folder.";

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

    /// Acquire, naming the cause: `busy` only when another editor holds it.
    fn acquire_or(path: &Path, busy: &'static str) -> Result<Self> {
        Self::acquire(path).map_err(|error| {
            let held = lock_busy(&error);
            error.context(if held { busy } else { RECOVERY_UNAVAILABLE })
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
    #[serde(with = "draft_text")]
    base: Text,
    #[serde(with = "draft_text")]
    text: Text,
    // Equal bytes can still be a newer input intent: Undo to the original
    // base must survive an older in-flight save publishing another revision.
    #[serde(default)]
    pending_input: bool,
}

// Keep the existing JSON recovery format. Flatten only at the persistence
// boundary (workers during typing), never while retaining a draft revision.
mod draft_text {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        text: &Text,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(text)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Text, D::Error> {
        Ok(String::deserialize(deserializer)?.into())
    }
}

/// What a `stat` of the note says about it: which file it is, its size, and
/// when its content or metadata last changed. Same-tick writes can preserve all
/// of these fields. Refresh verifies bytes on coarse/network filesystems and
/// during the current timestamp tick; save always checks exact bytes.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskStamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
#[cfg(unix)]
impl DiskStamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.size(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: crate::change_detection::change_time(metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}
/// Windows reads the note on every refresh, as before.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskStamp(());

pub struct FileEditor {
    authority: okilum_write_authority::Authority,
    raw_transaction: Option<Arc<raw_transaction::ByteTransaction>>,
    directory: Directory,
    draft: Draft,
    /// The file as this editor last read or wrote it while it held
    /// `draft.base` (#1363). `None` when that is not known.
    on_disk: Option<DiskStamp>,
    journal: PathBuf,
    /// `None` for an editor opened without ownership (`open_deferred`, #2817):
    /// it reads and shows the note, and takes the lock before its first edit.
    lock: Option<Arc<EditorLock>>,
    lock_path: PathBuf,
    writes: Arc<JournalWrites>,
    generation: u64,
    base_generation: u64,
}

#[derive(Default)]
struct JournalWrites {
    generation: AtomicU64,
    commit: Mutex<()>,
    // Canonical saves are serialized independently of journal commits. A UI
    // Save/close waits for an in-flight autosave and adopts only its confirmed
    // base. A late older worker can never publish over a newer Save.
    save: Mutex<SaveOrder>,
    // Separate short lock: observing publication never waits for canonical I/O.
    receipt: Mutex<Option<SaveReceipt>>,
}

struct SaveReceipt {
    digest: [u8; 32],
    base: Text,
    generation: u64,
}

#[derive(Default)]
struct SaveOrder {
    latest_requested: u64,
    saved: Option<(Text, Option<DiskStamp>, u64)>,
}

/// A supersedable background journal write. The note lock remains held until
/// every queued write ends; a new session can never race an old journal writer.
pub struct DraftWrite {
    draft: Draft,
    // Acknowledgement retires the old base on the journal worker, not the UI.
    retired: Vec<Text>,
    journal: PathBuf,
    writes: Arc<JournalWrites>,
    generation: u64,
    _lock: Option<Arc<EditorLock>>,
}
impl DraftWrite {
    pub fn persist(self) -> Result<()> {
        if self.writes.generation.load(Ordering::Acquire) != self.generation {
            return Ok(());
        }
        // An editor without ownership never writes recovery state. A clean
        // draft has nothing to recover; an edit must take the lock first.
        if self._lock.is_none() {
            anyhow::ensure!(
                !self.draft.pending_input && self.draft.text == self.draft.base,
                "{NOT_OWNED}"
            );
            return Ok(());
        }
        #[cfg(unix)]
        let parent = self.journal.parent().unwrap();
        #[cfg(unix)]
        {
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(&serde_json::to_vec(&self.draft)?)?;
            temp.as_file().sync_all()?;
            let _commit = self
                .writes
                .commit
                .lock()
                .map_err(|_| anyhow::anyhow!("Recovery worker failed; keep the draft open"))?;
            if self.writes.generation.load(Ordering::Acquire) != self.generation {
                return Ok(());
            }
            temp.persist(&self.journal)?;
            File::open(parent)?.sync_all()?;
        }
        #[cfg(windows)]
        {
            let _commit = self
                .writes
                .commit
                .lock()
                .map_err(|_| anyhow::anyhow!("Recovery worker failed; keep the draft open"))?;
            if self.writes.generation.load(Ordering::Acquire) != self.generation {
                return Ok(());
            }
            crate::source_state::persist(&self.journal, &serde_json::to_vec(&self.draft)?)?;
        }
        #[cfg(test)]
        JOURNAL_COMMITS.with(|commits| {
            commits
                .borrow_mut()
                .push(self.draft.base == self.draft.text)
        });
        Ok(())
    }
}

#[cfg(test)]
thread_local! { static JOURNAL_COMMITS: std::cell::RefCell<Vec<bool>> = const { std::cell::RefCell::new(Vec::new()) }; }

/// An edit reached an editor that was opened without taking ownership.
const NOT_OWNED: &str = "This editor does not own the note; choose Edit again";
/// Another window's editor holds the note.
pub const BEING_EDITED: &str = "This note is already being edited in another window";
/// Only an explicit Edit adopts an unsaved recovery draft.
pub const UNSAVED_DRAFT: &str =
    "This note has an unsaved recovery draft; choose Edit to recover it";

/// A write the current user cannot make in place: the file, its folder or
/// its filesystem is read-only. Checked without writing anything (#2817).
#[derive(Debug)]
pub struct NotWritable(pub std::io::Error);
impl std::fmt::Display for NotWritable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "This note is read-only here: {}", self.0)
    }
}
impl std::error::Error for NotWritable {}

#[cfg(unix)]
fn check_writable_in_place(path: &Path) -> Result<()> {
    use rustix::fs::{access, Access};
    for target in [path, path.parent().context("Missing source folder")?] {
        access(target, Access::WRITE_OK).map_err(|errno| {
            NotWritable(std::io::Error::from_raw_os_error(errno.raw_os_error()))
        })?;
    }
    Ok(())
}
#[cfg(windows)]
fn check_writable_in_place(path: &Path) -> Result<()> {
    // The same filesystem access check Save's replacement relies on, not the
    // read-only attribute alone: an ACL may deny replacing a readable note.
    if !crate::windows_files::can_replace(path)? {
        return Err(NotWritable(std::io::Error::from(std::io::ErrorKind::PermissionDenied)).into());
    }
    Ok(())
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

/// Fully prepared on a worker; foreground adoption never reads/materializes source.
pub struct PublicationRefresh {
    pub editor: FileEditor,
    pub outcome: Save,
    pub conflict: Option<String>,
    pub replacement: Option<String>,
}

impl FileEditor {
    /// Read-only recovery discovery: never acquire an editor or rewrite a draft.
    pub fn has_unsaved_draft(path: &Path, state: &Path) -> Result<bool> {
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path
                .parent()
                .context("Source has no folder")?
                .canonicalize()?
                .join(path.file_name().context("Source has no filename")?),
            Err(error) => return Err(error.into()),
        };
        let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let bytes = match fs::read(state.join(format!("{key}.json"))) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let draft: Draft = serde_json::from_slice(&bytes)?;
        if draft.path != path || (!draft.pending_input && draft.text == draft.base) {
            return Ok(false);
        }
        match fs::read_to_string(path) {
            // Windows publication can reach the file before its acknowledgement
            // reaches recovery. Equal bytes do not authorize forgetting that draft.
            Ok(current) => Ok(cfg!(windows) || draft.text != current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.into()),
        }
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
        fs::create_dir_all(state).context(RECOVERY_UNAVAILABLE)?;
        let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let lock = EditorLock::acquire_or(
            &state.join(format!("{key}.lock")),
            "The destination has an active editor or recovery writer",
        )?;
        match fs::read(state.join(format!("{key}.json"))) {
            Ok(bytes) => {
                let draft: Draft = serde_json::from_slice(&bytes)?;
                if draft.path != path || draft.pending_input || draft.text != draft.base {
                    bail!("The destination has a saved recovery draft. Choose another name; the draft has been preserved");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(lock)
    }

    pub fn open(path: &Path, state: &Path) -> Result<Self> {
        Self::open_with(path, state, true)
    }

    /// Open a note for an automatic Edit (Settings → Open notes in → Edit,
    /// #2817) without taking ownership: no editor lock, no recovery folder or
    /// journal write, no writer authority and no change to the file.
    /// `acquire_lock` takes the lock before the first edit. Refuses, like
    /// `open`, while another editor holds the note; also refuses a note the
    /// user cannot save in place (read-only file, folder or filesystem) and
    /// one with an unsaved recovery draft, which only an explicit Edit adopts.
    pub fn open_deferred(path: &Path, state: &Path) -> Result<Self> {
        Self::open_with(path, state, false)
    }

    /// Whether this editor holds the note's editor lock.
    pub fn owns_lock(&self) -> bool {
        self.lock.is_some()
    }

    /// Take ownership before the first edit of an `open_deferred` editor.
    /// Another editor that took the note meanwhile refuses with
    /// `BEING_EDITED`; a recovery draft written meanwhile is never adopted.
    pub fn acquire_lock(&mut self) -> Result<()> {
        if self.lock.is_some() {
            return Ok(());
        }
        let state = self.journal.parent().context("Missing recovery folder")?;
        fs::create_dir_all(state).context(RECOVERY_UNAVAILABLE)?;
        let lock = EditorLock::acquire_or(&self.lock_path, BEING_EDITED)?;
        anyhow::ensure!(
            !Self::has_unsaved_draft(&self.draft.path, state)?,
            "Another window left an unsaved draft of this note; reopen it to recover"
        );
        self.lock = Some(Arc::new(lock));
        Ok(())
    }

    fn open_with(path: &Path, state: &Path, owned: bool) -> Result<Self> {
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
        let authority = okilum_write_authority::Authority::for_path(&path)?;
        #[cfg(unix)]
        let directory = Directory::open(path.parent().context("Missing source folder")?)?;
        let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let lock_path = state.join(format!("{key}.lock"));
        let lock = if owned {
            fs::create_dir_all(state).context(RECOVERY_UNAVAILABLE)?;
            Some(Arc::new(EditorLock::acquire_or(&lock_path, BEING_EDITED)?))
        } else {
            check_writable_in_place(&path)?;
            anyhow::ensure!(!Self::has_unsaved_draft(&path, state)?, "{UNSAVED_DRAFT}");
            // Probe only: another window's editor refuses; nothing is kept and
            // a missing recovery folder is not created.
            if lock_path.exists() {
                drop(EditorLock::acquire_or(&lock_path, BEING_EDITED)?);
            }
            None
        };
        let journal = state.join(format!("{key}.json"));
        let (base, opened) = directory.read(path.file_name().unwrap())?;
        #[cfg(unix)]
        let opened_stamp = Some(DiskStamp::of(&opened));
        #[cfg(windows)]
        let opened_stamp: Option<DiskStamp> = None;
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
        let base: Text = base.into();
        // A recovered draft may carry another base than the file holds; then
        // its stamp says nothing about the base.
        let (draft, holds_base) = if journal.exists() {
            let draft: Draft = serde_json::from_slice(&fs::read(&journal)?)?;
            if draft.path != path {
                bail!("Recovery draft belongs to another file");
            }
            // Windows keeps an unacknowledged save pending even when publication
            // already reached disk. Only Save/reload can acknowledge its journal.
            if (!draft.pending_input && draft.text == draft.base)
                || (cfg!(unix) && draft.text == base)
            {
                let draft = Draft {
                    path,
                    text: base.clone(),
                    base,
                    pending_input: false,
                };
                (draft, true)
            } else {
                let holds_base = draft.base == base;
                (draft, holds_base)
            }
        } else {
            let draft = Draft {
                path,
                text: base.clone(),
                base,
                pending_input: false,
            };
            (draft, true)
        };
        let on_disk = opened_stamp.filter(|_| holds_base);
        Ok(Self {
            authority,
            directory,
            draft,
            on_disk,
            journal,
            lock,
            lock_path,
            writes: Arc::default(),
            generation: 0,
            base_generation: 0,
            raw_transaction: None,
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
        if self.raw_transaction.is_some() {
            self.base_bytes() != self.proposed_bytes()
        } else {
            self.draft.base != self.draft.text
        }
    }
    /// Constant-time UI hint. A distinct revision may be an Undo back to the
    /// base; exact byte equality is established by the save/recovery worker.
    pub fn has_pending_revision(&self) -> bool {
        self.raw_transaction.is_some()
            || self.draft.pending_input
            || !self.draft.base.same(&self.draft.text)
    }
    pub fn current(&self) -> Result<String> {
        Ok(self.read_current()?.0)
    }
    /// The note's text with its stamp. The stamp is taken before the read, so
    /// a write during the read leaves it older than the text, never newer.
    fn read_current(&self) -> Result<(String, Option<DiskStamp>)> {
        self.directory.validate(self.draft.path.parent().unwrap())?;
        let (text, _metadata) = self.directory.read(self.draft.path.file_name().unwrap())?;
        #[cfg(unix)]
        return Ok((text, Some(DiskStamp::of(&_metadata))));
        #[cfg(windows)]
        Ok((text, None))
    }
    /// The note's stamp now, verifying content against the base when required.
    #[cfg(unix)]
    fn stamp_now(&self) -> Result<Option<DiskStamp>> {
        let file = open_regular_at(&self.directory.file, self.draft.path.file_name().unwrap())?;
        let stamp = DiskStamp::of(&file.metadata()?);
        if self.on_disk == Some(stamp) {
            let changed = u64::try_from(stamp.ctime.0)
                .ok()
                .and_then(|seconds| {
                    std::time::UNIX_EPOCH
                        .checked_add(std::time::Duration::new(seconds, stamp.ctime.1 as u32))
                })
                .unwrap_or(std::time::SystemTime::now());
            if crate::change_detection::needs_content_check(&self.draft.path, changed)
                && crate::change_detection::hash_reader(&file, stamp.size)?
                    != crate::change_detection::digest(self.base_bytes())
            {
                return Ok(None);
            }
        }
        Ok(Some(stamp))
    }
    #[cfg(windows)]
    fn stamp_now(&self) -> Result<Option<DiskStamp>> {
        Ok(None)
    }
    /// The stamp of the file this editor last saw holding its base, for
    /// handing a worker's save back to the UI editor (`saved_as_queued_at`).
    /// Exact committed bytes, checked against a worker-observed publication.
    /// A watcher hint or a matching timestamp alone never proves an own save.
    pub fn owns_publication(&self, digest: &[u8; 32]) -> bool {
        self.writes.receipt.lock().is_ok_and(|receipt| {
            receipt.as_ref().is_some_and(|receipt| {
                &receipt.digest == digest
                    && (self.draft.base.same(&receipt.base)
                        || receipt.generation >= self.base_generation)
            })
        })
    }

    pub fn disk_stamp(&self) -> Option<DiskStamp> {
        self.on_disk
    }
    /// Update the UI-owned snapshot without performing filesystem I/O.
    pub fn queue_text(&mut self, text: String) -> DraftWrite {
        self.queue_snapshot(text.into())
    }
    /// Retain the native immutable Rope revision without copying/scanning text.
    pub fn queue_snapshot(&mut self, text: Text) -> DraftWrite {
        // An editor without ownership has no edit of its own: capturing the
        // text it already holds stays clean instead of becoming an unowned
        // pending revision. A different text is refused when it is persisted.
        if self.lock.is_none() && !self.draft.pending_input && self.draft.text == text {
            return self.pending_write();
        }
        self.draft.text = text;
        self.draft.pending_input = true;
        self.advance_generation();
        self.pending_write()
    }
    /// A whole-buffer reset is an explicit bulk operation, unlike a key edit.
    /// Establish exact equality here so a clean reload does not appear edited
    /// merely because the native buffer has a different revision identity.
    pub fn queue_reset_snapshot(&mut self, text: Text) -> DraftWrite {
        let unchanged = !self.draft.pending_input && self.draft.text == text;
        if self.draft.base == text {
            self.draft.base = text.clone();
        }
        let mut write = self.queue_snapshot(text);
        if unchanged {
            self.draft.pending_input = false;
            write.draft.pending_input = false;
        }
        write
    }
    pub fn snapshot(&self) -> Text {
        self.draft.text.clone()
    }
    fn advance_generation(&mut self) {
        self.generation = self.writes.generation.fetch_add(1, Ordering::AcqRel) + 1;
    }
    fn retire_snapshot_writes(&mut self) {
        // A successful save may retire only its own journal generation. Never
        // manufacture ownership for an older worker after a newer input edit.
        if self
            .writes
            .generation
            .compare_exchange(
                self.generation,
                self.generation + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.generation += 1;
        }
    }
    pub fn flush_draft(&self) -> Result<()> {
        self.persist()
    }
    pub fn recovery_write(&self) -> DraftWrite {
        self.pending_write()
    }
    pub fn set_text(&mut self, text: String) -> Result<()> {
        self.queue_text(text).persist()
    }
    fn pending_write(&self) -> DraftWrite {
        DraftWrite {
            draft: self.draft.clone(),
            retired: Vec::new(),
            journal: self.journal.clone(),
            writes: self.writes.clone(),
            generation: self.generation,
            _lock: self.lock.clone(),
        }
    }
    fn persist(&self) -> Result<()> {
        // Explicit byte transactions persist their complete snapshots in the new
        // History record before publication, never in a lossy text Draft.
        if self.raw_transaction.is_some() {
            return Ok(());
        }
        self.pending_write().persist()
    }
    /// Refresh only a clean buffer. Dirty source is never replaced by a watcher.
    ///
    /// The folder is checked every time; the note itself is read only when its
    /// stamp differs from the one this editor last saw holding its base.
    /// Coarse/network filesystems and the current tick additionally compare a
    /// bounded fingerprint against that exact base, including after own saves.
    pub fn refresh_from_disk(&mut self) -> Result<Save> {
        self.refresh_from_disk_inner(false)
    }

    /// Renew observation ownership after a superseded refresh reserved a newer
    /// journal generation. This never adopts its base or authorizes a save.
    pub fn publication_copy(&mut self) -> Result<Self> {
        self.generation = self.writes.generation.load(Ordering::Acquire);
        self.save_copy()
    }

    /// Background publication refresh. A stale read cannot supersede newer
    /// typing/recovery. Only this worker's generation may publish its journal.
    pub fn refresh_publication(mut self) -> Result<Option<PublicationRefresh>> {
        let original = self.snapshot().id();
        if self.writes.generation.load(Ordering::Acquire) != self.generation {
            return Ok(None);
        }
        let result = self.refresh_from_disk_inner(true)?;
        if self.writes.generation.load(Ordering::Acquire) != self.generation {
            return Ok(None);
        }
        let conflict = if result == Save::Conflict {
            Some(self.current()?)
        } else {
            None
        };
        let replacement = (self.snapshot().id() != original).then(|| self.text().to_owned());
        Ok(Some(PublicationRefresh {
            editor: self,
            outcome: result,
            conflict,
            replacement,
        }))
    }

    fn refresh_from_disk_inner(&mut self, guarded: bool) -> Result<Save> {
        self.directory.validate(self.draft.path.parent().unwrap())?;
        if self.on_disk.is_some() && self.stamp_now()? == self.on_disk {
            return Ok(Save::Saved);
        }
        let (current, stamp) = self.read_current()?;
        let current: Text = current.into();
        if self.dirty() || self.draft.pending_input {
            if current == self.draft.base {
                self.on_disk = stamp;
            }
            return Ok(
                if current == self.draft.base || current == self.draft.text {
                    Save::Saved
                } else {
                    Save::Conflict
                },
            );
        }
        if current != self.draft.base {
            if guarded {
                if self
                    .writes
                    .generation
                    .compare_exchange(
                        self.generation,
                        self.generation + 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    return Ok(Save::Saved);
                }
                self.generation += 1;
            } else {
                self.advance_generation();
            }
            let mut write = self.pending_write();
            write.draft.base = current.clone();
            write.draft.text = current.clone();
            write.draft.pending_input = false;
            // Keep the conflict baseline paired with the visible source until
            // recovery persistence succeeds. A failed refresh must not authorize
            // saving the old visible text over the external version. An editor
            // without ownership has no journal; it only adopts the new text.
            if self.lock.is_some() {
                write.persist()?;
            }
            self.draft.base = current.clone();
            self.draft.text = current;
            self.draft.pending_input = false;
            self.base_generation = self.generation;
        }
        self.on_disk = stamp;
        Ok(Save::Saved)
    }

    pub fn reload(&mut self) -> Result<()> {
        let writes = self.writes.clone();
        let mut order = writes
            .save
            .lock()
            .map_err(|_| anyhow::anyhow!("Save worker failed; draft retained"))?;
        let (current, stamp) = self.read_current()?;
        let current: Text = current.into();
        self.advance_generation();
        order.latest_requested = self.generation;
        let mut write = self.pending_write();
        write.draft.base = current.clone();
        write.draft.text = current.clone();
        write.draft.pending_input = false;
        if self.lock.is_some() {
            write.persist()?;
        }
        self.base_generation = self.generation;
        self.draft.base = current.clone();
        self.draft.text = current;
        self.draft.pending_input = false;
        self.on_disk = stamp;
        Ok(())
    }
    /// Explicit Keep mine uses the exact version displayed in the conflict UI.
    pub fn keep_mine(&mut self, reviewed: &str) -> Result<Save> {
        if self.current()? != reviewed {
            return Ok(Save::Conflict);
        }
        self.advance_generation();
        self.base_generation = self.generation;
        self.draft.base = reviewed.to_owned().into();
        self.on_disk = None;
        self.save()
    }
    pub fn save(&mut self) -> Result<Save> {
        self.save_before_exchange(|| {})
    }
    /// An editor for the same file and draft that can save on a worker while
    /// this one keeps taking edits (autosave, #1353). It shares the lock and
    /// the journal order; `save` still refuses to replace a changed file.
    pub fn save_copy(&self) -> Result<Self> {
        Ok(Self {
            authority: self.authority.clone(),
            directory: self.directory.try_clone()?,
            draft: self.draft.clone(),
            on_disk: self.on_disk,
            journal: self.journal.clone(),
            lock: self.lock.clone(),
            lock_path: self.lock_path.clone(),
            writes: self.writes.clone(),
            generation: self.generation,
            base_generation: self.base_generation,
            raw_transaction: self.raw_transaction.clone(),
        })
    }
    /// A copy saved `text`: that is the file now, so it becomes the base.
    pub fn saved_as(&mut self, text: String) -> Result<()> {
        self.saved_as_queued(text).persist()
    }
    /// `saved_as` with the recovery copy written by the caller, off the UI thread.
    pub fn saved_as_queued(&mut self, text: String) -> DraftWrite {
        self.saved_as_queued_at(text, None)
    }
    /// `saved_as_queued` with the stamp the saving copy left (`disk_stamp`),
    /// so the next refresh does not read back the text just written.
    pub fn saved_as_queued_at(&mut self, text: String, stamp: Option<DiskStamp>) -> DraftWrite {
        let text = if self.draft.text == text {
            self.draft.text.clone()
        } else {
            text.into()
        };
        self.saved_snapshot_queued_at(text, stamp)
    }
    pub fn saved_snapshot_queued_at(&mut self, text: Text, stamp: Option<DiskStamp>) -> DraftWrite {
        self.draft.pending_input = !text.same(&self.draft.text);
        let retired = std::mem::replace(&mut self.draft.base, text);
        self.on_disk = stamp;
        self.advance_generation();
        self.base_generation = self.generation;
        let mut write = self.pending_write();
        write.retired.push(retired);
        write
    }
    fn acknowledge_saved(&mut self, stamp: Option<DiskStamp>) -> Result<()> {
        let previous_base = std::mem::replace(&mut self.draft.base, self.draft.text.clone());
        self.draft.pending_input = false;
        self.retire_snapshot_writes();
        if let Err(error) = self.persist() {
            self.draft.base = previous_base;
            self.draft.pending_input = true;
            return Err(error);
        }
        self.on_disk = stamp;
        Ok(())
    }
    fn save_before_exchange(&mut self, before_exchange: impl FnOnce()) -> Result<Save> {
        // An editor without ownership has nothing of its own to publish. A
        // clean navigation save touches nothing; an edit must own the note.
        if self.lock.is_none() {
            anyhow::ensure!(!self.dirty() && !self.draft.pending_input, "{NOT_OWNED}");
            return Ok(Save::Saved);
        }
        let writes = self.writes.clone();
        let mut order = writes
            .save
            .lock()
            .map_err(|_| anyhow::anyhow!("Save worker failed; draft retained"))?;
        if self.generation < order.latest_requested {
            return Ok(Save::Conflict);
        }
        order.latest_requested = self.generation;
        if let Some((base, stamp, generation)) = &order.saved {
            if *generation > self.generation {
                return Ok(Save::Conflict);
            }
            if *generation >= self.base_generation {
                self.draft.base = base.clone();
                self.on_disk = *stamp;
                self.base_generation = *generation;
            }
        }
        let requested_generation = self.generation;
        // A clean navigation/close needs no canonical admission. Dirty drafts
        // are durable before refusal, with one pre-publication journal flush.
        let _admission = if self.dirty() || self.draft.pending_input {
            self.persist()?;
            Some(self.authority.operation()?)
        } else {
            None
        };
        let result = self.save_before_exchange_locked(before_exchange);
        if matches!(result, Ok(Save::Saved)) {
            order.saved = Some((self.draft.base.clone(), self.on_disk, requested_generation));
            let receipt = SaveReceipt {
                digest: Sha256::digest(self.base_bytes()).into(),
                base: self.draft.base.clone(),
                generation: requested_generation,
            };
            let retired = self.writes.receipt.lock().unwrap().replace(receipt);
            drop(retired); // Never destroy a full old Rope while holding the UI receipt lock.
        }
        result
    }
    #[cfg(unix)]
    fn save_before_exchange_locked(&mut self, before_exchange: impl FnOnce()) -> Result<Save> {
        self.refuse_unresolved_link_recovery()?;
        if !self.dirty() && !self.draft.pending_input {
            self.directory.validate(self.draft.path.parent().unwrap())?;
            // Undo or a clean explicit Save can have equal bytes in distinct
            // native revisions. Publish the worker's equality proof by sharing
            // the same snapshot, so UI dirty hints remain constant-time.
            self.draft.base = self.draft.text.clone();
            return Ok(Save::Saved);
        }
        self.directory.validate(self.draft.path.parent().unwrap())?;
        let (current, metadata) = self
            .directory
            .read_bytes(self.draft.path.file_name().unwrap())?;
        if self.raw_transaction.is_some() && current != self.base_bytes() {
            return Ok(Save::Conflict);
        }
        if current == self.proposed_bytes() {
            self.directory.validate(self.draft.path.parent().unwrap())?;
            self.acknowledge_saved(Some(DiskStamp::of(&metadata)))?;
            return Ok(Save::Saved);
        }
        if current != self.base_bytes() {
            return Ok(Save::Conflict);
        }
        let parent = self.draft.path.parent().unwrap();
        let directory = &self.directory.file;
        let (mut temp, name) = self.directory.temporary()?;
        let backup = parent.join(&name);
        let preparation: Result<_> = (|| {
            self.track_raw_temporary(&name, &temp)?;
            temp.set_permissions(metadata.permissions())?;
            #[cfg(test)]
            self.inject_partial_raw_write(&mut temp)?;
            temp.write_all(self.proposed_bytes())?;
            temp.sync_all()?;
            directory.sync_all()?;
            let history = self.begin_save_history(&backup, None)?;
            self.directory.validate(parent)?;
            before_exchange();
            Ok(history)
        })();
        let history = match preparation {
            Ok(history) => history,
            Err(error) => {
                return Err(with_cleanup(
                    error,
                    clean_owned_temp(directory, name.as_ref(), &temp),
                ))
            }
        };
        let mut compatible = false;
        match renameat_with(
            directory,
            backup.file_name().unwrap(),
            directory,
            self.draft.path.file_name().unwrap(),
            RenameFlags::EXCHANGE,
        ) {
            Ok(()) => {}
            Err(error) if crate::unix_publish::unsupported(error) => {
                compatible = true;
                if !self.compatible_replace(directory, &backup, &history)? {
                    return Ok(Save::Conflict);
                }
            }
            Err(error)
                if matches!(
                    error,
                    rustix::io::Errno::IO | rustix::io::Errno::TIMEDOUT | rustix::io::Errno::STALE
                ) =>
            {
                // A remote exchange may have executed before its reply failed.
                // Do not unlink either name or guess which revision is current.
                return Err(anyhow::Error::from(error).context(UnconfirmedSave::UnknownPublication));
            }
            Err(error) => {
                return Err(with_cleanup(
                    error.into(),
                    clean_owned_temp(directory, name.as_ref(), &temp),
                ))
            }
        }
        let completion = (|| -> Result<Option<Save>> {
            #[cfg(test)]
            if network_save_tests::FAIL_PUBLICATION.with(|v| v.get())
                == Some(network_save_tests::PublicationFailure::DirectorySync)
            {
                return Err(rustix::io::Errno::IO.into());
            }
            directory.sync_all().context(UnconfirmedSave::Published)?;
            #[cfg(test)]
            if compatible
                && network_save_tests::ALTER_PREIMAGE_AFTER_PUBLISH.with(|v| v.replace(false))
            {
                fs::write(&backup, b"external preimage write")?;
            }
            let displaced =
                open_regular_at(directory, backup.file_name().unwrap()).and_then(|mut file| {
                    // A racing writer may not have synced its replacement yet.
                    // Protect that inode before either archiving or rolling back.
                    file.sync_all()?;
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes)?;
                    Ok(bytes)
                });
            if !displaced.is_ok_and(|bytes| bytes == self.base_bytes()) {
                if compatible {
                    // Plain rename cannot safely roll back a third-party change.
                    // Both the draft and pending displaced preimage stay protected.
                    return Err(PublishedSaveNeedsAttention.into());
                }
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
                return Ok(Some(Save::Conflict));
            }
            // Complete all fallible publication checks while the preimage remains
            // available for a raw transaction rollback.
            if self.raw_transaction.is_some() {
                self.directory.validate(parent)?;
                let _ = temp.metadata()?;
                self.finish_raw_history_unix(&history)?;
                return Ok(Some(Save::Saved));
            }
            if compatible {
                crate::source_history::Preimage::finish_compatible(&history, directory)?;
            } else {
                crate::source_history::Preimage::finish_bound(&history, directory)?;
            }
            self.directory.validate(parent)?;
            Ok(None)
        })();
        // Once compatible rename succeeded, any subsequent error must retain
        // recovery. A plain rename cannot safely reverse a racing writer.
        let completed = completion.map_err(|error| {
            if compatible && !error.is::<PublishedSaveNeedsAttention>() {
                error.context(UnconfirmedSave::Published)
            } else {
                error
            }
        })?;
        if let Some(outcome) = completed {
            if outcome == Save::Saved {
                self.forget_link_recovery(&history);
            }
            return Ok(outcome);
        }
        let stamp = DiskStamp::of(&temp.metadata().context(UnconfirmedSave::Published)?);
        self.acknowledge_saved(Some(stamp))
            .context(UnconfirmedSave::Published)?;
        self.forget_link_recovery(&history);
        // Cleanup is best effort; a retention error never reverses a saved note.
        let _ = crate::source_history::prune(self.journal.parent().unwrap());
        Ok(Save::Saved)
    }
    /// Without EXCHANGE, capture the public entry before checking it and publish
    /// with an exclusive hard link. No rename ever targets the public note.
    /// This deliberately has a brief absent-name interval; durable intent and
    /// the captured inode permit create-only restoration after interruption.
    #[cfg(unix)]
    fn compatible_replace(&self, directory: &File, backup: &Path, history: &Path) -> Result<bool> {
        use rustix::fs::{linkat, openat, renameat, AtFlags, Mode, OFlags};
        let note = self.draft.path.file_name().unwrap();
        let old_name = backup.file_name().unwrap();
        let prepared = format!(".okilum-save-{}", uuid::Uuid::new_v4());
        let current = match self.directory.read_bytes(note) {
            Ok((text, _)) => Some(text),
            Err(e) if missing(&e) => None,
            Err(e) => {
                return Err(with_cleanup(
                    e,
                    clean_recovery_file(directory, old_name, self.proposed_bytes()),
                ))
            }
        };
        if current.as_deref() != Some(self.base_bytes()) {
            clean_recovery_file(directory, old_name, self.proposed_bytes())?;
            return Ok(false);
        }
        // Record both private names and inode ownership before the first
        // namespace change. Existing History remains the original-byte store.
        self.record_link_recovery(history, old_name, &prepared)?;
        self.track_raw_renamed(old_name, &prepared)?;
        if let Err(error) = renameat(directory, old_name, directory, prepared.as_str()) {
            return Err(with_cleanup(
                error.into(),
                clean_recovery_file(directory, old_name, self.proposed_bytes()),
            ));
        }
        let mut linked = false;
        let mut publication_attempted = false;
        let mut captured = false;
        let result = (|| -> Result<bool> {
            // No symlink following; private UUID backup name is created exclusively.
            #[cfg(test)]
            let link_error = network_save_tests::LINK_ERROR.with(|v| v.take());
            #[cfg(not(test))]
            let link_error: Option<rustix::io::Errno> = None;
            match link_error.map_or_else(
                || linkat(directory, note, directory, old_name, AtFlags::empty()),
                Err,
            ) {
                Ok(()) => {}
                Err(rustix::io::Errno::NOENT) => {
                    clean_recovery_file(
                        directory,
                        std::ffi::OsStr::new(&prepared),
                        self.proposed_bytes(),
                    )?;
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            }
            linked = true;
            #[cfg(test)]
            if let Some(error) = network_save_tests::ERROR_AFTER_LINK.with(|v| v.take()) {
                return Err(error.into());
            }
            #[cfg(test)]
            if network_save_tests::DELETE_AFTER_LINK.with(|v| v.replace(false)) {
                rustix::fs::unlinkat(directory, note, AtFlags::empty())?;
            }
            let original = openat(
                directory,
                note,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            );
            let mut original = match original {
                Ok(fd) => File::from(fd),
                Err(rustix::io::Errno::NOENT) => {
                    clean_recovery_file(directory, old_name, self.base_bytes())?;
                    clean_recovery_file(
                        directory,
                        std::ffi::OsStr::new(&prepared),
                        self.proposed_bytes(),
                    )?;
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            };
            let mut bytes = Vec::new();
            original.read_to_end(&mut bytes)?;
            if bytes != self.base_bytes() {
                clean_recovery_file(directory, old_name, self.base_bytes())?;
                clean_recovery_file(
                    directory,
                    std::ffi::OsStr::new(&prepared),
                    self.proposed_bytes(),
                )?;
                return Ok(false);
            }
            directory.sync_all()?;
            self.directory.validate(self.draft.path.parent().unwrap())?;
            publication_attempted = true;
            #[cfg(test)]
            if network_save_tests::FAIL_PUBLICATION.with(|v| v.get())
                == Some(network_save_tests::PublicationFailure::OutcomeProbe)
            {
                return Err(rustix::io::Errno::IO.into());
            }
            // The preflight hard link proved support without removing the note.
            // Remove that private alias, then capture whichever entry is current.
            // A replacement after our earlier byte check is captured and checked
            // below, never overwritten by a plain rename to the public name.
            rustix::fs::unlinkat(directory, old_name, AtFlags::empty())?;
            linked = false;
            #[cfg(test)]
            network_save_tests::at_boundary(&network_save_tests::BEFORE_CAPTURE);
            captured = true;
            renameat(directory, note, directory, old_name)?;
            open_regular_at(directory, old_name)?.sync_all()?;
            linked = true;
            directory.sync_all()?;
            let (displaced, _) = self.directory.read_bytes(old_name)?;
            #[cfg(test)]
            network_save_tests::at_boundary(&network_save_tests::AFTER_CAPTURE);
            if displaced != self.base_bytes() {
                // Restore the actual captured revision, including a raced newer
                // inode. EEXIST preserves both that revision and the new occupant.
                linkat(directory, old_name, directory, note, AtFlags::empty())?;
                directory.sync_all()?;
                rustix::fs::unlinkat(directory, old_name, AtFlags::empty())?;
                linked = false;
                captured = false;
                return Ok(false);
            }
            // Even if another writer creates the public name during the gap,
            // this cannot clobber it. Old-fd writes remain on the captured inode.
            match linkat(
                directory,
                prepared.as_str(),
                directory,
                note,
                AtFlags::empty(),
            ) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {
                    // Distinguish an external occupant from an unconfirmed
                    // retry that may already have linked our prepared inode.
                    let proposed = rustix::fs::statat(
                        directory,
                        prepared.as_str(),
                        AtFlags::SYMLINK_NOFOLLOW,
                    )?;
                    let occupant = rustix::fs::statat(directory, note, AtFlags::SYMLINK_NOFOLLOW)?;
                    if proposed.st_dev == occupant.st_dev && proposed.st_ino == occupant.st_ino {
                        return Err(rustix::io::Errno::EXIST.into());
                    }
                    clean_recovery_file(directory, old_name, self.base_bytes())?;
                    linked = false;
                    captured = false;
                    return Ok(false);
                }
                Err(error) => return Err(error.into()),
            }
            // Persist the new public link before retiring the prepared name.
            directory.sync_all()?;
            rustix::fs::unlinkat(directory, prepared.as_str(), AtFlags::empty())?;
            #[cfg(test)]
            if matches!(
                network_save_tests::FAIL_PUBLICATION.with(|v| v.get()),
                Some(
                    network_save_tests::PublicationFailure::LostReply
                        | network_save_tests::PublicationFailure::OutcomeProbe
                )
            ) {
                return Err(rustix::io::Errno::IO.into());
            }
            Ok(true)
        })();
        match result {
            Ok(true) => Ok(true),
            outcome => {
                // Once capture ran, a failed reply may hide a completed link.
                // Keep both complete versions and the durable intent. In
                // particular, never remove the captured inode on EEXIST.
                if captured {
                    return outcome
                        .map_err(|error| error.context(UnconfirmedSave::UnknownPublication));
                }
                // A server can publish a rename but lose its reply. Once the
                // prepared name disappeared, retain the preimage for recovery.
                if publication_attempted {
                    #[cfg(test)]
                    let probe_error = (network_save_tests::FAIL_PUBLICATION.with(|v| v.get())
                        == Some(network_save_tests::PublicationFailure::OutcomeProbe))
                    .then_some(rustix::io::Errno::IO);
                    #[cfg(not(test))]
                    let probe_error: Option<rustix::io::Errno> = None;
                    match probe_error.map_or_else(
                        || {
                            rustix::fs::statat(
                                directory,
                                prepared.as_str(),
                                AtFlags::SYMLINK_NOFOLLOW,
                            )
                        },
                        Err,
                    ) {
                        Err(rustix::io::Errno::NOENT) => {
                            return outcome.map_err(|error| {
                                error.context(UnconfirmedSave::UnknownPublication)
                            })
                        }
                        Err(error) => {
                            return outcome.map_err(|original| {
                                original
                                    .context(format!("Cannot verify unpublished save: {error}"))
                                    .context(UnconfirmedSave::UnknownPublication)
                            })
                        }
                        Ok(_) => {}
                    }
                }
                let draft_cleanup = clean_recovery_file(
                    directory,
                    std::ffi::OsStr::new(&prepared),
                    self.proposed_bytes(),
                );
                let preimage_cleanup = if linked {
                    clean_recovery_file(directory, old_name, self.base_bytes())
                } else {
                    Ok(())
                };
                if draft_cleanup.is_ok() && preimage_cleanup.is_ok() {
                    self.forget_link_recovery(history);
                }
                match outcome {
                    Err(error) => Err(with_cleanup(
                        with_cleanup(error, draft_cleanup),
                        preimage_cleanup,
                    )),
                    Ok(false) => {
                        draft_cleanup?;
                        preimage_cleanup?;
                        Ok(false)
                    }
                    Ok(true) => unreachable!(),
                }
            }
        }
    }
    #[cfg(windows)]
    fn save_before_exchange_locked(&mut self, before_exchange: impl FnOnce()) -> Result<Save> {
        if !self.dirty() && !self.draft.pending_input {
            self.directory.validate(self.draft.path.parent().unwrap())?;
            // Undo or a clean explicit Save can have equal bytes in distinct
            // native revisions. Publish the worker's equality proof by sharing
            // the same snapshot, so UI dirty hints remain constant-time.
            self.draft.base = self.draft.text.clone();
            return Ok(Save::Saved);
        }
        self.directory.validate(self.draft.path.parent().unwrap())?;
        let (current, _) = self
            .directory
            .read_bytes(self.draft.path.file_name().unwrap())?;
        if self.raw_transaction.is_some() && current != self.base_bytes() {
            return Ok(Save::Conflict);
        }
        if current == self.proposed_bytes() {
            self.acknowledge_saved(None)?;
            return Ok(Save::Saved);
        }
        let directory = self.directory.pin()?;
        let Some(plan) = directory.prepare_replace_tracked(
            self.draft.path.file_name().unwrap(),
            self.base_bytes(),
            self.proposed_bytes(),
            |path, identity| self.track_raw_windows_identity(path, identity),
        )?
        else {
            return Ok(Save::Conflict);
        };
        self.track_raw_windows_preimage(plan.preimage_path(), plan.preimage_identity())?;
        let history = self.begin_save_history(plan.preimage_path(), Some(plan.prepared_path()))?;
        before_exchange();
        let preimage_identity = plan.preimage_identity();
        let committed = plan.commit();
        // Recovery bytes were flushed to app state before publication. Keep
        // that pending record, and remove only its checked staging inode.
        // Cleanup failure must never mask the original save/conflict outcome;
        // startup retries without guessing ownership from a service filename.
        if !matches!(
            committed.as_ref(),
            Ok(crate::windows_files::Replacement::Saved { .. })
        ) {
            if self.raw_transaction.is_none() {
                let _ = crate::source_history::Preimage::archive_prepared_windows(&history);
            }
        }
        match committed? {
            crate::windows_files::Replacement::Conflict => return Ok(Save::Conflict),
            crate::windows_files::Replacement::Saved { .. } => {}
        }
        if self.raw_transaction.is_some() {
            self.finish_raw_history_windows(&history, &directory, preimage_identity)?;
            return Ok(Save::Saved);
        }
        crate::source_history::Preimage::finish_windows(&history, &directory, preimage_identity)?;
        self.acknowledge_saved(None)?;
        let _ = crate::source_history::prune(self.journal.parent().unwrap());
        Ok(Save::Saved)
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn windows_editor_lock_names_busy_only_when_another_editor_holds_it() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("note.lock");
        let held = EditorLock::acquire(&path).unwrap();
        // Positive control: a second editor on the same note is reported as busy.
        let busy = EditorLock::acquire_or(&path, "busy").err().unwrap();
        assert_eq!(busy.to_string(), "busy", "{busy:#}");
        drop(held);
        // A lock file that cannot be created is a recovery-folder problem.
        let missing = state.path().join("absent").join("note.lock");
        let error = EditorLock::acquire_or(&missing, "busy").err().unwrap();
        assert_eq!(error.to_string(), RECOVERY_UNAVAILABLE, "{error:#}");
        assert!(EditorLock::acquire_or(&path, "busy").is_ok());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn reads() -> usize {
        directory::READS.with(|reads| reads.get())
    }

    /// #1363: a refresh does not read the note back after this editor's own
    /// save, its autosave copy's save, or another refresh; any outside write
    /// is still seen, even one that keeps the size and sets the mtime back.
    #[test]
    fn refresh_reads_the_note_only_when_it_changed_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("note.md");
        fs::write(&path, "base").unwrap();
        let state = root.path().join("state");
        let mut editor = FileEditor::open(&path, &state).unwrap();
        let refresh_reads = |editor: &mut FileEditor| {
            let before = reads();
            let outcome = editor.refresh_from_disk().unwrap();
            (outcome, reads() - before)
        };
        assert_eq!(refresh_reads(&mut editor), (Save::Saved, 0), "after open");

        editor.set_text("saved edit".into()).unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
        assert_eq!(
            refresh_reads(&mut editor),
            (Save::Saved, 0),
            "after its own save"
        );

        // Autosave: a copy saves on a worker and hands its stamp back.
        editor.queue_text("autosaved edit".into());
        let mut copy = editor.save_copy().unwrap();
        assert_eq!(copy.save().unwrap(), Save::Saved);
        editor
            .saved_as_queued_at("autosaved edit".into(), copy.disk_stamp())
            .persist()
            .unwrap();
        assert_eq!(
            refresh_reads(&mut editor),
            (Save::Saved, 0),
            "after an autosave"
        );

        // Positive controls: an outside replacement is read and adopted.
        fs::write(root.path().join("other"), "outside").unwrap();
        fs::rename(root.path().join("other"), &path).unwrap();
        assert_eq!(
            refresh_reads(&mut editor),
            (Save::Saved, 1),
            "outside replacement"
        );
        assert_eq!(editor.text(), "outside");
        assert_eq!(
            refresh_reads(&mut editor),
            (Save::Saved, 0),
            "the adopted text"
        );

        // A write in place with the same size and the old mtime: only the
        // ctime tells, and it must be enough.
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, "OUTSIDE").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert_eq!(
            refresh_reads(&mut editor),
            (Save::Saved, 1),
            "same size, mtime set back"
        );
        assert_eq!(editor.text(), "OUTSIDE");

        // A dirty buffer still sees an outside change as a conflict.
        editor.set_text("typing".into()).unwrap();
        assert_eq!(
            refresh_reads(&mut editor),
            (Save::Saved, 0),
            "dirty, file unchanged"
        );
        fs::write(&path, "elsewhere").unwrap();
        assert_eq!(
            refresh_reads(&mut editor),
            (Save::Conflict, 1),
            "dirty, file changed"
        );
    }
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
                    .find(|e| e.file_name().to_string_lossy().starts_with(".okilum-save-"))
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
            assert!(fs::read_dir(&folder)
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(".okilum-save-")));
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
            .starts_with(".okilum-save-")
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
        let inherited = editor.lock.as_ref().unwrap().file.try_clone().unwrap();
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
        let inherited = editor.lock.as_ref().unwrap().file.try_clone().unwrap();
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
            let inherited_editor = editor.lock.as_ref().unwrap().file.try_clone().unwrap();
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
            let inherited_saved_editor = editor.lock.as_ref().unwrap().file.try_clone().unwrap();
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
        const KEY: &str = "OKILUM_CRASH_DRAFT_TEST";
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
    fn content_fallback_refresh_conflict_and_failed_recovery_keep_exact_baselines() {
        crate::change_detection::with_content_checks(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("note.md");
            fs::write(&path, "original").unwrap();
            let mut editor = FileEditor::open(&path, &dir.path().join("state")).unwrap();
            let mtime = fs::metadata(&path).unwrap().modified().unwrap();
            let cheap = DiskStamp::of(&fs::metadata(&path).unwrap());
            let write = |text| {
                fs::write(&path, text).unwrap();
                File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(mtime)
                    .unwrap();
                assert_eq!(
                    cheap,
                    DiskStamp::of(&fs::metadata(&path).unwrap()),
                    "positive control: identical cheap metadata"
                );
            };
            write("external");
            // Failed durable recovery must leave both visible text and save base intact.
            fs::create_dir(&editor.journal).unwrap();
            assert!(editor.refresh_from_disk().is_err());
            assert_eq!(editor.text(), "original");
            fs::remove_dir(&editor.journal).unwrap();
            editor.set_text("my draft".into()).unwrap();
            assert_eq!(editor.refresh_from_disk().unwrap(), Save::Conflict);
            assert_eq!(editor.save().unwrap(), Save::Conflict);
            assert_eq!(editor.text(), "my draft");
            assert_eq!(fs::read_to_string(&path).unwrap(), "external");
            editor.reload().unwrap();
            write("NEW text");
            assert_eq!(editor.refresh_from_disk().unwrap(), Save::Saved);
            assert_eq!(editor.text(), "NEW text");
            editor.set_text("mine now".into()).unwrap();
            write("last one");
            assert_eq!(editor.keep_mine("NEW text").unwrap(), Save::Conflict);
            assert_eq!(fs::read_to_string(&path).unwrap(), "last one");
        });
    }

    #[test]
    fn recovery_discovery_keeps_deleted_source_drafts_and_checks_returned_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("drafts");
        fs::write(&path, "original").unwrap();
        let mut editor = FileEditor::open(&path, &state).unwrap();
        editor
            .set_text("only unsaved copy שלום\r\n".into())
            .unwrap();
        drop(editor);
        let before = fs::read_dir(&state)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<Vec<_>>();
        fs::remove_file(&path).unwrap();
        assert!(FileEditor::has_unsaved_draft(&path, &state).unwrap());
        assert!(!path.exists(), "discovery never recreates the source");
        for (path, bytes) in &before {
            assert_eq!(&fs::read(path).unwrap(), bytes, "discovery is read-only");
        }
        fs::write(&path, "external return").unwrap();
        assert!(FileEditor::has_unsaved_draft(&path, &state).unwrap());
        fs::write(&path, "only unsaved copy שלום\r\n").unwrap();
        assert!(!FileEditor::has_unsaved_draft(&path, &state).unwrap());
        fs::remove_file(&path).unwrap();
        assert!(!FileEditor::has_unsaved_draft(&dir.path().join("other.md"), &state).unwrap());
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
        let journal_before = fs::read(state.join(format!(
            "{:x}.json",
            Sha256::digest(note.as_os_str().as_encoded_bytes())
        )))
        .unwrap();
        let mut recovered = FileEditor::open(&note, &state).unwrap();
        assert!(recovered.dirty());
        assert!(recovered.has_pending_revision());
        assert_eq!(recovered.text(), "proposed שלום\r\n");
        assert!(FileEditor::has_unsaved_draft(&note, &state).unwrap());
        assert_eq!(fs::read(&recovered.journal).unwrap(), journal_before);
        assert_eq!(fs::read_to_string(&note).unwrap(), "proposed שלום\r\n");
        assert_eq!(recovered.save().unwrap(), Save::Saved);
        assert!(!recovered.dirty());
        assert!(!FileEditor::has_unsaved_draft(&note, &state).unwrap());
        drop(recovered);
        let reopened = FileEditor::open(&note, &state).unwrap();
        assert!(!reopened.dirty());
        assert!(!reopened.has_pending_revision());
        assert_eq!(reopened.text(), "proposed שלום\r\n");
        let listing = crate::source_history::list(&state, &root).unwrap();
        assert!(listing
            .versions
            .iter()
            .any(|v| v.text == "base\r\n" && v.protected));
    }
    #[test]
    fn windows_editor_acknowledged_journal_keeps_a_new_disk_revision() {
        let fixture = tempfile::tempdir().unwrap();
        let note = fixture.path().join("Note.md");
        let state = fixture.path().join("drafts");
        fs::write(&note, "base\r\n").unwrap();
        let mut editor = FileEditor::open(&note, &state).unwrap();
        editor.set_text("saved שלום\r\n".into()).unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
        drop(editor);
        assert!(!FileEditor::has_unsaved_draft(&note, &state).unwrap());
        fs::write(&note, "external after acknowledged save\r\n").unwrap();
        let reopened = FileEditor::open(&note, &state).unwrap();
        assert_eq!(reopened.text(), "external after acknowledged save\r\n");
        assert!(!reopened.dirty());
        assert!(!reopened.has_pending_revision());
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
                    // ReFS can refuse REPLACE_EXISTING over an open destination.
                    // A racing publisher can still rename the old inode away
                    // and publish a new inode into the vacant canonical name.
                    fs::rename(&note, root.join("external-old.md")).unwrap();
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

#[cfg(unix)]
fn with_cleanup(error: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => error.context(format!("Save cleanup needs attention: {cleanup:#}")),
    }
}

#[cfg(unix)]
fn clean_owned_temp(directory: &File, name: &std::ffi::OsStr, temp: &File) -> Result<()> {
    use rustix::fs::{statat, unlinkat, AtFlags};
    let metadata = temp.metadata()?;
    let current = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(current) => current,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        crate::unix_publish::same_identity(&current, &metadata),
        "Temporary save replaced; retained for inspection"
    );
    unlinkat(directory, name, AtFlags::empty())?;
    directory.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn clean_recovery_file(directory: &File, name: &std::ffi::OsStr, expected: &[u8]) -> Result<()> {
    use rustix::fs::{openat, unlinkat, AtFlags, Mode, OFlags};
    let fd = match openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut file = File::from(fd);
    let metadata = file.metadata()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    anyhow::ensure!(
        metadata.is_file() && bytes == expected,
        "Recovery file changed; retained for inspection"
    );
    let current = rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
    anyhow::ensure!(
        crate::unix_publish::same_identity(&current, &metadata),
        "Recovery file replaced; retained for inspection"
    );
    unlinkat(directory, name, AtFlags::empty())?;
    directory.sync_all()?;
    Ok(())
}

#[cfg(all(test, unix))]
mod network_save_tests {
    use super::*;
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum PublicationFailure {
        LostReply,
        OutcomeProbe,
        DirectorySync,
    }
    thread_local! {
        pub(super) static FAIL_PUBLICATION: std::cell::Cell<Option<PublicationFailure>> = const { std::cell::Cell::new(None) };
    }
    pub(super) fn fail_publication<T>(failure: PublicationFailure, body: impl FnOnce() -> T) -> T {
        struct Reset(Option<PublicationFailure>);
        impl Drop for Reset {
            fn drop(&mut self) {
                FAIL_PUBLICATION.with(|v| v.set(self.0));
            }
        }
        let _reset = Reset(FAIL_PUBLICATION.with(|v| v.replace(Some(failure))));
        body()
    }

    #[test]
    fn unconfirmed_publication_keeps_draft_preimage_and_revision() {
        for failure in [
            PublicationFailure::LostReply,
            PublicationFailure::OutcomeProbe,
            PublicationFailure::DirectorySync,
        ] {
            crate::unix_publish::inject(rustix::io::Errno::NOTSUP, || {
                let fixture = tempfile::tempdir().unwrap();
                let root = fixture.path().join("vault");
                fs::create_dir(&root).unwrap();
                let note = root.join("note.md");
                fs::write(&note, b"base\r\n").unwrap();
                let mut editor = FileEditor::open(&note, &fixture.path().join("state")).unwrap();
                editor.set_text("mine\r\n".into()).unwrap();
                let error = fail_publication(failure, || editor.save()).unwrap_err();
                assert_eq!(
                    error.downcast_ref::<UnconfirmedSave>(),
                    Some(&match failure {
                        PublicationFailure::DirectorySync => UnconfirmedSave::Published,
                        _ => UnconfirmedSave::UnknownPublication,
                    })
                );
                let unknown_before_publish = failure == PublicationFailure::OutcomeProbe;
                assert_eq!(
                    fs::read(&note).unwrap(),
                    if unknown_before_publish {
                        b"base\r\n".as_slice()
                    } else {
                        b"mine\r\n".as_slice()
                    }
                );
                assert!(editor.dirty());
                assert_eq!(editor.draft.base, "base\r\n");
                let draft: Draft =
                    serde_json::from_slice(&fs::read(&editor.journal).unwrap()).unwrap();
                assert_eq!(draft.base, "base\r\n");
                assert_eq!(draft.text, "mine\r\n");
                let artifacts: Vec<_> = fs::read_dir(&root)
                    .unwrap()
                    .filter_map(|e| {
                        let e = e.unwrap();
                        e.file_name()
                            .to_string_lossy()
                            .starts_with(".okilum-save-")
                            .then_some(e.path())
                    })
                    .collect();
                assert_eq!(artifacts.len(), if unknown_before_publish { 2 } else { 1 });
                assert!(artifacts
                    .iter()
                    .any(|p| fs::read(p).unwrap() == b"base\r\n"));
                drop(editor);
                assert!(draft.text != draft.base);
            });
        }
    }

    thread_local! {
        pub(super) static LINK_ERROR: std::cell::Cell<Option<rustix::io::Errno>> = const { std::cell::Cell::new(None) };
        pub(super) static ERROR_AFTER_LINK: std::cell::Cell<Option<rustix::io::Errno>> = const { std::cell::Cell::new(None) };
        pub(super) static DELETE_AFTER_LINK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static ALTER_PREIMAGE_AFTER_PUBLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    pub(super) type Boundary = std::cell::RefCell<Option<Box<dyn FnOnce()>>>;
    thread_local! {
        pub(super) static BEFORE_CAPTURE: Boundary = const { std::cell::RefCell::new(None) };
        pub(super) static AFTER_CAPTURE: Boundary = const { std::cell::RefCell::new(None) };
    }
    pub(super) fn at_boundary(hook: &'static std::thread::LocalKey<Boundary>) {
        if let Some(body) = hook.with(|v| v.borrow_mut().take()) {
            body();
        }
    }
    #[test]
    fn interrupted_actual_capture_keeps_both_versions_and_restores_only_the_source() {
        for raw in [false, true] {
            crate::unix_publish::inject(rustix::io::Errno::INVAL, || {
                let fixture = tempfile::tempdir().unwrap();
                let root = fixture.path().join("vault");
                fs::create_dir(&root).unwrap();
                let note = root.join("note.md");
                let state = fixture.path().join("state");
                let before = if raw {
                    b"base\xff\r\n".as_slice()
                } else {
                    b"base\r\n".as_slice()
                };
                fs::write(&note, before).unwrap();
                AFTER_CAPTURE.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(|| {
                        panic!("simulated process interruption after capture")
                    }))
                });
                let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if raw {
                        FileEditor::replace_bytes(&note, &state, before, b"mine\r\n").unwrap();
                    } else {
                        let mut editor = FileEditor::open(&note, &state).unwrap();
                        editor.set_text("mine\r\n".into()).unwrap();
                        editor.save().unwrap();
                    }
                }));
                assert!(interrupted.is_err());
                assert!(!note.exists()); // positive control: the actual capture ran
                let inspection = FileEditor::inspect_interrupted_saves(&note, &state)
                    .unwrap()
                    .pop()
                    .unwrap();
                assert_eq!(inspection.state(), InterruptedSaveState::SourceMissing);
                assert_eq!(inspection.original_bytes(), before);
                assert!(fs::read_dir(&root)
                    .unwrap()
                    .any(|entry| fs::read(entry.unwrap().path()).unwrap() == b"mine\r\n"));
                inspection.restore_missing_source().unwrap();
                assert_eq!(fs::read(&note).unwrap(), before);
                drop(inspection);
                FileEditor::inspect_interrupted_saves(&note, &state)
                    .unwrap()
                    .pop()
                    .unwrap()
                    .keep_current()
                    .unwrap();
                assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
                // Both proposals remain in durable state, even after alias cleanup.
                let history = crate::source_history::list(&state, &root).unwrap();
                assert!(history.versions.iter().any(|v| v.bytes() == before));
                if raw {
                    let record = fs::read_dir(state.join("source-history"))
                        .unwrap()
                        .map(|v| v.unwrap().path())
                        .find(|p| crate::source_history::RawRecord::is_path(p))
                        .unwrap();
                    assert_eq!(
                        crate::source_history::RawRecord::load(&record)
                            .unwrap()
                            .after,
                        b"mine\r\n"
                    );
                } else {
                    let journal = fs::read_dir(&state)
                        .unwrap()
                        .map(|v| v.unwrap().path())
                        .find(|p| p.extension().is_some_and(|ext| ext == "json"))
                        .unwrap();
                    let draft: Draft = serde_json::from_slice(&fs::read(journal).unwrap()).unwrap();
                    assert_eq!(draft.text, "mine\r\n");
                    assert_ne!(draft.text, draft.base);
                }
            });
        }
    }
    #[test]
    fn compatible_save_never_overwrites_a_publication_boundary_replacement() {
        for error in [
            rustix::io::Errno::NOTSUP,
            rustix::io::Errno::INVAL,
            rustix::io::Errno::NOSYS,
        ] {
            for after_capture in [false, true] {
                crate::unix_publish::inject(error, || {
                    let fixture = tempfile::tempdir().unwrap();
                    let root = fixture.path().join("vault");
                    fs::create_dir(&root).unwrap();
                    let note = root.join("note.md");
                    fs::write(&note, b"base").unwrap();
                    let state = fixture.path().join("state");
                    let mut editor = FileEditor::open(&note, &state).unwrap();
                    editor.set_text("mine".into()).unwrap();
                    let path = note.clone();
                    let replacement = root.join("external.tmp");
                    fs::write(&replacement, b"newer external").unwrap();
                    let identity = fs::metadata(&replacement).unwrap().ino();
                    let hook = if after_capture {
                        &AFTER_CAPTURE
                    } else {
                        &BEFORE_CAPTURE
                    };
                    hook.with(|v| {
                        *v.borrow_mut() =
                            Some(Box::new(move || fs::rename(replacement, path).unwrap()))
                    });
                    let result = editor.save();
                    assert_eq!(result.unwrap(), Save::Conflict);
                    assert_eq!(fs::read(&note).unwrap(), b"newer external");
                    assert_eq!(fs::metadata(&note).unwrap().ino(), identity);
                    assert!(editor.dirty());
                    assert_eq!(editor.text(), "mine");
                    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
                });
            }
        }
    }
    fn assert_unpublished(editor: &FileEditor, root: &Path, note: &Path) {
        assert_eq!(fs::read(note).unwrap(), b"base");
        assert_eq!(fs::metadata(note).unwrap().nlink(), 1);
        assert!(fs::read_dir(root).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".okilum-save-")));
        assert_eq!(editor.text(), "mine");
        assert!(editor.dirty());
        FileEditor::open(note, &root.join("reopen-state")).unwrap();
    }
    #[test]
    fn link_failure_leaves_no_service_files() {
        for flag in [
            rustix::io::Errno::NOTSUP,
            rustix::io::Errno::INVAL,
            rustix::io::Errno::NOSYS,
        ] {
            for error in [rustix::io::Errno::PERM, rustix::io::Errno::NOTSUP] {
                crate::unix_publish::inject(flag, || {
                    let root = tempfile::tempdir().unwrap();
                    let note = root.path().join("note.md");
                    fs::write(&note, b"base").unwrap();
                    let mut editor = FileEditor::open(&note, &root.path().join("state")).unwrap();
                    editor.set_text("mine".into()).unwrap();
                    LINK_ERROR.with(|v| v.set(Some(error)));
                    let failure = editor.save().unwrap_err();
                    assert_eq!(failure.downcast_ref::<rustix::io::Errno>(), Some(&error));
                    assert_unpublished(&editor, root.path(), &note);
                    assert_eq!(editor.save().unwrap(), Save::Saved);
                    assert_eq!(fs::read(&note).unwrap(), b"mine");
                });
            }
        }
    }
    #[test]
    fn failure_after_link_keeps_note_editable() {
        for flag in [
            rustix::io::Errno::NOTSUP,
            rustix::io::Errno::INVAL,
            rustix::io::Errno::NOSYS,
        ] {
            crate::unix_publish::inject(flag, || {
                let root = tempfile::tempdir().unwrap();
                let note = root.path().join("note.md");
                fs::write(&note, b"base").unwrap();
                let mut editor = FileEditor::open(&note, &root.path().join("state")).unwrap();
                editor.set_text("mine".into()).unwrap();
                ERROR_AFTER_LINK.with(|v| v.set(Some(rustix::io::Errno::IO)));
                let error = editor.save().unwrap_err();
                assert_eq!(
                    error.downcast_ref::<rustix::io::Errno>(),
                    Some(&rustix::io::Errno::IO)
                );
                assert_unpublished(&editor, root.path(), &note);
                assert_eq!(editor.save().unwrap(), Save::Saved);
                assert_eq!(fs::read(&note).unwrap(), b"mine");
            });
        }
    }
    #[test]
    fn exchange_io_error_retains_draft_and_staging_without_guessing_outcome() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir(&root).unwrap();
        let note = root.join("note.md");
        fs::write(&note, b"base").unwrap();
        let mut editor = FileEditor::open(&note, &fixture.path().join("state")).unwrap();
        editor.set_text("mine".into()).unwrap();
        crate::unix_publish::inject(rustix::io::Errno::IO, || {
            let error = editor.save().unwrap_err();
            assert_eq!(
                error.downcast_ref::<UnconfirmedSave>(),
                Some(&UnconfirmedSave::UnknownPublication)
            );
        });
        assert_eq!(fs::read(&note).unwrap(), b"base");
        assert!(editor.dirty());
        let staging = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".okilum-save-")
            })
            .unwrap();
        assert_eq!(fs::read(staging).unwrap(), b"mine");
        assert!(editor.journal.exists());
    }

    #[test]
    fn denied_exchange_removes_unpublished_temp() {
        let root = tempfile::tempdir().unwrap();
        let note = root.path().join("note.md");
        fs::write(&note, b"base").unwrap();
        let mut editor = FileEditor::open(&note, &root.path().join("state")).unwrap();
        editor.set_text("mine".into()).unwrap();
        crate::unix_publish::inject(rustix::io::Errno::ACCESS, || {
            let error = editor.save().unwrap_err();
            assert_eq!(
                error.downcast_ref::<rustix::io::Errno>(),
                Some(&rustix::io::Errno::ACCESS)
            );
        });
        assert_unpublished(&editor, root.path(), &note);
        assert_eq!(editor.save().unwrap(), Save::Saved);
    }
    #[test]
    fn unsupported_exchange_handles_deletion_and_reports_publication_truthfully() {
        for error in [
            rustix::io::Errno::NOTSUP,
            rustix::io::Errno::INVAL,
            rustix::io::Errno::NOSYS,
        ] {
            for published in [false, true] {
                crate::unix_publish::inject(error, || {
                    let root = tempfile::tempdir().unwrap();
                    let note = root.path().join("note.md");
                    fs::write(&note, b"base").unwrap();
                    let mut editor = FileEditor::open(&note, &root.path().join("state")).unwrap();
                    editor.set_text("mine".into()).unwrap();
                    DELETE_AFTER_LINK.with(|v| v.set(!published));
                    ALTER_PREIMAGE_AFTER_PUBLISH.with(|v| v.set(published));
                    let result = editor.save();
                    if published {
                        assert!(result.unwrap_err().is::<PublishedSaveNeedsAttention>());
                        assert_eq!(fs::read(&note).unwrap(), b"mine");
                    } else {
                        assert_eq!(result.unwrap(), Save::Conflict);
                        assert!(!note.exists());
                        assert!(fs::read_dir(root.path()).unwrap().all(|e| !e
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".okilum-save-")));
                    }
                    assert_eq!(editor.text(), "mine");
                    assert!(editor.dirty());
                });
            }
        }
    }
    #[test]
    fn unsupported_exchange_rechecks_external_write_at_publication_boundary() {
        for error in [
            rustix::io::Errno::NOTSUP,
            rustix::io::Errno::INVAL,
            rustix::io::Errno::NOSYS,
        ] {
            crate::unix_publish::inject(error, || {
                let root = tempfile::tempdir().unwrap();
                let note = root.path().join("note.md");
                fs::write(&note, b"base").unwrap();
                let mut editor = FileEditor::open(&note, &root.path().join("state")).unwrap();
                editor.set_text("mine".into()).unwrap();
                let outcome = editor
                    .save_before_exchange(|| fs::write(&note, b"external").unwrap())
                    .unwrap();
                assert_eq!(outcome, Save::Conflict);
                assert_eq!(fs::read(&note).unwrap(), b"external");
                assert_eq!(editor.text(), "mine");
                assert!(fs::read_dir(root.path()).unwrap().all(|e| !e
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".okilum-save-")));
            });
        }
    }
}

#[cfg(test)]
mod persistence_order_tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, FileEditor) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("state");
        fs::write(&path, "\u{feff}base\r\n").unwrap();
        let editor = FileEditor::open(&path, &state).unwrap();
        (dir, path, state, editor)
    }

    #[test]
    fn issue2405_receipt_requires_confirmed_exact_bytes_and_survives_worker_adoption() {
        let (_dir, path, _state, mut editor) = fixture();
        let bytes = "\u{feff}saved\r\n";
        let digest: [u8; 32] = Sha256::digest(bytes.as_bytes()).into();
        assert!(!editor.owns_publication(&digest));
        editor.queue_text(bytes.into());
        let mut worker = editor.save_copy().unwrap();
        assert!(
            !editor.owns_publication(&digest),
            "queued text is not a receipt"
        );
        assert_eq!(worker.save().unwrap(), Save::Saved);
        assert!(
            editor.owns_publication(&digest),
            "receipt arrives before UI adopts worker"
        );
        assert!(!editor.owns_publication(&Sha256::digest(b"different").into()));
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
        fs::write(&path, "external reload").unwrap();
        editor.reload().unwrap();
        assert!(
            !editor.owns_publication(&digest),
            "reload revokes an older save receipt"
        );
    }

    #[test]
    fn issue2405_superseded_refresh_reservation_does_not_strand_the_ui_generation() {
        let (_dir, path, state, mut editor) = fixture();
        let first = editor.publication_copy().unwrap();
        fs::write(&path, "first external").unwrap();
        let stale = first.refresh_publication().unwrap().unwrap();
        // Supersede the completed worker before its foreground adoption. Its
        // journal reservation is newer than the still-visible editor's generation.
        fs::write(&path, "latest external").unwrap();
        let second = editor.publication_copy().unwrap();
        let latest = second
            .refresh_publication()
            .unwrap()
            .expect("refresh must converge, not retry forever");
        assert_eq!(latest.editor.text(), "latest external");
        assert_eq!(latest.outcome, Save::Saved);
        drop(stale);
        drop(editor);
        drop(latest);
        assert_eq!(
            FileEditor::open(&path, &state).unwrap().text(),
            "latest external"
        );
    }

    #[test]
    fn issue2405_stale_refresh_cannot_publish_over_new_input_or_recovery() {
        let (_dir, path, state, mut editor) = fixture();
        let stale = editor.save_copy().unwrap();
        editor.queue_text("latest input".into()).persist().unwrap();
        fs::write(&path, "external").unwrap();
        assert!(stale.refresh_publication().unwrap().is_none());
        drop(editor);
        let recovered = FileEditor::open(&path, &state).unwrap();
        assert_eq!(recovered.text(), "latest input");
        assert!(recovered.dirty());
    }

    #[test]
    fn issue2404_stale_save_worker_cannot_replace_latest_recovery_or_canonical_save() {
        let (_dir, path, state, mut editor) = fixture();
        editor
            .queue_text("\u{feff}older\r\n".into())
            .persist()
            .unwrap();
        let mut old = editor.save_copy().unwrap();
        let latest = "\u{feff}latest typed שלום 🧠\r\n";
        editor.queue_text(latest.into()).persist().unwrap();
        // A save is allowed to publish its captured revision, but its journal
        // acknowledgements must never replace the later unsaved generation.
        assert_eq!(old.save().unwrap(), Save::Saved);
        drop(old);
        drop(editor);
        let mut recovered = FileEditor::open(&path, &state).unwrap();
        assert_eq!(recovered.text(), latest);
        assert!(recovered.dirty());
        assert_eq!(fs::read(&path).unwrap(), "\u{feff}older\r\n".as_bytes());
        // Restart recovery offers a conflict rather than overwriting the file.
        assert_eq!(recovered.save().unwrap(), Save::Conflict);
        assert_eq!(
            recovered.keep_mine("\u{feff}older\r\n").unwrap(),
            Save::Saved
        );
        let mut stale = recovered.save_copy().unwrap();
        recovered.set_text("\u{feff}newest\r\n".into()).unwrap();
        assert_eq!(recovered.save().unwrap(), Save::Saved);
        assert_eq!(stale.save().unwrap(), Save::Conflict);
        assert_eq!(fs::read(&path).unwrap(), "\u{feff}newest\r\n".as_bytes());
    }

    #[test]
    fn issue2404_save_adopts_only_confirmed_in_flight_base_and_flushes_latest() {
        let (_dir, path, state, mut editor) = fixture();
        editor.set_text("\u{feff}autosave\r\n".into()).unwrap();
        let mut copy = editor.save_copy().unwrap();
        let (started, receive) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                copy.save_before_exchange(|| {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                })
            });
            receive.recv().unwrap();
            editor.queue_text("\u{feff}typed during save\r\n".into());
            editor.flush_draft().unwrap();
            release.send(()).unwrap();
            // Explicit Save/close takes the same ordering lock; it must adopt
            // the old save's confirmed base and publish this newer revision.
            assert_eq!(editor.save().unwrap(), Save::Saved);
            assert_eq!(worker.join().unwrap().unwrap(), Save::Saved);
        });
        drop(editor);
        let recovered = FileEditor::open(&path, &state).unwrap();
        assert_eq!(recovered.text(), "\u{feff}typed during save\r\n");
        assert!(!recovered.dirty());
        assert_eq!(fs::read(&path).unwrap(), recovered.text().as_bytes());
    }

    #[test]
    fn issue2404_saved_generation_retires_delayed_recovery_of_its_old_base() {
        let (_dir, path, state, mut editor) = fixture();
        let delayed = editor.queue_text("\u{feff}saved\r\n".into());
        assert_eq!(editor.save().unwrap(), Save::Saved);
        delayed.persist().unwrap();
        drop(editor);
        let recovered = FileEditor::open(&path, &state).unwrap();
        assert!(
            !recovered.dirty(),
            "late pre-save recovery cannot restore an obsolete base"
        );
        assert_eq!(recovered.text(), "\u{feff}saved\r\n");
    }

    #[test]
    fn issue2404_flush_supersedes_debounce_and_failure_is_retryable() {
        let (_dir, path, state, mut editor) = fixture();
        let old = editor.queue_text("old".into());
        let latest = editor.queue_text("\u{feff}latest before close\r\n".into());
        // A directory at the journal path makes atomic publication fail.
        fs::create_dir(&editor.journal).unwrap();
        assert!(latest.persist().is_err());
        assert_eq!(editor.text(), "\u{feff}latest before close\r\n");
        fs::remove_dir(&editor.journal).unwrap();
        editor.flush_draft().unwrap();
        old.persist().unwrap();
        drop(editor);
        let recovered = FileEditor::open(&path, &state).unwrap();
        assert_eq!(recovered.text(), "\u{feff}latest before close\r\n");
        assert_eq!(fs::read(&path).unwrap(), "\u{feff}base\r\n".as_bytes());
    }

    #[test]
    fn issue2404_process_exit_restores_latest_acknowledged_crlf_bom_revision() {
        const CHILD: &str = "OKILUM_2404_CRASH_FIXTURE";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            let mut editor = FileEditor::open(&root.join("note.md"), &root.join("state")).unwrap();
            let old = editor.queue_text("obsolete".into());
            editor
                .queue_snapshot(Text::from("\u{feff}last acknowledged 🧠\r\n"))
                .persist()
                .unwrap();
            old.persist().unwrap();
            // Skip every destructor/lifecycle guard, as with a process crash.
            std::process::exit(0);
        }
        let (dir, path, state, editor) = fixture();
        drop(editor);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "file_editor::persistence_order_tests::issue2404_process_exit_restores_latest_acknowledged_crlf_bom_revision"])
            .env(CHILD, dir.path()).status().unwrap();
        assert!(status.success(), "crash writer positive control must run");
        let recovered = FileEditor::open(&path, &state).unwrap();
        assert_eq!(recovered.text(), "\u{feff}last acknowledged 🧠\r\n");
        assert_eq!(fs::read(&path).unwrap(), "\u{feff}base\r\n".as_bytes());
    }

    #[test]
    fn issue2404_reload_supersedes_a_save_copy_that_has_not_started() {
        let (_dir, path, state, mut editor) = fixture();
        editor.set_text("older save".into()).unwrap();
        let mut older = editor.save_copy().unwrap();
        editor.reload().unwrap();
        assert_eq!(older.save().unwrap(), Save::Conflict);
        assert_eq!(fs::read(&path).unwrap(), "\u{feff}base\r\n".as_bytes());
        drop(older);
        drop(editor);
        assert!(!FileEditor::open(&path, &state)
            .unwrap()
            .has_pending_revision());
    }

    #[test]
    fn issue2404_clean_save_shares_the_equal_native_revision_after_undo() {
        let (_dir, _path, _state, mut editor) = fixture();
        let source = Text::from_rope(ropey::Rope::from_str("\u{feff}base\r\n"));
        editor.queue_snapshot(source.clone());
        assert!(editor.has_pending_revision());
        assert!(!editor.dirty());
        assert_eq!(editor.save().unwrap(), Save::Saved);
        assert!(!editor.has_pending_revision());
        assert!(editor.snapshot().same(&source));
    }

    #[test]
    fn issue2404_queue_and_save_copy_share_native_revisions_at_all_sizes() {
        use crate::source_projection::revision::{reset_work, take_work};
        for bytes in [25 * 1024, 130 * 1024, 1024 * 1024] {
            let (_dir, _path, _state, mut editor) = fixture();
            let original = Text::from_rope(ropey::Rope::from_str(&"prose\r\n".repeat(bytes / 7)));
            let next = original.replace(0..1, "x");
            reset_work();
            let pending = editor.queue_snapshot(next.clone());
            let copy = editor.save_copy().unwrap();
            assert!(copy.snapshot().same(&next));
            let work = take_work();
            assert_eq!(work.flattened_bytes, 0, "{bytes}: {work:?}");
            assert_eq!(work.scanned_bytes, 0, "{bytes}: {work:?}");
            // Positive control traverses the very same shared snapshot.
            reset_work();
            let _ = copy.text();
            assert!(take_work().flattened_bytes >= bytes - 7);
            pending.persist().unwrap();
        }
    }
}

// Kept self-contained so the same crash regression can be overlaid on the
// pre-fix source as a real data-loss positive control.
#[cfg(test)]
mod issue2404_crash_revert_control {
    use super::*;
    #[test]
    fn process_exit_after_older_save_restores_latest_undo() {
        const CHILD: &str = "OKILUM_2404_CRASH_REVERT_FIXTURE";
        const BASE: &str = "\u{feff}original שלום\r\n";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            let mut editor = FileEditor::open(&root.join("note.md"), &root.join("state")).unwrap();
            editor
                .set_text("\u{feff}older save snapshot\r\n".into())
                .unwrap();
            let mut older = editor.save_copy().unwrap();
            // The latest input is an Undo to the original base: byte equality
            // must not make it disposable while this older save may publish.
            editor.queue_text(BASE.into()).persist().unwrap();
            assert_eq!(older.save().unwrap(), Save::Saved);
            std::process::exit(0);
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        let state = dir.path().join("state");
        fs::write(&path, BASE).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "file_editor::issue2404_crash_revert_control::process_exit_after_older_save_restores_latest_undo"])
            .env(CHILD, dir.path()).status().unwrap();
        assert!(status.success(), "crash writer positive control must run");
        let offered = FileEditor::has_unsaved_draft(&path, &state).unwrap();
        let mut recovered = FileEditor::open(&path, &state).unwrap();
        assert_eq!(
            recovered.text(),
            BASE,
            "latest typed Undo must survive an older save and process exit"
        );
        assert!(offered, "equal-base input intent is still recoverable");
        assert_eq!(recovered.refresh_from_disk().unwrap(), Save::Conflict);
        assert_eq!(
            recovered.text(),
            BASE,
            "refresh must not replace the pending Undo"
        );
        assert_eq!(recovered.save().unwrap(), Save::Conflict);
        assert_eq!(
            recovered
                .keep_mine("\u{feff}older save snapshot\r\n")
                .unwrap(),
            Save::Saved
        );
        assert_eq!(fs::read(&path).unwrap(), BASE.as_bytes());
    }
}

#[cfg(all(test, unix))]
mod local_writer_admission_tests {
    use super::*;
    use std::{
        io::Write,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    #[test]
    fn local_writer_admission_actual_save_reaches_exchange_before_drain_can_finish() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("vault");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("note.md"), b"original").unwrap();
        let a = okilum_write_authority::Authority::for_root(&root).unwrap();
        let ready = t.path().join("save-ready");
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "file_editor::local_writer_admission_tests::admitted_save_process",
                "--nocapture",
            ])
            .env("LOCAL_SAVE_ROOT", &root)
            .env("LOCAL_SAVE_DRAFTS", t.path().join("drafts"))
            .env("LOCAL_SAVE_READY", &ready)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        struct Owned(std::process::Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Owned(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready.exists() {
            assert!(child.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "save hook never fired");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut hold = a.begin_drain("actual-editor-save").unwrap();
        let token = hold.token().clone();
        assert!(!hold.try_hold(&token).unwrap());
        assert!(crate::note_files::create(&root, Path::new("next.md")).is_err());
        assert_eq!(fs::read(root.join("note.md")).unwrap(), b"original");
        child.0.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "admitted save blocked behind its drain"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            fs::read(root.join("note.md")).unwrap(),
            b"saved by another process"
        );
        assert!(hold.try_hold(&token).unwrap());
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        crate::note_files::create(&root, Path::new("next.md")).unwrap();
    }
    #[test]
    #[ignore = "production save subprocess, selected by parent test"]
    fn admitted_save_process() {
        let root = PathBuf::from(std::env::var_os("LOCAL_SAVE_ROOT").unwrap());
        let drafts = PathBuf::from(std::env::var_os("LOCAL_SAVE_DRAFTS").unwrap());
        let ready = PathBuf::from(std::env::var_os("LOCAL_SAVE_READY").unwrap());
        let mut editor = FileEditor::open(&root.join("note.md"), &drafts).unwrap();
        editor.set_text("saved by another process".into()).unwrap();
        assert_eq!(
            editor
                .save_before_exchange(|| {
                    fs::write(ready, b"real FileEditor save reached exchange hook").unwrap();
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).unwrap();
                    assert_eq!(line, "go\n");
                })
                .unwrap(),
            Save::Saved
        );
    }
}

#[cfg(test)]
mod local_writer_review_tests {
    use super::*;
    #[test]
    fn windows_local_writer_review_vault_discovery_promotes_subfolder_first() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let sub = root.join("Sub");
        let sibling = root.join("Other");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir(&sibling).unwrap();
        fs::write(sub.join("note.md"), b"original").unwrap();
        let mut editor =
            FileEditor::open(&sub.join("note.md"), &temp.path().join("drafts")).unwrap();
        crate::Vault::scan_metadata(&root).unwrap();
        let vault = okilum_write_authority::Authority::for_root(&root).unwrap();
        assert_eq!(
            okilum_write_authority::Authority::for_root(&sibling)
                .unwrap()
                .root(),
            vault.root()
        );
        editor.queue_text("promoted editor".into());
        assert_eq!(editor.save().unwrap(), Save::Saved);
        let hold = vault.begin_drain("promoted-root").unwrap();
        editor.queue_text("held draft".into());
        assert!(editor.save().is_err());
        assert!(okilum_write_authority::Authority::for_root(&sibling)
            .unwrap()
            .admit()
            .is_err());
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
    }

    #[test]
    fn windows_local_writer_review_clean_save_during_hold_and_single_draft_flush() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir(&root).unwrap();
        let path = root.join("note.md");
        fs::write(&path, b"original").unwrap();
        let authority = okilum_write_authority::Authority::for_root(&root).unwrap();
        let mut editor = FileEditor::open(&path, &temp.path().join("drafts")).unwrap();
        let hold = authority.begin_drain("clean-navigation").unwrap();
        JOURNAL_COMMITS.with(|commits| commits.borrow_mut().clear());
        assert_eq!(editor.save().unwrap(), Save::Saved);
        assert!(JOURNAL_COMMITS.with(|commits| commits.borrow().is_empty()));
        editor.queue_text("held draft".into());
        assert!(editor.save().is_err());
        assert_eq!(
            JOURNAL_COMMITS.with(|commits| commits.borrow().clone()),
            vec![false]
        );
        assert_eq!(fs::read(&path).unwrap(), b"original");
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        JOURNAL_COMMITS.with(|commits| commits.borrow_mut().clear());
        assert_eq!(editor.save().unwrap(), Save::Saved);
        // One recovery flush before publication, one clean acknowledgement.
        // No duplicate pre-publication temp-file/fsync sequence was added.
        assert_eq!(
            JOURNAL_COMMITS.with(|commits| commits.borrow().clone()),
            vec![false, true]
        );
        assert_eq!(fs::read(&path).unwrap(), b"held draft");
        editor.queue_text("ordinary autosave".into());
        JOURNAL_COMMITS.with(|commits| commits.borrow_mut().clear());
        assert_eq!(editor.save().unwrap(), Save::Saved);
        assert_eq!(
            JOURNAL_COMMITS.with(|commits| commits.borrow().clone()),
            vec![false, true]
        );
    }
}

/// #2817: an automatic Edit opens without taking ownership of the note.
#[cfg(test)]
mod deferred_open_tests {
    use super::*;

    fn note() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir(&root).unwrap();
        let path = root.join("note.md");
        fs::write(&path, b"# Note\n\nbody\n").unwrap();
        let state = temp.path().join("drafts");
        (temp, path, state)
    }

    fn busy(error: &anyhow::Error) -> bool {
        format!("{error:#}").contains(BEING_EDITED)
    }

    #[test]
    fn deferred_open_owns_nothing_writes_nothing_and_leaves_the_note_editable_elsewhere() {
        let (_temp, path, state) = note();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let authority =
            okilum_write_authority::Authority::for_root(path.parent().unwrap()).unwrap();
        // A held local-writer drain refuses every writer operation.
        let hold = authority.begin_drain("deferred-open").unwrap();
        let mut deferred = FileEditor::open_deferred(&path, &state).unwrap();
        assert!(!deferred.owns_lock());
        assert!(!deferred.dirty() && !deferred.has_pending_revision());
        assert_eq!(deferred.text(), "# Note\n\nbody\n");
        // Clean navigation/quit paths: no admission, no journal, no folder.
        assert_eq!(deferred.save().unwrap(), Save::Saved);
        deferred.flush_draft().unwrap();
        deferred.recovery_write().persist().unwrap();
        assert!(!state.exists(), "no recovery folder or journal is created");
        // Another window can take the note while this one only shows it.
        let other = FileEditor::open(&path, &state).unwrap();
        assert!(
            other.owns_lock(),
            "positive control: an explicit Edit owns it"
        );
        drop(other);
        // An edit without ownership is refused and never reaches disk.
        deferred.queue_text("unowned".into()).persist().unwrap_err();
        deferred.save().unwrap_err();
        assert_eq!(fs::read(&path).unwrap(), b"# Note\n\nbody\n");
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        assert!(
            fs::read_dir(&state).unwrap().all(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "lock")),
            "only the other editor's lock file, never a journal"
        );
        let token = hold.token().clone();
        hold.release(&token).unwrap();
    }

    #[test]
    fn deferred_open_refuses_a_note_another_editor_holds() {
        let (_temp, path, state) = note();
        let owner = FileEditor::open(&path, &state).unwrap();
        let error = FileEditor::open_deferred(&path, &state).err().unwrap();
        assert!(busy(&error), "{error:#}");
        drop(owner);
        assert!(FileEditor::open_deferred(&path, &state).is_ok());
    }

    #[test]
    fn first_edit_takes_ownership_or_refuses_when_another_window_took_it() {
        let (_temp, path, state) = note();
        let mut deferred = FileEditor::open_deferred(&path, &state).unwrap();
        deferred.acquire_lock().unwrap();
        assert!(deferred.owns_lock());
        let error = FileEditor::open(&path, &state).err().unwrap();
        assert!(busy(&error), "the owner now excludes others: {error:#}");
        deferred.set_text("# Note\n\nedited\n".into()).unwrap();
        assert_eq!(deferred.save().unwrap(), Save::Saved);
        assert_eq!(fs::read(&path).unwrap(), b"# Note\n\nedited\n");
        drop(deferred);

        let mut late = FileEditor::open_deferred(&path, &state).unwrap();
        let owner = FileEditor::open(&path, &state).unwrap();
        let error = late.acquire_lock().unwrap_err();
        assert!(busy(&error), "{error:#}");
        assert!(!late.owns_lock());
        drop(owner);
    }

    #[test]
    fn deferred_open_refuses_an_unsaved_recovery_draft() {
        let (_temp, path, state) = note();
        let mut explicit = FileEditor::open(&path, &state).unwrap();
        explicit.set_text("unsaved".into()).unwrap();
        drop(explicit);
        assert!(FileEditor::open_deferred(&path, &state).is_err());
        assert_eq!(FileEditor::open(&path, &state).unwrap().text(), "unsaved");
    }

    #[cfg(unix)]
    #[test]
    fn deferred_open_refuses_a_note_that_cannot_be_saved_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, path, state) = note();
        let folder = path.parent().unwrap().to_owned();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        let result = FileEditor::open_deferred(&path, &state);
        let writable = rustix::fs::access(&folder, rustix::fs::Access::WRITE_OK).is_ok();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        if writable {
            return; // Running as a user that ignores permissions.
        }
        let error = result
            .err()
            .expect("a read-only folder is not editable in place");
        assert!(error.downcast_ref::<NotWritable>().is_some(), "{error:#}");
        assert!(
            FileEditor::open_deferred(&path, &state).is_ok(),
            "positive control"
        );
    }
}
