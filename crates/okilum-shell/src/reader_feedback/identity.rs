//! Private per-install feedback identity, outside disposable indexes and drafts.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Data {
    pub install_id: uuid::Uuid,
    pub reporter_name: String,
}
impl std::fmt::Debug for Data {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateFeedbackIdentity")
            .field("reporter_supplied", &!self.reporter_name.is_empty())
            .finish_non_exhaustive()
    }
}
impl Data {
    pub fn anonymous() -> Self {
        Self {
            install_id: uuid::Uuid::new_v4(),
            reporter_name: String::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.install_id.get_version_num() == 4
                && self.install_id.get_variant() == uuid::Variant::RFC4122,
            "Feedback installation ID is invalid. Its file was kept; restore it before sending."
        );
        ensure!(
            self.reporter_name.chars().count() <= 128
                && !self.reporter_name.chars().any(char::is_control),
            "Name or GitHub must contain at most 128 characters without line breaks."
        );
        Ok(())
    }
}

pub(super) struct Store {
    path: PathBuf,
    pub data: Data,
}
impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        let parent = path
            .parent()
            .context("Feedback identity folder unavailable")?;
        fs::create_dir_all(parent)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        lock.lock()?;
        let data = if path.try_exists()? {
            ensure!(
                fs::metadata(&path)?.len() <= 4096,
                "Feedback identity file is oversized; its contents were kept."
            );
            let data: Data = serde_json::from_slice(&fs::read(&path)?)?;
            data.validate()?;
            data
        } else {
            let data = Data::anonymous();
            Self::persist(&path, &data)?;
            data
        };
        Ok(Self { path, data })
    }
    fn persist(path: &std::path::Path, data: &Data) -> Result<()> {
        data.validate()?;
        let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        temp.write_all(&serde_json::to_vec(data)?)?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|error| error.error)?;
        #[cfg(unix)]
        fs::File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    }
    pub fn remember(&mut self, reporter: &str) -> Result<()> {
        let reporter = reporter.trim();
        if reporter == self.data.reporter_name {
            return Ok(()); // Closing an unchanged older window cannot overwrite a newer name.
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        lock.lock()?;
        ensure!(
            fs::metadata(&self.path)?.len() <= 4096,
            "Feedback identity file is oversized; its contents were kept."
        );
        let disk: Data = serde_json::from_slice(&fs::read(&self.path)?)?;
        disk.validate()?;
        ensure!(
            disk.install_id == self.data.install_id,
            "Feedback installation identity changed. Reopen the app before sending."
        );
        let data = Data {
            install_id: disk.install_id,
            reporter_name: reporter.into(),
        };
        Self::persist(&self.path, &data)?;
        self.data = data;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_survives_reopen_and_name_clear_without_using_drafts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("identity.json");
        let mut first = Store::open(path.clone()).unwrap();
        let id = first.data.install_id;
        first.remember("  synthetic-reporter  ").unwrap();
        let mut second = Store::open(path.clone()).unwrap();
        assert_eq!(second.data.install_id, id);
        assert_eq!(second.data.reporter_name, "synthetic-reporter");
        second.remember("").unwrap();
        assert_eq!(Store::open(path).unwrap().data.install_id, id);
        assert!(second.data.reporter_name.is_empty());
        assert!(!format!("{:?}", first.data).contains("synthetic-reporter"));
        assert!(!format!("{:?}", first.data).contains(&id.to_string()));
    }
    #[test]
    fn concurrent_first_opens_choose_one_id_and_corrupt_state_is_not_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("identity.json");
        let threads = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || Store::open(path).unwrap().data.install_id)
            })
            .collect::<Vec<_>>();
        let ids = threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>();
        assert!(ids.iter().all(|id| *id == ids[0]));
        fs::write(&path, b"broken state").unwrap();
        assert!(Store::open(path.clone()).is_err());
        assert_eq!(fs::read(path).unwrap(), b"broken state");
    }
    #[test]
    fn invalid_names_preserve_the_saved_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("identity.json");
        let mut store = Store::open(path.clone()).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(store.remember("bad\nname").is_err());
        assert!(store.remember(&"a".repeat(129)).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }
}
