//! Vault change watcher (#6).
//!
//! v0 is a reader while editing happens elsewhere, so this is not a
//! convenience — it is the only way the reader ever sees a change.
//!
//! Design, stated so it is not rediscovered:
//!
//! - **Events are coalesced, not forwarded.** Editors write a file several
//!   times per save (temp file, rename, chmod), and a `git checkout` or a
//!   Syncthing sync lands hundreds of files inside a second. The watcher
//!   collects paths for a quiet window and hands over one batch.
//! - **The watcher does not rescan or reindex.** It reports which notes
//!   changed; the caller decides whether that is a handful of `update_note`
//!   calls or a full rescan. A vault scan is 0.8–1.6 s on ~4000 notes, and only
//!   the caller knows whether it is worth paying.
//! - **Notes and directory topology matter.** Service directories and non-note
//!   file writes are excluded; ordinary dot/underscore folders remain searchable.
//!   Directory events report scope hints: a populated tree can arrive before
//!   recursive watches observe its individual files. Only lost events force rescan. The legacy in-vault
//!   index directory is excluded to avoid reacting to its own writes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// One coalesced batch of changes, as vault-relative note paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// Notes that were written or created and still exist on disk.
    pub changed: BTreeSet<String>,
    /// Notes that no longer exist on disk.
    pub removed: BTreeSet<String>,
    /// Directory hints: inspect their children; discover new/removed subtrees.
    pub directories: BTreeSet<String>,
    /// Dropped events require a complete metadata scan.
    pub rescan: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.rescan
            && self.changed.is_empty()
            && self.removed.is_empty()
            && self.directories.is_empty()
    }

    /// A batch this large is a bulk operation (checkout, sync, restore), and
    /// a full rescan is cheaper than that many single updates.
    pub fn is_bulk(&self) -> bool {
        self.rescan || self.changed.len() + self.removed.len() >= BULK_THRESHOLD
    }
}

/// Above this many notes in one batch, tell the caller to rescan instead.
pub const BULK_THRESHOLD: usize = 50;

/// How long the vault must be quiet before a batch is released.
pub const QUIET_WINDOW: Duration = Duration::from_millis(300);

/// Native local watchers cannot promise visibility of another network client's
/// writes. Probe on the preparation worker, never on the UI thread.
pub fn is_network_root(root: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        match root.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) => true,
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                    let name = [u16::from(drive), b':' as u16, b'\\' as u16, 0];
                    unsafe { GetDriveTypeW(name.as_ptr()) == 4 } // DRIVE_REMOTE
                }
                _ => false,
            },
            _ => false,
        }
    }
    #[cfg(target_os = "linux")]
    {
        rustix::fs::statfs(root)
            .is_ok_and(|stat| matches!(i128::from(stat.f_type), 0x6969 | 0xff53_4d42 | 0xfe53_4d42))
        // NFS, CIFS, SMB2
    }
    #[cfg(target_os = "macos")]
    {
        rustix::fs::statfs(root).is_ok_and(|stat| {
            let name: Vec<u8> = stat
                .f_fstypename
                .iter()
                .take_while(|c| **c != 0)
                .map(|c| *c as u8)
                .collect();
            matches!(name.as_slice(), b"smbfs" | b"nfs" | b"afpfs" | b"webdav")
        })
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = root;
        false
    }
}

pub struct VaultWatcher {
    root: PathBuf,
    rx: mpsc::Receiver<notify::Result<Event>>,
    // Dropping the watcher stops it; it is held only for that.
    _watcher: RecommendedWatcher,
    folders: Option<BTreeSet<String>>,
    pending: Changes,
    last_event: Option<Instant>,
}

impl VaultWatcher {
    /// Start watching `root` recursively. Returns immediately; poll with
    /// [`poll`](Self::poll).
    pub fn new(root: &Path) -> notify::Result<VaultWatcher> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(VaultWatcher {
            root: root.to_path_buf(),
            rx,
            _watcher: watcher,
            folders: None,
            pending: Changes::default(),
            last_event: None,
        })
    }

    /// Watch only explicitly browsed folders, including attachment changes.
    /// Uses the same debounce and Changes delivery as a full vault, without
    /// enrolling unexpanded subtrees or reading source bodies.
    pub fn for_folders(root: &Path) -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })?;
        Ok(Self {
            root: root.to_path_buf(),
            rx,
            _watcher: watcher,
            folders: Some(BTreeSet::new()),
            pending: Changes::default(),
            last_event: None,
        })
    }

    /// Register before enumeration; a new subscription also refreshes that
    /// folder to cover changes between its previous listing and registration.
    pub fn watch_folders(&mut self, folders: &BTreeSet<String>) -> notify::Result<()> {
        let Some(watched) = self.folders.as_mut() else {
            return Ok(());
        };
        // Release obsolete subscriptions even if another folder cannot be added.
        for folder in watched.difference(folders).cloned().collect::<Vec<_>>() {
            let _ = self._watcher.unwatch(&self.root.join(&folder));
            watched.remove(&folder);
        }
        let mut first_error = None;
        for folder in folders.difference(watched).cloned().collect::<Vec<_>>() {
            let path = self.root.join(&folder);
            let registration = (|| {
                let resolved = path.canonicalize().map_err(notify::Error::io)?;
                if resolved != path || !resolved.starts_with(&self.root) {
                    return Err(notify::Error::generic(
                        "Folder watch escaped the viewer root",
                    ));
                }
                self._watcher.watch(&resolved, RecursiveMode::NonRecursive)
            })();
            match registration {
                Ok(()) => {
                    watched.insert(folder.clone());
                    self.pending.directories.insert(folder);
                    self.last_event.get_or_insert_with(Instant::now);
                }
                Err(error) => {
                    // Disappearance between enumeration and subscription is an
                    // inventory change. Refresh its parent without resetting
                    // the quiet window on every retry or starving other folders.
                    let missing = match &error.kind {
                        notify::ErrorKind::Io(io) => io.kind() == std::io::ErrorKind::NotFound,
                        notify::ErrorKind::PathNotFound => true,
                        _ => false,
                    };
                    if missing && !folder.is_empty() {
                        if let Some(parent) = Path::new(&folder).parent() {
                            self.pending
                                .directories
                                .insert(crate::vault::note_path(parent));
                            self.last_event.get_or_insert_with(Instant::now);
                        }
                    } else if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// A path the vault cares about, made vault-relative. `None` for anything
    /// the reader must never react to.
    fn relevant(&self, p: &Path) -> Option<String> {
        if p.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref()
            != Some("md")
        {
            return None;
        }
        let rel = p.strip_prefix(&self.root).ok()?;
        if crate::vault::service_path(rel) {
            return None;
        }
        Some(crate::vault::note_path(rel))
    }

    /// Drain whatever the OS has delivered, then release a batch if the vault
    /// has been quiet for [`QUIET_WINDOW`]. Non-blocking; call it from a timer.
    pub fn poll(&mut self) -> Option<Changes> {
        while let Ok(ev) = self.rx.try_recv() {
            let ev = match ev {
                Ok(ev) => ev,
                Err(_) => {
                    self.pending.rescan = true;
                    self.last_event = Some(Instant::now());
                    continue;
                }
            };
            if ev.need_rescan() {
                self.pending.rescan = true;
                self.last_event = Some(Instant::now());
            }
            // Only writes count. notify's inotify backend also subscribes to
            // OPEN and CLOSE_NOWRITE, so a plain read of a note arrives here as
            // an Access event. Treating those as changes made the reader chase
            // its own tail: opening a note reads every note for backlinks,
            // that read is a "bulk change", the index rebuild reads every note
            // again, and the vault rebuilt itself forever at 300% CPU.
            if matches!(ev.kind, EventKind::Access(_)) {
                continue;
            }
            let removing = matches!(ev.kind, EventKind::Remove(_));
            for p in &ev.paths {
                let Some(relative) = p.strip_prefix(&self.root).ok() else {
                    continue;
                };
                if crate::vault::service_path(relative)
                    || p.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(".okilum-save-"))
                {
                    continue;
                }
                if let Some(folders) = &self.folders {
                    let parent = relative.parent().map(crate::vault::note_path);
                    if let Some(parent) = parent.filter(|parent| folders.contains(parent)) {
                        self.pending.directories.insert(parent);
                    }
                    let path = crate::vault::note_path(relative);
                    if folders.contains(&path) {
                        self.pending.directories.insert(path);
                    }
                    continue;
                }
                // Newly created/moved trees may arrive as a directory event only,
                // before recursive watches can observe their individual files.
                // A rename source may already be gone, so its file type cannot
                // be recovered with stat. Known Markdown endpoints use a source
                // batch; moved trees report both directory scopes.
                if p.is_dir()
                    // Windows removes entries without their old type. Once
                    // gone, stat and the extension cannot identify a folder.
                    || matches!(
                        ev.kind,
                        EventKind::Remove(notify::event::RemoveKind::Any)
                    )
                    || matches!(
                        ev.kind,
                        EventKind::Create(notify::event::CreateKind::Folder)
                            | EventKind::Remove(notify::event::RemoveKind::Folder)
                    )
                    || (matches!(
                        ev.kind,
                        EventKind::Modify(notify::event::ModifyKind::Name(_))
                    ) && p
                        .extension()
                        .is_none_or(|ext| !ext.eq_ignore_ascii_case("md"))
                        && !ev.paths.iter().any(|endpoint| {
                            endpoint.is_file()
                                && endpoint
                                    .extension()
                                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                        }))
                {
                    self.pending
                        .directories
                        .insert(crate::vault::note_path(relative));
                }
                let Some(rel) = self.relevant(p) else {
                    continue;
                };
                // Existence on disk is the truth, not the event kind: a
                // rename delivers Remove for the old name and Create for the
                // new, and an editor's atomic save can look like either.
                if removing || !p.is_file() {
                    self.pending.changed.remove(&rel);
                    self.pending.removed.insert(rel);
                } else {
                    self.pending.removed.remove(&rel);
                    self.pending.changed.insert(rel);
                }
            }
            self.last_event = Some(Instant::now());
        }
        let quiet = self.last_event.is_some_and(|t| t.elapsed() >= QUIET_WINDOW);
        if quiet && !self.pending.is_empty() {
            self.last_event = None;
            return Some(std::mem::take(&mut self.pending));
        }
        None
    }

    /// Drain scan-time events without waiting for the UI debounce window.
    /// A background preparation owner reconciles this batch before publication.
    pub fn drain_preparation_changes(&mut self) -> Option<Changes> {
        if let Some(changes) = self.poll() {
            return Some(changes);
        }
        if self.pending.is_empty() {
            return None;
        }
        self.last_event = None;
        Some(std::mem::take(&mut self.pending))
    }

    /// Block until a batch is ready or `timeout` passes. For tests and CLIs;
    /// a UI polls instead.
    pub fn wait(&mut self, timeout: Duration) -> Option<Changes> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(c) = self.poll() {
                return Some(c);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod topology_tests {
    use super::*;
    #[test]
    fn browsed_folders_report_attachments_without_enrolling_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("child")).unwrap();
        let mut watcher = VaultWatcher::for_folders(&root).unwrap();
        watcher
            .watch_folders(&BTreeSet::from([String::new()]))
            .unwrap();
        watcher.pending = Changes::default();
        let (tx, rx) = mpsc::channel();
        watcher.rx = rx;
        for path in ["image.png", "child/hidden.md"] {
            tx.send(Ok(Event::new(EventKind::Create(
                notify::event::CreateKind::File,
            ))
            .add_path(root.join(path))))
                .unwrap();
        }
        watcher.poll();
        watcher.last_event = Some(Instant::now() - QUIET_WINDOW);
        assert_eq!(
            watcher.poll().unwrap().directories,
            BTreeSet::from([String::new()])
        );
        watcher
            .watch_folders(&BTreeSet::from([String::new(), "child".into()]))
            .unwrap();
        assert!(
            watcher.pending.directories.contains("child"),
            "new registration closes listing race"
        );
        watcher.pending = Changes::default();
        tx.send(Ok(Event::new(EventKind::Remove(
            notify::event::RemoveKind::File,
        ))
        .add_path(root.join("child/removed.txt"))))
            .unwrap();
        watcher.poll();
        watcher.last_event = Some(Instant::now() - QUIET_WINDOW);
        assert_eq!(
            watcher.poll().unwrap().directories,
            BTreeSet::from(["child".into()])
        );
    }

    #[test]
    fn missing_browsed_folder_does_not_starve_other_subscriptions_or_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["old", "z-valid"] {
            std::fs::create_dir(root.join(folder)).unwrap();
        }
        let mut watcher = VaultWatcher::for_folders(&root).unwrap();
        watcher
            .watch_folders(&BTreeSet::from([String::new(), "old".into()]))
            .unwrap();
        std::fs::remove_dir(root.join("old")).unwrap();
        watcher.pending = Changes::default();
        let requested = BTreeSet::from([String::new(), "a-missing".into(), "z-valid".into()]);
        watcher.watch_folders(&requested).unwrap();
        assert_eq!(
            watcher.folders.as_ref().unwrap(),
            &BTreeSet::from([String::new(), "z-valid".into()])
        );
        assert!(
            watcher.pending.directories.contains(""),
            "missing folder refreshes its parent"
        );
        assert!(
            watcher.pending.directories.contains("z-valid"),
            "later folder still registered"
        );
        let deadline = watcher.last_event;
        watcher.watch_folders(&requested).unwrap();
        assert_eq!(
            watcher.last_event, deadline,
            "retries must not postpone the quiet window"
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_folder_reports_error_after_other_subscriptions_and_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        for folder in ["old", "z-valid"] {
            std::fs::create_dir(root.join(folder)).unwrap();
        }
        std::os::unix::fs::symlink(temp.path(), root.join("a-outside")).unwrap();
        let mut watcher = VaultWatcher::for_folders(&root).unwrap();
        watcher
            .watch_folders(&BTreeSet::from([String::new(), "old".into()]))
            .unwrap();
        assert!(watcher
            .watch_folders(&BTreeSet::from([
                String::new(),
                "a-outside".into(),
                "z-valid".into()
            ]))
            .is_err());
        assert_eq!(
            watcher.folders.as_ref().unwrap(),
            &BTreeSet::from([String::new(), "z-valid".into()])
        );
    }

    #[test]
    fn deleted_folder_untyped_event_keeps_topology_hint() {
        let temp = tempfile::tempdir().unwrap();
        let mut watcher = VaultWatcher::new(temp.path()).unwrap();
        let (tx, rx) = mpsc::channel();
        watcher.rx = rx;
        // Windows reports FILE_ACTION_REMOVED without the old entry's type.
        // Include a .md directory: an extension is not proof it was a file.
        for name in ["mytpl", "folder.md"] {
            let folder = temp.path().join(name);
            std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join("Note.md"), "# Note").unwrap();
            std::fs::remove_dir_all(&folder).unwrap();
            tx.send(Ok(Event::new(EventKind::Remove(
                notify::event::RemoveKind::Any,
            ))
            .add_path(folder)))
                .unwrap();
        }
        // A regular note removal remains visible too (positive control).
        tx.send(Ok(Event::new(EventKind::Remove(
            notify::event::RemoveKind::Any,
        ))
        .add_path(temp.path().join("Ordinary.md"))))
            .unwrap();
        watcher.poll();
        watcher.last_event = Some(Instant::now() - QUIET_WINDOW);
        let changes = watcher.poll().expect("removed note positive control");
        assert!(changes.removed.contains("Ordinary.md"));
        assert!(changes.directories.contains("mytpl"));
        assert!(changes.directories.contains("folder.md"));
        assert!(!changes.rescan);
    }

    #[test]
    fn parent_metadata_is_a_hint_but_dropped_events_require_rescan() {
        let temp = tempfile::tempdir().unwrap();
        let mut watcher = VaultWatcher::new(temp.path()).unwrap();
        let (tx, rx) = mpsc::channel();
        watcher.rx = rx;
        tx.send(Ok(Event::new(EventKind::Modify(
            notify::event::ModifyKind::Metadata(notify::event::MetadataKind::Any),
        ))
        .add_path(temp.path().to_owned())))
            .unwrap();
        watcher.poll();
        watcher.last_event = Some(Instant::now() - QUIET_WINDOW);
        let hint = watcher.poll().unwrap();
        assert_eq!(hint.directories, BTreeSet::from([String::new()]));
        assert!(!hint.rescan);
        tx.send(Err(notify::Error::generic("lost events positive control")))
            .unwrap();
        watcher.poll();
        watcher.last_event = Some(Instant::now() - QUIET_WINDOW);
        assert!(watcher.poll().unwrap().rescan);
    }

    #[test]
    fn cloud_materialization_metadata_is_forwarded_as_a_source_change() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Pending.md");
        std::fs::write(&path, [0xff]).unwrap();
        let mut watcher = VaultWatcher::new(temp.path()).unwrap();
        let (tx, rx) = mpsc::channel();
        watcher.rx = rx;
        // Metadata routing uses only the path/file metadata, not source bytes
        // (these bytes deliberately are not a valid Markdown UTF-8 source).
        tx.send(Ok(Event::new(EventKind::Modify(
            notify::event::ModifyKind::Metadata(notify::event::MetadataKind::Any),
        ))
        .add_path(path)))
            .unwrap();
        watcher.poll();
        watcher.last_event = Some(Instant::now() - QUIET_WINDOW);
        let changes = watcher
            .poll()
            .expect("metadata notification must reach reconciliation");
        assert_eq!(changes.changed, BTreeSet::from(["Pending.md".into()]));
        assert!(!changes.rescan);
    }
}
