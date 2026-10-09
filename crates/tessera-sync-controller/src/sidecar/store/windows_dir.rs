//! Windows state directory. The exclusive instance lock is an exclusive open of
//! the protected private directory itself (no lock file, so an absent journal
//! leaves the prepared directory empty): while the handle lives no other open of
//! the directory can succeed, in this process or another, and the directory
//! cannot be renamed or replaced. Journal files are created with an explicit
//! owner and a protected single-grant DACL and are validated on every read.
use super::{StateDir, Store};
use crate::sidecar::windows::private::{
    create_private_file, open_private_file, verify_private_file, PrivateDirectory,
};
use anyhow::{ensure, Context, Result};
use std::{
    io::{Read, Write},
    os::windows::ffi::OsStrExt,
    sync::Arc,
    time::{Duration, Instant},
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::ERROR_SHARING_VIOLATION,
        Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
    },
};

const LIMIT: u64 = 65536;

pub struct WindowsDir {
    path: String,
    identity: (u32, u64),
    #[cfg(test)]
    pub(super) fault: std::cell::Cell<bool>,
}
/// The exclusive directory handle; dropping it releases the lock.
pub struct WindowsLock {
    _directory: PrivateDirectory,
}
fn sharing_violation(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<windows::core::Error>()
        .is_some_and(|e| e.code() == windows::core::HRESULT::from_win32(ERROR_SHARING_VIOLATION.0))
}
impl WindowsDir {
    fn file(&self, name: &str) -> String {
        format!("{}\\{name}", self.path.trim_end_matches('\\'))
    }
}
impl StateDir for WindowsDir {
    type Lock = WindowsLock;
    fn lock(self: &Arc<Self>, deadline: Instant) -> Result<WindowsLock> {
        loop {
            match PrivateDirectory::lock_exclusive(&self.path) {
                Ok(directory) => {
                    ensure!(
                        directory.identity()? == self.identity,
                        "state directory was replaced"
                    );
                    return Ok(WindowsLock {
                        _directory: directory,
                    });
                }
                Err(e) if sharing_violation(&e) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    ensure!(!left.is_zero(), "sidecar state is busy");
                    std::thread::sleep(left.min(Duration::from_millis(2)));
                }
                Err(e) => return Err(e),
            }
        }
    }
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let Some(file) = open_private_file(&self.file(name))? else {
            return Ok(None);
        };
        verify_private_file(&file)?;
        ensure!(
            file.metadata()?.len() <= LIMIT,
            "sidecar journal exceeds limit"
        );
        let mut data = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut data)?;
        ensure!(data.len() as u64 <= LIMIT, "sidecar journal exceeds limit");
        Ok(Some(data))
    }
    fn write(&self, name: &str, data: &[u8]) -> Result<()> {
        ensure!(data.len() as u64 <= LIMIT, "sidecar journal exceeds limit");
        // Refuse redirected or foreign prior state instead of replacing it.
        self.read(name)?;
        let temporary = self.file(&format!(".sidecar-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = create_private_file(&temporary)?;
            file.write_all(data)?;
            file.sync_all()?;
            verify_private_file(&file)?;
            drop(file);
            #[cfg(test)]
            ensure!(!self.fault.get(), "injected flush failure");
            let from: Vec<u16> = std::ffi::OsStr::new(&temporary)
                .encode_wide()
                .chain(Some(0))
                .collect();
            let to: Vec<u16> = std::ffi::OsStr::new(&self.file(name))
                .encode_wide()
                .chain(Some(0))
                .collect();
            unsafe {
                MoveFileExW(
                    PCWSTR(from.as_ptr()),
                    PCWSTR(to.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .context("replacing the journal failed")
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
    fn remove(&self, name: &str) -> Result<()> {
        self.delete(name)
    }
}

pub type WindowsStore = Store<WindowsDir>;
impl WindowsDir {
    fn delete(&self, name: &str) -> Result<()> {
        let Some(file) = open_private_file(&self.file(name))? else {
            return Ok(());
        };
        verify_private_file(&file)?;
        drop(file);
        std::fs::remove_file(self.file(name)).context("removing the file failed")
    }
}
impl Store<WindowsDir> {
    /// Validates an already prepared directory (`PrivateDirectory::prepare`,
    /// after explicit Enable) and remembers its identity. Creates nothing and
    /// holds no handle, so it works while another process holds the lock.
    pub fn open_existing(path: &str) -> Result<Self> {
        let directory = PrivateDirectory::inspect(path)?;
        let identity = directory.identity()?;
        Ok(Store::new(WindowsDir {
            path: path.to_string(),
            identity,
            #[cfg(test)]
            fault: Default::default(),
        }))
    }
}
