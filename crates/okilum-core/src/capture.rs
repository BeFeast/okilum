//! Explicit local capture into an existing, user-selected vault folder.
//! Operational records belong outside the disposable index. No reader protocol IO.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    pub operation_id: String,
    pub path: PathBuf,
}

/// One authored capture. Keep this value on failure: a new attempt must not
/// silently allocate another destination after publication became uncertain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capture {
    receipt: Receipt,
    text: String,
}

impl Capture {
    pub fn new(folder: &Path, text: String) -> Result<Self> {
        if text.trim().is_empty() {
            bail!("Enter capture text before saving");
        }
        if folder
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!("Choose a capture folder inside the vault");
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        let path = folder.join(format!("Capture-{operation_id}.md"));
        if crate::vault::service_path(&path) {
            bail!("Choose a capture folder outside service files");
        }
        Ok(Self {
            receipt: Receipt { operation_id, path },
            text,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn path(&self) -> &Path {
        &self.receipt.path
    }

    fn validate(&self) -> Result<()> {
        let id = uuid::Uuid::parse_str(&self.receipt.operation_id)?;
        anyhow::ensure!(
            id.to_string() == self.receipt.operation_id,
            "Invalid capture identity"
        );
        anyhow::ensure!(
            self.receipt.path.file_name().and_then(|p| p.to_str())
                == Some(format!("Capture-{id}.md").as_str()),
            "Invalid capture filename"
        );
        anyhow::ensure!(
            self.receipt
                .path
                .components()
                .all(|p| matches!(p, Component::Normal(_)))
                && !crate::vault::service_path(&self.receipt.path),
            "Invalid capture path"
        );
        anyhow::ensure!(!self.text.trim().is_empty(), "Capture text is empty");
        Ok(())
    }

    /// Preserve a closed composer before any publication. Immutable drafts remain
    /// discoverable after a restart and are superseded by their completion receipt.
    pub fn checkpoint(&self, root: &Path, records: &Path) -> Result<()> {
        self.validate()?;
        prepare_records(records)?;
        let record = Record {
            root: root.canonicalize()?,
            capture: self.clone(),
        };
        persist_equal(
            records,
            &format!("{}.draft.json", self.receipt.operation_id),
            &record,
        )
    }

    /// Retry the same durable operation, never allocate another destination.
    /// A completion receipt is authoritative even if the user later edits/deletes
    /// the note. An unfinished publication requires exact source reconciliation.
    pub fn publish(&self, root: &Path, drafts: &Path, records: &Path) -> Result<Receipt> {
        self.validate()?;
        prepare_records(records)?;
        let record = Record {
            root: root.canonicalize()?,
            capture: self.clone(),
        };
        let intent = records.join(format!("{}.intent.json", self.receipt.operation_id));
        let receipt_name = format!("{}.receipt.json", self.receipt.operation_id);
        let prior = read_optional::<Record>(&intent)?;
        if let Some(prior) = &prior {
            anyhow::ensure!(
                prior == &record,
                "Capture intent belongs to different text or vault"
            );
            if let Some(receipt) = read_optional::<Receipt>(&records.join(&receipt_name))? {
                anyhow::ensure!(
                    receipt == self.receipt,
                    "Capture receipt does not match its intent"
                );
                #[cfg(unix)]
                std::fs::File::open(records)?.sync_all()?;
                return Ok(receipt);
            }
        } else {
            anyhow::ensure!(
                !records.join(&receipt_name).try_exists()?,
                "Capture receipt has no intent"
            );
        }
        let _reservation = crate::file_editor::FileEditor::reserve_destination(
            &root.join(&self.receipt.path),
            drafts,
        )?;
        if prior.is_none() {
            // A pre-existing file, including identical bytes, is never adopted by
            // the first save. NOREPLACE still closes a subsequent creation race.
            anyhow::ensure!(
                read_destination(root, self.path())?.is_none(),
                "Capture destination already exists"
            );
            persist_equal(
                records,
                &format!("{}.intent.json", self.receipt.operation_id),
                &record,
            )?;
        }
        match read_destination(root, self.path())? {
            Some(bytes) => anyhow::ensure!(bytes == self.text.as_bytes(), "Capture destination changed; authored text is retained, and newer source will not be overwritten"),
            None => crate::note_files::create_with_source(root, self.path(), self.text.as_bytes())
                .context("Capture was not acknowledged; retry this same durable operation")?,
        }
        persist_equal(records, &receipt_name, &self.receipt)
            .context("Note exists; retry to recover its completion receipt")?;
        Ok(self.receipt.clone())
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Record {
    root: PathBuf,
    capture: Capture,
}

/// Recover drafts and unfinished saves for this exact vault. Corruption is an
/// error rather than an empty inbox. Completed operations never become drafts.
pub fn recover(root: &Path, records: &Path) -> Result<Vec<Capture>> {
    let root = root.canonicalize()?;
    let entries = match std::fs::read_dir(records) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut captures = std::collections::BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".intent.json") && !name.ends_with(".draft.json") {
            continue;
        }
        let record: Record =
            read_optional(&entry.path())?.context("Capture record disappeared during recovery")?;
        record.capture.validate()?;
        anyhow::ensure!(
            record.root == root,
            "Capture recovery directory belongs to another vault"
        );
        let capture = record.capture;
        let id = &capture.receipt.operation_id;
        anyhow::ensure!(
            name == format!("{id}.intent.json") || name == format!("{id}.draft.json"),
            "Capture record identity mismatch"
        );
        if let Some(receipt) =
            read_optional::<Receipt>(&records.join(format!("{id}.receipt.json")))?
        {
            anyhow::ensure!(
                receipt == capture.receipt,
                "Capture receipt identity mismatch"
            );
            continue;
        }
        if let Some(previous) = captures.insert(id.clone(), capture.clone()) {
            anyhow::ensure!(previous == capture, "Capture draft and intent disagree");
        }
    }
    Ok(captures.into_values().collect())
}

/// One composer session. Checkpoints must be submitted serially, in input order.
/// The stable attempt is persisted before publishing, so restart/retry cannot
/// turn one Save into two canonical notes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposerDraft {
    pub id: String,
    pub text: String,
    pub folder: Option<PathBuf>,
    pub attempt: Option<Capture>,
}

impl Default for ComposerDraft {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            text: String::new(),
            folder: None,
            attempt: None,
        }
    }
}

impl ComposerDraft {
    pub fn prepare_save(&mut self) -> Result<()> {
        if let Some(attempt) = &self.attempt {
            anyhow::ensure!(
                attempt.text() == self.text && attempt.path().parent() == self.folder.as_deref(),
                "Retry must retain the original capture text and folder"
            );
        } else {
            self.attempt = Some(Capture::new(
                self.folder.as_deref().context("Choose a capture folder")?,
                self.text.clone(),
            )?);
        }
        Ok(())
    }

    pub fn persist(&self, root: &Path, records: &Path) -> Result<()> {
        anyhow::ensure!(
            uuid::Uuid::parse_str(&self.id)?.to_string() == self.id,
            "Invalid composer identity"
        );
        if let Some(attempt) = &self.attempt {
            attempt.validate()?;
            anyhow::ensure!(
                attempt.text() == self.text && attempt.path().parent() == self.folder.as_deref(),
                "Composer and pending capture disagree"
            );
        }
        prepare_records(records)?;
        let name = format!("{}.composer.json", self.id);
        let _lock = crate::file_editor::FileEditor::reserve_destination(
            &records.join(&name),
            &records.join("locks"),
        )?;
        let record = ComposerRecord {
            root: root.canonicalize()?,
            draft: self.clone(),
        };
        if let Some(prior) = read_optional::<ComposerRecord>(&records.join(&name))? {
            anyhow::ensure!(
                prior.root == record.root && prior.draft.id == self.id,
                "Composer draft belongs to another vault"
            );
            if let Some(attempt) = prior.draft.attempt {
                anyhow::ensure!(self.attempt.as_ref() == Some(&attempt), "Another save already owns this recovered draft; reopen recovery instead of creating a second note");
            }
        }
        use std::io::Write;
        let mut temporary = tempfile::NamedTempFile::new_in(records)?;
        temporary.write_all(&serde_json::to_vec(&record)?)?;
        temporary.as_file().sync_all()?;
        temporary.persist(records.join(name))?;
        #[cfg(unix)]
        std::fs::File::open(records)?.sync_all()?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct ComposerRecord {
    root: PathBuf,
    draft: ComposerDraft,
}

/// Incomplete composer sessions, plus orphaned intents from the earlier capture
/// implementation. Never silently select one of several recovered drafts.
pub fn recover_composers(root: &Path, records: &Path) -> Result<Vec<ComposerDraft>> {
    let canonical = root.canonicalize()?;
    let mut captures = recover(root, records)?;
    let mut drafts = Vec::new();
    let entries = match std::fs::read_dir(records) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(drafts),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".composer.json") {
            continue;
        }
        let record: ComposerRecord =
            read_optional(&entry.path())?.context("Composer draft disappeared during recovery")?;
        anyhow::ensure!(
            record.root == canonical,
            "Composer recovery directory belongs to another vault"
        );
        let draft = record.draft;
        anyhow::ensure!(
            uuid::Uuid::parse_str(&draft.id)?.to_string() == draft.id
                && name == format!("{}.composer.json", draft.id),
            "Composer record identity mismatch"
        );
        if let Some(attempt) = &draft.attempt {
            attempt.validate()?;
            anyhow::ensure!(
                attempt.text() == draft.text && attempt.path().parent() == draft.folder.as_deref(),
                "Recovered composer and capture disagree"
            );
            captures.retain(|capture| capture.receipt.operation_id != attempt.receipt.operation_id);
            if let Some(receipt) = read_optional::<Receipt>(
                &records.join(format!("{}.receipt.json", attempt.receipt.operation_id)),
            )? {
                anyhow::ensure!(
                    receipt == attempt.receipt,
                    "Recovered completion receipt disagrees"
                );
                continue;
            }
        }
        if !draft.text.is_empty() || draft.attempt.is_some() {
            drafts.push(draft);
        }
    }
    drafts.extend(captures.into_iter().map(|capture| ComposerDraft {
        id: uuid::Uuid::new_v4().to_string(),
        text: capture.text.clone(),
        folder: capture.path().parent().map(Path::to_path_buf),
        attempt: Some(capture),
    }));
    drafts.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(drafts)
}

fn prepare_records(records: &Path) -> Result<()> {
    std::fs::create_dir_all(records)?;
    #[cfg(unix)]
    if let Some(parent) = records.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("Invalid capture recovery record: {}", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn persist_equal<T: Serialize + serde::de::DeserializeOwned + PartialEq>(
    directory: &Path,
    name: &str,
    value: &T,
) -> Result<()> {
    if let Some(existing) = read_optional::<T>(&directory.join(name))? {
        anyhow::ensure!(
            &existing == value,
            "Capture recovery record differs from this operation"
        );
        // Also retry directory sync after a previous post-publication sync error.
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        return Ok(());
    }
    persist_new(directory, name, value)
}

/// Descriptor-relative reads refuse symlinks and special files during recovery.
fn read_destination(root: &Path, relative: &Path) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    {
        use rustix::fs::{open, openat, Mode, OFlags};
        use std::io::Read;
        let mut folder = open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        for component in relative
            .parent()
            .context("Missing capture folder")?
            .components()
        {
            folder = openat(
                &folder,
                component.as_os_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
        }
        let file = match openat(
            &folder,
            relative.file_name().context("Missing capture filename")?,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => file,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut file = std::fs::File::from(file);
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "Capture destination is not a regular file"
        );
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        file.sync_all()?;
        std::fs::File::from(folder).sync_all()?;
        Ok(Some(bytes))
    }
    #[cfg(windows)]
    {
        let _root = crate::windows_files::Directory::open(root)?;
        let folder = crate::windows_files::Directory::open(
            &root.join(relative.parent().context("Missing capture folder")?),
        )?;
        match folder.read(relative.file_name().context("Missing capture filename")?) {
            Ok((file, bytes, _)) => {
                file.sync_all()?;
                Ok(Some(bytes))
            }
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

fn persist_new(directory: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec(value)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(directory.join(name))?;
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_text_receipt_and_no_overwrite() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir(vault.path().join("Inbox")).unwrap();
        let capture = Capture::new(Path::new("Inbox"), "\u{feff}Original\r\ntext".into()).unwrap();
        let receipt = capture
            .publish(
                vault.path(),
                &state.path().join("drafts"),
                &state.path().join("captures"),
            )
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(vault.path().join(&receipt.path)).unwrap(),
            capture.text()
        );
        let saved: Receipt = serde_json::from_slice(
            &std::fs::read(
                state
                    .path()
                    .join("captures")
                    .join(format!("{}.receipt.json", receipt.operation_id)),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(saved, receipt);
        std::fs::write(vault.path().join(&receipt.path), "external edit").unwrap();
        assert_eq!(
            capture
                .publish(
                    vault.path(),
                    &state.path().join("drafts"),
                    &state.path().join("captures")
                )
                .unwrap(),
            receipt
        );
        assert_eq!(
            std::fs::read_to_string(vault.path().join(receipt.path)).unwrap(),
            "external edit"
        );
        assert_eq!(capture.text(), "\u{feff}Original\r\ntext");
    }

    #[test]
    fn chosen_folder_only_and_collision_preserves_both_sources() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        assert!(Capture::new(Path::new("../outside"), "text".into()).is_err());
        assert!(Capture::new(Path::new(""), " \n".into()).is_err());
        let capture = Capture::new(Path::new(""), "authored".into()).unwrap();
        std::fs::write(vault.path().join(&capture.receipt.path), "existing").unwrap();
        assert!(capture
            .publish(
                vault.path(),
                &state.path().join("drafts"),
                &state.path().join("captures")
            )
            .is_err());
        assert_eq!(
            std::fs::read_to_string(vault.path().join(&capture.receipt.path)).unwrap(),
            "existing"
        );
        assert_eq!(capture.text(), "authored");
    }
    #[test]
    fn restart_reconciles_lost_receipt_and_does_not_resurrect_deleted_note() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let locks = state.path().join("drafts");
        let capture = Capture::new(Path::new(""), "exact\r\nsource".into()).unwrap();
        capture.checkpoint(vault.path(), &records).unwrap();
        let receipt = capture.publish(vault.path(), &locks, &records).unwrap();
        std::fs::remove_file(records.join(format!("{}.receipt.json", receipt.operation_id)))
            .unwrap();
        let recovered = recover(vault.path(), &records).unwrap();
        assert_eq!(recovered, vec![capture.clone()]);
        assert_eq!(
            recovered[0]
                .publish(vault.path(), &locks, &records)
                .unwrap(),
            receipt
        );
        assert!(recover(vault.path(), &records).unwrap().is_empty());
        std::fs::remove_file(vault.path().join(&receipt.path)).unwrap();
        assert_eq!(
            capture.publish(vault.path(), &locks, &records).unwrap(),
            receipt
        );
        assert!(!vault.path().join(&receipt.path).exists());
    }

    #[test]
    fn unfinished_intent_retries_absence_but_refuses_divergence_and_foreign_vault() {
        let vault = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let locks = state.path().join("drafts");
        let capture = Capture::new(Path::new(""), "authored".into()).unwrap();
        prepare_records(&records).unwrap();
        persist_new(
            &records,
            &format!("{}.intent.json", capture.receipt.operation_id),
            &Record {
                root: vault.path().canonicalize().unwrap(),
                capture: capture.clone(),
            },
        )
        .unwrap();
        std::fs::write(vault.path().join(capture.path()), "newer").unwrap();
        assert!(capture.publish(vault.path(), &locks, &records).is_err());
        assert!(capture.publish(other.path(), &locks, &records).is_err());
        assert!(recover(other.path(), &records).is_err());
        assert_eq!(
            std::fs::read_to_string(vault.path().join(capture.path())).unwrap(),
            "newer"
        );
        std::fs::remove_file(vault.path().join(capture.path())).unwrap();
        capture.publish(vault.path(), &locks, &records).unwrap();
        assert_eq!(
            std::fs::read_to_string(vault.path().join(capture.path())).unwrap(),
            "authored"
        );
    }

    #[test]
    fn composer_checkpoint_recovers_latest_text_and_same_attempt() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let mut draft = ComposerDraft {
            text: "first".into(),
            folder: Some(PathBuf::new()),
            ..Default::default()
        };
        draft.persist(vault.path(), &records).unwrap();
        draft.text = "latest\r\n".into();
        draft.persist(vault.path(), &records).unwrap();
        assert_eq!(
            recover_composers(vault.path(), &records).unwrap(),
            vec![draft.clone()]
        );
        draft.prepare_save().unwrap();
        draft.persist(vault.path(), &records).unwrap();
        let recovered = recover_composers(vault.path(), &records).unwrap();
        assert_eq!(recovered, vec![draft.clone()]);
        recovered[0]
            .attempt
            .as_ref()
            .unwrap()
            .publish(vault.path(), &state.path().join("drafts"), &records)
            .unwrap();
        assert!(recover_composers(vault.path(), &records)
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_refuses_symlink_destination_and_folder() {
        use std::os::unix::fs::symlink;
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let capture = Capture::new(Path::new(""), "authored".into()).unwrap();
        std::fs::write(outside.path().join("target"), "untouched").unwrap();
        symlink(
            outside.path().join("target"),
            vault.path().join(capture.path()),
        )
        .unwrap();
        assert!(capture
            .publish(
                vault.path(),
                &state.path().join("drafts"),
                &state.path().join("records")
            )
            .is_err());
        symlink(outside.path(), vault.path().join("Inbox")).unwrap();
        let capture = Capture::new(Path::new("Inbox"), "authored".into()).unwrap();
        assert!(capture
            .publish(
                vault.path(),
                &state.path().join("drafts"),
                &state.path().join("records")
            )
            .is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("target")).unwrap(),
            "untouched"
        );
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn failed_safe_writer_retains_intent_then_retry_publishes_once() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let locks = state.path().join("drafts");
        let capture = Capture::new(Path::new(""), "retained".into()).unwrap();
        let failed = crate::unix_publish::inject(rustix::io::Errno::ACCESS, || {
            capture.publish(vault.path(), &locks, &records)
        });
        assert!(failed.is_err());
        assert!(!vault.path().join(capture.path()).exists());
        assert_eq!(
            recover(vault.path(), &records).unwrap(),
            vec![capture.clone()]
        );
        let recovered = recover(vault.path(), &records).unwrap().remove(0);
        let receipt = recovered.publish(vault.path(), &locks, &records).unwrap();
        assert_eq!(
            recovered.publish(vault.path(), &locks, &records).unwrap(),
            receipt
        );
        assert_eq!(std::fs::read_dir(vault.path()).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(vault.path().join(capture.path())).unwrap(),
            "retained"
        );
    }

    #[test]
    fn identical_existing_note_is_not_adopted_and_corruption_is_visible() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let capture = Capture::new(Path::new(""), "same bytes".into()).unwrap();
        std::fs::write(vault.path().join(capture.path()), capture.text()).unwrap();
        assert!(capture
            .publish(vault.path(), &state.path().join("locks"), &records)
            .is_err());
        assert!(!records
            .join(format!("{}.receipt.json", capture.receipt.operation_id))
            .exists());
        std::fs::write(records.join("broken.intent.json"), b"{not-json").unwrap();
        assert!(recover(vault.path(), &records).is_err());
    }

    #[test]
    fn recovered_draft_cannot_allocate_a_second_save_identity() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let mut draft = ComposerDraft {
            text: "one thought".into(),
            folder: Some(PathBuf::new()),
            ..Default::default()
        };
        draft.persist(vault.path(), &records).unwrap();
        let mut competing = draft.clone();
        draft.prepare_save().unwrap();
        draft.persist(vault.path(), &records).unwrap();
        competing.prepare_save().unwrap();
        assert_ne!(competing.attempt, draft.attempt);
        assert!(competing.persist(vault.path(), &records).is_err());
        let recovered = recover_composers(vault.path(), &records).unwrap();
        assert_eq!(recovered, vec![draft]);
    }
}
