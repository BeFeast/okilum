//! System Trash transport and collision-safe, identity-checked session Undo.
use anyhow::{ensure, Context, Result};
use rustix::fs::{open, openat, renameat_with, Mode, OFlags, RenameFlags};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Trashed {
    pub root: PathBuf,
    pub relative: PathBuf,
    location: PathBuf,
    info: Option<PathBuf>,
    identity: (u64, u64),
}

fn parent(root: &Path, relative: &Path) -> Result<(rustix::fd::OwnedFd, std::ffi::OsString)> {
    let parts: Vec<_> = relative.components().collect();
    ensure!(
        !parts.is_empty() && parts.iter().all(|p| matches!(p, Component::Normal(_))),
        "Choose an item inside the vault"
    );
    ensure!(
        !tessera_core::vault::service_path(relative),
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

pub fn move_to_trash(root: &Path, relative: &Path) -> Result<Trashed> {
    let root = root.canonicalize()?;
    let _parent = parent(&root, relative)?;
    let source = root.join(relative);
    let metadata = fs::symlink_metadata(&source)?;
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        "Choose a regular file or folder, not a symbolic link"
    );
    let (location, info) = system_trash(&source)?;
    let metadata = fs::symlink_metadata(&location)
        .context("The item reached Trash, but Undo is unavailable. Restore it from system Trash")?;
    Ok(Trashed {
        root,
        relative: relative.to_owned(),
        location,
        info,
        identity: (metadata.dev(), metadata.ino()),
    })
}

impl Trashed {
    pub fn restore(&self) -> Result<()> {
        let metadata =
            fs::symlink_metadata(&self.location).context("The item is no longer in Trash")?;
        ensure!(
            (metadata.dev(), metadata.ino()) == self.identity,
            "The item in Trash changed; restore it through the system file manager"
        );
        let (destination, name) = parent(&self.root, &self.relative)?;
        let source_parent = open(
            self.location.parent().context("Missing Trash folder")?,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        renameat_with(&source_parent, self.location.file_name().context("Missing Trash name")?, &destination, &name, RenameFlags::NOREPLACE)
            .context("Cannot Undo: the original path is occupied, unavailable, or on a different filesystem. The item remains in system Trash")?;
        if let Some(info) = &self.info {
            let _ = fs::remove_file(info);
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn system_trash(source: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
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
    freedesktop_trash(source, &trash)
}

#[cfg(any(target_os = "linux", test))]
fn freedesktop_trash(source: &Path, trash: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
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
        "tessera-{}-{}",
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
        renameat_with(
            rustix::fs::CWD,
            source,
            rustix::fs::CWD,
            &location,
            RenameFlags::NOREPLACE,
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
fn system_trash(source: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let path = source.to_str().context("Use a UTF-8 path")?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    let mut resulting = None;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
        .map_err(|error| anyhow::anyhow!("Cannot move to system Trash: {error}"))?;
    let location = resulting
        .and_then(|url| url.path())
        .context("The item reached Trash; restore it through Finder")?;
    Ok((PathBuf::from(location.to_string()), None))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        };
        // Keep the original inode alive so the replacement cannot reuse it.
        fs::rename(&location, root.join("kept")).unwrap();
        fs::write(&location, b"different").unwrap();
        assert!(trashed.restore().is_err());
        assert!(!root.join("note.md").exists());
        assert_eq!(fs::read(root.join("kept")).unwrap(), b"original");
    }
}
