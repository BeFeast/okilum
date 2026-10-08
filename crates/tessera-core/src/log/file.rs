//! A log file opened read-only, its bytes, and its index.

use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};

use super::{LogIndex, Record};

/// Files up to this size use a vector; larger ones use an anonymous mapped
/// snapshot. Neither storage borrows the mutable canonical file's pages.
pub const MAP_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
/// Upper bound for one in-memory index. Larger files are refused with a
/// message, never truncated silently.
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024 * 1024;

enum Bytes {
    Owned(Vec<u8>),
    Mapped { map: memmap2::Mmap, len: usize },
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Owned(bytes) => bytes,
            Bytes::Mapped { map, len } => &map[..*len],
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
        Self::from_file(path, &file, map_threshold)
    }

    /// Snapshots and indexes `file` from its current position; `path` is
    /// what the viewer shows. A decompressed spool enters here too, under
    /// the compressed source's path.
    pub(super) fn from_file(path: &Path, file: &std::fs::File, map_threshold: u64) -> Result<Self> {
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
            use std::io::Read;
            // A canonical log can be truncated or overwritten at any time.
            // Copy into anonymous pages before publishing a read-only map:
            // file-backed maps can fault after truncation and expose changed
            // bytes that no longer match their already-published index.
            let mut map = memmap2::MmapOptions::new()
                .len(len as usize)
                .map_anon()
                .context("The log snapshot cannot be allocated")?;
            let mut source = file.take(len);
            let mut copied = 0;
            while copied < map.len() {
                match source.read(&mut map[copied..]) {
                    Ok(0) => break,
                    Ok(n) => copied += n,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error).context("The log file cannot be read"),
                }
            }
            let map = map
                .make_read_only()
                .context("The log snapshot cannot be protected")?;
            Bytes::Mapped { map, len: copied }
        } else {
            use std::io::Read;
            let mut bytes = Vec::with_capacity(len as usize);
            file.take(len)
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
        matches!(self.bytes, Bytes::Mapped { .. })
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
    #[test]
    fn mapped_snapshot_survives_canonical_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mutable.logfmt");
        std::fs::write(
            &path,
            b"level=info message=first\nlevel=error message=second\n",
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "log::file::tests::mapped_snapshot_truncation_child",
                "--ignored",
                "--nocapture",
            ])
            .env("TESSERA_LOG_TRUNCATION_FIXTURE", &path)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("mapped snapshot opened"),
            "child never opened the snapshot: {stdout}"
        );
        assert!(
            output.status.success(),
            "truncation child failed: {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("snapshot remained readable after canonical truncation"));
    }

    #[test]
    #[ignore = "invoked in an isolated process by mapped_snapshot_survives_canonical_truncation"]
    fn mapped_snapshot_truncation_child() {
        let path = PathBuf::from(std::env::var_os("TESSERA_LOG_TRUNCATION_FIXTURE").unwrap());
        let snapshot = LogFile::open_with(&path, 0).unwrap();
        assert!(
            snapshot.is_mapped(),
            "positive control: exercise the large-file storage path"
        );
        println!("mapped snapshot opened");
        std::fs::write(&path, []).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(snapshot.raw(0).unwrap(), b"level=info message=first");
        assert_eq!(snapshot.raw(1).unwrap(), b"level=error message=second");
        println!("snapshot remained readable after canonical truncation");
    }
}
