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
//!   Directory events also require reconciliation: a populated tree can arrive
//!   before recursive watches observe its individual files. The legacy in-vault
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
    /// Directory topology or dropped events require a complete metadata scan.
    pub rescan: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.rescan && self.changed.is_empty() && self.removed.is_empty()
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
            pending: Changes::default(),
            last_event: None,
        })
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
                if crate::vault::service_path(relative) {
                    continue;
                }
                // Newly created/moved trees may arrive as a directory event only,
                // before recursive watches can observe their individual files.
                // A rename source may already be gone, so its file type cannot
                // be recovered with stat. Conservatively reconcile renames too.
                if p.is_dir()
                    || matches!(
                        ev.kind,
                        EventKind::Create(notify::event::CreateKind::Folder)
                            | EventKind::Remove(notify::event::RemoveKind::Folder)
                    )
                    || matches!(
                        ev.kind,
                        EventKind::Modify(notify::event::ModifyKind::Name(_))
                    )
                {
                    self.pending.rescan = true;
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
