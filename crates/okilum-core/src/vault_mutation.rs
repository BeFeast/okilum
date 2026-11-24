//! Filesystem preflight for explicit structural changes in a vault.
use crate::vault_descriptor::{self, Lifecycle, NestedBoundary};
use anyhow::{bail, ensure, Context, Result};
use okilum_write_authority::{operation_for_directory, Authority, OperationGuard};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Validate the existing directory chain before resolving any writer authority.
/// An absent suffix is allowed only when the operation will create it.
pub fn validate_directories(root: &Path, relative: &Path, allow_missing: bool) -> Result<()> {
    ensure!(
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_))),
        "Choose a folder inside the vault"
    );
    let mut path = root.to_owned();
    ensure_real_directory(&path, &fs::symlink_metadata(&path)?)?;
    for part in relative.components() {
        path.push(part.as_os_str());
        match fs::symlink_metadata(&path) {
            Ok(metadata) => ensure_real_directory(&path, &metadata)?,
            Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error).context("Cannot inspect the destination folder"),
        }
    }
    Ok(())
}

fn ensure_real_directory(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        ensure!(
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "Choose a real folder, not a symbolic link or reparse point: {}",
            path.display()
        );
    }
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Choose a real folder, not a symbolic link: {}",
        path.display()
    );
    Ok(())
}

/// Refuse destructive operations on a folder or subtree containing a vault,
/// unit or archive. The outer inventory deliberately omits these descendants.
pub fn check_relocation(root: &Path, relative: &Path) -> Result<()> {
    validate_item(relative)?;
    let parent = relative.parent().context("Missing item parent")?;
    validate_directories(root, parent, false)?;
    let source = root.join(relative);
    let metadata = fs::symlink_metadata(&source)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    ensure_real_directory(&source, &metadata)?;
    let boundaries = vault_descriptor::declared_boundaries(root);
    for entry in walkdir::WalkDir::new(&source).follow_root_links(false) {
        let entry = entry.context("Cannot verify the folder before moving it")?;
        if !entry.file_type().is_dir() {
            continue;
        }
        let directory = entry.path();
        let path = crate::vault::note_path(directory.strip_prefix(root)?);
        let marker = vault_descriptor::path(directory);
        let descriptor = marker_exists(&marker)?;
        let obsidian = directory.join(".obsidian");
        let obsidian_marker = match fs::metadata(&obsidian) {
            Ok(metadata) => metadata.is_dir(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error).context("Cannot verify the nested vault marker"),
        };
        let lifecycle = Lifecycle::load(directory);
        ensure!(
            lifecycle != Lifecycle::Uncertain,
            "Cannot move a folder whose vault lifecycle cannot be verified: {}",
            directory.display()
        );
        let archive = lifecycle == Lifecycle::Archive;
        let declared =
            vault_descriptor::boundary_at(root, &boundaries, &path).map(|(_, kind)| kind);
        if archive || descriptor || obsidian_marker || declared.is_some() {
            let folder_name = directory.file_name().unwrap_or_default().to_string_lossy();
            let (kind, name) = match declared {
                Some(NestedBoundary::Vault(name)) => ("vault", name.clone()),
                Some(NestedBoundary::Unit) if !archive => ("unit", folder_name.into_owned()),
                _ => {
                    let name = vault_descriptor::read(directory)
                        .and_then(|descriptor| descriptor.get("name")?.as_str().map(str::to_owned))
                        .unwrap_or_else(|| folder_name.into_owned());
                    ("vault", name)
                }
            };
            let action = if directory == source {
                "The selected folder is"
            } else {
                "The selected folder contains"
            };
            let archive = if archive { " (read-only archive)" } else { "" };
            bail!("{action} a separate {kind} \"{name}\"{archive}");
        }
    }
    Ok(())
}

fn validate_item(relative: &Path) -> Result<()> {
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "Choose an item inside the vault"
    );
    Ok(())
}

fn marker_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("Cannot verify the nested vault descriptor"),
    }
}

pub(crate) struct MoveScope {
    authority: Authority,
    root: PathBuf,
    from: PathBuf,
    to: PathBuf,
}

impl MoveScope {
    pub(crate) fn prepare(root: &Path, from: &Path, to: &Path) -> Result<Self> {
        validate_item(from)?;
        validate_item(to)?;
        let root = std::path::absolute(root)?;
        let source = from.parent().context("Missing source folder")?;
        let target = to.parent().context("Missing destination folder")?;
        validate_directories(&root, source, false)?;
        validate_directories(&root, target, false)?;
        Lifecycle::check_ancestors(&root.join(source))?;
        Lifecycle::check_ancestors(&root.join(target))?;
        check_relocation(&root, from)?;
        Ok(Self {
            authority: Authority::for_root(&root)?,
            root,
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }

    pub(crate) fn admit(&self) -> Result<MoveAdmission> {
        let source = self.from.parent().context("Missing source folder")?;
        let target = self.to.parent().context("Missing destination folder")?;
        validate_directories(&self.root, source, false)?;
        validate_directories(&self.root, target, false)?;
        let root = self.authority.operation()?;
        let source = operation_for_directory(&self.root.join(source))?;
        let target = operation_for_directory(&self.root.join(target))?;
        check_relocation(&self.root, &self.from)?;
        Ok(MoveAdmission {
            _target: target,
            _source: source,
            _root: root,
        })
    }

    #[cfg(unix)]
    pub(crate) fn check_pinned_parents(&self, source: &fs::File, target: &fs::File) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        for (item, pinned) in [(&self.from, source), (&self.to, target)] {
            let relative = item.parent().context("Missing move parent")?;
            let path = self.root.join(relative);
            let named = fs::symlink_metadata(&path)?;
            let pinned = pinned.metadata()?;
            ensure!(
                named.is_dir() && (named.dev(), named.ino()) == (pinned.dev(), pinned.ino()),
                "A move parent changed after preview; nothing was moved"
            );
        }
        Ok(())
    }
}

pub(crate) struct MoveAdmission {
    _target: OperationGuard,
    _source: OperationGuard,
    _root: OperationGuard,
}
