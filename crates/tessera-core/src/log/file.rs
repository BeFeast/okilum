//! A log file opened read-only, its bytes, and its index.

use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};

use super::{LogIndex, Record};

/// Files up to this size are read into memory; larger ones are mapped.
/// Reading keeps the common small log (Tessera's own is bounded at 4 MiB)
/// immune to a writer truncating it while it is shown.
pub const MAP_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
/// Upper bound for one in-memory index. Larger files are refused with a
/// message, never truncated silently.
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024 * 1024;

enum Bytes {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Owned(bytes) => bytes,
            Bytes::Mapped(map) => map,
        }
    }
}

pub struct LogFile {
    path: PathBuf,
    bytes: Bytes,
    index: LogIndex,
}

impl LogFile {
    /// Opens and indexes `path`. Nothing is written anywhere.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, MAP_THRESHOLD_BYTES)
    }

    fn open_with(path: &Path, map_threshold: u64) -> Result<Self> {
        let file = std::fs::File::open(path).context("The log file cannot be opened")?;
        let len = file
            .metadata()
            .context("The log file cannot be inspected")?
            .len();
        ensure!(
            len <= MAX_LOG_BYTES,
            "The log file is larger than the viewer's {} GiB limit",
            MAX_LOG_BYTES >> 30
        );
        let bytes = if len > map_threshold {
            // SAFETY: the map is read-only and covers the length measured
            // above, so later appends are simply not visible. A writer that
            // truncates the file below that length while it is mapped would
            // fault on access; files that small are read instead, and follow
            // mode (a later slice) must not map a file it follows.
            let map = unsafe {
                memmap2::MmapOptions::new()
                    .len(len as usize)
                    .map(&file)
                    .context("The log file cannot be mapped")?
            };
            Bytes::Mapped(map)
        } else {
            use std::io::Read;
            let mut bytes = Vec::with_capacity(len as usize);
            (&file)
                .take(len)
                .read_to_end(&mut bytes)
                .context("The log file cannot be read")?;
            Bytes::Owned(bytes)
        };
        let index = LogIndex::build(&bytes);
        Ok(Self {
            path: path.to_owned(),
            bytes,
            index,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn index(&self) -> &LogIndex {
        &self.index
    }
    pub fn is_mapped(&self) -> bool {
        matches!(self.bytes, Bytes::Mapped(_))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Level;

    #[test]
    fn mapped_and_read_files_index_identically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.jsonl");
        let mut text = String::new();
        for i in 0..2_000 {
            text.push_str(&format!(
                "{{\"level\":\"{}\",\"msg\":\"m{i}\"}}\n",
                if i % 10 == 0 { "error" } else { "info" }
            ));
        }
        text.push_str("tail without newline");
        std::fs::write(&path, &text).unwrap();
        let read = LogFile::open(&path).unwrap();
        let mapped = LogFile::open_with(&path, 0).unwrap();
        assert!(!read.is_mapped() && mapped.is_mapped());
        assert_eq!(read.index(), mapped.index());
        assert_eq!(mapped.index().stats().count(Level::Error), 200);
        assert_eq!(mapped.raw(2_000).unwrap(), b"tail without newline");
        assert_eq!(mapped.record(1).unwrap().fields[1].value, "m1");
        assert!(mapped.record(2_000).is_none());
        // Opening never writes: the file is byte-identical afterwards.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn empty_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.log");
        std::fs::write(&path, "").unwrap();
        assert!(LogFile::open_with(&path, 0).unwrap().index().is_empty());
        assert!(LogFile::open(&dir.path().join("absent.log")).is_err());
    }
}
