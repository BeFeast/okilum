//! One supervisor at a time, including the gap between journal commit and spawn.
//! A surviving runtime from an older helper is refused, never adopted or signalled.
use super::{process_group::scan, Launch};
use anyhow::{ensure, Context, Result};
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub struct Lease {
    state: PathBuf,
    _file: File,
}
impl Lease {
    pub fn acquire(state: &Path, launch: &Launch) -> Result<Self> {
        let state = state.canonicalize()?;
        // Reuse the store's owner, location and private-directory checks.
        let _ = crate::sidecar::store::UnixStore::open_existing(&state)?;
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(state.join("supervisor.lock"))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0
                && metadata.nlink() == 1,
            "private supervisor lock required"
        );
        file.try_lock()
            .context("another Sync helper owns the supervisor lock")?;
        let image = Path::new(&launch.executable).canonicalize()?;
        ensure!(
            scan::live_copies(&image, unsafe { libc::geteuid() }, 0)?.is_empty(),
            "an earlier Syncthing runtime is still running; restart it before recovering Sync"
        );
        Ok(Self { state, _file: file })
    }
    pub(crate) fn state(&self) -> &Path {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn a_second_helper_is_excluded_until_the_owner_exits() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        // An existing executable with no running copies.
        let image = root.path().join("runtime");
        std::fs::write(&image, b"synthetic executable").unwrap();
        let launch = Launch {
            executable: image.to_str().unwrap().into(),
            config: "/config".into(),
            data: "/data".into(),
        };
        let first = Lease::acquire(&state, &launch).unwrap();
        assert!(Lease::acquire(&state, &launch).is_err());
        drop(first);
        assert!(Lease::acquire(&state, &launch).is_ok());
    }
}
