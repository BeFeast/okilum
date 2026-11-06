//! Lossless, descriptor-relative policy files; no daemon or app-state authority.
use super::*;
use rustix::fs::{AtFlags, Mode, OFlags};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

pub(super) struct Files {
    root: File,
    path: PathBuf,
    identity: (u64, u64),
}
impl Files {
    pub fn open(path: &Path, identity: (u64, u64)) -> Result<Self> {
        let root = File::from(rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )?);
        let result = Self {
            root,
            path: path.to_owned(),
            identity,
        };
        result.verify()?;
        Ok(result)
    }
    fn verify(&self) -> Result<()> {
        let held = self.root.metadata()?;
        let actual = std::fs::symlink_metadata(&self.path)?;
        ensure!(
            held.is_dir()
                && actual.is_dir()
                && held.uid() == rustix::process::geteuid().as_raw()
                && (held.dev(), held.ino()) == self.identity
                && (actual.dev(), actual.ino()) == self.identity,
            "Shared ignore vault identity changed"
        );
        Ok(())
    }
    fn settings(&self, create: bool) -> Result<Option<File>> {
        self.verify()?;
        if create {
            match rustix::fs::mkdirat(&self.root, ".okilum", Mode::from_raw_mode(0o700)) {
                Ok(()) => self.root.sync_all()?,
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(e.into()),
            }
        }
        let fd = match rustix::fs::openat(
            &self.root,
            ".okilum",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let file = File::from(fd);
        ensure!(
            file.metadata()?.uid() == rustix::process::geteuid().as_raw(),
            "Shared ignore directory ownership changed"
        );
        Ok(Some(file))
    }
    pub fn template(&self) -> Result<Option<Vec<u8>>> {
        self.verify()?;
        let directory = match rustix::fs::openat(
            &self.root,
            "_Schema",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => File::from(fd),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let bytes = read(&directory, "stignore.template")?;
        self.verify()?;
        Ok(bytes)
    }
    pub fn policy(&self) -> Result<Option<Vec<u8>>> {
        let bytes = match self.settings(false)? {
            Some(dir) => {
                let mut count = 0;
                for entry in rustix::fs::Dir::read_from(&dir)? {
                    count += 1;
                    ensure!(
                        count <= 1024,
                        "Shared policy directory needs review before reading"
                    );
                    let entry = entry?;
                    ensure!(!entry.file_name().to_bytes().starts_with(b"sync-ignore.sync-conflict-"),
                        "Shared ignore policy has conflicting versions in .okilum; review both files before applying");
                }
                read(&dir, "sync-ignore")?
            }
            None => None,
        };
        self.verify()?;
        Ok(bytes)
    }
    pub fn local(&self) -> Result<Option<Vec<u8>>> {
        let bytes = read(&self.root, ".stignore")?;
        self.verify()?;
        Ok(bytes)
    }
    pub fn publish_policy(&self, expected: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.policy()?.as_deref() == expected,
            "Shared ignore policy changed since preview"
        );
        let dir = self
            .settings(true)?
            .context("Shared ignore directory missing")?;
        self.publish(&dir, "sync-ignore", expected, bytes)?;
        ensure!(
            self.policy()?.as_deref() == Some(bytes),
            "Shared ignore policy readback changed"
        );
        Ok(())
    }
    pub fn publish_local(&self, expected: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
        self.publish(&self.root, ".stignore", expected, bytes)?;
        ensure!(
            self.local()?.as_deref() == Some(bytes),
            "Local ignore readback changed"
        );
        Ok(())
    }
    fn publish(&self, dir: &File, name: &str, expected: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
        self.verify()?;
        if expected == Some(bytes) {
            return Ok(());
        }
        let temporary = format!(".okilum-save-{}", Uuid::new_v4());
        let fd = rustix::fs::openat(
            dir,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )?;
        let result = (|| -> Result<()> {
            let mut file = File::from(fd);
            file.write_all(bytes)?;
            file.sync_all()?;
            self.verify()?;
            ensure!(
                read(dir, name)?.as_deref() == expected,
                "Ignore file changed since preview; nothing was replaced"
            );
            rustix::fs::renameat(dir, temporary.as_str(), dir, name)?;
            dir.sync_all()?;
            self.verify()?;
            Ok(())
        })();
        let _ = rustix::fs::unlinkat(dir, temporary.as_str(), AtFlags::empty());
        result
    }
}
fn read(dir: &File, name: &str) -> Result<Option<Vec<u8>>> {
    let fd = match rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let file = File::from(fd);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.len() <= LIMIT as u64,
        "Ignore policy must be a bounded, owner-held regular file"
    );
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= LIMIT, "Ignore policy exceeds limit");
    Ok(Some(bytes))
}
