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

// macOS RENAME_EXCL permits a case-only rename when both names resolve to
// the same vnode in the same directory on a case-insensitive volume. Keep the
// atomic no-replace operation: a plain rename could overwrite a racing writer.
fn destination_conflicts(
    macos: bool,
    source: (u64, u64),
    target: (u64, u64),
    source_parent: (u64, u64),
    target_parent: (u64, u64),
) -> bool {
    !(macos && source == target && source_parent == target_parent)
}

fn check_destination(
    source_dir: &File,
    source_name: &OsString,
    target_dir: &File,
    target_name: &OsString,
) -> Result<()> {
    match rustix::fs::statat(
        target_dir,
        target_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(target) => {
            let source = rustix::fs::statat(
                source_dir,
                source_name,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )?;
            let source_parent = source_dir.metadata()?;
            let target_parent = target_dir.metadata()?;
            // rustix stat field widths differ on Linux and macOS.
            #[allow(clippy::unnecessary_cast)]
            let conflicts = destination_conflicts(
                cfg!(target_os = "macos"),
                (source.st_dev as u64, source.st_ino as u64),
                (target.st_dev as u64, target.st_ino as u64),
                (source_parent.dev(), source_parent.ino()),
                (target_parent.dev(), target_parent.ino()),
            );
            if conflicts {
                bail!("The destination already exists; it will not be replaced");
            }
        }
        Err(rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
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
        check_destination(&source_dir, &source_name, &target_dir, &target_name)?;
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
    fn case_only_collision_policy_requires_same_entry_and_parent_on_macos() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("test.md"), "original").unwrap();
        std::fs::write(root.path().join("other.md"), "original").unwrap();
        std::fs::create_dir(root.path().join("Folder")).unwrap();
        let identity = |path: &Path| {
            let meta = std::fs::symlink_metadata(path).unwrap();
            (meta.dev(), meta.ino())
        };
        let source = identity(&root.path().join("test.md"));
        let other = identity(&root.path().join("other.md"));
        let parent = identity(root.path());
        let folder = identity(&root.path().join("Folder"));
        // APFS reports the same identity for test.md and Test.md. Exercise
        // that stat result on Linux too; native macOS tests cover the syscall.
        for entry in [source, folder] {
            assert!(
                !destination_conflicts(true, entry, entry, parent, parent),
                "CASE1731: a macOS alias of the same entry is a rename"
            );
            assert!(destination_conflicts(false, entry, entry, parent, parent));
            assert!(destination_conflicts(true, entry, entry, parent, folder));
        }
        assert!(destination_conflicts(true, source, other, parent, parent));
        // The existing file collision is still refused by the real preflight.
        assert!(
            MovePlan::prepare(root.path(), Path::new("test.md"), Path::new("other.md")).is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("other.md")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn case_only_preview_keeps_resolvable_wiki_text_and_undo_restores_bytes() {
        use crate::link_rewrite::{Operation, Preview};
        use std::{collections::BTreeMap, fs};
        for folder in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            let (from, to, source, destination, wiki) = if folder {
                fs::create_dir(root.path().join("Security")).unwrap();
                fs::write(root.path().join("Security/picture.png"), [0, 255, 13]).unwrap();
                (
                    "Security",
                    "security",
                    "Security/test.md",
                    "security/test.md",
                    "Security/test",
                )
            } else {
                ("test.md", "Test.md", "test.md", "Test.md", "test")
            };
            let body = "\u{feff}# Test\r\n";
            let reference = format!("[[{wiki}]]\r\n");
            fs::write(root.path().join(source), body).unwrap();
            fs::write(root.path().join("ref.md"), &reference).unwrap();
            let preview = Preview::prepare(root.path(), from, to).unwrap();
            assert!(
                preview.changes.is_empty(),
                "already-resolving wiki source stays byte-exact"
            );
            assert!(
                preview.skipped.is_empty(),
                "positive link-discovery control"
            );
            let applied = preview
                .apply(root.path(), state.path(), &mut BTreeMap::new())
                .unwrap();
            assert!(
                applied.moved && applied.warning.is_none(),
                "{:?}",
                applied.warning
            );
            assert_eq!(
                fs::read(root.path().join(destination)).unwrap(),
                body.as_bytes()
            );
            assert_eq!(
                fs::read(root.path().join("ref.md")).unwrap(),
                reference.as_bytes()
            );
            assert_eq!(
                crate::vault::Vault::scan(root.path())
                    .unwrap()
                    .resolve_from(wiki, "ref.md")
                    .path(),
                Some(destination)
            );
            if folder {
                assert_eq!(
                    fs::read(root.path().join("security/picture.png")).unwrap(),
                    [0, 255, 13]
                );
            }
            Operation::revert(&applied.journal, state.path()).unwrap();
            assert_eq!(fs::read(root.path().join(source)).unwrap(), body.as_bytes());
            assert_eq!(
                fs::read(root.path().join("ref.md")).unwrap(),
                reference.as_bytes()
            );
            if folder {
                assert_eq!(
                    fs::read(root.path().join("Security/picture.png")).unwrap(),
                    [0, 255, 13]
                );
            }
        }
    }

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

#[cfg(all(test, target_os = "macos"))]
mod case_rename_native_tests;
