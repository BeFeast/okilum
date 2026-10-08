//! Compressed log input (#602, slice 7).
//!
//! A compressed stream cannot be addressed by byte offset, so it is
//! decompressed once into a temporary spool file and then snapshotted and
//! indexed exactly like a plain log. The format comes from the stream's magic
//! bytes; the file name only decides whether the viewer offers to open it.
//!
//! The spool lives in the system temp directory (never next to the source,
//! never in a vault or the index directory) and is deleted when it is dropped,
//! including on every error path. Output above the size cap is refused with
//! a typed error; a partially decompressed stream is never indexed.
//!
//! Corruption is detected as far as the format allows: gzip and bzip2 carry
//! CRCs, zstd only when the frame has a content checksum (the `zstd` CLI
//! writes one by default). A checksum-less zstd frame whose payload is
//! damaged without breaking its structure decodes to the damaged bytes.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use super::{is_log_path, LogFile, MAP_THRESHOLD_BYTES, MAX_LOG_BYTES};

/// Compression suffixes the viewer offers to open, after a log name
/// (`app.log.gz`, `app.jsonl.zst`, `app.log.1.bz2`). xz is not offered: see
/// open question 3 in docs/research/602-hl-log-viewer.md.
pub const COMPRESSED_EXTENSIONS: [&str; 4] = ["gz", "zst", "zstd", "bz2"];

/// Default limit for decompressed bytes: the plain-file limit, because the
/// spool is indexed by the same path.
pub const DEFAULT_SPOOL_CAP: u64 = MAX_LOG_BYTES;

/// Whether `path` names a compressed log by extension: a log name followed
/// by an optional numeric rotation suffix and a compression suffix. A bare
/// `archive.gz` is not claimed. Opening still decides by magic bytes.
pub fn is_compressed_log_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if !COMPRESSED_EXTENSIONS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
    {
        return false;
    }
    let stem = match stem.rsplit_once('.') {
        Some((inner, rotation))
            if !rotation.is_empty() && rotation.bytes().all(|b| b.is_ascii_digit()) =>
        {
            inner
        }
        _ => stem,
    };
    is_log_path(Path::new(stem))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    Gzip,
    Zstd,
    Bzip2,
    /// Recognised so it can be refused by name; no decoder is linked.
    Xz,
}

impl Compression {
    /// Longest magic sequence `sniff` looks at.
    pub const MAGIC_LEN: usize = 6;

    /// The compression of a stream starting with `head`, from magic bytes.
    pub fn sniff(head: &[u8]) -> Option<Self> {
        match head {
            // ID1 ID2 and CM = deflate, the only method gzip defines.
            [0x1f, 0x8b, 0x08, ..] => Some(Self::Gzip),
            // A zstd frame, or a skippable frame (pzstd writes one first).
            [0x28, 0xb5, 0x2f, 0xfd, ..] => Some(Self::Zstd),
            [0x50..=0x5f, 0x2a, 0x4d, 0x18, ..] => Some(Self::Zstd),
            // "BZh" plus the block size digit.
            [b'B', b'Z', b'h', b'1'..=b'9', ..] => Some(Self::Bzip2),
            [0xfd, b'7', b'z', b'X', b'Z', 0x00, ..] => Some(Self::Xz),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
            Self::Bzip2 => "bzip2",
            Self::Xz => "xz",
        }
    }

    pub fn is_supported(self) -> bool {
        !matches!(self, Self::Xz)
    }
}

/// Why a compressed log could not be opened.
#[derive(Debug)]
pub enum CompressedError {
    /// The source cannot be opened or read.
    Source(io::Error),
    /// The source has no magic bytes of a known compression format.
    NotCompressed,
    /// The format is recognised but no decoder is linked.
    Unsupported(Compression),
    /// The stream ends before its format says it does.
    Truncated(Compression),
    /// The stream is not valid for its format.
    Corrupt {
        compression: Compression,
        source: io::Error,
    },
    /// Decompressed output exceeds `cap` bytes.
    TooLarge { compression: Compression, cap: u64 },
    /// The temporary spool cannot be created or written.
    Spool(io::Error),
    /// The decompressed log cannot be snapshotted or indexed.
    Index(anyhow::Error),
}

impl fmt::Display for CompressedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(_) => f.write_str("The log file cannot be read"),
            Self::NotCompressed => f.write_str("The log file is not compressed"),
            Self::Unsupported(c) => {
                write!(f, "{} compressed logs cannot be opened yet", c.label())
            }
            Self::Truncated(c) => write!(f, "The {} stream is incomplete", c.label()),
            Self::Corrupt { compression, .. } => {
                write!(f, "The {} stream is damaged", compression.label())
            }
            Self::TooLarge { cap, .. } => write!(
                f,
                "The uncompressed log is larger than the viewer's {} limit",
                size_label(*cap)
            ),
            Self::Spool(_) => f.write_str("The log cannot be uncompressed to temporary storage"),
            Self::Index(error) => write!(f, "{error:#}"),
        }
    }
}

fn size_label(bytes: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")];
    UNITS
        .iter()
        .find(|(unit, _)| bytes >= *unit)
        .map(|(unit, name)| format!("{} {name}", bytes / unit))
        .unwrap_or_else(|| format!("{bytes} bytes"))
}

impl std::error::Error for CompressedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) | Self::Spool(error) => Some(error),
            Self::Corrupt { source, .. } => Some(source),
            Self::Index(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SpoolOptions {
    /// Maximum decompressed bytes. Values above `MAX_LOG_BYTES` are clamped.
    pub cap: u64,
    /// Spool directory; `None` is the system temp directory.
    pub dir: Option<PathBuf>,
}

impl Default for SpoolOptions {
    fn default() -> Self {
        Self {
            cap: DEFAULT_SPOOL_CAP,
            dir: None,
        }
    }
}

/// A fully decompressed copy of a compressed log, deleted on drop.
pub struct Spool {
    file: tempfile::NamedTempFile,
    compression: Compression,
    len: u64,
}

impl Spool {
    /// Decompresses `path` into a new spool. On any error the partial spool
    /// is deleted before returning.
    pub fn decompress(path: &Path, options: &SpoolOptions) -> Result<Self, CompressedError> {
        let mut source = File::open(path).map_err(CompressedError::Source)?;
        let compression = sniff_reader(&mut source)
            .map_err(CompressedError::Source)?
            .ok_or(CompressedError::NotCompressed)?;
        let source = io::BufReader::new(source);
        let mut decoder: Box<dyn Read> = match compression {
            Compression::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(source)),
            Compression::Zstd => Box::new(
                zstd::stream::read::Decoder::with_buffer(source).map_err(|source| {
                    CompressedError::Corrupt {
                        compression,
                        source,
                    }
                })?,
            ),
            Compression::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(source)),
            Compression::Xz => return Err(CompressedError::Unsupported(compression)),
        };

        let dir = options.dir.clone().unwrap_or_else(std::env::temp_dir);
        let mut file = tempfile::Builder::new()
            .prefix(".tessera-log-spool-")
            .tempfile_in(dir)
            .map_err(CompressedError::Spool)?;
        let cap = options.cap.min(MAX_LOG_BYTES);
        let mut buffer = vec![0; 256 * 1024];
        let mut len = 0u64;
        loop {
            let n = match decoder.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(CompressedError::Truncated(compression))
                }
                Err(source) => {
                    return Err(CompressedError::Corrupt {
                        compression,
                        source,
                    })
                }
            };
            len += n as u64;
            if len > cap {
                return Err(CompressedError::TooLarge { compression, cap });
            }
            file.write_all(&buffer[..n])
                .map_err(CompressedError::Spool)?;
        }
        file.flush().map_err(CompressedError::Spool)?;
        Ok(Self {
            file,
            compression,
            len,
        })
    }

    pub fn compression(&self) -> Compression {
        self.compression
    }
    /// Decompressed bytes.
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Where the spool lives until it is dropped.
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Snapshots and indexes the spool as a log shown under `source`. The
    /// snapshot owns its bytes, so the spool is deleted when this returns.
    pub fn into_log(mut self, source: &Path) -> Result<LogFile, CompressedError> {
        self.file
            .as_file_mut()
            .rewind()
            .map_err(CompressedError::Spool)?;
        LogFile::from_file(source, self.file.as_file(), MAP_THRESHOLD_BYTES)
            .map_err(CompressedError::Index)
    }
}

/// The compression of the file at `path`, from its magic bytes.
pub fn sniff_path(path: &Path) -> io::Result<Option<Compression>> {
    sniff_reader(&mut File::open(path)?)
}

fn sniff_reader(file: &mut File) -> io::Result<Option<Compression>> {
    let mut head = [0; Compression::MAGIC_LEN];
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    file.rewind()?;
    Ok(Compression::sniff(&head[..filled]))
}

/// Opens a compressed log through a temporary spool.
pub fn open(path: &Path, options: &SpoolOptions) -> Result<LogFile, CompressedError> {
    Spool::decompress(path, options)?.into_log(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Level;

    fn lines(count: usize) -> Vec<u8> {
        let mut text = String::new();
        for i in 0..count {
            let level = if i % 10 == 0 { "error" } else { "info" };
            text.push_str(&format!(
                "{{\"time\":\"2026-10-08T12:00:{:02}Z\",\"level\":\"{level}\",\"msg\":\"m{i}\"}}\n",
                i % 60
            ));
        }
        text.into_bytes()
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }
    /// With a content checksum, as the `zstd` CLI writes by default.
    fn zstd(data: &[u8]) -> Vec<u8> {
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
        encoder.include_checksum(true).unwrap();
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }
    fn bzip2(data: &[u8]) -> Vec<u8> {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    type Encode = fn(&[u8]) -> Vec<u8>;
    const ENCODERS: [(Compression, Encode, &str); 3] = [
        (Compression::Gzip, gzip, "app.log.gz"),
        (Compression::Zstd, zstd, "app.jsonl.zst"),
        (Compression::Bzip2, bzip2, "app.log.bz2"),
    ];

    struct Fixture {
        source_dir: tempfile::TempDir,
        spool_dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                source_dir: tempfile::tempdir().unwrap(),
                spool_dir: tempfile::tempdir().unwrap(),
            }
        }
        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.source_dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
        fn options(&self, cap: u64) -> SpoolOptions {
            SpoolOptions {
                cap,
                dir: Some(self.spool_dir.path().to_owned()),
            }
        }
        fn count(dir: &Path) -> usize {
            std::fs::read_dir(dir).unwrap().count()
        }
        fn spooled(&self) -> usize {
            Self::count(self.spool_dir.path())
        }
        fn sources(&self) -> usize {
            Self::count(self.source_dir.path())
        }
    }

    #[test]
    fn each_format_round_trips_through_the_plain_index() {
        let plain = lines(500);
        let reference = crate::log::LogIndex::build(&plain);
        for (compression, encode, name) in ENCODERS {
            let fixture = Fixture::new();
            let compressed = encode(&plain);
            assert!(compressed.len() < 20 * 1024, "fixtures stay small");
            let path = fixture.write(name, &compressed);
            assert_eq!(sniff_path(&path).unwrap(), Some(compression));

            let spool = Spool::decompress(&path, &fixture.options(DEFAULT_SPOOL_CAP)).unwrap();
            assert_eq!(spool.compression(), compression);
            assert_eq!(spool.len(), plain.len() as u64);
            // Positive control for the cleanup checks below: the spool is
            // a real file in the configured directory while it is alive.
            assert!(spool.path().starts_with(fixture.spool_dir.path()));
            assert_eq!(fixture.spooled(), 1);

            let log = spool.into_log(&path).unwrap();
            assert_eq!(
                fixture.spooled(),
                0,
                "{compression:?} spool outlived indexing"
            );
            assert_eq!(log.path(), path);
            assert_eq!(log.bytes(), &plain[..]);
            assert_eq!(log.index(), &reference);
            assert_eq!(log.index().stats().count(Level::Error), 50);
            assert_eq!(log.record(1).unwrap().fields[2].value, "m1");
            // Nothing is written next to the source.
            assert_eq!(fixture.sources(), 1);
            assert_eq!(std::fs::read(&path).unwrap(), compressed);
        }
    }

    #[test]
    fn default_spool_goes_to_the_temp_dir() {
        let fixture = Fixture::new();
        let path = fixture.write("app.log.gz", &gzip(b"level=info msg=one\n"));
        let spool = Spool::decompress(&path, &SpoolOptions::default()).unwrap();
        assert!(spool.path().starts_with(std::env::temp_dir()));
        assert!(!spool.path().starts_with(fixture.source_dir.path()));
        let spool_path = spool.path().to_owned();
        assert!(spool_path.exists());
        drop(spool);
        assert!(!spool_path.exists(), "spool survived drop");
        assert_eq!(
            open(&path, &SpoolOptions::default()).unwrap().index().len(),
            1
        );
        assert_eq!(fixture.sources(), 1);
    }

    #[test]
    fn concatenated_members_and_frames_are_all_read() {
        let (a, b) = (
            b"level=info msg=a\n".as_slice(),
            b"level=warn msg=b\n".as_slice(),
        );
        for (_, encode, name) in ENCODERS {
            let fixture = Fixture::new();
            let path = fixture.write(name, &[encode(a), encode(b)].concat());
            let log = open(&path, &fixture.options(DEFAULT_SPOOL_CAP)).unwrap();
            assert_eq!(log.bytes(), [a, b].concat(), "{name}");
        }
    }

    #[test]
    fn empty_payloads_open_as_empty_logs() {
        for (_, encode, name) in ENCODERS {
            let fixture = Fixture::new();
            let path = fixture.write(name, &encode(b""));
            assert!(open(&path, &fixture.options(DEFAULT_SPOOL_CAP))
                .unwrap()
                .index()
                .is_empty());
        }
    }

    #[test]
    fn truncated_streams_are_typed_errors_and_leave_no_spool() {
        let plain = lines(500);
        for (compression, encode, name) in ENCODERS {
            let compressed = encode(&plain);
            // Cut inside the body and just before the end (gzip trailer,
            // zstd checksum or last block, bzip2 stream footer).
            for keep in [compressed.len() / 2, compressed.len() - 3] {
                let fixture = Fixture::new();
                let path = fixture.write(name, &compressed[..keep]);
                match Spool::decompress(&path, &fixture.options(DEFAULT_SPOOL_CAP)) {
                    Err(CompressedError::Truncated(c)) => assert_eq!(c, compression),
                    other => panic!("{name} cut at {keep}: {:?}", other.map(|s| s.len())),
                }
                assert_eq!(fixture.spooled(), 0);
            }
        }
    }

    #[test]
    fn corrupt_streams_are_typed_errors_and_leave_no_spool() {
        let plain = lines(500);
        for (compression, encode, name) in ENCODERS {
            let mut compressed = encode(&plain);
            let middle = compressed.len() / 2;
            for byte in &mut compressed[middle..middle + 16] {
                *byte ^= 0x5a;
            }
            let fixture = Fixture::new();
            let path = fixture.write(name, &compressed);
            match Spool::decompress(&path, &fixture.options(DEFAULT_SPOOL_CAP)) {
                Err(CompressedError::Corrupt { compression: c, .. }) => assert_eq!(c, compression),
                other => panic!("{name}: {:?}", other.map(|s| s.len())),
            }
            assert_eq!(fixture.spooled(), 0);
        }
        // Trailing garbage after a valid gzip member is not silently dropped.
        let fixture = Fixture::new();
        let path = fixture.write(
            "app.log.gz",
            &[gzip(&plain), b"\x1f\x8bjunk".to_vec()].concat(),
        );
        assert!(Spool::decompress(&path, &fixture.options(DEFAULT_SPOOL_CAP)).is_err());
        assert_eq!(fixture.spooled(), 0);
    }

    #[test]
    fn size_cap_is_a_typed_error_not_a_partial_index() {
        let plain = lines(500);
        let size = plain.len() as u64;
        for (compression, encode, name) in ENCODERS {
            let fixture = Fixture::new();
            let path = fixture.write(name, &encode(&plain));
            // Exactly at the cap is accepted (positive control) …
            assert_eq!(
                Spool::decompress(&path, &fixture.options(size))
                    .unwrap()
                    .len(),
                size
            );
            assert_eq!(fixture.spooled(), 0);
            // … one byte less is refused, and nothing is left behind.
            match open(&path, &fixture.options(size - 1)) {
                Err(CompressedError::TooLarge {
                    compression: c,
                    cap,
                }) => {
                    assert_eq!((c, cap), (compression, size - 1));
                }
                other => panic!("{name}: {:?}", other.map(|log| log.index().len())),
            }
            assert_eq!(fixture.spooled(), 0);
        }
        // The cap never exceeds the plain-file limit.
        let fixture = Fixture::new();
        let path = fixture.write("app.log.gz", &gzip(b"x\n"));
        assert!(Spool::decompress(&path, &fixture.options(u64::MAX)).is_ok());
    }

    #[test]
    fn detection_uses_magic_bytes_not_names() {
        let fixture = Fixture::new();
        // A gzip stream under a plain log name is still gzip …
        let disguised = fixture.write("app.log", &gzip(b"level=info msg=x\n"));
        assert_eq!(sniff_path(&disguised).unwrap(), Some(Compression::Gzip));
        assert_eq!(
            open(&disguised, &fixture.options(1024))
                .unwrap()
                .index()
                .len(),
            1
        );
        // … and plain text under a compressed name is not.
        let plain = fixture.write("app.log.gz", b"level=info msg=x\n");
        assert_eq!(sniff_path(&plain).unwrap(), None);
        assert!(matches!(
            open(&plain, &fixture.options(1024)),
            Err(CompressedError::NotCompressed)
        ));
        let empty = fixture.write("empty.log.zst", b"");
        assert!(matches!(
            open(&empty, &fixture.options(1024)),
            Err(CompressedError::NotCompressed)
        ));
        assert!(matches!(
            open(
                &fixture.source_dir.path().join("absent.log.gz"),
                &fixture.options(1024)
            ),
            Err(CompressedError::Source(_))
        ));
        assert_eq!(fixture.spooled(), 0);
    }

    #[test]
    fn xz_is_recognised_and_refused() {
        let fixture = Fixture::new();
        let path = fixture.write("app.log.xz", b"\xfd7zXZ\x00\x00\x04\xe6\xd6\xb4\x46");
        assert_eq!(sniff_path(&path).unwrap(), Some(Compression::Xz));
        assert!(!Compression::Xz.is_supported());
        let error = open(&path, &fixture.options(1024)).err().unwrap();
        assert!(matches!(
            error,
            CompressedError::Unsupported(Compression::Xz)
        ));
        assert_eq!(error.to_string(), "xz compressed logs cannot be opened yet");
        assert_eq!(fixture.spooled(), 0);
    }

    #[test]
    fn sniff_magic_table() {
        assert_eq!(
            Compression::sniff(b"\x1f\x8b\x08\x00"),
            Some(Compression::Gzip)
        );
        assert_eq!(Compression::sniff(b"\x1f\x8b"), None, "too short");
        assert_eq!(
            Compression::sniff(b"\x28\xb5\x2f\xfd"),
            Some(Compression::Zstd)
        );
        assert_eq!(
            Compression::sniff(b"\x50\x2a\x4d\x18"),
            Some(Compression::Zstd)
        );
        assert_eq!(Compression::sniff(b"BZh9"), Some(Compression::Bzip2));
        assert_eq!(Compression::sniff(b"BZh0"), None);
        assert_eq!(Compression::sniff(b"{\"level\":"), None);
        assert_eq!(Compression::sniff(b""), None);
    }

    #[test]
    fn compressed_log_names() {
        for yes in [
            "app.log.gz",
            "APP.LOG.GZ",
            "app.jsonl.zst",
            "app.ndjson.zstd",
            "app.logfmt.bz2",
            "app.log.1.gz",
            "/var/log/nginx/access.log.12.gz",
        ] {
            assert!(is_compressed_log_path(Path::new(yes)), "{yes}");
        }
        for no in [
            "app.log",
            "archive.gz",
            "notes.md.gz",
            "app.log.xz",
            "app.log.gz.1",
            "app.log..gz",
            "app.log.1a.gz",
            ".gz",
        ] {
            assert!(!is_compressed_log_path(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn error_messages_name_no_paths() {
        let cap = CompressedError::TooLarge {
            compression: Compression::Gzip,
            cap: MAX_LOG_BYTES,
        };
        assert_eq!(
            cap.to_string(),
            "The uncompressed log is larger than the viewer's 2 GiB limit"
        );
        let cap = CompressedError::TooLarge {
            compression: Compression::Gzip,
            cap: 64 * 1024 * 1024,
        };
        assert_eq!(
            cap.to_string(),
            "The uncompressed log is larger than the viewer's 64 MiB limit"
        );
        assert_eq!(
            CompressedError::Truncated(Compression::Bzip2).to_string(),
            "The bzip2 stream is incomplete"
        );
    }
}
