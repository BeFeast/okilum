//! NTFS policy files use retained ordinary-directory handles and revision-aware
//! publication. Every interrupted replacement retains its preimage in the vault.
use super::*;
use okilum_core::windows_files::Directory;
use std::{ffi::OsStr, io::Read};

pub(super) struct Files {
    root: Directory,
    identity: (u64, u64),
}
fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
fn read(directory: &Directory, name: &str) -> Result<Option<Vec<u8>>> {
    let file = match directory.open_file(OsStr::new(name)) {
        Ok(file) => file,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    ensure!(
        file.metadata()?.len() <= LIMIT as u64,
        "Ignore file exceeds the limit"
    );
    let mut bytes = vec![];
    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= LIMIT, "Ignore file exceeds the limit");
    Ok(Some(bytes))
}
impl Files {
    pub fn open(path: &Path, identity: (u64, u64)) -> Result<Self> {
        let result = Self {
            root: Directory::open(path)?,
            identity,
        };
        result.verify()?;
        Ok(result)
    }
    fn verify(&self) -> Result<()> {
        let held = existing::VaultRoot::open(self.root.path())?;
        ensure!(
            held.identity()? == self.identity
                && held.stable_identity()? == self.root.filesystem_identity()?,
            "Shared ignore vault identity changed"
        );
        Ok(())
    }
    fn settings(&self, create: bool) -> Result<Option<Directory>> {
        self.verify()?;
        let path = self.root.child(OsStr::new(".okilum"))?;
        match Directory::open(&path) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if missing(&error) && create => {
                self.root.create_directory(OsStr::new(".okilum"))?;
                Ok(Some(Directory::open(&path)?))
            }
            Err(error) if missing(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub fn template(&self) -> Result<Option<Vec<u8>>> {
        self.verify()?;
        let path = self.root.child(OsStr::new("@Schema"))?;
        let directory = match Directory::open(&path) {
            Ok(directory) => Some(directory),
            Err(error) if missing(&error) => None,
            Err(error) => return Err(error),
        };
        let bytes = match directory {
            Some(directory) => read(&directory, "stignore.template")?,
            None => None,
        };
        self.verify()?;
        Ok(bytes)
    }

    pub fn policy(&self) -> Result<Option<Vec<u8>>> {
        let bytes = match self.settings(false)? {
            Some(directory) => {
                for (index, entry) in std::fs::read_dir(directory.path())?.enumerate() {
                    ensure!(
                        index < 1024,
                        "Shared policy directory needs review before reading"
                    );
                    let name = entry?.file_name();
                    let name = name
                        .to_str()
                        .context("Shared policy filename is not Unicode")?;
                    ensure!(!name.starts_with("sync-ignore.sync-conflict-"),
                        "Shared ignore policy has conflicting versions in .okilum; review both files before applying");
                }
                read(&directory, "sync-ignore")?
            }
            None => None,
        };
        self.verify()?;
        Ok(bytes)
    }
    pub fn local(&self) -> Result<Option<Vec<u8>>> {
        self.verify()?;
        read(&self.root, ".stignore")
    }
    pub fn publish_policy(&self, expected: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.policy()?.as_deref() == expected,
            "Shared ignore policy changed since preview"
        );
        let directory = self
            .settings(true)?
            .context("Shared ignore directory missing")?;
        self.publish(&directory, "sync-ignore", expected, bytes)?;
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
    fn publish(
        &self,
        directory: &Directory,
        name: &str,
        expected: Option<&[u8]>,
        bytes: &[u8],
    ) -> Result<()> {
        self.verify()?;
        ensure!(
            read(directory, name)?.as_deref() == expected,
            "Ignore file changed since preview"
        );
        if expected == Some(bytes) {
            return Ok(());
        }
        match expected {
            Some(before) => {
                if let Some(prepared) =
                    directory.prepare_replace(OsStr::new(name), before, bytes)?
                {
                    prepared.commit()?;
                }
            }
            None => directory.create(OsStr::new(name), bytes)?,
        }
        self.verify()?;
        ensure!(
            read(directory, name)?.as_deref() == Some(bytes),
            "Ignore file readback changed"
        );
        Ok(())
    }
}
