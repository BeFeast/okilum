//! Authored Feedback recovery, separate from rebuildable indexes. Each immutable
//! revision is published with create-if-absent semantics; stale editors cannot
//! overwrite a newer draft. Opening a draft never sends it.
use super::{Video, MAX_IMAGE};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Saved {
    pub version: u32,
    pub text: String,
    pub feature: bool,
    pub include_log: bool,
    pub video: Option<Video>,
    pub screenshot: Option<String>,
    /// Sending is deliberately recovered as uncertain, never as queued work.
    pub status: String,
    pub sent: bool,
}
#[derive(Clone)]
pub(crate) struct Store {
    pub directory: PathBuf,
    revision: u64,
}
impl Store {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            revision: 0,
        }
    }
    fn latest(&self) -> Result<u64> {
        if !self.directory.try_exists()? {
            return Ok(0);
        }
        let mut latest = 0;
        for entry in fs::read_dir(&self.directory)? {
            let name = entry?.file_name();
            if let Some(revision) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok())
            {
                latest = latest.max(revision);
            }
        }
        Ok(latest)
    }
    pub fn save(&mut self, mut saved: Saved, screenshot: Option<&[u8]>) -> Result<()> {
        fs::create_dir_all(&self.directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))?;
        }
        ensure!(
            self.latest()? == self.revision,
            "Feedback draft has a newer revision. Reopen it before editing or retrying."
        );
        if let Some(bytes) = screenshot {
            use sha2::{Digest, Sha256};
            ensure!(
                bytes.len() <= MAX_IMAGE,
                "Feedback screenshot exceeds its limit."
            );
            let name = format!("{:x}.png", Sha256::digest(bytes));
            if !self.directory.join(&name).try_exists()? {
                publish(&self.directory, &name, bytes)?;
            } else {
                ensure!(
                    read_bounded(&self.directory.join(&name), MAX_IMAGE as u64)? == bytes,
                    "Saved screenshot changed. Its files have been kept."
                );
            }
            saved.screenshot = Some(name);
        }
        let revision = self
            .revision
            .checked_add(1)
            .context("Feedback draft revision overflow")?;
        let bytes = serde_json::to_vec(&saved)?;
        ensure!(
            bytes.len() <= 128 * 1024,
            "Feedback draft exceeds its recovery size limit."
        );
        publish(&self.directory, &format!("{revision}.json"), &bytes)?;
        self.revision = revision;
        Ok(())
    }
    pub fn load(directory: PathBuf) -> Result<(Self, Saved)> {
        let mut store = Self::new(directory);
        store.revision = store.latest()?;
        ensure!(
            store.revision > 0,
            "Feedback draft has no committed revision."
        );
        let bytes = read_bounded(
            &store.directory.join(format!("{}.json", store.revision)),
            128 * 1024,
        )?;
        let mut saved: Saved = serde_json::from_slice(&bytes).context("Read Feedback draft")?;
        ensure!(
            saved.version == 1,
            "Unsupported Feedback draft version; its files have been kept."
        );
        if let Some(video) = &mut saved.video {
            video.mime = super::video_mime(&video.path)
                .context("Unsupported saved video type; its selection has been kept.")?;
        }
        Ok((store, saved))
    }
    pub fn screenshot(&self, saved: &Saved) -> Result<Option<Vec<u8>>> {
        let Some(name) = &saved.screenshot else {
            return Ok(None);
        };
        ensure!(
            name.len() == 68
                && name.ends_with(".png")
                && name.as_bytes()[..64].iter().all(|b| b.is_ascii_hexdigit()),
            "Invalid saved screenshot identity."
        );
        let bytes = read_bounded(&self.directory.join(name), MAX_IMAGE as u64)?;
        use sha2::{Digest, Sha256};
        ensure!(
            format!("{:x}.png", Sha256::digest(&bytes)) == *name,
            "Saved Feedback screenshot changed; its files have been kept."
        );
        Ok(Some(bytes))
    }
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Feedback recovery file exceeds its limit."
    );
    Ok(bytes)
}
fn publish(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    // persist_noclobber atomically refuses a concurrent revision. A crash before
    // this publication leaves the last committed revision readable.
    temporary
        .persist_noclobber(directory.join(name))
        .map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}
pub(crate) type Discovered = (Vec<(Store, Saved)>, Vec<String>);

pub(crate) fn discover(root: &Path) -> Result<Discovered> {
    if !root.try_exists()? {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut result = Vec::new();
    let mut errors = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        // A corrupt committed draft is visible as an error, never silently
        // treated as an empty draft or overwritten by a newly allocated one.
        if Store::new(entry.path()).latest()? == 0 {
            continue;
        }
        match Store::load(entry.path()) {
            Ok(loaded) if !loaded.1.sent => result.push(loaded),
            Ok(_) => {}
            Err(error) => errors.push(format!(
                "Couldn't reopen a saved Feedback draft: {error}. Its files have been kept."
            )),
        }
    }
    result.sort_by(|a, b| a.0.directory.cmp(&b.0.directory));
    Ok((result, errors))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn saved() -> Saved {
        Saved {
            version: 1,
            text: "Authored Ελληνικά draft".into(),
            feature: true,
            include_log: true,
            video: None,
            screenshot: None,
            status: "Sending: delivery may have happened. Check before retrying.".into(),
            sent: false,
        }
    }
    #[test]
    fn restart_recovers_exact_authored_state_and_refuses_stale_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("draft");
        let mut store = Store::new(path.clone());
        store
            .save(saved(), Some(b"original screenshot bytes"))
            .unwrap();
        let (mut stale, _) = Store::load(path.clone()).unwrap();
        let (mut reopened, recovered) = Store::load(path.clone()).unwrap();
        assert_eq!(recovered.text, saved().text);
        assert!(recovered.feature && recovered.include_log);
        assert!(recovered.status.contains("Check before retrying"));
        assert_eq!(
            reopened.screenshot(&recovered).unwrap().unwrap(),
            b"original screenshot bytes"
        );
        reopened
            .save(recovered.clone(), Some(b"replacement screenshot"))
            .unwrap();
        assert!(stale.save(saved(), None).is_err());
        fs::write(path.join(".interrupted-unpublished"), b"partial").unwrap();
        assert_eq!(Store::load(path.clone()).unwrap().0.revision, 2);
        assert_eq!(discover(dir.path()).unwrap().0.len(), 1);
        let (mut store, mut recovered) = Store::load(path).unwrap();
        recovered.sent = true;
        store.save(recovered, None).unwrap();
        assert!(discover(dir.path()).unwrap().0.is_empty());
    }
    #[test]
    fn corrupt_selected_screenshot_is_visible_and_never_substituted() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::new(dir.path().join("draft"));
        store.save(saved(), Some(b"approved")).unwrap();
        let (store, saved) = Store::load(store.directory).unwrap();
        fs::write(
            store.directory.join(saved.screenshot.as_ref().unwrap()),
            b"replaced",
        )
        .unwrap();
        assert!(store.screenshot(&saved).is_err());
    }
    #[test]
    fn restarted_selection_refuses_same_size_changed_bytes_and_reselection_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.webm");
        fs::write(&path, b"original").unwrap();
        let selected = super::super::video_file(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let mut state = saved();
        state.video = Some(selected.clone());
        let mut store = Store::new(dir.path().join("authored-draft"));
        store.save(state, None).unwrap();
        let directory = store.directory.clone();
        drop(store);
        fs::write(&path, b"replaced").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let (_, recovered) = Store::load(directory).unwrap();
        let recovered = recovered.video.unwrap();
        assert_eq!(
            recovered, selected,
            "restart retains the exact original selection"
        );
        assert!(super::super::selected_video_snapshot(&recovered).is_err());
        let explicitly_reselected = super::super::video_file(&path).unwrap();
        let mut frozen = super::super::selected_video_snapshot(&explicitly_reselected).unwrap();
        fs::write(&path, b"mutated!").unwrap();
        let mut bytes = Vec::new();
        frozen.read_to_end(&mut bytes).unwrap();
        assert_eq!(
            bytes, b"replaced",
            "source mutation cannot replace approved upload bytes"
        );
    }
    #[test]
    fn byte_identical_replacement_is_a_new_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.mov");
        fs::write(&path, b"original bytes").unwrap();
        let selected = super::super::video_file(&path).unwrap();
        let replacement = dir.path().join("replacement.mov");
        fs::write(&replacement, b"original bytes").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert_ne!(super::super::video_file(&path).unwrap(), selected);
        assert!(super::super::selected_video_snapshot(&selected).is_err());
    }
    #[test]
    fn corrupt_committed_draft_does_not_hide_healthy_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let mut healthy = Store::new(dir.path().join("healthy"));
        healthy.save(saved(), None).unwrap();
        fs::create_dir(dir.path().join("corrupt")).unwrap();
        fs::write(dir.path().join("corrupt/1.json"), b"broken").unwrap();
        fs::create_dir(dir.path().join("unpublished")).unwrap();
        let (drafts, errors) = discover(dir.path()).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(dir.path().join("corrupt/1.json").exists());
    }
}
