//! Explicit first-receive destination creation; never creates missing parents.
use std::path::Path;
#[cfg(any(target_os = "linux", test))]
use std::path::PathBuf;

use anyhow::{ensure, Context as _, Result};

/// Resolve a proposed leaf before creation so other-runtime overlap checks still
/// run before any filesystem write. An absent parent is never inferred or made.
#[cfg(any(target_os = "linux", test))]
pub(super) fn canonical_path(path: &Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .context("local Sync folder parent unavailable")?;
            let name = path
                .file_name()
                .context("local Sync folder name unavailable")?;
            Ok(parent
                .canonicalize()
                .context("local Sync folder parent unavailable")?
                .join(name))
        }
        Err(error) => Err(error).context("local Sync folder could not be checked"),
    }
}

pub(super) fn prepare(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("local Sync folder could not be created"),
    }
    let metadata =
        std::fs::symlink_metadata(path).context("local Sync folder could not be checked")?;
    ensure!(
        metadata.is_dir() && !metadata.is_symlink(),
        "local Sync folder must be a real directory"
    );
    let first = path
        .read_dir()
        .context("local Sync folder could not be checked")?
        .next()
        .transpose()
        .context("local Sync folder could not be checked")?;
    ensure!(first.is_none(), "unknown nonempty destination");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn sync_new_destination_creates_only_private_leaf_and_reuses_empty() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("Notes");
        assert!(!path.exists(), "new-folder precondition");
        assert_eq!(
            canonical_path(&path)?,
            root.path().canonicalize()?.join("Notes")
        );
        prepare(&path)?;
        assert_eq!(canonical_path(&path)?, path.canonicalize()?);
        assert!(path.read_dir()?.next().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!(std::fs::metadata(&path)?.mode() & 0o777, 0o700);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))?;
            prepare(&path)?;
            assert_eq!(std::fs::metadata(&path)?.mode() & 0o777, 0o750);
            // Exercise the /var -> /private/var case explicitly on either Unix host.
            let parent = root.path().join("parent");
            std::fs::create_dir(&parent)?;
            let alias = root.path().join("parent-alias");
            std::os::unix::fs::symlink(&parent, &alias)?;
            let proposed = alias.join("Notes");
            assert!(!proposed.exists());
            assert_eq!(
                canonical_path(&proposed)?,
                parent.canonicalize()?.join("Notes")
            );
            assert!(!proposed.exists(), "resolution must precede creation");
            prepare(&proposed)?;
            assert_eq!(canonical_path(&proposed)?, proposed.canonicalize()?);
            assert_eq!(std::fs::metadata(&proposed)?.mode() & 0o777, 0o700);
        }
        let nested = root.path().join("missing/Notes");
        assert!(canonical_path(&nested).is_err());
        assert!(prepare(&nested).is_err());
        assert!(!root.path().join("missing").exists());
        Ok(())
    }

    #[test]
    fn sync_new_destination_refuses_nonempty_file_and_symlink_without_writes() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("Notes");
        std::fs::create_dir(&path)?;
        let note = path.join("keep.md");
        std::fs::write(&note, b"original bytes\n")?;
        let error = prepare(&path).unwrap_err();
        assert!(super::super::user_error(&error).contains("Your files have not changed"));
        assert_eq!(std::fs::read(&note)?, b"original bytes\n");
        assert_eq!(path.read_dir()?.count(), 1);
        assert!(prepare(&note).is_err());
        let empty = root.path().join("empty");
        std::fs::create_dir(&empty)?;
        let alias = root.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&empty, &alias)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&empty, &alias)?;
        assert!(prepare(&alias).is_err());
        assert!(empty.read_dir()?.next().is_none());
        Ok(())
    }
}
