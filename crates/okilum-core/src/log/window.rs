//! Bounded positional reads for the large-log window (#1389).
//!
//! This module deliberately knows nothing about parsers or indexes. The
//! caller aligns a structured record head after detecting its format. No
//! operation searches backwards outside the byte budget for a long record.

use std::fs::File;
use std::io;

pub const WINDOW_BYTES: usize = 64 * 1024 * 1024;
pub const WINDOW_LINES: usize = 262_144;
pub const WINDOW_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ReadBudget {
    pub bytes: usize,
    pub lines: usize,
    pub line_bytes: usize,
}

impl Default for ReadBudget {
    fn default() -> Self {
        Self {
            bytes: WINDOW_BYTES,
            lines: WINDOW_LINES,
            line_bytes: WINDOW_LINE_BYTES,
        }
    }
}

/// Small interface so tests can reject a full-file scan before it happens.
pub trait ReadAt {
    fn read_at(&self, output: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl ReadAt for File {
    fn read_at(&self, output: &mut [u8], offset: u64) -> io::Result<usize> {
        read_file_at(self, output, offset)
    }
}

#[cfg(unix)]
fn read_file_at(file: &File, output: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, output, offset)
}

#[cfg(windows)]
fn read_file_at(file: &File, output: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, output, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_file_at(mut file: &File, output: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    file.read(output)
}

#[derive(Debug)]
pub struct ReadWindow {
    pub bytes: Vec<u8>,
    /// Absolute file offset of bytes[0], not a line number guessed by scanning.
    pub offset: u64,
    /// Absolute offset just after the bytes that were actually read.
    pub end: u64,
    /// Metadata size captured by the caller before the read.
    pub file_len: u64,
    /// At least one record-sized line did not fit and was excluded whole.
    pub oversized: bool,
}

impl ReadWindow {
    /// Discards a prefix in place; no second raw-data allocation.
    pub fn discard_before(&mut self, count: usize) {
        assert!(count <= self.bytes.len());
        self.bytes.copy_within(count.., 0);
        self.bytes.truncate(self.bytes.len() - count);
        self.offset += count as u64;
    }

    /// Discards complete leading lines until the parser can prove a record
    /// starts here. A continuation from an unloaded record is never presented
    /// as that record. Plain text callers need no additional alignment.
    pub fn align_record(&mut self, mut starts_record: impl FnMut(&[u8]) -> bool) {
        let mut start = 0;
        while start < self.bytes.len() {
            if starts_record(&self.bytes[start..]) {
                break;
            }
            start = match memchr::memchr(b'\n', &self.bytes[start..]) {
                Some(at) => start + at + 1,
                None => self.bytes.len(),
            };
        }
        self.discard_before(start);
    }

    /// The read raced a truncation and did not reach the captured endpoint.
    /// The caller may retry with fresh metadata; it must not read the gap.
    pub fn shortened(&self) -> bool {
        self.end < self.file_len
    }
}

/// Reads only the suffix budget plus one preceding boundary byte. An append
/// racing this read is left for the next poll. A shrink returns a short read.
pub fn read_tail(
    source: &impl ReadAt,
    file_len: u64,
    budget: ReadBudget,
) -> io::Result<ReadWindow> {
    if budget.bytes == 0
        || budget.bytes > WINDOW_BYTES
        || budget.lines == 0
        || budget.lines > WINDOW_LINES
        || budget.line_bytes == 0
        || budget.line_bytes > WINDOW_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid log read budget",
        ));
    }
    let start = file_len.saturating_sub(budget.bytes as u64);
    let mut boundary = *b"\n";
    if start > 0 {
        read_into(source, &mut boundary, start - 1)?;
    }
    let mut bytes = vec![0; (file_len - start) as usize];
    let read = read_into(source, &mut bytes, start)?;
    bytes.truncate(read);
    let mut window = ReadWindow {
        bytes,
        offset: start,
        end: start + read as u64,
        file_len,
        oversized: false,
    };
    if start > 0 && boundary[0] != b'\n' {
        // This could also be a partial UTF-8 codepoint. Never decode it or
        // allocate an expanding buffer while looking for the missing head.
        let keep = memchr::memchr(b'\n', &window.bytes).map_or(window.bytes.len(), |at| at + 1);
        if keep == window.bytes.len() && keep >= budget.line_bytes {
            window.oversized = true;
        }
        window.discard_before(keep);
    }
    bound_lines(&mut window, budget);
    Ok(window)
}

fn read_into(source: &impl ReadAt, bytes: &mut [u8], start: u64) -> io::Result<usize> {
    let mut filled = 0;
    while filled < bytes.len() {
        match source.read_at(&mut bytes[filled..], start + filled as u64) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn bound_lines(window: &mut ReadWindow, budget: ReadBudget) {
    let mut lines = usize::from(!window.bytes.is_empty() && !window.bytes.ends_with(b"\n"));
    let mut keep = 0;
    for at in memchr::memrchr_iter(b'\n', &window.bytes) {
        lines += 1;
        if lines > budget.lines {
            keep = at + 1;
            break;
        }
    }
    window.discard_before(keep);

    let mut start = 0;
    let mut keep = 0;
    while start < window.bytes.len() {
        let newline = memchr::memchr(b'\n', &window.bytes[start..]);
        let end = newline.map_or(window.bytes.len(), |at| start + at);
        let next = end + usize::from(newline.is_some());
        if end - start > budget.line_bytes {
            keep = next;
            window.oversized = true;
        }
        start = next;
    }
    window.discard_before(keep);
}
