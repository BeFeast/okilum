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
//!   note being removed. A subfolder whose listing fails, even partway,
//!   keeps its previous entries; an entry whose type or metadata cannot be
//!   read keeps its previous stamp and subtree. Only confirmed absence is a
//!   removal. After reconnection the next walk reports what really changed.
//! - **Baseline before inventory.** Registration walks the vault once on the
//!   caller's (preparation) thread, before the reader enumerates its
//!   inventory, so a note deleted after the inventory captured it is still
//!   reported. If the share cannot be listed then, the first later successful
//!   walk asks for one full reconciliation instead of adopting a baseline that
//!   could hide such a deletion.
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
    pub list: Lister,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min: MIN_POLL_INTERVAL,
            max: MAX_POLL_INTERVAL,
            load: 4,
            racy: Duration::from_secs(3),
            list: Arc::new(list_dir),
        }
    }
}

/// One listed directory entry, without following symlinked directories.
#[derive(Debug, Clone)]
pub(crate) struct Listed {
    pub name: std::ffi::OsString,
    /// `None`: the entry exists but its type or metadata could not be read.
    pub known: Option<Meta>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Meta {
    pub dir: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

pub(crate) type Lister = Arc<dyn Fn(&Path) -> std::io::Result<Vec<Listed>> + Send + Sync>;

fn list_dir(dir: &Path) -> std::io::Result<Vec<Listed>> {
    collect(std::fs::read_dir(dir)?, |entry| {
        let name = entry.file_name();
        let meta = (|| {
            let kind = entry.file_type()?;
            // A symlinked note is followed like the inventory does; a symlinked
            // folder is not descended, so a link cycle cannot loop the walk.
            let metadata = if kind.is_symlink() {
                match std::fs::metadata(entry.path()) {
                    Ok(metadata) if metadata.is_file() => metadata,
                    // Dangling or folder link: never part of the walk.
                    Ok(_) => return Ok(None),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error),
                }
            } else {
                entry.metadata()?
            };
            Ok::<_, std::io::Error>(Some(Meta {
                dir: metadata.is_dir(),
                len: if metadata.is_dir() { 0 } else { metadata.len() },
                modified: metadata.modified().ok(),
            }))
        })();
        match meta {
            Ok(Some(meta)) => Some(Listed {
                name,
                known: Some(meta),
            }),
            Ok(None) => None,
            Err(_) => Some(Listed { name, known: None }),
        }
    })
}

/// A listing is complete or an error: an iterator failure leaves unknown
/// which entries were not returned, so none of them may become a removal.
/// Per-entry failures stay entries of unknown state.
fn collect<E>(
    entries: impl IntoIterator<Item = std::io::Result<E>>,
    mut describe: impl FnMut(E) -> Option<Listed>,
) -> std::io::Result<Vec<Listed>> {
    let mut listed = Vec::new();
    for entry in entries {
        listed.extend(describe(entry?));
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
    /// Records the baseline on the calling thread before returning, so it
    /// precedes the caller's inventory enumeration, then walks periodically
    /// on its own thread.
    pub fn start(
        root: &Path,
        tx: mpsc::Sender<notify::Result<Event>>,
        config: Config,
    ) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (wake, sleep) = mpsc::channel::<()>();
        let mut walker = Walker::new(root, config.clone());
        let started = Instant::now();
        let baseline = walker.step(SystemTime::now(), &stop);
        let elapsed = started.elapsed();
        let status = Arc::new(Mutex::new(PollStatus {
            interval: next_interval(elapsed, &config),
            last_walk: baseline.is_some().then_some(elapsed),
            unavailable: baseline.is_none(),
        }));
        let thread_stop = stop.clone();
        let thread_status = status.clone();
        std::thread::Builder::new()
            .name("okilum-vault-poll".into())
            .spawn(move || run(walker, &tx, &config, &thread_stop, &sleep, &thread_status))?;
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
    mut walker: Walker,
    tx: &mpsc::Sender<notify::Result<Event>>,
    config: &Config,
    stop: &AtomicBool,
    sleep: &mpsc::Receiver<()>,
    status: &Mutex<PollStatus>,
) {
    let mut pause = status.lock().unwrap().interval;
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
        let walked = walker.step(SystemTime::now(), stop);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let elapsed = started.elapsed();
        let mut current = status.lock().unwrap();
        match walked {
            Some(events) => {
                for event in events {
                    if tx.send(Ok(event)).is_err() {
                        return;
                    }
                }
                current.interval = next_interval(elapsed, config);
                current.last_walk = Some(elapsed);
                current.unavailable = false;
            }
            None => current.unavailable = true,
        }
        pause = current.interval;
    }
}

/// The walk/diff state machine, free of threads and wall-clock reads so it
/// can be stepped deterministically.
pub(crate) struct Walker {
    root: PathBuf,
    config: Config,
    baseline: Option<Manifest>,
    /// The registration walk failed: whatever changed between the reader's
    /// inventory and a later baseline (deletions included) is unknown.
    missed_baseline: bool,
}

impl Walker {
    pub fn new(root: &Path, config: Config) -> Self {
        Self {
            root: root.to_path_buf(),
            config,
            baseline: None,
            missed_baseline: false,
        }
    }

    /// One walk at time `now`. `None` when the share is unavailable (or the
    /// watcher stopped); the baseline is then kept. The registration walk
    /// only records the baseline; a baseline recorded later asks for one full
    /// reconciliation.
    pub fn step(&mut self, now: SystemTime, stop: &AtomicBool) -> Option<Vec<Event>> {
        let Some(next) = walk(&self.root, self.baseline.as_ref(), &self.config, now, stop) else {
            self.missed_baseline |= self.baseline.is_none();
            return None;
        };
        let events = match &self.baseline {
            Some(previous) => diff(&self.root, previous, &next),
            None if self.missed_baseline => {
                vec![Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)]
            }
            None => Vec::new(),
        };
        self.baseline = Some(next);
        Some(events)
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
/// watcher stopped; a failed subfolder keeps its entries from `previous`,
/// and an entry of unknown state keeps its stamp and subtree.
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
                keep_previous(previous, &relative, false, &mut manifest);
                continue;
            }
        };
        for entry in listed {
            if !relative.as_os_str().is_empty() && entry.name == ".obsidian" {
                let path = relative.join(&entry.name);
                match entry.known {
                    Some(meta) if meta.dir => {
                        // Only its existence is meaningful. Opening or changing
                        // a service file must not invalidate the reader index.
                        manifest.insert(
                            path,
                            Stamp {
                                dir: true,
                                len: 0,
                                modified: None,
                                racy: false,
                            },
                        );
                    }
                    None => keep_previous(previous, &path, true, &mut manifest),
                    Some(_) => {}
                }
                continue;
            }
            if skipped(&entry.name, entry.known.is_none_or(|meta| meta.dir)) {
                continue;
            }
            let path = relative.join(&entry.name);
            let Some(meta) = entry.known else {
                // It exists, its state is unknown: keep its stamp and subtree.
                keep_previous(previous, &path, true, &mut manifest);
                continue;
            };
            let racy = meta.modified.is_none_or(|modified| modified >= threshold);
            manifest.insert(
                path.clone(),
                Stamp {
                    dir: meta.dir,
                    len: meta.len,
                    modified: meta.modified,
                    racy,
                },
            );
            if meta.dir {
                pending.push(path);
            }
        }
    }
    Some(manifest)
}

/// Copy `path`'s previously recorded descendants (and, with `itself`, its
/// own stamp) into the new manifest unchanged.
fn keep_previous(previous: Option<&Manifest>, path: &Path, itself: bool, manifest: &mut Manifest) {
    let Some(previous) = previous else { return };
    manifest.extend(
        previous
            .range(path.to_path_buf()..)
            .take_while(|(entry, _)| entry.starts_with(path))
            .filter(|(entry, _)| itself || entry.as_path() != path)
            .map(|(entry, stamp)| (entry.clone(), *stamp)),
    );
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
    //! Deterministic: every walk is stepped explicitly with a simulated clock
    //! and its events are drained at once; no sleeps or timing windows.
    use super::*;
    use crate::watch::VaultWatcher;
    use std::collections::BTreeSet;

    /// A share that never delivers native events, rounds stamps down to
    /// 2 s like FAT/SMB, and can be "disconnected", have one folder listing
    /// fail, or one entry whose type/metadata cannot be read.
    #[derive(Default)]
    struct Share {
        offline: AtomicBool,
        failing: Mutex<Option<PathBuf>>,
        unknown: Mutex<Option<PathBuf>>,
    }

    fn coarse(time: SystemTime) -> SystemTime {
        let since = time.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        SystemTime::UNIX_EPOCH + Duration::from_secs(since.as_secs() / 2 * 2)
    }

    fn shared_lister(share: &Arc<Share>) -> Lister {
        let share = share.clone();
        Arc::new(move |dir: &Path| {
            if share.offline.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "share is not responding",
                ));
            }
            if share.failing.lock().unwrap().as_deref() == Some(dir) {
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
            }
            let unknown = share.unknown.lock().unwrap().clone();
            Ok(list_dir(dir)?
                .into_iter()
                .map(|mut entry| {
                    if unknown.as_deref() == Some(dir.join(&entry.name).as_path()) {
                        entry.known = None;
                    }
                    if let Some(meta) = &mut entry.known {
                        meta.modified = meta.modified.map(coarse);
                    }
                    entry
                })
                .collect())
        })
    }

    /// Fixture notes carry this stamp; the simulated clock starts later.
    fn epoch() -> SystemTime {
        coarse(SystemTime::now()) - Duration::from_secs(3600)
    }

    fn stamp(path: &Path, time: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    struct Harness {
        _temp: tempfile::TempDir,
        root: PathBuf,
        share: Arc<Share>,
        walker: Walker,
        watcher: VaultWatcher,
        tx: mpsc::Sender<notify::Result<Event>>,
        /// Simulated clock, one minute per walk: far beyond the racy window
        /// unless a test stamps a file "now" on purpose.
        now: SystemTime,
        stop: AtomicBool,
    }

    impl Harness {
        fn new() -> Self {
            Self::registered(true)
        }

        /// `reachable`: whether the share answers the registration walk.
        fn registered(reachable: bool) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("vault");
            std::fs::create_dir_all(root.join("Notes")).unwrap();
            for note in ["a.md", "Notes/b.md", "Notes/c.md"] {
                std::fs::write(root.join(note), "# note\n").unwrap();
                stamp(&root.join(note), epoch());
            }
            let root = root.canonicalize().unwrap();
            let share = Arc::new(Share::default());
            let config = Config {
                list: shared_lister(&share),
                ..Config::default()
            };
            // A minute after the fixture: its folders are not racy.
            let now = SystemTime::now();
            let walker = Walker::new(&root, config);
            // Coalescing only: native events go to a detached channel.
            let mut watcher = VaultWatcher::new(&root).unwrap();
            let (tx, rx) = mpsc::channel();
            watcher.rx = rx;
            let mut harness = Self {
                _temp: temp,
                root,
                share,
                walker,
                watcher,
                tx,
                now,
                stop: AtomicBool::new(false),
            };
            harness.share.offline.store(!reachable, Ordering::SeqCst);
            let baseline = harness.walk();
            assert_eq!(baseline.is_some(), reachable, "registration walk");
            assert_eq!(baseline.unwrap_or_default(), crate::Changes::default());
            harness.share.offline.store(false, Ordering::SeqCst);
            harness
        }

        /// One walk at the next simulated minute; `None` if unavailable.
        fn walk(&mut self) -> Option<crate::Changes> {
            self.now += Duration::from_secs(60);
            let events = self.walker.step(self.now, &self.stop)?;
            for event in events {
                self.tx.send(Ok(event)).unwrap();
            }
            Some(self.watcher.drain_preparation_changes().unwrap_or_default())
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }
    }

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn vd4_review_polling_notices_nested_obsidian_marker_without_reading_contents() {
        let mut share = Harness::new();
        let marker = share.path("Notes/.obsidian");
        std::fs::create_dir(&marker).unwrap();
        // Fail any attempt to enumerate the marker, as a service directory.
        *share.share.failing.lock().unwrap() = Some(marker.clone());
        assert!(
            share.walk().unwrap().rescan,
            "new marker requires reconciliation"
        );
        std::fs::write(marker.join("plugin.md"), "service content").unwrap();
        assert!(
            !share.walk().unwrap().rescan,
            "service churn does not require reconciliation"
        );
        std::fs::remove_dir_all(marker).unwrap();
        assert!(
            share.walk().unwrap().rescan,
            "marker removal requires reconciliation"
        );
    }

    #[test]
    fn other_clients_creates_writes_and_removals_arrive_without_native_events() {
        let mut share = Harness::new();
        // Negative control: an unchanged vault produces nothing.
        assert_eq!(share.walk(), Some(crate::Changes::default()));

        std::fs::write(share.path("Notes/new.md"), "# new\n").unwrap();
        std::fs::write(share.path("a.md"), "# a, edited elsewhere\n").unwrap();
        std::fs::remove_file(share.path("Notes/c.md")).unwrap();
        let changes = share.walk().unwrap();
        assert_eq!(changes.changed, set(&["Notes/new.md", "a.md"]));
        assert_eq!(changes.removed, set(&["Notes/c.md"]));
        assert!(!changes.rescan);
        assert_eq!(share.walk(), Some(crate::Changes::default()));

        // Attachments change the listed inventory; service trees are skipped.
        std::fs::create_dir_all(share.path(".git/objects")).unwrap();
        std::fs::write(share.path(".git/objects/x"), "blob").unwrap();
        std::fs::write(share.path("Notes/image.png"), "png").unwrap();
        let changes = share.walk().unwrap();
        assert!(changes.directories.contains("Notes"), "{changes:?}");
        assert!(changes
            .directories
            .iter()
            .all(|dir| !dir.starts_with(".git")));
        assert!(changes.changed.is_empty() && changes.removed.is_empty());
    }

    #[test]
    fn same_size_rewrite_inside_a_coarse_mtime_tick_is_still_reported() {
        let mut share = Harness::new();
        // Another client writes a note during the next walk's server tick.
        let fresh = share.path("Notes/fresh.md");
        std::fs::write(&fresh, "version 1\n").unwrap();
        let tick = coarse(share.now + Duration::from_secs(60));
        stamp(&fresh, tick);
        assert_eq!(share.walk().unwrap().changed, set(&["Notes/fresh.md"]));
        // Same size, same rounded stamp: metadata alone cannot see it.
        std::fs::write(&fresh, "version 2\n").unwrap();
        stamp(&fresh, tick);
        let changes = share.walk().unwrap();
        assert_eq!(changes.changed, set(&["Notes/fresh.md"]), "racy re-report");
        // Reported once: the stamp is now older than the racy window.
        assert_eq!(share.walk(), Some(crate::Changes::default()));
        // Control: an old stamp is trusted, so the same trick on a settled
        // note stays invisible to metadata polling (why Rescan rereads bytes).
        let settled = share.path("Notes/b.md");
        let old = std::fs::metadata(&settled).unwrap().modified().unwrap();
        std::fs::write(&settled, "# NOTE\n").unwrap();
        stamp(&settled, old);
        assert_eq!(share.walk(), Some(crate::Changes::default()));
    }

    #[test]
    fn renames_are_reported_as_removal_and_creation_including_case_only() {
        let mut share = Harness::new();
        std::fs::rename(share.path("Notes/b.md"), share.path("Notes/B.md")).unwrap();
        std::fs::rename(share.path("a.md"), share.path("Notes/moved.md")).unwrap();
        let changes = share.walk().unwrap();
        assert!(changes.removed.contains("Notes/b.md"), "{changes:?}");
        assert!(changes.removed.contains("a.md"), "{changes:?}");
        assert!(changes.changed.contains("Notes/moved.md"), "{changes:?}");
        // On a case-insensitive share both spellings name one file; the
        // caller's existence check then keeps it. Here the FS is sensitive.
        if !share.path("Notes/b.md").exists() {
            assert!(changes.changed.contains("Notes/B.md"), "{changes:?}");
        }

        // A whole folder renamed away: its notes are removed, both folders hinted.
        std::fs::rename(share.path("Notes"), share.path("Archive")).unwrap();
        let changes = share.walk().unwrap();
        assert!(changes.removed.contains("Notes/c.md"), "{changes:?}");
        assert!(changes.changed.contains("Archive/c.md"), "{changes:?}");
        assert!(changes.directories.contains("Notes"), "{changes:?}");
        assert!(changes.directories.contains("Archive"), "{changes:?}");
    }

    #[test]
    fn disconnected_share_never_reports_removals_and_catches_up_after_reconnect() {
        let mut share = Harness::new();
        share.share.offline.store(true, Ordering::SeqCst);
        for _ in 0..3 {
            assert_eq!(share.walk(), None, "offline is unavailable, not empty");
        }
        // Another client edits while this computer cannot reach the server.
        std::fs::write(share.path("Notes/b.md"), "# b, edited while offline\n").unwrap();
        std::fs::remove_file(share.path("Notes/c.md")).unwrap();

        share.share.offline.store(false, Ordering::SeqCst);
        let changes = share.walk().unwrap();
        assert_eq!(changes.changed, set(&["Notes/b.md"]));
        assert_eq!(changes.removed, set(&["Notes/c.md"]));
    }

    #[test]
    fn unreadable_subfolder_keeps_its_notes() {
        let mut share = Harness::new();
        *share.share.failing.lock().unwrap() = Some(share.path("Notes"));
        std::fs::write(share.path("a.md"), "# positive control, longer\n").unwrap();
        let changes = share.walk().unwrap();
        assert_eq!(changes.changed, set(&["a.md"]));
        assert!(changes.removed.is_empty(), "{changes:?}");
        assert_eq!(share.walk(), Some(crate::Changes::default()));
        // Once readable again, the folder's real state is reported.
        std::fs::remove_file(share.path("Notes/c.md")).unwrap();
        *share.share.failing.lock().unwrap() = None;
        assert_eq!(share.walk().unwrap().removed, set(&["Notes/c.md"]));
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
    fn baseline_missed_at_registration_requests_full_reconciliation() {
        // The share did not answer when the reader registered, so nothing
        // recorded what the inventory saw; a later baseline alone would hide
        // a deletion made in between.
        let mut share = Harness::registered(false);
        std::fs::remove_file(share.path("a.md")).unwrap();
        let changes = share.walk().unwrap();
        assert!(changes.rescan, "{changes:?}");
        // From then on, ordinary differences.
        std::fs::remove_file(share.path("Notes/c.md")).unwrap();
        let changes = share.walk().unwrap();
        assert!(!changes.rescan);
        assert_eq!(changes.removed, set(&["Notes/c.md"]));
    }

    #[test]
    fn entries_of_unknown_state_keep_their_stamp_and_subtree() {
        let mut share = Harness::new();
        // A note whose metadata fails is neither removed nor reported.
        *share.share.unknown.lock().unwrap() = Some(share.path("a.md"));
        std::fs::write(share.path("Notes/b.md"), "# positive control, longer\n").unwrap();
        let changes = share.walk().unwrap();
        assert_eq!(changes.changed, set(&["Notes/b.md"]));
        assert!(changes.removed.is_empty(), "{changes:?}");
        // A folder whose type fails keeps every descendant, unlisted.
        *share.share.unknown.lock().unwrap() = Some(share.path("Notes"));
        std::fs::remove_file(share.path("Notes/c.md")).unwrap();
        assert_eq!(share.walk(), Some(crate::Changes::default()));
        // Known again: the real state, including what changed meanwhile.
        *share.share.unknown.lock().unwrap() = None;
        assert_eq!(share.walk().unwrap().removed, set(&["Notes/c.md"]));
    }

    #[test]
    fn interrupted_listing_is_an_error_and_failed_entries_stay_entries() {
        let entry = |name: &str| Listed {
            name: name.into(),
            known: None,
        };
        let interrupted = collect(
            [
                Ok("a.md"),
                Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
                Ok("b.md"),
            ],
            |name| Some(entry(name)),
        );
        assert!(
            interrupted.is_err(),
            "partial listing must not look complete"
        );
        let complete = collect([Ok("a.md"), Ok("b.md")], |name| Some(entry(name))).unwrap();
        assert_eq!(complete.len(), 2);

        // A walk over a subfolder whose listing breaks partway keeps it whole.
        let mut share = Harness::new();
        *share.share.failing.lock().unwrap() = Some(share.path("Notes"));
        assert_eq!(share.walk(), Some(crate::Changes::default()));
    }

    /// The thread is driven by a gate in the lister: each root listing after
    /// registration announces itself and waits for permission, so the test
    /// knows exactly which walk has finished. Blocking handoffs, no sleeps.
    struct Gated {
        watcher: VaultWatcher,
        gate: mpsc::SyncSender<()>,
        entered: mpsc::Receiver<()>,
    }

    const LIMIT: Duration = Duration::from_secs(30);

    fn gated(root: &Path) -> Gated {
        let (gate, gate_rx) = mpsc::sync_channel::<()>(0);
        let (entered_tx, entered) = mpsc::channel::<()>();
        let (gate_rx, entered_tx) = (Mutex::new(gate_rx), Mutex::new(entered_tx));
        let registration = AtomicBool::new(true);
        let listed_root = root.to_path_buf();
        let config = Config {
            min: Duration::ZERO,
            max: Duration::ZERO,
            list: Arc::new(move |dir: &Path| {
                // The registration walk runs on the caller's thread, ungated.
                if dir == listed_root && !registration.swap(false, Ordering::SeqCst) {
                    let _ = entered_tx.lock().unwrap().send(());
                    gate_rx
                        .lock()
                        .unwrap()
                        .recv()
                        .map_err(|_| std::io::Error::other("gate closed"))?;
                }
                list_dir(dir)
            }),
            ..Config::default()
        };
        let watcher = VaultWatcher::polling_with(root, config).unwrap();
        // The thread's first walk is waiting at the gate.
        entered.recv_timeout(LIMIT).unwrap();
        Gated {
            watcher,
            gate,
            entered,
        }
    }

    impl Gated {
        /// Let exactly one walk run; its events are sent when this returns.
        fn walk(&mut self) -> Option<crate::Changes> {
            self.gate.send(()).unwrap();
            self.entered.recv_timeout(LIMIT).unwrap();
            self.watcher.drain_preparation_changes()
        }
    }

    #[test]
    fn note_deleted_after_registration_before_any_thread_walk_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for note in ["kept.md", "deleted.md"] {
            std::fs::write(root.join(note), "# note\n").unwrap();
            stamp(&root.join(note), epoch());
        }
        // Registration returns with the baseline recorded; the reader's
        // inventory then captures both notes.
        let mut share = gated(&root);
        assert!(!share.watcher.polling_status().unwrap().unavailable);
        // Deleted before the polling thread has walked even once, with no
        // other filesystem activity.
        std::fs::remove_file(root.join("deleted.md")).unwrap();
        let changes = share.walk().expect("the deletion must be reported");
        assert_eq!(changes.removed, set(&["deleted.md"]));
        assert!(changes.changed.is_empty() && !changes.rescan, "{changes:?}");
    }

    #[test]
    fn background_thread_delivers_walks_and_stops_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("a.md"), "# a\n").unwrap();
        stamp(&root.join("a.md"), epoch());
        let mut share = gated(&root);
        assert_eq!(share.walk(), None, "unchanged vault");
        std::fs::write(root.join("new.md"), "# new\n").unwrap();
        let changes = share.walk().unwrap();
        assert!(changes.changed.contains("new.md"), "{changes:?}");
        let status = share.watcher.polling_status().unwrap();
        assert!(!status.unavailable && status.last_walk.is_some());

        let Gated {
            watcher,
            gate,
            entered,
        } = share;
        drop(watcher);
        // Closing the gate fails the blocked listing; the stopped thread then
        // exits and drops the lister, which disconnects `entered`.
        drop(gate);
        assert_eq!(
            entered.recv_timeout(LIMIT),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "poll thread outlived its watcher"
        );
    }
}
