//! Bind every note operation to the directory opened with the editor.
use super::*;
use rustix::fs::{openat, Mode, OFlags, CWD};
use std::{ffi::OsStr, io::Read, path::Component};

pub(super) struct Directory {
    pub file: File,
    ancestors: Vec<(u64, u64)>,
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        anyhow::ensure!(path.is_absolute(), "Expected an absolute source folder");
        let mut file = File::from(openat(
            CWD,
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let mut ancestors = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    file = File::from(openat(
                        &file,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?);
                    let meta = file.metadata()?;
                    ancestors.push((meta.dev(), meta.ino()));
                }
                _ => bail!("Invalid source folder"),
            }
        }
        Ok(Self { file, ancestors })
    }
    pub fn validate(&self, path: &Path) -> Result<()> {
        let current = Self::open(path).context(
            "The note's folder changed. Your draft is preserved; restore the folder or copy your edits before reopening",
        )?;
        anyhow::ensure!(
            current.ancestors == self.ancestors,
            "The note's folder changed. Your draft is preserved; restore the folder or copy your edits before reopening"
        );
        Ok(())
    }
    pub fn read(&self, name: &OsStr) -> Result<(String, fs::Metadata)> {
        let mut file = open_regular_at(&self.file, name)?;
        let metadata = file.metadata()?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .context("Only lossless UTF-8 source editing is supported")?;
        Ok((text, metadata))
    }
    pub fn temporary(&self) -> Result<(File, String)> {
        let name = format!(".tessera-save-{}", uuid::Uuid::new_v4());
        let file = File::from(openat(
            &self.file,
            name.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?);
        // Deliberately no path-based destructor: after an exchange this name
        // belongs to the displaced inode and must survive every failure.
        Ok((file, name))
    }
}

pub(crate) fn open_regular_at(directory: &File, name: &OsStr) -> Result<File> {
    let file = File::from(openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "Editing requires a regular file without symlinks or hard links"
    );
    Ok(file)
}
