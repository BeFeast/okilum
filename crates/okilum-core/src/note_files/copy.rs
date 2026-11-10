//! Two-phase, create-only binary copies using the native note publication path.
//! Preparation is background work; the owner validates its source lease before
//! publish. Existing destinations, including symlinks, are never replaced.
use anyhow::{Context, Result};
#[cfg(unix)]
use std::fs::File;
use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};

pub struct PreparedCopy {
    parent: PathBuf,
    chosen_parent: PathBuf,
    name: OsString,
    _lock: crate::file_editor::EditorLock,
    #[cfg(unix)]
    folder: File,
    #[cfg(unix)]
    temporary: OsString,
    #[cfg(unix)]
    file: File,
    #[cfg(windows)]
    _folder: crate::windows_files::Directory,
    #[cfg(windows)]
    file: Option<tempfile::NamedTempFile>,
    // Fields drop in declaration order. Release admission after Windows staging
    // cleanup; the explicit Unix Drop implementation also runs before release.
    admission: okilum_write_authority::WriterLease,
}

impl PreparedCopy {
    pub fn prepare(path: &Path, drafts: &Path, bytes: &[u8]) -> Result<Self> {
        let admission = okilum_write_authority::Authority::for_path(path)?.admit()?;
        let chosen_parent = path
            .parent()
            .context("Destination has no folder")?
            .to_path_buf();
        let parent = chosen_parent.canonicalize()?;
        let name = path
            .file_name()
            .context("Destination has no filename")?
            .to_owned();
        anyhow::ensure!(
            Path::new(&name).components().count() == 1,
            "Choose one filename"
        );
        let lock =
            crate::file_editor::FileEditor::reserve_destination(&parent.join(&name), drafts)?;
        // symlink_metadata also rejects dangling links, unlike exists(). The
        // native no-replace operation below is the authoritative race check.
        match std::fs::symlink_metadata(parent.join(&name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(_) => anyhow::bail!("Destination already exists; choose a new filename"),
        }
        #[cfg(unix)]
        {
            use rustix::fs::{open, openat, Mode, OFlags};
            let folder = File::from(open(
                &parent,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
            let temporary: OsString = format!(".okilum-create-{}", uuid::Uuid::new_v4()).into();
            let file = File::from(openat(
                &folder,
                &temporary,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o644),
            )?);
            let mut prepared = Self {
                admission,
                parent,
                chosen_parent,
                name,
                _lock: lock,
                folder,
                temporary,
                file,
            };
            prepared.file.write_all(bytes)?;
            prepared.file.sync_all()?;
            Ok(prepared)
        }
        #[cfg(windows)]
        {
            let folder = crate::windows_files::Directory::open(&parent)?;
            folder.child(&name)?; // native filename/ADS/device-name validation
            let mut file = tempfile::NamedTempFile::new_in(&parent)?;
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            Ok(Self {
                admission,
                parent,
                chosen_parent,
                name,
                _lock: lock,
                _folder: folder,
                file: Some(file),
            })
        }
    }

    /// Call only after validating the owning document and source image. Staging
    /// already flushed the full bytes; this performs only exclusive publication.
    pub fn publish(mut self) -> Result<()> {
        let _admission = self.admission.enter()?;
        anyhow::ensure!(
            self.chosen_parent.canonicalize()? == self.parent,
            "Destination folder changed; choose it again"
        );
        #[cfg(unix)]
        {
            use rustix::fs::{statat, AtFlags};
            let current = std::fs::symlink_metadata(&self.parent)?;
            use std::os::unix::fs::MetadataExt;
            let held = self.folder.metadata()?;
            anyhow::ensure!(
                current.is_dir() && current.dev() == held.dev() && current.ino() == held.ino(),
                "Destination folder changed; choose it again"
            );
            anyhow::ensure!(
                crate::unix_publish::same_identity(
                    &statat(&self.folder, &self.temporary, AtFlags::SYMLINK_NOFOLLOW)?,
                    &self.file.metadata()?
                ),
                "Prepared copy changed"
            );
            crate::unix_publish::no_replace(
                &self.folder,
                &self.temporary,
                &self.folder,
                &self.name,
            )
            .context("Destination already exists or cannot be written; choose a new filename")?;
            self.temporary.clear();
            self.folder.sync_all().context(
                "The copy exists, but its folder could not be synced; inspect it before retrying",
            )?;
        }
        #[cfg(windows)]
        {
            // Directory pins every ancestor and retains the ordinary local
            // NTFS/ReFS gate. Use the same write-through NOREPLACE rename as notes.
            // keep() removes Windows' temporary-file attribute before the
            // native write-through rename. Publication errors retain the full
            // prepared copy and report its location for inspection.
            let (_file, prepared) = self.file.take().unwrap().keep()?;
            crate::windows_files::move_no_replace(&prepared, &self.parent.join(&self.name))
                .with_context(|| format!("Destination already exists or cannot be written; prepared copy retained at {}", prepared.display()))?;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for PreparedCopy {
    fn drop(&mut self) {
        if !self.temporary.is_empty() {
            use rustix::fs::{statat, unlinkat, AtFlags};
            if statat(&self.folder, &self.temporary, AtFlags::SYMLINK_NOFOLLOW)
                .ok()
                .zip(self.file.metadata().ok())
                .is_some_and(|(stat, metadata)| {
                    crate::unix_publish::same_identity(&stat, &metadata)
                })
            {
                let _ = unlinkat(&self.folder, &self.temporary, AtFlags::empty());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_copy_is_exact_cancelled_or_exclusive_and_respects_editor_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.webp");
        let drafts = dir.path().join("drafts");
        let bytes = b"RIFF\0\xffunmodified\r\nEXIF";
        let staged = PreparedCopy::prepare(&path, &drafts, bytes).unwrap();
        assert!(!path.exists());
        drop(staged);
        assert!(!path.exists());
        let staged = PreparedCopy::prepare(&path, &drafts, bytes).unwrap();
        std::fs::write(&path, b"concurrent destination").unwrap();
        assert!(staged.publish().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"concurrent destination");
        assert!(PreparedCopy::prepare(&path, &drafts, bytes).is_err());
        std::fs::remove_file(&path).unwrap();
        let lock = crate::file_editor::FileEditor::reserve_destination(&path, &drafts).unwrap();
        assert!(PreparedCopy::prepare(&path, &drafts, bytes).is_err());
        drop(lock);
        PreparedCopy::prepare(&path, &drafts, bytes)
            .unwrap()
            .publish()
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(!std::fs::read_dir(dir.path()).unwrap().flatten().any(|p| p
            .file_name()
            .to_string_lossy()
            .starts_with(".okilum-create-")));
    }
    #[cfg(unix)]
    #[test]
    fn replaced_destination_folder_or_symlink_cannot_receive_copy() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("chosen");
        std::fs::create_dir(&parent).unwrap();
        let drafts = dir.path().join("drafts");
        let path = parent.join("copy.png");
        let staged = PreparedCopy::prepare(&path, &drafts, b"image").unwrap();
        std::fs::rename(&parent, dir.path().join("old")).unwrap();
        std::fs::create_dir(&parent).unwrap();
        assert!(staged.publish().is_err());
        assert!(!path.exists() && !dir.path().join("old/copy.png").exists());
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"canary").unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(PreparedCopy::prepare(&path, &drafts, b"image").is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"canary");
    }
}
