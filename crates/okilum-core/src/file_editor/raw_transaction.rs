//! Explicit byte transactions share FileEditor's native publication path.
//! Ordinary editor opens and drafts remain strict, lossless UTF-8.
use super::*;
use std::{collections::BTreeMap, ffi::OsString};
#[cfg(all(test, unix))]
thread_local! { static FAIL_STAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(unix)]
type Identity = (u64, u64);
#[cfg(windows)]
type Identity = (u32, u32, u32);

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
            draft: Draft {
                path: path.clone(),
                base: String::from_utf8_lossy(&current).into_owned(),
                text: String::from_utf8_lossy(proposed).into_owned(),
            },
            directory,
            on_disk: None,
            journal: state.join(format!("{key}.json")),
            lock: Arc::new(lock),
            writes: Arc::default(),
            raw_transaction: Some(transaction),
        };
        let result = editor.save_before_exchange(before_exchange);
        if !matches!(result, Ok(Save::Saved)) {
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
    pub(super) fn track_raw_windows_preimage(
        &self,
        path: &Path,
        identity: (u64, u64),
    ) -> Result<()> {
        self.track_raw_windows_identity(
            path,
            (
                identity.0 as u32,
                (identity.1 >> 32) as u32,
                identity.1 as u32,
            ),
        )
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
                    (
                        u64::from(identity.0),
                        u64::from(identity.1) << 32 | u64::from(identity.2),
                    ),
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
        expected_identity: (u64, u64),
    ) -> Result<()> {
        let record = crate::source_history::RawRecord::load(history)?;
        let name = record
            .displaced
            .file_name()
            .context("Missing preimage name")?;
        let (_, bytes, info) = native.read(name)?;
        let identity = crate::windows_files::identity(&info);
        anyhow::ensure!(
            bytes == self.base_bytes()
                && (
                    u64::from(identity.0),
                    u64::from(identity.1) << 32 | u64::from(identity.2)
                ) == expected_identity,
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
                rustix::fs::renameat(&self.directory.file, &original, &self.directory.file, note)?;
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
    fn unsupported_exchange_raw_failures_preserve_bytes_and_leave_no_tails() {
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
