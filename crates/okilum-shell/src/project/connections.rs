use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use anyhow::{Context, Result};
use okilum_t3::{manifest::AreaBinding, Environment};

#[cfg(test)]
pub(super) fn area_for(vault: &Path, note: &Path) -> Result<PathBuf> {
    let mut directory = note.parent().unwrap_or(vault);
    while directory.starts_with(vault) {
        match std::fs::metadata(directory.join("ops/project.yaml")) {
            Ok(metadata) if metadata.is_file() => return Ok(directory.to_owned()),
            Ok(_) => anyhow::bail!("The area's project manifest is not a file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Cannot read this area's project manifest"),
        }
        if directory == vault {
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent;
    }
    Ok(vault.to_owned())
}

pub(super) fn load(state: &Path, binding: &AreaBinding) -> Result<Option<Environment>> {
    let source = match std::fs::read(state.join("t3-connections.json")) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Cannot read saved T3 connections"),
    };
    let saved: BTreeMap<String, String> =
        serde_json::from_slice(&source).context("Saved T3 connections are unreadable")?;
    saved
        .get(&binding.environment)
        .map(|origin| Environment::new(&binding.environment, origin))
        .transpose()
}

pub(super) fn save(state: &Path, env: &Environment) -> Result<()> {
    std::fs::create_dir_all(state)?;
    let path = state.join("t3-connections.json");
    let mut saved: BTreeMap<String, String> = match std::fs::read(&path) {
        Ok(source) => {
            serde_json::from_slice(&source).context("Saved T3 connections are unreadable")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error).context("Cannot read saved T3 connections"),
    };
    saved.insert(env.alias.clone(), env.origin());
    let temporary = state.join(format!(".t3-connections-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&saved)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        #[cfg(unix)]
        std::fs::File::open(state)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}
