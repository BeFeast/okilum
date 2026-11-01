//! Bounded session-only raw log streams. Transport chunks are not records.
use super::{snapshot, window_budget::Reservation, LogIndex, WindowInfo};
use anyhow::{ensure, Context as _, Result};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_LINES: usize = 20_000;
pub const MAX_LINE_BYTES: usize = 64 * 1024;
pub const MAX_CHUNK_BYTES: usize = 1024 * 1024;

/// A leased immutable snapshot consumed by the same row/filter/export view as
/// files. Scope identifies one session; offsets remain stable within a session.
pub struct Snapshot {
    bytes: Vec<u8>,
    index: LogIndex,
    path: PathBuf,
    scope: String,
    _lease: Reservation,
}
impl Snapshot {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn index(&self) -> &LogIndex {
        &self.index
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn window(&self) -> WindowInfo {
        WindowInfo {
            end: self.bytes.len() as u64,
            file_len: self.bytes.len() as u64,
            ..Default::default()
        }
    }
}

pub struct Assembler {
    complete: Vec<u8>,
    pending: Vec<u8>,
    lines: usize,
    path: PathBuf,
    scope: String,
    _lease: Reservation,
}
impl Assembler {
    /// Pool exhaustion is explicit; no unleased fallback allocation is made.
    pub fn new(path: PathBuf, scope: String) -> Result<Self> {
        let lease = snapshot::reserve(MAX_BYTES + 3 * MAX_CHUNK_BYTES + 4 * MAX_LINE_BYTES)?;
        Ok(Self {
            complete: Vec::new(),
            pending: Vec::new(),
            lines: 0,
            path,
            scope,
            _lease: lease,
        })
    }
    pub fn pending_bytes(&self) -> usize {
        self.pending.len()
    }
    /// Atomic with respect to bounds/UTF-8 errors. Existing rows and fragments
    /// are unchanged on rejection. Partial UTF-8 stays pending until newline.
    pub fn append(&mut self, bytes: &[u8]) -> Result<Option<Arc<Snapshot>>> {
        ensure!(
            bytes.len() <= MAX_CHUNK_BYTES,
            "Transport chunk exceeds 1 MiB"
        );
        let mut candidate = self.pending.clone();
        candidate.extend_from_slice(bytes);
        let end = candidate
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |at| at + 1);
        let (complete, pending) = candidate.split_at(end);
        ensure!(
            pending.len() <= MAX_LINE_BYTES
                && complete
                    .split(|&b| b == b'\n')
                    .all(|line| line.len() <= MAX_LINE_BYTES),
            "Line exceeds 64 KiB; previous rows retained"
        );
        let lines = complete.iter().filter(|&&b| b == b'\n').count();
        ensure!(
            self.complete.len() + candidate.len() <= MAX_BYTES && self.lines + lines <= MAX_LINES,
            "Loaded history limit reached (8 MiB / 20,000 lines); previous rows retained"
        );
        std::str::from_utf8(complete).context("Log is not valid UTF-8; previous rows retained")?;
        if complete.is_empty() {
            self.pending = pending.to_vec();
            return Ok(None);
        }
        let lease = snapshot::reserve(snapshot::charge(
            self.complete.len() + complete.len(),
            self.lines + lines,
        ))?;
        let mut data = Vec::with_capacity(self.complete.len() + complete.len());
        data.extend_from_slice(&self.complete);
        data.extend_from_slice(complete);
        let index = LogIndex::build_with_threads(&data, 1);
        self.complete.extend_from_slice(complete);
        self.pending = pending.to_vec();
        self.lines += lines;
        Ok(Some(Arc::new(Snapshot {
            bytes: data,
            index,
            path: self.path.clone(),
            scope: self.scope.clone(),
            _lease: lease,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn open() -> Assembler {
        Assembler::new("session.log".into(), "session-1".into()).unwrap()
    }
    #[test]
    fn chunks_preserve_crlf_utf8_and_complete_rows_exactly() {
        let mut a = open();
        assert!(a.append(b"first\r").unwrap().is_none());
        let first = a.append(b"\n\xe9").unwrap().unwrap();
        assert_eq!(first.bytes(), b"first\r\n");
        assert_eq!(a.pending_bytes(), 1);
        assert!(a.append(b"\x9b").unwrap().is_none());
        let second = a.append(b"\xaa\nthird").unwrap().unwrap();
        assert_eq!(second.bytes(), "first\r\n雪\n".as_bytes());
        assert_eq!(second.index().len(), 2);
        assert_eq!(first.bytes(), b"first\r\n", "published snapshot immutable");
        assert_eq!(second.scope(), "session-1");
    }
    #[test]
    fn invalid_input_is_atomic_and_has_a_healthy_successor() {
        let mut a = open();
        a.append(b"retained\npartial").unwrap();
        assert!(a.append(b"\xff\n").is_err());
        assert_eq!(a.pending_bytes(), 7);
        assert_eq!(
            a.append(b" done\n").unwrap().unwrap().bytes(),
            b"retained\npartial done\n"
        );
    }
    #[test]
    fn line_and_history_bounds_reject_without_evicting_or_allocating_more() {
        let mut a = open();
        a.append(b"kept\n").unwrap();
        assert!(a.append(&vec![b'x'; MAX_LINE_BYTES + 1]).is_err());
        let mut full = open();
        let rows = b"x\n".repeat(MAX_LINES);
        assert_eq!(
            full.append(&rows).unwrap().unwrap().index().len(),
            MAX_LINES
        );
        assert!(full.append(b"extra\n").is_err());
        assert!(full.append(&vec![b' '; MAX_CHUNK_BYTES + 1]).is_err());
        assert_eq!(a.append(b"ok\n").unwrap().unwrap().bytes(), b"kept\nok\n");
    }
}
