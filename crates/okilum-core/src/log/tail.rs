//! Bounded, incremental Follow over shared immutable snapshots (#1389).
use super::window::{ReadAt, WINDOW_BYTES};
use super::window_budget::Reservation;
use super::{LogIndex, LogSnapshot, Record, WindowInfo};
use anyhow::{ensure, Context, Result};
use std::{
    fs::File,
    io,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
pub const LOCAL_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub const NETWORK_POLL_INTERVAL: Duration = Duration::from_secs(2);
pub(super) const POLL_BYTES: usize = 4 * 1024 * 1024;
const GUARD_BYTES: usize = 64;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}
pub fn poll_interval_for(path: &Path) -> Duration {
    if crate::watch::is_network_root(path.parent().unwrap_or(path)) {
        NETWORK_POLL_INTERVAL
    } else {
        LOCAL_POLL_INTERVAL
    }
}
#[derive(Debug)]
pub struct LogSegment {
    snapshot: Arc<LogSnapshot>,
}
impl LogSegment {
    pub fn bytes(&self) -> &[u8] {
        self.snapshot.bytes()
    }
    pub fn index(&self) -> &LogIndex {
        self.snapshot.index()
    }
    pub fn window(&self) -> WindowInfo {
        self.snapshot.window()
    }
    pub fn snapshot(&self) -> Arc<LogSnapshot> {
        self.snapshot.clone()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes().is_empty()
    }
    pub fn raw(&self, index: usize) -> Option<&[u8]> {
        self.snapshot.raw(index)
    }
    pub fn record(&self, index: usize) -> Option<Record> {
        self.snapshot.record(index)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetReason {
    Truncated,
    Rotated,
}
#[derive(Debug)]
pub enum TailEvent {
    Unchanged,
    Stale,
    /// No state was advanced: retry after background snapshots release memory.
    Deferred(String),
    /// The loaded prefix was evicted. All window-relative entry IDs changed.
    WindowChanged,
    Appended {
        entries: Range<usize>,
        reindexed: bool,
        grown: bool,
    },
    Reset {
        reason: ResetReason,
        previous: LogSegment,
        entries: Range<usize>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileId {
    device: u64,
    index: u64,
}
pub struct PollRequest {
    generation: u64,
    path: PathBuf,
    file: Arc<File>,
    id: FileId,
    offset: u64,
    guard: Vec<u8>,
    regex: Option<Arc<super::regex_text::RegexParser>>,
}
pub struct PollRead {
    generation: u64,
    missing: bool,
    change: Change,
    _lease: Option<Reservation>,
}
impl PollRead {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
enum Change {
    None,
    Grew {
        bytes: Vec<u8>,
        end: u64,
        file_len: u64,
        guard: Vec<u8>,
    },
    Window {
        snapshot: Arc<LogSnapshot>,
        guard: Vec<u8>,
    },
    Reset {
        reason: ResetReason,
        snapshot: Arc<LogSnapshot>,
        guard: Vec<u8>,
        file: Arc<File>,
        id: FileId,
        old_snapshot: Option<Arc<LogSnapshot>>,
        old_tail: Vec<u8>,
        old_end: u64,
        old_len: u64,
    },
}
pub struct LogTail {
    path: PathBuf,
    file: Arc<File>,
    id: FileId,
    snapshot: Arc<LogSnapshot>,
    guard: Vec<u8>,
    missing: bool,
    epoch: u64,
    generation: u64,
    interval: Duration,
    last_poll: Option<Instant>,
    skip_partial: bool,
}
impl LogTail {
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(
            super::compression(path).is_none(),
            "A compressed log does not grow and cannot be followed"
        );
        let file = Arc::new(open_shared(path).context("The log file cannot be opened")?);
        let id = identity(&file)?;
        let snapshot = LogSnapshot::read(&file, true, None)?;
        let guard = snapshot_guard(&file, &snapshot)?;
        let skip_partial = snapshot.bytes().is_empty()
            && snapshot.window().offset > 0
            && guard.last() != Some(&b'\n');
        Ok(Self {
            path: path.to_owned(),
            file,
            id,
            snapshot,
            guard,
            missing: false,
            epoch: 0,
            generation: next_generation(),
            interval: LOCAL_POLL_INTERVAL,
            last_poll: Some(Instant::now()),
            skip_partial,
        })
    }
    pub fn with_regex(mut self, regex: Arc<super::regex_text::RegexParser>) -> Result<Self> {
        LogSnapshot::mutable(&mut self.snapshot)?.set_regex(regex);
        Ok(self)
    }
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn bytes(&self) -> &[u8] {
        self.snapshot.bytes()
    }
    pub fn index(&self) -> &LogIndex {
        self.snapshot.index()
    }
    pub fn window(&self) -> WindowInfo {
        self.snapshot.window()
    }
    pub fn snapshot(&self) -> Arc<LogSnapshot> {
        self.snapshot.clone()
    }
    pub fn pending(&self) -> &[u8] {
        &self.bytes()[self.index().bytes() as usize..]
    }
    pub fn raw(&self, index: usize) -> Option<&[u8]> {
        self.snapshot.raw(index)
    }
    pub fn record(&self, index: usize) -> Option<Record> {
        self.snapshot.record(index)
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn is_missing(&self) -> bool {
        self.missing
    }
    pub fn interval(&self) -> Duration {
        self.interval
    }
    pub fn is_due(&self, now: Instant) -> bool {
        self.last_poll
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval)
    }
    pub fn poll(&mut self) -> Result<TailEvent> {
        let read = self.begin_poll().read()?;
        Ok(self.apply(read))
    }
    pub fn begin_poll(&mut self) -> PollRequest {
        self.last_poll = Some(Instant::now());
        PollRequest {
            generation: self.generation,
            path: self.path.clone(),
            file: self.file.clone(),
            id: self.id,
            offset: self.window().end,
            guard: self.guard.clone(),
            regex: self.index().regex().cloned(),
        }
    }
    pub fn apply(&mut self, read: PollRead) -> TailEvent {
        if read.generation != self.generation {
            return TailEvent::Stale;
        }
        self.missing = read.missing;
        match read.change {
            Change::None => TailEvent::Unchanged,
            Change::Grew {
                bytes,
                end,
                file_len,
                guard,
            } => {
                let before = self.index().len();
                let snapshot = match LogSnapshot::mutable(&mut self.snapshot) {
                    Ok(snapshot) => snapshot,
                    Err(e) => return TailEvent::Deferred(format!("{e:#}")),
                };
                let mut incoming = bytes.as_slice();
                if self.skip_partial {
                    if let Err(error) = snapshot.prepare_skip_append(incoming.len()) {
                        return TailEvent::Deferred(format!("{error:#}"));
                    }
                    let skip = memchr::memchr(b'\n', incoming).map_or(incoming.len(), |at| at + 1);
                    snapshot.skip_to(end - incoming.len() as u64 + skip as u64, file_len);
                    self.skip_partial = skip == incoming.len() && incoming.last() != Some(&b'\n');
                    incoming = &incoming[skip..];
                }
                let (slid, grown, reindexed) = match snapshot.append(incoming, end, file_len) {
                    Ok(result) => result,
                    Err(e) => return TailEvent::Deferred(format!("{e:#}")),
                };
                self.skip_partial = snapshot.bytes().is_empty()
                    && snapshot.window().offset > 0
                    && guard.last() != Some(&b'\n');
                self.guard = guard;
                self.generation = next_generation();
                if slid {
                    TailEvent::WindowChanged
                } else if self.index().len() == before && !grown && !reindexed {
                    TailEvent::Unchanged
                } else {
                    TailEvent::Appended {
                        entries: before..self.index().len(),
                        reindexed: reindexed && before > 0,
                        grown,
                    }
                }
            }
            Change::Window { snapshot, guard } => {
                self.snapshot = snapshot;
                self.guard = guard;
                self.skip_partial = self.bytes().is_empty() && self.guard.last() != Some(&b'\n');
                self.generation = next_generation();
                TailEvent::WindowChanged
            }
            Change::Reset {
                reason,
                snapshot,
                guard,
                file,
                id,
                old_snapshot,
                old_tail,
                old_end,
                old_len,
            } => {
                // A backlog larger than one poll is captured as a bounded
                // final suffix, never an arbitrary prefix cut mid-record.
                let previous = if let Some(old) = old_snapshot {
                    self.snapshot = snapshot;
                    LogSegment { snapshot: old }
                } else {
                    let old = match LogSnapshot::mutable(&mut self.snapshot) {
                        Ok(old) => old,
                        Err(e) => return TailEvent::Deferred(format!("{e:#}")),
                    };
                    if !old_tail.is_empty() {
                        let mut incoming = old_tail.as_slice();
                        if self.skip_partial {
                            if let Err(error) = old.prepare_skip_append(incoming.len()) {
                                return TailEvent::Deferred(format!("{error:#}"));
                            }
                            let skip =
                                memchr::memchr(b'\n', incoming).map_or(incoming.len(), |at| at + 1);
                            old.skip_to(old_end - incoming.len() as u64 + skip as u64, old_len);
                            incoming = &incoming[skip..];
                        }
                        if let Err(error) = old.append(incoming, old_end, old_len) {
                            return TailEvent::Deferred(format!("{error:#}"));
                        }
                    }
                    old.finish();
                    LogSegment {
                        snapshot: std::mem::replace(&mut self.snapshot, snapshot),
                    }
                };
                self.guard = guard;
                self.file = file;
                self.id = id;
                self.skip_partial = self.bytes().is_empty() && self.guard.last() != Some(&b'\n');
                self.epoch += 1;
                self.generation = next_generation();
                TailEvent::Reset {
                    reason,
                    previous,
                    entries: 0..self.index().len(),
                }
            }
        }
    }
}
impl PollRequest {
    pub fn read(self) -> Result<PollRead> {
        let at_path = path_identity(&self.path).context("The log file cannot be inspected")?;
        let missing = at_path.is_none();
        let (change, lease) = if at_path.is_some_and(|id| id != self.id) {
            let file =
                Arc::new(open_shared(&self.path).context("The rotated log cannot be opened")?);
            let id = identity(&file)?;
            let old_len = self.file.metadata()?.len();
            let old_end = old_len.min(self.offset + POLL_BYTES as u64);
            let large_backlog = old_len.saturating_sub(self.offset) > POLL_BYTES as u64;
            let lease = super::snapshot::reserve(if large_backlog {
                1
            } else {
                old_end.saturating_sub(self.offset) as usize
            })?;
            let old_snapshot = if large_backlog {
                let old = LogSnapshot::read(&self.file, false, self.regex.clone())?;
                snapshot_guard(&self.file, &old)?;
                Some(old)
            } else {
                None
            };
            let old_tail = if large_backlog {
                Vec::new()
            } else {
                read_range(&self.file, self.offset, old_end.max(self.offset))?
            };
            let snapshot = LogSnapshot::read(&file, true, self.regex.clone())?;
            let guard = snapshot_guard(&file, &snapshot)?;
            (
                Change::Reset {
                    reason: ResetReason::Rotated,
                    snapshot,
                    guard,
                    file,
                    id,
                    old_snapshot,
                    old_tail,
                    old_end,
                    old_len,
                },
                Some(lease),
            )
        } else {
            let len = self.file.metadata()?.len();
            let same_guard = len >= self.offset && guard_at(&self.file, self.offset)? == self.guard;
            if !same_guard {
                let snapshot = LogSnapshot::read(&self.file, true, self.regex.clone())?;
                let guard = snapshot_guard(&self.file, &snapshot)?;
                (
                    Change::Reset {
                        reason: ResetReason::Truncated,
                        snapshot,
                        guard,
                        file: self.file.clone(),
                        id: self.id,
                        old_snapshot: None,
                        old_tail: Vec::new(),
                        old_end: self.offset,
                        old_len: self.offset,
                    },
                    None,
                )
            } else if len == self.offset {
                (Change::None, None)
            } else if len - self.offset > WINDOW_BYTES as u64 {
                let snapshot = LogSnapshot::read(&self.file, true, self.regex.clone())?;
                let guard = snapshot_guard(&self.file, &snapshot)?;
                (Change::Window { snapshot, guard }, None)
            } else {
                let end = len.min(self.offset + POLL_BYTES as u64);
                let lease = super::snapshot::reserve((end - self.offset) as usize)?;
                let bytes = read_range(&self.file, self.offset, end)?;
                let end = self.offset + bytes.len() as u64;
                if guard_at(&self.file, self.offset)? != self.guard {
                    let snapshot = LogSnapshot::read(&self.file, true, self.regex.clone())?;
                    let guard = snapshot_guard(&self.file, &snapshot)?;
                    return Ok(PollRead {
                        generation: self.generation,
                        missing,
                        change: Change::Reset {
                            reason: ResetReason::Truncated,
                            snapshot,
                            guard,
                            file: self.file.clone(),
                            id: self.id,
                            old_snapshot: None,
                            old_tail: Vec::new(),
                            old_end: self.offset,
                            old_len: self.offset,
                        },
                        _lease: Some(lease),
                    });
                }
                let guard = guard_at(&self.file, end)?;
                (
                    Change::Grew {
                        bytes,
                        end,
                        file_len: len,
                        guard,
                    },
                    Some(lease),
                )
            }
        };
        Ok(PollRead {
            generation: self.generation,
            missing,
            change,
            _lease: lease,
        })
    }
}
fn snapshot_guard(file: &File, snapshot: &LogSnapshot) -> io::Result<Vec<u8>> {
    let guard = guard_at(file, snapshot.window().end)?;
    let suffix = snapshot.bytes();
    let n = guard.len().min(suffix.len());
    if guard[guard.len() - n..] != suffix[suffix.len() - n..] {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Log changed while reading the window; retry",
        ));
    }
    Ok(guard)
}
fn guard_at(file: &File, end: u64) -> io::Result<Vec<u8>> {
    read_range(file, end.saturating_sub(GUARD_BYTES as u64), end)
}
fn read_range(file: &File, start: u64, end: u64) -> io::Result<Vec<u8>> {
    if end.saturating_sub(start) > (POLL_BYTES + GUARD_BYTES) as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unbounded log poll rejected",
        ));
    }
    let mut bytes = vec![0; end.saturating_sub(start) as usize];
    let mut n = 0;
    while n < bytes.len() {
        match file.read_at(&mut bytes[n..], start + n as u64) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    bytes.truncate(n);
    Ok(bytes)
}

/// Read-only, and on Windows shared for read, write and delete, so the writer
/// can still truncate, rename or delete the file while it is followed.
pub(super) fn open_shared(path: &Path) -> io::Result<File> {
    let mut options = File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    options.open(path)
}

/// The identity of the file `path` names now; `None` when it names none.
fn path_identity(path: &Path) -> io::Result<Option<FileId>> {
    #[cfg(unix)]
    let found = {
        use std::os::unix::fs::MetadataExt;
        // Follows a symlink: a link re-pointed at a new file is a rotation.
        std::fs::metadata(path).map(|meta| FileId {
            device: meta.dev(),
            index: meta.ino(),
        })
    };
    #[cfg(windows)]
    let found = {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        // A metadata-only handle that denies no other opener.
        File::options()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .and_then(|file| identity(&file))
    };
    #[cfg(not(any(unix, windows)))]
    let found = std::fs::metadata(path).and_then(|_| identity_unknown());
    match found {
        Ok(id) => Ok(Some(id)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        // A file deleted while another handle holds it open is "delete
        // pending" until that handle closes; opening it again is refused.
        #[cfg(windows)]
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn identity(file: &File) -> io::Result<FileId> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(FileId {
        device: meta.dev(),
        index: meta.ino(),
    })
}

#[cfg(windows)]
fn identity(file: &File) -> io::Result<FileId> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // The handle is owned by `file` and stays open for the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileId {
        device: u64::from(info.dwVolumeSerialNumber),
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

/// Without a platform file identity, rotation looks like the same file:
/// truncation is still detected, a rename and recreate is not.
#[cfg(not(any(unix, windows)))]
fn identity(_file: &File) -> io::Result<FileId> {
    identity_unknown()
}

#[cfg(not(any(unix, windows)))]
fn identity_unknown() -> io::Result<FileId> {
    Ok(FileId {
        device: 0,
        index: 0,
    })
}

#[cfg(test)]
mod tests;
