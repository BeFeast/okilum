//! Preview-bound NTFS moves. Ancestors stay pinned; publication never replaces.
use crate::windows_files::{identity, information, move_no_replace, Directory};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Component, Path},
};
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;

type Identity = (u32, u32, u32);
fn parent(root: &Path, relative: &Path, markdown: bool) -> Result<(Directory, OsString)> {
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "Choose a path inside the open folder"
    );
    ensure!(
        !crate::vault::service_path(relative),
        "Service files cannot be moved"
    );
    ensure!(
        !markdown
            || relative
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md")),
        "Use a Markdown (.md) filename"
    );
    let _root = Directory::open(root)?;
    let directory = Directory::open(&root.join(relative.parent().context("Missing parent")?))?;
    let name = relative.file_name().context("Missing name")?.to_owned();
    directory.child(&name)?;
    Ok((directory, name))
}
fn vacant(directory: &Directory, name: &OsString) -> Result<()> {
    match std::fs::symlink_metadata(directory.child(name)?) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(_) => anyhow::bail!("The destination already exists; it will not be replaced"),
    }
}
fn snapshot(directory: &Directory, name: &OsString) -> Result<(Vec<u8>, Identity)> {
    let (_, bytes, info) = directory.read(name)?;
    Ok((bytes, identity(&info)))
}
pub struct Moved {
    pub warning: Option<String>,
}
pub struct MovePlan {
    source: Directory,
    source_name: OsString,
    target: Directory,
    target_name: OsString,
    expected: (Vec<u8>, Identity),
}
impl MovePlan {
    pub fn prepare(root: &Path, from: &Path, to: &Path) -> Result<Self> {
        ensure!(from != to, "Choose a different name or folder");
        let (source, source_name) = parent(root, from, true)?;
        let (target, target_name) = parent(root, to, true)?;
        vacant(&target, &target_name)?;
        let expected = snapshot(&source, &source_name)?;
        Ok(Self {
            source,
            source_name,
            target,
            target_name,
            expected,
        })
    }
    pub fn prepare_exact(root: &Path, from: &Path, to: &Path, bytes: &[u8]) -> Result<Self> {
        let plan = Self::prepare(root, from, to)?;
        ensure!(
            plan.expected.0 == bytes,
            "Source changed before move; recovery is retained"
        );
        Ok(plan)
    }
    pub fn commit(self) -> Result<Moved> {
        // Retain the read handle through publication to exclude in-place writes.
        // DELETE sharing permits publication; replacement races are checked below.
        let (_guard, bytes, info) = self.source.read(&self.source_name)?;
        ensure!(
            (bytes, identity(&info)) == self.expected,
            "The note changed after preview; nothing was moved"
        );
        move_no_replace(
            &self.source.child(&self.source_name)?,
            &self.target.child(&self.target_name)?,
        )?;
        Ok(Moved {
            warning: (snapshot(&self.target, &self.target_name).ok().as_ref()
                != Some(&self.expected))
            .then(|| {
                "The note moved, but an external change requires inspection before editing.".into()
            }),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectorySnapshot(Vec<Entry>);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    path: String,
    identity: Identity,
    attributes: u32,
    size: u64,
    modified: u64,
}
impl DirectorySnapshot {
    pub fn read(root: &Path, relative: &Path) -> Result<Self> {
        let (parent, name) = parent(root, relative, false)?;
        Self::at(&parent, &name)
    }
    fn at(parent: &Directory, name: &OsString) -> Result<Self> {
        fn walk(directory: &Directory, path: &str, entries: &mut Vec<Entry>) -> Result<()> {
            let info = directory.information()?;
            entries.push(Entry {
                path: path.into(),
                identity: identity(&info),
                attributes: info.dwFileAttributes,
                size: 0,
                modified: 0,
            });
            for entry in std::fs::read_dir(directory.path())? {
                let entry = entry?;
                let name = entry.file_name();
                let text = name.to_str().context("Move requires Unicode filenames")?;
                // Save recovery remains on the source NTFS volume with its DACL.
                // Rewrites generate these files; they are not canonical inventory.
                if text.starts_with(".tessera-save-") {
                    continue;
                }
                let next = if path.is_empty() {
                    text.to_owned()
                } else {
                    format!("{path}/{text}")
                };
                let child = directory.child(&name)?;
                if std::fs::symlink_metadata(&child)?.is_dir() {
                    walk(&Directory::open(&child)?, &next, entries)?;
                } else {
                    let file = directory.open_file(&name)?;
                    let info = information(&file)?;
                    entries.push(Entry {
                        path: next,
                        identity: identity(&info),
                        attributes: info.dwFileAttributes,
                        size: u64::from(info.nFileSizeHigh) << 32 | u64::from(info.nFileSizeLow),
                        modified: u64::from(info.ftLastWriteTime.dwHighDateTime) << 32
                            | u64::from(info.ftLastWriteTime.dwLowDateTime),
                    });
                }
            }
            Ok(())
        }
        let mut entries = vec![];
        walk(&Directory::open(&parent.child(name)?)?, "", &mut entries)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self(entries))
    }
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
            if rewritten.contains(&before.path) {
                ensure!(
                    after.attributes & FILE_ATTRIBUTE_DIRECTORY == 0,
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
    source: Directory,
    source_name: OsString,
    target: Directory,
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
        let (source, source_name) = parent(root, from, false)?;
        let (target, target_name) = parent(root, to, false)?;
        ensure!(
            DirectorySnapshot::at(&source, &source_name)? == *expected,
            "Folder changed after preview"
        );
        vacant(&target, &target_name)?;
        Ok(Self {
            source,
            source_name,
            target,
            target_name,
            expected: expected.clone(),
        })
    }
    pub fn commit(self) -> Result<Moved> {
        ensure!(
            DirectorySnapshot::at(&self.source, &self.source_name)? == self.expected,
            "Folder changed after preview; nothing was moved"
        );
        // Snapshot guards are released before rename. Only parents remain pinned;
        // do not block our own directory publication with a subtree guard.
        move_no_replace(
            &self.source.child(&self.source_name)?,
            &self.target.child(&self.target_name)?,
        )?;
        Ok(Moved {
            warning: (DirectorySnapshot::at(&self.target, &self.target_name)
                .ok()
                .as_ref()
                != Some(&self.expected))
            .then(|| {
                "The folder moved, but an external change requires inspection before editing."
                    .into()
            }),
        })
    }
}
