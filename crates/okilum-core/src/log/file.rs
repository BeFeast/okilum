//! Read-only log files backed by leased, bounded immutable snapshots.
use super::{LogIndex, LogSnapshot, Record, WindowInfo};
use anyhow::{ensure, Context, Result};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
/// Legacy threshold retained for callers; snapshots now use bounded reads.
pub const MAP_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
/// Legacy public constant; large uncompressed files now use bounded tails.
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub struct LogFile {
    path: PathBuf,
    snapshot: Arc<LogSnapshot>,
}
impl LogFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = super::tail::open_shared(path).context("The log file cannot be opened")?;
        let snapshot = if let Some(ext) = super::compression(path) {
            let lease = super::snapshot::reserve(super::window::WINDOW_BYTES + 40 * 1024 * 1024)?;
            LogSnapshot::decoded(decompress(file, ext)?, lease)?
        } else {
            LogSnapshot::read(&file, false, None)?
        };
        Ok(Self {
            path: path.to_owned(),
            snapshot,
        })
    }
    pub fn with_regex(mut self, regex: Arc<super::regex_text::RegexParser>) -> Result<Self> {
        LogSnapshot::mutable(&mut self.snapshot)?.set_regex(regex);
        Ok(self)
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
    pub fn is_compressed(&self) -> bool {
        super::compression(&self.path).is_some()
    }
    /// Bounded owned snapshots remain valid across canonical truncation.
    pub fn is_mapped(&self) -> bool {
        false
    }
    pub fn raw(&self, index: usize) -> Option<&[u8]> {
        self.snapshot.raw(index)
    }
    pub fn record(&self, index: usize) -> Option<Record> {
        self.snapshot.record(index)
    }
}

/// Compressed streams cannot seek to a suffix without scanning their prefix.
/// Keep ordinary small streams, refuse an expansion beyond the memory budget.
fn decompress(file: std::fs::File, ext: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = std::io::BufReader::new(file);
    let mut reader: Box<dyn Read> = match ext.to_ascii_lowercase().as_str() {
        "gz" => Box::new(flate2::read::MultiGzDecoder::new(file)),
        "zst" => {
            let mut decoder = zstd::stream::read::Decoder::with_buffer(file)
                .context("The compressed log cannot be read")?;
            decoder
                .window_log_max(23)
                .context("The compressed log window exceeds its memory budget")?;
            Box::new(decoder)
        }
        "bz2" => Box::new(bzip2::read::MultiBzDecoder::new(file)),
        other => anyhow::bail!("`.{other}` logs cannot be decoded"),
    };
    let mut bytes = Vec::with_capacity(super::window::WINDOW_BYTES);
    let mut chunk = [0; 64 * 1024];
    loop {
        let n = reader
            .read(&mut chunk)
            .context("The compressed log cannot be decoded")?;
        if n == 0 {
            break;
        }
        ensure!(bytes.len() + n <= super::window::WINDOW_BYTES,
            "The compressed log exceeds the 64 MiB decoded budget; open an uncompressed copy for bounded tail viewing");
        bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Level;

    #[test]
    fn owned_snapshot_indexes_exact_bytes() {
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
        let reopened = LogFile::open(&path).unwrap();
        assert!(!read.is_mapped());
        assert_eq!(read.index(), reopened.index());
        assert_eq!(reopened.index().stats().count(Level::Error), 200);
        assert_eq!(reopened.raw(2_000).unwrap(), b"tail without newline");
        assert_eq!(reopened.record(1).unwrap().fields[1].value, "m1");
        assert!(reopened.record(2_000).is_none());
        // Opening never writes: the file is byte-identical afterwards.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn empty_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.log");
        std::fs::write(&path, "").unwrap();
        assert!(LogFile::open(&path).unwrap().index().is_empty());
        assert!(LogFile::open(&dir.path().join("absent.log")).is_err());
    }
    #[test]
    fn owned_snapshot_survives_canonical_truncation() {
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
                "log::file::tests::owned_snapshot_truncation_child",
                "--ignored",
                "--nocapture",
            ])
            .env("OKILUM_LOG_TRUNCATION_FIXTURE", &path)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("owned snapshot opened"),
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
    #[ignore = "invoked in an isolated process by owned_snapshot_survives_canonical_truncation"]
    fn owned_snapshot_truncation_child() {
        let path = PathBuf::from(std::env::var_os("OKILUM_LOG_TRUNCATION_FIXTURE").unwrap());
        let snapshot = LogFile::open(&path).unwrap();
        assert!(
            !snapshot.is_mapped(),
            "positive control: canonical bytes are copied into a bounded snapshot"
        );
        println!("owned snapshot opened");
        std::fs::write(&path, []).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(snapshot.raw(0).unwrap(), b"level=info message=first");
        assert_eq!(snapshot.raw(1).unwrap(), b"level=error message=second");
        println!("snapshot remained readable after canonical truncation");
    }
}
