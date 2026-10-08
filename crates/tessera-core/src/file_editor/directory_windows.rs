//! Revision-bound Windows parents; each operation pins the complete ancestry.
use anyhow::{ensure, Context, Result};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
pub(super) struct Directory {
    path: PathBuf,
    identities: Vec<(u32, u32, u32)>,
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        let native = crate::windows_files::Directory::open(path)?;
        Ok(Self {
            path: path.to_owned(),
            identities: native.identities()?,
        })
    }
    pub fn pin(&self) -> Result<crate::windows_files::Directory> {
        let native = crate::windows_files::Directory::open(&self.path)
            .context("The note's folder changed; your draft is preserved")?;
        ensure!(
            native.identities()? == self.identities,
            "The note's folder changed; your draft is preserved"
        );
        Ok(native)
    }
    pub fn validate(&self, path: &Path) -> Result<()> {
        let native = crate::windows_files::Directory::open(path)?;
        ensure!(
            native.identities()? == self.identities,
            "The note's folder changed; your draft is preserved"
        );
        self.pin()?;
        Ok(())
    }
    pub fn read(&self, name: &OsStr) -> Result<(String, (u32, u32, u32))> {
        let native = self.pin()?;
        let (_, bytes, info) = native.read(name)?;
        Ok((
            String::from_utf8(bytes).context("Only lossless UTF-8 source editing is supported")?,
            crate::windows_files::identity(&info),
        ))
    }
}
