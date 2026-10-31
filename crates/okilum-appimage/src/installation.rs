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
pub fn install(home: &Path, source: &Path) -> Result<PathBuf> {
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
    if source != image {
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
        let image = install(home.path(), &legacy).unwrap();
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
        install(home.path(), &update).unwrap();
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
        assert!(install(home.path(), &candidate).is_err());
        assert!(remove(home.path()).is_err());
        assert_eq!(fs::read(&image).unwrap(), b"foreign");
        fs::remove_file(&image).unwrap();
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        fs::write(&command, b"foreign").unwrap();
        let source = home.path().join("download");
        fs::write(&source, b"app").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(install(home.path(), &source).is_err());
        assert!(!image.exists());
    }
}
