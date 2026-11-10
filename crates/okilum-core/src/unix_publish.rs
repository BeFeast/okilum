//! Compatibility for filesystems that reject exclusive rename flags (#1915).
//! File publication remains atomic and exclusive via linkat. Directory fallback
//! uses plain rename after a no-follow destination check; POSIX offers no atomic
//! exclusive directory rename here, so an external empty-directory race remains.
use rustix::{
    fd::AsFd,
    fs::{AtFlags, RenameFlags},
    io::{Errno, Result},
};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
};

#[derive(Default)]
struct Capabilities {
    unsupported: u32,
    reported: bool,
}
type Identity = (u64, u64);
type Scope = (Identity, u64);
#[derive(Default)]
struct Cache {
    roots: HashSet<Identity>,
    flags: HashMap<Scope, Capabilities>,
}
// Each open vault has independent capabilities, shared by its folders/editors.
// Include the actual device so a nested mount never inherits local support.
// Separate flags: a server may implement NOREPLACE but reject EXCHANGE.
#[cfg(not(test))]
static CAPABILITIES: std::sync::LazyLock<std::sync::Mutex<Cache>> =
    std::sync::LazyLock::new(Default::default);
#[cfg(test)]
thread_local! {
    static CAPABILITIES: std::cell::RefCell<Cache> = Default::default();
    static FLAG_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn capabilities<T>(body: impl FnOnce(&mut Cache) -> T) -> T {
    #[cfg(not(test))]
    {
        body(&mut CAPABILITIES.lock().unwrap())
    }
    #[cfg(test)]
    {
        CAPABILITIES.with(|cache| body(&mut cache.borrow_mut()))
    }
}
fn identity(fd: impl AsFd) -> Result<Identity> {
    let stat = rustix::fs::fstat(fd)?;
    #[cfg(target_os = "macos")]
    let device = stat.st_dev as u64;
    #[cfg(not(target_os = "macos"))]
    let device = stat.st_dev;
    Ok((device, stat.st_ino))
}
/// Register a vault scope without probing/mutating its contents. The cache is
/// session-only, never operational state or an on-disk capability assumption.
pub(crate) fn register_vault(root: &std::path::Path) -> Result<()> {
    use rustix::fs::{open, Mode, OFlags};
    let fd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let root = identity(&fd)?;
    capabilities(|cache| {
        cache.roots.insert(root);
    });
    Ok(())
}
fn scope(fd: rustix::fd::BorrowedFd<'_>) -> Result<Scope> {
    use rustix::fs::{openat, Mode, OFlags};
    let directory = identity(fd)?;
    let mut current = directory;
    let mut parent = None;
    let root = loop {
        if capabilities(|cache| cache.roots.contains(&current)) {
            break current;
        }
        let next = match openat(
            parent.as_ref().map_or(fd, AsFd::as_fd),
            "..",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(next) => next,
            Err(_) => break directory, // ancestor permissions do not gate publication
        };
        let next_identity = identity(&next)?;
        if next_identity == current {
            break directory;
        } // single-file viewer
        current = next_identity;
        parent = Some(next);
    };
    // Cache capabilities, not mutable ancestry or open descriptors. A moved
    // folder must resolve its new vault, and closing a vault must not leave a
    // global directory handle that prevents unmounting the network drive.
    Ok((root, directory.0))
}

#[cfg(test)]
thread_local! { static FLAG_ERROR: std::cell::Cell<Option<Errno>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
pub(crate) fn inject<T>(error: Errno, body: impl FnOnce() -> T) -> T {
    struct Reset(Option<Errno>, Cache);
    impl Drop for Reset {
        fn drop(&mut self) {
            FLAG_ERROR.with(|v| v.set(self.0));
            capabilities(|cache| *cache = std::mem::take(&mut self.1));
        }
    }
    let reset = Reset(
        FLAG_ERROR.with(|v| v.replace(Some(error))),
        capabilities(std::mem::take),
    );
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
    let a = a.as_fd();
    let c = c.as_fd();
    let scope = scope(a)?;
    if capabilities(|cache| {
        cache
            .flags
            .get(&scope)
            .is_some_and(|v| v.unsupported & flags.bits() != 0)
    }) {
        return Err(Errno::NOTSUP);
    }
    #[cfg(test)]
    FLAG_CALLS.with(|calls| calls.set(calls.get() + 1));
    #[cfg(test)]
    let result = FLAG_ERROR
        .with(|v| v.get())
        .map_or_else(|| rustix::fs::renameat_with(a, b, c, d, flags), Err);
    #[cfg(not(test))]
    let result = rustix::fs::renameat_with(a, b, c, d, flags);
    if result.is_err_and(unsupported) {
        capabilities(|cache| {
            let entry = cache.flags.entry(scope).or_default();
            entry.unsupported |= flags.bits();
            if !entry.reported {
                diagnostic_error!("Okilum: this vault's drive does not support exclusive rename/exchange; using compatible link-based publication (vault {:?}, device {}).", scope.0, scope.1);
                entry.reported = true;
            }
        });
    }
    result
}

/// std exposes dev_t as u64 on every Unix; Darwin's native dev_t is signed
/// i32. Use the same conversion as std, preserving its exact bit/sign mapping.
pub(crate) fn same_identity(stat: &rustix::fs::Stat, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    #[cfg(target_os = "macos")]
    let device = stat.st_dev as u64;
    #[cfg(not(target_os = "macos"))]
    let device = stat.st_dev;
    device == metadata.dev() && stat.st_ino == metadata.ino()
}

pub fn unsupported(error: Errno) -> bool {
    error == Errno::NOTSUP
        || error == Errno::OPNOTSUPP
        || error == Errno::INVAL
        || error == Errno::NOSYS
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
    no_replace_compatible(a, source, b, target)
}

/// Exclusive link-based publication when flag support was already refused.
/// Callers must handle both names remaining if source unlink fails.
pub fn no_replace_compatible(
    source_dir: impl AsFd,
    source: &OsStr,
    target_dir: impl AsFd,
    target: &OsStr,
) -> Result<()> {
    let a = source_dir.as_fd();
    let b = target_dir.as_fd();
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
    fn ordinary_errors_do_not_downgrade_the_vault() {
        for error in [Errno::ACCESS, Errno::IO, Errno::TIMEDOUT] {
            inject(error, || {
                let root = tempfile::tempdir().unwrap();
                register_vault(root.path()).unwrap();
                let fd = File::open(root.path()).unwrap();
                FLAG_CALLS.with(|v| v.set(0));
                for _ in 0..3 {
                    assert_eq!(
                        flagged_rename(
                            &fd,
                            OsStr::new("a"),
                            &fd,
                            OsStr::new("b"),
                            RenameFlags::NOREPLACE
                        ),
                        Err(error)
                    );
                }
                assert_eq!(FLAG_CALLS.with(|v| v.get()), 3);
                capabilities(|cache| assert!(cache.flags.is_empty()));
            });
        }
    }
    #[test]
    fn unsupported_capability_is_shared_across_folders_and_flags_are_independent() {
        inject(Errno::INVAL, || {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join("nested")).unwrap();
            register_vault(root.path()).unwrap();
            let other = tempfile::tempdir().unwrap();
            register_vault(other.path()).unwrap();
            let parent = File::open(root.path()).unwrap();
            let child = File::open(root.path().join("nested")).unwrap();
            FLAG_CALLS.with(|v| v.set(0));
            for fd in [&parent, &child, &parent] {
                assert!(flagged_rename(
                    fd,
                    OsStr::new("a"),
                    fd,
                    OsStr::new("b"),
                    RenameFlags::NOREPLACE
                )
                .is_err());
            }
            assert_eq!(FLAG_CALLS.with(|v| v.get()), 1);
            assert!(flagged_rename(
                &child,
                OsStr::new("a"),
                &child,
                OsStr::new("b"),
                RenameFlags::EXCHANGE
            )
            .is_err());
            assert_eq!(FLAG_CALLS.with(|v| v.get()), 2);
            capabilities(|cache| assert!(cache.flags.values().all(|v| v.reported)));
            fs::rename(root.path().join("nested"), other.path().join("nested")).unwrap();
            assert!(flagged_rename(
                &child,
                OsStr::new("a"),
                &child,
                OsStr::new("b"),
                RenameFlags::NOREPLACE
            )
            .is_err());
            assert_eq!(FLAG_CALLS.with(|v| v.get()), 3);
            let fd = File::open(other.path()).unwrap();
            assert!(flagged_rename(
                &fd,
                OsStr::new("a"),
                &fd,
                OsStr::new("b"),
                RenameFlags::NOREPLACE
            )
            .is_err());
            assert_eq!(FLAG_CALLS.with(|v| v.get()), 3);
            capabilities(|cache| assert_eq!(cache.flags.len(), 2));
        });
    }
    #[test]
    fn unsupported_flags_publish_files_and_directories_without_clobber() {
        for error in [Errno::NOTSUP, Errno::INVAL, Errno::NOSYS, Errno::OPNOTSUPP] {
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
        for error in [Errno::NOTSUP, Errno::INVAL, Errno::NOSYS, Errno::OPNOTSUPP] {
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
        for error in [Errno::NOTSUP, Errno::INVAL, Errno::NOSYS, Errno::OPNOTSUPP] {
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
