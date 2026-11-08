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
    // Valid basenames still undergo Directory's final-component NOFOLLOW checks.
    ensure!(
        !name.is_empty()
            && !matches!(name, "." | "..")
            && !name.contains('/')
            && !name.contains('\0'),
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
    /// A launchd restart is authorized only by a lifetime lease, never by an
    /// absent endpoint. Old runtimes and concurrent helpers prevent this call.
    pub fn claim_supervised_launch(
        &self,
        binding: &crate::sidecar::Binding,
        lease: &crate::sidecar::supervisor::lease::Lease,
        deadline: Instant,
    ) -> Result<uuid::Uuid> {
        ensure!(
            Path::new(&binding.state_directory).canonicalize()? == lease.state(),
            "supervisor lease belongs to another state directory"
        );
        self.dir.dir.check_location()?;
        let opened = self.dir.dir.handle.metadata()?;
        let leased = std::fs::metadata(lease.state())?;
        use std::os::unix::fs::MetadataExt as _;
        ensure!(
            opened.dev() == leased.dev() && opened.ino() == leased.ino(),
            "supervisor lease does not cover this store"
        );
        let mut tx = self.begin(deadline)?;
        let saved = tx
            .current()?
            .ok_or_else(|| anyhow::anyhow!("missing launch journal"))?;
        if saved.launch_registration().is_some_and(|launch| {
            launch.phase() == crate::sidecar::authority::launch::Phase::Claimed
        }) {
            let next = saved.restart_claim(binding)?;
            let generation = next.launch_registration().unwrap().generation();
            // Dedicated recovery commit. Ordinary successor validation still
            // refuses replacing/re-arming a launch without this lease.
            tx.write(next)?;
            Ok(generation)
        } else {
            drop(tx);
            self.claim_launch(binding, deadline)
        }
    }
    pub fn write_supervisor_failure(&self, message: &str, deadline: Instant) -> Result<()> {
        let _lock = self.dir.lock(deadline)?;
        let message: String = message.chars().take(8192).collect();
        self.dir.write("supervisor-error.log", message.as_bytes())
    }
    pub fn read_supervisor_failure(&self) -> Result<Option<String>> {
        Ok(self
            .dir
            .read("supervisor-error.log")?
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }
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

    fn invalid_name_refused(dir: &UnixDir, name: &str) {
        for result in [
            dir.read(name).map(|_| ()),
            dir.write(name, b"replacement"),
            dir.remove(name),
        ] {
            assert_eq!(
                result.unwrap_err().to_string(),
                "single state file name required",
                "invalid name {name:?} must be refused before directory access"
            );
        }
    }

    fn snapshot(path: &Path) -> (Vec<u8>, u64, u64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        (fs::read(path).unwrap(), metadata.dev(), metadata.ino())
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
        let committed = snapshot(&state.join(NAME));
        let outside_committed = snapshot(&absolute);
        let redirect = fs::symlink_metadata(state.join("redirect")).unwrap();
        for name in [
            absolute.to_str().unwrap(),
            "../outside/sidecar.json",
            "redirect/sidecar.json",
            "./sidecar.json",
            "sidecar.json/",
            "",
            ".",
            "..",
            "\0",
            "sidecar.json\0ignored",
        ] {
            invalid_name_refused(&store.dir, name);
            assert_eq!(snapshot(&state.join(NAME)), committed);
            assert_eq!(snapshot(&absolute), outside_committed);
            let unchanged = fs::symlink_metadata(state.join("redirect")).unwrap();
            assert_eq!(
                (unchanged.dev(), unchanged.ino()),
                (redirect.dev(), redirect.ino())
            );
            assert_eq!(fs::read_link(state.join("redirect")).unwrap(), outside);
        }
        assert_eq!(fs::read_dir(&state).unwrap().count(), 2);
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        // Dots and Unix backslashes are legal basename characters.
        for name in [
            "probe.json",
            ".hidden.json",
            "probe..json",
            "...",
            "probe.",
            r"probe\name.json",
        ] {
            store.dir.write(name, b"healthy").unwrap();
            assert_eq!(store.dir.read(name).unwrap().unwrap(), b"healthy");
            store.dir.remove(name).unwrap();
            assert!(store.dir.read(name).unwrap().is_none());
        }
    }

    #[test]
    fn invalid_names_are_refused_before_directory_revalidation() {
        let (root, store) = fixture();
        let displaced = root.path().join("displaced");
        fs::rename(root.path().join("state"), &displaced).unwrap();
        let committed = snapshot(&displaced.join(NAME));
        for name in [
            "",
            ".",
            "..",
            "/absolute",
            "../outside",
            "a/b",
            "a/",
            "a\0b",
        ] {
            invalid_name_refused(&store.dir, name);
        }
        assert_eq!(snapshot(&displaced.join(NAME)), committed);
        // Positive control: a valid name reaches the now-missing directory check.
        let error = store.dir.read(NAME).unwrap_err();
        assert_ne!(error.to_string(), "single state file name required");
    }

    #[test]
    fn final_symlink_is_refused_without_replacing_the_link_or_target() {
        let (root, store) = fixture();
        let state = root.path().join("state");
        let outside = root.path().join("outside");
        private_directory(&outside);
        let other = UnixStore::open_existing(&outside).unwrap();
        other.dir.write(NAME, b"outside committed").unwrap();
        let target = outside.join(NAME);
        let link = state.join("alias.json");
        symlink(&target, &link).unwrap();
        let committed = snapshot(&state.join(NAME));
        let outside_committed = snapshot(&target);
        let linked = snapshot(&link);
        refused(&store.dir, "alias.json");
        assert_eq!(snapshot(&state.join(NAME)), committed);
        assert_eq!(snapshot(&target), outside_committed);
        assert_eq!(snapshot(&link), linked);
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), target);
        assert_eq!(fs::read_dir(&state).unwrap().count(), 2);
    }

    #[test]
    fn unsafe_directory_mode_identity_and_symlink_preserve_committed_files() {
        let (root, store) = fixture();
        let state = root.path().join("state");
        let committed = snapshot(&state.join(NAME));
        for mode in [0o750, 0o707] {
            fs::set_permissions(&state, fs::Permissions::from_mode(mode)).unwrap();
            assert!(UnixStore::open_existing(&state).is_err());
            refused(&store.dir, NAME);
            assert_eq!(snapshot(&state.join(NAME)), committed);
        }
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        store.dir.write(NAME, b"committed").unwrap(); // healthy again
        let committed = snapshot(&state.join(NAME));
        let displaced = root.path().join("displaced");
        fs::rename(&state, &displaced).unwrap();
        private_directory(&state);
        let replacement = UnixStore::open_existing(&state).unwrap();
        replacement.dir.write(NAME, b"other identity").unwrap();
        let replacement_committed = snapshot(&state.join(NAME));
        refused(&store.dir, NAME);
        assert_eq!(snapshot(&displaced.join(NAME)), committed);
        assert_eq!(snapshot(&state.join(NAME)), replacement_committed);
        drop(replacement);
        fs::remove_dir_all(&state).unwrap();
        symlink(&displaced, &state).unwrap();
        assert!(UnixStore::open_existing(&state).is_err());
        refused(&store.dir, NAME);
        assert_eq!(snapshot(&displaced.join(NAME)), committed);
    }

    #[test]
    fn interrupted_atomic_replace_preserves_old_bytes_until_rename() {
        let (root, store) = fixture();
        let state = root.path().join("state");
        for (fault, visible) in [
            (Fault::BeforeRename, &b"committed"[..]),
            (Fault::AfterRename, &b"replacement"[..]),
        ] {
            let before = snapshot(&state.join(NAME));
            store.dir.dir.fault.set(Some(fault));
            assert!(store.dir.write(NAME, b"replacement").is_err());
            assert_eq!(fs::read(state.join(NAME)).unwrap(), visible);
            if fault == Fault::BeforeRename {
                assert_eq!(snapshot(&state.join(NAME)), before);
            }
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
