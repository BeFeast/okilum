//! Explicit byte transactions share FileEditor's native publication path.
//! Ordinary editor opens and drafts remain strict, lossless UTF-8.
use super::*;
use std::{collections::BTreeMap, ffi::OsString};
#[cfg(all(test, unix))]
thread_local! { static FAIL_STAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(unix)]
type Identity = (u64, u64);
#[cfg(windows)]
type Identity = crate::windows_files::Identity;

pub(super) struct ByteTransaction {
    before: Vec<u8>,
    after: Vec<u8>,
    before_identity: Identity,
    artifacts: Mutex<BTreeMap<OsString, Vec<Identity>>>,
    history: Mutex<Option<PathBuf>>,
}

impl FileEditor {
    pub fn repair_encoding(path: &Path, state: &Path) -> Result<Save> {
        let original = fs::read(path)?;
        if std::str::from_utf8(&original).is_ok() {
            return Ok(Save::Conflict);
        }
        let repaired = String::from_utf8_lossy(&original).into_owned();
        Self::replace_bytes(path, state, &original, repaired.as_bytes())
    }

    /// Explicit Repair/History Restore only. The caller supplies the exact
    /// reviewed byte revision; no lossy text can authorize an ordinary save.
    pub fn replace_bytes(
        path: &Path,
        state: &Path,
        reviewed: &[u8],
        proposed: &[u8],
    ) -> Result<Save> {
        Self::replace_bytes_before(path, state, reviewed, proposed, || {})
    }

    fn replace_bytes_before(
        path: &Path,
        state: &Path,
        reviewed: &[u8],
        proposed: &[u8],
        before_exchange: impl FnOnce(),
    ) -> Result<Save> {
        #[cfg(unix)]
        let original = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        anyhow::ensure!(
            original.is_file() && original.nlink() == 1,
            "Repair requires a regular file without symlinks or hard links"
        );
        #[cfg(windows)]
        let original_directory = Directory::open(path.parent().context("Missing source folder")?)?;
        #[cfg(windows)]
        let (_, original) =
            original_directory.read_bytes(path.file_name().context("Missing source name")?)?;
        let path = path.canonicalize()?;
        let directory = Directory::open(path.parent().context("Missing source folder")?)?;
        let lock = Self::reserve_destination(&path, state)?;
        let (current, metadata) = directory.read_bytes(path.file_name().unwrap())?;
        #[cfg(unix)]
        let identity = (metadata.dev(), metadata.ino());
        #[cfg(unix)]
        anyhow::ensure!(
            identity == (original.dev(), original.ino()),
            "The note changed while opening"
        );
        #[cfg(windows)]
        let identity = metadata;
        #[cfg(windows)]
        anyhow::ensure!(identity == original, "The note changed while opening");
        directory.validate(path.parent().unwrap())?;
        if current != reviewed {
            return Ok(Save::Conflict);
        }
        if current == proposed {
            return Ok(Save::Saved);
        }
        let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let transaction = Arc::new(ByteTransaction {
            before: current.clone(),
            after: proposed.to_vec(),
            before_identity: identity,
            artifacts: Mutex::default(),
            history: Mutex::default(),
        });
        let mut editor = Self {
            authority: okilum_write_authority::Authority::for_path(&path)?,
            draft: Draft {
                path: path.clone(),
                base: String::from_utf8_lossy(&current).into_owned().into(),
                text: String::from_utf8_lossy(proposed).into_owned().into(),
                pending_input: false,
            },
            directory,
            on_disk: None,
            journal: state.join(format!("{key}.json")),
            lock: Some(Arc::new(lock)),
            lock_path: state.join(format!("{key}.lock")),
            writes: Arc::default(),
            generation: 0,
            base_generation: 0,
            raw_transaction: Some(transaction),
        };
        editor.persist()?;
        let lease = match editor.authority.admit() {
            Ok(lease) => lease,
            Err(error) => {
                use base64::{engine::general_purpose::STANDARD, Engine};
                let record = state
                    .join("deferred-byte-publications")
                    .join(format!("{}.json", uuid::Uuid::new_v4()));
                crate::source_state::persist(
                    &record,
                    &serde_json::to_vec(&serde_json::json!({
                        "path": path, "reviewed_base64": STANDARD.encode(reviewed), "proposed_base64": STANDARD.encode(proposed)
                    }))?,
                )?;
                return Err(error.context(format!(
                    "Exact byte proposal retained at {}",
                    record.display()
                )));
            }
        };
        let _admission = lease.enter()?;
        let result = editor.save_before_exchange(before_exchange);
        let retain_recovery = result.as_ref().err().is_some_and(|error| {
            error.is::<UnconfirmedSave>() || error.is::<PublishedSaveNeedsAttention>()
        });
        if !matches!(result, Ok(Save::Saved)) && !retain_recovery {
            editor
                .rollback_raw_transaction()
                .context("Encoding change failed; recovery must be inspected")?;
        }
        result
    }

    pub(super) fn base_bytes(&self) -> &[u8] {
        self.raw_transaction
            .as_ref()
            .map_or_else(|| self.draft.base.as_bytes(), |raw| raw.before.as_slice())
    }
    pub(super) fn proposed_bytes(&self) -> &[u8] {
        self.raw_transaction
            .as_ref()
            .map_or_else(|| self.draft.text.as_bytes(), |raw| raw.after.as_slice())
    }
    pub(super) fn begin_save_history(
        &self,
        displaced: &Path,
        prepared: Option<&Path>,
    ) -> Result<PathBuf> {
        if let Some(raw) = &self.raw_transaction {
            let path = crate::source_history::RawRecord::begin(
                self.journal.parent().unwrap(),
                &self.draft.path,
                self.base_bytes(),
                self.proposed_bytes(),
                displaced,
                prepared,
            )?;
            *raw.history.lock().unwrap() = Some(path.clone());
            return Ok(path);
        }
        #[cfg(unix)]
        return crate::source_history::Preimage::begin(
            self.journal.parent().unwrap(),
            &self.draft.path,
            &self.draft.base,
            displaced,
        );
        #[cfg(windows)]
        crate::source_history::Preimage::begin_windows(
            self.journal.parent().unwrap(),
            &self.draft.path,
            &self.draft.base,
            displaced,
            prepared.context("Missing prepared source")?,
        )
    }

    #[cfg(all(test, unix))]
    pub(super) fn inject_partial_raw_write(&self, file: &mut File) -> Result<()> {
        if self.raw_transaction.is_some() && FAIL_STAGE.with(|flag| flag.replace(false)) {
            file.write_all(&self.proposed_bytes()[..self.proposed_bytes().len().min(3)])?;
            return Err(std::io::Error::other("injected partial staging write failure").into());
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn track_raw_temporary(&self, name: &str, file: &File) -> Result<()> {
        if let Some(raw) = &self.raw_transaction {
            let metadata = file.metadata()?;
            raw.artifacts.lock().unwrap().insert(
                name.into(),
                vec![raw.before_identity, (metadata.dev(), metadata.ino())],
            );
        }
        Ok(())
    }
    #[cfg(unix)]
    pub(super) fn track_raw_renamed(&self, original: &std::ffi::OsStr, name: &str) -> Result<()> {
        if let Some(raw) = &self.raw_transaction {
            let mut artifacts = raw.artifacts.lock().unwrap();
            let identities = artifacts
                .get(original)
                .context("Missing owned staging identity")?
                .clone();
            artifacts.insert(name.into(), identities);
        }
        Ok(())
    }
    #[cfg(windows)]
    pub(super) fn track_raw_windows_identity(&self, path: &Path, identity: Identity) -> Result<()> {
        if let Some(raw) = &self.raw_transaction {
            anyhow::ensure!(
                path.parent() == self.draft.path.parent(),
                "Staging parent changed"
            );
            let name = path.file_name().context("Missing staging name")?;
            let mut artifacts = raw.artifacts.lock().unwrap();
            let identities = artifacts.entry(name.to_owned()).or_default();
            if !identities.contains(&identity) {
                identities.push(identity);
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    pub(super) fn track_raw_windows_preimage(&self, path: &Path, identity: Identity) -> Result<()> {
        self.track_raw_windows_identity(path, identity)
    }

    #[cfg(unix)]
    fn clean_raw_artifact(&self, name: &std::ffi::OsStr) -> Result<()> {
        use rustix::fs::{openat, unlinkat, AtFlags, Mode, OFlags};
        let raw = self
            .raw_transaction
            .as_ref()
            .context("Missing byte transaction")?;
        let identities = raw
            .artifacts
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .context("Unowned staging name")?;
        let fd = match openat(
            &self.directory.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut file = File::from(fd);
        let metadata = file.metadata()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        anyhow::ensure!(
            metadata.is_file()
                && identities.contains(&(metadata.dev(), metadata.ino()))
                && (bytes == raw.before
                    || bytes == raw.after
                    || ((metadata.dev(), metadata.ino()) != raw.before_identity
                        && (raw.before.starts_with(&bytes) || raw.after.starts_with(&bytes)))),
            "Staging file changed; recovery retained"
        );
        let current = rustix::fs::statat(&self.directory.file, name, AtFlags::SYMLINK_NOFOLLOW)?;
        anyhow::ensure!(
            crate::unix_publish::same_identity(&current, &metadata),
            "Staging file replaced; recovery retained"
        );
        unlinkat(&self.directory.file, name, AtFlags::empty())?;
        Ok(())
    }
    #[cfg(windows)]
    fn clean_raw_artifact(&self, name: &std::ffi::OsStr) -> Result<()> {
        let raw = self
            .raw_transaction
            .as_ref()
            .context("Missing byte transaction")?;
        let identities = raw
            .artifacts
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .context("Unowned staging name")?;
        let native = self.directory.pin()?;
        match native.read(name) {
            Ok((_, bytes, info)) => {
                let identity = crate::windows_files::identity(&info);
                anyhow::ensure!(
                    identities.contains(&identity)
                        && (bytes == raw.before
                            || bytes == raw.after
                            || (identity != raw.before_identity
                                && (raw.before.starts_with(&bytes)
                                    || raw.after.starts_with(&bytes)))),
                    "Staging file changed; recovery retained"
                );
                crate::windows_files::undo_created(
                    &self.draft.path.parent().unwrap().join(name),
                    identity,
                    Some(&bytes),
                )?;
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn finish_raw_history_unix(&self, history: &Path) -> Result<()> {
        let record = crate::source_history::RawRecord::load(history)?;
        let name = record
            .displaced
            .file_name()
            .context("Missing preimage name")?;
        let (bytes, _) = self.directory.read_bytes(name)?;
        anyhow::ensure!(
            bytes == self.base_bytes(),
            "Displaced bytes changed; recovery retained"
        );
        crate::source_history::RawRecord::complete(history)?;
        // Last fallible step: all commit checks and durable recovery precede it.
        self.clean_raw_artifact(name)
    }
    #[cfg(windows)]
    pub(super) fn finish_raw_history_windows(
        &self,
        history: &Path,
        native: &crate::windows_files::Directory,
        expected_identity: Identity,
    ) -> Result<()> {
        let record = crate::source_history::RawRecord::load(history)?;
        let name = record
            .displaced
            .file_name()
            .context("Missing preimage name")?;
        let (_, bytes, info) = native.read(name)?;
        let identity = crate::windows_files::identity(&info);
        anyhow::ensure!(
            bytes == self.base_bytes() && identity == expected_identity,
            "Displaced bytes changed; recovery retained"
        );
        crate::source_history::RawRecord::complete(history)?;
        self.clean_raw_artifact(name)
    }

    fn rollback_raw_transaction(&self) -> Result<()> {
        let raw = self
            .raw_transaction
            .as_ref()
            .context("Missing byte transaction")?;
        if raw.artifacts.lock().unwrap().is_empty() && raw.history.lock().unwrap().is_none() {
            return Ok(());
        }
        let name = self.draft.path.file_name().unwrap();
        let (current, _) = self.directory.read_bytes(name)?;
        if current == raw.after {
            self.rollback_raw_publication()?;
        }
        // A racing writer owns its new revision. The ordinary publication path
        // already reverses a raced exchange; never overwrite that writer here.
        let (restored, _) = self.directory.read_bytes(name)?;
        anyhow::ensure!(
            restored != raw.after || raw.before == raw.after,
            "Published replacement could not be reversed; all recovery retained"
        );
        let names: Vec<_> = raw.artifacts.lock().unwrap().keys().cloned().collect();
        for artifact in names {
            self.clean_raw_artifact(&artifact)?;
        }
        if restored == raw.before {
            if let Some(history) = raw.history.lock().unwrap().as_ref() {
                crate::source_history::RawRecord::discard(
                    history,
                    &self.draft.path,
                    &raw.before,
                    &raw.after,
                )?;
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn rollback_raw_publication(&self) -> Result<()> {
        let raw = self.raw_transaction.as_ref().unwrap();
        let names: Vec<_> = raw.artifacts.lock().unwrap().keys().cloned().collect();
        let mut original = None;
        for name in &names {
            if self
                .directory
                .read_bytes(name)
                .is_ok_and(|(bytes, metadata)| {
                    bytes == raw.before && (metadata.dev(), metadata.ino()) == raw.before_identity
                })
            {
                original = Some(name.clone());
                break;
            }
        }
        let original = original.context("Original preimage must remain available for rollback")?;
        let note = self.draft.path.file_name().unwrap();
        match renameat_with(
            &self.directory.file,
            &original,
            &self.directory.file,
            note,
            RenameFlags::EXCHANGE,
        ) {
            Ok(()) => {}
            Err(error) if crate::unix_publish::unsupported(error) => {
                anyhow::bail!("This drive cannot safely exchange a rollback; draft and preimage retained for explicit recovery");
            }
            Err(error) => return Err(error.into()),
        }
        self.directory.file.sync_all()?;
        Ok(())
    }
    #[cfg(windows)]
    fn rollback_raw_publication(&self) -> Result<()> {
        let raw = self.raw_transaction.as_ref().unwrap();
        let native = self.directory.pin()?;
        let plan = native
            .prepare_replace_tracked(
                self.draft.path.file_name().unwrap(),
                &raw.after,
                &raw.before,
                |path, identity| self.track_raw_windows_identity(path, identity),
            )?
            .context("The note changed before rollback; recovery retained")?;
        self.track_raw_windows_preimage(plan.preimage_path(), plan.preimage_identity())?;
        anyhow::ensure!(
            matches!(
                plan.commit()?,
                crate::windows_files::Replacement::Saved { .. }
            ),
            "Rollback conflicted; recovery retained"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_vault_tails(root: &Path) {
        assert!(fs::read_dir(root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".okilum-save-")));
    }

    #[test]
    fn explicit_repair_and_history_restore_keep_exact_original_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let state = fixture.path().join("state/editor-drafts");
        fs::create_dir(&root).unwrap();
        let note = root.join("Synthetic.md");
        let original = b"# Synthetic\r\n\r\nBefore \xff After\r\n";
        fs::write(&note, original).unwrap();
        assert!(FileEditor::open(&note, &state).is_err());
        assert_eq!(fs::read(&note).unwrap(), original);
        assert!(crate::source_history::list(&state, &root)
            .unwrap()
            .versions
            .is_empty());
        assert_eq!(
            FileEditor::repair_encoding(&note, &state).unwrap(),
            Save::Saved
        );
        let repaired = fs::read(&note).unwrap();
        assert_eq!(
            std::str::from_utf8(&repaired).unwrap(),
            String::from_utf8_lossy(original)
        );
        drop(FileEditor::open(&note, &state).unwrap());
        let listing = crate::source_history::list(&state, &root).unwrap();
        assert!(listing.warnings.is_empty());
        let retained = listing
            .versions
            .iter()
            .find(|version| version.raw.is_some())
            .unwrap();
        assert_eq!(retained.bytes(), original);
        assert_eq!(
            FileEditor::replace_bytes(&note, &state, &repaired, retained.bytes()).unwrap(),
            Save::Saved
        );
        assert_eq!(fs::read(&note).unwrap(), original);
        assert!(FileEditor::open(&note, &state).is_err());
        no_vault_tails(&root);
    }

    #[test]
    fn history_error_after_publication_rolls_back_and_removes_owned_staging() {
        for (original, proposed) in [
            (
                b"Before \xff After\n".as_slice(),
                b"Before \xef\xbf\xbd After\n".as_slice(),
            ),
            (
                b"Before \xef\xbf\xbd After\n".as_slice(),
                b"Before \xff After\n".as_slice(),
            ),
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let state = fixture.path().join("state/editor-drafts");
            let root = fixture.path().join("vault");
            fs::create_dir(&root).unwrap();
            let note = root.join("Synthetic.md");
            fs::write(&note, original).unwrap();
            crate::source_history::RawRecord::fail_next_complete();
            let error = FileEditor::replace_bytes(&note, &state, original, proposed).unwrap_err();
            assert!(format!("{error:#}").contains("History acknowledgement"));
            assert_eq!(fs::read(&note).unwrap(), original);
            #[cfg(unix)]
            assert_eq!(fs::metadata(&note).unwrap().nlink(), 1);
            no_vault_tails(&root);
            assert!(crate::source_history::list(&state, &root)
                .unwrap()
                .versions
                .is_empty());
        }
    }

    #[test]
    fn failed_recovery_preparation_and_revision_conflict_do_not_write_the_note() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let state = fixture.path().join("state/editor-drafts");
        fs::create_dir(&root).unwrap();
        fs::create_dir_all(&state).unwrap();
        let note = root.join("Synthetic.md");
        let original = b"Before \xff After\n";
        fs::write(&note, original).unwrap();
        fs::write(
            state.join("source-history"),
            b"blocks History directory creation",
        )
        .unwrap();
        assert!(FileEditor::repair_encoding(&note, &state).is_err());
        assert_eq!(fs::read(&note).unwrap(), original);
        no_vault_tails(&root);
        assert_eq!(
            FileEditor::replace_bytes(&note, &state, b"stale reviewed bytes", b"replacement")
                .unwrap(),
            Save::Conflict
        );
        assert_eq!(fs::read(&note).unwrap(), original);
        no_vault_tails(&root);
        let lock = FileEditor::reserve_destination(&note, &state).unwrap();
        assert!(FileEditor::repair_encoding(&note, &state).is_err());
        drop(lock);
        assert_eq!(fs::read(&note).unwrap(), original);
        no_vault_tails(&root);
    }
}

#[cfg(all(test, unix))]
mod partial_write_tests {
    use super::*;
    fn assert_clean(root: &Path, note: &Path, expected: &[u8]) {
        assert_eq!(fs::read(note).unwrap(), expected);
        assert_eq!(fs::metadata(note).unwrap().nlink(), 1);
        assert_eq!(fs::read_dir(root).unwrap().count(), 1);
    }

    #[test]
    fn unsupported_exchange_repair_and_raw_restore_round_trip() {
        for error in [
            rustix::io::Errno::NOTSUP,
            rustix::io::Errno::INVAL,
            rustix::io::Errno::NOSYS,
        ] {
            crate::unix_publish::inject(error, || {
                let fixture = tempfile::tempdir().unwrap();
                let root = fixture.path().join("vault");
                let state = fixture.path().join("state/editor-drafts");
                fs::create_dir(&root).unwrap();
                let note = root.join("Synthetic.md");
                let original = b"Before \xff After\r\n";
                let repaired = String::from_utf8_lossy(original);
                fs::write(&note, original).unwrap();
                assert_eq!(
                    FileEditor::repair_encoding(&note, &state).unwrap(),
                    Save::Saved
                );
                assert_clean(&root, &note, repaired.as_bytes());
                drop(FileEditor::open(&note, &state).unwrap());
                let listing = crate::source_history::list(&state, &root).unwrap();
                assert!(listing.warnings.is_empty());
                let retained = listing.versions.iter().find(|v| v.raw.is_some()).unwrap();
                assert_eq!(retained.bytes(), original);
                assert_eq!(
                    FileEditor::replace_bytes(&note, &state, repaired.as_bytes(), retained.bytes())
                        .unwrap(),
                    Save::Saved
                );
                assert_clean(&root, &note, original);
                assert!(FileEditor::open(&note, &state).is_err());
            });
        }
    }

    #[test]
    fn unsupported_exchange_raw_failures_clean_before_publish_and_retain_after_publish() {
        for restoring in [false, true] {
            for failure in 0..3 {
                crate::unix_publish::inject(rustix::io::Errno::NOTSUP, || {
                    let fixture = tempfile::tempdir().unwrap();
                    let root = fixture.path().join("vault");
                    let state = fixture.path().join("state/editor-drafts");
                    fs::create_dir(&root).unwrap();
                    let note = root.join("Synthetic.md");
                    let damaged = b"Before \xff After\r\n";
                    let repaired = String::from_utf8_lossy(damaged);
                    let (before, after) = if restoring {
                        (repaired.as_bytes(), damaged.as_slice())
                    } else {
                        (damaged.as_slice(), repaired.as_bytes())
                    };
                    fs::write(&note, before).unwrap();
                    match failure {
                        0 => network_save_tests::LINK_ERROR
                            .with(|v| v.set(Some(rustix::io::Errno::PERM))),
                        1 => network_save_tests::ERROR_AFTER_LINK
                            .with(|v| v.set(Some(rustix::io::Errno::IO))),
                        _ => crate::source_history::RawRecord::fail_next_complete(),
                    }
                    let error =
                        FileEditor::replace_bytes(&note, &state, before, after).unwrap_err();
                    match failure {
                        0 => assert_eq!(
                            error.downcast_ref::<rustix::io::Errno>(),
                            Some(&rustix::io::Errno::PERM)
                        ),
                        1 => assert_eq!(
                            error.downcast_ref::<rustix::io::Errno>(),
                            Some(&rustix::io::Errno::IO)
                        ),
                        _ => assert!(format!("{error:#}").contains("History acknowledgement")),
                    }
                    if failure == 2 {
                        assert!(error.is::<UnconfirmedSave>());
                        assert_eq!(fs::read(&note).unwrap(), after);
                        let listing = crate::source_history::list(&state, &root).unwrap();
                        assert!(listing.versions.iter().any(|v| v.bytes() == before));
                        assert!(fs::read_dir(&root).unwrap().any(|e| e
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".okilum-save-")));
                        return;
                    }
                    assert_clean(&root, &note, before);
                    assert!(crate::source_history::list(&state, &root)
                        .unwrap()
                        .versions
                        .is_empty());
                    assert_eq!(
                        FileEditor::replace_bytes(&note, &state, before, after).unwrap(),
                        Save::Saved
                    );
                    assert_clean(&root, &note, after);
                });
            }
        }
    }

    #[test]
    fn raw_unconfirmed_publication_never_blindly_rolls_back() {
        use network_save_tests::{fail_publication, PublicationFailure};
        for failure in [
            PublicationFailure::LostReply,
            PublicationFailure::OutcomeProbe,
            PublicationFailure::DirectorySync,
        ] {
            crate::unix_publish::inject(rustix::io::Errno::NOTSUP, || {
                let fixture = tempfile::tempdir().unwrap();
                let root = fixture.path().join("vault");
                let state = fixture.path().join("state");
                fs::create_dir(&root).unwrap();
                let note = root.join("Synthetic.md");
                let before = b"\xef\xbb\xbfBefore \xff\r\n";
                let after = b"\xef\xbb\xbfRepaired\r\n";
                fs::write(&note, before).unwrap();
                let error = fail_publication(failure, || {
                    FileEditor::replace_bytes(&note, &state, before, after)
                })
                .unwrap_err();
                assert!(error.is::<UnconfirmedSave>());
                assert_eq!(
                    fs::read(&note).unwrap(),
                    if failure == PublicationFailure::OutcomeProbe {
                        before.as_slice()
                    } else {
                        after.as_slice()
                    }
                );
                let listing = crate::source_history::list(&state, &root).unwrap();
                assert!(listing.versions.iter().any(|v| v.bytes() == before));
                let history = fs::read_dir(state.join("source-history"))
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| crate::source_history::RawRecord::is_path(p))
                    .unwrap();
                let record = crate::source_history::RawRecord::load(&history).unwrap();
                assert_eq!(record.before, before);
                assert_eq!(record.after, after);
                assert_eq!(fs::read(&record.displaced).unwrap(), before);
                assert!(!record.completed);
            });
        }
    }

    #[test]
    fn partial_repair_write_does_not_leave_a_tail_or_change_the_note() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        fs::create_dir(&root).unwrap();
        let note = root.join("Synthetic.md");
        let original = b"Before \xff After\n";
        fs::write(&note, original).unwrap();
        FAIL_STAGE.with(|flag| flag.set(true));
        let error = FileEditor::repair_encoding(&note, &fixture.path().join("state/editor-drafts"))
            .unwrap_err();
        assert!(format!("{error:#}").contains("partial staging write"));
        assert_eq!(fs::read(&note).unwrap(), original);
        assert_eq!(fs::metadata(&note).unwrap().nlink(), 1);
        assert_eq!(fs::read_dir(root).unwrap().count(), 1);
    }
}

// The intent names existing History and its two private namespace entries; it
// is operational state, never placed in the rebuildable index. Original bytes
// stay in the existing History record. No mount or filesystem heuristic is used.
#[cfg(unix)]
#[derive(Clone, Serialize, Deserialize)]
struct LinkRecovery {
    schema: u32,
    note: PathBuf,
    history: PathBuf,
    displaced: OsString,
    prepared: OsString,
    original_identity: Identity,
    staged_identity: Identity,
    ancestors: Vec<Identity>,
    before_hash: Vec<u8>,
    after_hash: Vec<u8>,
    #[serde(default)]
    retained_current: Option<Vec<u8>>,
    #[serde(default)]
    resolved: bool,
}

#[cfg(unix)]
impl LinkRecovery {
    fn persist(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        anyhow::ensure!(
            bytes.len() <= 128 * 1024 * 1024,
            "Recovery snapshot exceeds storage limit; all entries retained"
        );
        crate::source_state::persist(path, &bytes)
    }
}

#[cfg(unix)]
#[derive(Clone, PartialEq, Eq)]
struct RecoveryRevision {
    identity: Identity,
    bytes: Vec<u8>,
}

#[cfg(unix)]
pub(super) struct RecoveryInspection {
    directory: Directory,
    intent: LinkRecovery,
    intent_path: PathBuf,
    intent_bytes: Vec<u8>,
    history_bytes: Vec<u8>,
    original: Vec<u8>,
    current: Option<RecoveryRevision>,
    artifacts: Vec<(OsString, Option<RecoveryRevision>)>,
    _lock: Arc<EditorLock>,
}

#[cfg(unix)]
impl Directory {
    // Interrupted link publication deliberately has two names for one inode.
    // Ordinary editor reads keep their stricter nlink==1 rule.
    fn recovery_revision(&self, name: &std::ffi::OsStr) -> Result<Option<RecoveryRevision>> {
        use rustix::fs::{openat, AtFlags, Mode, OFlags};
        let fd = match openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut file = File::from(fd);
        let meta = file.metadata()?;
        anyhow::ensure!(
            meta.is_file() && meta.len() <= 128 * 1024 * 1024,
            "Recovery entry is not a bounded regular file; retained"
        );
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= 128 * 1024 * 1024,
            "Recovery entry grew; retained"
        );
        let now = rustix::fs::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW)?;
        anyhow::ensure!(
            crate::unix_publish::same_identity(&now, &meta),
            "Recovery entry replaced during inspection; retained"
        );
        Ok(Some(RecoveryRevision {
            identity: (meta.dev(), meta.ino()),
            bytes,
        }))
    }
}

#[cfg(unix)]
impl FileEditor {
    pub(super) fn record_link_recovery(
        &self,
        history: &Path,
        displaced: &std::ffi::OsStr,
        prepared: &str,
    ) -> Result<()> {
        let original = self
            .directory
            .recovery_revision(self.draft.path.file_name().unwrap())?
            .context("Source disappeared before recovery preparation")?;
        let staged = self
            .directory
            .recovery_revision(displaced)?
            .context("Missing staged save")?;
        anyhow::ensure!(
            original.bytes == self.base_bytes() && staged.bytes == self.proposed_bytes(),
            "Source or staged revision changed; recovery retained"
        );
        let intent = LinkRecovery {
            schema: 1,
            note: self.draft.path.clone(),
            history: history.to_owned(),
            displaced: displaced.to_owned(),
            prepared: prepared.into(),
            original_identity: original.identity,
            staged_identity: staged.identity,
            ancestors: self.directory.ancestor_identities().to_vec(),
            before_hash: Sha256::digest(&original.bytes).to_vec(),
            after_hash: Sha256::digest(&staged.bytes).to_vec(),
            retained_current: None,
            resolved: false,
        };
        intent.persist(&history.with_extension("link-recovery"))
    }

    pub(super) fn refuse_unresolved_link_recovery(&self) -> Result<()> {
        let folder = self.journal.parent().unwrap().join("source-history");
        if !folder.try_exists()? {
            return Ok(());
        }
        for entry in fs::read_dir(folder)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "link-recovery") {
                continue;
            }
            let intent: LinkRecovery = serde_json::from_slice(&read_bounded_recovery(&path)?)?;
            if intent.note == self.draft.path && !intent.resolved {
                return Err(UnconfirmedSave::UnknownPublication.into());
            }
        }
        Ok(())
    }

    pub(super) fn forget_link_recovery(&self, history: &Path) {
        // Completed saves have already archived the original and acknowledged
        // the draft. A failed removal merely leaves explicit recovery to inspect.
        let _ = fs::remove_file(history.with_extension("link-recovery"));
    }

    /// Explicit recovery discovery for an already-open editor. It shares its
    /// lock with pending worker copies; each inspection is revision-bound.
    pub fn inspect_uncertain_saves(&self) -> Result<Vec<InterruptedSave>> {
        let lock = match &self.lock {
            Some(lock) => lock.clone(),
            None => Arc::new(EditorLock::acquire_or(
                &self.lock_path,
                "This note has an active editor; inspect recovery in that editor",
            )?),
        };
        Self::inspect_link_recovery(&self.draft.path, self.journal.parent().unwrap(), lock)
    }

    /// Restart path that also works when an interrupted hard link prevents a
    /// normal editor open. Does not adopt, discard or change a dirty draft.
    pub fn inspect_interrupted_saves(note: &Path, state: &Path) -> Result<Vec<InterruptedSave>> {
        let parent = note
            .parent()
            .context("Missing source folder")?
            .canonicalize()?;
        let note = parent.join(note.file_name().context("Missing source name")?);
        let key = format!("{:x}", Sha256::digest(note.as_os_str().as_encoded_bytes()));
        let lock = Arc::new(EditorLock::acquire_or(
            &state.join(format!("{key}.lock")),
            "This note has an active editor; inspect recovery in that editor",
        )?);
        Self::inspect_link_recovery(&note, state, lock)
    }

    fn inspect_link_recovery(
        note: &Path,
        state: &Path,
        lock: Arc<EditorLock>,
    ) -> Result<Vec<InterruptedSave>> {
        let folder = state.join("source-history");
        if !folder.try_exists()? {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for entry in fs::read_dir(&folder)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "link-recovery") {
                continue;
            }
            let intent_bytes = read_bounded_recovery(&path)?;
            let intent: LinkRecovery = serde_json::from_slice(&intent_bytes)?;
            if intent.note != note || intent.resolved {
                continue;
            }
            anyhow::ensure!(
                intent.schema == 1
                    && intent.history.parent() == Some(folder.as_path())
                    && intent.history.with_extension("link-recovery") == path,
                "Recovery identity changed; retained"
            );
            anyhow::ensure!(
                intent.displaced != intent.prepared,
                "Recovery names alias; retained"
            );
            for name in [&intent.displaced, &intent.prepared] {
                anyhow::ensure!(
                    Some(name.as_os_str()) != note.file_name(),
                    "Recovery entry aliases source; retained"
                );
                anyhow::ensure!(
                    Path::new(name).components().count() == 1
                        && name.to_string_lossy().starts_with(".okilum-save-"),
                    "Invalid recovery name; retained"
                );
            }
            let history_bytes = read_bounded_recovery(&intent.history)?;
            let original = if crate::source_history::RawRecord::is_path(&intent.history) {
                let record = crate::source_history::RawRecord::load(&intent.history)?;
                anyhow::ensure!(
                    record.note == note && record.displaced.file_name() == Some(&intent.displaced),
                    "Raw History identity changed; retained"
                );
                record.before
            } else {
                let record: crate::source_history::Preimage =
                    serde_json::from_slice(&history_bytes)?;
                anyhow::ensure!(
                    record.note == note && record.displaced.file_name() == Some(&intent.displaced),
                    "History identity changed; retained"
                );
                record.text.into_bytes()
            };
            anyhow::ensure!(
                Sha256::digest(&original).as_slice() == intent.before_hash,
                "Original snapshot changed; retained"
            );
            let directory = Directory::open(note.parent().unwrap())?;
            anyhow::ensure!(
                directory.ancestor_identities() == intent.ancestors,
                "Source folder identity changed; recovery retained"
            );
            let current = directory.recovery_revision(note.file_name().unwrap())?;
            let mut artifacts = Vec::new();
            for name in [&intent.displaced, &intent.prepared] {
                artifacts.push((name.clone(), directory.recovery_revision(name)?));
            }
            result.push(InterruptedSave {
                inner: RecoveryInspection {
                    directory,
                    intent,
                    intent_path: path,
                    intent_bytes,
                    history_bytes,
                    original,
                    current,
                    artifacts,
                    _lock: lock.clone(),
                },
            });
        }
        Ok(result)
    }
}

#[cfg(unix)]
fn read_bounded_recovery(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)?;
    anyhow::ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 128 * 1024 * 1024,
        "Invalid recovery record; retained"
    );
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 128 * 1024 * 1024,
        "Recovery record grew; retained"
    );
    Ok(bytes)
}

#[cfg(unix)]
impl InterruptedSave {
    pub fn state(&self) -> InterruptedSaveState {
        let Some(current) = &self.inner.current else {
            return InterruptedSaveState::SourceMissing;
        };
        let hash = Sha256::digest(&current.bytes);
        if hash.as_slice() == self.inner.intent.after_hash {
            InterruptedSaveState::ProposedPresent
        } else if hash.as_slice() == self.inner.intent.before_hash {
            InterruptedSaveState::OriginalPresent
        } else {
            InterruptedSaveState::ChangedExternally
        }
    }
    pub fn current_bytes(&self) -> Option<&[u8]> {
        self.inner.current.as_ref().map(|v| v.bytes.as_slice())
    }

    /// Explicitly restore the captured source after an interrupted namespace
    /// gap. The inspection pins exact bytes/identity. linkat is create-only:
    /// a writer recreating the note wins, and both recovery versions remain.
    /// The proposal is never adopted. Reinspect before cleaning recovery.
    pub fn restore_missing_source(&self) -> Result<()> {
        let r = &self.inner;
        let _admission = okilum_write_authority::operation_for_path(&r.intent.note)?;
        anyhow::ensure!(r.current.is_none(), "Source was present at inspection");
        r.directory.validate(r.intent.note.parent().unwrap())?;
        anyhow::ensure!(
            read_bounded_recovery(&r.intent_path)? == r.intent_bytes
                && read_bounded_recovery(&r.intent.history)? == r.history_bytes,
            "Recovery changed since inspection; retained"
        );
        let captured = r
            .artifacts
            .iter()
            .find(|(name, _)| name == &r.intent.displaced)
            .and_then(|(_, revision)| revision.as_ref())
            .context("Captured source is missing; recovery retained")?;
        anyhow::ensure!(
            captured.identity != r.intent.staged_identity
                && r.directory.recovery_revision(&r.intent.displaced)?.as_ref() == Some(captured),
            "Captured source changed since inspection; retained"
        );
        rustix::fs::linkat(
            &r.directory.file,
            &r.intent.displaced,
            &r.directory.file,
            r.intent.note.file_name().unwrap(),
            rustix::fs::AtFlags::empty(),
        )?;
        r.directory.file.sync_all()?;
        Ok(())
    }
    pub fn original_bytes(&self) -> &[u8] {
        &self.inner.original
    }

    /// Explicit Keep current + clean owned links. Never publishes proposed bytes
    /// and never acknowledges/discards a draft. A changed revision requires a
    /// fresh inspection. Current bytes are flushed into the durable intent before
    /// cleanup; the existing original History record stays protected/pending.
    pub fn keep_current(self) -> Result<()> {
        let r = self.inner;
        let current = r
            .current
            .as_ref()
            .context("Source is missing; restore captured source before cleanup")?;
        let parent = r.intent.note.parent().unwrap();
        r.directory.validate(parent)?;
        anyhow::ensure!(
            read_bounded_recovery(&r.intent_path)? == r.intent_bytes
                && read_bounded_recovery(&r.intent.history)? == r.history_bytes,
            "Recovery changed since inspection; inspect again"
        );
        anyhow::ensure!(
            r.directory
                .recovery_revision(r.intent.note.file_name().unwrap())?
                .as_ref()
                == Some(current),
            "Source changed since inspection; inspect again"
        );
        for (name, revision) in &r.artifacts {
            anyhow::ensure!(
                r.directory.recovery_revision(name)? == *revision,
                "Recovery entry changed since inspection; retained"
            );
            if let Some(revision) = revision {
                let hash = Sha256::digest(&revision.bytes);
                anyhow::ensure!(
                    (revision.identity == r.intent.original_identity
                        && hash.as_slice() == r.intent.before_hash)
                        || (revision.identity == r.intent.staged_identity
                            && hash.as_slice() == r.intent.after_hash)
                        || revision == current,
                    "Recovery entry belongs to another revision; retained"
                );
            }
        }
        let mut intent = r.intent.clone();
        intent.retained_current = Some(current.bytes.clone());
        intent.persist(&r.intent_path)?;
        // Same optimistic namespace boundary as ordinary checked cleanup: POSIX
        // offers no conditional unlink. Recheck bytes and identity immediately
        // before each unlink, and stop on any uncertainty. Never rename over note.
        for (name, revision) in &r.artifacts {
            if revision.is_none() {
                continue;
            }
            anyhow::ensure!(
                r.directory
                    .recovery_revision(r.intent.note.file_name().unwrap())?
                    .as_ref()
                    == Some(current)
                    && r.directory.recovery_revision(name)? == *revision,
                "Source or recovery changed during cleanup; remaining recovery retained"
            );
            rustix::fs::unlinkat(&r.directory.file, name, rustix::fs::AtFlags::empty())?;
            r.directory.file.sync_all()?;
        }
        intent.resolved = true;
        intent.persist(&r.intent_path)?;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod interrupted_link_tests {
    use super::*;

    // Exact interruption boundaries from the real fallback, not a mount or
    // process-kill timing test: prepare intent, optional staging rename/link,
    // optional publication, then drop the editor without running cleanup.
    fn interrupted(stage: u8, raw: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let state = fixture.path().join("state");
        let actual_root = fixture.path().join("vault-real");
        fs::create_dir(&actual_root).unwrap();
        // Exercise a noncanonical parent on Linux too, just like Darwin's
        // /var -> /private/var temporary directory. Production stages beside
        // the canonical editor path; recovery records require that same parent.
        std::os::unix::fs::symlink(&actual_root, &root).unwrap();
        assert_ne!(root, root.canonicalize().unwrap());
        let note = root.join("Synthetic.md");
        fs::write(&note, b"\xef\xbb\xbforiginal\r\n").unwrap();
        let mut editor = FileEditor::open(&note, &state).unwrap();
        // Ancestor replacement tests must move the real directory, not its alias.
        let note = editor.draft.path.clone();
        editor.set_text("proposed\r\n".into()).unwrap();
        if raw {
            let original = b"\xef\xbb\xbforiginal\xff\r\n";
            fs::write(&note, original).unwrap();
            let meta = fs::metadata(&note).unwrap();
            editor.raw_transaction = Some(Arc::new(ByteTransaction {
                before: original.to_vec(),
                after: b"proposed\r\n".to_vec(),
                before_identity: (meta.dev(), meta.ino()),
                artifacts: Mutex::default(),
                history: Mutex::default(),
            }));
        }
        let (mut staged, name) = editor.directory.temporary().unwrap();
        staged.write_all(editor.proposed_bytes()).unwrap();
        staged.sync_all().unwrap();
        let displaced = editor.draft.path.parent().unwrap().join(&name);
        let history = editor.begin_save_history(&displaced, None).unwrap();
        let prepared = format!(".okilum-save-{}", uuid::Uuid::new_v4());
        editor
            .record_link_recovery(&history, name.as_ref(), &prepared)
            .unwrap();
        let directory = &editor.directory.file;
        if stage >= 1 {
            rustix::fs::renameat(directory, name.as_str(), directory, prepared.as_str()).unwrap();
        }
        if stage >= 2 {
            rustix::fs::linkat(
                directory,
                "Synthetic.md",
                directory,
                name.as_str(),
                rustix::fs::AtFlags::empty(),
            )
            .unwrap();
        }
        if stage == 3 {
            // Legacy plain-rename records must remain inspectable too.
            rustix::fs::renameat(directory, prepared.as_str(), directory, "Synthetic.md").unwrap();
        }
        if stage >= 4 {
            rustix::fs::unlinkat(directory, name.as_str(), rustix::fs::AtFlags::empty()).unwrap();
            rustix::fs::renameat(directory, "Synthetic.md", directory, name.as_str()).unwrap();
        }
        if stage >= 5 {
            rustix::fs::linkat(
                directory,
                prepared.as_str(),
                directory,
                "Synthetic.md",
                rustix::fs::AtFlags::empty(),
            )
            .unwrap();
        }
        directory.sync_all().unwrap();
        drop(editor);
        (fixture, note, state)
    }

    #[test]
    fn captured_source_restores_exclusively_after_a_crash_in_the_namespace_gap() {
        for raw in [false, true] {
            for external in [false, true] {
                let (_fixture, note, state) = interrupted(4, raw);
                assert!(!note.exists());
                let inspection = FileEditor::inspect_interrupted_saves(&note, &state)
                    .unwrap()
                    .pop()
                    .unwrap();
                assert_eq!(inspection.state(), InterruptedSaveState::SourceMissing);
                assert_eq!(inspection.current_bytes(), None);
                assert!(!inspection.original_bytes().is_empty());
                if external {
                    fs::write(&note, b"newer external").unwrap();
                    assert!(inspection.restore_missing_source().is_err());
                    assert_eq!(fs::read(&note).unwrap(), b"newer external");
                } else {
                    inspection.restore_missing_source().unwrap();
                    assert_eq!(fs::read(&note).unwrap(), inspection.original_bytes());
                }
                drop(inspection);
                FileEditor::inspect_interrupted_saves(&note, &state)
                    .unwrap()
                    .pop()
                    .unwrap()
                    .keep_current()
                    .unwrap();
                assert_eq!(fs::metadata(&note).unwrap().nlink(), 1);
                assert_eq!(fs::read_dir(note.parent().unwrap()).unwrap().count(), 1);
            }
            let (_fixture, note, state) = interrupted(5, raw);
            let inspection = FileEditor::inspect_interrupted_saves(&note, &state)
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(inspection.state(), InterruptedSaveState::ProposedPresent);
            inspection.keep_current().unwrap();
            assert_eq!(fs::read(&note).unwrap(), b"proposed\r\n");
            assert_eq!(fs::metadata(&note).unwrap().nlink(), 1);
        }
    }
    #[test]
    fn every_interruption_boundary_keeps_current_original_and_dirty_draft() {
        for raw in [false, true] {
            for stage in 0..4 {
                let (_fixture, note, state) = interrupted(stage, raw);
                let before = fs::read(&note).unwrap();
                let draft_paths: Vec<_> = fs::read_dir(&state)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .filter(|p| p.extension().is_some_and(|e| e == "json"))
                    .collect();
                assert_eq!(draft_paths.len(), 1);
                let journal_before = fs::read(&draft_paths[0]).unwrap();
                let mut inspections = FileEditor::inspect_interrupted_saves(&note, &state).unwrap();
                assert_eq!(inspections.len(), 1);
                let inspection = inspections.pop().unwrap();
                assert_eq!(
                    inspection.state(),
                    if stage < 3 {
                        InterruptedSaveState::OriginalPresent
                    } else {
                        InterruptedSaveState::ProposedPresent
                    }
                );
                assert_eq!(inspection.current_bytes(), Some(before.as_slice()));
                let original = if raw {
                    b"\xef\xbb\xbforiginal\xff\r\n".as_slice()
                } else {
                    b"\xef\xbb\xbforiginal\r\n".as_slice()
                };
                assert_eq!(inspection.original_bytes(), original);
                inspection.keep_current().unwrap();
                assert_eq!(fs::read(&note).unwrap(), before);
                assert_eq!(fs::metadata(&note).unwrap().nlink(), 1);
                assert_eq!(fs::read_dir(note.parent().unwrap()).unwrap().count(), 1);
                assert_eq!(fs::read(&draft_paths[0]).unwrap(), journal_before);
                let listing = crate::source_history::list(&state, note.parent().unwrap()).unwrap();
                assert!(listing.versions.iter().any(|v| v.bytes() == original));
                let intent = fs::read_dir(state.join("source-history"))
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| p.extension().is_some_and(|e| e == "link-recovery"))
                    .unwrap();
                let record: LinkRecovery =
                    serde_json::from_slice(&fs::read(intent).unwrap()).unwrap();
                assert!(record.resolved);
                assert_eq!(record.retained_current, Some(before));
                assert!(FileEditor::inspect_interrupted_saves(&note, &state)
                    .unwrap()
                    .is_empty());
            }
        }
    }

    #[test]
    fn stale_revision_and_same_byte_replacement_cannot_authorize_cleanup() {
        for same_bytes in [false, true] {
            let (_fixture, note, state) = interrupted(3, false);
            let inspection = FileEditor::inspect_interrupted_saves(&note, &state)
                .unwrap()
                .pop()
                .unwrap();
            let replacement = note.with_file_name("external.md");
            fs::write(
                &replacement,
                if same_bytes {
                    b"proposed\r\n".as_slice()
                } else {
                    b"external".as_slice()
                },
            )
            .unwrap();
            fs::rename(replacement, &note).unwrap();
            let expected = fs::read(&note).unwrap();
            assert!(inspection.keep_current().is_err());
            assert_eq!(fs::read(&note).unwrap(), expected);
            assert_eq!(fs::read_dir(note.parent().unwrap()).unwrap().count(), 2);
            let inspection = FileEditor::inspect_interrupted_saves(&note, &state)
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(inspection.current_bytes(), Some(expected.as_slice()));
            inspection.keep_current().unwrap();
            assert_eq!(fs::read(&note).unwrap(), expected);
        }
    }

    #[test]
    fn foreign_preimage_bytes_and_symlink_are_retained() {
        for symlink in [false, true] {
            let (_fixture, note, state) = interrupted(3, false);
            let inspection = FileEditor::inspect_interrupted_saves(&note, &state)
                .unwrap()
                .pop()
                .unwrap();
            let displaced = note
                .parent()
                .unwrap()
                .join(&inspection.inner.intent.displaced);
            if symlink {
                fs::remove_file(&displaced).unwrap();
                std::os::unix::fs::symlink(&note, &displaced).unwrap();
            } else {
                fs::write(&displaced, b"external old-fd bytes").unwrap();
            }
            assert!(inspection.keep_current().is_err());
            assert!(fs::symlink_metadata(&displaced).is_ok());
            assert_eq!(fs::read(&note).unwrap(), b"proposed\r\n");
            if !symlink {
                let fresh = FileEditor::inspect_interrupted_saves(&note, &state)
                    .unwrap()
                    .pop()
                    .unwrap();
                assert!(fresh.keep_current().is_err());
                assert_eq!(fs::read(&displaced).unwrap(), b"external old-fd bytes");
            }
        }
    }

    #[test]
    fn interrupted_cleanup_reinspects_remaining_links_without_touching_current() {
        for remove_prepared in [false, true] {
            let (_fixture, note, state) = interrupted(2, true);
            let inspection = FileEditor::inspect_interrupted_saves(&note, &state)
                .unwrap()
                .pop()
                .unwrap();
            // Simulate interruption after the durable Keep-current snapshot and
            // exactly one unlink, before the resolved acknowledgement.
            let mut intent = inspection.inner.intent.clone();
            intent.retained_current = Some(inspection.current_bytes().unwrap().to_vec());
            crate::source_state::persist(
                &inspection.inner.intent_path,
                &serde_json::to_vec(&intent).unwrap(),
            )
            .unwrap();
            let removed = if remove_prepared {
                &intent.prepared
            } else {
                &intent.displaced
            };
            fs::remove_file(note.parent().unwrap().join(removed)).unwrap();
            let current = fs::read(&note).unwrap();
            drop(inspection);
            let fresh = FileEditor::inspect_interrupted_saves(&note, &state)
                .unwrap()
                .pop()
                .unwrap();
            fresh.keep_current().unwrap();
            assert_eq!(fs::read(&note).unwrap(), current);
            assert_eq!(fs::metadata(&note).unwrap().nlink(), 1);
            assert_eq!(fs::read_dir(note.parent().unwrap()).unwrap().count(), 1);
        }
    }

    #[test]
    fn replaced_parent_is_refused_after_restart_with_a_matching_public_revision() {
        let (fixture, note, state) = interrupted(3, false);
        let original_parent = note.parent().unwrap();
        let moved = fixture.path().join("moved-vault");
        fs::rename(original_parent, &moved).unwrap();
        fs::create_dir(original_parent).unwrap();
        fs::write(&note, b"proposed\r\n").unwrap();
        assert!(FileEditor::inspect_interrupted_saves(&note, &state).is_err());
        assert_eq!(fs::read(&note).unwrap(), b"proposed\r\n");
        assert_eq!(fs::read_dir(&moved).unwrap().count(), 2);
    }

    #[test]
    fn unresolved_intent_blocks_worker_retry_until_explicit_resolution() {
        let (_fixture, note, state) = interrupted(3, false);
        let mut editor = FileEditor::open(&note, &state).unwrap();
        assert!(editor.save().unwrap_err().is::<UnconfirmedSave>());
        let inspection = editor.inspect_uncertain_saves().unwrap().pop().unwrap();
        inspection.keep_current().unwrap();
        assert_eq!(editor.save().unwrap(), Save::Saved);
        assert_eq!(fs::read(&note).unwrap(), b"proposed\r\n");
    }
}
