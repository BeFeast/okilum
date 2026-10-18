//! Immutable, leased log windows shared by UI, filters, counts and exports.
use super::window::{
    self, ReadAt, ReadBudget, ReadWindow, WINDOW_BYTES, WINDOW_LINES, WINDOW_LINE_BYTES,
};
use super::window_budget::{MemoryBudget, Reservation};
use super::{block, detect, record, Format, LogIndex, Record};
use anyhow::{ensure, Context, Result};
use std::{
    fs::File,
    io,
    sync::{Arc, OnceLock},
};

/// Includes raw capacities, index construction/copies and parser scratch.
/// IO buffers reserve from this same process-wide pool before allocating.
const POOL_BYTES: usize = 320 * 1024 * 1024;
const SCRATCH_BYTES: usize = 8 * 1024 * 1024;
pub(super) fn pool() -> &'static MemoryBudget {
    static POOL: OnceLock<MemoryBudget> = OnceLock::new();
    POOL.get_or_init(|| MemoryBudget::new(POOL_BYTES))
}
pub(super) fn reserve(bytes: usize) -> Result<Reservation> {
    pool()
        .reserve(bytes.max(1))
        .context(super::window_budget::BudgetBusy)
}
fn charge(capacity: usize, lines: usize) -> usize {
    capacity + lines.min(WINDOW_LINES) * std::mem::size_of::<super::LogEntry>() * 4 + SCRATCH_BYTES
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowInfo {
    /// Absolute byte offset corresponding to bytes()[0].
    pub offset: u64,
    pub end: u64,
    pub file_len: u64,
    /// Sticky: the window has omitted earlier data or oversized records.
    pub tail: bool,
    pub oversized: bool,
    /// Offsets refer to decoded bytes, not compressed source positions.
    pub decoded: bool,
}
impl WindowInfo {
    pub fn absolute_offset(&self, entry: &super::LogEntry) -> u64 {
        self.offset + entry.offset()
    }
    /// Suffix-relative line numbers must never masquerade as file lines.
    pub fn file_line(&self, entry: &super::LogEntry) -> Option<u64> {
        (self.offset == 0).then(|| entry.line())
    }
}

pub struct LogSnapshot {
    bytes: Vec<u8>,
    index: LogIndex,
    info: WindowInfo,
    complete_only: bool,
    lease: Reservation,
}
impl std::fmt::Debug for LogSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogSnapshot")
            .field("window", &self.info)
            .field("entries", &self.index.len())
            .finish()
    }
}
impl LogSnapshot {
    pub fn reserved_bytes(&self) -> usize {
        self.lease.bytes()
    }
    pub fn memory_used() -> usize {
        pool().used()
    }
    pub fn memory_limit() -> usize {
        pool().limit()
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn index(&self) -> &LogIndex {
        &self.index
    }
    pub fn window(&self) -> WindowInfo {
        self.info
    }
    pub fn raw(&self, index: usize) -> Option<&[u8]> {
        self.index.raw(&self.bytes, index)
    }
    pub fn record(&self, index: usize) -> Option<Record> {
        self.index.record(self.raw(index)?)
    }
    pub(super) fn read(
        file: &File,
        complete: bool,
        regex: Option<Arc<super::regex_text::RegexParser>>,
    ) -> Result<Arc<Self>> {
        let len = file.metadata()?.len();
        // Reserve the worst-case read/index before even the raw allocation.
        let capacity = len.min(WINDOW_BYTES as u64) as usize;
        // At most one physical line per byte. Tiny files must not reserve
        // the dense-window maximum while many independent views are opening.
        let lease = reserve(charge(capacity, capacity.min(WINDOW_LINES)))?;
        let mut sample = vec![0; len.min(detect::SAMPLE_BYTES as u64) as usize];
        let mut n = 0;
        while n < sample.len() {
            match file.read_at(&mut sample[n..], n as u64) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        sample.truncate(n);
        let format = detect::detect(&sample);
        let window = window::read_tail(file, len, ReadBudget::default())?;
        ensure!(
            !window.shortened(),
            "The log was truncated while opening; reopen it"
        );
        Self::from_window(window, format, regex, complete, lease).map(Arc::new)
    }
    pub(super) fn decoded(bytes: Vec<u8>, lease: Reservation) -> Result<Arc<Self>> {
        let len = bytes.len() as u64;
        let format = detect::detect(&bytes[..bytes.len().min(detect::SAMPLE_BYTES)]);
        let mut snapshot = Self::from_window(
            ReadWindow {
                bytes,
                offset: 0,
                end: len,
                file_len: len,
                oversized: false,
            },
            format,
            None,
            false,
            lease,
        )?;
        snapshot.info.decoded = true;
        Ok(Arc::new(snapshot))
    }
    fn from_window(
        mut w: ReadWindow,
        format: Format,
        regex: Option<Arc<super::regex_text::RegexParser>>,
        complete: bool,
        lease: Reservation,
    ) -> Result<Self> {
        window::bound_lines(&mut w, ReadBudget::default());
        if w.offset > 0 {
            align(&mut w, format, regex.is_some(), complete);
        }
        let end = indexed_end(&w.bytes, complete);
        let index = LogIndex::build_format(&w.bytes[..end], format, regex, 1);
        let mut s = Self {
            info: WindowInfo {
                offset: w.offset,
                end: w.end,
                file_len: w.file_len,
                tail: w.offset > 0 || w.oversized,
                oversized: w.oversized,
                decoded: false,
            },
            bytes: w.bytes,
            index,
            complete_only: complete,
            lease,
        };
        s.exclude_oversized();
        s.recharge()?;
        Ok(s)
    }
    fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
    fn recharge(&mut self) -> Result<()> {
        ensure!(
            self.lease
                .resize(charge(self.capacity(), physical_lines(&self.bytes))),
            super::window_budget::BudgetBusy
        );
        Ok(())
    }
    /// COW is fallible and charged BEFORE cloning. Old background generations
    /// retain their own lease until the last reader actually releases them.
    pub(super) fn mutable(snapshot: &mut Arc<Self>) -> Result<&mut Self> {
        if Arc::get_mut(snapshot).is_none() {
            let lease = reserve(charge(snapshot.capacity(), physical_lines(&snapshot.bytes)))?;
            let mut bytes = Vec::with_capacity(snapshot.capacity());
            bytes.extend_from_slice(&snapshot.bytes);
            *snapshot = Arc::new(Self {
                bytes,
                index: snapshot.index.clone(),
                info: snapshot.info,
                complete_only: snapshot.complete_only,
                lease,
            });
        }
        Ok(Arc::get_mut(snapshot).expect("unique leased snapshot"))
    }
    pub(super) fn set_regex(&mut self, regex: Arc<super::regex_text::RegexParser>) {
        self.index = LogIndex::build_format(
            &self.bytes[..indexed_end(&self.bytes, self.complete_only)],
            Format::Regex,
            Some(regex),
            1,
        );
        self.exclude_oversized();
    }
    fn rebuild(&mut self) {
        self.index = LogIndex::build_format(
            &self.bytes[..indexed_end(&self.bytes, self.complete_only)],
            self.index.format(),
            self.index.regex().cloned(),
            1,
        );
    }
    fn discard(&mut self, mut count: usize) {
        if self.index.format() != Format::Plain && self.index.regex().is_none() {
            while count < self.bytes.len()
                && !(record::is_record(block::head(&self.bytes[count..]), self.index.format())
                    || block::block_kind(&self.bytes[count..]).is_some())
            {
                let Some(at) = memchr::memchr(b'\n', &self.bytes[count..]) else {
                    // A possible next head can still be incomplete. Keep it
                    // pending, never indexed, until a later poll proves it.
                    if !self.complete_only {
                        count = self.bytes.len();
                    }
                    break;
                };
                count += at + 1;
            }
        }
        let bytes = &mut self.bytes;
        bytes.copy_within(count.., 0);
        bytes.truncate(bytes.len() - count);
        self.info.offset += count as u64;
        self.info.tail = true;
        self.rebuild();
    }
    fn exclude_oversized(&mut self) {
        let keep = self
            .index
            .entries()
            .iter()
            .rev()
            .find(|e| {
                e.len() > WINDOW_LINE_BYTES
                    || !record::within_decode_budget(
                        &self.bytes[e.range()],
                        self.index.format(),
                        self.index.regex().map(AsRef::as_ref),
                    )
            })
            .map(|e| {
                let end = e.range().end;
                end + usize::from(self.bytes.get(end) == Some(&b'\n'))
                    + usize::from(self.bytes.get(end..end + 2) == Some(b"\r\n")) * 2
            });
        if let Some(keep) = keep {
            self.info.oversized = true;
            self.discard(keep.min(self.bytes.len()));
        }
    }
    /// Appends a bounded IO chunk; a sliding prefix is discarded in place.
    /// Returns whether earlier entry identities changed.
    pub(super) fn append(
        &mut self,
        incoming: &[u8],
        end: u64,
        file_len: u64,
    ) -> Result<(bool, bool, bool)> {
        ensure!(
            incoming.len() <= super::tail::POLL_BYTES,
            "Oversized log poll"
        );
        let planned = (self.bytes.len() + incoming.len()).min(WINDOW_BYTES);
        let capacity = self
            .capacity()
            .max(planned.next_power_of_two().min(WINDOW_BYTES));
        let lines = physical_lines(&self.bytes) + physical_lines(incoming);
        // Vec reallocation may hold old and new buffers simultaneously.
        // Reserve that transient overlap before touching bytes or cursor.
        let final_charge = charge(capacity, lines);
        let overlap = if capacity > self.capacity() {
            self.capacity()
        } else {
            0
        };
        ensure!(
            self.lease.resize(final_charge + overlap),
            super::window_budget::BudgetBusy
        );
        let old_offset = self.info.offset;
        if self.bytes.len() + incoming.len() > WINDOW_BYTES {
            let minimum = (self.bytes.len() + incoming.len() - WINDOW_BYTES + WINDOW_BYTES / 8)
                .min(self.bytes.len());
            let mut keep = minimum;
            if keep > 0 && self.bytes.get(keep - 1) != Some(&b'\n') {
                keep = memchr::memchr(b'\n', &self.bytes[keep..])
                    .map_or(self.bytes.len(), |at| keep + at + 1);
            }
            // Use existing entry boundaries rather than retaining a continuation.
            keep = self
                .index
                .entries()
                .iter()
                .find(|e| e.offset() as usize >= keep)
                .map_or(self.bytes.len(), |e| e.offset() as usize);
            self.discard(keep);
        }
        let bytes = &mut self.bytes;
        if capacity > bytes.capacity() {
            bytes.reserve_exact(capacity - bytes.len());
        }
        bytes.extend_from_slice(incoming);
        // The old allocation has now been released. This only shrinks.
        assert!(self.lease.resize(final_charge));
        self.info.end = end;
        self.info.file_len = file_len;
        let mut w = ReadWindow {
            bytes: std::mem::take(bytes),
            offset: self.info.offset,
            end,
            file_len,
            oversized: self.info.oversized,
        };
        window::bound_lines(&mut w, ReadBudget::default());
        if w.offset != self.info.offset || (self.info.tail && self.index.is_empty()) {
            // Empty structured tail = still awaiting a proven record head,
            // including across polls after excluding a complete long block.
            align(
                &mut w,
                self.index.format(),
                self.index.regex().is_some(),
                self.complete_only,
            );
        }
        self.info.offset = w.offset;
        self.info.oversized |= w.oversized;
        self.info.tail |= w.offset > 0 || w.oversized;
        self.bytes = w.bytes;
        let slid = self.info.offset != old_offset;
        let end = indexed_end(&self.bytes, self.complete_only);
        let format = if self.index.regex().is_none()
            && !self.info.tail
            && self.index.bytes() < detect::SAMPLE_BYTES as u64
            && self.index.len() < detect::SAMPLE_LINES
        {
            detect::detect(&self.bytes[..end])
        } else {
            self.index.format()
        };
        let reindexed = format != self.index.format();
        let grown = if slid || reindexed {
            self.index =
                LogIndex::build_format(&self.bytes[..end], format, self.index.regex().cloned(), 1);
            false
        } else {
            let end = indexed_end(&self.bytes, self.complete_only);
            self.index.extend(&self.bytes, end)
        };
        self.exclude_oversized();
        Ok((self.info.offset != old_offset, grown, reindexed))
    }
    pub(super) fn finish(&mut self) {
        let awaiting_head = self.info.tail && self.index.is_empty();
        self.complete_only = false;
        if awaiting_head {
            self.discard(0);
        }
        self.info.tail |= self.info.end < self.info.file_len;
        self.rebuild();
        self.exclude_oversized();
    }
    pub(super) fn prepare_skip_append(&mut self, len: usize) -> Result<()> {
        ensure!(
            self.lease.resize(charge(
                self.capacity().max(len.min(WINDOW_BYTES)),
                WINDOW_LINES
            )),
            super::window_budget::BudgetBusy
        );
        Ok(())
    }
    pub(super) fn skip_to(&mut self, end: u64, file_len: u64) {
        self.info.offset = end;
        self.info.end = end;
        self.info.file_len = file_len;
        self.info.tail = true;
    }
}
fn physical_lines(bytes: &[u8]) -> usize {
    memchr::memchr_iter(b'\n', bytes).count()
        + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
}
fn indexed_end(bytes: &[u8], complete: bool) -> usize {
    if complete {
        memchr::memrchr(b'\n', bytes).map_or(0, |at| at + 1)
    } else {
        bytes.len()
    }
}
fn align(w: &mut ReadWindow, format: Format, regex: bool, complete: bool) {
    if regex || format == Format::Plain {
        return;
    }
    w.align_record(|raw| {
        (complete && !raw.contains(&b'\n'))
            || record::is_record(block::head(raw), format)
            || block::block_kind(raw).is_some()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_snapshots_enforce_the_pool_before_copying() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "log::snapshot::tests::pool_negative_control_child",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout)
            .contains("positive: leased COW succeeded after refusal"));
    }
    #[test]
    #[ignore = "isolated process so pool pressure cannot interfere with unrelated tests"]
    fn pool_negative_control_child() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.logfmt");
        std::fs::write(&path, b"level=info msg=before\n").unwrap();
        let file = super::super::LogFile::open(&path).unwrap();
        let held = file.snapshot();
        let mut mutable = file.snapshot();
        drop(file);
        let used = pool().used();
        assert!(used > 0);
        let blocked = pool().reserve(pool().limit() - used).unwrap();
        assert!(
            super::super::LogFile::open(&path).is_err(),
            "negative: refuse before raw IO allocation"
        );
        assert!(
            LogSnapshot::mutable(&mut mutable).is_err(),
            "negative: cannot clone past leased budget"
        );
        assert!(Arc::ptr_eq(&held, &mutable));
        assert_eq!(pool().used(), pool().limit());
        drop(blocked);
        LogSnapshot::mutable(&mut mutable)
            .unwrap()
            .append(b"level=warn msg=after\n", 42, 42)
            .unwrap();
        assert!(pool().used() > used);
        assert_eq!(held.index().len(), 1);
        assert_eq!(mutable.index().len(), 2);
        drop(mutable);
        assert_eq!(pool().used(), used);
        drop(held);
        assert_eq!(pool().used(), 0);
        println!("positive: leased COW succeeded after refusal");

        // Sparse indexes do not hide the old+new raw Vec overlap. Leave
        // exactly enough for final capacity: the pre-fix allocation fit its
        // accounting but could overrun the pool while realloc held both.
        use std::io::Write as _;
        let path = dir.path().join("sparse.log");
        let mut writer = File::create(&path).unwrap();
        let mut line = vec![b'x'; 32 * 1024];
        *line.last_mut().unwrap() = b'\n';
        for _ in 0..1024 {
            writer.write_all(&line).unwrap();
        }
        drop(writer);
        let file = super::super::LogFile::open(&path).unwrap();
        let mut snapshot = file.snapshot();
        drop(file);
        let incoming = b"positive-new-row\n";
        let before = snapshot.window();
        let lines = physical_lines(snapshot.bytes()) + physical_lines(incoming);
        let final_charge = charge(WINDOW_BYTES, lines);
        let free = final_charge - snapshot.reserved_bytes();
        let pressure = pool()
            .reserve(pool().limit() - pool().used() - free)
            .unwrap();
        assert!(
            LogSnapshot::mutable(&mut snapshot)
                .unwrap()
                .append(
                    incoming,
                    before.end + incoming.len() as u64,
                    before.end + incoming.len() as u64
                )
                .is_err(),
            "negative: final capacity fits but transient raw overlap does not"
        );
        assert_eq!(snapshot.window(), before);
        assert_eq!(snapshot.capacity(), 32 * 1024 * 1024);
        drop(pressure);
        LogSnapshot::mutable(&mut snapshot)
            .unwrap()
            .append(
                incoming,
                before.end + incoming.len() as u64,
                before.end + incoming.len() as u64,
            )
            .unwrap();
        assert_eq!(snapshot.capacity(), WINDOW_BYTES);
        assert_eq!(
            snapshot.raw(snapshot.index().len() - 1).unwrap(),
            b"positive-new-row"
        );
        drop(snapshot);
        assert_eq!(pool().used(), 0);
        println!("positive: raw growth succeeded after overlap refusal");
    }
}
