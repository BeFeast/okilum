//! Unix state directory: the verified directory descriptor is the lock (flock) and
//! the anchor for every file access, so replacement of the path cannot split writers.
use super::{StateDir, Store};
use crate::sidecar::journal::Directory;
use anyhow::{ensure, Result};
use std::{
    fs::TryLockError,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub struct UnixDir {
    pub(super) dir: Directory,
    /// flock is per open file description, so two handles in this process would
    /// both "hold" it. This flag makes the lock exclusive in-process as well.
    busy: AtomicBool,
}
pub struct UnixLock(Arc<UnixDir>);
fn check_name(name: &str) -> Result<()> {
    // openat's NOFOLLOW protects only the final component. A path, including
    // an absolute one, could otherwise escape the verified directory anchor.
    ensure!(
        !name.is_empty() && !matches!(name, "." | "..") && !name.contains('/'),
        "single state file name required"
    );
    Ok(())
}
impl Drop for UnixLock {
    fn drop(&mut self) {
        let _ = self.0.dir.handle.unlock();
        self.0.busy.store(false, Ordering::Release);
    }
}
impl StateDir for UnixDir {
    type Lock = UnixLock;
    fn lock(self: &Arc<Self>, deadline: Instant) -> Result<UnixLock> {
        loop {
            if self
                .busy
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                match self.dir.handle.try_lock() {
                    Ok(()) => return Ok(UnixLock(self.clone())),
                    Err(TryLockError::WouldBlock) => self.busy.store(false, Ordering::Release),
                    Err(TryLockError::Error(e)) => {
                        self.busy.store(false, Ordering::Release);
                        return Err(e.into());
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            ensure!(!left.is_zero(), "sidecar state is busy");
            std::thread::sleep(left.min(Duration::from_millis(2)));
        }
    }
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        check_name(name)?;
        self.dir.read_bytes(name)
    }
    fn write(&self, name: &str, data: &[u8]) -> Result<()> {
        check_name(name)?;
        self.dir.write_bytes(name, data)
    }
    fn remove(&self, name: &str) -> Result<()> {
        check_name(name)?;
        self.dir.remove_bytes(name)
    }
}

pub type UnixStore = Store<UnixDir>;
impl Store<UnixDir> {
    /// No directory, journal or lock is created. Preparation supplies a private
    /// directory outside installation, vault and index, after explicit Enable.
    pub fn open_existing(path: &Path) -> Result<Self> {
        let dir = Directory::open(path)?;
        dir.check_location()?;
        Ok(Store::new(UnixDir {
            dir,
            busy: AtomicBool::new(false),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::{journal::Fault, store::NAME};
    use std::{
        fs,
        os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    };

    fn private_directory(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn fixture() -> (tempfile::TempDir, UnixStore) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        private_directory(&path);
        let store = UnixStore::open_existing(&path).unwrap();
        store.dir.write(NAME, b"committed").unwrap();
        assert_eq!(store.dir.read(NAME).unwrap().unwrap(), b"committed");
        assert_eq!(fs::metadata(path.join(NAME)).unwrap().mode() & 0o777, 0o600);
        (root, store)
    }

    fn refused(dir: &UnixDir, name: &str) {
        assert!(dir.read(name).is_err(), "read accepted {name:?}");
        assert!(
            dir.write(name, b"replacement").is_err(),
            "write accepted {name:?}"
        );
        assert!(dir.remove(name).is_err(), "remove accepted {name:?}");
    }

    #[test]
    fn file_names_cannot_escape_the_private_directory() {
        let (root, store) = fixture();
        let state = root.path().join("state");
        let outside = root.path().join("outside");
        private_directory(&outside);
        let other = UnixStore::open_existing(&outside).unwrap();
        other.dir.write(NAME, b"outside committed").unwrap();
        symlink(&outside, state.join("redirect")).unwrap();
        let absolute = outside.join(NAME);
        for name in [
            absolute.to_str().unwrap(),
            "../outside/sidecar.json",
            "redirect/sidecar.json",
            "./sidecar.json",
            "sidecar.json/",
            "",
            ".",
            "..",
        ] {
            refused(&store.dir, name);
            assert_eq!(fs::read(state.join(NAME)).unwrap(), b"committed");
            assert_eq!(fs::read(&absolute).unwrap(), b"outside committed");
        }
        assert_eq!(fs::read_dir(&state).unwrap().count(), 2);
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        // Positive control exercises all three production operations on a basename.
        store.dir.write("probe.json", b"healthy").unwrap();
        assert_eq!(store.dir.read("probe.json").unwrap().unwrap(), b"healthy");
        store.dir.remove("probe.json").unwrap();
        assert!(store.dir.read("probe.json").unwrap().is_none());
    }

    #[test]
    fn unsafe_directory_mode_identity_and_symlink_preserve_committed_files() {
        let (root, store) = fixture();
        let state = root.path().join("state");
        for mode in [0o750, 0o707] {
            fs::set_permissions(&state, fs::Permissions::from_mode(mode)).unwrap();
            assert!(UnixStore::open_existing(&state).is_err());
            refused(&store.dir, NAME);
            assert_eq!(fs::read(state.join(NAME)).unwrap(), b"committed");
        }
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        store.dir.write(NAME, b"committed").unwrap(); // healthy again
        let displaced = root.path().join("displaced");
        fs::rename(&state, &displaced).unwrap();
        private_directory(&state);
        let replacement = UnixStore::open_existing(&state).unwrap();
        replacement.dir.write(NAME, b"other identity").unwrap();
        refused(&store.dir, NAME);
        assert_eq!(fs::read(displaced.join(NAME)).unwrap(), b"committed");
        assert_eq!(fs::read(state.join(NAME)).unwrap(), b"other identity");
        drop(replacement);
        fs::remove_dir_all(&state).unwrap();
        symlink(&displaced, &state).unwrap();
        assert!(UnixStore::open_existing(&state).is_err());
        refused(&store.dir, NAME);
        assert_eq!(fs::read(displaced.join(NAME)).unwrap(), b"committed");
    }

    #[test]
    fn interrupted_atomic_replace_preserves_old_bytes_until_rename() {
        let (root, store) = fixture();
        let state = root.path().join("state");
        for (fault, visible) in [
            (Fault::BeforeRename, &b"committed"[..]),
            (Fault::AfterRename, &b"replacement"[..]),
        ] {
            store.dir.dir.fault.set(Some(fault));
            assert!(store.dir.write(NAME, b"replacement").is_err());
            assert_eq!(fs::read(state.join(NAME)).unwrap(), visible);
            assert_eq!(fs::read_dir(&state).unwrap().count(), 1);
            store.dir.dir.fault.set(None);
            let reopened = UnixStore::open_existing(&state).unwrap();
            assert_eq!(reopened.dir.read(NAME).unwrap().unwrap(), visible);
        }
        // After-rename failure is ambiguous, never a claim of rollback. The same
        // production write succeeds once the injected durability failure is gone.
        store.dir.write(NAME, b"healthy").unwrap();
        assert_eq!(store.dir.read(NAME).unwrap().unwrap(), b"healthy");
    }
}
