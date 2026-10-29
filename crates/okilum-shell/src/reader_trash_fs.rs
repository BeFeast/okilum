//! System Trash transport and collision-safe, identity-checked session Undo.
use anyhow::{ensure, Context, Result};
use rustix::fs::{open, openat, Mode, OFlags};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

#[cfg(test)]
thread_local! { static FLAG_ERROR: std::cell::Cell<Option<rustix::io::Errno>> = const { std::cell::Cell::new(None) }; }
fn publish(
    a: impl rustix::fd::AsFd,
    b: &std::ffi::OsStr,
    c: impl rustix::fd::AsFd,
    d: &std::ffi::OsStr,
) -> rustix::io::Result<()> {
    #[cfg(test)]
    if let Some(error) = FLAG_ERROR.with(|v| v.get()) {
        if okilum_core::unix_publish::unsupported(error) {
            return okilum_core::unix_publish::no_replace_compatible(a, b, c, d);
        }
        return Err(error);
    }
    okilum_core::unix_publish::no_replace(a, b, c, d)
}

#[derive(Debug)]
pub struct TrashUnavailable;
impl std::fmt::Display for TrashUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Trash is unavailable on this disk. The note was not deleted.")
    }
}
impl std::error::Error for TrashUnavailable {}

#[derive(Clone, Debug)]
pub struct Trashed {
    pub root: PathBuf,
    pub relative: PathBuf,
    location: PathBuf,
    info: Option<PathBuf>,
    identity: (u64, u64),
    vault_trash: Option<VaultTrash>,
}

#[derive(Clone, Debug)]
struct VaultTrash {
    root_identity: (u64, u64),
    entry: PathBuf,
}

/// Cocoa's feature-unsupported error is the only accepted no-Trash signal.
/// Permission, missing-source, busy and I/O errors never authorize fallback.
#[cfg(any(target_os = "macos", test))]
fn no_volume_trash(domain: &str, code: i64) -> bool {
    domain == "NSCocoaErrorDomain" && code == 3328 // NSFeatureUnsupportedError
}

fn parent(root: &Path, relative: &Path) -> Result<(rustix::fd::OwnedFd, std::ffi::OsString)> {
    let parts: Vec<_> = relative.components().collect();
    ensure!(
        !parts.is_empty() && parts.iter().all(|p| matches!(p, Component::Normal(_))),
        "Choose an item inside the vault"
    );
    ensure!(
        !okilum_core::vault::service_path(relative),
        "Service files cannot be moved to Trash"
    );
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = open(root, flags, Mode::empty())?;
    for part in &parts[..parts.len() - 1] {
        fd = openat(&fd, part.as_os_str(), flags, Mode::empty())
            .context("The original folder changed or is a symbolic link")?;
    }
    Ok((fd, parts.last().unwrap().as_os_str().to_owned()))
}

/// What the confirmation inventory compares: identity, size and mtime.
pub fn stamp(_path: &Path, meta: &fs::Metadata) -> (u64, u64, u64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
    )
}

pub fn move_to_trash(root: &Path, relative: &Path) -> Result<Trashed> {
    let root = root.canonicalize()?;
    let (source_parent, source_name) = parent(&root, relative)?;
    let source = root.join(relative);
    let metadata = fs::symlink_metadata(&source)?;
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        "Choose a regular file or folder, not a symbolic link"
    );
    move_with_system(&root, relative, &source_parent, &source_name, || {
        system_trash(&source, &source_parent, &source_name)
    })
}

fn move_with_system(
    root: &Path,
    relative: &Path,
    source_parent: &rustix::fd::OwnedFd,
    source_name: &std::ffi::OsStr,
    system: impl FnOnce() -> Result<(PathBuf, Option<PathBuf>)>,
) -> Result<Trashed> {
    let (location, info, vault_trash) = match system() {
        Ok((location, info)) => (location, info, None),
        Err(error) if error.is::<TrashUnavailable>() => {
            let (location, saved) = vault_trash_at(root, relative, source_parent, source_name)?;
            (location, None, Some(saved))
        }
        Err(error) => return Err(error),
    };
    let metadata = fs::symlink_metadata(&location)
        .context("The item reached Trash, but Undo is unavailable. Restore it from system Trash")?;
    Ok(Trashed {
        root: root.to_owned(),
        relative: relative.to_owned(),
        location,
        info,
        identity: (metadata.dev(), metadata.ino()),
        vault_trash,
    })
}

impl Trashed {
    #[cfg(test)]
    pub(super) fn test_move(root: &Path, relative: &Path, trash: &Path) -> Self {
        let (location, info) = freedesktop_trash(&root.join(relative), trash).unwrap();
        let meta = fs::metadata(&location).unwrap();
        Self {
            root: root.to_owned(),
            relative: relative.to_owned(),
            location,
            info,
            identity: (meta.dev(), meta.ino()),
            vault_trash: None,
        }
    }

    pub fn in_vault_trash(&self) -> bool {
        self.vault_trash.is_some()
    }

    pub fn restore(&self) -> Result<()> {
        let metadata =
            fs::symlink_metadata(&self.location).context("The item is no longer in Trash")?;
        ensure!(
            (metadata.dev(), metadata.ino()) == self.identity,
            "The item in Trash changed; restore it through the system file manager"
        );
        let (destination, name) = parent(&self.root, &self.relative)?;
        let source_parent = if let Some(saved) = &self.vault_trash {
            let root = open(&self.root, directory_flags(), Mode::empty())?;
            let metadata = std::fs::File::from(root.try_clone()?).metadata()?;
            ensure!(
                (metadata.dev(), metadata.ino()) == saved.root_identity,
                "Vault root replaced; Trash retained"
            );
            let relative = self.location.strip_prefix(&self.root)?;
            ensure!(
                relative.starts_with(&saved.entry),
                "Vault Trash entry changed"
            );
            open_components(
                root,
                relative.parent().context("Missing vault Trash parent")?,
            )?
        } else {
            open(
                self.location.parent().context("Missing Trash folder")?,
                trash_parent_flags(),
                Mode::empty(),
            )
            .context("Cannot open the Trash folder for Undo; the item remains in system Trash")?
        };
        let pinned = rustix::fs::statat(
            &source_parent,
            self.location.file_name().context("Missing Trash name")?,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )?;
        #[cfg(target_os = "macos")]
        let device = pinned.st_dev as u64;
        #[cfg(not(target_os = "macos"))]
        let device = pinned.st_dev;
        ensure!(
            (device, pinned.st_ino) == self.identity,
            "Trash source changed before Undo; retained"
        );
        publish(&source_parent, self.location.file_name().context("Missing Trash name")?, &destination, &name)
            .context("Cannot Undo: the original path is occupied, unavailable, or on a different filesystem. The item remains in system Trash")?;
        if let Some(info) = &self.info {
            let _ = fs::remove_file(info);
        }
        Ok(())
    }
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn open_components(mut directory: rustix::fd::OwnedFd, path: &Path) -> Result<rustix::fd::OwnedFd> {
    for part in path.components() {
        ensure!(matches!(part, Component::Normal(_)), "Invalid Trash path");
        directory = openat(
            &directory,
            part.as_os_str(),
            directory_flags(),
            Mode::empty(),
        )?;
    }
    Ok(directory)
}
fn vault_trash_at(
    root: &Path,
    relative: &Path,
    source_parent: &rustix::fd::OwnedFd,
    source_name: &std::ffi::OsStr,
) -> Result<(PathBuf, VaultTrash)> {
    use rustix::fs::{mkdirat, AtFlags};
    let root_fd = open(root, directory_flags(), Mode::empty())?;
    let metadata = std::fs::File::from(root_fd.try_clone()?).metadata()?;
    match mkdirat(&root_fd, ".trash", Mode::RUSR | Mode::WUSR | Mode::XUSR) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let trash = openat(&root_fd, ".trash", directory_flags(), Mode::empty())?;
    // Each deletion owns a new entry. Nested original paths remain intact,
    // and repeating deletion can never overwrite an older recoverable item.
    let id = uuid::Uuid::new_v4().to_string();
    mkdirat(&trash, id.as_str(), Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
    let entry = Path::new(".trash").join(&id);
    let mut destination = openat(&trash, id.as_str(), directory_flags(), Mode::empty())?;
    let mut ancestors = vec![destination.try_clone()?];
    for part in relative
        .parent()
        .context("Missing relative parent")?
        .components()
    {
        ensure!(matches!(part, Component::Normal(_)), "Invalid Trash target");
        mkdirat(
            &destination,
            part.as_os_str(),
            Mode::RUSR | Mode::WUSR | Mode::XUSR,
        )?;
        destination = openat(
            &destination,
            part.as_os_str(),
            directory_flags(),
            Mode::empty(),
        )?;
    }
    let source = rustix::fs::statat(source_parent, source_name, AtFlags::SYMLINK_NOFOLLOW)?;
    publish(
        source_parent,
        source_name,
        &destination,
        relative.file_name().context("Missing Trash target")?,
    )?;
    let moved = rustix::fs::statat(
        &destination,
        relative.file_name().unwrap(),
        AtFlags::SYMLINK_NOFOLLOW,
    )?;
    ensure!(
        source.st_dev == moved.st_dev && source.st_ino == moved.st_ino,
        "Trash target changed; inspect vault .trash before continuing"
    );
    for fd in [&destination, source_parent, &trash, &root_fd] {
        std::fs::File::from(fd.try_clone()?).sync_all()?;
    }
    let location = root.join(&entry).join(relative);
    Ok((
        location,
        VaultTrash {
            root_identity: (metadata.dev(), metadata.ino()),
            entry,
        },
    ))
}

fn trash_parent_flags() -> OFlags {
    // macOS protects directory reads of the user's Trash. Undo only needs a
    // directory descriptor for renameatx_np, so request search access instead
    // of permission to enumerate Trash. Keep the no-symlink and no-replace
    // guards; no Full Disk Access or copy/unlink fallback is needed.
    #[cfg(target_os = "macos")]
    let access = OFlags::from_bits_retain(libc::O_SEARCH as u32);
    #[cfg(not(target_os = "macos"))]
    let access = OFlags::RDONLY;
    access | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

#[cfg(target_os = "linux")]
fn system_trash(
    source: &Path,
    source_parent: &rustix::fd::OwnedFd,
    source_name: &std::ffi::OsStr,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("No system Trash directory is available")?;
    let source_dev = fs::symlink_metadata(source)?.dev();
    fs::create_dir_all(&data)?;
    let trash = if fs::metadata(&data)?.dev() == source_dev {
        data.join("Trash")
    } else {
        // The Trash specification permits a private per-user directory at the
        // filesystem root. Never copy/unlink across volumes as a fallback.
        let mut mount = source
            .parent()
            .context("Missing source folder")?
            .to_path_buf();
        while let Some(parent) = mount.parent() {
            if fs::metadata(parent)?.dev() != source_dev {
                break;
            }
            mount = parent.to_path_buf();
        }
        mount.join(format!(".Trash-{}", rustix::process::getuid().as_raw()))
    };
    freedesktop_trash_at(source, &trash, source_parent, source_name)
}

#[cfg(test)]
fn freedesktop_trash(source: &Path, trash: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let source_parent = open(
        source.parent().context("Missing parent")?,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    freedesktop_trash_at(
        source,
        trash,
        &source_parent,
        source.file_name().context("Missing filename")?,
    )
}

#[cfg(any(target_os = "linux", test))]
fn freedesktop_trash_at(
    source: &Path,
    trash: &Path,
    source_parent: &rustix::fd::OwnedFd,
    source_name: &std::ffi::OsStr,
) -> Result<(PathBuf, Option<PathBuf>)> {
    use std::io::Write;
    use std::os::unix::fs::DirBuilderExt;
    let files = trash.join("files");
    let info = trash.join("info");
    for dir in [&files, &info] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    #[cfg(target_os = "linux")]
    for dir in [trash, &files, &info] {
        let metadata = fs::symlink_metadata(dir)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == rustix::process::getuid().as_raw(),
            "System Trash must be a real directory owned by the current user"
        );
    }
    let name = format!(
        "okilum-{}-{}",
        uuid::Uuid::new_v4(),
        source
            .file_name()
            .context("Missing filename")?
            .to_string_lossy()
    );
    let location = files.join(&name);
    let info = info.join(format!("{name}.trashinfo"));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&info)?;
    let result = (|| -> Result<()> {
        // freedesktop.org Trash specification: absolute, percent-encoded path.
        let encoded =
            url::Url::from_file_path(source).map_err(|_| anyhow::anyhow!("Invalid source path"))?;
        let now =
            time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
        writeln!(
            file,
            "[Trash Info]\nPath={}\nDeletionDate={:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            encoded.path(),
            now.year(),
            u8::from(now.month()),
            now.day(),
            now.hour(),
            now.minute(),
            now.second()
        )?;
        file.sync_all()?;
        let destination = open(
            &files,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        publish(
            source_parent,
            source_name,
            &destination,
            std::ffi::OsStr::new(&name),
        )
        .context("Cannot move to system Trash on this filesystem; the original item was kept")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&info);
    }
    result?;
    Ok((location, Some(info)))
}

#[cfg(target_os = "macos")]
fn system_trash(
    source: &Path,
    source_parent: &rustix::fd::OwnedFd,
    source_name: &std::ffi::OsStr,
) -> Result<(PathBuf, Option<PathBuf>)> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let path = source.to_str().context("Use a UTF-8 path")?;
    // Cocoa's file-reference URL follows the verified object rather than a
    // subsequently replaced pathname, while retaining native Trash metadata.
    let object = openat(
        source_parent,
        source_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let object_file = std::fs::File::from(object);
    let object = object_file.metadata()?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(path))
        .fileReferenceURL()
        .context("Cannot identify this item for system Trash; the original was kept")?;
    let resolved = url
        .filePathURL()
        .and_then(|url| url.path())
        .context("The source moved before Trash")?;
    let actual = fs::symlink_metadata(PathBuf::from(resolved.to_string()))?;
    ensure!(
        (actual.dev(), actual.ino()) == (object.dev(), object.ino()),
        "The source changed before Trash; try again"
    );
    let mut resulting = None;
    if let Err(error) = NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
    {
        // No permanent-delete or vault-local fallback (#1924). Only claim the
        // note was kept when the original identity/revision remains at source.
        let retained = fs::symlink_metadata(source)?;
        ensure!(
            stamp(source, &retained) == stamp(source, &object),
            "Trash failed and the source changed; inspect it before continuing: {error}"
        );
        let domain = error.domain().to_string();
        let code = error.code() as i64;
        eprintln!("Native Trash refused domain={domain} code={code}: {error}");
        if no_volume_trash(&domain, code) {
            return Err(TrashUnavailable.into());
        }
        anyhow::bail!("Cannot move to system Trash; the note was kept: {error}");
    }
    let location = resulting
        .and_then(|url| url.path())
        .context("The item reached Trash; restore it through Finder")?;
    Ok((PathBuf::from(location.to_string()), None))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_flags_trash_and_restore_files_and_folders_without_clobber() {
        for error in [rustix::io::Errno::NOTSUP, rustix::io::Errno::INVAL] {
            for folder in [false, true] {
                struct Reset;
                impl Drop for Reset {
                    fn drop(&mut self) {
                        FLAG_ERROR.with(|v| v.set(None));
                    }
                }
                FLAG_ERROR.with(|v| v.set(Some(error)));
                let _reset = Reset;
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().join("vault");
                fs::create_dir(&root).unwrap();
                let name = if folder { "Folder" } else { "note.md" };
                let source = root.join(name);
                if folder {
                    fs::create_dir(&source).unwrap();
                    fs::write(source.join("note.md"), b"exact").unwrap();
                } else {
                    fs::write(&source, b"exact").unwrap();
                }
                let trashed =
                    Trashed::test_move(&root, Path::new(name), &temp.path().join("Trash"));
                assert!(!source.exists());
                fs::write(&source, b"external").unwrap();
                assert!(trashed.restore().is_err());
                assert_eq!(fs::read(&source).unwrap(), b"external");
                fs::remove_file(&source).unwrap();
                trashed.restore().unwrap();
                let note = if folder {
                    source.join("note.md")
                } else {
                    source
                };
                assert_eq!(fs::read(note).unwrap(), b"exact");
                assert!(!trashed.location.exists());
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn undo_needs_search_but_not_read_access_to_the_trash_parent() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir_all(root.join("Folder/Nested")).unwrap();
        fs::write(root.join("Folder/Nested/Note.md"), b"original\r\n").unwrap();
        fs::write(root.join("Folder/image.bin"), [0, 255, 42]).unwrap();
        let trashed = Trashed::test_move(&root, Path::new("Folder"), &temp.path().join("Trash"));
        let trash_parent = trashed.location.parent().unwrap();
        fs::set_permissions(trash_parent, fs::Permissions::from_mode(0o300)).unwrap();
        // Positive control: the old read-open requires a permission that Undo
        // must not need. Restore permissions before asserting, even on failure.
        let read_open = open(
            trash_parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        );
        let restored = trashed.restore();
        fs::set_permissions(trash_parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            read_open.is_err(),
            "positive control: directory read denied"
        );
        restored.unwrap();
        assert_eq!(
            fs::read(root.join("Folder/Nested/Note.md")).unwrap(),
            b"original\r\n"
        );
        assert_eq!(
            fs::read(root.join("Folder/image.bin")).unwrap(),
            [0, 255, 42]
        );
        assert!(!trashed.location.exists());
        assert!(!trashed.info.unwrap().exists());
    }

    #[test]
    fn trash_and_undo_preserve_bytes_and_never_replace_a_new_destination() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir(&root).unwrap();
        let relative = PathBuf::from("Заметка 🧠.md");
        let source = root.join(&relative);
        let bytes = b"\xef\xbb\xbf# original\r\n";
        fs::write(&source, bytes).unwrap();
        let (location, info) = freedesktop_trash(&source, &temp.path().join("Trash")).unwrap();
        let meta = fs::metadata(&location).unwrap();
        let trashed = Trashed {
            root,
            relative,
            location,
            info,
            identity: (meta.dev(), meta.ino()),
            vault_trash: None,
        };
        assert!(!source.exists());
        fs::write(&source, b"new").unwrap();
        assert!(trashed.restore().is_err());
        assert_eq!(fs::read(&source).unwrap(), b"new");
        assert_eq!(fs::read(&trashed.location).unwrap(), bytes);
        fs::remove_file(&source).unwrap();
        trashed.restore().unwrap();
        assert_eq!(fs::read(source).unwrap(), bytes);
    }
    #[test]
    fn folder_undo_keeps_nested_files_and_rejects_replaced_parent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir_all(root.join("Parent/Folder/nested")).unwrap();
        fs::write(root.join("Parent/Folder/nested/note.md"), b"source").unwrap();
        let relative = PathBuf::from("Parent/Folder");
        let (location, info) =
            freedesktop_trash(&root.join(&relative), &temp.path().join("Trash")).unwrap();
        let meta = fs::metadata(&location).unwrap();
        let trashed = Trashed {
            root: root.clone(),
            relative,
            location,
            info,
            identity: (meta.dev(), meta.ino()),
            vault_trash: None,
        };
        fs::remove_dir(root.join("Parent")).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("Parent")).unwrap();
        assert!(trashed.restore().is_err());
        assert!(!outside.join("Folder").exists());
        fs::remove_file(root.join("Parent")).unwrap();
        fs::create_dir(root.join("Parent")).unwrap();
        trashed.restore().unwrap();
        assert_eq!(
            fs::read(root.join("Parent/Folder/nested/note.md")).unwrap(),
            b"source"
        );
    }

    #[test]
    fn invalid_targets_and_replaced_trash_identity_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for path in ["", "..", "../note.md", ".obsidian/config", "._note.md"] {
            assert!(parent(root, Path::new(path)).is_err(), "{path}");
        }
        fs::write(root.join("note.md"), b"original").unwrap();
        let (location, info) =
            freedesktop_trash(&root.join("note.md"), &root.join("Trash")).unwrap();
        let meta = fs::metadata(&location).unwrap();
        let trashed = Trashed {
            root: root.to_owned(),
            relative: "note.md".into(),
            location: location.clone(),
            info,
            identity: (meta.dev(), meta.ino()),
            vault_trash: None,
        };
        // Keep the original inode alive so the replacement cannot reuse it.
        fs::rename(&location, root.join("kept")).unwrap();
        fs::write(&location, b"different").unwrap();
        assert!(trashed.restore().is_err());
        assert!(!root.join("note.md").exists());
        assert_eq!(fs::read(root.join("kept")).unwrap(), b"original");
    }
    #[test]
    fn trash_move_uses_the_verified_parent_after_path_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        fs::create_dir_all(root.join("Parent")).unwrap();
        fs::write(root.join("Parent/note.md"), b"inside").unwrap();
        let (fd, name) = parent(&root, Path::new("Parent/note.md")).unwrap();
        fs::rename(root.join("Parent"), root.join("Original")).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("note.md"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("Parent")).unwrap();
        let (location, _) = freedesktop_trash_at(
            &root.join("Parent/note.md"),
            &temp.path().join("Trash"),
            &fd,
            &name,
        )
        .unwrap();
        assert_eq!(fs::read(location).unwrap(), b"inside");
        assert_eq!(fs::read(outside.join("note.md")).unwrap(), b"outside");
        assert!(!root.join("Original/note.md").exists());
    }
}

#[cfg(test)]
mod vault_fallback_tests {
    use super::*;
    #[test]
    fn no_trash_is_specific_not_permission_or_io() {
        assert!(no_volume_trash("NSCocoaErrorDomain", 3328));
        for code in [4, 13, 260, 512, 513, 640] {
            assert!(!no_volume_trash("NSCocoaErrorDomain", code));
        }
        assert!(!no_volume_trash("NSPOSIXErrorDomain", 3328));
    }
    fn injected(root: &Path, relative: &Path, missing_trash: bool) -> Result<Trashed> {
        let (fd, name) = parent(root, relative)?;
        move_with_system(root, relative, &fd, &name, || {
            if missing_trash {
                Err(TrashUnavailable.into())
            } else {
                anyhow::bail!("permission denied")
            }
        })
    }
    #[test]
    fn fallback_preserves_path_bytes_collisions_and_undo_redo_for_files_and_folders() {
        for folder in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let relative = Path::new(if folder {
                "Parent/Folder"
            } else {
                "Parent/note.md"
            });
            fs::create_dir_all(root.join("Parent")).unwrap();
            let source = root.join(relative);
            if folder {
                fs::create_dir(&source).unwrap();
                fs::write(source.join("note.md"), b"original\r\n").unwrap();
            } else {
                fs::write(&source, b"original\r\n").unwrap();
            }
            assert!(injected(&root, relative, false).is_err());
            assert!(source.exists());
            assert!(
                !root.join(".trash").exists(),
                "permission error created fallback"
            );
            let first = injected(&root, relative, true).unwrap();
            assert!(first.in_vault_trash());
            assert!(first.location.ends_with(relative));
            assert!(!source.exists());
            fs::write(&source, b"external").unwrap();
            assert!(first.restore().is_err());
            assert_eq!(fs::read(&source).unwrap(), b"external");
            fs::remove_file(&source).unwrap();
            first.restore().unwrap();
            let second = injected(&root, relative, true).unwrap();
            assert_ne!(
                first.location, second.location,
                "Redo reused old Trash entry"
            );
            second.restore().unwrap();
            let note = if folder {
                source.join("note.md")
            } else {
                source
            };
            assert_eq!(fs::read(note).unwrap(), b"original\r\n");
        }
    }
    #[test]
    fn symlinked_trash_and_replaced_root_or_entry_cannot_redirect_moves() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let outside = temp.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(root.join("note.md"), b"inside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".trash")).unwrap();
        assert!(injected(&root, Path::new("note.md"), true).is_err());
        assert_eq!(fs::read(root.join("note.md")).unwrap(), b"inside");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        fs::remove_file(root.join(".trash")).unwrap();
        let saved = injected(&root, Path::new("note.md"), true).unwrap();
        let old = temp.path().join("old-vault");
        fs::rename(&root, &old).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(saved.restore().is_err());
        assert!(!root.join("note.md").exists());
        assert_eq!(
            fs::read(old.join(saved.location.strip_prefix(&root).unwrap())).unwrap(),
            b"inside"
        );
    }
}

#[cfg(test)]
mod vault_trash_inventory_tests {
    use super::*;
    #[test]
    fn vault_trash_stays_out_of_tree_and_search_with_positive_control() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("vault");
        fs::create_dir(&root)?;
        fs::write(root.join("visible.md"), b"visiblecontrol")?;
        fs::write(root.join("removed.md"), b"deletedcontrol")?;
        let (fd, name) = parent(&root, Path::new("removed.md"))?;
        let saved = move_with_system(&root, Path::new("removed.md"), &fd, &name, || {
            Err(TrashUnavailable.into())
        })?;
        assert_eq!(fs::read(&saved.location)?, b"deletedcontrol");
        let vault = okilum_core::Vault::scan(&root)?;
        assert!(vault.entries.iter().any(|e| e.path == "visible.md"));
        assert!(vault.entries.iter().all(|e| !e.path.starts_with(".trash")));
        assert!(vault.notes.iter().all(|e| !e.path.starts_with(".trash")));
        let search = okilum_core::search::Searcher::build(&vault, &temp.path().join("index"))?;
        assert!(!search.search("visiblecontrol", 10)?.is_empty());
        assert!(search.search("deletedcontrol", 10)?.is_empty());
        saved.restore()?;
        assert_eq!(fs::read(root.join("removed.md"))?, b"deletedcontrol");
        Ok(())
    }
}
