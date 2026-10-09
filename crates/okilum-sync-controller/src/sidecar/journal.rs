//! Unix durable journal for a directory prepared only after explicit Enable.
//! The directory itself is locked, so lock-file replacement cannot split writers.
use super::{Journal, LockedJournal};
use anyhow::{ensure, Context, Result};
use rustix::fs::{openat, renameat, unlinkat, AtFlags, Mode, OFlags};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;
const NAME: &str = "sidecar.json";
const LIMIT: u64 = 65536;
/// Verified private state directory. It holds no lock: the legacy journal locks
/// its descriptor for its lifetime, the transactional store only per transaction.
pub(super) struct Directory {
    pub(super) handle: File,
    path: PathBuf,
    #[cfg(test)]
    pub(super) fault: std::cell::Cell<Option<Fault>>,
}
/// Injected one-shot write failures, always paired with an unfaulted control.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    BeforeRename,
    AfterRename,
}
impl Directory {
    /// Does not create directories or a journal. Preparation must explicitly
    /// supply a private directory outside installation, vault and index.
    pub(super) fn open(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute(),
            "absolute private state directory required"
        );
        let handle = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
            .open(path)?;
        Self::validate_directory(&handle)?;
        Ok(Self {
            handle,
            path: path.to_path_buf(),
            #[cfg(test)]
            fault: Default::default(),
        })
    }
    fn validate_directory(directory: &File) -> Result<()> {
        let m = directory.metadata()?;
        ensure!(
            m.is_dir() && m.uid() == rustix::process::geteuid().as_raw() && m.mode() & 0o077 == 0,
            "private user-owned directory required"
        );
        Ok(())
    }
    pub(super) fn check_location(&self) -> Result<()> {
        Self::validate_directory(&self.handle)?;
        let current = std::fs::symlink_metadata(&self.path)?;
        let opened = self.handle.metadata()?;
        ensure!(
            current.is_dir()
                && !current.file_type().is_symlink()
                && current.dev() == opened.dev()
                && current.ino() == opened.ino(),
            "state directory was replaced"
        );
        Ok(())
    }
    pub(super) fn read_bytes(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.check_location()?;
        let fd = match openat(
            &self.handle,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let file = File::from(fd);
        let m = file.metadata()?;
        ensure!(
            m.is_file()
                && m.uid() == rustix::process::geteuid().as_raw()
                && m.mode() & 0o077 == 0
                && m.nlink() == 1
                && m.len() <= LIMIT,
            "private regular journal required"
        );
        let mut data = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut data)?;
        ensure!(data.len() as u64 <= LIMIT, "sidecar journal exceeds limit");
        Ok(Some(data))
    }
    /// Delete a journal-class file. The file must be a private regular file (a
    /// redirected or foreign one is refused, never deleted); absent is fine.
    pub(super) fn remove_bytes(&self, name: &str) -> Result<()> {
        if self.read_bytes(name)?.is_none() {
            return Ok(());
        }
        unlinkat(&self.handle, name, AtFlags::empty())?;
        self.handle.sync_all()?;
        Ok(())
    }
    /// Atomic replace: flush the temporary file, rename, flush the directory.
    /// An error after the rename means the new state may already be visible.
    pub(super) fn write_bytes(&self, name: &str, data: &[u8]) -> Result<()> {
        self.check_location()?;
        // Refuse redirected prior state instead of overwriting it.
        self.read_bytes(name)?;
        ensure!(data.len() as u64 <= LIMIT, "sidecar journal exceeds limit");
        let temporary = format!(".sidecar-{}.tmp", Uuid::new_v4());
        let result = (|| -> Result<()> {
            let fd = openat(
                &self.handle,
                temporary.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?;
            let mut file = File::from(fd);
            file.write_all(data)?;
            file.sync_all()?;
            self.check_location()?;
            #[cfg(test)]
            ensure!(
                self.fault.get() != Some(Fault::BeforeRename),
                "injected flush failure"
            );
            renameat(&self.handle, temporary.as_str(), &self.handle, name)?;
            #[cfg(test)]
            ensure!(
                self.fault.get() != Some(Fault::AfterRename),
                "injected directory flush failure"
            );
            self.handle.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = unlinkat(&self.handle, temporary.as_str(), AtFlags::empty());
        }
        result
    }
}

pub struct UnixJournal {
    dir: Directory,
}
impl UnixJournal {
    pub fn open_existing(path: &Path) -> Result<Self> {
        let dir = Directory::open(path)?;
        dir.handle
            .try_lock()
            .context("sidecar state is already in use")?;
        dir.check_location()?;
        Ok(Self { dir })
    }
}
impl UnixJournal {
    fn read_record<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let Some(data) = self.dir.read_bytes(name)? else {
            return Ok(None);
        };
        Ok(Some(
            serde_json::from_slice(&data).context("invalid sidecar journal; recovery required")?,
        ))
    }
    fn write_record<T: serde::Serialize + serde::de::DeserializeOwned>(
        &mut self,
        name: &str,
        journal: &T,
    ) -> Result<()> {
        // Refuse corrupt prior state instead of overwriting it.
        self.read_record::<T>(name)?;
        self.dir.write_bytes(name, &serde_json::to_vec(journal)?)
    }
}
impl LockedJournal for UnixJournal {
    fn load(&self) -> Result<Option<Journal>> {
        self.read_record(NAME)
    }
    fn save(&mut self, value: &Journal) -> Result<()> {
        self.write_record(NAME, value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::{Binding, Intent};
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
    };
    fn fixture() -> (tempfile::TempDir, Journal) {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let journal = Journal {
            binding: Binding {
                instance: Uuid::new_v4(),
                installation: Uuid::new_v4(),
                owner: rustix::process::geteuid().as_raw().to_string(),
                supervisor: "/Applications/Okilum.app/Contents/MacOS/supervisor".into(),
                state_directory: directory.path().display().to_string(),
                device_identity: "existing-device".into(),
            },
            intent: Intent::Enabled,
        };
        (directory, journal)
    }
    #[test]
    fn absent_is_inert_and_saved_intent_survives_reopen() {
        let (dir, mut journal) = fixture();
        let mut store = UnixJournal::open_existing(dir.path()).unwrap();
        assert!(store.load().unwrap().is_none());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        store.save(&journal).unwrap();
        journal.intent = Intent::Disabled;
        store.save(&journal).unwrap();
        drop(store);
        let store = UnixJournal::open_existing(dir.path()).unwrap();
        assert_eq!(store.load().unwrap(), Some(journal));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(
            fs::metadata(dir.path().join(NAME)).unwrap().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn concurrent_writer_fails_then_succeeds_after_drop() {
        let (dir, _) = fixture();
        let store = UnixJournal::open_existing(dir.path()).unwrap();
        assert!(UnixJournal::open_existing(dir.path()).is_err());
        drop(store);
        assert!(UnixJournal::open_existing(dir.path()).is_ok());
    }
    #[test]
    fn redirected_or_shared_state_is_rejected_without_overwrite() {
        let (dir, journal) = fixture();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"preserve").unwrap();
        let mut store = UnixJournal::open_existing(dir.path()).unwrap();
        let path = dir.path().join(NAME);
        symlink(outside.path(), &path).unwrap();
        assert!(store.save(&journal).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(outside.path(), &path).unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read(outside.path()).unwrap(), b"preserve");
    }
    #[test]
    fn malformed_journal_and_replaced_directory_require_recovery() {
        let (dir, journal) = fixture();
        let mut store = UnixJournal::open_existing(dir.path()).unwrap();
        store.save(&journal).unwrap();
        fs::write(dir.path().join(NAME), b"{").unwrap();
        assert!(store.save(&journal).is_err());
        let displaced = dir.path().with_extension("moved");
        fs::rename(dir.path(), &displaced).unwrap();
        fs::create_dir(dir.path()).unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&journal).is_err());
        assert_eq!(fs::read(displaced.join(NAME)).unwrap(), b"{");
        fs::remove_dir_all(displaced).unwrap();
    }
    #[test]
    fn directory_rejects_symlink_and_shared_permissions() {
        let (dir, _) = fixture();
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("alias");
        symlink(dir.path(), &link).unwrap();
        assert!(UnixJournal::open_existing(&link).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(UnixJournal::open_existing(dir.path()).is_err());
    }
}
