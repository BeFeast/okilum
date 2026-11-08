//! Owner-protected Windows controller state. Ancestor handles pin every lookup;
//! private state never repairs a shared DACL or follows a reparse point.
use crate::sidecar::windows::private::{
    create_private_file, open_private_file, verify_private_file, PrivateDirectory,
};
use anyhow::{ensure, Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::Path,
};
use uuid::Uuid;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS},
        Storage::FileSystem::{
            MoveFileExW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        },
    },
};

fn text(path: &Path) -> Result<String> {
    let value = path.to_str().context("Sync state path must be UTF-8")?;
    // Rust canonicalize returns the extended local form. The native private
    // directory adapter pins ordinary drive paths; UNC/device paths still fail.
    let value = value.strip_prefix(r"\\?\").unwrap_or(value);
    crate::sidecar::windows::path(value)?;
    Ok(value.into())
}
fn parent(path: &Path) -> Result<PrivateDirectory> {
    PrivateDirectory::open_existing(&text(path.parent().context("Sync state parent missing")?)?)
}
pub(crate) fn directory(path: &Path) -> Result<()> {
    PrivateDirectory::prepare(&text(path)?)?;
    Ok(())
}
pub(crate) fn read(path: &Path) -> Result<Vec<u8>> {
    read_limit(path, 4 * 1024 * 1024)
}
pub(crate) fn read_limit(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let _parent = parent(path)?;
    let file = open_private_file(&text(path)?)?.context("private Sync state file missing")?;
    verify_private_file(&file)?;
    ensure!(
        file.metadata()?.len() <= limit as u64,
        "private Sync state file exceeds bound"
    );
    let mut bytes = vec![];
    file.take(
        u64::try_from(limit)?
            .checked_add(1)
            .context("journal size overflow")?,
    )
    .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "private Sync state file exceeds bound"
    );
    Ok(bytes)
}
pub(crate) fn public_file(path: &Path) -> Result<Vec<u8>> {
    // Generated identity files and staged runtime provenance are created in the
    // same protected controller root, including the non-secret certificate.
    read(path)
}
pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let _parent = parent(path)?;
    let destination = text(path)?;
    if let Some(file) = open_private_file(&destination)? {
        verify_private_file(&file)?;
    }
    let temporary = path.with_file_name(format!(".sync-{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = create_private_file(&text(&temporary)?)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        verify_private_file(&file)?;
        drop(file);
        let from: Vec<_> = text(&temporary)?.encode_utf16().chain(Some(0)).collect();
        let to: Vec<_> = destination.encode_utf16().chain(Some(0)).collect();
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
/// A file lock serializes cooperating processes; retained directory/ancestor
/// handles additionally prevent the state being renamed out from under it.
pub(crate) struct Lock {
    _file: File,
    _directory: PrivateDirectory,
}
pub(crate) fn lock(path: &Path) -> Result<Lock> {
    directory(path)?;
    let directory = PrivateDirectory::open_existing(&text(path)?)?;
    let lock_path = text(&path.join("prepare.lock"))?;
    let file = match create_private_file(&lock_path) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<windows::core::Error>()
                .is_some_and(|error| {
                    [ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS]
                        .iter()
                        .any(|code| error.code() == windows::core::HRESULT::from_win32(code.0))
                }) =>
        {
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(&lock_path)?
        }
        Err(error) => return Err(error),
    };
    verify_private_file(&file)?;
    file.try_lock()
        .context("private Sync state is already owned")?;
    Ok(Lock {
        _file: file,
        _directory: directory,
    })
}

/// Staged private blob with retained ancestor ownership. Publication closes its
/// writable descriptor first, then uses a same-directory no-replace rename.
pub(crate) struct Writer {
    file: File,
    path: std::path::PathBuf,
    _directory: PrivateDirectory,
}
impl std::ops::Deref for Writer {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}
impl std::ops::DerefMut for Writer {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.file
    }
}
impl Read for Writer {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(bytes)
    }
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
impl std::io::Seek for Writer {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.file, position)
    }
}
impl Writer {
    pub(crate) fn publish_noreplace(self, destination: &Path) -> Result<()> {
        ensure!(
            text(self.path.parent().context("staging parent missing")?)?
                == text(destination.parent().context("publication parent missing")?)?,
            "private publication must stay in its pinned directory"
        );
        verify_private_file(&self.file)?;
        self.file.sync_all()?;
        let Self {
            file,
            path,
            _directory,
        } = self;
        drop(file);
        let from: Vec<_> = text(&path)?.encode_utf16().chain(Some(0)).collect();
        let to: Vec<_> = text(destination)?.encode_utf16().chain(Some(0)).collect();
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }?;
        Ok(())
    }
}
pub(crate) fn open_rw(path: &Path) -> Result<Writer> {
    let directory = parent(path)?;
    let location = text(path)?;
    let file = match create_private_file(&location) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<windows::core::Error>()
                .is_some_and(|error| {
                    [ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS]
                        .iter()
                        .any(|code| error.code() == windows::core::HRESULT::from_win32(code.0))
                }) =>
        {
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(&location)?
        }
        Err(error) => return Err(error),
    };
    verify_private_file(&file)?;
    Ok(Writer {
        file,
        path: path.to_owned(),
        _directory: directory,
    })
}

pub(crate) fn open_existing_rw(path: &Path) -> Result<Writer> {
    let directory = parent(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(text(path)?)?;
    verify_private_file(&file)?;
    Ok(Writer {
        file,
        path: path.to_owned(),
        _directory: directory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_private_state_reopens_the_same_bytes_and_serializes_owners() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let owner = lock(&state)?;
        let file = state.join("record.json");
        write(&file, b"original")?;
        assert_eq!(read(&file)?, b"original");
        write(&file, b"replacement")?;
        assert_eq!(read(&file)?, b"replacement");
        assert!(lock(&state).is_err());
        assert!(fs::rename(&state, temp.path().join("moved")).is_err());
        drop(owner);
        let _owner = lock(&state)?;
        assert_eq!(public_file(&file)?, b"replacement");
        let partial = state.join("staged.partial");
        let mut writer = open_rw(&partial)?;
        writer.write_all(b"staged blob")?;
        assert!(writer.publish_noreplace(&file).is_err());
        assert_eq!(read(&file)?, b"replacement");
        assert_eq!(read_limit(&partial, 11)?, b"staged blob");
        assert!(read_limit(&partial, 3).is_err());
        let complete = state.join("complete");
        open_rw(&partial)?.publish_noreplace(&complete)?;
        assert_eq!(read(&complete)?, b"staged blob");
        assert!(!partial.exists());
        Ok(())
    }
    #[test]
    fn windows_private_state_refuses_shared_directories_and_redirected_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let shared = temp.path().join("shared");
        fs::create_dir(&shared)?;
        assert!(lock(&shared).is_err(), "shared DACL must never be repaired");
        let state = temp.path().join("private");
        let _owner = lock(&state)?;
        let target = state.join("target");
        write(&target, b"kept")?;
        let link = state.join("redirect");
        std::os::windows::fs::symlink_file(&target, &link)?;
        assert!(read(&link).is_err());
        assert!(write(&link, b"must not write").is_err());
        assert_eq!(read(&target)?, b"kept");
        let alias = temp.path().join("alias");
        std::os::windows::fs::symlink_dir(&state, &alias)?;
        assert!(lock(&alias).is_err());
        Ok(())
    }
}
