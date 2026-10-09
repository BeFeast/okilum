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
        self.dir.read_bytes(name)
    }
    fn write(&self, name: &str, data: &[u8]) -> Result<()> {
        self.dir.write_bytes(name, data)
    }
    fn remove(&self, name: &str) -> Result<()> {
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
