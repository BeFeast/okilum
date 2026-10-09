//! Preview-bound, non-replacing moves of ordinary notes within one vault.
use anyhow::{bail, Context, Result};
use rustix::fs::{open, openat, renameat_with, Mode, OFlags, RenameFlags};
use std::{
    ffi::OsString,
    fs::File,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

mod directory;
pub use directory::{DirectoryMovePlan, DirectorySnapshot};

fn parent(root: &Path, relative: &Path) -> Result<(File, OsString)> {
    if !relative
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
    {
        bail!("Use a Markdown (.md) filename");
    }
    parent_any(root, relative)
}

fn parent_any(root: &Path, relative: &Path) -> Result<(File, OsString)> {
    let parts: Vec<_> = relative.components().collect();
    if parts.is_empty() || parts.iter().any(|p| !matches!(p, Component::Normal(_))) {
        bail!("Choose a note inside the open folder");
    }
    let mut dir = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    for part in &parts[..parts.len() - 1] {
        dir = openat(
            &dir,
            part.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .context("Choose an existing real folder, not a symbolic link")?;
    }
    Ok((
        File::from(dir),
        parts.last().unwrap().as_os_str().to_owned(),
    ))
}

fn snapshot(dir: &File, name: &OsString) -> Result<(Vec<u8>, u64, u64)> {
    let mut file = File::from(openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 {
        bail!("Move requires a regular note without hard links or symbolic links");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((bytes, meta.dev(), meta.ino()))
}

pub struct MovePlan {
    source_dir: File,
    source_name: OsString,
    target_dir: File,
    target_name: OsString,
    expected: (Vec<u8>, u64, u64),
}

pub struct Moved {
    /// The rename succeeded; warnings must never be reported as a failed move
    /// that the user should blindly retry. Both paths may be touched by writers.
    pub warning: Option<String>,
}

impl MovePlan {
    pub fn prepare(root: &Path, from: &Path, to: &Path) -> Result<Self> {
        if crate::vault::service_path(from) || crate::vault::service_path(to) {
            bail!("Service files are not note move targets");
        }
        if crate::vault::cloud_placeholder(&root.join(from)) {
            bail!("The moved note is an iCloud placeholder; download it first");
        }
        if from == to {
            bail!("Choose a different name or folder");
        }
        let (source_dir, source_name) = parent(root, from)?;
        let (target_dir, target_name) = parent(root, to)?;
        let expected = snapshot(&source_dir, &source_name)?;
        // Atomic NOREPLACE below is authoritative; this only improves the preview.
        match rustix::fs::statat(
            &target_dir,
            &target_name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => bail!("The destination already exists; it will not be replaced"),
            Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Self {
            source_dir,
            source_name,
            target_dir,
            target_name,
            expected,
        })
    }

    /// Bind a fresh inode (after a rewrite) to the approved source bytes.
    pub fn prepare_exact(root: &Path, from: &Path, to: &Path, bytes: &[u8]) -> Result<Self> {
        let plan = Self::prepare(root, from, to)?;
        if plan.expected.0 != bytes {
            bail!("Source changed before move; recovery is retained");
        }
        Ok(plan)
    }

    pub fn commit(self) -> Result<Moved> {
        self.commit_before_rename(|| {})
    }

    fn commit_before_rename(self, before: impl FnOnce()) -> Result<Moved> {
        if snapshot(&self.source_dir, &self.source_name)
            .context("The source is no longer readable. Nothing was moved; preview again")?
            != self.expected
        {
            bail!("The note changed after preview. Nothing was moved; preview again");
        }
        before();
        renameat_with(&self.source_dir, &self.source_name, &self.target_dir, &self.target_name, RenameFlags::NOREPLACE)
            .context("Nothing was moved: destination exists, source changed, or the filesystem cannot perform a safe move")?;
        let mut warnings = Vec::new();
        match snapshot(&self.target_dir, &self.target_name) {
            Ok(current) if current == self.expected => {},
            Ok(_) => warnings.push("An external writer changed the note during the move. Inspect the destination before editing."),
            Err(_) => warnings.push("The move completed, but the destination is now missing or unreadable. Inspect both folders before continuing."),
        }
        let target_synced = self.target_dir.sync_all();
        let source_synced = self.source_dir.sync_all();
        if target_synced.is_err() || source_synced.is_err() {
            warnings.push("The note was moved, but directory sync failed. Keep the app open and check the destination.");
        }
        Ok(Moved {
            warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn move_preserves_exact_bytes_and_refuses_changed_preview() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Folder")).unwrap();
        let original = "\u{feff}---\r\ntitle: Привет 🧠\r\n---\r\n[[same]]\r\n";
        std::fs::write(root.path().join("old.md"), original).unwrap();
        let plan = MovePlan::prepare(
            root.path(),
            Path::new("old.md"),
            Path::new("Folder/Новое.md"),
        )
        .unwrap();
        assert!(plan.commit().unwrap().warning.is_none());
        assert_eq!(
            std::fs::read(root.path().join("Folder/Новое.md")).unwrap(),
            original.as_bytes()
        );
        assert!(!root.path().join("old.md").exists());
        let plan = MovePlan::prepare(
            root.path(),
            Path::new("Folder/Новое.md"),
            Path::new("new.md"),
        )
        .unwrap();
        std::fs::write(root.path().join("Folder/Новое.md"), "external").unwrap();
        assert!(plan.commit().is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("Folder/Новое.md")).unwrap(),
            "external"
        );
        assert!(!root.path().join("new.md").exists());
    }
    #[test]
    fn collision_race_and_symlinks_never_replace_files() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("old.md");
        let to = root.path().join("new.md");
        std::fs::write(&from, "original").unwrap();
        let plan =
            MovePlan::prepare(root.path(), Path::new("old.md"), Path::new("new.md")).unwrap();
        assert!(plan
            .commit_before_rename(|| std::fs::write(&to, "other writer").unwrap())
            .is_err());
        assert_eq!(std::fs::read_to_string(&from).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "other writer");
        assert!(
            MovePlan::prepare(root.path(), Path::new("old.md"), Path::new("../escape.md")).is_err()
        );
        std::os::unix::fs::symlink(root.path(), root.path().join("link")).unwrap();
        assert!(MovePlan::prepare(
            root.path(),
            Path::new("old.md"),
            Path::new("link/target.md")
        )
        .is_err());
        std::fs::remove_file(&to).unwrap();
        std::os::unix::fs::symlink("absent.md", &to).unwrap();
        assert!(MovePlan::prepare(root.path(), Path::new("old.md"), Path::new("new.md")).is_err());
    }
    #[test]
    fn last_moment_source_replacement_is_retained_and_reported() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("old.md"), "original").unwrap();
        let plan =
            MovePlan::prepare(root.path(), Path::new("old.md"), Path::new("new.md")).unwrap();
        let moved = plan
            .commit_before_rename(|| {
                std::fs::write(root.path().join("replacement.md"), "external").unwrap();
                std::fs::rename(
                    root.path().join("replacement.md"),
                    root.path().join("old.md"),
                )
                .unwrap();
            })
            .unwrap();
        assert!(moved.warning.is_some());
        assert_eq!(
            std::fs::read_to_string(root.path().join("new.md")).unwrap(),
            "external"
        );
    }
}
