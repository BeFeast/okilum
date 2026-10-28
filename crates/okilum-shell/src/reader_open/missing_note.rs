//! Explicit OS/CLI paths may create a missing Markdown file. Pure validation,
//! history restoration and derived-cache probing never call this operation.
use anyhow::{bail, Context as _, Result};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(super) struct MissingParent(PathBuf);
impl std::fmt::Display for MissingParent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The containing folder {} does not exist.",
            okilum_core::vault::display_path(&self.0)
        )
    }
}
impl std::error::Error for MissingParent {}

pub(crate) fn resolve_explicit_path(
    path: &Path,
    explicit_root: Option<&Path>,
) -> Result<(PathBuf, bool)> {
    // A logical iCloud note can already exist as a placeholder. Hydrate that
    // identity before deciding it is missing; never replace it with an empty file.
    #[cfg(target_os = "macos")]
    let materialized = crate::reader_icloud::materialize(path, || Ok(()))?;
    #[cfg(target_os = "macos")]
    let path = materialized.as_path();
    let missing = match fs::symlink_metadata(path) {
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error).context("The requested path cannot be inspected"),
    };
    let markdown = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    if !missing || !markdown {
        return Ok((
            okilum_core::vault::canonical_root(path)
                .context("The requested path is missing or inaccessible")?,
            false,
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = okilum_core::vault::canonical_root(parent).map_err(|error| {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::NotFound)
        {
            error.context(MissingParent(parent.to_path_buf()))
        } else {
            error.context(format!(
                "The containing folder {} cannot be opened",
                okilum_core::vault::display_path(parent)
            ))
        }
    })?;
    if !parent.is_dir() {
        bail!(
            "The containing path {} is not a folder",
            okilum_core::vault::display_path(&parent)
        );
    }
    if let Some(root) = explicit_root {
        let root = okilum_core::vault::canonical_root(root)
            .context("The selected root is inaccessible")?;
        if !root.is_dir() || !parent.starts_with(&root) {
            bail!("The requested path is outside the selected root");
        }
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("The document path is not valid UTF-8")?;
    let destination = parent.join(name);
    let created = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
    {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error).context("The new Markdown file could not be created"),
    };
    Ok((okilum_core::vault::canonical_root(&destination)?, created))
}

#[cfg(test)]
mod tests;
