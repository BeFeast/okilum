//! Explicit polling fallback for network vaults (#516).
//!
//! inotify, FSEvents and ReadDirectoryChangesW report this computer's writes
//! to an SMB/NFS share but not those of another client. For such roots the
//! watcher walks the vault's metadata on a background thread and turns the
//! difference into the same synthetic `notify` events a native backend
//! delivers, so coalescing, relevance and service-path rules stay in one
//! place ([`super::VaultWatcher::poll`]).
//!
//! Network semantics the walk must survive, each covered by a test:
//!
//! - **No events at all.** Nothing here depends on native notifications.
//! - **Coarse mtimes.** Servers round modification times (FAT/SMB to 2 s,
//!   some NFS exports to 1 s), so a same-size rewrite inside one tick keeps
//!   an identical stamp. As in Git's "racy" index, an entry stamped within
//!   [`Config::racy`] of the walk that recorded it is reported once more on
//!   the next walk; the caller rereads its bytes and keeps unchanged content.
//! - **Rename quirks.** A rename, including a case-only rename, can surface
//!   as removal plus creation, and a walk can observe it half done. Existence
//!   in the next walk is the truth; directory stamp changes add a folder hint.
//! - **Unavailable share.** A root that cannot be listed skips the round and
//!   keeps the previous baseline: a disconnect is never reported as every
//!   note being removed. A subfolder that cannot be listed keeps its previous
//!   entries. After reconnection the next walk reports what really changed.
//!
//! The walk never reads note bodies and never writes to the vault. Its
//! interval adapts to the drive: at least [`Config::min`], and at least
//! [`Config::load`] times the previous walk, so a slow share spends at most a
//! small fraction of the time being listed; never longer than [`Config::max`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use notify::event::{CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind};

/// Shortest pause between two walks of a network vault.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Longest pause, reached only on a very slow share.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(300);

/// Directories never walked. They are service or tool trees, often large, and
/// never note input. `.okilum` stays walked: project/board metadata lives there.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".obsidian",
    ".trash",
    ".stfolder",
    ".stversions",
    ".okilum-index",
    "node_modules",
];

/// Private publication/recovery names of our own saves and macOS AppleDouble
/// files. Their churn must not cost a network round trip.
const SKIP_PREFIXES: &[&str] = &[
    ".okilum-save-",
    ".okilum-create-",
    ".service-relocate-",
    "._",
];

#[derive(Clone)]
pub(crate) struct Config {
    pub min: Duration,
    pub max: Duration,
    /// The pause is at least this many times the previous walk.
    pub load: u32,
    /// Stamps this close to the walk (or later) are not trusted as revisions.
    pub racy: Duration,
    /// The first walk only records the baseline; it waits this long so it
    /// does not compete with the reader's own startup scan.
    pub first: Duration,
    pub list: Lister,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min: MIN_POLL_INTERVAL,
            max: MAX_POLL_INTERVAL,
            load: 4,
            racy: Duration::from_secs(3),
            first: Duration::from_secs(2),
            list: Arc::new(list_dir),
        }
    }
}

/// One listed directory entry, without following symlinked directories.
#[derive(Debug, Clone)]
pub(crate) struct Listed {
    pub name: std::ffi::OsString,
    pub dir: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

pub(crate) type Lister = Arc<dyn Fn(&Path) -> std::io::Result<Vec<Listed>> + Send + Sync>;

fn list_dir(dir: &Path) -> std::io::Result<Vec<Listed>> {
    let mut listed = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // A symlinked note is followed like the inventory does; a symlinked
        // folder is not descended, so a link cycle cannot loop the walk.
        let metadata = if kind.is_symlink() {
            match std::fs::metadata(entry.path()) {
                Ok(metadata) if metadata.is_file() => metadata,
                _ => continue,
            }
        } else {
            match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            }
        };
        listed.push(Listed {
            name: entry.file_name(),
            dir: metadata.is_dir(),
            len: if metadata.is_dir() { 0 } else { metadata.len() },
            modified: metadata.modified().ok(),
        });
    }
    Ok(listed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dir: bool,
    len: u64,
    modified: Option<SystemTime>,
    /// Recorded within the racy window: the stamp cannot prove "unchanged".
    racy: bool,
}

type Manifest = BTreeMap<PathBuf, Stamp>;

/// What the UI and diagnostics may show about the fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollStatus {
    /// Current pause between walks.
    pub interval: Duration,
    /// Duration of the last complete walk; `None` before the first one.
    pub last_walk: Option<Duration>,
    /// The last walk could not list the vault folder (share unavailable).
    pub unavailable: bool,
}

pub(crate) struct Poller {
    stop: Arc<AtomicBool>,
    // Dropping the sender wakes the sleeping thread at once.
    _wake: mpsc::Sender<()>,
    status: Arc<Mutex<PollStatus>>,
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Poller {
    pub fn start(
        root: &Path,
        tx: mpsc::Sender<notify::Result<Event>>,
        config: Config,
    ) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (wake, sleep) = mpsc::channel::<()>();
        let status = Arc::new(Mutex::new(PollStatus {
            interval: config.min,
            last_walk: None,
            unavailable: false,
        }));
        let root = root.to_path_buf();
        let thread_stop = stop.clone();
        let thread_status = status.clone();
        std::thread::Builder::new()
            .name("okilum-vault-poll".into())
            .spawn(move || run(&root, &tx, &config, &thread_stop, &sleep, &thread_status))?;
        Ok(Self {
            stop,
            _wake: wake,
            status,
        })
    }

    pub fn status(&self) -> PollStatus {
        *self.status.lock().unwrap()
    }
}

fn run(
    root: &Path,
    tx: &mpsc::Sender<notify::Result<Event>>,
    config: &Config,
    stop: &AtomicBool,
    sleep: &mpsc::Receiver<()>,
    status: &Mutex<PollStatus>,
) {
    let registered = SystemTime::now();
    let mut baseline: Option<Manifest> = None;
    let mut pause = config.first;
    loop {
        match sleep.recv_timeout(pause) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Woken or dropped: the watcher is gone.
            _ => return,
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let started = Instant::now();
        // The baseline also distrusts stamps written since registration: the
        // reader's inventory may have read those files before they changed.
        let since = match &baseline {
            None => registered.min(SystemTime::now()),
            Some(_) => SystemTime::now(),
        };
        let walked = walk(root, baseline.as_ref(), config, since, stop);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let elapsed = started.elapsed();
        let mut current = status.lock().unwrap();
        match walked {
            Some(next) => {
                if let Some(previous) = &baseline {
                    for event in diff(root, previous, &next) {
                        if tx.send(Ok(event)).is_err() {
                            return;
                        }
                    }
                }
                baseline = Some(next);
                current.interval = next_interval(elapsed, config);
                current.last_walk = Some(elapsed);
                current.unavailable = false;
            }
            None => current.unavailable = true,
        }
        pause = current.interval;
    }
}

/// Pause after a walk that took `walk`.
pub(crate) fn next_interval(walk: Duration, config: &Config) -> Duration {
    walk.saturating_mul(config.load)
        .clamp(config.min, config.max.max(config.min))
}

fn skipped(name: &std::ffi::OsStr, dir: bool) -> bool {
    let bytes = name.as_encoded_bytes();
    SKIP_PREFIXES
        .iter()
        .any(|prefix| bytes.starts_with(prefix.as_bytes()))
        || (dir && name.to_str().is_some_and(|name| SKIP_DIRS.contains(&name)))
}

/// One metadata walk. `None` when the root itself cannot be listed or the
/// watcher stopped; a failed subfolder keeps its entries from `previous`.
fn walk(
    root: &Path,
    previous: Option<&Manifest>,
    config: &Config,
    since: SystemTime,
    stop: &AtomicBool,
) -> Option<Manifest> {
    let threshold = since
        .checked_sub(config.racy)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut manifest = Manifest::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let listed = match (config.list)(&root.join(&relative)) {
            Ok(listed) => listed,
            Err(_) if relative.as_os_str().is_empty() => return None,
            Err(_) => {
                // Unknown is not removed: keep what this folder held.
                if let Some(previous) = previous {
                    manifest.extend(
                        previous
                            .range(relative.clone()..)
                            .take_while(|(path, _)| path.starts_with(&relative))
                            .filter(|(path, _)| **path != relative)
                            .map(|(path, stamp)| (path.clone(), *stamp)),
                    );
                }
                continue;
            }
        };
        for entry in listed {
            if skipped(&entry.name, entry.dir) {
                continue;
            }
            let path = relative.join(&entry.name);
            let racy = entry.modified.is_none_or(|modified| modified >= threshold);
            manifest.insert(
                path.clone(),
                Stamp {
                    dir: entry.dir,
                    len: entry.len,
                    modified: entry.modified,
                    racy,
                },
            );
            if entry.dir {
                pending.push(path);
            }
        }
    }
    Some(manifest)
}

/// Synthetic events for everything that differs, one event per kind.
fn diff(root: &Path, previous: &Manifest, next: &Manifest) -> Vec<Event> {
    let mut created_files = Vec::new();
    let mut created_dirs = Vec::new();
    let mut written = Vec::new();
    let mut touched_dirs = Vec::new();
    let mut removed_files = Vec::new();
    let mut removed_dirs = Vec::new();
    for (path, stamp) in next {
        match previous.get(path) {
            None if stamp.dir => created_dirs.push(root.join(path)),
            None => created_files.push(root.join(path)),
            // A file replaced by a folder of the same name, or the reverse.
            Some(old) if old.dir != stamp.dir => {
                if old.dir {
                    removed_dirs.push(root.join(path));
                    created_files.push(root.join(path));
                } else {
                    removed_files.push(root.join(path));
                    created_dirs.push(root.join(path));
                }
            }
            Some(old) => {
                let moved = old.len != stamp.len || old.modified != stamp.modified;
                if moved || old.racy {
                    if stamp.dir {
                        touched_dirs.push(root.join(path));
                    } else {
                        written.push(root.join(path));
                    }
                }
            }
        }
    }
    for (path, stamp) in previous {
        if !next.contains_key(path) {
            if stamp.dir {
                removed_dirs.push(root.join(path));
            } else {
                removed_files.push(root.join(path));
            }
        }
    }
    [
        (EventKind::Remove(RemoveKind::File), removed_files),
        (EventKind::Remove(RemoveKind::Folder), removed_dirs),
        (EventKind::Create(CreateKind::Folder), created_dirs),
        (EventKind::Create(CreateKind::File), created_files),
        (
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            written,
        ),
        (
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)),
            touched_dirs,
        ),
    ]
    .into_iter()
    .filter(|(_, paths)| !paths.is_empty())
    .map(|(kind, paths)| Event {
        kind,
        paths,
        attrs: Default::default(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::VaultWatcher;
    use std::sync::atomic::AtomicUsize;

    /// A share that never delivers native events, rounds stamps down to
    /// 2 s like FAT/SMB, and can be "disconnected" or have one folder fail.
    #[derive(Default)]
    struct Share {
        offline: AtomicBool,
        failing: Mutex<Option<PathBuf>>,
        walks: AtomicUsize,
    }

    fn coarse(time: SystemTime) -> SystemTime {
        let since = time.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        SystemTime::UNIX_EPOCH + Duration::from_secs(since.as_secs() / 2 * 2)
    }

    fn config(share: &Arc<Share>, root: &Path) -> Config {
        let share = share.clone();
        let root = root.to_path_buf();
        Config {
            min: Duration::from_millis(40),
            max: Duration::from_millis(200),
            load: 4,
            racy: Duration::from_secs(3),
            first: Duration::from_millis(10),
            list: Arc::new(move |dir: &Path| {
                if dir == root {
                    share.walks.fetch_add(1, Ordering::SeqCst);
                }
                if share.offline.load(Ordering::SeqCst) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "share is not responding",
                    ));
                }
                if share.failing.lock().unwrap().as_deref() == Some(dir) {
                    return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
                }
                Ok(list_dir(dir)?
                    .into_iter()
                    .map(|mut entry| {
                        entry.modified = entry.modified.map(coarse);
                        entry
                    })
                    .collect())
            }),
        }
    }

    fn watcher(root: &Path, share: &Arc<Share>) -> VaultWatcher {
        VaultWatcher::polling_with(root, config(share, root)).unwrap()
    }

    /// Wait until `n` more complete walks of the root have started and ended.
    fn walks(share: &Share, n: usize) {
        let target = share.walks.load(Ordering::SeqCst) + n + 1;
        let deadline = Instant::now() + Duration::from_secs(10);
        while share.walks.load(Ordering::SeqCst) < target {
            assert!(Instant::now() < deadline, "poller stopped walking");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn old(path: &Path) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
    }

    fn vault() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Notes")).unwrap();
        for note in ["a.md", "Notes/b.md", "Notes/c.md"] {
            std::fs::write(root.join(note), "# note\n").unwrap();
            old(&root.join(note));
        }
        (temp, root.canonicalize().unwrap())
    }

    /// Drain every batch until the share stays quiet. Recent stamps (fresh
    /// folders, just-written notes) are re-reported until they leave the
    /// racy window, so this also proves that re-reporting ends.
    fn settle(watcher: &mut VaultWatcher, share: &Share) -> Vec<crate::Changes> {
        walks(share, 2);
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut batches = Vec::new();
        while let Some(changes) = watcher.wait(QUIET) {
            assert!(Instant::now() < deadline, "racy entries never settled");
            batches.push(changes);
        }
        batches
    }

    const QUIET: Duration = Duration::from_millis(600);

    fn next(watcher: &mut VaultWatcher, share: &Share) -> crate::Changes {
        walks(share, 1);
        watcher
            .wait(Duration::from_secs(5))
            .expect("polling must report the change")
    }

    #[test]
    fn other_clients_creates_writes_and_removals_arrive_without_native_events() {
        let (_temp, root) = vault();
        let share = Arc::new(Share::default());
        let mut watcher = watcher(&root, &share);
        settle(&mut watcher, &share);
        // Negative control: an unchanged vault produces no batch.
        walks(&share, 2);
        assert!(
            watcher.wait(QUIET).is_none(),
            "unchanged share must stay quiet"
        );

        std::fs::write(root.join("Notes/new.md"), "# new\n").unwrap();
        std::fs::write(root.join("a.md"), "# a, edited elsewhere\n").unwrap();
        std::fs::remove_file(root.join("Notes/c.md")).unwrap();
        let changes = next(&mut watcher, &share);
        assert!(changes.changed.contains("Notes/new.md"), "{changes:?}");
        assert!(changes.changed.contains("a.md"), "{changes:?}");
        assert!(changes.removed.contains("Notes/c.md"), "{changes:?}");
        assert!(!changes.rescan);
        settle(&mut watcher, &share);

        // Attachments change the listed inventory; service trees are skipped.
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join(".git/objects/x"), "blob").unwrap();
        std::fs::write(root.join("Notes/image.png"), "png").unwrap();
        let changes = next(&mut watcher, &share);
        assert!(changes.directories.contains("Notes"), "{changes:?}");
        assert!(changes
            .directories
            .iter()
            .all(|dir| !dir.starts_with(".git")));
        assert!(changes.changed.is_empty() && changes.removed.is_empty());
    }

    #[test]
    fn same_size_rewrite_inside_a_coarse_mtime_tick_is_still_reported() {
        let (_temp, root) = vault();
        let fresh = root.join("Notes/fresh.md");
        std::fs::write(&fresh, "version 1\n").unwrap();
        let stamp = std::fs::metadata(&fresh).unwrap().modified().unwrap();
        let share = Arc::new(Share::default());
        let mut watcher = watcher(&root, &share);
        walks(&share, 1);
        // Another client rewrites within the same server tick: same size and,
        // after the server's rounding, the same stamp.
        std::fs::write(&fresh, "version 2\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&fresh)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        let changes = next(&mut watcher, &share);
        assert!(changes.changed.contains("Notes/fresh.md"), "{changes:?}");
        // Control: an old stamp is trusted, so the same trick on a settled
        // note is invisible to metadata polling. This is why only recent
        // stamps are reported again, and why Rescan rereads bytes.
        settle(&mut watcher, &share);
        let settled_note = root.join("Notes/b.md");
        let stamp = std::fs::metadata(&settled_note)
            .unwrap()
            .modified()
            .unwrap();
        std::fs::write(&settled_note, "# NOTE\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&settled_note)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        walks(&share, 2);
        let quiet = watcher.wait(QUIET);
        assert!(
            quiet
                .as_ref()
                .is_none_or(|c| !c.changed.contains("Notes/b.md")),
            "{quiet:?}"
        );
    }

    #[test]
    fn renames_are_reported_as_removal_and_creation_including_case_only() {
        let (_temp, root) = vault();
        let share = Arc::new(Share::default());
        let mut watcher = watcher(&root, &share);
        settle(&mut watcher, &share);
        std::fs::rename(root.join("Notes/b.md"), root.join("Notes/B.md")).unwrap();
        std::fs::rename(root.join("a.md"), root.join("Notes/moved.md")).unwrap();
        let changes = next(&mut watcher, &share);
        assert!(changes.removed.contains("Notes/b.md"), "{changes:?}");
        assert!(changes.removed.contains("a.md"), "{changes:?}");
        assert!(changes.changed.contains("Notes/moved.md"), "{changes:?}");
        // On a case-insensitive share both spellings name one file; the
        // caller's existence check then keeps it. Here the FS is sensitive.
        if !root.join("Notes/b.md").exists() {
            assert!(changes.changed.contains("Notes/B.md"), "{changes:?}");
        }
        settle(&mut watcher, &share);

        // A whole folder renamed away: its notes are removed, the folders hinted.
        std::fs::rename(root.join("Notes"), root.join("Archive")).unwrap();
        let changes = next(&mut watcher, &share);
        assert!(changes.removed.contains("Notes/c.md"), "{changes:?}");
        assert!(changes.changed.contains("Archive/c.md"), "{changes:?}");
        assert!(changes.directories.contains("Notes"), "{changes:?}");
        assert!(changes.directories.contains("Archive"), "{changes:?}");
    }

    #[test]
    fn disconnected_share_never_reports_removals_and_catches_up_after_reconnect() {
        let (_temp, root) = vault();
        let share = Arc::new(Share::default());
        let mut watcher = watcher(&root, &share);
        settle(&mut watcher, &share);

        share.offline.store(true, Ordering::SeqCst);
        walks(&share, 3);
        assert!(
            watcher.wait(QUIET).is_none(),
            "offline must not empty the vault"
        );
        assert!(watcher.polling_status().unwrap().unavailable);
        // Another client edits while this computer cannot reach the server.
        std::fs::write(root.join("Notes/b.md"), "# b, edited while offline\n").unwrap();
        std::fs::remove_file(root.join("Notes/c.md")).unwrap();

        share.offline.store(false, Ordering::SeqCst);
        let changes = next(&mut watcher, &share);
        assert!(changes.changed.contains("Notes/b.md"), "{changes:?}");
        assert!(changes.removed.contains("Notes/c.md"), "{changes:?}");
        assert!(!changes.removed.contains("a.md"), "{changes:?}");
        assert!(!watcher.polling_status().unwrap().unavailable);
    }

    #[test]
    fn unreadable_subfolder_keeps_its_notes() {
        let (_temp, root) = vault();
        let share = Arc::new(Share::default());
        let mut watcher = watcher(&root, &share);
        settle(&mut watcher, &share);
        *share.failing.lock().unwrap() = Some(root.join("Notes"));
        std::fs::write(root.join("a.md"), "# positive control, longer\n").unwrap();
        let changes = next(&mut watcher, &share);
        assert!(changes.changed.contains("a.md"), "{changes:?}");
        for changes in std::iter::once(changes).chain(settle(&mut watcher, &share)) {
            assert!(changes.removed.is_empty(), "{changes:?}");
        }
    }

    #[test]
    fn interval_adapts_to_walk_time_within_bounds() {
        // The reader's header tooltip names these bounds in plain words.
        assert_eq!(MIN_POLL_INTERVAL, Duration::from_secs(10));
        assert_eq!(MAX_POLL_INTERVAL, Duration::from_secs(5 * 60));
        let config = Config::default();
        assert_eq!(next_interval(Duration::ZERO, &config), MIN_POLL_INTERVAL);
        assert_eq!(
            next_interval(Duration::from_millis(12_700), &config),
            Duration::from_millis(50_800)
        );
        assert_eq!(
            next_interval(Duration::from_secs(600), &config),
            MAX_POLL_INTERVAL
        );
    }

    #[test]
    fn dropping_the_watcher_stops_the_walk_thread() {
        let (_temp, root) = vault();
        let share = Arc::new(Share::default());
        let watcher = watcher(&root, &share);
        walks(&share, 1);
        drop(watcher);
        // The lister (holding a clone of `share`) is released with the thread.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&share) > 1 {
            assert!(
                Instant::now() < deadline,
                "poll thread outlived its watcher"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
