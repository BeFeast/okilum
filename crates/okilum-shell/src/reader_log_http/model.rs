use anyhow::{ensure, Context as _, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    pub name: String,
    pub url: String,
}
impl Source {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 128,
            "Enter a source name of at most 128 bytes"
        );
        ensure!(self.url.len() <= 4096, "Source URL exceeds 4096 bytes");
        let url = url::Url::parse(&self.url).context("Enter a complete HTTPS file URL")?;
        ensure!(
            url.scheme() == "https" && url.host_str().is_some(),
            "The log source must use HTTPS"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Credentials, query tokens and fragments are not supported in saved log URLs"
        );
        Ok(())
    }
}
fn path(state: &Path, vault: &Path) -> PathBuf {
    state.join("http-log-sources").join(format!(
        "{}.json",
        okilum_core::remote_log::vault_scope(vault)
    ))
}
pub(super) fn load(state: &Path, vault: &Path) -> Result<Vec<Source>> {
    let mut bytes = Vec::new();
    match std::fs::File::open(path(state, vault)) {
        Ok(file) => {
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    }
    ensure!(bytes.len() <= 1024 * 1024, "Saved log sources exceed 1 MiB");
    let sources: Vec<Source> = serde_json::from_slice(&bytes)?;
    ensure!(sources.len() <= 32, "Saved log source limit exceeded");
    for source in &sources {
        source.validate()?;
    }
    Ok(sources)
}
pub(super) fn save(state: &Path, vault: &Path, source: &Source) -> Result<Vec<Source>> {
    source.validate()?;
    let path = path(state, vault);
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut sources = load(state, vault)?;
    if let Some(old) = sources.iter_mut().find(|s| s.url == source.url) {
        *old = source.clone();
    } else {
        ensure!(sources.len() < 32, "At most 32 HTTPS log sources per vault");
        sources.push(source.clone());
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, &sources)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(sources)
}
