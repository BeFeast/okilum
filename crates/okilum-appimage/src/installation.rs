//! Per-user AppImage location. Never writes system paths or replaces foreign files.
use anyhow::{ensure, Context, Result};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
const OWNER: &[u8] = b"Okilum managed AppImage v1\n";

pub fn paths(home: &Path) -> Result<(PathBuf, PathBuf)> {
    ensure!(home.is_absolute(), "Installation home must be absolute");
    Ok((
        home.join(".local/share/okilum/Okilum.AppImage"),
        home.join(".local/bin/okilum"),
    ))
}
fn regular(path: &Path) -> Result<fs::Metadata> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() && m.uid() == unsafe { libc::geteuid() } && m.nlink() == 1,
        "Refusing non-regular, linked or foreign AppImage: {}",
        path.display()
    );
    Ok(m)
}
fn marker(image: &Path) -> PathBuf {
    image.with_file_name(".appimage-owner")
}
/// Build of the bytes at the managed path, recorded by the process that put
/// them there or ran from there (#2455). Absent or unreadable means unknown.
fn build_record(image: &Path) -> PathBuf {
    image.with_file_name(".appimage-build")
}
fn recorded_build(image: &Path) -> Option<u64> {
    regular(&build_record(image)).ok()?;
    fs::read_to_string(build_record(image))
        .ok()?
        .trim()
        .parse()
        .ok()
}
fn write_build(image: &Path, build: u64) -> Result<()> {
    let mut record = tempfile::NamedTempFile::new_in(image.parent().context("Missing parent")?)?;
    record.write_all(build.to_string().as_bytes())?;
    record.as_file().sync_all()?;
    record.persist(build_record(image))?;
    Ok(())
}

/// Before an update helper replaces `image`, record the incoming build when
/// `image` is our managed install. If the apply fails, the record overstates
/// the bytes, which only stops an older external copy from replacing them
/// (never a downgrade); the next managed launch records the real build.
pub(crate) fn note_incoming(image: &Path, build: u64) -> Result<()> {
    if image.file_name() != Some(std::ffi::OsStr::new("Okilum.AppImage")) || owned(image).is_err() {
        return Ok(());
    }
    if recorded_build(image).is_none_or(|current| build > current) {
        write_build(image, build)?;
    }
    Ok(())
}

/// Record the build now running from `running` when it is our managed image
/// (an applied update, or an ordinary managed launch). Anything else is ignored.
pub fn record_running(home: &Path, running: &Path, build: u64) -> Result<()> {
    let (image, _) = paths(home)?;
    if running != image || image.symlink_metadata().is_err() {
        return Ok(());
    }
    regular(&image)?;
    owned(&image)?;
    if recorded_build(&image) != Some(build) {
        write_build(&image, build)?;
    }
    Ok(())
}

fn owned(image: &Path) -> Result<()> {
    regular(&marker(image))?;
    ensure!(
        fs::read(marker(image))? == OWNER,
        "Unowned AppImage installation"
    );
    Ok(())
}

/// Publish bytes before changing the launchers; keep arbitrary downloaded copies.
/// Only the exact legacy ~/Applications/Okilum.AppImage is removed after success.
/// `build` is the build of `source` (the running process). An external copy
/// never replaces a managed install already at the same or a newer build: a
/// launcher pinned to an old download must not undo self-updates (#2455).
pub fn install(home: &Path, source: &Path, build: u64) -> Result<PathBuf> {
    ensure!(source.is_absolute(), "AppImage source must be absolute");
    let metadata = regular(source)?;
    ensure!(
        metadata.permissions().mode() & 0o111 != 0,
        "AppImage is not executable"
    );
    let (image, command) = paths(home)?;
    // A symlinked install directory could redirect writes outside the user's
    // named installation. Shared XDG parent directories may themselves be user
    // configured, but the owned leaf must be an actual directory.
    if let Ok(meta) = fs::symlink_metadata(image.parent().unwrap()) {
        ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
            "Refusing redirected or foreign installation directory"
        );
    }
    fs::create_dir_all(image.parent().unwrap())?;
    fs::create_dir_all(command.parent().unwrap())?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(image.with_file_name(".install-lock"))?;
    lock.lock()?;
    if command.symlink_metadata().is_ok() {
        ensure!(
            fs::read_link(&command).is_ok_and(|p| p == image),
            "Refusing to replace unowned okilum command"
        );
    }
    if image.symlink_metadata().is_ok() {
        regular(&image)?;
        // Running a manually downloaded image at the final path is an explicit
        // adoption; another source may only replace our marked installation.
        if source != image || marker(&image).symlink_metadata().is_ok() {
            owned(&image)?;
        }
    }
    let current = if image.symlink_metadata().is_ok() {
        recorded_build(&image)
    } else {
        None
    };
    let mut replace = source != image && current.is_none_or(|installed| build > installed);
    // Exclude the self-updater: from download through the apply helper it
    // holds this lock, and the installed bytes may be newer than the record.
    let _update = if replace && image.symlink_metadata().is_ok() {
        match crate::update_lock(&image) {
            Ok(lock) => Some(lock),
            Err(_) => {
                replace = false;
                None
            }
        }
    } else {
        None
    };
    if replace {
        let mut input = fs::File::open(source)?;
        let mut staged = tempfile::NamedTempFile::new_in(image.parent().unwrap())?;
        std::io::copy(&mut input, staged.as_file_mut())?;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
        staged.as_file().sync_all()?;
        let after = regular(source)?;
        ensure!(
            after.ino() == metadata.ino()
                && after.dev() == metadata.dev()
                && after.len() == metadata.len()
                && after.mtime() == metadata.mtime()
                && after.mtime_nsec() == metadata.mtime_nsec(),
            "AppImage source changed while copying"
        );
        staged.persist(&image)?;
    }
    // Ownership is explicit, independent of the changing update payload hash.
    let mut owner = tempfile::NamedTempFile::new_in(image.parent().unwrap())?;
    owner.write_all(OWNER)?;
    owner.as_file().sync_all()?;
    owner.persist(marker(&image))?;
    if replace || source == image {
        write_build(&image, build)?;
    }
    fs::File::open(image.parent().unwrap())?.sync_all()?;
    if command.symlink_metadata().is_err() {
        std::os::unix::fs::symlink(&image, &command)?;
    }
    let legacy = home.join("Applications/Okilum.AppImage");
    if source == legacy {
        let current = regular(source)?;
        ensure!(
            current.ino() == metadata.ino()
                && current.dev() == metadata.dev()
                && current.len() == metadata.len(),
            "Legacy AppImage changed during migration"
        );
        fs::remove_file(source)?;
        // An absent/empty legacy directory may go; foreign files always survive.
        let _ = fs::remove_dir(legacy.parent().unwrap());
    }
    Ok(image)
}

pub fn remove(home: &Path) -> Result<()> {
    let (image, command) = paths(home)?;
    if image.symlink_metadata().is_ok() {
        regular(&image)?;
        owned(&image)?;
        if command.symlink_metadata().is_ok() {
            ensure!(
                fs::read_link(&command).is_ok_and(|p| p == image),
                "Refusing to remove unowned okilum command"
            );
            fs::remove_file(&command)?;
        }
        fs::remove_file(&image)?;
        fs::remove_file(marker(&image))?;
        let _ = fs::remove_file(build_record(&image));
        let _ = fs::remove_file(image.with_file_name(".install-lock"));
        let _ = fs::remove_dir(image.parent().context("Missing install directory")?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrates_legacy_updates_and_removes_only_owned_payload() {
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join("Applications/Okilum.AppImage");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(legacy.parent().unwrap().join("foreign"), "keep").unwrap();
        fs::write(&legacy, b"old").unwrap();
        fs::set_permissions(&legacy, fs::Permissions::from_mode(0o755)).unwrap();
        let image = install(home.path(), &legacy, 7464).unwrap();
        crate::desktop::install(home.path(), &image).unwrap();
        let (desktop, _) = crate::desktop::paths(home.path()).unwrap();
        let entry = fs::read_to_string(&desktop).unwrap();
        assert!(entry.matches(&image.to_string_lossy().to_string()).count() == 2);
        assert!(entry.contains("--vault %f"));
        assert!(!entry.contains("Applications/"));
        assert!(!legacy.exists());
        assert_eq!(fs::read(&image).unwrap(), b"old");
        let update = home.path().join("new.AppImage");
        fs::write(&update, b"new").unwrap();
        fs::set_permissions(&update, fs::Permissions::from_mode(0o755)).unwrap();
        install(home.path(), &update, 7538).unwrap();
        assert_eq!(fs::read(&image).unwrap(), b"new");
        assert_eq!(fs::read_link(paths(home.path()).unwrap().1).unwrap(), image);
        crate::desktop::remove(home.path()).unwrap();
        remove(home.path()).unwrap();
        assert!(!desktop.exists());
        assert!(!image.exists());
        assert!(update.exists());
        assert_eq!(
            fs::read(legacy.parent().unwrap().join("foreign")).unwrap(),
            b"keep"
        );
    }
    #[test]
    fn refuses_foreign_image_and_command() {
        let home = tempfile::tempdir().unwrap();
        let (image, command) = paths(home.path()).unwrap();
        fs::create_dir_all(image.parent().unwrap()).unwrap();
        fs::write(&image, b"foreign").unwrap();
        fs::set_permissions(&image, fs::Permissions::from_mode(0o755)).unwrap();
        let candidate = home.path().join("candidate");
        fs::write(&candidate, b"candidate").unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(install(home.path(), &candidate, 7538).is_err());
        assert!(remove(home.path()).is_err());
        assert_eq!(fs::read(&image).unwrap(), b"foreign");
        fs::remove_file(&image).unwrap();
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        fs::write(&command, b"foreign").unwrap();
        let source = home.path().join("download");
        fs::write(&source, b"app").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(install(home.path(), &source, 7538).is_err());
        assert!(!image.exists());
    }

    fn executable(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn older_external_launcher_never_downgrades_a_self_updated_install() {
        // #2455: the launcher runs ~/AppImages/okilum.appimage (7464); the
        // managed copy self-updates to 7538; the next launch must keep 7538.
        let home = tempfile::tempdir().unwrap();
        let external = home.path().join("AppImages/okilum.appimage");
        executable(&external, b"7464");
        let image = install(home.path(), &external, 7464).unwrap();
        assert_eq!(fs::read(&image).unwrap(), b"7464");
        // Applied self-update replaces the managed bytes; the new process records itself.
        executable(&image, b"7538");
        record_running(home.path(), &image, 7538).unwrap();
        assert_eq!(install(home.path(), &external, 7464).unwrap(), image);
        assert_eq!(fs::read(&image).unwrap(), b"7538");
        assert_eq!(recorded_build(&image), Some(7538));
        // The same build from the launcher is not a reason to copy either.
        executable(&external, b"7538");
        install(home.path(), &external, 7538).unwrap();
        assert_eq!(fs::read(&image).unwrap(), b"7538");
        // A strictly newer manual download still replaces the install.
        executable(&external, b"7539");
        install(home.path(), &external, 7539).unwrap();
        assert_eq!(fs::read(&image).unwrap(), b"7539");
        assert_eq!(recorded_build(&image), Some(7539));
        assert_eq!(fs::read(&external).unwrap(), b"7539"); // never written
    }

    #[test]
    fn unknown_installed_build_keeps_the_previous_replacement_behaviour() {
        let home = tempfile::tempdir().unwrap();
        let external = home.path().join("AppImages/okilum.appimage");
        executable(&external, b"old");
        let image = install(home.path(), &external, 7464).unwrap();
        fs::remove_file(build_record(&image)).unwrap(); // installed before #2455
        executable(&external, b"new");
        install(home.path(), &external, 7465).unwrap();
        assert_eq!(fs::read(&image).unwrap(), b"new");
    }

    #[test]
    fn running_record_ignores_foreign_paths() {
        let home = tempfile::tempdir().unwrap();
        let external = home.path().join("AppImages/okilum.appimage");
        executable(&external, b"x");
        let image = install(home.path(), &external, 7464).unwrap();
        record_running(home.path(), &external, 9999).unwrap();
        assert_eq!(recorded_build(&image), Some(7464));
    }

    #[test]
    fn active_self_update_blocks_external_replacement() {
        let home = tempfile::tempdir().unwrap();
        let external = home.path().join("AppImages/okilum.appimage");
        executable(&external, b"7538");
        let image = install(home.path(), &external, 7538).unwrap();
        // The updater holds its transaction lock while staging/applying 7540.
        let lock = crate::update_lock(&image).unwrap();
        note_incoming(&image, 7540).unwrap();
        executable(&external, b"7539");
        assert_eq!(install(home.path(), &external, 7539).unwrap(), image);
        assert_eq!(fs::read(&image).unwrap(), b"7538");
        drop(lock);
        // Applied 7540 before startup confirmation: 7539 still must not win.
        executable(&image, b"7540");
        install(home.path(), &external, 7539).unwrap();
        assert_eq!(fs::read(&image).unwrap(), b"7540");
        // Not our managed install: nothing recorded.
        note_incoming(&external, 9999).unwrap();
        assert!(!build_record(&external).exists());
    }
}
