//! Follow mode for one log file (#602, slice 5).
//!
//! A [`LogTail`] holds the bytes read so far from the followed file and their
//! [`LogIndex`]. It owns no thread and no watcher: the caller polls it, at
//! [`LogTail::interval`], and decides where each step runs.
//!
//! A poll has two halves so the slow one can leave the UI thread:
//!
//! - [`LogTail::begin_poll`] captures what is held (a cheap, `Send` request);
//!   [`PollRequest::read`] does all file IO and can run on any worker.
//! - [`LogTail::apply`] indexes the new bytes into the held index. A read taken
//!   at an older generation is discarded as [`TailEvent::Stale`], so a slow
//!   poll that finishes late never overwrites a newer state.
//!
//! [`LogTail::poll`] does both in place.
//!
//! What a read can find:
//!
//! - **Append.** Only the new bytes are read and indexed; old entries are not
//!   parsed again. A last line without its newline is held back, not indexed,
//!   until the newline arrives.
//! - **Truncate** (the file shrank, or the bytes just before the read offset
//!   changed: copy-truncate followed by a larger rewrite). The held content is
//!   finished and handed back; indexing restarts at byte 0.
//! - **Rotate** (the path now names a different file: rename and recreate).
//!   The old file is read to its end and finished, then the new one is
//!   followed from byte 0. Identity is device and inode on Unix, volume serial
//!   number and file index on Windows.
//!
//! The followed file is read with positional reads through a shared-mode
//! handle and is never mapped, so the writer stays free to truncate, rename or
//! delete it (docs/research/602-hl-log-viewer.md, §3.6). Nothing is written.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};

use super::detect::{self, SAMPLE_BYTES, SAMPLE_LINES};
use super::{LogIndex, Record, MAX_LOG_BYTES};

/// Poll interval for a file on a local disk.
pub const LOCAL_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Poll interval for a file on a network root, where every poll is a round
/// trip and native change events from other clients are not reliable.
pub const NETWORK_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Bytes before the read offset compared on every growing poll. A file that
/// was truncated and rewritten past the old offset between two polls differs
/// there; one that rewrote these bytes identically is indistinguishable.
const GUARD_BYTES: usize = 64;

/// Generations are unique in the process, so a read taken from one follower
/// can never be accepted by another.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// The poll interval suited to `path`'s file system. It probes the file
/// system, so call it off the UI thread.
pub fn poll_interval_for(path: &Path) -> Duration {
    let root = path.parent().unwrap_or(path);
    if crate::watch::is_network_root(root) {
        NETWORK_POLL_INTERVAL
    } else {
        LOCAL_POLL_INTERVAL
    }
}

/// Bytes and index of a file incarnation that follow mode finished, after a
/// truncation or rotation. Its last line is indexed even without a newline.
#[derive(Debug)]
pub struct LogSegment {
    bytes: Vec<u8>,
    index: LogIndex,
}

impl LogSegment {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn index(&self) -> &LogIndex {
        &self.index
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    /// The exact bytes of entry `index`, without its line ending.
    pub fn raw(&self, index: usize) -> Option<&[u8]> {
        self.index.raw(&self.bytes, index)
    }
    /// Entry `index` decoded into fields; `None` for an unparsed line.
    pub fn record(&self, index: usize) -> Option<Record> {
        Record::parse(self.raw(index)?, self.index.format())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetReason {
    /// Same file, but it shrank or its earlier bytes changed.
    Truncated,
    /// The path names a different file now.
    Rotated,
}

#[derive(Debug)]
pub enum TailEvent {
    /// No new entry. Bytes of an unfinished line may have been held back.
    Unchanged,
    /// The read was taken at an older generation and was discarded. Poll again.
    Stale,
    /// `entries` are new. With `reindexed`, the format verdict changed while
    /// the file was still shorter than the detection sample, and every earlier
    /// entry was indexed again (the entry count before `entries` is unchanged).
    Appended {
        entries: Range<usize>,
        reindexed: bool,
    },
    /// The previous content was finished and handed back; the index restarted
    /// at byte 0 and `entries` are what the file holds now.
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

/// What a poll needs from the follower. Holds no borrow; send it to a worker.
pub struct PollRequest {
    generation: u64,
    path: PathBuf,
    file: Arc<File>,
    id: FileId,
    offset: u64,
    guard: Vec<u8>,
}

/// The result of [`PollRequest::read`], to hand to [`LogTail::apply`].
pub struct PollRead {
    generation: u64,
    missing: bool,
    change: Change,
}

impl PollRead {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

enum Change {
    None,
    Grew(Vec<u8>),
    Truncated(Vec<u8>),
    Rotated {
        old_tail: Vec<u8>,
        file: Arc<File>,
        id: FileId,
        bytes: Vec<u8>,
    },
}

pub struct LogTail {
    path: PathBuf,
    file: Arc<File>,
    id: FileId,
    /// Everything read from the current file incarnation, including an
    /// unfinished last line after `index.bytes()`.
    bytes: Vec<u8>,
    index: LogIndex,
    /// Whether the format verdict has seen its whole sample and is final.
    settled: bool,
    missing: bool,
    epoch: u64,
    generation: u64,
    interval: Duration,
    last_poll: Option<Instant>,
}

impl LogTail {
    /// Opens `path` and reads what it holds now. Nothing is written.
    pub fn open(path: &Path) -> Result<Self> {
        let file = open_shared(path).context("The log file cannot be opened")?;
        let id = identity(&file).context("The log file cannot be inspected")?;
        let mut tail = Self {
            path: path.to_owned(),
            file: Arc::new(file),
            id,
            bytes: Vec::new(),
            index: LogIndex::build(&[]),
            settled: false,
            missing: false,
            epoch: 0,
            generation: next_generation(),
            interval: LOCAL_POLL_INTERVAL,
            last_poll: None,
        };
        tail.poll()?;
        Ok(tail)
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The bytes read so far, an unfinished last line included.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// The index over the complete lines of [`Self::bytes`].
    pub fn index(&self) -> &LogIndex {
        &self.index
    }
    /// An unfinished last line, held until its newline arrives.
    pub fn pending(&self) -> &[u8] {
        &self.bytes[self.index.bytes() as usize..]
    }
    /// The exact bytes of entry `index`, without its line ending.
    pub fn raw(&self, index: usize) -> Option<&[u8]> {
        self.index.raw(&self.bytes, index)
    }
    /// Entry `index` decoded into fields; `None` for an unparsed line.
    pub fn record(&self, index: usize) -> Option<Record> {
        Record::parse(self.raw(index)?, self.index.format())
    }
    /// Counts truncations and rotations since opening.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Changes whenever the held state changes. A read taken at another
    /// generation is stale.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// The path named no file at the last poll (mid-rotation, or deleted).
    /// The open file is still followed meanwhile.
    pub fn is_missing(&self) -> bool {
        self.missing
    }
    pub fn interval(&self) -> Duration {
        self.interval
    }
    /// Whether a poll is due at `now`, one interval after the last one began.
    pub fn is_due(&self, now: Instant) -> bool {
        self.last_poll
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval)
    }

    /// Reads and applies in place.
    pub fn poll(&mut self) -> Result<TailEvent> {
        let read = self.begin_poll().read()?;
        Ok(self.apply(read))
    }

    pub fn begin_poll(&mut self) -> PollRequest {
        self.last_poll = Some(Instant::now());
        let end = self.bytes.len();
        PollRequest {
            generation: self.generation,
            path: self.path.clone(),
            file: Arc::clone(&self.file),
            id: self.id,
            offset: end as u64,
            guard: self.bytes[end.saturating_sub(GUARD_BYTES)..].to_vec(),
        }
    }

    pub fn apply(&mut self, read: PollRead) -> TailEvent {
        if read.generation != self.generation {
            return TailEvent::Stale;
        }
        self.missing = read.missing;
        match read.change {
            Change::None => TailEvent::Unchanged,
            Change::Grew(bytes) => {
                self.generation = next_generation();
                self.bytes.extend_from_slice(&bytes);
                self.index_complete_lines()
            }
            Change::Truncated(bytes) => self.reset(ResetReason::Truncated, &[], None, bytes),
            Change::Rotated {
                old_tail,
                file,
                id,
                bytes,
            } => self.reset(ResetReason::Rotated, &old_tail, Some((file, id)), bytes),
        }
    }

    fn index_complete_lines(&mut self) -> TailEvent {
        let start = self.index.bytes() as usize;
        let Some(at) = memchr::memrchr(b'\n', &self.bytes[start..]) else {
            return TailEvent::Unchanged;
        };
        let before = self.index.len();
        let reindexed = self.index_to(start + at + 1) && before > 0;
        if self.index.len() == before && !reindexed {
            return TailEvent::Unchanged;
        }
        TailEvent::Appended {
            entries: before..self.index.len(),
            reindexed,
        }
    }

    /// Indexes the held bytes up to `end`. Returns whether the whole index
    /// was rebuilt because the format verdict changed.
    fn index_to(&mut self, end: usize) -> bool {
        let mut rebuilt = false;
        if !self.settled {
            // The verdict reads at most the first SAMPLE_LINES non-blank lines
            // or SAMPLE_BYTES, so re-detecting here touches only a short file.
            let format = detect::detect(&self.bytes[..end]);
            if format != self.index.format() {
                self.index = LogIndex::build(&self.bytes[..end]);
                rebuilt = true;
            }
        }
        if !rebuilt {
            self.index.extend(&self.bytes, end);
        }
        self.settled = end >= SAMPLE_BYTES || self.index.len() >= SAMPLE_LINES;
        rebuilt
    }

    fn reset(
        &mut self,
        reason: ResetReason,
        old_tail: &[u8],
        file: Option<(Arc<File>, FileId)>,
        bytes: Vec<u8>,
    ) -> TailEvent {
        // Finish the old incarnation: its last line is complete now, newline
        // or not, and nothing that was read is dropped.
        self.bytes.extend_from_slice(old_tail);
        self.index_to(self.bytes.len());
        let previous = LogSegment {
            bytes: std::mem::replace(&mut self.bytes, bytes),
            index: std::mem::replace(&mut self.index, LogIndex::build(&[])),
        };
        if let Some((file, id)) = file {
            self.file = file;
            self.id = id;
        }
        self.settled = false;
        self.epoch += 1;
        self.generation = next_generation();
        self.index_complete_lines();
        TailEvent::Reset {
            reason,
            previous,
            entries: 0..self.index.len(),
        }
    }
}

impl PollRequest {
    /// Does the file IO of one poll. Blocking; run it wherever the caller
    /// likes. The follower is not touched.
    pub fn read(self) -> Result<PollRead> {
        let at_path = path_identity(&self.path).context("The log file cannot be inspected")?;
        let change = match at_path {
            Some(id) if id != self.id => self.rotated()?,
            _ => self.same_file()?,
        };
        Ok(PollRead {
            generation: self.generation,
            missing: at_path.is_none(),
            change,
        })
    }

    fn same_file(&self) -> Result<Change> {
        let len = checked_len(&self.file)?;
        if len < self.offset {
            return Ok(Change::Truncated(read_all(&self.file)?));
        }
        if len == self.offset {
            return Ok(Change::None);
        }
        // Read the guard bytes again with the new ones; one read either way.
        let start = self.offset - self.guard.len() as u64;
        let mut bytes =
            read_range(&self.file, start, len).context("The log file cannot be read")?;
        if !bytes.starts_with(&self.guard) {
            return Ok(Change::Truncated(read_all(&self.file)?));
        }
        bytes.drain(..self.guard.len());
        Ok(Change::Grew(bytes))
    }

    fn rotated(&self) -> Result<Change> {
        // A writer keeps appending to the renamed file until it reopens the
        // path; read the old file to its end before leaving it.
        let old_len = checked_len(&self.file)?;
        let old_tail = if old_len > self.offset {
            read_range(&self.file, self.offset, old_len).context("The log file cannot be read")?
        } else {
            Vec::new()
        };
        let file = open_shared(&self.path).context("The rotated log file cannot be opened")?;
        let id = identity(&file).context("The rotated log file cannot be inspected")?;
        let bytes = read_all(&file)?;
        Ok(Change::Rotated {
            old_tail,
            file: Arc::new(file),
            id,
            bytes,
        })
    }
}

fn checked_len(file: &File) -> Result<u64> {
    let len = file
        .metadata()
        .context("The log file cannot be inspected")?
        .len();
    ensure!(
        len <= MAX_LOG_BYTES,
        "The log file is larger than the viewer's {} GiB limit",
        MAX_LOG_BYTES >> 30
    );
    Ok(len)
}

fn read_all(file: &File) -> Result<Vec<u8>> {
    let len = checked_len(file)?;
    read_range(file, 0, len).context("The log file cannot be read")
}

/// Reads `start..end` with positional reads, shorter if the file ends first.
/// The handle's cursor is never used, so requests can share it.
fn read_range(file: &File, start: u64, end: u64) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; end.saturating_sub(start) as usize];
    let mut filled = 0;
    while filled < bytes.len() {
        match read_at(file, &mut bytes[filled..], start + filled as u64) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    bytes.truncate(filled);
    Ok(bytes)
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_at(mut file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    file.read(buf)
}

/// Read-only, and on Windows shared for read, write and delete, so the writer
/// can still truncate, rename or delete the file while it is followed.
fn open_shared(path: &Path) -> io::Result<File> {
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
