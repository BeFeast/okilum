//! ZIP archives in the Reader (#996): list entries, read one entry for preview, and
//! extract to a chosen folder, without trusting the archive.
//!
//! - Paths that would leave the destination (`..`, absolute, NUL) are listed but never
//!   extracted; one such entry refuses the whole extraction.
//! - Each entry is capped by size and by compression ratio before any byte is
//!   inflated, and while inflating it may not exceed its declared size.
//! - Encrypted entries are listed as locked and never read. Symbolic links are listed
//!   and skipped.
//! - Extraction never overwrites: an existing target refuses the whole operation, and a
//!   failed run removes what it wrote.
//! - Names without the UTF-8 flag are decoded as CP437, as the ZIP format specifies.
use anyhow::{Context as _, Result};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use zip::{CompressionMethod, ZipArchive};

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
    // Raw access reads metadata only: no decryption, no inflation.
    let file = archive.by_index_raw(index)?;
    let name = file.name().to_owned();
    let raw = file.name_raw();
    let kind = if file.is_symlink() {
        Kind::Symlink
    } else if file.is_dir() {
        Kind::Directory
    } else {
        Kind::File
    };
    Ok(Entry {
        index,
        cp437_name: std::str::from_utf8(raw).ok() != Some(name.as_str()),
        path: file
            .enclosed_name()
            .filter(|p| !p.as_os_str().is_empty() && !absolute(&name)),
        name,
        kind,
        size: file.size(),
        packed: file.compressed_size(),
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

/// The zip crate makes `/etc/x` relative; an archive that names absolute paths is refused instead.
fn absolute(name: &str) -> bool {
    let bytes = name.as_bytes();
    name.starts_with(['/', '\\'])
        || (bytes.len() > 1 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// List every entry from the central directory.
pub fn list(path: &Path, limits: Limits) -> Result<Listing> {
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
    if entry.size > RATIO_FLOOR && (entry.packed == 0 || entry.size / entry.packed > limits.ratio) {
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
    let mut archive = open(path)?;
    anyhow::ensure!(index < archive.len(), "No entry {index} in the archive");
    let entry = describe(&mut archive, index)?;
    anyhow::ensure!(entry.kind != Kind::Directory, "{} is a folder", entry.name);
    admit(&entry, limits)?;
    let mut data = Vec::with_capacity(entry.size as usize);
    copy(&mut archive, &entry, &mut data)?;
    Ok(data)
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
    let mut archive = open(path)?;
    let written = (|| -> Result<()> {
        for (entry, target) in &plan {
            create_directories(
                destination,
                target,
                entry.kind == Kind::Directory,
                &mut result,
            )?;
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
    })();
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
}
