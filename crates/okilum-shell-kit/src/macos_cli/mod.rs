//! The per-user command for the macOS app installed at the standard DMG and Sprint path.
use rustix::fs::{self as unix_fs, Mode, OFlags};
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const EXECUTABLE: &str = "/Applications/Okilum.app/Contents/MacOS/okilum";
const LAUNCHER: &[u8] = include_bytes!("okilum.sh");
// Keep this header stable so any revision of the v1 launcher is recognized.
const OWNED_HEADER: &[u8] = b"#!/bin/sh\n# okilum-launcher v1\n";

fn command_path() -> Option<PathBuf> {
    if std::env::var_os("OKILUM_SYNC_QA_ROOT").is_some()
        || std::env::current_exe().ok()?.as_path() != Path::new(EXECUTABLE)
    {
        return None;
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.is_absolute().then(|| home.join(".local/bin/okilum"))
}

pub fn install() -> io::Result<()> {
    if let Some(path) = command_path() {
        install_at(&path)?;
    }
    Ok(())
}

fn install_at(path: &Path) -> io::Result<()> {
    match path.symlink_metadata() {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let directory = path.parent().expect("terminal command has a parent");
    fs::create_dir_all(directory)?;
    let mut launcher = tempfile::NamedTempFile::new_in(directory)?;
    launcher.write_all(LAUNCHER)?;
    launcher
        .as_file()
        .set_permissions(Permissions::from_mode(0o755))?;
    // Publish the complete launcher while preserving an existing user command,
    // including a symlink whose destination may be temporarily unavailable.
    match launcher.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error),
    }
}

pub fn owned_command() -> Option<PathBuf> {
    command_path().filter(|path| is_owned(path))
}

fn is_owned(path: &Path) -> bool {
    let Ok(fd) = unix_fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) else {
        return false;
    };
    let mut file = File::from(fd);
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut header = [0; OWNED_HEADER.len()];
    file.read_exact(&mut header).is_ok() && header == OWNED_HEADER
}

pub fn remove() -> io::Result<()> {
    if let Some(path) = owned_command() {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_an_executable_launcher_without_replacing_existing_commands() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".local/bin/okilum");
        install_at(&path).unwrap();
        assert!(is_owned(&path));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );

        fs::write(&path, [OWNED_HEADER, b"echo newer version\n"].concat()).unwrap();
        assert!(is_owned(&path));

        fs::write(&path, b"user launcher").unwrap();
        install_at(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"user launcher");
        assert!(!is_owned(&path));

        fs::remove_file(&path).unwrap();
        let missing_target = home.path().join("temporarily-missing");
        std::os::unix::fs::symlink(&missing_target, &path).unwrap();
        install_at(&path).unwrap();
        assert_eq!(fs::read_link(&path).unwrap(), missing_target);
        assert!(!is_owned(&path));
    }
}
