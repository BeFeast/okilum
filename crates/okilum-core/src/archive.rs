//! Archives in the Reader (#996, #3290): list entries, read one entry for preview,
//! and extract to a chosen folder, without trusting the archive.
//!
//! Supported formats: `.zip`, `.tar`, `.tar.gz` / `.tgz`, `.tar.bz2` / `.tbz2`,
//! `.tar.xz` / `.txz`, `.tar.zst` / `.tzst`, and `.7z`.
//!
//! - Paths that would leave the destination (`..`, absolute, NUL) are listed but never
//!   extracted; one such entry refuses the whole extraction.
//! - Each entry is capped by size and by compression ratio before any byte is
//!   inflated, and while inflating it may not exceed its declared size.
//! - Stream-compressed archives (`.tar.gz`, `.tar.bz2`, `.tar.xz`, `.tar.zst`) are
//!   bounded while decompressing so decompression bombs abort early.
//! - Encrypted entries are listed as locked and never read. Symbolic links and
//!   hard links are listed and skipped.
//! - Extraction never overwrites: an existing target refuses the whole operation, and a
//!   failed run removes what it wrote.
//! - ZIP names without the UTF-8 flag are decoded as CP437, as the ZIP format specifies.
use anyhow::{anyhow, Context as _, Result};
use chrono::{Datelike as _, Timelike as _};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use zip::{CompressionMethod, ZipArchive};

/// Archive formats supported by the read-only archive viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
    SevenZ,
}

/// Detect a supported archive format from `path`, matching compound extensions
/// (`.tar.gz`, `.tar.bz2`, `.tar.xz`, `.tar.zst`) before single suffixes.
///
/// Standalone single-file compression (`.gz`, `.bz2`, `.xz`, `.zst`) is not an
/// archive and returns `None`.
pub fn format_for_path(path: &Path) -> Option<Format> {
    let name = path.file_name()?.to_str()?;
    format_for_name(name)
}

/// Detect a supported archive format from a file name (or relative path).
pub fn format_for_name(name: &str) -> Option<Format> {
    let file_name = Path::new(name).file_name()?.to_str()?;
    let lower = file_name.to_ascii_lowercase();
    // Compound extensions first so `.tar.gz` is never treated as `.gz`.
    const SUFFIXES: [(&str, Format); 11] = [
        (".tar.gz", Format::TarGz),
        (".tar.bz2", Format::TarBz2),
        (".tar.xz", Format::TarXz),
        (".tar.zst", Format::TarZst),
        (".tgz", Format::TarGz),
        (".tbz2", Format::TarBz2),
        (".txz", Format::TarXz),
        (".tzst", Format::TarZst),
        (".tar", Format::Tar),
        (".zip", Format::Zip),
        (".7z", Format::SevenZ),
    ];
    for (suffix, format) in SUFFIXES {
        if let Some(stem) = lower.strip_suffix(suffix) {
            if !stem.is_empty() && !stem.chars().all(|c| c == '.') {
                return Some(format);
            }
        }
    }
    None
}

/// Decoder memory cap for XZ and 7z streams (128 MiB).
const DECODER_MEM_LIMIT_KB: u32 = 128 * 1024;

/// Bounds applied to every read and extraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest single entry that is read or extracted.
    pub entry_bytes: u64,
    /// Largest total of one extraction.
    pub total_bytes: u64,
    /// Highest uncompressed/compressed ratio for entries above `RATIO_FLOOR`.
    pub ratio: u64,
    /// Most entries an archive may list.
    pub entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            entry_bytes: 256 << 20,
            total_bytes: 8 << 30,
            ratio: 250,
            entries: 200_000,
        }
    }
}

/// Small entries compress arbitrarily well (a page of spaces); only larger ones are ratio-checked.
const RATIO_FLOOR: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Symlink,
}

/// The DOS timestamp stored in the archive: local time of the machine that wrote it, no zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Modified {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub index: usize,
    /// Display name, decoded from UTF-8 or CP437.
    pub name: String,
    /// Relative path inside a destination; `None` when the name would escape it.
    pub path: Option<PathBuf>,
    pub kind: Kind,
    pub size: u64,
    pub packed: u64,
    /// True when `packed` is a real per-entry compressed size (ZIP, uncompressed TAR);
    /// false for stream-compressed TAR and solid 7z archives.
    pub packed_known: bool,
    pub modified: Option<Modified>,
    pub encrypted: bool,
    /// The name had no UTF-8 flag and was decoded as CP437.
    pub cp437_name: bool,
    /// Compression method as the archive names it.
    pub method: String,
    /// Stored or deflated; other methods are listed but cannot be read.
    pub supported: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub files: usize,
    pub directories: usize,
    pub size: u64,
    pub packed: u64,
}

/// Why an entry or an extraction was refused. Callers show it; nothing is guessed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    Encrypted(String),
    Symlink(String),
    Unsupported(String, String),
    UnsafePath(String),
    TooLarge(String, u64),
    Bomb(String),
    TooManyEntries(usize),
    TotalTooLarge(u64),
    Exists(Vec<PathBuf>),
    NotDirectory(String),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Encrypted(name) => write!(f, "{name} is encrypted"),
            Refused::Symlink(name) => write!(f, "{name} is a symbolic link"),
            Refused::Unsupported(name, method) => {
                write!(f, "{name} uses unsupported compression ({method})")
            }
            Refused::UnsafePath(name) => write!(f, "{name} would be written outside the folder"),
            Refused::TooLarge(name, size) => write!(f, "{name} is too large ({size} bytes)"),
            Refused::Bomb(name) => write!(f, "{name} expands far more than real files do"),
            Refused::TooManyEntries(count) => write!(f, "The archive lists {count} entries"),
            Refused::TotalTooLarge(size) => write!(f, "The selection expands to {size} bytes"),
            Refused::Exists(paths) => write!(f, "{} already exist", paths.len()),
            Refused::NotDirectory(name) => write!(f, "{name} is not a folder"),
        }
    }
}

impl std::error::Error for Refused {}

fn open(path: &Path) -> Result<ZipArchive<BufReader<File>>> {
    let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    ZipArchive::new(BufReader::new(file)).context("Not a readable ZIP archive")
}

fn describe<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>, index: usize) -> Result<Entry> {
    // ZIP 9 exposes central-directory metadata without opening the entry reader:
    // listing needs neither local-file I/O, decryption nor inflation.
    let file = archive.by_index_data(index)?;
    let raw = file.name_raw();
    let utf8 = file.flags().as_u16() & (1 << 11) != 0;
    // ZIP 9 guesses UTF-8 before CP437, even with EFS clear. Preserve the
    // pre-upgrade name and extraction identity, including lossy flagged UTF-8.
    let name = if utf8 {
        String::from_utf8_lossy(raw).into_owned()
    } else {
        decode_cp437(raw)
    };
    // ZIP 8 treated upper Unix mode bits conservatively regardless of origin.
    let raw_symlink = (file.external_attributes() >> 16) & 0o120000 == 0o120000;
    let kind = if raw_symlink || file.is_symlink() {
        Kind::Symlink
    } else if file.is_dir() {
        Kind::Directory
    } else {
        Kind::File
    };
    Ok(Entry {
        index,
        cp437_name: !utf8 && !raw.is_ascii(),
        path: file.enclosed_name().and_then(|_| decoded_path(&name)),
        name,
        kind,
        size: file.size(),
        packed: file.compressed_size(),
        packed_known: true,
        modified: file.last_modified().map(|t| Modified {
            year: t.year(),
            month: t.month(),
            day: t.day(),
            hour: t.hour(),
            minute: t.minute(),
            second: t.second(),
        }),
        encrypted: file.encrypted(),
        method: format!("{:?}", file.compression()),
        supported: matches!(
            file.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ),
    })
}

/// CP437 maps ASCII unchanged; the upper half is fixed by the ZIP format.
fn decode_cp437(raw: &[u8]) -> String {
    const HIGH: [char; 128] = [
        'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ',
        'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú',
        'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡',
        '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟',
        '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘',
        '┌', '█', '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ',
        '∞', 'φ', 'ε', '∩', '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²',
        '■', ' ',
    ];
    raw.iter()
        .map(|&byte| {
            if byte < 128 {
                char::from(byte)
            } else {
                HIGH[usize::from(byte - 128)]
            }
        })
        .collect()
}

/// The crate has already validated the name's path structure. Decoding only
/// changes non-ASCII characters, so rebuild the same components with our name.
fn decoded_path(name: &str) -> Option<PathBuf> {
    if absolute(name) || name.contains('\0') {
        return None;
    }
    let mut path = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => (),
            ".." => {
                if !path.pop() {
                    return None;
                }
            }
            part => path.push(part),
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

/// The zip crate makes `/etc/x` relative; an archive that names absolute paths is refused instead.
fn absolute(name: &str) -> bool {
    let bytes = name.as_bytes();
    name.starts_with(['/', '\\'])
        || (bytes.len() > 1 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// Sanitize control characters in an untrusted archive entry name before display.
fn sanitize_display_name(raw: &str) -> String {
    if !raw.chars().any(char::is_control) {
        return raw.to_owned();
    }
    raw.chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

/// Strict relative path validation for TAR and 7z entries: rejects absolute paths,
/// Windows drive prefixes, NUL bytes, and any `..` traversal component.
fn safe_relative_path(raw: &str) -> Option<PathBuf> {
    if absolute(raw) || raw.contains('\0') {
        return None;
    }
    let mut path = PathBuf::new();
    for part in raw.split(['/', '\\']) {
        match part {
            "" | "." => (),
            ".." => return None,
            part => path.push(part),
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

fn unix_to_modified(secs: i64) -> Option<Modified> {
    let dt = chrono::DateTime::from_timestamp(secs, 0)?;
    let year = u16::try_from(dt.year()).ok()?;
    Some(Modified {
        year,
        month: dt.month() as u8,
        day: dt.day() as u8,
        hour: dt.hour() as u8,
        minute: dt.minute() as u8,
        second: dt.second() as u8,
    })
}

fn nt_time_to_modified(nt_time: sevenz_rust2::NtTime) -> Option<Modified> {
    const TICKS_PER_SEC: u64 = 10_000_000;
    const UNIX_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;
    let ticks = u64::from(nt_time);
    if ticks == 0 {
        return None;
    }
    let secs = i64::try_from(ticks / TICKS_PER_SEC).ok()? - UNIX_EPOCH_OFFSET_SECS;
    unix_to_modified(secs)
}

/// Bounds a decompressed TAR stream against decompression bombs and excessive output.
struct BoundedStream<R> {
    inner: R,
    read: u64,
    compressed_size: u64,
    max_bytes: u64,
    max_ratio: u64,
    check_ratio: bool,
    label: String,
}

impl<R: Read> Read for BoundedStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read = self.read.saturating_add(n as u64);
        if self.read > self.max_bytes {
            return Err(io::Error::other(Refused::TotalTooLarge(self.read)));
        }
        if self.check_ratio
            && self.read > RATIO_FLOOR
            && self.read / self.compressed_size.max(1) > self.max_ratio
        {
            return Err(io::Error::other(Refused::Bomb(self.label.clone())));
        }
        Ok(n)
    }
}

fn find_refused(err: &(dyn std::error::Error + 'static)) -> Option<Refused> {
    if let Some(refused) = err.downcast_ref::<Refused>() {
        return Some(refused.clone());
    }
    if let Some(io_err) = err.downcast_ref::<io::Error>() {
        if let Some(inner) = io_err.get_ref() {
            if let Some(refused) = find_refused(inner) {
                return Some(refused);
            }
        }
    }
    err.source().and_then(find_refused)
}

fn unwrap_io_refusal(err: io::Error, context: &str) -> anyhow::Error {
    if let Some(refused) = find_refused(&err) {
        return refused.into();
    }
    anyhow::Error::new(err).context(context.to_owned())
}

fn tar_method_name(format: Format) -> &'static str {
    match format {
        Format::Tar => "Stored",
        Format::TarGz => "Gzip",
        Format::TarBz2 => "Bzip2",
        Format::TarXz => "Xz",
        Format::TarZst => "Zstd",
        Format::Zip => "Zip",
        Format::SevenZ => "7z",
    }
}

fn open_tar_stream(
    path: &Path,
    format: Format,
    limits: Limits,
) -> Result<(tar::Archive<BoundedStream<Box<dyn Read>>>, u64)> {
    let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    let file_size = file
        .metadata()
        .with_context(|| format!("Cannot stat {}", path.display()))?
        .len();
    if file_size == 0 || (format == Format::Tar && file_size < 512) {
        return Err(anyhow!("Not a readable TAR archive"));
    }
    let reader = BufReader::new(file);
    let inner: Box<dyn Read> = match format {
        Format::Tar => Box::new(reader),
        Format::TarGz => Box::new(flate2::read::MultiGzDecoder::new(reader)),
        Format::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(reader)),
        Format::TarXz => Box::new(lzma_rust2::XzReader::new_mem_limit(
            reader,
            true,
            DECODER_MEM_LIMIT_KB,
        )),
        Format::TarZst => Box::new(
            zstd::stream::read::Decoder::new(reader).context("Not a readable Zstd TAR archive")?,
        ),
        Format::Zip | Format::SevenZ => unreachable!("non-tar format"),
    };
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    // TAR headers add 512-byte overhead per entry, so allow generous stream headroom
    // above `total_bytes` while still enforcing `total_bytes` and compression ratio.
    let max_bytes = limits
        .total_bytes
        .saturating_add((limits.entries as u64).saturating_mul(1024));
    let bounded = BoundedStream {
        inner,
        read: 0,
        compressed_size: file_size,
        max_bytes,
        max_ratio: limits.ratio,
        check_ratio: format != Format::Tar,
        label,
    };
    Ok((tar::Archive::new(bounded), file_size))
}

fn tar_skip_root(raw_name: &str, kind: Kind) -> bool {
    kind == Kind::Directory && matches!(raw_name, "." | "./")
}

fn describe_tar_entry<R: Read>(
    entry: &tar::Entry<'_, R>,
    index: usize,
    format: Format,
) -> Result<Option<Entry>> {
    let header = entry.header();
    let entry_type = header.entry_type();
    let kind = match entry_type {
        tar::EntryType::Directory => Kind::Directory,
        tar::EntryType::Symlink | tar::EntryType::Link => Kind::Symlink,
        _ => Kind::File,
    };
    let raw_bytes = entry.path_bytes();
    let raw_name = String::from_utf8_lossy(&raw_bytes);
    if tar_skip_root(&raw_name, kind) {
        return Ok(None);
    }
    let supported = matches!(
        entry_type,
        tar::EntryType::Regular
            | tar::EntryType::Continuous
            | tar::EntryType::Directory
            | tar::EntryType::Symlink
            | tar::EntryType::Link
    );
    let size = if kind == Kind::Directory {
        0
    } else {
        entry.size()
    };
    let packed_known = format == Format::Tar;
    let modified = header
        .mtime()
        .ok()
        .filter(|&secs| secs > 0)
        .and_then(|secs| i64::try_from(secs).ok())
        .and_then(unix_to_modified);
    Ok(Some(Entry {
        index,
        name: sanitize_display_name(&raw_name),
        path: safe_relative_path(&raw_name),
        kind,
        size,
        packed: if packed_known { size } else { 0 },
        packed_known,
        modified,
        encrypted: false,
        cp437_name: false,
        method: tar_method_name(format).into(),
        supported,
    }))
}

fn verify_tar_trailer<R: Read>(mut stream: BoundedStream<R>, format: Format) -> Result<()> {
    if format == Format::Tar {
        return Ok(());
    }
    let mut buf = [0u8; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(err) => return Err(unwrap_io_refusal(err, "Truncated or corrupt TAR archive")),
        }
    }
}

fn list_tar(path: &Path, format: Format, limits: Limits) -> Result<Listing> {
    let (mut archive, file_size) = open_tar_stream(path, format, limits)?;
    let mut listing = Listing::default();
    let mut index = 0usize;
    {
        let entries = archive
            .entries()
            .map_err(|err| unwrap_io_refusal(err, "Not a readable TAR archive"))?;
        for raw_entry in entries {
            let entry =
                raw_entry.map_err(|err| unwrap_io_refusal(err, "Not a readable TAR archive"))?;
            let Some(described) = describe_tar_entry(&entry, index, format)? else {
                continue;
            };
            index += 1;
            if index > limits.entries {
                return Err(Refused::TooManyEntries(index).into());
            }
            match described.kind {
                Kind::Directory => listing.directories += 1,
                _ => listing.files += 1,
            }
            listing.size = listing.size.saturating_add(described.size);
            listing.entries.push(described);
        }
    }
    verify_tar_trailer(archive.into_inner(), format)?;
    listing.packed = file_size;
    Ok(listing)
}

fn read_tar_entry(path: &Path, format: Format, index: usize, limits: Limits) -> Result<Vec<u8>> {
    let (mut archive, _) = open_tar_stream(path, format, limits)?;
    let entries = archive
        .entries()
        .map_err(|err| unwrap_io_refusal(err, "Not a readable TAR archive"))?;
    let mut current = 0usize;
    for raw_entry in entries {
        let entry =
            raw_entry.map_err(|err| unwrap_io_refusal(err, "Not a readable TAR archive"))?;
        let Some(described) = describe_tar_entry(&entry, current, format)? else {
            continue;
        };
        if current == index {
            anyhow::ensure!(
                described.kind != Kind::Directory,
                "{} is a folder",
                described.name
            );
            admit(&described, limits)?;
            let mut data = Vec::with_capacity(described.size.min(1 << 20) as usize);
            let written = io::copy(&mut entry.take(described.size + 1), &mut data)
                .map_err(|err| unwrap_io_refusal(err, "Cannot read TAR entry"))?;
            if written != described.size {
                return Err(Refused::Bomb(described.name).into());
            }
            return Ok(data);
        }
        current += 1;
        if current > limits.entries {
            return Err(Refused::TooManyEntries(current).into());
        }
    }
    anyhow::bail!("No entry {index} in the archive")
}

fn open_sevenz(path: &Path) -> Result<(sevenz_rust2::ArchiveReader<BufReader<File>>, u64)> {
    let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    let file_size = file
        .metadata()
        .with_context(|| format!("Cannot stat {}", path.display()))?
        .len();
    let reader = sevenz_rust2::ArchiveReader::new_with_memory_limit(
        BufReader::new(file),
        sevenz_rust2::Password::empty(),
        DECODER_MEM_LIMIT_KB as usize,
    )
    .map_err(|err| match err {
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_) => {
            anyhow!("This 7z archive has encrypted headers")
        }
        other => anyhow!("Not a readable 7z archive: {other}"),
    })?;
    Ok((reader, file_size))
}

fn describe_sevenz_entry(archive: &sevenz_rust2::Archive, index: usize) -> Entry {
    let file = &archive.files[index];
    let encrypted = archive.is_file_encrypted(index);
    let is_symlink = file.has_windows_attributes
        && ((file.windows_attributes & 0x0400 != 0)
            || ((file.windows_attributes >> 16) & 0o170000 == 0o120000));
    let kind = if is_symlink {
        Kind::Symlink
    } else if file.is_directory {
        Kind::Directory
    } else {
        Kind::File
    };
    let size = if kind == Kind::Directory {
        0
    } else {
        file.size
    };
    let modified = file
        .has_last_modified_date
        .then(|| nt_time_to_modified(file.last_modified_date))
        .flatten();
    Entry {
        index,
        name: sanitize_display_name(&file.name),
        path: safe_relative_path(&file.name),
        kind,
        size,
        packed: file.compressed_size,
        packed_known: false,
        modified,
        encrypted,
        cp437_name: false,
        method: "7z".into(),
        supported: !file.is_anti_item,
    }
}

fn list_sevenz(path: &Path, limits: Limits) -> Result<Listing> {
    let (reader, file_size) = open_sevenz(path)?;
    let archive = reader.archive();
    if archive.files.len() > limits.entries {
        return Err(Refused::TooManyEntries(archive.files.len()).into());
    }
    let mut listing = Listing::default();
    for index in 0..archive.files.len() {
        let entry = describe_sevenz_entry(archive, index);
        match entry.kind {
            Kind::Directory => listing.directories += 1,
            _ => listing.files += 1,
        }
        listing.size = listing.size.saturating_add(entry.size);
        listing.entries.push(entry);
    }
    listing.packed = file_size;
    Ok(listing)
}

fn read_sevenz_entry(path: &Path, index: usize, limits: Limits) -> Result<Vec<u8>> {
    let (reader, file_size) = open_sevenz(path)?;
    let entry = {
        let archive = reader.archive();
        anyhow::ensure!(
            index < archive.files.len(),
            "No entry {index} in the archive"
        );
        describe_sevenz_entry(archive, index)
    };
    anyhow::ensure!(entry.kind != Kind::Directory, "{} is a folder", entry.name);
    admit(&entry, limits)?;
    if entry.size > RATIO_FLOOR && entry.size / file_size.max(1) > limits.ratio {
        return Err(Refused::Bomb(entry.name).into());
    }
    let (has_stream, block_index) = {
        let archive = reader.archive();
        (
            archive.files[index].has_stream,
            archive.stream_map.file_block_index[index],
        )
    };
    if !has_stream || entry.size == 0 {
        return Ok(Vec::new());
    }
    let Some(block_index) = block_index else {
        return Ok(Vec::new());
    };
    let block_files: Vec<usize> = reader
        .archive()
        .stream_map
        .file_block_index
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| (b == Some(block_index)).then_some(i))
        .collect();
    // Guard against a solid block whose preceding files expand beyond limits before our entry.
    let block_prefix_bytes: u64 = block_files
        .iter()
        .take_while(|&&i| i <= index)
        .map(|&i| reader.archive().files[i].size)
        .fold(0u64, u64::saturating_add);
    if block_prefix_bytes > limits.total_bytes {
        return Err(Refused::TotalTooLarge(block_prefix_bytes).into());
    }
    if block_prefix_bytes > RATIO_FLOOR && block_prefix_bytes / file_size.max(1) > limits.ratio {
        return Err(Refused::Bomb(entry.name).into());
    }
    let mut found: Option<Vec<u8>> = None;
    let mut read_error: Option<anyhow::Error> = None;
    let mut pos = 0usize;
    let archive = reader.archive().clone();
    let mut source = reader.into_inner();
    let password = sevenz_rust2::Password::empty();
    let decoder = sevenz_rust2::BlockDecoder::new_with_memory_limit(
        1,
        DECODER_MEM_LIMIT_KB as usize,
        block_index,
        &archive,
        &password,
        &mut source,
    );
    decoder
        .for_each_entries(&mut |_, entry_reader| {
            let file_idx = block_files.get(pos).copied();
            pos += 1;
            if file_idx == Some(index) {
                let mut data = Vec::with_capacity(entry.size.min(1 << 20) as usize);
                match io::copy(&mut entry_reader.take(entry.size + 1), &mut data) {
                    Ok(written) if written == entry.size => {
                        found = Some(data);
                    }
                    Ok(_) => {
                        read_error = Some(Refused::Bomb(entry.name.clone()).into());
                    }
                    Err(err) => {
                        read_error = Some(anyhow!("Cannot read 7z entry: {err}"));
                    }
                }
                return Ok(false);
            }
            Ok(true)
        })
        .map_err(|err| anyhow!("Cannot read 7z entry: {err}"))?;
    if let Some(err) = read_error {
        return Err(err);
    }
    found.ok_or_else(|| anyhow!("No entry {index} in the archive"))
}

/// List every entry from the archive.
pub fn list(path: &Path, limits: Limits) -> Result<Listing> {
    match format_for_path(path).unwrap_or(Format::Zip) {
        Format::Zip => list_zip(path, limits),
        format
        @ (Format::Tar | Format::TarGz | Format::TarBz2 | Format::TarXz | Format::TarZst) => {
            list_tar(path, format, limits)
        }
        Format::SevenZ => list_sevenz(path, limits),
    }
}

fn list_zip(path: &Path, limits: Limits) -> Result<Listing> {
    let mut archive = open(path)?;
    if archive.len() > limits.entries {
        return Err(Refused::TooManyEntries(archive.len()).into());
    }
    let mut listing = Listing::default();
    for index in 0..archive.len() {
        let entry = describe(&mut archive, index)?;
        match entry.kind {
            Kind::Directory => listing.directories += 1,
            _ => listing.files += 1,
        }
        listing.size = listing.size.saturating_add(entry.size);
        listing.packed = listing.packed.saturating_add(entry.packed);
        listing.entries.push(entry);
    }
    Ok(listing)
}

/// Refuse an entry before reading a byte of it.
fn admit(entry: &Entry, limits: Limits) -> Result<(), Refused> {
    let name = || entry.name.clone();
    if entry.encrypted {
        return Err(Refused::Encrypted(name()));
    }
    if entry.kind == Kind::Symlink {
        return Err(Refused::Symlink(name()));
    }
    if !entry.supported {
        return Err(Refused::Unsupported(name(), entry.method.clone()));
    }
    if entry.size > limits.entry_bytes {
        return Err(Refused::TooLarge(name(), entry.size));
    }
    if entry.packed_known
        && entry.size > RATIO_FLOOR
        && (entry.packed == 0 || entry.size / entry.packed > limits.ratio)
    {
        return Err(Refused::Bomb(name()));
    }
    Ok(())
}

/// Inflate one admitted entry into `out`, never past its declared size.
fn copy<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    entry: &Entry,
    out: &mut impl Write,
) -> Result<u64> {
    let file = archive.by_index(entry.index)?;
    let written = std::io::copy(&mut file.take(entry.size + 1), out)?;
    if written != entry.size {
        return Err(Refused::Bomb(entry.name.clone()).into());
    }
    Ok(written)
}

/// Read one entry for preview, streaming it out of the archive only.
pub fn read_entry(path: &Path, index: usize, limits: Limits) -> Result<Vec<u8>> {
    match format_for_path(path).unwrap_or(Format::Zip) {
        Format::Zip => {
            let mut archive = open(path)?;
            anyhow::ensure!(index < archive.len(), "No entry {index} in the archive");
            let entry = describe(&mut archive, index)?;
            anyhow::ensure!(entry.kind != Kind::Directory, "{} is a folder", entry.name);
            admit(&entry, limits)?;
            let mut data = Vec::with_capacity(entry.size as usize);
            copy(&mut archive, &entry, &mut data)?;
            Ok(data)
        }
        format
        @ (Format::Tar | Format::TarGz | Format::TarBz2 | Format::TarXz | Format::TarZst) => {
            read_tar_entry(path, format, index, limits)
        }
        Format::SevenZ => read_sevenz_entry(path, index, limits),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extracted {
    pub files: Vec<PathBuf>,
    pub directories: Vec<PathBuf>,
    pub bytes: u64,
    /// Entries left out, with the reason: encrypted, symbolic links, unsupported methods.
    pub skipped: Vec<(String, String)>,
}

/// Extract `indices` (all entries when empty) into the existing folder `destination`.
///
/// Every check runs before the first write. A failure while writing removes the files and
/// folders this call created, in reverse order.
pub fn extract(
    path: &Path,
    indices: &[usize],
    destination: &Path,
    limits: Limits,
) -> Result<Extracted> {
    let meta = std::fs::symlink_metadata(destination)
        .with_context(|| format!("Cannot open {}", destination.display()))?;
    if !meta.is_dir() {
        return Err(Refused::NotDirectory(destination.display().to_string()).into());
    }
    let listing = list(path, limits)?;
    let selected: Vec<&Entry> = if indices.is_empty() {
        listing.entries.iter().collect()
    } else {
        let mut chosen = Vec::new();
        for &index in indices {
            chosen.push(
                listing
                    .entries
                    .get(index)
                    .with_context(|| format!("No entry {index} in the archive"))?,
            );
        }
        chosen
    };
    let mut result = Extracted::default();
    let mut plan = Vec::new();
    let mut total = 0u64;
    let mut existing = Vec::new();
    for entry in selected {
        let Some(relative) = entry.path.as_ref() else {
            return Err(Refused::UnsafePath(entry.name.clone()).into());
        };
        let target = destination.join(relative);
        if entry.kind == Kind::Directory {
            // An existing folder is merged into; anything else in its place is a conflict.
            if std::fs::symlink_metadata(&target).is_ok_and(|meta| !meta.is_dir()) {
                existing.push(target.clone());
            }
            plan.push((entry, target));
            continue;
        }
        match admit(entry, limits) {
            Ok(()) => {}
            Err(
                reason @ (Refused::Encrypted(_) | Refused::Symlink(_) | Refused::Unsupported(..)),
            ) => {
                result
                    .skipped
                    .push((entry.name.clone(), reason.to_string()));
                continue;
            }
            Err(reason) => return Err(reason.into()),
        }
        total = total.saturating_add(entry.size);
        if std::fs::symlink_metadata(&target).is_ok() {
            existing.push(target.clone());
        }
        plan.push((entry, target));
    }
    if total > limits.total_bytes {
        return Err(Refused::TotalTooLarge(total).into());
    }
    if !existing.is_empty() {
        return Err(Refused::Exists(existing).into());
    }
    for (_, target) in &plan {
        refuse_linked_ancestor(destination, target)?;
    }
    let format = format_for_path(path).unwrap_or(Format::Zip);
    let written = match format {
        Format::Zip => extract_zip(path, destination, &plan, &mut result),
        format
        @ (Format::Tar | Format::TarGz | Format::TarBz2 | Format::TarXz | Format::TarZst) => {
            extract_tar(path, format, destination, &plan, limits, &mut result)
        }
        Format::SevenZ => extract_sevenz(path, destination, &plan, total, limits, &mut result),
    };
    if let Err(error) = written {
        for file in result.files.iter().rev() {
            let _ = std::fs::remove_file(file);
        }
        for directory in result.directories.iter().rev() {
            let _ = std::fs::remove_dir(directory);
        }
        return Err(error);
    }
    Ok(result)
}

fn extract_zip(
    path: &Path,
    destination: &Path,
    plan: &[(&Entry, PathBuf)],
    result: &mut Extracted,
) -> Result<()> {
    let mut archive = open(path)?;
    for (entry, target) in plan {
        create_directories(destination, target, entry.kind == Kind::Directory, result)?;
        if entry.kind == Kind::Directory {
            continue;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .with_context(|| format!("Cannot create {}", target.display()))?;
        result.files.push(target.clone());
        result.bytes += copy(&mut archive, entry, &mut file)?;
        file.sync_all()?;
    }
    Ok(())
}

fn extract_tar(
    path: &Path,
    format: Format,
    destination: &Path,
    plan: &[(&Entry, PathBuf)],
    limits: Limits,
    result: &mut Extracted,
) -> Result<()> {
    let mut planned: HashMap<usize, (&Entry, &Path)> = plan
        .iter()
        .map(|(entry, target)| (entry.index, (*entry, target.as_path())))
        .collect();
    let (mut archive, _) = open_tar_stream(path, format, limits)?;
    let entries = archive
        .entries()
        .map_err(|err| unwrap_io_refusal(err, "Not a readable TAR archive"))?;
    let mut current = 0usize;
    for raw_entry in entries {
        let entry =
            raw_entry.map_err(|err| unwrap_io_refusal(err, "Not a readable TAR archive"))?;
        let Some(described) = describe_tar_entry(&entry, current, format)? else {
            continue;
        };
        if let Some((planned_entry, target)) = planned.remove(&current) {
            create_directories(
                destination,
                target,
                planned_entry.kind == Kind::Directory,
                result,
            )?;
            if planned_entry.kind != Kind::Directory {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(target)
                    .with_context(|| format!("Cannot create {}", target.display()))?;
                result.files.push(target.to_path_buf());
                let written = io::copy(&mut entry.take(described.size + 1), &mut file)
                    .map_err(|err| unwrap_io_refusal(err, "Cannot extract TAR entry"))?;
                if written != described.size {
                    return Err(Refused::Bomb(described.name).into());
                }
                result.bytes += written;
                file.sync_all()?;
            }
            if planned.is_empty() {
                break;
            }
        }
        current += 1;
    }
    Ok(())
}

fn extract_sevenz(
    path: &Path,
    destination: &Path,
    plan: &[(&Entry, PathBuf)],
    total: u64,
    limits: Limits,
    result: &mut Extracted,
) -> Result<()> {
    let (reader, file_size) = open_sevenz(path)?;
    if total > RATIO_FLOOR && total / file_size.max(1) > limits.ratio {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".into());
        return Err(Refused::Bomb(name).into());
    }
    let archive = reader.archive().clone();
    let mut source = reader.into_inner();
    let mut by_block: HashMap<usize, HashMap<usize, (&Entry, &Path)>> = HashMap::new();
    for (entry, target) in plan {
        if entry.kind == Kind::Directory {
            create_directories(destination, target, true, result)?;
            continue;
        }
        let arc_file = &archive.files[entry.index];
        if !arc_file.has_stream || entry.size == 0 {
            create_directories(destination, target, false, result)?;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)
                .with_context(|| format!("Cannot create {}", target.display()))?;
            result.files.push(target.clone());
            file.sync_all()?;
            continue;
        }
        if let Some(block_index) = archive.stream_map.file_block_index[entry.index] {
            by_block
                .entry(block_index)
                .or_default()
                .insert(entry.index, (*entry, target.as_path()));
        }
    }
    let mut block_indices: Vec<usize> = by_block.keys().copied().collect();
    block_indices.sort_unstable();
    let password = sevenz_rust2::Password::empty();
    for block_index in block_indices {
        let mut targets = by_block.remove(&block_index).unwrap_or_default();
        let block_files: Vec<usize> = archive
            .stream_map
            .file_block_index
            .iter()
            .enumerate()
            .filter_map(|(i, &b)| (b == Some(block_index)).then_some(i))
            .collect();
        let mut pos = 0usize;
        let mut write_error: Option<anyhow::Error> = None;
        let decoder = sevenz_rust2::BlockDecoder::new_with_memory_limit(
            1,
            DECODER_MEM_LIMIT_KB as usize,
            block_index,
            &archive,
            &password,
            &mut source,
        );
        decoder
            .for_each_entries(&mut |_, entry_reader| {
                let file_idx = block_files.get(pos).copied();
                pos += 1;
                if let Some((planned_entry, target)) = file_idx.and_then(|idx| targets.remove(&idx))
                {
                    let step = (|| -> Result<()> {
                        create_directories(destination, target, false, result)?;
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(target)
                            .with_context(|| format!("Cannot create {}", target.display()))?;
                        result.files.push(target.to_path_buf());
                        let written =
                            io::copy(&mut entry_reader.take(planned_entry.size + 1), &mut file)
                                .with_context(|| {
                                    format!("Cannot extract {}", planned_entry.name)
                                })?;
                        if written != planned_entry.size {
                            return Err(Refused::Bomb(planned_entry.name.clone()).into());
                        }
                        result.bytes += written;
                        file.sync_all()?;
                        Ok(())
                    })();
                    if let Err(err) = step {
                        write_error = Some(err);
                        return Ok(false);
                    }
                    if targets.is_empty() {
                        return Ok(false);
                    }
                }
                Ok(true)
            })
            .map_err(|err| anyhow!("Cannot extract 7z archive: {err}"))?;
        if let Some(err) = write_error {
            return Err(err);
        }
    }
    Ok(())
}

/// An existing symbolic link between the destination and a target would redirect the write.
fn refuse_linked_ancestor(destination: &Path, target: &Path) -> Result<()> {
    let mut current = target.parent();
    while let Some(directory) = current {
        if directory == destination {
            break;
        }
        if let Ok(meta) = std::fs::symlink_metadata(directory) {
            if meta.file_type().is_symlink() {
                return Err(Refused::UnsafePath(directory.display().to_string()).into());
            }
        }
        current = directory.parent();
    }
    Ok(())
}

fn create_directories(
    destination: &Path,
    target: &Path,
    is_directory: bool,
    result: &mut Extracted,
) -> Result<()> {
    let leaf = if is_directory {
        Some(target)
    } else {
        target.parent()
    };
    let Some(leaf) = leaf else { return Ok(()) };
    let mut missing = Vec::new();
    let mut current = Some(leaf);
    while let Some(directory) = current {
        if directory == destination || directory.exists() {
            break;
        }
        missing.push(directory.to_path_buf());
        current = directory.parent();
    }
    for directory in missing.into_iter().rev() {
        std::fs::create_dir(&directory)
            .with_context(|| format!("Cannot create {}", directory.display()))?;
        result.directories.push(directory);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    /// A hand-written ZIP of stored entries: (raw name, UTF-8 flag, encrypted flag, data).
    /// Lets tests use names and flags a well-behaved writer refuses to produce.
    fn stored_zip(entries: &[(&[u8], bool, bool, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, utf8, encrypted, data) in entries {
            let offset = out.len() as u32;
            let flags: u16 = if *utf8 { 1 << 11 } else { 0 } | if *encrypted { 1 } else { 0 };
            let crc = crc32fast::hash(data);
            let header = |sig: u32, central: bool| {
                let mut h = Vec::new();
                h.extend(sig.to_le_bytes());
                if central {
                    h.extend(20u16.to_le_bytes()); // made by
                }
                h.extend(20u16.to_le_bytes()); // needed
                h.extend(flags.to_le_bytes());
                h.extend(0u16.to_le_bytes()); // stored
                h.extend(0u16.to_le_bytes()); // time
                h.extend(0x5949u16.to_le_bytes()); // date 2024-10-09
                h.extend(crc.to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend(0u16.to_le_bytes()); // extra
                if central {
                    h.extend(0u16.to_le_bytes()); // comment
                    h.extend(0u16.to_le_bytes()); // disk
                    h.extend(0u16.to_le_bytes()); // internal attrs
                    h.extend(0u32.to_le_bytes()); // external attrs
                    h.extend(offset.to_le_bytes());
                }
                h.extend(*name);
                h
            };
            out.extend(header(0x04034b50, false));
            out.extend(*data);
            central.extend(header(0x02014b50, true));
        }
        let start = out.len() as u32;
        out.extend(&central);
        out.extend(0x06054b50u32.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    /// Preserve hostile mode bits that a well-behaved ZIP writer may mask.
    fn with_unix_modes(mut bytes: Vec<u8>, modes: &[u32]) -> Vec<u8> {
        let mut offset =
            u32::from_le_bytes(bytes[bytes.len() - 6..bytes.len() - 2].try_into().unwrap())
                as usize;
        for mode in modes {
            assert_eq!(&bytes[offset..offset + 4], &0x02014b50u32.to_le_bytes());
            bytes[offset + 4..offset + 6].copy_from_slice(&((3u16 << 8) | 20).to_le_bytes());
            bytes[offset + 38..offset + 42].copy_from_slice(&(mode << 16).to_le_bytes());
            assert_eq!(
                u32::from_le_bytes(bytes[offset + 38..offset + 42].try_into().unwrap()) >> 16,
                *mode
            );
            let field_len = |index| {
                u16::from_le_bytes(
                    bytes[offset + index..offset + index + 2]
                        .try_into()
                        .unwrap(),
                ) as usize
            };
            offset += 46 + field_len(28) + field_len(30) + field_len(32);
        }
        bytes
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn refused(error: anyhow::Error) -> Refused {
        error.downcast::<Refused>().expect("a typed refusal")
    }

    /// Deflated entries written by the zip crate itself.
    fn deflated(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn lists_nested_entries_with_sizes_and_cp437_names() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write(
            dir.path(),
            "a.zip",
            &stored_zip(&[
                (b"notes/", true, false, b""),
                (b"notes/read me.md", true, false, b"# Hello\n"),
                ("notes/привет.txt".as_bytes(), true, false, b"utf-8"),
                (b"caf\x82.txt", false, false, b"cp437"),
            ]),
        );
        let listing = list(&archive, Limits::default()).unwrap();
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["notes/", "notes/read me.md", "notes/привет.txt", "café.txt"]
        );
        assert_eq!((listing.files, listing.directories), (3, 1));
        assert_eq!(listing.size, 8 + 5 + 5);
        assert_eq!(listing.entries[0].kind, Kind::Directory);
        assert!(!listing.entries[2].cp437_name);
        assert!(listing.entries[3].cp437_name);
        assert_eq!(
            listing.entries[1].path.as_deref(),
            Some(Path::new("notes/read me.md"))
        );
        let modified = listing.entries[1].modified.unwrap();
        assert_eq!((modified.year, modified.month, modified.day), (2024, 10, 9));
        assert_eq!(
            read_entry(&archive, 3, Limits::default()).unwrap(),
            b"cp437"
        );
    }

    #[test]
    fn utf8_flag_controls_ambiguous_names_and_extraction_paths() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write(
            dir.path(),
            "ambiguous.zip",
            &stored_zip(&[
                (b"\xc3\xa9.txt", false, false, b"CP437 bytes"),
                (b"utf8/\xc3\xa9.txt", true, false, b"UTF-8 bytes"),
                (b"ascii.txt", false, false, b"ASCII"),
            ]),
        );
        let listing = list(&archive, Limits::default()).unwrap();
        assert_eq!(listing.entries[0].name, "├⌐.txt");
        assert!(listing.entries[0].cp437_name);
        assert_eq!(listing.entries[1].name, "utf8/é.txt");
        assert!(!listing.entries[1].cp437_name);
        assert!(!listing.entries[2].cp437_name);
        for entry in &listing.entries {
            assert_eq!(entry.path.as_deref(), Some(Path::new(&entry.name)));
        }
        let out = tempfile::tempdir().unwrap();
        let result = extract(&archive, &[], out.path(), Limits::default()).unwrap();
        assert_eq!(result.files.len(), 3);
        assert_eq!(
            std::fs::read(out.path().join("├⌐.txt")).unwrap(),
            b"CP437 bytes"
        );
        assert_eq!(
            std::fs::read(out.path().join("utf8/é.txt")).unwrap(),
            b"UTF-8 bytes"
        );
    }

    #[test]
    fn dos_origin_unix_symlink_attributes_are_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = with_unix_modes(
            stored_zip(&[
                (b"link", true, false, b"../outside"),
                (b"plain.txt", true, false, b"safe"),
            ]),
            &[0o120777, 0o100644],
        );
        let central =
            u32::from_le_bytes(bytes[bytes.len() - 6..bytes.len() - 2].try_into().unwrap())
                as usize;
        bytes[central + 5] = 0; // DOS origin, retaining raw upper Unix bits.
        let archive = write(dir.path(), "dos-link.zip", &bytes);
        let listing = list(&archive, Limits::default()).unwrap();
        assert_eq!(listing.entries[0].kind, Kind::Symlink);
        assert_eq!(
            refused(read_entry(&archive, 0, Limits::default()).unwrap_err()),
            Refused::Symlink("link".into())
        );
        let out = tempfile::tempdir().unwrap();
        let result = extract(&archive, &[], out.path(), Limits::default()).unwrap();
        assert_eq!(result.files, [out.path().join("plain.txt")]);
        assert_eq!(result.skipped.len(), 1);
        assert!(!out.path().join("link").exists());
        assert_eq!(
            std::fs::read(out.path().join("plain.txt")).unwrap(),
            b"safe"
        );
    }

    #[test]
    fn reads_one_deflated_entry_without_extracting() {
        let dir = tempfile::tempdir().unwrap();
        let text = "line of a log file\n".repeat(2000);
        let archive = write(
            dir.path(),
            "d.zip",
            &deflated(&[
                ("docs/", b""),
                ("docs/log.txt", text.as_bytes()),
                ("x.png", b"\x89PNG"),
            ]),
        );
        let listing = list(&archive, Limits::default()).unwrap();
        let log = listing
            .entries
            .iter()
            .find(|e| e.name == "docs/log.txt")
            .unwrap();
        assert!(log.packed < log.size);
        assert_eq!(
            read_entry(&archive, log.index, Limits::default()).unwrap(),
            text.as_bytes()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn encrypted_entry_is_listed_locked_and_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write(
            dir.path(),
            "e.zip",
            &stored_zip(&[
                (b"secret.md", true, true, b"ciphertext"),
                (b"open.md", true, false, b"plain"),
            ]),
        );
        let listing = list(&archive, Limits::default()).unwrap();
        assert!(listing.entries[0].encrypted && !listing.entries[1].encrypted);
        let error = read_entry(&archive, 0, Limits::default()).unwrap_err();
        assert_eq!(refused(error), Refused::Encrypted("secret.md".into()));
        // Positive control: the plain entry of the same archive reads.
        assert_eq!(
            read_entry(&archive, 1, Limits::default()).unwrap(),
            b"plain"
        );
        let out = tempfile::tempdir().unwrap();
        let done = extract(&archive, &[], out.path(), Limits::default()).unwrap();
        assert_eq!(done.files, [out.path().join("open.md")]);
        assert_eq!(done.skipped.len(), 1);
        assert!(!out.path().join("secret.md").exists());
    }

    #[test]
    fn zip_slip_entries_refuse_the_whole_extraction() {
        for evil in [
            &b"../evil.txt"[..],
            b"/tmp/evil.txt",
            b"a/../../evil.txt",
            b"C:/evil.txt",
            b"\\server\\evil.txt",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let archive = write(
                dir.path(),
                "s.zip",
                &stored_zip(&[
                    (b"good.txt", true, false, b"ok"),
                    (evil, true, false, b"pwned"),
                ]),
            );
            let listing = list(&archive, Limits::default()).unwrap();
            assert_eq!(listing.entries[1].path, None, "{}", listing.entries[1].name);
            let out = tempfile::tempdir().unwrap();
            let target = out.path().join("inner");
            std::fs::create_dir(&target).unwrap();
            let error = extract(&archive, &[], &target, Limits::default()).unwrap_err();
            assert!(matches!(refused(error), Refused::UnsafePath(_)));
            assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
            assert!(!out.path().join("evil.txt").exists());
            // Positive control: without the escaping entry the same extraction writes.
            let safe = extract(&archive, &[0], &target, Limits::default()).unwrap();
            assert_eq!(std::fs::read(target.join("good.txt")).unwrap(), b"ok");
            assert_eq!(safe.files.len(), 1);
        }
    }

    #[test]
    fn zip_bomb_is_refused_before_inflating_and_a_real_file_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let zeros = vec![0u8; 16 << 20];
        let archive = write(dir.path(), "b.zip", &deflated(&[("zeros.bin", &zeros)]));
        let entry = list(&archive, Limits::default()).unwrap().entries.remove(0);
        assert!(
            entry.size / entry.packed > 250,
            "fixture must be a bomb: {entry:?}"
        );
        assert_eq!(
            refused(read_entry(&archive, 0, Limits::default()).unwrap_err()),
            Refused::Bomb("zeros.bin".into())
        );
        let out = tempfile::tempdir().unwrap();
        assert_eq!(
            refused(extract(&archive, &[], out.path(), Limits::default()).unwrap_err()),
            Refused::Bomb("zeros.bin".into())
        );
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
        // Positive control: 2 MiB of ordinary text, well compressible, passes.
        let text: String = (0..60_000)
            .map(|i| format!("{i} event processed\n"))
            .collect();
        let archive = write(
            dir.path(),
            "t.zip",
            &deflated(&[("big.log", text.as_bytes())]),
        );
        assert_eq!(
            read_entry(&archive, 0, Limits::default()).unwrap(),
            text.as_bytes()
        );
        // And the size cap itself.
        let small = Limits {
            entry_bytes: 1024,
            ..Limits::default()
        };
        assert!(matches!(
            refused(read_entry(&archive, 0, small).unwrap_err()),
            Refused::TooLarge(..)
        ));
    }

    #[test]
    fn extraction_never_overwrites_and_reports_every_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write(
            dir.path(),
            "o.zip",
            &stored_zip(&[
                (b"a.md", true, false, b"new a"),
                (b"sub/b.md", true, false, b"new b"),
            ]),
        );
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("a.md"), "mine").unwrap();
        let error = extract(&archive, &[], out.path(), Limits::default()).unwrap_err();
        assert_eq!(
            refused(error),
            Refused::Exists(vec![out.path().join("a.md")])
        );
        assert_eq!(
            std::fs::read_to_string(out.path().join("a.md")).unwrap(),
            "mine"
        );
        assert!(!out.path().join("sub").exists());
        // Positive control: an empty folder takes the whole archive.
        let clean = tempfile::tempdir().unwrap();
        let done = extract(&archive, &[], clean.path(), Limits::default()).unwrap();
        assert_eq!(done.bytes, 10);
        assert_eq!(
            std::fs::read_to_string(clean.path().join("sub/b.md")).unwrap(),
            "new b"
        );
        assert_eq!(done.directories, [clean.path().join("sub")]);
    }

    #[test]
    fn a_file_where_the_archive_has_a_folder_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write(
            dir.path(),
            "f.zip",
            &stored_zip(&[
                (b"docs/", true, false, b""),
                (b"docs/a.md", true, false, b"a"),
            ]),
        );
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("docs"), "a plain file").unwrap();
        let error = extract(&archive, &[0], out.path(), Limits::default()).unwrap_err();
        assert_eq!(
            refused(error),
            Refused::Exists(vec![out.path().join("docs")])
        );
        assert_eq!(
            std::fs::read_to_string(out.path().join("docs")).unwrap(),
            "a plain file"
        );
        // Positive control: an existing folder of that name is merged into.
        let merge = tempfile::tempdir().unwrap();
        std::fs::create_dir(merge.path().join("docs")).unwrap();
        let done = extract(&archive, &[], merge.path(), Limits::default()).unwrap();
        assert_eq!(done.files, [merge.path().join("docs/a.md")]);
        assert!(done.directories.is_empty());
    }

    #[test]
    fn archive_symlinks_with_huge_declared_sizes_are_never_read_or_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = with_unix_modes(
            stored_zip(&[
                (b"link", true, false, b"../outside"),
                (b"plain.txt", true, false, b"safe"),
            ]),
            &[0o120777, 0o100644],
        );
        let central =
            u32::from_le_bytes(bytes[bytes.len() - 6..bytes.len() - 2].try_into().unwrap())
                as usize;
        // An untrusted symlink must not allocate from its declared target size.
        let declared_size = u32::MAX - 1;
        bytes[central + 24..central + 28].copy_from_slice(&declared_size.to_le_bytes());
        let archive = write(dir.path(), "links.zip", &bytes);
        let listing = list(&archive, Limits::default()).unwrap();
        assert_eq!(listing.entries[0].kind, Kind::Symlink);
        assert_eq!(listing.entries[0].size, u64::from(declared_size));
        assert_eq!(
            refused(read_entry(&archive, 0, Limits::default()).unwrap_err()),
            Refused::Symlink("link".into())
        );
        let out = tempfile::tempdir().unwrap();
        let result = extract(&archive, &[], out.path(), Limits::default()).unwrap();
        assert_eq!(result.files, [out.path().join("plain.txt")]);
        assert_eq!(
            result.skipped,
            [("link".into(), "link is a symbolic link".into())]
        );
        assert!(std::fs::symlink_metadata(out.path().join("link")).is_err());
        assert_eq!(
            std::fs::read(out.path().join("plain.txt")).unwrap(),
            b"safe"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extraction_never_preserves_archive_suid_sgid_or_sticky_bits() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let archive = write(
            dir.path(),
            "privileged.zip",
            &with_unix_modes(
                stored_zip(&[
                    (b"folder/", true, false, b""),
                    (b"folder/file", true, false, b"ordinary content"),
                ]),
                &[0o047777, 0o107777],
            ),
        );
        // The raw fixture contains privilege bits (checked by with_unix_modes).
        // ZIP 9 must mask them in metadata while preserving ordinary permission bits.
        let raw = open(&archive).unwrap();
        for index in 0..2 {
            assert_eq!(
                raw.by_index_data(index).unwrap().unix_mode().unwrap() & 0o7777,
                0o777
            );
        }
        let out = tempfile::tempdir().unwrap();
        let result = extract(&archive, &[], out.path(), Limits::default()).unwrap();
        assert_eq!(result.files, [out.path().join("folder/file")]);
        assert_eq!(
            std::fs::read(&result.files[0]).unwrap(),
            b"ordinary content"
        );
        for target in [out.path().join("folder"), out.path().join("folder/file")] {
            assert_eq!(
                std::fs::metadata(target).unwrap().permissions().mode() & 0o7000,
                0
            );
        }
    }

    #[test]
    fn entry_count_and_total_size_limits_refuse_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write(
            dir.path(),
            "limits.zip",
            &stored_zip(&[
                (b"a.txt", true, false, b"one"),
                (b"b.txt", true, false, b"two"),
            ]),
        );
        let out = tempfile::tempdir().unwrap();
        let count_limit = Limits {
            entries: 1,
            ..Limits::default()
        };
        assert_eq!(
            refused(extract(&archive, &[], out.path(), count_limit).unwrap_err()),
            Refused::TooManyEntries(2)
        );
        let total_limit = Limits {
            total_bytes: 3,
            ..Limits::default()
        };
        assert_eq!(
            refused(extract(&archive, &[], out.path(), total_limit).unwrap_err()),
            Refused::TotalTooLarge(6)
        );
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
        let result = extract(&archive, &[1], out.path(), total_limit).unwrap();
        assert_eq!(result.bytes, 3);
        assert_eq!(std::fs::read(out.path().join("b.txt")).unwrap(), b"two");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_folder_in_the_destination_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write(
            dir.path(),
            "l.zip",
            &stored_zip(&[(b"sub/b.md", true, false, b"payload")]),
        );
        let out = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), out.path().join("sub")).unwrap();
        let error = extract(&archive, &[], out.path(), Limits::default()).unwrap_err();
        assert!(matches!(refused(error), Refused::UnsafePath(_)));
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    fn build_tar_bytes(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, kind, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(if *kind == tar::EntryType::Directory {
                0
            } else {
                data.len() as u64
            });
            header.set_mode(if *kind == tar::EntryType::Directory {
                0o755
            } else {
                0o644
            });
            header.set_mtime(1_728_432_000); // 2024-10-09 00:00:00 UTC
            if *kind == tar::EntryType::Symlink || *kind == tar::EntryType::Link {
                header.set_size(0);
                builder
                    .append_link(&mut header, *name, std::str::from_utf8(data).unwrap())
                    .unwrap();
                continue;
            }
            // Write path bytes directly into the header when testing unsafe paths
            // like `../evil.txt` or `/etc/passwd` that `Builder::append_data` refuses.
            if name.starts_with('/') || name.contains("..") {
                let raw = name.as_bytes();
                let target = &mut header.as_old_mut().name;
                target[..raw.len()].copy_from_slice(raw);
                header.set_cksum();
                builder.append(&header, *data).unwrap();
            } else {
                header.set_cksum();
                builder.append_data(&mut header, *name, *data).unwrap();
            }
        }
        builder.into_inner().unwrap()
    }

    fn compress_tar(raw_tar: &[u8], format: Format) -> Vec<u8> {
        match format {
            Format::Tar => raw_tar.to_vec(),
            Format::TarGz => {
                let mut enc =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                enc.write_all(raw_tar).unwrap();
                enc.finish().unwrap()
            }
            Format::TarBz2 => {
                let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
                enc.write_all(raw_tar).unwrap();
                enc.finish().unwrap()
            }
            Format::TarXz => {
                let mut enc =
                    lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1))
                        .unwrap();
                enc.write_all(raw_tar).unwrap();
                enc.finish().unwrap()
            }
            Format::TarZst => zstd::stream::encode_all(Cursor::new(raw_tar), 1).unwrap(),
            Format::Zip | Format::SevenZ => unreachable!(),
        }
    }

    fn build_sevenz_bytes(entries: &[(&str, bool, &[u8])]) -> Vec<u8> {
        let mut writer = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        for (name, is_dir, data) in entries {
            if *is_dir {
                let entry = sevenz_rust2::ArchiveEntry::new_directory(name);
                writer
                    .push_archive_entry::<Cursor<&[u8]>>(entry, None)
                    .unwrap();
            } else {
                let mut entry = sevenz_rust2::ArchiveEntry::new_file(name);
                entry.has_last_modified_date = true;
                entry.last_modified_date = sevenz_rust2::NtTime::UNIX_EPOCH;
                writer
                    .push_archive_entry(entry, Some(Cursor::new(*data)))
                    .unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn compound_and_single_archive_extensions_are_detected_cleanly() {
        let cases = [
            ("bundle.zip", Some(Format::Zip)),
            ("dir/BUNDLE.ZIP", Some(Format::Zip)),
            ("backup.tar", Some(Format::Tar)),
            ("release.tar.gz", Some(Format::TarGz)),
            ("release.TAR.GZ", Some(Format::TarGz)),
            ("release.tgz", Some(Format::TarGz)),
            ("release.tar.bz2", Some(Format::TarBz2)),
            ("release.tbz2", Some(Format::TarBz2)),
            ("release.tar.xz", Some(Format::TarXz)),
            ("release.txz", Some(Format::TarXz)),
            ("release.tar.zst", Some(Format::TarZst)),
            ("release.tzst", Some(Format::TarZst)),
            ("bundle.7z", Some(Format::SevenZ)),
            ("bundle.7Z", Some(Format::SevenZ)),
            ("app.log.tar.gz", Some(Format::TarGz)),
            // Standalone compression and non-archives must NOT match.
            ("single.gz", None),
            ("single.bz2", None),
            ("single.xz", None),
            ("single.zst", None),
            ("app.log.gz", None),
            ("notes.tar.gz.md", None),
            (".tar.gz", None),
            (".zip", None),
            ("..tar.gz", None),
            ("tar", None),
        ];
        for (name, expected) in cases {
            assert_eq!(format_for_name(name), expected, "{name}");
            assert_eq!(format_for_path(Path::new(name)), expected, "{name}");
        }
        // Compound `.tar.gz` must not be misclassified as a compressed log.
        assert!(!crate::log::is_log_path(Path::new("app.log.tar.gz")));
        assert!(!crate::log::is_log_path(Path::new("site.tar.gz")));
    }

    #[test]
    fn tar_and_compressed_tar_formats_list_read_and_extract() {
        let raw_tar = build_tar_bytes(&[
            ("docs/", tar::EntryType::Directory, b""),
            ("docs/readme.md", tar::EntryType::Regular, b"# Hello TAR\n"),
            (
                "notes/data.json",
                tar::EntryType::Regular,
                b"{\"ok\":true}\n",
            ),
        ]);
        let formats = [
            ("sample.tar", Format::Tar, true),
            ("sample.tar.gz", Format::TarGz, false),
            ("sample.tgz", Format::TarGz, false),
            ("sample.tar.bz2", Format::TarBz2, false),
            ("sample.tbz2", Format::TarBz2, false),
            ("sample.tar.xz", Format::TarXz, false),
            ("sample.txz", Format::TarXz, false),
            ("sample.tar.zst", Format::TarZst, false),
            ("sample.tzst", Format::TarZst, false),
        ];
        for (filename, format, packed_known) in formats {
            let dir = tempfile::tempdir().unwrap();
            let bytes = compress_tar(&raw_tar, format);
            let archive = write(dir.path(), filename, &bytes);
            let listing = list(&archive, Limits::default()).unwrap();
            assert_eq!((listing.files, listing.directories), (2, 1), "{filename}");
            assert_eq!(listing.entries[0].kind, Kind::Directory, "{filename}");
            assert_eq!(listing.entries[1].name, "docs/readme.md", "{filename}");
            assert_eq!(listing.entries[1].packed_known, packed_known, "{filename}");
            assert_eq!(listing.entries[2].name, "notes/data.json", "{filename}");
            let modified = listing.entries[1].modified.unwrap();
            assert_eq!((modified.year, modified.month, modified.day), (2024, 10, 9));
            assert_eq!(
                read_entry(&archive, 1, Limits::default()).unwrap(),
                b"# Hello TAR\n",
                "{filename}"
            );
            assert_eq!(
                read_entry(&archive, 2, Limits::default()).unwrap(),
                b"{\"ok\":true}\n",
                "{filename}"
            );
            let out = tempfile::tempdir().unwrap();
            let extracted = extract(&archive, &[], out.path(), Limits::default()).unwrap();
            assert_eq!(extracted.files.len(), 2, "{filename}");
            assert_eq!(
                std::fs::read(out.path().join("docs/readme.md")).unwrap(),
                b"# Hello TAR\n",
                "{filename}"
            );
            assert_eq!(
                std::fs::read(out.path().join("notes/data.json")).unwrap(),
                b"{\"ok\":true}\n",
                "{filename}"
            );
        }
    }

    #[test]
    fn sevenz_lists_reads_and_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = build_sevenz_bytes(&[
            ("docs", true, b""),
            ("docs/readme.md", false, b"# Hello 7z\n"),
            ("empty.txt", false, b""),
            ("data.json", false, b"{\"sevenz\":1}\n"),
        ]);
        let archive = write(dir.path(), "bundle.7z", &bytes);
        let listing = list(&archive, Limits::default()).unwrap();
        assert_eq!((listing.files, listing.directories), (3, 1));
        assert_eq!(listing.entries[0].kind, Kind::Directory);
        assert_eq!(listing.entries[1].name, "docs/readme.md");
        assert!(!listing.entries[1].packed_known);
        assert_eq!(
            read_entry(&archive, 1, Limits::default()).unwrap(),
            b"# Hello 7z\n"
        );
        assert_eq!(read_entry(&archive, 2, Limits::default()).unwrap(), b"");
        assert_eq!(
            read_entry(&archive, 3, Limits::default()).unwrap(),
            b"{\"sevenz\":1}\n"
        );
        let out = tempfile::tempdir().unwrap();
        let extracted = extract(&archive, &[], out.path(), Limits::default()).unwrap();
        assert_eq!(extracted.files.len(), 3);
        assert_eq!(
            std::fs::read(out.path().join("docs/readme.md")).unwrap(),
            b"# Hello 7z\n"
        );
        assert_eq!(
            std::fs::read(out.path().join("data.json")).unwrap(),
            b"{\"sevenz\":1}\n"
        );
    }

    #[test]
    fn tar_path_traversal_and_symlinks_are_refused_safely() {
        let dir = tempfile::tempdir().unwrap();
        let raw_tar = build_tar_bytes(&[
            ("good.txt", tar::EntryType::Regular, b"safe"),
            ("../evil.txt", tar::EntryType::Regular, b"pwned"),
            ("/etc/passwd", tar::EntryType::Regular, b"root"),
            ("link.txt", tar::EntryType::Symlink, b"../outside"),
        ]);
        let archive = write(
            dir.path(),
            "hostile.tar.gz",
            &compress_tar(&raw_tar, Format::TarGz),
        );
        let listing = list(&archive, Limits::default()).unwrap();
        assert_eq!(
            listing.entries[0].path.as_deref(),
            Some(Path::new("good.txt"))
        );
        assert_eq!(listing.entries[1].path, None);
        assert_eq!(listing.entries[2].path, None);
        assert_eq!(listing.entries[3].kind, Kind::Symlink);
        assert_eq!(
            refused(read_entry(&archive, 3, Limits::default()).unwrap_err()),
            Refused::Symlink("link.txt".into())
        );
        let out = tempfile::tempdir().unwrap();
        let err = extract(&archive, &[], out.path(), Limits::default()).unwrap_err();
        assert!(matches!(refused(err), Refused::UnsafePath(_)));
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
        // Selecting only safe + symlink extracts the safe file and skips the symlink.
        let done = extract(&archive, &[0, 3], out.path(), Limits::default()).unwrap();
        assert_eq!(done.files, [out.path().join("good.txt")]);
        assert_eq!(done.skipped.len(), 1);
    }

    #[test]
    fn tar_gz_decompression_bomb_and_entry_limit_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let zeros = vec![0u8; 16 << 20];
        let raw_tar = build_tar_bytes(&[("zeros.bin", tar::EntryType::Regular, &zeros)]);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&raw_tar).unwrap();
        let gz_bytes = enc.finish().unwrap();
        assert!(
            (raw_tar.len() as u64) / (gz_bytes.len() as u64).max(1) > 250,
            "fixture must exceed compression ratio limit"
        );
        let bomb = write(dir.path(), "bomb.tar.gz", &gz_bytes);
        assert!(matches!(
            refused(list(&bomb, Limits::default()).unwrap_err()),
            Refused::Bomb(_)
        ));

        let small_tar = build_tar_bytes(&[
            ("a.txt", tar::EntryType::Regular, b"a"),
            ("b.txt", tar::EntryType::Regular, b"b"),
        ]);
        let limited = write(
            dir.path(),
            "two.tar.gz",
            &compress_tar(&small_tar, Format::TarGz),
        );
        let count_limit = Limits {
            entries: 1,
            ..Limits::default()
        };
        assert_eq!(
            refused(list(&limited, count_limit).unwrap_err()),
            Refused::TooManyEntries(2)
        );
    }

    #[test]
    fn malformed_and_truncated_archives_return_errors_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "bad.zip",
            "bad.tar",
            "bad.tar.gz",
            "bad.tgz",
            "bad.tar.bz2",
            "bad.tar.xz",
            "bad.tar.zst",
            "bad.7z",
        ] {
            let garbage = write(dir.path(), name, b"not a valid archive payload");
            assert!(
                list(&garbage, Limits::default()).is_err(),
                "{name} garbage must fail"
            );
            let empty = write(dir.path(), &format!("empty-{name}"), b"");
            assert!(
                list(&empty, Limits::default()).is_err(),
                "empty {name} must fail"
            );
        }
        // Truncated valid `.tar.gz` must fail cleanly.
        let raw_tar =
            build_tar_bytes(&[("hello.txt", tar::EntryType::Regular, &b"hello ".repeat(500))]);
        let gz = compress_tar(&raw_tar, Format::TarGz);
        let truncated = write(dir.path(), "trunc.tar.gz", &gz[..gz.len() / 2]);
        assert!(list(&truncated, Limits::default()).is_err());
    }
}
