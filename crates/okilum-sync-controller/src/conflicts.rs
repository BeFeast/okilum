//! Explicit, bounded inventory of Syncthing and Okilum editor conflict copies. This is
//! filename evidence, not proof of an unresolved conflict or synchronization.
//! Inventory reads names only. Explicit preview/apply reads exact source bytes.
//! No daemon or background scan is started.
use anyhow::Result;
use rustix::fs::{open, openat, Dir, FileType, Mode, OFlags};
use std::{
    ffi::OsStr,
    os::{fd::OwnedFd, unix::ffi::OsStrExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Inventory {
    pub copies: Vec<PathBuf>,
    /// False for inaccessible entries, resource limits or interrupted enumeration.
    /// Even a complete snapshot makes no assertion about later filesystem changes.
    pub complete: bool,
}

pub fn inspect(root: &Path) -> Result<Inventory> {
    inspect_with_limit(root, 100_000)
}
fn inspect_with_limit(root: &Path, limit: usize) -> Result<Inventory> {
    let root = root.canonicalize()?;
    let fd = open(&root, flags(), Mode::empty())?;
    let mut scan = Scan {
        inventory: Inventory {
            copies: vec![],
            complete: true,
        },
        remaining: limit,
        deadline: Instant::now() + Duration::from_secs(2),
    };
    scan.walk(fd, Path::new(""), 0);
    scan.inventory.copies.sort();
    Ok(scan.inventory)
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
struct Scan {
    inventory: Inventory,
    remaining: usize,
    deadline: Instant,
}
impl Scan {
    fn walk(&mut self, fd: OwnedFd, relative: &Path, depth: usize) {
        let Ok(entries) = Dir::read_from(&fd) else {
            self.inventory.complete = false;
            return;
        };
        for entry in entries {
            if self.remaining == 0 || Instant::now() >= self.deadline {
                self.inventory.complete = false;
                return;
            }
            self.remaining -= 1;
            let Ok(entry) = entry else {
                self.inventory.complete = false;
                continue;
            };
            let bytes = entry.file_name().to_bytes();
            if matches!(
                bytes,
                b"." | b".." | b".stversions" | b".stfolder" | b".okilum-index"
            ) {
                continue;
            }
            let path = relative.join(OsStr::from_bytes(bytes));
            match entry.file_type() {
                FileType::Directory if depth < 64 => {
                    // Open relative to the enumerated directory, never through a
                    // potentially replaced ancestor or a directory symlink.
                    match openat(&fd, entry.file_name(), flags(), Mode::empty()) {
                        Ok(child) => self.walk(child, &path, depth + 1),
                        Err(_) => self.inventory.complete = false,
                    }
                }
                FileType::Directory => self.inventory.complete = false,
                FileType::RegularFile if possible_copy(bytes) => self.inventory.copies.push(path),
                FileType::Unknown => self.inventory.complete = false,
                _ => {}
            }
        }
    }
}
fn possible_copy(name: &[u8]) -> bool {
    conflict_marker(name).is_some()
}
fn conflict_marker(name: &[u8]) -> Option<(usize, usize)> {
    [
        b".sync-conflict-".as_slice(),
        b".okilum-conflict-".as_slice(),
    ]
    .into_iter()
    .flat_map(|marker| {
        name.windows(marker.len())
            .enumerate()
            .filter_map(move |(index, part)| {
                if part != marker {
                    return None;
                }
                let suffix = &name[index + marker.len()..];
                let valid = suffix.len() >= 23
                    && suffix[..8].iter().all(u8::is_ascii_digit)
                    && suffix[8] == b'-'
                    && suffix[9..15].iter().all(u8::is_ascii_digit)
                    && suffix[15] == b'-'
                    && suffix[16..23]
                        .iter()
                        .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(b))
                    && (suffix.len() == 23 || suffix[23] == b'.');
                valid.then_some((index, marker.len() + 23))
            })
    })
    .min_by_key(|(index, _)| *index)
}

/// Both filename kinds carry a time and seven-character discriminator. Only
/// Syncthing's discriminator is a device prefix; Okilum's is random.
pub fn canonical_path(copy: &Path) -> Option<PathBuf> {
    let name = copy.file_name()?.as_bytes();
    let (index, length) = conflict_marker(name)?;
    // Syncthing inserts its marker before the extension, including the
    // leading dot of a hidden extensionless file. Legacy editor copies of
    // extensionless notes carried an accidental trailing dot.
    let suffix = &name[index + length..];
    let suffix = if name[index..].starts_with(b".okilum-conflict-") && suffix == b"." {
        b"".as_slice()
    } else {
        suffix
    };
    let original = [&name[..index], suffix].concat();
    if original.is_empty() || matches!(original.as_slice(), b"." | b"..") {
        return None;
    }
    if conflict_marker(&original).is_some() {
        return None;
    }
    Some(copy.with_file_name(OsStr::from_bytes(&original)))
}

/// Shallow selected-note inventory: sibling changes must update the banner even
/// when the canonical note itself is unchanged. No content or daemon IO.
pub fn inspect_note(root: &Path, note: &Path) -> Result<Inventory> {
    let root = root.canonicalize()?;
    let fd = parent(&root, note)?;
    let mut inventory = Inventory {
        copies: vec![],
        complete: true,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    for (count, entry) in Dir::read_from(&fd)?.enumerate() {
        if count >= 100_000 || Instant::now() >= deadline {
            inventory.complete = false;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                inventory.complete = false;
                continue;
            }
        };
        let path = note.with_file_name(OsStr::from_bytes(entry.file_name().to_bytes()));
        if entry.file_type() == FileType::RegularFile
            && canonical_path(&path).as_deref() == Some(note)
        {
            inventory.copies.push(path);
        }
    }
    inventory.copies.sort();
    Ok(inventory)
}

#[cfg(test)]
#[path = "conflicts/tests.rs"]
mod resolution_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_copies_without_changing_files_or_following_links() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let copy = "note.sync-conflict-20261007-123456-ABCDEFG.md";
        std::fs::write(vault.path().join("note.md"), "original").unwrap();
        std::fs::create_dir(vault.path().join("nested")).unwrap();
        std::fs::write(vault.path().join("nested").join(copy), "other version").unwrap();
        std::fs::write(outside.path().join(copy), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), vault.path().join("linked")).unwrap();
        std::fs::create_dir(vault.path().join(".stversions")).unwrap();
        std::fs::write(vault.path().join(".stversions").join(copy), "history").unwrap();
        let found = inspect(vault.path()).unwrap();
        assert!(found.complete);
        assert_eq!(found.copies, [PathBuf::from("nested").join(copy)]);
        assert_eq!(
            std::fs::read_to_string(vault.path().join("note.md")).unwrap(),
            "original"
        );
        assert_eq!(
            std::fs::read_to_string(vault.path().join("nested").join(copy)).unwrap(),
            "other version"
        );
        assert!(!inspect_with_limit(vault.path(), 1).unwrap().complete);
    }
    #[test]
    fn missing_folder_and_similar_names_are_not_clean_conflict_receipts() {
        assert!(inspect(Path::new("/nonexistent-okilum-conflict-fixture")).is_err());
        assert!(possible_copy(b"a.sync-conflict-20261007-123456-ABC2345.md"));
        assert!(possible_copy(b"a.sync-conflict-20261007-123456-ABC2345"));
        assert!(!possible_copy(b"a.sync-conflict-not-a-timestamp.md"));
        assert!(!possible_copy(
            b"a.sync-conflict-20261007-123456-ABC2345extra.md"
        ));
    }
}

/// Explicit decision over the two exact versions shown in a Preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    KeepThis,
    KeepThat,
    KeepBoth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    device: u64,
    inode: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

#[derive(Debug)]
pub struct Version {
    path: PathBuf,
    bytes: Vec<u8>,
    revision: Revision,
    mode: u32,
}
impl Version {
    pub fn modified_seconds(&self) -> i64 {
        self.revision.modified.0
    }
    pub fn revision(&self) -> &Revision {
        &self.revision
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Immutable authorization: paths, identities and bytes cannot be replaced by a
/// UI selection after preview. A new observation requires a new explicit action.
#[derive(Debug)]
pub struct Preview {
    root: PathBuf,
    directory_identity: (u64, u64),
    this: Version,
    that: Version,
}
impl Preview {
    pub fn this(&self) -> &Version {
        &self.this
    }
    pub fn that(&self) -> &Version {
        &self.that
    }
}

#[derive(Debug)]
pub struct Applied {
    pub canonical: PathBuf,
    pub kept_both: Option<PathBuf>,
    /// Durable exact preimages outside the vault/index, written before mutation.
    pub recovery_record: PathBuf,
    /// Displaced originals remain recoverable; never unlink as routine cleanup.
    pub originals: [PathBuf; 2],
}
#[derive(Debug)]
pub enum ApplyOutcome {
    Applied(Applied),
    /// Publication may be partial. Never present this as resolved or unchanged.
    NeedsAttention {
        recovery: Applied,
        reason: String,
    },
}

use anyhow::{ensure, Context};
use rustix::fs::{renameat_with, RenameFlags};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

const MAX_SOURCE: u64 = 8 * 1024 * 1024;
fn identity(fd: &OwnedFd) -> Result<(u64, u64)> {
    let stat = rustix::fs::fstat(fd)?;
    Ok((stat.st_dev, stat.st_ino))
}
fn parent(root: &Path, relative: &Path) -> Result<OwnedFd> {
    use std::path::Component;
    ensure!(relative.file_name().is_some(), "Missing source name");
    let mut fd = open(root, flags(), Mode::empty())?;
    for part in relative.components() {
        let Component::Normal(name) = part else {
            anyhow::bail!("Source must be vault-relative")
        };
        ensure!(
            !matches!(
                name.as_bytes(),
                b".stversions" | b".stfolder" | b".okilum-index"
            ),
            "Reserved source folder"
        );
    }
    for part in relative.parent().unwrap().components() {
        fd = openat(&fd, part.as_os_str(), flags(), Mode::empty())?;
    }
    Ok(fd)
}
fn read_version(directory: &OwnedFd, path: &Path) -> Result<Version> {
    // NONBLOCK prevents a raced FIFO from blocking the UI worker indefinitely.
    let fd = openat(
        directory,
        path.file_name().context("Missing source name")?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut file = File::from(fd);
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.nlink() == 1 && before.len() <= MAX_SOURCE,
        "Conflict preview requires a bounded regular file without hard links"
    );
    let stamp = |m: &std::fs::Metadata| Revision {
        device: m.dev(),
        inode: m.ino(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
    };
    let mut bytes = Vec::new();
    (&mut file).take(MAX_SOURCE + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    ensure!(
        bytes.len() as u64 <= MAX_SOURCE
            && before.len() == after.len()
            && stamp(&before) == stamp(&after),
        "Source changed while reading"
    );
    Ok(Version {
        path: path.to_owned(),
        bytes,
        revision: stamp(&after),
        mode: after.mode() & 0o777,
    })
}

/// Preview only an actual filename pair from the existing inventory convention.
/// This is lossless source, never rendered Reader/MCP input.
pub fn preview(root: &Path, copy: &Path) -> Result<Preview> {
    let name = copy
        .file_name()
        .context("Missing conflict filename")?
        .as_bytes();
    ensure!(possible_copy(name), "Not a conflict-copy filename");
    let this_path = canonical_path(copy)
        .context("Ambiguous conflict filename; inspect the candidate sources explicitly")?;
    let root = root.canonicalize()?;
    ensure!(
        root.to_str().is_some() && this_path.to_str().is_some() && copy.to_str().is_some(),
        "Conflict recovery requires UTF-8 source paths; files have not been changed"
    );
    let directory = parent(&root, copy)?;
    let this = read_version(&directory, &this_path)?;
    let that = read_version(&directory, copy)?;
    ensure!(
        (this.revision.device, this.revision.inode) != (that.revision.device, that.revision.inode),
        "Inputs alias one file"
    );
    Ok(Preview {
        root,
        directory_identity: identity(&directory)?,
        this,
        that,
    })
}

fn check(directory: &OwnedFd, expected: &Version) -> Result<()> {
    let current = read_version(directory, &expected.path)?;
    ensure!(
        current.revision == expected.revision && current.bytes == expected.bytes,
        "Conflict input changed or was replaced; preview again"
    );
    Ok(())
}
fn move_exclusive(fd: &OwnedFd, from: &Path, to: &Path) -> Result<()> {
    renameat_with(
        fd,
        from.file_name().context("Missing source name")?,
        fd,
        to.file_name().context("Missing destination name")?,
        RenameFlags::NOREPLACE,
    )?;
    rustix::fs::fsync(fd)?;
    Ok(())
}
fn publish(fd: &OwnedFd, destination: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let name = format!(".okilum-save-{}", uuid::Uuid::new_v4());
    let stage = Path::new(&name);
    let mut file = File::from(openat(
        fd,
        stage,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?);
    file.write_all(bytes)?;
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    file.sync_all()?;
    // Never replace an external editor's intervening creation. On failure retain
    // the staged proposal as unassigned recovery, alongside the exact journal.
    move_exclusive(fd, stage, destination)
}

/// Apply on a worker, only after an explicit Choice. `recovery_directory` must
/// already exist in durable application state outside the vault and its index.
/// No original is deleted, including on publication/fsync failure.
pub fn apply(preview: &Preview, choice: Choice, recovery_directory: &Path) -> Result<ApplyOutcome> {
    apply_with_hooks(preview, choice, recovery_directory, || {}, || {})
}

fn apply_with_hooks(
    preview: &Preview,
    choice: Choice,
    recovery_directory: &Path,
    before_capture: impl FnOnce(),
    before_publish: impl FnOnce(),
) -> Result<ApplyOutcome> {
    apply_with_recheck_hook(
        preview,
        choice,
        recovery_directory,
        || {},
        before_capture,
        before_publish,
    )
}

fn apply_with_recheck_hook(
    preview: &Preview,
    choice: Choice,
    recovery_directory: &Path,
    before_recheck: impl FnOnce(),
    before_capture: impl FnOnce(),
    before_publish: impl FnOnce(),
) -> Result<ApplyOutcome> {
    let directory = parent(&preview.root, &preview.this.path)?;
    require_local_retention_policy(&preview.root)?;
    ensure!(
        identity(&directory)? == preview.directory_identity,
        "Source directory was replaced"
    );
    check(&directory, &preview.this)?;
    check(&directory, &preview.that)?;
    let state = recovery_directory.canonicalize()?;
    ensure!(
        !state.starts_with(&preview.root),
        "Recovery must be outside the vault/index"
    );
    let state_fd = open(&state, flags(), Mode::empty())?;
    let id = uuid::Uuid::new_v4();
    let record_name = format!("sync-conflict-{id}.json");
    let originals = [
        preview
            .this
            .path
            .with_file_name(format!(".okilum-save-{id}-this")),
        preview
            .that
            .path
            .with_file_name(format!(".okilum-save-{id}-that")),
    ];
    let kept_both = (choice == Choice::KeepBoth).then(|| {
        let stem = preview.this.path.file_stem().unwrap().to_string_lossy();
        let extension = preview
            .this
            .path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        preview
            .this
            .path
            .with_file_name(format!("{stem}-kept-{id}{extension}"))
    });
    let mut receipt = Applied {
        canonical: preview.this.path.clone(),
        kept_both,
        recovery_record: state.join(&record_name),
        originals,
    };
    let record = serde_json::json!({"schema":1, "root":preview.root,
        "this_path":preview.this.path, "that_path":preview.that.path,
        "this_bytes":preview.this.bytes, "that_bytes":preview.that.bytes,
        "this_revision":format!("{:?}", preview.this.revision),
        "that_revision":format!("{:?}", preview.that.revision),
        "originals":receipt.originals, "kept_both":receipt.kept_both,
        "choice":format!("{choice:?}"),
        "directory_identity":preview.directory_identity,
        "this_mode":preview.this.mode, "that_mode":preview.that.mode,
        "created":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros() as u64});
    // Refusals cannot leave a recoverable transaction behind. Recheck after
    // preparing the receipt, before creating any durable recovery record.
    before_recheck();
    check(&directory, &preview.this)?;
    check(&directory, &preview.that)?;
    let mut journal = File::from(openat(
        &state_fd,
        record_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?);
    let persist = (|| -> Result<()> {
        journal.write_all(&serde_json::to_vec(&record)?)?;
        journal.sync_all()?;
        rustix::fs::fsync(&state_fd)?;
        Ok(())
    })();
    if let Err(error) = persist {
        let terminal = recovery::refused(&mut receipt.recovery_record);
        return Err(error).context(format!(
            "Journal refused before source mutation; terminal acknowledgement: {terminal:?}"
        ));
    }
    checkpoint("journal");
    // A failed journal write never authorizes any source mutation. A race
    // after this point is a partial operation, handled by capture validation.
    before_capture();
    let mutation = || -> Result<()> {
        let mut captures = Vec::new();
        for (source, archived) in [
            (&preview.this, &receipt.originals[0]),
            (&preview.that, &receipt.originals[1]),
        ] {
            move_exclusive(&directory, &source.path, archived)?;
            let captured = read_version(&directory, archived)?;
            // rename changes ctime; compare identity, mtime and exact bytes.
            ensure!(
                captured.revision.device == source.revision.device
                    && captured.revision.inode == source.revision.inode
                    && captured.revision.modified == source.revision.modified
                    && captured.bytes == source.bytes,
                "Input changed during capture; originals retained"
            );
            captures.push(captured);
            checkpoint(if captures.len() == 1 {
                "capture-this"
            } else {
                "capture-that"
            });
        }
        before_publish();
        for captured in &captures {
            check(&directory, captured)?;
        }
        // Refuse detached/replaced ancestors before publishing.
        ensure!(
            identity(&parent(&preview.root, &preview.this.path)?)? == preview.directory_identity,
            "Source directory changed during apply"
        );
        let bytes = if choice == Choice::KeepThat {
            &preview.that.bytes
        } else {
            &preview.this.bytes
        };
        publish(&directory, &receipt.canonical, bytes, preview.this.mode)?;
        checkpoint("publish-this");
        if let Some(path) = &receipt.kept_both {
            publish(&directory, path, &preview.that.bytes, preview.that.mode)?;
            checkpoint("publish-both");
        }
        for captured in &captures {
            check(&directory, captured)?;
        }
        ensure!(
            read_version(&directory, &receipt.canonical)?.bytes == *bytes,
            "Published source changed; inspect the retained originals"
        );
        if let Some(path) = &receipt.kept_both {
            ensure!(
                read_version(&directory, path)?.bytes == preview.that.bytes,
                "Kept source changed; inspect the retained originals"
            );
        }
        ensure!(
            matches!(
                rustix::fs::statat(
                    &directory,
                    preview.that.path.file_name().unwrap(),
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW
                ),
                Err(rustix::io::Errno::NOENT)
            ),
            "A new conflict copy appeared; inspect it before resolving again"
        );
        ensure!(
            identity(&parent(&preview.root, &preview.this.path)?)? == preview.directory_identity,
            "Source directory changed after publication"
        );
        Ok(())
    };
    match mutation() {
        Ok(()) => {
            if let Err(error) = recovery::complete(&receipt.recovery_record) {
                // Publication succeeded. Disable path recreation even when the
                // normal acknowledgement fails; retain both preimages in History.
                let terminal = recovery::refused(&mut receipt.recovery_record);
                return Ok(ApplyOutcome::NeedsAttention {
                    recovery: receipt,
                    reason: match terminal {
                        Ok(()) => format!("Published decision retained; completion failed: {error}"),
                        Err(marker) => format!("Completion failed: {error}; cannot acknowledge retained decision: {marker}"),
                    },
                });
            }
            Ok(ApplyOutcome::Applied(receipt))
        }
        Err(error) => {
            // Best effort restoration never overwrites a racing external writer.
            for (archived, source) in receipt
                .originals
                .iter()
                .zip([&preview.this.path, &preview.that.path])
            {
                let _ = move_exclusive(&directory, archived, source);
            }
            Ok(ApplyOutcome::NeedsAttention {
                recovery: receipt,
                reason: error.to_string(),
            })
        }
    }
}

#[cfg(test)]
fn apply_before_capture(
    preview: &Preview,
    choice: Choice,
    recovery_directory: &Path,
    before_capture: impl FnOnce(),
) -> Result<ApplyOutcome> {
    apply_with_hooks(preview, choice, recovery_directory, before_capture, || {})
}

/// Source-exact previous versions for the host's existing History composition.
/// Reading a recovery record grants no authority to overwrite either source.
#[derive(Debug)]
pub struct RetainedVersion {
    pub root: PathBuf,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}
pub fn retained_versions(record: &Path) -> Result<[RetainedVersion; 2]> {
    retained_from_value(record_value(record)?)
}
fn record_value(record: &Path) -> Result<serde_json::Value> {
    let fd = open(
        record,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut file = File::from(fd);
    let metadata = file.metadata()?;
    // JSON arrays use at most four bytes per source byte, plus path metadata.
    const MAX_RECORD: u64 = MAX_SOURCE * 8 + 1024 * 1024;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_RECORD,
        "Invalid conflict recovery record"
    );
    let mut bytes = Vec::new();
    (&mut file).take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECORD,
        "Conflict recovery record is too large"
    );
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(value["schema"] == 1, "Unsupported conflict recovery record");
    Ok(value)
}
fn retained_from_value(mut value: serde_json::Value) -> Result<[RetainedVersion; 2]> {
    let root: PathBuf = serde_json::from_value(value["root"].take())?;
    ensure!(root.is_absolute(), "Invalid recovery root");
    let mut read = |prefix: &str| -> Result<RetainedVersion> {
        let path: PathBuf = serde_json::from_value(value[format!("{prefix}_path")].take())?;
        ensure!(
            path.file_name().is_some()
                && path
                    .components()
                    .all(|p| matches!(p, std::path::Component::Normal(_))),
            "Invalid recovered source path"
        );
        let bytes: Vec<u8> = serde_json::from_value(value[format!("{prefix}_bytes")].take())?;
        ensure!(
            bytes.len() as u64 <= MAX_SOURCE,
            "Recovered source exceeds preview limit"
        );
        Ok(RetainedVersion {
            root: root.clone(),
            path,
            bytes,
        })
    };
    Ok([read("this")?, read("that")?])
}

mod recovery;
pub use recovery::{history, recover_interrupted, recover_startup, History, HistoryEntry};

/// Private durable app-state directory. Its parent is supplied by the shell,
/// never a deletable index directory or a path selected from the vault.
pub fn prepare_recovery(directory: &Path) -> Result<()> {
    crate::private::directory(directory)
}

fn checkpoint(_step: &str) {
    #[cfg(test)]
    if std::env::var("OKILUM_CONFLICT_CRASH_STEP").as_deref() == Ok(_step) {
        // process::exit bypasses destructors and error rollback, like an abrupt
        // process stop between durable steps. No fixture daemon runs here.
        std::process::exit(86);
    }
}

/// Managed enrollment seeds this exclusion on both computers. Existing/external
/// folders must prove it too before a local-only retention name can be created.
fn require_local_retention_policy(root: &Path) -> Result<()> {
    let fd = open(root, flags(), Mode::empty())?;
    match rustix::fs::statat(&fd, ".stfolder", rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let policy = read_version(&fd, Path::new(".stignore"))
        .context("Add .okilum-save-* to this computer's .stignore before resolving conflicts")?;
    let text = std::str::from_utf8(&policy.bytes)?;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        // Syncthing uses first-match precedence. Includes and earlier negations
        // can override a later exclusion; do not guess at their meaning.
        ensure!(!line.starts_with("#include") && !line.contains('!'),
            "Cannot prove local retention policy; put .okilum-save-* before includes/negations in .stignore");
        let pattern = line
            .trim_start_matches("(?i)")
            .trim_start_matches("(?d)")
            .trim_start_matches("(?i)");
        if matches!(pattern, ".okilum-save-*" | "**/.okilum-save-*") {
            return Ok(());
        }
    }
    anyhow::bail!("Add .okilum-save-* to this computer's .stignore before resolving conflicts")
}
