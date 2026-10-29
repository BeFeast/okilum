//! Compatibility for filesystems that reject exclusive rename flags (#1915).
//! File publication remains atomic and exclusive via linkat. Directory fallback
//! uses plain rename after a no-follow destination check; POSIX offers no atomic
//! exclusive directory rename here, so an external empty-directory race remains.
use rustix::{
    fd::AsFd,
    fs::{AtFlags, RenameFlags},
    io::{Errno, Result},
};
use std::ffi::OsStr;

#[cfg(test)]
thread_local! { static FLAG_ERROR: std::cell::Cell<Option<Errno>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
pub(crate) fn inject<T>(error: Errno, body: impl FnOnce() -> T) -> T {
    struct Reset(Option<Errno>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FLAG_ERROR.with(|v| v.set(self.0));
        }
    }
    let reset = Reset(FLAG_ERROR.with(|v| v.replace(Some(error))));
    let result = body();
    drop(reset);
    result
}
pub(crate) fn flagged_rename(
    a: impl AsFd,
    b: &OsStr,
    c: impl AsFd,
    d: &OsStr,
    flags: RenameFlags,
) -> Result<()> {
    #[cfg(test)]
    if let Some(error) = FLAG_ERROR.with(|v| v.get()) {
        return Err(error);
    }
    rustix::fs::renameat_with(a, b, c, d, flags)
}

pub fn unsupported(error: Errno) -> bool {
    error == Errno::NOTSUP || error == Errno::INVAL || error == Errno::NOSYS
}

pub fn no_replace(
    source_dir: impl AsFd,
    source: &OsStr,
    target_dir: impl AsFd,
    target: &OsStr,
) -> Result<()> {
    no_replace_with(source_dir, source, target_dir, target, |a, b, c, d| {
        flagged_rename(a, b, c, d, RenameFlags::NOREPLACE)
    })
}
fn no_replace_with(
    source_dir: impl AsFd,
    source: &OsStr,
    target_dir: impl AsFd,
    target: &OsStr,
    rename: impl FnOnce(
        rustix::fd::BorrowedFd<'_>,
        &OsStr,
        rustix::fd::BorrowedFd<'_>,
        &OsStr,
    ) -> Result<()>,
) -> Result<()> {
    let a = source_dir.as_fd();
    let b = target_dir.as_fd();
    match rename(a, source, b, target) {
        Ok(()) => return Ok(()),
        Err(e) if unsupported(e) => {}
        Err(e) => return Err(e),
    }
    let metadata = rustix::fs::statat(a, source, AtFlags::SYMLINK_NOFOLLOW)?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode) == rustix::fs::FileType::Directory {
        match rustix::fs::statat(b, target, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => return Err(Errno::EXIST),
            Err(Errno::NOENT) => {}
            Err(e) => return Err(e),
        }
        // Network-only compatibility: an external empty directory can appear
        // between the check and rename. Never recursively remove a destination.
        return rustix::fs::renameat(a, source, b, target);
    }
    rustix::fs::linkat(a, source, b, target, AtFlags::empty())?;
    // If removal fails, keep both names and report failure; do not roll back by
    // unlinking the target, which an external actor may already have replaced.
    rustix::fs::unlinkat(a, source, AtFlags::empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, fs::File};
    #[test]
    fn unsupported_flags_publish_files_and_directories_without_clobber() {
        for error in [Errno::NOTSUP, Errno::INVAL] {
            for directory in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let fd = File::open(root.path()).unwrap();
                if directory {
                    fs::create_dir(root.path().join("source")).unwrap();
                } else {
                    fs::write(root.path().join("source"), b"exact bytes").unwrap();
                }
                fs::write(root.path().join("target"), b"external").unwrap();
                let run = || {
                    no_replace_with(
                        &fd,
                        OsStr::new("source"),
                        &fd,
                        OsStr::new("target"),
                        |_, _, _, _| Err(error),
                    )
                };
                assert_eq!(run(), Err(Errno::EXIST));
                assert_eq!(fs::read(root.path().join("target")).unwrap(), b"external");
                fs::remove_file(root.path().join("target")).unwrap();
                run().unwrap();
                assert!(!root.path().join("source").exists());
                assert_eq!(root.path().join("target").is_dir(), directory);
                if !directory {
                    assert_eq!(
                        fs::read(root.path().join("target")).unwrap(),
                        b"exact bytes"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod operation_tests {
    use super::*;
    use crate::{
        file_editor::{FileEditor, Save},
        note_files,
        note_move::MovePlan,
    };
    use std::fs;
    #[test]
    fn create_save_move_rename_and_external_conflict_on_unsupported_flags() {
        for error in [Errno::NOTSUP, Errno::INVAL] {
            inject(error, || {
                let root = tempfile::tempdir().unwrap();
                let vault = root.path().join("vault");
                fs::create_dir(&vault).unwrap();
                note_files::create_with_source(&vault, std::path::Path::new("note.md"), b"base")
                    .unwrap();
                assert!(note_files::create(&vault, std::path::Path::new("note.md")).is_err());
                let mut editor =
                    FileEditor::open(&vault.join("note.md"), &root.path().join("state")).unwrap();
                editor.set_text("saved".into()).unwrap();
                assert_eq!(editor.save().unwrap(), Save::Saved);
                assert_eq!(fs::read(vault.join("note.md")).unwrap(), b"saved");
                editor.set_text("draft".into()).unwrap();
                fs::write(vault.join("note.md"), b"external").unwrap();
                assert_eq!(editor.save().unwrap(), Save::Conflict);
                assert_eq!(fs::read(vault.join("note.md")).unwrap(), b"external");
                assert_eq!(editor.text(), "draft");
                drop(editor);
                MovePlan::prepare(
                    &vault,
                    std::path::Path::new("note.md"),
                    std::path::Path::new("renamed.md"),
                )
                .unwrap()
                .commit()
                .unwrap();
                fs::create_dir(vault.join("folder")).unwrap();
                MovePlan::prepare(
                    &vault,
                    std::path::Path::new("renamed.md"),
                    std::path::Path::new("folder/moved.md"),
                )
                .unwrap()
                .commit()
                .unwrap();
                assert_eq!(
                    fs::read(vault.join("folder/moved.md")).unwrap(),
                    b"external"
                );
                assert!(fs::read_dir(&vault).unwrap().all(|e| !e
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".okilum-")));
            });
        }
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    use crate::note_move::{DirectoryMovePlan, DirectorySnapshot};
    use std::{fs, path::Path};
    #[test]
    fn directory_move_and_rename_accept_both_unsupported_errors() {
        for error in [Errno::NOTSUP, Errno::INVAL] {
            inject(error, || {
                let root = tempfile::tempdir().unwrap();
                fs::create_dir(root.path().join("old")).unwrap();
                fs::write(root.path().join("old/note.md"), b"bytes").unwrap();
                let expected = DirectorySnapshot::read(root.path(), Path::new("old")).unwrap();
                DirectoryMovePlan::prepare(
                    root.path(),
                    Path::new("old"),
                    Path::new("new"),
                    &expected,
                )
                .unwrap()
                .commit()
                .unwrap();
                assert_eq!(fs::read(root.path().join("new/note.md")).unwrap(), b"bytes");
                assert!(!root.path().join("old").exists());
            });
        }
    }
}
