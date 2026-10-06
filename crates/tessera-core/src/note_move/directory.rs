//! Descriptor-relative, non-replacing directory moves with revision-bound inventory.
use super::*;
use anyhow::ensure;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectorySnapshot(Vec<Entry>);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    path: Vec<u8>,
    dev: u64,
    ino: u64,
    mode: u32,
    size: u64,
    mtime: i64,
    nanos: i64,
}
// rustix stat field widths differ between Linux and macOS.
#[allow(clippy::unnecessary_cast)]
impl DirectorySnapshot {
    pub fn read(root: &Path, relative: &Path) -> Result<Self> {
        let (parent, name) = parent_any(root, relative)?;
        Self::at(&parent, &name)
    }
    fn at(parent: &File, name: &OsString) -> Result<Self> {
        let dir = File::from(openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let mut entries = vec![];
        fn walk(dir: &File, path: Vec<u8>, entries: &mut Vec<Entry>) -> Result<()> {
            let meta = dir.metadata()?;
            entries.push(Entry {
                path: path.clone(),
                dev: meta.dev(),
                ino: meta.ino(),
                mode: meta.mode(),
                size: 0,
                mtime: 0,
                nanos: 0,
            });
            for child in rustix::fs::Dir::read_from(dir)? {
                let child = child?;
                let name = child.file_name();
                if matches!(name.to_bytes(), b"." | b"..") {
                    continue;
                }
                let stat = rustix::fs::statat(dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
                let mut next = path.clone();
                if !next.is_empty() {
                    next.push(b'/');
                }
                next.extend_from_slice(name.to_bytes());
                if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                    == rustix::fs::FileType::Directory
                {
                    let child = File::from(openat(
                        dir,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?);
                    walk(&child, next, entries)?;
                } else {
                    entries.push(Entry {
                        path: next,
                        dev: stat.st_dev as u64,
                        ino: stat.st_ino as u64,
                        mode: stat.st_mode as u32,
                        size: stat.st_size as u64,
                        mtime: stat.st_mtime as i64,
                        nanos: stat.st_mtime_nsec as i64,
                    });
                }
            }
            Ok(())
        }
        walk(&dir, vec![], &mut entries)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self(entries))
    }
    /// Only explicitly rewritten regular files may acquire a new revision.
    /// Other entries, binary assets, directory identities and the inventory stay bound.
    pub fn validate_except(&self, current: &Self, rewritten: &[String]) -> Result<()> {
        ensure!(
            self.0.len() == current.0.len(),
            "Folder contents changed; preview again"
        );
        for (before, after) in self.0.iter().zip(&current.0) {
            ensure!(
                before.path == after.path,
                "Folder contents changed; preview again"
            );
            if rewritten.iter().any(|p| p.as_bytes() == before.path) {
                ensure!(
                    rustix::fs::FileType::from_raw_mode(after.mode as _)
                        == rustix::fs::FileType::RegularFile,
                    "Rewritten source is no longer a regular file"
                );
            } else {
                ensure!(before == after, "Folder entry changed; preview again");
            }
        }
        Ok(())
    }
}

pub struct DirectoryMovePlan {
    source_dir: File,
    source_name: OsString,
    target_dir: File,
    target_name: OsString,
    expected: DirectorySnapshot,
}
impl DirectoryMovePlan {
    pub fn prepare(
        root: &Path,
        from: &Path,
        to: &Path,
        expected: &DirectorySnapshot,
    ) -> Result<Self> {
        ensure!(
            from != to && !to.starts_with(from),
            "Choose a different folder outside the moved subtree"
        );
        ensure!(
            !crate::vault::service_path(from) && !crate::vault::service_path(to),
            "Service folders cannot be moved"
        );
        let (source_dir, source_name) = parent_any(root, from)?;
        let (target_dir, target_name) = parent_any(root, to)?;
        ensure!(
            DirectorySnapshot::at(&source_dir, &source_name)? == *expected,
            "Folder changed after preview"
        );
        match rustix::fs::statat(
            &target_dir,
            &target_name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => bail!("The destination already exists; it will not be replaced"),
            Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Self {
            source_dir,
            source_name,
            target_dir,
            target_name,
            expected: expected.clone(),
        })
    }
    pub fn commit(self) -> Result<Moved> {
        ensure!(
            DirectorySnapshot::at(&self.source_dir, &self.source_name)? == self.expected,
            "Folder changed after preview; nothing was moved"
        );
        renameat_with(
            &self.source_dir,
            &self.source_name,
            &self.target_dir,
            &self.target_name,
            RenameFlags::NOREPLACE,
        )
        .context(
            "Nothing was moved: destination exists or the filesystem refused a safe folder move",
        )?;
        let mut warnings = vec![];
        if DirectorySnapshot::at(&self.target_dir, &self.target_name)
            .ok()
            .as_ref()
            != Some(&self.expected)
        {
            warnings.push(
                "The folder moved, but an external change requires inspection before editing.",
            );
        }
        if self.target_dir.sync_all().is_err() || self.source_dir.sync_all().is_err() {
            warnings.push("The folder moved, but directory sync failed. Check the destination.");
        }
        Ok(Moved {
            warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
        })
    }
}
