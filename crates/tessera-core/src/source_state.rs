//! Atomic, flushed application-state records for desktop source operations.
use anyhow::{Context, Result};
use std::path::Path;

pub(crate) fn persist(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing state folder")?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::{fs::File, io::Write};
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
        File::open(parent)?.sync_all()?;
    }
    #[cfg(windows)]
    {
        use crate::windows_files::{Directory, Replacement};
        let directory = Directory::open(parent)?;
        let name = path.file_name().context("Missing state filename")?;
        match directory.read(name) {
            Ok((_, current, _)) => {
                let plan = directory
                    .prepare_replace(name, &current, bytes)?
                    .context("Application state changed while preparing a write")?;
                match plan.commit()? {
                    Replacement::Saved { preimage } => {
                        let _ = std::fs::remove_file(preimage);
                    }
                    Replacement::Conflict => {
                        anyhow::bail!("Application state changed during a write")
                    }
                }
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                directory.create(name, bytes)?
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
