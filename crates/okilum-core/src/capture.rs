//! Explicit local capture into an existing, user-selected vault folder.
//! Operational records belong outside the disposable index. No reader protocol IO.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[path = "capture_attachments.rs"]
pub mod attachments;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    pub operation_id: String,
    pub path: PathBuf,
}

/// One authored capture. Keep this value on failure: a new attempt must not
/// silently allocate another destination after publication became uncertain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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

/// Recover bound drafts and unfinished saves for this exact vault. Unbound
/// legacy intents are exposed separately by `load_recovery`, never auto-bound.
/// Corruption is an error; completed operations never become drafts.
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
        let bytes = std::fs::read(entry.path())?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value.get("root").is_none() {
            let legacy: Capture = serde_json::from_value(value)?;
            legacy.validate()?;
            anyhow::ensure!(
                name == format!("{}.intent.json", legacy.receipt.operation_id),
                "Legacy capture identity mismatch"
            );
            continue;
        }
        let record: Record = serde_json::from_value(value)?;
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
        if source_dismissed(&root, records, &capture)? {
            continue;
        }
        if attachments::owns_operation(&root, records, id)? {
            continue;
        }
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
    #[serde(default)]
    pub copy_origin: Option<CopyPreview>,
    #[serde(default)]
    pub attachment_attempt: Option<String>,
}

impl Default for ComposerDraft {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            text: String::new(),
            folder: None,
            attempt: None,
            copy_origin: None,
            attachment_attempt: None,
        }
    }
}

impl ComposerDraft {
    /// Checkpoint first, including copy provenance. Every retry after restart
    /// revalidates the reviewed source before any new canonical publication.
    pub fn publish(&self, root: &Path, drafts: &Path, records: &Path) -> Result<Receipt> {
        anyhow::ensure!(
            self.attachment_attempt.is_none(),
            "This capture has pending attachments; use its attachment publisher"
        );
        if let Some(origin) = &self.copy_origin {
            anyhow::ensure!(
                root.canonicalize()? == origin.root,
                "Copy destination vault changed; preview again"
            );
        }
        self.persist(root, records)?;
        let attempt = self
            .attempt
            .as_ref()
            .context("Capture has no prepared operation")?;
        if let Some(origin) = &self.copy_origin {
            if read_optional::<Receipt>(
                &records.join(format!("{}.receipt.json", attempt.receipt.operation_id)),
            )?
            .is_none()
            {
                origin.validate_copy(root, records, self)?;
            }
        }
        attempt.publish(root, drafts, records)
    }

    pub fn prepare_save(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.attachment_attempt.is_none(),
            "This capture has pending attachments; retry its attachment publisher"
        );
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
        if let Some(operation) = &self.attachment_attempt {
            anyhow::ensure!(
                uuid::Uuid::parse_str(operation)?.to_string() == *operation
                    && self.attempt.is_none()
                    && self.copy_origin.is_none(),
                "Invalid attachment capture operation"
            );
        }
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
            if let Some(operation) = &prior.draft.attachment_attempt {
                anyhow::ensure!(
                    self.attachment_attempt.as_ref() == Some(operation)
                        && self.text == prior.draft.text
                        && self.folder == prior.draft.folder,
                    "Pending attachment text, folder and operation are immutable"
                );
            }
            if let Some(attempt) = prior.draft.attempt {
                anyhow::ensure!(
                    self.copy_origin == prior.draft.copy_origin,
                    "Capture copy provenance changed"
                );
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
        let mut draft = record.draft;
        anyhow::ensure!(
            uuid::Uuid::parse_str(&draft.id)?.to_string() == draft.id
                && name == format!("{}.composer.json", draft.id),
            "Composer record identity mismatch"
        );
        if attachments::reconcile_recovered(root, records, &mut draft)? {
            continue;
        }
        if let Some(attempt) = &draft.attempt {
            attempt.validate()?;
            anyhow::ensure!(
                attempt.text() == draft.text && attempt.path().parent() == draft.folder.as_deref(),
                "Recovered composer and capture disagree"
            );
            captures.retain(|capture| capture.receipt.operation_id != attempt.receipt.operation_id);
            if source_dismissed(root, records, attempt)? {
                continue;
            }
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
        if !draft.text.is_empty() || draft.attempt.is_some() || draft.attachment_attempt.is_some() {
            drafts.push(draft);
        }
    }
    drafts.extend(captures.into_iter().map(|capture| ComposerDraft {
        id: uuid::Uuid::new_v4().to_string(),
        text: capture.text.clone(),
        folder: capture.path().parent().map(Path::to_path_buf),
        attempt: Some(capture),
        copy_origin: None,
        attachment_attempt: None,
    }));
    drafts.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(drafts)
}

/// A preserved recovery source. An absent root means genuinely unknown, even
/// when the legacy intent happens to live in the current vault's state folder.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CopySource {
    record_name: String,
    record_bytes: Vec<u8>,
    capture: Capture,
    bound_root: Option<PathBuf>,
}

impl CopySource {
    pub fn text(&self) -> &str {
        self.capture.text()
    }
    pub fn original_path(&self) -> &Path {
        self.capture.path()
    }
    pub fn bound_root(&self) -> Option<&Path> {
        self.bound_root.as_deref()
    }

    pub fn from_composer(root: &Path, records: &Path, draft: &ComposerDraft) -> Result<Self> {
        let capture = draft
            .attempt
            .as_ref()
            .context("Choose a pending capture to copy")?;
        let record_name = format!("{}.composer.json", draft.id);
        let record_bytes = std::fs::read(records.join(&record_name))?;
        let record: ComposerRecord = serde_json::from_slice(&record_bytes)?;
        anyhow::ensure!(
            record.root == root.canonicalize()? && record.draft == *draft,
            "Capture draft changed; reload recovery before previewing"
        );
        Ok(Self {
            record_name,
            record_bytes,
            capture: capture.clone(),
            bound_root: Some(record.root),
        })
    }

    /// Read-only preview: no operation UUID is allocated and no source file or
    /// recovery record is changed. The user must explicitly choose vault/folder.
    pub fn preview(&self, root: &Path, folder: &Path, records: &Path) -> Result<CopyPreview> {
        self.verify(records)?;
        validate_copy_folder(root, folder)?;
        Ok(CopyPreview {
            source: self.clone(),
            root: root.canonicalize()?,
            folder: folder.to_path_buf(),
        })
    }

    fn verify(&self, records: &Path) -> Result<()> {
        self.capture.validate()?;
        anyhow::ensure!(
            Path::new(&self.record_name).components().count() == 1
                && matches!(
                    Path::new(&self.record_name).components().next(),
                    Some(Component::Normal(_))
                ),
            "Invalid copy source record path"
        );
        anyhow::ensure!(
            std::fs::read(records.join(&self.record_name))? == self.record_bytes,
            "Original recovery record changed; preview it again before Save copy"
        );
        if self.record_name.ends_with(".composer.json") {
            let record: ComposerRecord = serde_json::from_slice(&self.record_bytes)?;
            anyhow::ensure!(
                self.bound_root.as_ref() == Some(&record.root)
                    && record.draft.attempt.as_ref() == Some(&self.capture)
                    && self.record_name == format!("{}.composer.json", record.draft.id),
                "Copy source identity mismatch"
            );
        } else {
            let value: serde_json::Value = serde_json::from_slice(&self.record_bytes)?;
            if value.get("root").is_some() {
                let record: Record = serde_json::from_value(value)?;
                anyhow::ensure!(
                    self.bound_root.as_ref() == Some(&record.root)
                        && record.capture == self.capture,
                    "Copy source vault binding mismatch"
                );
            } else {
                let capture: Capture = serde_json::from_value(value)?;
                anyhow::ensure!(
                    self.bound_root.is_none() && capture == self.capture,
                    "Legacy source must remain unbound"
                );
            }
            anyhow::ensure!(
                self.record_name == format!("{}.intent.json", self.capture.receipt.operation_id),
                "Copy source record identity mismatch"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CopyPreview {
    source: CopySource,
    root: PathBuf,
    folder: PathBuf,
}

impl CopyPreview {
    pub fn source(&self) -> &CopySource {
        &self.source
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Call only on the explicit Save copy action, after displaying the preview.
    /// The old draft/intent is never rewritten or consumed by this operation.
    pub fn start_copy(&self) -> Result<ComposerDraft> {
        let capture = Capture::new(&self.folder, self.source.text().to_owned())?;
        Ok(ComposerDraft {
            text: self.source.text().to_owned(),
            folder: Some(self.folder.clone()),
            attempt: Some(capture),
            copy_origin: Some(self.clone()),
            ..Default::default()
        })
    }

    fn validate_copy(&self, root: &Path, records: &Path, draft: &ComposerDraft) -> Result<()> {
        anyhow::ensure!(
            root.canonicalize()? == self.root,
            "Copy destination vault changed; preview again"
        );
        validate_copy_folder(root, &self.folder)?;
        self.source.verify(records)?;
        let attempt = draft
            .attempt
            .as_ref()
            .context("Save copy has no operation")?;
        anyhow::ensure!(
            attempt.receipt.operation_id != self.source.capture.receipt.operation_id
                && draft.text == self.source.text()
                && draft.folder.as_ref() == Some(&self.folder),
            "Copy must retain the reviewed text and use a new operation"
        );
        Ok(())
    }
}

pub struct RecoveryInbox {
    pub drafts: Vec<ComposerDraft>,
    pub unbound: Vec<CopySource>,
}

pub fn load_recovery(root: &Path, records: &Path) -> Result<RecoveryInbox> {
    let drafts = recover_composers(root, records)?;
    let mut unbound = Vec::new();
    let entries = match std::fs::read_dir(records) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecoveryInbox { drafts, unbound })
        }
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".intent.json") {
            continue;
        }
        let bytes = std::fs::read(entry.path())?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value.get("root").is_some() {
            continue;
        }
        let capture: Capture = serde_json::from_value(value)?;
        capture.validate()?;
        anyhow::ensure!(
            name == format!("{}.intent.json", capture.receipt.operation_id),
            "Legacy capture record identity mismatch"
        );
        if source_dismissed(root, records, &capture)? {
            continue;
        }
        // An old receipt has no vault binding either. It cannot choose a vault
        // or prove that a note in the current vault belongs to this intent.
        unbound.push(CopySource {
            record_name: name.into(),
            record_bytes: bytes,
            capture,
            bound_root: None,
        });
    }
    unbound.sort_by(|a, b| a.record_name.cmp(&b.record_name));
    Ok(RecoveryInbox { drafts, unbound })
}

fn validate_copy_folder(root: &Path, folder: &Path) -> Result<()> {
    anyhow::ensure!(
        folder
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
            && !crate::vault::service_path(folder),
        "Choose a real capture folder inside the selected vault"
    );
    #[cfg(unix)]
    {
        use rustix::fs::{open, openat, Mode, OFlags};
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut directory = open(root, flags, Mode::empty())?;
        for component in folder.components() {
            directory = openat(&directory, component.as_os_str(), flags, Mode::empty())?;
        }
    }
    #[cfg(windows)]
    {
        let _root = crate::windows_files::Directory::open(root)?;
        let _folder = crate::windows_files::Directory::open(&root.join(folder))?;
    }
    Ok(())
}

/// An explicit presentation decision after Save copy. This is not deletion,
/// completion of the original operation, or authority to discard any source.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SupersededSource {
    origin: CopyPreview,
    saved: Capture,
}

/// Hide a recovery source only after this exact copy has a durable receipt.
/// Keep the successful ComposerDraft until the user chooses this action.
pub fn dismiss_saved_copy(root: &Path, records: &Path, copy: &ComposerDraft) -> Result<()> {
    let origin = copy
        .copy_origin
        .as_ref()
        .context("Choose a successfully saved recovery copy")?;
    let saved = copy
        .attempt
        .as_ref()
        .context("Copy has no saved operation")?;
    origin.validate_copy(root, records, copy)?;
    let decision = SupersededSource {
        origin: origin.clone(),
        saved: saved.clone(),
    };
    decision.validate(root, records)?;
    let name = superseded_name(&origin.source.capture.receipt.operation_id);
    persist_equal(records, &name, &decision)?;
    set_presentation(root, records, &name, true, None)
}
fn superseded_name(operation: &str) -> String {
    format!("{operation}.superseded.json")
}
impl SupersededSource {
    fn validate(&self, root: &Path, records: &Path) -> Result<()> {
        anyhow::ensure!(
            root.canonicalize()? == self.origin.root,
            "Superseded source belongs to another vault"
        );
        self.origin.source.verify(records)?;
        self.saved.validate()?;
        anyhow::ensure!(
            self.saved.text == self.origin.source.text()
                && self.saved.path().parent() == Some(self.origin.folder())
                && self.saved.receipt.operation_id
                    != self.origin.source.capture.receipt.operation_id,
            "Superseding copy differs from reviewed source"
        );
        let intent: Record = read_optional(
            &records.join(format!("{}.intent.json", self.saved.receipt.operation_id)),
        )?
        .context("Saved copy intent is missing; keep source visible")?;
        let receipt: Receipt = read_optional(
            &records.join(format!("{}.receipt.json", self.saved.receipt.operation_id)),
        )?
        .context("Saved copy receipt is missing; keep source visible")?;
        anyhow::ensure!(
            intent.root == self.origin.root
                && intent.capture == self.saved
                && receipt == self.saved.receipt,
            "Superseding copy receipt/intent differs"
        );
        Ok(())
    }
}
fn source_dismissed(root: &Path, records: &Path, capture: &Capture) -> Result<bool> {
    let Some(decision) = read_optional::<SupersededSource>(
        &records.join(superseded_name(&capture.receipt.operation_id)),
    )?
    else {
        return Ok(false);
    };
    decision.validate(root, records)?;
    anyhow::ensure!(
        decision.origin.source.capture == *capture,
        "Dismissed source identity changed"
    );
    Ok(presentation(
        root,
        records,
        &superseded_name(&capture.receipt.operation_id),
    )?
    .dismissed)
}

/// Read-only inspected recovery content and opaque compare/revision authority.
/// Reopen changes visibility only; it never retries a save or grants cleanup.
#[derive(Clone, Debug)]
pub struct DismissedRecovery {
    pub text: String,
    pub original_path: PathBuf,
    pub saved_path: PathBuf,
    pub assets: Vec<attachments::PublishedAsset>,
    pub revision: PresentationRevision,
}
#[derive(Clone, Debug)]
pub struct PresentationRevision {
    root: PathBuf,
    marker: String,
    marker_revision: String,
    revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PresentationState {
    root: PathBuf,
    marker_revision: String,
    revision: String,
    dismissed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_revision: Option<String>,
}
fn presentation_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn presentation(root: &Path, records: &Path, marker: &str) -> Result<PresentationState> {
    anyhow::ensure!(
        Path::new(marker).components().count() == 1 && marker.ends_with(".superseded.json"),
        "Invalid presentation marker"
    );
    let marker_revision = presentation_hash(&std::fs::read(records.join(marker))?);
    let root = root.canonicalize()?;
    if let Some(state) =
        read_optional::<PresentationState>(&records.join(format!("{marker}.presentation.json")))?
    {
        anyhow::ensure!(
            state.root == root
                && state.marker_revision == marker_revision
                && uuid::Uuid::parse_str(&state.revision)?.to_string() == state.revision,
            "Recovery presentation changed or belongs to another vault; inspect again"
        );
        return Ok(state);
    }
    // Delivered b015d727 markers are dismissed without a sidecar. Their exact
    // bytes form the legacy revision; no migration is performed by inspection.
    Ok(PresentationState {
        root,
        marker_revision: marker_revision.clone(),
        revision: marker_revision,
        dismissed: true,
        previous_revision: None,
    })
}
fn presentation_token(root: &Path, records: &Path, marker: &str) -> Result<PresentationRevision> {
    let state = presentation(root, records, marker)?;
    anyhow::ensure!(
        state.dismissed,
        "Recovery source has already been reopened; inspect again"
    );
    Ok(PresentationRevision {
        root: state.root,
        marker: marker.into(),
        marker_revision: state.marker_revision,
        revision: state.revision,
    })
}
fn set_presentation(
    root: &Path,
    records: &Path,
    marker: &str,
    dismissed: bool,
    expected: Option<&PresentationRevision>,
) -> Result<()> {
    let head = format!("{marker}.presentation.json");
    let _lock = crate::file_editor::FileEditor::reserve_destination(
        &records.join(&head),
        &records.join("presentation-locks"),
    )?;
    let current = presentation(root, records, marker)?;
    if let Some(expected) = expected {
        anyhow::ensure!(
            current.root == expected.root
                && marker == expected.marker
                && current.marker_revision == expected.marker_revision
                && current.revision == expected.revision
                && current.dismissed,
            "Dismissal decision changed since inspection; refresh before reopening"
        );
    }
    let state = PresentationState {
        revision: uuid::Uuid::new_v4().to_string(),
        dismissed,
        previous_revision: Some(current.revision.clone()),
        ..current
    };
    // Append immutable decision history before atomically replacing its head.
    persist_new(
        records,
        &format!("{}.presentation-event.json", state.revision),
        &state,
    )?;
    use std::io::Write;
    let mut temporary = tempfile::NamedTempFile::new_in(records)?;
    temporary.write_all(&serde_json::to_vec(&state)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(records.join(head))?;
    #[cfg(unix)]
    std::fs::File::open(records)?.sync_all()?;
    Ok(())
}
/// Inspect retained dismissed text and attachment sources without changing files.
pub fn inspect_dismissed(root: &Path, records: &Path) -> Result<Vec<DismissedRecovery>> {
    let entries = match std::fs::read_dir(records) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut result = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".superseded.json") {
            continue;
        }
        if name.ends_with(".attachments.superseded.json") {
            if let Some(source) = attachments::inspect_dismissed_source(root, records, name)? {
                result.push(source);
            }
        } else {
            let decision: SupersededSource =
                read_optional(&records.join(name))?.context("Dismissal marker disappeared")?;
            decision.validate(root, records)?;
            anyhow::ensure!(
                name == superseded_name(&decision.origin.source.capture.receipt.operation_id),
                "Dismissal marker identity changed"
            );
            if presentation(root, records, name)?.dismissed {
                result.push(DismissedRecovery {
                    text: decision.origin.source.text().into(),
                    original_path: decision.origin.source.original_path().into(),
                    saved_path: decision.saved.path().into(),
                    assets: Vec::new(),
                    revision: presentation_token(root, records, name)?,
                });
            }
        }
    }
    result.sort_by(|a, b| a.revision.marker.cmp(&b.revision.marker));
    Ok(result)
}
/// Explicit revision-checked undo of PRESENTATION dismissal. Original operations,
/// copies, frozen manifests and staging remain untouched and are never published.
pub fn reopen_dismissed(
    root: &Path,
    records: &Path,
    revision: &PresentationRevision,
) -> Result<()> {
    anyhow::ensure!(
        root.canonicalize()? == revision.root,
        "Choose the inspected vault before reopening"
    );
    let inspected = inspect_dismissed(root, records)?;
    anyhow::ensure!(
        inspected
            .iter()
            .any(|item| item.revision.marker == revision.marker
                && item.revision.marker_revision == revision.marker_revision
                && item.revision.revision == revision.revision),
        "Dismissal decision changed since inspection; refresh before reopening"
    );
    set_presentation(root, records, &revision.marker, false, Some(revision))
}

const HISTORY_MAX_ENTRIES: usize = 1024;
const HISTORY_RECORD_BYTES: usize = 8 * 1024 * 1024;
const HISTORY_TOTAL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresentationHistoryStatus {
    Dismissed,
    Reopened,
    Held,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresentationHistoryRelation {
    Current,
    Previous,
    UnlinkedHeld,
}
#[derive(Clone, Debug)]
pub struct PresentationHistoryEvent {
    pub revision: String,
    pub status: PresentationHistoryStatus,
    pub relation: PresentationHistoryRelation,
    pub previous_revision: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentationHistoryRevision {
    root: PathBuf,
    marker: String,
    digest: String,
}
#[derive(Clone, Debug)]
pub struct PresentationTimeline {
    pub marker: String,
    pub marker_revision: String,
    pub current_revision: Option<String>,
    pub current: PresentationHistoryStatus,
    /// Verified head -> predecessor order only. Unlinked records follow with
    /// UnlinkedHeld; their lexical display order is NEVER a chronology claim.
    pub events: Vec<PresentationHistoryEvent>,
    pub held: Vec<String>,
    pub revision: PresentationHistoryRevision,
}
#[derive(Clone, Debug)]
pub struct PresentationHistory {
    pub sources: Vec<PresentationTimeline>,
    pub held: Vec<String>,
    pub complete: bool,
}

/// Bounded read-only projection over immutable marker/events and exact head.
/// No timestamps, UUID sort order, or orphan events are used to infer commits.
/// Legacy unlinked histories remain held; this function never repairs them.
pub fn inspect_presentation_history(root: &Path, records: &Path) -> Result<PresentationHistory> {
    use std::collections::{BTreeMap, BTreeSet};
    let root = root.canonicalize()?;
    let mut result = PresentationHistory {
        sources: Vec::new(),
        held: Vec::new(),
        complete: true,
    };
    let entries = match std::fs::read_dir(records) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(error.into()),
    };
    let mut files = BTreeMap::new();
    let mut encountered = BTreeSet::new();
    let mut budget = HISTORY_TOTAL_BYTES;
    for (index, entry) in entries.enumerate() {
        if index >= HISTORY_MAX_ENTRIES {
            result.held.push(
                "History entry bound reached; refresh or inspect retained records externally"
                    .into(),
            );
            break;
        }
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            result.held.push("Unidentifiable history entry".into());
            continue;
        };
        if !name.ends_with(".superseded.json")
            && !name.ends_with(".superseded.json.presentation.json")
            && !name.ends_with(".presentation-event.json")
        {
            continue;
        }
        encountered.insert(name.to_owned());
        if budget <= 1 {
            result
                .held
                .push("History byte bound reached; projection is incomplete".into());
            break;
        }
        let limit = HISTORY_RECORD_BYTES.min(budget - 1);
        match attachments::read_bytes(&records.join(name), limit) {
            Ok(Some(bytes)) => {
                budget -= bytes.len();
                files.insert(name.to_owned(), bytes);
            }
            Ok(None) => result
                .held
                .push(format!("History entry disappeared: {name}")),
            Err(error) => {
                budget = budget.saturating_sub(limit + 1);
                result.held.push(format!("Held {name}: {error:#}"));
            }
        }
    }
    let mut events = BTreeMap::<String, Vec<(String, PresentationState)>>::new();
    for (name, bytes) in &files {
        if !name.ends_with(".presentation-event.json") {
            continue;
        }
        let state = match serde_json::from_slice::<PresentationState>(bytes) {
            Ok(state) => state,
            Err(error) => {
                result
                    .held
                    .push(format!("Corrupt history event {name}: {error}"));
                continue;
            }
        };
        if state.root != root
            || !uuid::Uuid::parse_str(&state.revision)
                .is_ok_and(|u| u.to_string() == state.revision)
            || state.previous_revision.as_ref().is_some_and(|previous| {
                previous == &state.revision
                    || (previous != &state.marker_revision
                        && !uuid::Uuid::parse_str(previous)
                            .is_ok_and(|u| u.to_string() == *previous))
            })
            || name != &format!("{}.presentation-event.json", state.revision)
        {
            result
                .held
                .push(format!("History event scope/identity mismatch: {name}"));
            continue;
        }
        events
            .entry(state.marker_revision.clone())
            .or_default()
            .push((name.clone(), state));
    }
    let mut known_markers = BTreeSet::new();
    for (name, bytes) in &files {
        if !name.ends_with(".superseded.json") {
            continue;
        }
        let marker_hash = presentation_hash(bytes);
        let value: serde_json::Value = match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(error) => {
                result.held.push(format!("Corrupt marker {name}: {error}"));
                continue;
            }
        };
        let (bound, identity) = if name.ends_with(".attachments.superseded.json") {
            (
                value.get("root"),
                value.get("source_composer_id").and_then(|v| v.as_str()),
            )
        } else {
            (
                value.pointer("/origin/root"),
                value
                    .pointer("/origin/source/capture/receipt/operation_id")
                    .and_then(|v| v.as_str()),
            )
        };
        let valid = bound
            .and_then(|v| serde_json::from_value::<PathBuf>(v.clone()).ok())
            .is_some_and(|p| p == root)
            && identity.is_some_and(|id| {
                uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id)
                    && (name == &format!("{id}.superseded.json")
                        || name == &format!("{id}.attachments.superseded.json"))
            });
        if !valid {
            result
                .held
                .push(format!("Marker scope/identity held: {name}"));
            continue;
        }
        let structurally_valid = if name.ends_with(".attachments.superseded.json") {
            attachments::validate_history_marker(&root, bytes, name)
        } else {
            (|| -> Result<()> {
                let decision: SupersededSource = serde_json::from_slice(bytes)?;
                decision.saved.validate()?;
                decision.origin.source.capture.validate()?;
                anyhow::ensure!(
                    decision.saved.text() == decision.origin.source.text()
                        && decision.saved.path().parent() == Some(decision.origin.folder())
                        && decision.saved.receipt.operation_id
                            != decision.origin.source.capture.receipt.operation_id,
                    "Inconsistent saved-source marker"
                );
                Ok(())
            })()
        };
        if let Err(error) = structurally_valid {
            result
                .held
                .push(format!("Marker structure held {name}: {error:#}"));
            continue;
        }
        known_markers.insert(marker_hash.clone());
        let source_events = events.get(&marker_hash).cloned().unwrap_or_default();
        let by_revision: BTreeMap<_, _> = source_events
            .iter()
            .map(|(_, state)| (state.revision.clone(), state.clone()))
            .collect();
        let head_name = format!("{name}.presentation.json");
        let mut held = Vec::new();
        let mut current = PresentationHistoryStatus::Held;
        let mut current_revision = None;
        let mut rows = Vec::new();
        let mut seen = BTreeSet::new();
        if let Some(head_bytes) = files.get(&head_name) {
            match serde_json::from_slice::<PresentationState>(head_bytes) {
                Ok(head) if head.root == root && head.marker_revision == marker_hash && by_revision.get(&head.revision) == Some(&head) => {
                    current = if head.dismissed { PresentationHistoryStatus::Dismissed } else { PresentationHistoryStatus::Reopened };
                    current_revision = Some(head.revision.clone());
                    let mut next = Some(head.revision.clone());
                    while let Some(revision) = next {
                        if revision == marker_hash { break; }
                        if !seen.insert(revision.clone()) { held.push("History predecessor cycle; chronology held".into()); break; }
                        let Some(state) = by_revision.get(&revision) else { held.push(format!("Missing predecessor {revision}; chronology held")); break };
                        rows.push(PresentationHistoryEvent { revision: revision.clone(), status: if state.dismissed { PresentationHistoryStatus::Dismissed } else { PresentationHistoryStatus::Reopened }, relation: if revision == head.revision { PresentationHistoryRelation::Current } else { PresentationHistoryRelation::Previous }, previous_revision: state.previous_revision.clone() });
                        if state.previous_revision.is_none() { held.push(format!("Legacy event {revision} has no predecessor; earlier chronology is unproven")); }
                        next = state.previous_revision.clone();
                    }
                }
                _ => held.push("Head is corrupt, mismatched, or lacks its exact immutable event; current decision held".into()),
            }
        } else if source_events.is_empty()
            && !encountered.contains(&head_name)
            && result.held.is_empty()
        {
            current = PresentationHistoryStatus::Dismissed;
            current_revision = Some(marker_hash.clone());
        } else {
            held.push(
                "Head is absent/unreadable or history is incomplete; current decision held".into(),
            );
        }
        for (_, state) in &source_events {
            if !seen.contains(&state.revision) {
                rows.push(PresentationHistoryEvent {
                    revision: state.revision.clone(),
                    status: if state.dismissed {
                        PresentationHistoryStatus::Dismissed
                    } else {
                        PresentationHistoryStatus::Reopened
                    },
                    relation: PresentationHistoryRelation::UnlinkedHeld,
                    previous_revision: state.previous_revision.clone(),
                });
                held.push(format!(
                    "Unlinked event {}; historical vs unpublished status is unproven",
                    state.revision
                ));
            }
        }
        let mut revision_material = vec![(name.clone(), marker_hash.clone())];
        if let Some(head) = files.get(&head_name) {
            revision_material.push((head_name.clone(), presentation_hash(head)));
        }
        for (event_name, _) in &source_events {
            revision_material.push((event_name.clone(), presentation_hash(&files[event_name])));
        }
        let projection_digest = presentation_hash(&serde_json::to_vec(&revision_material)?);
        result.sources.push(PresentationTimeline {
            marker: name.clone(),
            marker_revision: marker_hash,
            current_revision,
            current,
            events: rows,
            held,
            revision: PresentationHistoryRevision {
                root: root.clone(),
                marker: name.clone(),
                digest: projection_digest,
            },
        });
    }
    for (marker, unbound) in events {
        if !known_markers.contains(&marker) {
            for (name, _) in unbound {
                result.held.push(format!(
                    "Event {name} has no validated marker; scope/chronology held"
                ));
            }
        }
    }
    for name in files
        .keys()
        .filter(|name| name.ends_with(".superseded.json.presentation.json"))
    {
        if !files.contains_key(name.trim_end_matches(".presentation.json")) {
            result
                .held
                .push(format!("Head {name} has no retained marker; held"));
        }
    }
    result.complete =
        result.held.is_empty() && result.sources.iter().all(|source| source.held.is_empty());
    Ok(result)
}
/// Compare a read-only history projection against current immutable bytes/head.
/// No undo/publication authority is conveyed by this token or this comparison.
pub fn presentation_history_is_current(
    root: &Path,
    records: &Path,
    revision: &PresentationHistoryRevision,
) -> Result<bool> {
    anyhow::ensure!(
        root.canonicalize()? == revision.root,
        "History projection belongs to another vault"
    );
    let history = inspect_presentation_history(root, records)?;
    anyhow::ensure!(
        history.complete,
        "History is incomplete/held; refresh inspection"
    );
    Ok(history
        .sources
        .iter()
        .any(|source| source.revision == *revision))
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
    read_destination_bounded(root, relative, usize::MAX - 1)
}

fn read_destination_bounded(root: &Path, relative: &Path, max: usize) -> Result<Option<Vec<u8>>> {
    read_destination_guarded(root, relative, max, true)
}

/// Read-only inspection never flushes, repairs or publishes canonical source.
fn inspect_destination_bounded(
    root: &Path,
    relative: &Path,
    max: usize,
) -> Result<Option<Vec<u8>>> {
    read_destination_guarded(root, relative, max, false)
}

fn read_destination_guarded(
    root: &Path,
    relative: &Path,
    max: usize,
    flush: bool,
) -> Result<Option<Vec<u8>>> {
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
        (&mut file)
            .take(max.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= max,
            "Capture destination exceeds recovery bounds"
        );
        if flush {
            file.sync_all()?;
            std::fs::File::from(folder).sync_all()?;
        }
        Ok(Some(bytes))
    }
    #[cfg(windows)]
    {
        let _root = crate::windows_files::Directory::open(root)?;
        let folder = crate::windows_files::Directory::open(
            &root.join(relative.parent().context("Missing capture folder")?),
        )?;
        use std::io::Read;
        let mut file =
            match folder.open_file(relative.file_name().context("Missing capture filename")?) {
                Ok(file) => file,
                Err(e)
                    if e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    return Ok(None)
                }
                Err(e) => return Err(e),
            };
        let mut bytes = Vec::new();
        (&mut file)
            .take(max.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= max,
            "Capture destination exceeds recovery bounds"
        );
        if flush {
            file.sync_all()?;
        }
        Ok(Some(bytes))
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
    #[test]
    fn divergent_capture_copy_requires_preview_and_preserves_original_intent() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let locks = state.path().join("locks");
        std::fs::create_dir(vault.path().join("Copies")).unwrap();
        let mut original = ComposerDraft {
            text: "authored\r\n".into(),
            folder: Some(PathBuf::new()),
            ..Default::default()
        };
        original.prepare_save().unwrap();
        original.persist(vault.path(), &records).unwrap();
        let attempt = original.attempt.as_ref().unwrap();
        persist_new(
            &records,
            &format!("{}.intent.json", attempt.receipt.operation_id),
            &Record {
                root: vault.path().canonicalize().unwrap(),
                capture: attempt.clone(),
            },
        )
        .unwrap();
        std::fs::write(vault.path().join(attempt.path()), "external divergent text").unwrap();
        assert!(original.publish(vault.path(), &locks, &records).is_err());
        let intent = records.join(format!("{}.intent.json", attempt.receipt.operation_id));
        let intent_before = std::fs::read(&intent).unwrap();
        let composer = records.join(format!("{}.composer.json", original.id));
        let composer_before = std::fs::read(&composer).unwrap();
        let count_before = std::fs::read_dir(&records).unwrap().count();
        let source = CopySource::from_composer(vault.path(), &records, &original).unwrap();
        let preview = source
            .preview(vault.path(), Path::new("Copies"), &records)
            .unwrap();
        assert_eq!(std::fs::read_dir(&records).unwrap().count(), count_before);
        assert_eq!(
            std::fs::read_dir(vault.path().join("Copies"))
                .unwrap()
                .count(),
            0
        );
        let copy = preview.start_copy().unwrap();
        assert_ne!(
            copy.attempt.as_ref().unwrap().receipt.operation_id,
            attempt.receipt.operation_id
        );
        let receipt = copy.publish(vault.path(), &locks, &records).unwrap();
        assert_eq!(
            copy.publish(vault.path(), &locks, &records).unwrap(),
            receipt
        );
        assert_eq!(
            std::fs::read_to_string(vault.path().join(&receipt.path)).unwrap(),
            "authored\r\n"
        );
        assert_eq!(
            std::fs::read_to_string(vault.path().join(attempt.path())).unwrap(),
            "external divergent text"
        );
        assert_eq!(std::fs::read(intent).unwrap(), intent_before);
        assert_eq!(std::fs::read(composer).unwrap(), composer_before);
        assert_eq!(
            std::fs::read_dir(vault.path().join("Copies"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn legacy_intent_stays_unbound_until_explicit_copy_in_selected_folder() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        prepare_records(&records).unwrap();
        std::fs::create_dir(vault.path().join("Chosen")).unwrap();
        let legacy = Capture::new(
            Path::new("UnknownOldFolder"),
            "\u{feff}legacy\r\nsource".into(),
        )
        .unwrap();
        let name = format!("{}.intent.json", legacy.receipt.operation_id);
        persist_new(&records, &name, &legacy).unwrap();
        // An old receipt still cannot establish which vault originally received it.
        persist_new(
            &records,
            &format!("{}.receipt.json", legacy.receipt.operation_id),
            &legacy.receipt,
        )
        .unwrap();
        let before = std::fs::read(records.join(&name)).unwrap();
        let inbox = load_recovery(vault.path(), &records).unwrap();
        assert!(inbox.drafts.is_empty());
        assert_eq!(inbox.unbound.len(), 1);
        let source = &inbox.unbound[0];
        assert!(source.bound_root().is_none());
        assert_eq!(source.original_path(), legacy.path());
        assert!(legacy
            .publish(vault.path(), &state.path().join("locks"), &records)
            .is_err());
        let preview = source
            .preview(vault.path(), Path::new("Chosen"), &records)
            .unwrap();
        assert_eq!(
            std::fs::read_dir(vault.path().join("Chosen"))
                .unwrap()
                .count(),
            0
        );
        let copy = preview.start_copy().unwrap();
        let receipt = copy
            .publish(vault.path(), &state.path().join("locks"), &records)
            .unwrap();
        assert_ne!(receipt.operation_id, legacy.receipt.operation_id);
        assert_eq!(receipt.path.parent(), Some(Path::new("Chosen")));
        assert_eq!(
            std::fs::read_to_string(vault.path().join(receipt.path)).unwrap(),
            legacy.text()
        );
        assert_eq!(std::fs::read(records.join(name)).unwrap(), before);
        assert!(!vault.path().join("UnknownOldFolder").exists());
        assert_eq!(
            load_recovery(vault.path(), &records).unwrap().unbound.len(),
            1
        );
    }

    #[test]
    fn copy_preview_is_revision_checked_and_refuses_guessed_or_unsafe_destination() {
        let vault = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        prepare_records(&records).unwrap();
        let legacy = Capture::new(Path::new(""), "retained".into()).unwrap();
        let name = format!("{}.intent.json", legacy.receipt.operation_id);
        persist_new(&records, &name, &legacy).unwrap();
        let source = load_recovery(vault.path(), &records)
            .unwrap()
            .unbound
            .remove(0);
        assert!(source
            .preview(vault.path(), Path::new("../outside"), &records)
            .is_err());
        assert!(source
            .preview(vault.path(), Path::new("missing"), &records)
            .is_err());
        let preview = source
            .preview(vault.path(), Path::new(""), &records)
            .unwrap();
        let copy = preview.start_copy().unwrap();
        let error = copy
            .publish(other.path(), &state.path().join("locks"), &records)
            .unwrap_err();
        assert!(error.to_string().contains("vault changed"));
        assert!(!records.join(format!("{}.composer.json", copy.id)).exists());
        std::fs::write(records.join(&name), b"changed after preview").unwrap();
        let error = copy
            .publish(vault.path(), &state.path().join("locks"), &records)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("Original recovery record changed"));
        assert!(!vault
            .path()
            .join(copy.attempt.as_ref().unwrap().path())
            .exists());
        assert_eq!(
            std::fs::read(records.join(&name)).unwrap(),
            b"changed after preview"
        );
        assert_eq!(copy.text, "retained");
        // Positive control: restoring the reviewed record lets this same copy
        // operation finish, proving the refusal was the source revision guard.
        std::fs::write(records.join(&name), &source.record_bytes).unwrap();
        let receipt = copy
            .publish(vault.path(), &state.path().join("locks"), &records)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(vault.path().join(&receipt.path)).unwrap(),
            "retained"
        );
        std::fs::write(records.join(name), b"later external change").unwrap();
        assert_eq!(
            copy.publish(vault.path(), &state.path().join("locks"), &records)
                .unwrap(),
            receipt
        );
        assert_eq!(std::fs::read_dir(vault.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn copy_writer_failure_restarts_with_same_copy_identity_and_provenance() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let locks = state.path().join("locks");
        prepare_records(&records).unwrap();
        let legacy = Capture::new(Path::new(""), "copy text".into()).unwrap();
        persist_new(
            &records,
            &format!("{}.intent.json", legacy.receipt.operation_id),
            &legacy,
        )
        .unwrap();
        let source = load_recovery(vault.path(), &records)
            .unwrap()
            .unbound
            .remove(0);
        let preview = source
            .preview(vault.path(), Path::new(""), &records)
            .unwrap();
        let copy = preview.start_copy().unwrap();
        assert!(
            crate::unix_publish::inject(rustix::io::Errno::ACCESS, || copy.publish(
                vault.path(),
                &locks,
                &records
            ))
            .is_err()
        );
        let recovered = load_recovery(vault.path(), &records).unwrap().drafts;
        assert_eq!(recovered, vec![copy.clone()]);
        let receipt = recovered[0]
            .publish(vault.path(), &locks, &records)
            .unwrap();
        assert_eq!(
            receipt.operation_id,
            copy.attempt.as_ref().unwrap().receipt.operation_id
        );
        assert_eq!(std::fs::read_dir(vault.path()).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(vault.path().join(receipt.path)).unwrap(),
            legacy.text()
        );
    }
    #[test]
    fn explicit_saved_copy_dismissal_is_durable_and_keeps_original_source() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        prepare_records(&records).unwrap();
        let legacy = Capture::new(Path::new(""), "retained legacy\r\n".into()).unwrap();
        let name = format!("{}.intent.json", legacy.receipt.operation_id);
        persist_new(&records, &name, &legacy).unwrap();
        let original = std::fs::read(records.join(&name)).unwrap();
        let source = load_recovery(vault.path(), &records)
            .unwrap()
            .unbound
            .remove(0);
        let copy = source
            .preview(vault.path(), Path::new(""), &records)
            .unwrap()
            .start_copy()
            .unwrap();
        assert!(dismiss_saved_copy(vault.path(), &records, &copy).is_err());
        assert_eq!(
            load_recovery(vault.path(), &records).unwrap().unbound.len(),
            1
        );
        let receipt = copy
            .publish(vault.path(), &state.path().join("drafts"), &records)
            .unwrap();
        assert_eq!(
            load_recovery(vault.path(), &records).unwrap().unbound.len(),
            1,
            "Save copy alone never dismisses its source"
        );
        dismiss_saved_copy(vault.path(), &records, &copy).unwrap();
        dismiss_saved_copy(vault.path(), &records, &copy).unwrap();
        assert!(load_recovery(vault.path(), &records)
            .unwrap()
            .unbound
            .is_empty());
        assert_eq!(std::fs::read(records.join(&name)).unwrap(), original);
        std::fs::write(vault.path().join(&receipt.path), "later user edit").unwrap();
        assert!(load_recovery(vault.path(), &records)
            .unwrap()
            .unbound
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(vault.path().join(receipt.path)).unwrap(),
            "later user edit"
        );
        let marker = records.join(superseded_name(&legacy.receipt.operation_id));
        let saved_marker = std::fs::read(&marker).unwrap();
        std::fs::write(&marker, b"corrupt marker").unwrap();
        assert!(load_recovery(vault.path(), &records).is_err());
        std::fs::write(marker, saved_marker).unwrap();
        assert!(load_recovery(vault.path(), &records)
            .unwrap()
            .unbound
            .is_empty());
        // Exact delivered legacy marker without a presentation head remains
        // inspectable; read-only inspection never invents/migrates a head.
        let head = records.join(format!(
            "{}.presentation.json",
            superseded_name(&legacy.receipt.operation_id)
        ));
        std::fs::remove_file(&head).unwrap();
        let marker_before_inspection =
            std::fs::read(records.join(superseded_name(&legacy.receipt.operation_id))).unwrap();
        let count = std::fs::read_dir(&records).unwrap().count();
        let inspected = inspect_dismissed(vault.path(), &records).unwrap();
        assert!(!head.exists());
        assert_eq!(std::fs::read_dir(&records).unwrap().count(), count);
        assert_eq!(
            std::fs::read(records.join(superseded_name(&legacy.receipt.operation_id))).unwrap(),
            marker_before_inspection
        );
        assert_eq!(inspected.len(), 1);
        assert_eq!(inspected[0].text, legacy.text());
        let stale = inspected[0].revision.clone();
        // A newer explicit dismissal, even for the same saved copy, changes the
        // presentation revision; the earlier preview has no undo authority.
        dismiss_saved_copy(vault.path(), &records, &copy).unwrap();
        assert!(reopen_dismissed(vault.path(), &records, &stale).is_err());
        assert!(load_recovery(vault.path(), &records)
            .unwrap()
            .unbound
            .is_empty());
        let current = inspect_dismissed(vault.path(), &records).unwrap().remove(0);
        let foreign = tempfile::tempdir().unwrap();
        assert!(reopen_dismissed(foreign.path(), &records, &current.revision).is_err());
        let marker_path = records.join(superseded_name(&legacy.receipt.operation_id));
        let saved_marker = std::fs::read(&marker_path).unwrap();
        let mut changed_marker = saved_marker.clone();
        changed_marker.push(b'\n');
        std::fs::write(&marker_path, changed_marker).unwrap();
        assert!(reopen_dismissed(vault.path(), &records, &current.revision).is_err());
        std::fs::write(&marker_path, saved_marker).unwrap();
        reopen_dismissed(vault.path(), &records, &current.revision).unwrap();
        assert_eq!(
            load_recovery(vault.path(), &records).unwrap().unbound.len(),
            1
        );
        assert!(inspect_dismissed(vault.path(), &records)
            .unwrap()
            .is_empty());
        assert!(reopen_dismissed(vault.path(), &records, &current.revision).is_err());
        assert_eq!(std::fs::read(records.join(&name)).unwrap(), original);
    }
    #[test]
    fn bound_source_dismissal_hides_both_composer_and_original_intent() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        let locks = state.path().join("drafts");
        let mut original = ComposerDraft {
            text: "original bound thought".into(),
            folder: Some(PathBuf::new()),
            ..Default::default()
        };
        original.prepare_save().unwrap();
        original.persist(vault.path(), &records).unwrap();
        let attempt = original.attempt.as_ref().unwrap();
        persist_new(
            &records,
            &format!("{}.intent.json", attempt.receipt.operation_id),
            &Record {
                root: vault.path().canonicalize().unwrap(),
                capture: attempt.clone(),
            },
        )
        .unwrap();
        std::fs::write(vault.path().join(attempt.path()), "external original").unwrap();
        let copy = CopySource::from_composer(vault.path(), &records, &original)
            .unwrap()
            .preview(vault.path(), Path::new(""), &records)
            .unwrap()
            .start_copy()
            .unwrap();
        copy.publish(vault.path(), &locks, &records).unwrap();
        assert_eq!(
            recover_composers(vault.path(), &records).unwrap(),
            vec![original.clone()]
        );
        dismiss_saved_copy(vault.path(), &records, &copy).unwrap();
        assert!(recover(vault.path(), &records).unwrap().is_empty());
        assert!(recover_composers(vault.path(), &records)
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(vault.path().join(attempt.path())).unwrap(),
            "external original"
        );
        let raw: ComposerRecord =
            read_optional(&records.join(format!("{}.composer.json", original.id)))
                .unwrap()
                .unwrap();
        assert_eq!(raw.draft, original);
    }
    fn history_fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
        ComposerDraft,
        String,
    ) {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        prepare_records(&records).unwrap();
        let original = Capture::new(Path::new(""), "history source\r\n".into()).unwrap();
        persist_new(
            &records,
            &format!("{}.intent.json", original.receipt.operation_id),
            &original,
        )
        .unwrap();
        let source = load_recovery(vault.path(), &records)
            .unwrap()
            .unbound
            .remove(0);
        let copy = source
            .preview(vault.path(), Path::new(""), &records)
            .unwrap()
            .start_copy()
            .unwrap();
        copy.publish(vault.path(), &state.path().join("drafts"), &records)
            .unwrap();
        dismiss_saved_copy(vault.path(), &records, &copy).unwrap();
        let marker = superseded_name(&original.receipt.operation_id);
        (vault, state, records, copy, marker)
    }

    #[test]
    fn presentation_history_projects_only_linked_decisions_and_invalidates_snapshots() {
        let (vault, _state, records, copy, marker) = history_fixture();
        let before = std::fs::read(records.join(&marker)).unwrap();
        let first = inspect_presentation_history(vault.path(), &records).unwrap();
        assert!(first.complete);
        assert_eq!(
            first.sources[0].current,
            PresentationHistoryStatus::Dismissed
        );
        assert_eq!(first.sources[0].events.len(), 1);
        assert_eq!(
            first.sources[0].events[0].relation,
            PresentationHistoryRelation::Current
        );
        assert!(presentation_history_is_current(
            vault.path(),
            &records,
            &first.sources[0].revision
        )
        .unwrap());
        let undo = inspect_dismissed(vault.path(), &records).unwrap().remove(0);
        reopen_dismissed(vault.path(), &records, &undo.revision).unwrap();
        assert!(!presentation_history_is_current(
            vault.path(),
            &records,
            &first.sources[0].revision
        )
        .unwrap());
        let second = inspect_presentation_history(vault.path(), &records).unwrap();
        assert!(second.complete);
        assert_eq!(
            second.sources[0].current,
            PresentationHistoryStatus::Reopened
        );
        assert_eq!(second.sources[0].events.len(), 2);
        dismiss_saved_copy(vault.path(), &records, &copy).unwrap();
        let third = inspect_presentation_history(vault.path(), &records).unwrap();
        assert!(third.complete);
        let rows = &third.sources[0].events;
        assert_eq!(
            rows.iter()
                .map(|row| row.status.clone())
                .collect::<Vec<_>>(),
            vec![
                PresentationHistoryStatus::Dismissed,
                PresentationHistoryStatus::Reopened,
                PresentationHistoryStatus::Dismissed
            ]
        );
        assert_eq!(
            rows[0].previous_revision.as_deref(),
            Some(rows[1].revision.as_str())
        );
        assert_eq!(
            rows[1].previous_revision.as_deref(),
            Some(rows[2].revision.as_str())
        );
        assert_eq!(
            rows[2].previous_revision.as_deref(),
            Some(third.sources[0].marker_revision.as_str())
        );
        let head = std::fs::read(records.join(format!("{marker}.presentation.json"))).unwrap();
        let count = std::fs::read_dir(&records).unwrap().count();
        inspect_presentation_history(vault.path(), &records).unwrap();
        assert_eq!(std::fs::read(records.join(&marker)).unwrap(), before);
        assert_eq!(
            std::fs::read(records.join(format!("{marker}.presentation.json"))).unwrap(),
            head
        );
        assert_eq!(std::fs::read_dir(&records).unwrap().count(), count);
    }

    #[test]
    fn presentation_history_holds_legacy_orphan_corrupt_and_partial_records_without_repair() {
        let (vault, _state, records, _copy, marker) = history_fixture();
        let head_path = records.join(format!("{marker}.presentation.json"));
        let mut head: PresentationState = read_optional(&head_path).unwrap().unwrap();
        let event_path = records.join(format!("{}.presentation-event.json", head.revision));
        // Model exact delivered older events without invented predecessor data.
        head.previous_revision = None;
        let legacy_bytes = serde_json::to_vec(&head).unwrap();
        std::fs::write(&head_path, &legacy_bytes).unwrap();
        std::fs::write(&event_path, &legacy_bytes).unwrap();
        let legacy = inspect_presentation_history(vault.path(), &records).unwrap();
        assert!(!legacy.complete);
        assert_eq!(
            legacy.sources[0].current,
            PresentationHistoryStatus::Dismissed
        );
        assert!(legacy.sources[0]
            .held
            .iter()
            .any(|issue| issue.contains("no predecessor")));
        assert!(presentation_history_is_current(
            vault.path(),
            &records,
            &legacy.sources[0].revision
        )
        .is_err());
        let orphan = PresentationState {
            revision: uuid::Uuid::new_v4().to_string(),
            previous_revision: Some(head.marker_revision.clone()),
            dismissed: false,
            ..head.clone()
        };
        let orphan_name = format!("{}.presentation-event.json", orphan.revision);
        persist_new(&records, &orphan_name, &orphan).unwrap();
        let partial = inspect_presentation_history(vault.path(), &records).unwrap();
        assert_eq!(
            partial.sources[0].current_revision.as_deref(),
            Some(head.revision.as_str())
        );
        assert!(partial.sources[0]
            .events
            .iter()
            .any(|row| row.revision == orphan.revision
                && row.relation == PresentationHistoryRelation::UnlinkedHeld));
        assert_eq!(std::fs::read(&head_path).unwrap(), legacy_bytes);
        std::fs::write(&head_path, b"broken head").unwrap();
        let corrupt = inspect_presentation_history(vault.path(), &records).unwrap();
        assert_eq!(corrupt.sources[0].current, PresentationHistoryStatus::Held);
        assert_eq!(std::fs::read(&head_path).unwrap(), b"broken head");
        std::fs::remove_file(&head_path).unwrap();
        let missing = inspect_presentation_history(vault.path(), &records).unwrap();
        assert_eq!(missing.sources[0].current, PresentationHistoryStatus::Held);
        assert!(!head_path.exists());
        std::fs::remove_file(&event_path).unwrap();
        std::fs::remove_file(records.join(&orphan_name)).unwrap();
        let baseline = inspect_presentation_history(vault.path(), &records).unwrap();
        assert!(baseline.complete);
        assert_eq!(
            baseline.sources[0].current,
            PresentationHistoryStatus::Dismissed
        );
        assert!(baseline.sources[0].events.is_empty());
        let broken_event = format!("{}.presentation-event.json", uuid::Uuid::new_v4());
        std::fs::write(records.join(&broken_event), b"broken event").unwrap();
        let corrupt_event = inspect_presentation_history(vault.path(), &records).unwrap();
        assert!(!corrupt_event.complete);
        assert_eq!(
            corrupt_event.sources[0].current,
            PresentationHistoryStatus::Held
        );
        assert!(corrupt_event
            .held
            .iter()
            .any(|issue| issue.contains("Corrupt history event")));
        std::fs::remove_file(records.join(broken_event)).unwrap();
        std::fs::write(&head_path, &legacy_bytes).unwrap();
        let missing_event = inspect_presentation_history(vault.path(), &records).unwrap();
        assert_eq!(
            missing_event.sources[0].current,
            PresentationHistoryStatus::Held
        );
    }

    #[test]
    fn presentation_history_bounds_are_explicit_held_results() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let records = state.path().join("records");
        prepare_records(&records).unwrap();
        for index in 0..=HISTORY_MAX_ENTRIES {
            std::fs::write(records.join(format!("ignored-{index}")), b"").unwrap();
        }
        let history = inspect_presentation_history(vault.path(), &records).unwrap();
        assert!(!history.complete);
        assert!(history
            .held
            .iter()
            .any(|issue| issue.contains("entry bound")));
        let bounded = state.path().join("bounded");
        prepare_records(&bounded).unwrap();
        let name = format!("{}.presentation-event.json", uuid::Uuid::new_v4());
        std::fs::write(bounded.join(&name), vec![b' '; HISTORY_RECORD_BYTES + 1]).unwrap();
        let history = inspect_presentation_history(vault.path(), &bounded).unwrap();
        assert!(!history.complete);
        assert!(history
            .held
            .iter()
            .any(|issue| issue.contains("exceeds bounds")));
        assert_eq!(
            std::fs::metadata(bounded.join(name)).unwrap().len(),
            (HISTORY_RECORD_BYTES + 1) as u64
        );
    }
}
