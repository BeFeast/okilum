//! Attachment transactions over the existing capture and shared create-only writer.
//! The mobile staging owner maps its frozen VerifiedAsset fields to these values.
//! This module never opens provider paths, deletes staging, or widens a protocol.
use super::{
    inspect_destination_bounded, persist_equal, prepare_records, read_destination_bounded, Capture,
    ComposerDraft, Receipt,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const MAX_ASSETS: usize = 8;
pub const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CAPTURE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_NOTE_BYTES: usize = 1024 * 1024;
const MAX_PLAN_BYTES: usize = 3 * MAX_NOTE_BYTES;
const MAX_RECEIPT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Photo,
    SharedFile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub id: String,
    pub display_name: String,
    pub kind: AssetKind,
    pub byte_length: usize,
    pub sha256: String,
}

pub struct VerifiedAsset {
    pub asset: Asset,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedAsset {
    pub asset: Asset,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentReceipt {
    pub composer_id: String,
    pub note: Receipt,
    pub assets: Vec<PublishedAsset>,
}

/// No cleanup authority is produced by a staged manifest or note-only receipt.
/// Only `verify_cleanup_receipt` constructs this process-local proof.
#[derive(Debug)]
pub struct CleanupPermit {
    composer_id: String,
    operation_id: String,
    assets: Vec<Asset>,
}
impl CleanupPermit {
    pub fn composer_id(&self) -> &str {
        &self.composer_id
    }
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn assets(&self) -> &[Asset] {
        &self.assets
    }
}

#[derive(Debug)]
pub struct IncompletePublication {
    pub operation_id: String,
    pub completed_assets: usize,
    pub total_assets: usize,
    pub note_acknowledged: bool,
    pub reason: String,
}
impl std::fmt::Display for IncompletePublication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Capture {} is incomplete: {}/{} asset receipts confirmed durable; note receipt confirmed: {}. Canonical files may exist. Keep staged bytes and draft; retry the same operation. {}", self.operation_id, self.completed_assets, self.total_assets, self.note_acknowledged, self.reason)
    }
}
impl std::error::Error for IncompletePublication {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    version: u32,
    root: PathBuf,
    composer_id: String,
    original_text: String,
    capture_folder: PathBuf,
    attachment_folder: PathBuf,
    assets: Vec<PublishedAsset>,
    note: Capture,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copy_origin: Option<AttachmentCopyOrigin>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Owner {
    root: PathBuf,
    composer_id: String,
    operation_id: String,
}

fn id(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.to_string() == value,
        "Use an exact canonical operation/asset UUID"
    );
    Ok(())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn folder(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
        && !crate::vault::service_path(path)
}
fn validate_assets(assets: &[Asset]) -> Result<()> {
    ensure!(
        !assets.is_empty() && assets.len() <= MAX_ASSETS,
        "Capture requires 1..=8 assets"
    );
    let mut seen = std::collections::HashSet::new();
    let mut total = 0usize;
    for asset in assets {
        id(&asset.id)?;
        ensure!(seen.insert(&asset.id), "Duplicate attachment identity");
        ensure!(
            asset.byte_length > 0 && asset.byte_length <= MAX_ASSET_BYTES,
            "Attachment byte limit exceeded"
        );
        total = total
            .checked_add(asset.byte_length)
            .context("Attachment length overflow")?;
        ensure!(
            total <= MAX_CAPTURE_BYTES,
            "Capture attachment byte limit exceeded"
        );
        ensure!(
            !asset.display_name.is_empty()
                && asset.display_name.len() <= 255
                && !asset.display_name.chars().any(char::is_control),
            "Invalid attachment display name"
        );
        ensure!(
            asset.sha256.len() == 64
                && asset
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Invalid exact attachment revision"
        );
    }
    Ok(())
}
fn read_bytes(path: &Path, max: usize) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    let opened = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(std::io::Error::from);
    #[cfg(not(unix))]
    let opened = std::fs::File::open(path);
    let file = match opened {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        file.metadata()?.is_file(),
        "Attachment operational record is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take((max + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= max,
        "Attachment operational record exceeds bounds"
    );
    Ok(Some(bytes))
}
fn read<T: serde::de::DeserializeOwned>(path: &Path, max: usize) -> Result<Option<T>> {
    read_bytes(path, max)?
        .map(|bytes| {
            serde_json::from_slice(&bytes)
                .with_context(|| format!("Invalid attachment record: {}", path.display()))
        })
        .transpose()
}

fn plan_name(composer: &str) -> String {
    format!("{composer}.attachments.json")
}
fn receipt_name(composer: &str) -> String {
    format!("{composer}.attachments.receipt.json")
}
fn asset_receipt_name(operation: &str, asset: &str) -> String {
    format!("{operation}-{asset}.asset.receipt.json")
}

fn extension(asset: &VerifiedAsset) -> Result<&'static str> {
    if asset.asset.kind == AssetKind::SharedFile {
        return Ok("bin");
    }
    let b = &asset.bytes;
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok("jpg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Ok("gif")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Ok("webp")
    } else if b.len() >= 12
        && &b[4..8] == b"ftyp"
        && matches!(&b[8..12], b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1")
    {
        Ok("heic")
    } else if b.len() >= 12 && &b[4..8] == b"ftyp" && matches!(&b[8..12], b"avif" | b"avis") {
        Ok("avif")
    } else {
        anyhow::bail!("Unsupported photo bytes; retain staging and choose a supported photo")
    }
}
fn asset_path(directory: &Path, operation: &str, asset: &str, extension: &str) -> PathBuf {
    directory.join(format!("Capture-{operation}-{asset}.{extension}"))
}
fn url(from: &Path, target: &Path) -> Result<String> {
    let from: Vec<_> = from.components().collect();
    let target: Vec<_> = target.components().collect();
    let common = from.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut result = if common == from.len() {
        "./".to_owned()
    } else {
        "../".repeat(from.len() - common)
    };
    for (i, component) in target[common..].iter().enumerate() {
        if i > 0 {
            result.push('/');
        }
        for byte in component
            .as_os_str()
            .to_str()
            .context("Use UTF-8 capture folders")?
            .bytes()
        {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                result.push(byte as char);
            } else {
                result.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    Ok(result)
}
fn note_text(original: &str, capture_folder: &Path, assets: &[PublishedAsset]) -> Result<String> {
    ensure!(
        original.len() <= MAX_NOTE_BYTES,
        "Capture text exceeds bounds"
    );
    let mut result = original.to_owned();
    if !result.is_empty() {
        result.push_str("\n\n");
    }
    for asset in assets {
        let label: String = asset
            .asset
            .display_name
            .chars()
            .flat_map(|c| {
                if c.is_ascii_punctuation() {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect();
        if asset.asset.kind == AssetKind::Photo {
            result.push('!');
        }
        result.push_str(&format!(
            "[{label}]({})\n",
            url(capture_folder, &asset.path)?
        ));
    }
    ensure!(
        result.len() <= MAX_NOTE_BYTES,
        "Capture note and links exceed bounds"
    );
    Ok(result)
}
impl Plan {
    fn validate(&self, root: &Path, draft: Option<&ComposerDraft>) -> Result<()> {
        ensure!(
            self.version == 1 && self.root == root.canonicalize()?,
            "Attachment plan vault/version mismatch"
        );
        id(&self.composer_id)?;
        self.note.validate()?;
        ensure!(
            folder(&self.capture_folder) && folder(&self.attachment_folder),
            "Invalid attachment destination folder"
        );
        ensure!(
            self.note.path()
                == self
                    .capture_folder
                    .join(format!("Capture-{}.md", self.note.receipt.operation_id)),
            "Attachment note destination changed"
        );
        validate_assets(
            &self
                .assets
                .iter()
                .map(|a| a.asset.clone())
                .collect::<Vec<_>>(),
        )?;
        for asset in &self.assets {
            let ext = asset
                .path
                .extension()
                .and_then(|s| s.to_str())
                .context("Missing generated attachment extension")?;
            ensure!(
                matches!(
                    ext,
                    "bin" | "png" | "jpg" | "gif" | "webp" | "heic" | "avif"
                ) && asset.path
                    == asset_path(
                        &self.attachment_folder,
                        &self.note.receipt.operation_id,
                        &asset.asset.id,
                        ext
                    ),
                "Attachment generated destination changed"
            );
        }
        ensure!(
            self.note.text() == note_text(&self.original_text, &self.capture_folder, &self.assets)?,
            "Attachment note composition changed"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_PLAN_BYTES,
            "Attachment plan representation exceeds recovery bounds"
        );
        if let Some(origin) = &self.copy_origin {
            id(&origin.source_composer_id)?;
            id(&origin.source_operation_id)?;
            ensure!(
                valid_revision(&origin.source_revision)
                    && origin.source_composer_id != self.composer_id
                    && origin.source_operation_id != self.note.receipt.operation_id,
                "Invalid attachment copy provenance"
            );
        }
        if let Some(draft) = draft {
            ensure!(
                draft.id == self.composer_id
                    && draft.text == self.original_text
                    && draft.folder.as_ref() == Some(&self.capture_folder)
                    && draft.attempt.is_none()
                    && draft.copy_origin.is_none(),
                "Attachment draft identity/text/destination changed"
            );
            if let Some(operation) = &draft.attachment_attempt {
                ensure!(
                    operation == &self.note.receipt.operation_id,
                    "Attachment attempt changed"
                );
            }
        }
        Ok(())
    }
    fn receipt(&self) -> AttachmentReceipt {
        AttachmentReceipt {
            composer_id: self.composer_id.clone(),
            note: self.note.receipt.clone(),
            assets: self.assets.clone(),
        }
    }
}

pub struct Publisher<'a> {
    root: &'a Path,
    drafts: &'a Path,
    records: &'a Path,
    attachment_folder: &'a Path,
    draft: &'a mut ComposerDraft,
}
impl<'a> Publisher<'a> {
    pub fn new(
        root: &'a Path,
        drafts: &'a Path,
        records: &'a Path,
        attachment_folder: &'a Path,
        draft: &'a mut ComposerDraft,
    ) -> Self {
        Self {
            root,
            drafts,
            records,
            attachment_folder,
            draft,
        }
    }
    pub fn publish_capture(
        &mut self,
        composer_id: &str,
        assets: &[VerifiedAsset],
    ) -> Result<AttachmentReceipt> {
        id(composer_id)?;
        ensure!(self.draft.id == composer_id, "Attachment composer changed");
        ensure!(
            !assets.is_empty() && assets.len() <= MAX_ASSETS,
            "Capture requires 1..=8 assets"
        );
        for input in assets {
            validate_assets(std::slice::from_ref(&input.asset))?;
        }
        let identities: Vec<_> = assets.iter().map(|a| a.asset.clone()).collect();
        validate_assets(&identities)?;
        for asset in assets {
            ensure!(
                asset.bytes.len() == asset.asset.byte_length
                    && hash(&asset.bytes) == asset.asset.sha256,
                "Verified attachment bytes/identity changed"
            );
        }
        prepare_records(self.records)?;
        let name = plan_name(composer_id);
        let _lock = crate::file_editor::FileEditor::reserve_destination(
            &self.records.join(&name),
            &self.records.join("attachment-locks"),
        )?;
        let prior: Option<Plan> = read(&self.records.join(&name), MAX_PLAN_BYTES)?;
        let plan = if let Some(plan) = prior {
            plan.validate(self.root, Some(self.draft))?;
            ensure!(
                plan.attachment_folder == self.attachment_folder
                    && plan.assets.iter().map(|a| &a.asset).eq(identities.iter()),
                "Frozen attachment set/order/destination changed"
            );
            for (published, input) in plan.assets.iter().zip(assets) {
                ensure!(
                    published.path
                        == asset_path(
                            &plan.attachment_folder,
                            &plan.note.receipt.operation_id,
                            &input.asset.id,
                            extension(input)?
                        ),
                    "Frozen attachment filename/type changed"
                );
            }
            plan
        } else {
            ensure!(
                self.draft.attachment_attempt.is_none()
                    && self.draft.attempt.is_none()
                    && self.draft.copy_origin.is_none(),
                "An existing capture attempt cannot be replaced with attachments"
            );
            let capture_folder = self
                .draft
                .folder
                .as_deref()
                .context("Choose a capture folder")?;
            super::validate_copy_folder(self.root, capture_folder)?;
            super::validate_copy_folder(self.root, self.attachment_folder)?;
            let operation = uuid::Uuid::new_v4().to_string();
            let published: Vec<_> = assets
                .iter()
                .map(|asset| {
                    Ok(PublishedAsset {
                        asset: asset.asset.clone(),
                        path: asset_path(
                            self.attachment_folder,
                            &operation,
                            &asset.asset.id,
                            extension(asset)?,
                        ),
                    })
                })
                .collect::<Result<_>>()?;
            let note = Capture {
                receipt: Receipt {
                    operation_id: operation.clone(),
                    path: capture_folder.join(format!("Capture-{operation}.md")),
                },
                text: note_text(&self.draft.text, capture_folder, &published)?,
            };
            let plan = Plan {
                version: 1,
                root: self.root.canonicalize()?,
                composer_id: composer_id.into(),
                original_text: self.draft.text.clone(),
                capture_folder: capture_folder.into(),
                attachment_folder: self.attachment_folder.into(),
                assets: published,
                note,
                copy_origin: None,
            };
            plan.validate(self.root, Some(self.draft))?;
            for asset in &plan.assets {
                ensure!(
                    read_destination_bounded(self.root, &asset.path, MAX_ASSET_BYTES)?.is_none(),
                    "Attachment destination already exists"
                );
            }
            ensure!(
                read_destination_bounded(self.root, plan.note.path(), MAX_NOTE_BYTES)?.is_none(),
                "Capture note destination already exists"
            );
            self.draft.persist(self.root, self.records)?;
            persist_equal(self.records, &name, &plan)?;
            plan
        };
        let expected = plan.receipt();
        if let Some(receipt) = read::<AttachmentReceipt>(
            &self.records.join(receipt_name(composer_id)),
            MAX_RECEIPT_BYTES,
        )? {
            ensure!(receipt == expected, "Attachment completion receipt changed");
            #[cfg(unix)]
            std::fs::File::open(self.records)?.sync_all()?;
            self.draft.attachment_attempt = Some(plan.note.receipt.operation_id.clone());
            return Ok(receipt);
        }
        self.draft.attachment_attempt = Some(plan.note.receipt.operation_id.clone());
        let mut completed = 0usize;
        let mut note_acknowledged = false;
        let result = (|| -> Result<AttachmentReceipt> {
            verify_copy_origin(self.root, self.records, &plan)?;
            // Freeze draft/operation before the first canonical byte is published.
            self.draft.persist(self.root, self.records)?;
            persist_equal(
                self.records,
                &format!("{}.attachment-owner.json", plan.note.receipt.operation_id),
                &Owner {
                    root: plan.root.clone(),
                    composer_id: composer_id.into(),
                    operation_id: plan.note.receipt.operation_id.clone(),
                },
            )?;
            // Lost aggregate acknowledgement after all canonical publication:
            // durable subordinate receipts reconstruct completion without ever
            // reopening or rewriting subsequently edited/deleted canonical files.
            if let Some(note) = read::<Receipt>(
                &self
                    .records
                    .join(format!("{}.receipt.json", plan.note.receipt.operation_id)),
                MAX_RECEIPT_BYTES,
            )? {
                ensure!(
                    note == expected.note,
                    "Completed attachment note receipt changed"
                );
                note_acknowledged = true;
                for target in &plan.assets {
                    let asset: PublishedAsset = read(
                        &self.records.join(asset_receipt_name(
                            &plan.note.receipt.operation_id,
                            &target.asset.id,
                        )),
                        MAX_RECEIPT_BYTES,
                    )?
                    .context("Completed note has a missing asset receipt; retain staging")?;
                    ensure!(
                        &asset == target,
                        "Completed note has a changed asset receipt; retain staging"
                    );
                    completed += 1;
                }
                persist_equal(self.records, &receipt_name(composer_id), &expected)?;
                return Ok(expected);
            }
            for (target, input) in plan.assets.iter().zip(assets) {
                let _destination = crate::file_editor::FileEditor::reserve_destination(
                    &self.root.join(&target.path),
                    self.drafts,
                )?;
                let name = asset_receipt_name(&plan.note.receipt.operation_id, &target.asset.id);
                let prior: Option<PublishedAsset> =
                    read(&self.records.join(&name), MAX_RECEIPT_BYTES)?;
                if let Some(prior) = &prior {
                    ensure!(prior == target, "Attachment receipt identity changed");
                }
                match read_destination_bounded(self.root, &target.path, MAX_ASSET_BYTES)? {
                    Some(bytes) => ensure!(
                        bytes == input.bytes,
                        "Partial capture attachment changed; staged bytes and draft retained"
                    ),
                    None if prior.is_some() => anyhow::bail!(
                        "An acknowledged attachment disappeared; it will not be silently recreated"
                    ),
                    None => crate::note_files::create_attachment_with_source(
                        self.root,
                        &target.path,
                        &input.bytes,
                    )?,
                }
                persist_equal(self.records, &name, target)?;
                completed += 1;
            }
            let note = plan.note.publish(self.root, self.drafts, self.records)?;
            ensure!(
                note == expected.note,
                "Capture note receipt identity changed"
            );
            note_acknowledged = true;
            persist_equal(self.records, &receipt_name(composer_id), &expected)?;
            Ok(expected)
        })();
        result.map_err(|error| {
            IncompletePublication {
                operation_id: plan.note.receipt.operation_id,
                completed_assets: completed,
                total_assets: assets.len(),
                note_acknowledged,
                reason: format!("{error:#}"),
            }
            .into()
        })
    }
}

/// Recover an exact durable attachment receipt by composer identity after restart.
///
/// `None` means no durable aggregate completion exists yet, NOT an empty capture:
/// retain the original draft/staging and resume the existing publisher explicitly.
/// An invalid ledger, vault mismatch, missing/changed/unreadable canonical file or
/// exceeded read bound returns an error; keep that capture held for recovery.
/// `Some` validates the plan, operation owner, aggregate and subordinate receipts,
/// and the current exact note/asset bytes observed during this read. It is not a
/// lease on those files or a CleanupPermit. This function creates no files, does
/// not reconstruct/persist missing completion, and never publishes or performs GC.
pub fn recover_receipt(
    root: &Path,
    records: &Path,
    composer_id: &str,
) -> Result<Option<AttachmentReceipt>> {
    id(composer_id)?;
    let plan: Option<Plan> = read(&records.join(plan_name(composer_id)), MAX_PLAN_BYTES)?;
    let receipt: Option<AttachmentReceipt> =
        read(&records.join(receipt_name(composer_id)), MAX_RECEIPT_BYTES)?;
    let Some(plan) = plan else {
        ensure!(
            receipt.is_none(),
            "Attachment receipt held: durable plan is missing; retain staging"
        );
        return Ok(None);
    };
    plan.validate(root, None)?;
    ensure!(
        plan.composer_id == composer_id,
        "Attachment receipt held: composer identity differs from its plan"
    );
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    ensure!(
        receipt == plan.receipt(),
        "Attachment receipt held: aggregate identity differs from its plan"
    );
    let owner: Owner = read(
        &records.join(format!(
            "{}.attachment-owner.json",
            receipt.note.operation_id
        )),
        MAX_RECEIPT_BYTES,
    )?
    .context("Attachment receipt held: operation owner is missing")?;
    ensure!(
        owner.root == plan.root
            && owner.composer_id == composer_id
            && owner.operation_id == receipt.note.operation_id,
        "Attachment receipt held: operation owner differs"
    );
    for asset in &receipt.assets {
        let durable: PublishedAsset = read(
            &records.join(asset_receipt_name(
                &receipt.note.operation_id,
                &asset.asset.id,
            )),
            MAX_RECEIPT_BYTES,
        )?
        .context("Attachment receipt held: asset receipt is missing")?;
        ensure!(
            &durable == asset,
            "Attachment receipt held: asset receipt identity differs"
        );
        let bytes = inspect_destination_bounded(root, &asset.path, MAX_ASSET_BYTES)
            .with_context(|| {
                format!(
                    "Attachment receipt held: cannot inspect {}",
                    asset.path.display()
                )
            })?
            .context("Attachment receipt held: canonical attachment is missing; retain staging")?;
        ensure!(
            bytes.len() == asset.asset.byte_length && hash(&bytes) == asset.asset.sha256,
            "Attachment receipt held: canonical attachment changed; retain staging"
        );
    }
    let note: Receipt = read(
        &records.join(format!("{}.receipt.json", receipt.note.operation_id)),
        MAX_RECEIPT_BYTES,
    )?
    .context("Attachment receipt held: note receipt is missing")?;
    ensure!(
        note == receipt.note,
        "Attachment receipt held: note receipt identity differs"
    );
    let bytes = inspect_destination_bounded(root, &receipt.note.path, MAX_NOTE_BYTES)
        .context("Attachment receipt held: cannot inspect canonical note")?
        .context("Attachment receipt held: canonical note is missing; retain staging")?;
    ensure!(
        bytes == plan.note.text().as_bytes(),
        "Attachment receipt held: canonical note changed; retain staging"
    );
    Ok(Some(receipt))
}

pub fn verify_cleanup_receipt(
    root: &Path,
    records: &Path,
    composer_id: &str,
    assets: &[Asset],
    receipt: &AttachmentReceipt,
) -> Result<CleanupPermit> {
    id(composer_id)?;
    validate_assets(assets)?;
    let plan: Plan = read(&records.join(plan_name(composer_id)), MAX_PLAN_BYTES)?
        .context("No durable attachment plan; cleanup forbidden")?;
    plan.validate(root, None)?;
    ensure!(plan.copy_origin.is_none(), "A Save copy receipt does not authorize source staging cleanup; original intent and staged bytes are retained");
    ensure!(
        receipt == &plan.receipt()
            && receipt.composer_id == composer_id
            && receipt.assets.iter().map(|a| &a.asset).eq(assets.iter()),
        "Receipt does not authorize this exact staged capture"
    );
    let durable: AttachmentReceipt =
        read(&records.join(receipt_name(composer_id)), MAX_RECEIPT_BYTES)?
            .context("Capture completion is not durable; keep staging")?;
    ensure!(
        &durable == receipt,
        "Durable attachment receipt differs; keep staging"
    );
    for asset in &receipt.assets {
        let durable: PublishedAsset = read(
            &records.join(asset_receipt_name(
                &receipt.note.operation_id,
                &asset.asset.id,
            )),
            MAX_RECEIPT_BYTES,
        )?
        .context("Missing asset receipt; keep staging")?;
        ensure!(&durable == asset, "Asset receipt differs; keep staging");
    }
    let note: Receipt = read(
        &records.join(format!("{}.receipt.json", receipt.note.operation_id)),
        MAX_RECEIPT_BYTES,
    )?
    .context("Missing note receipt; keep staging")?;
    ensure!(note == receipt.note, "Note receipt differs; keep staging");
    // A visible completion file after a failed directory flush is not yet
    // cleanup authority. Finish that durability boundary before issuing proof.
    #[cfg(unix)]
    std::fs::File::open(records)?.sync_all()?;
    Ok(CleanupPermit {
        composer_id: composer_id.into(),
        operation_id: receipt.note.operation_id.clone(),
        assets: assets.to_vec(),
    })
}

pub(super) fn reconcile_recovered(
    root: &Path,
    records: &Path,
    draft: &mut ComposerDraft,
) -> Result<bool> {
    let Some(plan) = read::<Plan>(&records.join(plan_name(&draft.id)), MAX_PLAN_BYTES)? else {
        ensure!(
            draft.attachment_attempt.is_none(),
            "Pending attachment plan is missing; keep draft and staging"
        );
        return Ok(false);
    };
    plan.validate(root, Some(draft))?;
    if plan.copy_origin.is_some() {
        // Copy authorizations are recovered through recover_copies using the
        // ORIGINAL staging composer, never a guessed new staging identity.
        let source = &plan.copy_origin.as_ref().unwrap().source_composer_id;
        ensure!(
            recover_copies(root, records, source)?
                .iter()
                .any(|copy| copy.plan == plan),
            "Attachment copy authorization is missing; retain draft and staging"
        );
        return Ok(true);
    }
    if source_superseded(root, records, &plan)? {
        return Ok(true);
    }
    draft.attachment_attempt = Some(plan.note.receipt.operation_id.clone());
    if let Some(receipt) =
        read::<AttachmentReceipt>(&records.join(receipt_name(&draft.id)), MAX_RECEIPT_BYTES)?
    {
        ensure!(
            receipt == plan.receipt(),
            "Recovered attachment receipt differs"
        );
        return Ok(true);
    }
    Ok(false)
}

pub(super) fn owns_operation(root: &Path, records: &Path, operation: &str) -> Result<bool> {
    let Some(owner) = read::<Owner>(
        &records.join(format!("{operation}.attachment-owner.json")),
        MAX_RECEIPT_BYTES,
    )?
    else {
        return Ok(false);
    };
    id(&owner.composer_id)?;
    ensure!(
        owner.root == root.canonicalize()? && owner.operation_id == operation,
        "Attachment operation owner changed"
    );
    let plan: Plan = read(&records.join(plan_name(&owner.composer_id)), MAX_PLAN_BYTES)?
        .context("Attachment owner has no plan")?;
    plan.validate(root, None)?;
    ensure!(
        plan.composer_id == owner.composer_id && plan.note.receipt.operation_id == operation,
        "Attachment operation plan changed"
    );
    Ok(true)
}

const MAX_COPY_RECORD_BYTES: usize = MAX_PLAN_BYTES + MAX_RECEIPT_BYTES;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentCopyOrigin {
    source_composer_id: String,
    source_operation_id: String,
    source_revision: String,
}

/// An explicit preview distinguishes absent files from an inspection failure.
/// The observation is bounded; it is never used as authority to replace a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentObservation {
    Missing,
    Present { byte_length: usize, sha256: String },
    Unavailable { reason: String },
}
fn observe(root: &Path, relative: &Path, max: usize) -> AttachmentObservation {
    match read_destination_bounded(root, relative, max) {
        Ok(Some(bytes)) => AttachmentObservation::Present {
            byte_length: bytes.len(),
            sha256: hash(&bytes),
        },
        Ok(None) => AttachmentObservation::Missing,
        Err(error) => AttachmentObservation::Unavailable {
            reason: format!("{error:#}"),
        },
    }
}
fn valid_revision(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentCopyReview {
    version: u32,
    root: PathBuf,
    origin: AttachmentCopyOrigin,
    capture_folder: PathBuf,
    attachment_folder: PathBuf,
    note: AttachmentObservation,
    assets: Vec<AttachmentObservation>,
}
impl AttachmentCopyReview {
    fn fingerprint(&self) -> Result<String> {
        Ok(hash(&serde_json::to_vec(self)?))
    }
}

/// Read-only review of exact frozen text/manifest, observed current files and
/// explicitly chosen destination folders. No UUID is allocated by preview.
#[derive(Debug)]
pub struct AttachmentCopyPreview {
    source: Plan,
    review: AttachmentCopyReview,
    existing: Option<AttachmentCopy>,
}
impl AttachmentCopyPreview {
    pub fn root(&self) -> &Path {
        &self.review.root
    }
    pub fn capture_folder(&self) -> &Path {
        &self.review.capture_folder
    }
    pub fn attachment_folder(&self) -> &Path {
        &self.review.attachment_folder
    }
    pub fn source_composer_id(&self) -> &str {
        &self.source.composer_id
    }
    pub fn source_operation_id(&self) -> &str {
        &self.source.note.receipt.operation_id
    }
    pub fn source_note_path(&self) -> &Path {
        self.source.note.path()
    }
    pub fn text(&self) -> &str {
        &self.source.original_text
    }
    pub fn assets(&self) -> &[PublishedAsset] {
        &self.source.assets
    }
    pub fn note_observation(&self) -> &AttachmentObservation {
        &self.review.note
    }
    /// Same order as assets(), including every frozen asset ID and revision.
    pub fn asset_observations(&self) -> &[AttachmentObservation] {
        &self.review.assets
    }
    /// If this exact review was already authorized, show Resume/Open copy rather
    /// than suggesting that retry will allocate a fresh operation.
    pub fn existing_copy(&self) -> Option<&AttachmentCopy> {
        self.existing.as_ref()
    }

    /// Explicit Save copy only. A durable authorization is written FIRST and
    /// contains the full new plan, so even a failed checkpoint can be recovered.
    /// Repeating this same reviewed action resumes exactly the same identity.
    pub fn start_copy(&self, root: &Path, records: &Path) -> Result<AttachmentCopy> {
        ensure!(
            root.canonicalize()? == self.review.root,
            "Selected copy vault changed; preview again"
        );
        prepare_records(records)?;
        let lock_path = records.join(format!("{}.attachment-copy", self.source.composer_id));
        let _lock = crate::file_editor::FileEditor::reserve_destination(
            &lock_path,
            &records.join("attachment-locks"),
        )?;
        let directory = copy_directory(records, &self.source.composer_id);
        let name = format!("{}.json", self.review.fingerprint()?);
        if let Some(copy) = read::<AttachmentCopy>(&directory.join(&name), MAX_COPY_RECORD_BYTES)? {
            copy.validate(root)?;
            ensure!(
                copy.review == self.review,
                "Copy authorization differs from this review"
            );
            persist_equal(&directory, &name, &copy)?;
            copy.install(root, records)?;
            return Ok(copy);
        }
        let (source, revision) = source_plan(root, records, &self.source.composer_id)?;
        ensure!(
            source == self.source && revision == self.review.origin.source_revision,
            "Frozen attachment intent changed; preview again before Save copy"
        );
        super::validate_copy_folder(root, &self.review.capture_folder)?;
        super::validate_copy_folder(root, &self.review.attachment_folder)?;
        ensure!(
            self.review.note == observe(root, source.note.path(), MAX_NOTE_BYTES)
                && self.review.assets
                    == source
                        .assets
                        .iter()
                        .map(|a| observe(root, &a.path, MAX_ASSET_BYTES))
                        .collect::<Vec<_>>(),
            "Original capture files changed after preview; review the current conflict again"
        );
        let operation = uuid::Uuid::new_v4().to_string();
        let composer = uuid::Uuid::new_v4().to_string();
        let assets = source
            .assets
            .iter()
            .map(|asset| {
                Ok(PublishedAsset {
                    asset: asset.asset.clone(),
                    path: asset_path(
                        &self.review.attachment_folder,
                        &operation,
                        &asset.asset.id,
                        asset
                            .path
                            .extension()
                            .and_then(|e| e.to_str())
                            .context("Missing frozen attachment type")?,
                    ),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let plan = Plan {
            version: 1,
            root: self.review.root.clone(),
            composer_id: composer,
            original_text: source.original_text.clone(),
            capture_folder: self.review.capture_folder.clone(),
            attachment_folder: self.review.attachment_folder.clone(),
            note: Capture {
                receipt: Receipt {
                    operation_id: operation.clone(),
                    path: self
                        .review
                        .capture_folder
                        .join(format!("Capture-{operation}.md")),
                },
                text: note_text(&source.original_text, &self.review.capture_folder, &assets)?,
            },
            assets,
            copy_origin: Some(self.review.origin.clone()),
        };
        let copy = AttachmentCopy {
            review: self.review.clone(),
            plan,
        };
        copy.validate(root)?;
        prepare_copy_directory(records, &self.source.composer_id)?;
        // Immutable action-keyed authorization is the recovery authority. No
        // original composer, intent, staging blob or canonical file is modified.
        persist_equal(&directory, &name, &copy)?;
        copy.install(root, records).context("Copy authorization is retained; recover this copy instead of starting another operation")?;
        Ok(copy)
    }
}

/// A durable authorized copy. Asset IDs/kinds/names/lengths/hashes retain their
/// ORIGINAL staging identities; only copy composer/note operation and generated
/// canonical paths are new. Its receipt grants no source-staging cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentCopy {
    review: AttachmentCopyReview,
    plan: Plan,
}
impl AttachmentCopy {
    pub fn composer_id(&self) -> &str {
        &self.plan.composer_id
    }
    pub fn operation_id(&self) -> &str {
        &self.plan.note.receipt.operation_id
    }
    pub fn source_composer_id(&self) -> &str {
        &self.review.origin.source_composer_id
    }
    pub fn source_operation_id(&self) -> &str {
        &self.review.origin.source_operation_id
    }
    pub fn root(&self) -> &Path {
        &self.plan.root
    }
    pub fn capture_folder(&self) -> &Path {
        &self.plan.capture_folder
    }
    pub fn attachment_folder(&self) -> &Path {
        &self.plan.attachment_folder
    }
    pub fn text(&self) -> &str {
        &self.plan.original_text
    }
    pub fn assets(&self) -> &[PublishedAsset] {
        &self.plan.assets
    }

    fn validate(&self, root: &Path) -> Result<()> {
        self.plan.validate(root, None)?;
        ensure!(
            self.review.version == 1
                && self.review.root == self.plan.root
                && self.plan.copy_origin.as_ref() == Some(&self.review.origin)
                && self.review.capture_folder == self.plan.capture_folder
                && self.review.attachment_folder == self.plan.attachment_folder
                && self.review.assets.len() == self.plan.assets.len(),
            "Attachment copy review/plan mismatch"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_COPY_RECORD_BYTES,
            "Attachment copy authorization exceeds recovery bounds"
        );
        Ok(())
    }
    fn draft(&self) -> ComposerDraft {
        ComposerDraft {
            id: self.plan.composer_id.clone(),
            text: self.plan.original_text.clone(),
            folder: Some(self.plan.capture_folder.clone()),
            attachment_attempt: Some(self.plan.note.receipt.operation_id.clone()),
            attempt: None,
            copy_origin: None,
        }
    }

    fn authorization_path(&self, records: &Path) -> Result<PathBuf> {
        Ok(copy_directory(records, self.source_composer_id())
            .join(format!("{}.json", self.review.fingerprint()?)))
    }
    fn install(&self, root: &Path, records: &Path) -> Result<()> {
        self.validate(root)?;
        let authorized: AttachmentCopy =
            read(&self.authorization_path(records)?, MAX_COPY_RECORD_BYTES)?
                .context("Copy authorization is missing; publication refused")?;
        ensure!(
            authorized == *self,
            "Copy authorization changed; publication refused"
        );
        persist_equal(records, &plan_name(self.composer_id()), &self.plan)?;
        let draft = self.draft();
        if let Some(record) = read::<super::ComposerRecord>(
            &records.join(format!("{}.composer.json", self.composer_id())),
            MAX_PLAN_BYTES,
        )? {
            ensure!(
                record.root == self.plan.root && record.draft == draft,
                "Authorized copy draft changed; keep original staging"
            );
            return Ok(());
        }
        draft.persist(root, records)
    }
    pub fn receipt(&self, root: &Path, records: &Path) -> Result<Option<AttachmentReceipt>> {
        self.validate(root)?;
        let authorized: AttachmentCopy =
            read(&self.authorization_path(records)?, MAX_COPY_RECORD_BYTES)?
                .context("Copy authorization is missing; receipt lookup refused")?;
        ensure!(
            authorized == *self,
            "Copy authorization changed; receipt lookup refused"
        );
        let receipt: Option<AttachmentReceipt> = read(
            &records.join(receipt_name(self.composer_id())),
            MAX_RECEIPT_BYTES,
        )?;
        if let Some(receipt) = &receipt {
            ensure!(
                receipt == &self.plan.receipt(),
                "Attachment copy receipt changed"
            );
        }
        Ok(receipt)
    }
    /// Adapter receives ORIGINAL staging composer/bytes through the unchanged
    /// frozen publisher trait, then routes them here. Never stage under copy ID.
    pub fn publish_capture(
        &self,
        root: &Path,
        drafts: &Path,
        records: &Path,
        source_composer_id: &str,
        assets: &[VerifiedAsset],
    ) -> Result<AttachmentReceipt> {
        ensure!(
            source_composer_id == self.source_composer_id(),
            "Copy was supplied another staging composer"
        );
        self.install(root, records)?;
        let mut draft = self.draft();
        Publisher::new(root, drafts, records, self.attachment_folder(), &mut draft)
            .publish_capture(self.composer_id(), assets)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupersededAttachments {
    root: PathBuf,
    source_composer_id: String,
    source_operation_id: String,
    source_revision: String,
    copy: AttachmentCopy,
    receipt: AttachmentReceipt,
}
impl AttachmentCopy {
    /// Explicitly dismiss the ORIGINAL recovery source after this copy finished.
    /// Leaves its full frozen manifest, draft, staged blobs and current assets
    /// untouched. A copy receipt still never grants source staging GC authority.
    pub fn dismiss_source(&self, root: &Path, records: &Path) -> Result<()> {
        self.validate(root)?;
        verify_copy_origin(root, records, &self.plan)?;
        let receipt = recover_receipt(root, records, self.composer_id())?
            .context("Copy is not durably complete; source cannot be dismissed")?;
        let authorized: AttachmentCopy =
            read(&self.authorization_path(records)?, MAX_COPY_RECORD_BYTES)?
                .context("Copy authorization is missing")?;
        ensure!(authorized == *self, "Copy authorization changed");
        let decision = SupersededAttachments {
            root: root.canonicalize()?,
            source_composer_id: self.source_composer_id().into(),
            source_operation_id: self.source_operation_id().into(),
            source_revision: self.review.origin.source_revision.clone(),
            copy: self.clone(),
            receipt,
        };
        persist_equal(
            records,
            &format!("{}.attachments.superseded.json", self.source_composer_id()),
            &decision,
        )
    }
}
fn source_superseded(root: &Path, records: &Path, source: &Plan) -> Result<bool> {
    let Some(decision) = read::<SupersededAttachments>(
        &records.join(format!(
            "{}.attachments.superseded.json",
            source.composer_id
        )),
        MAX_COPY_RECORD_BYTES + MAX_RECEIPT_BYTES,
    )?
    else {
        return Ok(false);
    };
    ensure!(
        decision.root == root.canonicalize()?
            && decision.source_composer_id == source.composer_id
            && decision.source_operation_id == source.note.receipt.operation_id,
        "Superseded attachment source identity changed"
    );
    decision.copy.validate(root)?;
    verify_copy_origin(root, records, &decision.copy.plan)?;
    ensure!(
        decision.source_revision == decision.copy.review.origin.source_revision
            && decision.copy.source_composer_id() == source.composer_id
            && decision.receipt == decision.copy.plan.receipt(),
        "Superseded attachment copy manifest changed"
    );
    let authorized: AttachmentCopy = read(
        &decision.copy.authorization_path(records)?,
        MAX_COPY_RECORD_BYTES,
    )?
    .context("Superseding copy authorization is missing")?;
    let receipt: AttachmentReceipt = read(
        &records.join(receipt_name(decision.copy.composer_id())),
        MAX_RECEIPT_BYTES,
    )?
    .context("Superseding copy receipt is missing")?;
    ensure!(
        authorized == decision.copy && receipt == decision.receipt,
        "Superseding copy receipt/authorization changed"
    );
    Ok(true)
}

fn copy_directory(records: &Path, composer: &str) -> PathBuf {
    records.join("attachment-copies").join(composer)
}
fn prepare_copy_directory(records: &Path, composer: &str) -> Result<()> {
    // Flush both newly introduced directory entries before a marker can become
    // publication authority. Flushing only the leaf would lose the recovery
    // namespace after a crash even if its marker file had already been synced.
    prepare_records(&records.join("attachment-copies"))?;
    prepare_records(&copy_directory(records, composer))
}
fn source_plan(root: &Path, records: &Path, composer: &str) -> Result<(Plan, String)> {
    id(composer)?;
    let bytes = read_bytes(&records.join(plan_name(composer)), MAX_PLAN_BYTES)?
        .context("No frozen attachment intent to preview")?;
    let plan: Plan = serde_json::from_slice(&bytes)?;
    plan.validate(root, None)?;
    ensure!(
        plan.composer_id == composer,
        "Source attachment composer changed"
    );
    Ok((plan, hash(&bytes)))
}
fn verify_copy_origin(root: &Path, records: &Path, plan: &Plan) -> Result<()> {
    let Some(origin) = &plan.copy_origin else {
        return Ok(());
    };
    let (source, revision) = source_plan(root, records, &origin.source_composer_id)?;
    ensure!(
        revision == origin.source_revision
            && source.note.receipt.operation_id == origin.source_operation_id
            && source.original_text == plan.original_text
            && source
                .assets
                .iter()
                .map(|a| &a.asset)
                .eq(plan.assets.iter().map(|a| &a.asset)),
        "Frozen source intent/manifest changed; preserve copy and staging and preview again"
    );
    Ok(())
}

pub fn preview_copy(
    root: &Path,
    records: &Path,
    source_composer_id: &str,
    capture_folder: &Path,
    attachment_folder: &Path,
) -> Result<AttachmentCopyPreview> {
    let (source, revision) = source_plan(root, records, source_composer_id)?;
    ensure!(
        source.copy_origin.is_none(),
        "Choose the original staging composer; a copy ID is not a new staging identity"
    );
    super::validate_copy_folder(root, capture_folder)?;
    super::validate_copy_folder(root, attachment_folder)?;
    let review = AttachmentCopyReview {
        version: 1,
        root: root.canonicalize()?,
        origin: AttachmentCopyOrigin {
            source_composer_id: source.composer_id.clone(),
            source_operation_id: source.note.receipt.operation_id.clone(),
            source_revision: revision,
        },
        capture_folder: capture_folder.into(),
        attachment_folder: attachment_folder.into(),
        note: observe(root, source.note.path(), MAX_NOTE_BYTES),
        assets: source
            .assets
            .iter()
            .map(|a| observe(root, &a.path, MAX_ASSET_BYTES))
            .collect(),
    };
    let existing = read::<AttachmentCopy>(
        &copy_directory(records, source_composer_id)
            .join(format!("{}.json", review.fingerprint()?)),
        MAX_COPY_RECORD_BYTES,
    )?;
    if let Some(copy) = &existing {
        copy.validate(root)?;
        ensure!(copy.review == review, "Existing copy authorization differs");
    }
    Ok(AttachmentCopyPreview {
        source,
        review,
        existing,
    })
}

/// Recover both pending and completed copies under the ORIGINAL staging identity.
/// Corrupt entries fail explicitly; no stage or canonical data is deleted.
pub fn recover_copies(
    root: &Path,
    records: &Path,
    source_composer_id: &str,
) -> Result<Vec<AttachmentCopy>> {
    id(source_composer_id)?;
    let entries = match std::fs::read_dir(copy_directory(records, source_composer_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut copies = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            anyhow::bail!("Invalid copy authorization filename")
        };
        // NamedTempFile residue before atomic authorization publication is not
        // an approved operation. Preserve it, but do not let it hide durable
        // authorizations or treat its bytes as a publication request.
        if name.strip_prefix(".tmp").is_some_and(|suffix| {
            suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
        }) {
            continue;
        }
        let Some(stem) = name.strip_suffix(".json") else {
            anyhow::bail!("Unexpected copy recovery entry")
        };
        ensure!(valid_revision(stem), "Invalid copy authorization identity");
        let copy: AttachmentCopy = read(&entry.path(), MAX_COPY_RECORD_BYTES)?
            .context("Copy authorization disappeared during recovery")?;
        copy.validate(root)?;
        ensure!(
            copy.source_composer_id() == source_composer_id && copy.review.fingerprint()? == stem,
            "Copy recovery scope/identity mismatch"
        );
        copies.push(copy);
    }
    copies.sort_by(|a, b| a.composer_id().cmp(b.composer_id()));
    Ok(copies)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    const ASSET: &str = "22222222-2222-4222-8222-222222222222";
    const SECOND: &str = "33333333-3333-4333-8333-333333333333";
    fn input(id: &str, bytes: &[u8], kind: AssetKind, name: &str) -> VerifiedAsset {
        VerifiedAsset {
            asset: Asset {
                id: id.into(),
                display_name: name.into(),
                kind,
                byte_length: bytes.len(),
                sha256: hash(bytes),
            },
            bytes: bytes.into(),
        }
    }
    fn fixture(text: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, ComposerDraft) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::create_dir(root.join("Attachments")).unwrap();
        let records = temp.path().join("records");
        let drafts = temp.path().join("drafts");
        let composer = ComposerDraft {
            text: text.into(),
            folder: Some("Inbox".into()),
            ..Default::default()
        };
        (temp, root, records, drafts, composer)
    }
    fn publish(
        root: &Path,
        drafts: &Path,
        records: &Path,
        draft: &mut ComposerDraft,
        assets: &[VerifiedAsset],
    ) -> Result<AttachmentReceipt> {
        let composer = draft.id.clone();
        Publisher::new(root, drafts, records, Path::new("Attachments"), draft)
            .publish_capture(&composer, assets)
    }
    fn plan(records: &Path, draft: &ComposerDraft) -> Plan {
        read(&records.join(plan_name(&draft.id)), MAX_PLAN_BYTES)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn exact_manifest_links_and_durable_receipt_gate_cleanup() {
        let (_temp, root, records, drafts, mut draft) = fixture("\u{feff}authored\r\ntext");
        let assets = vec![
            input(
                ASSET,
                b"\x89PNG\r\n\x1a\nphoto",
                AssetKind::Photo,
                "../photo](bad).png",
            ),
            input(
                SECOND,
                b"\0file\xff",
                AssetKind::SharedFile,
                "file [one].txt",
            ),
        ];
        let receipt = publish(&root, &drafts, &records, &mut draft, &assets).unwrap();
        let note = std::fs::read_to_string(root.join(&receipt.note.path)).unwrap();
        assert!(note.starts_with("\u{feff}authored\r\ntext\n\n!"));
        assert!(note.contains("../Attachments/Capture-"));
        assert!(!note.contains("(bad).png)"));
        assert_eq!(draft.text, "\u{feff}authored\r\ntext");
        assert_eq!(receipt.assets.len(), 2);
        for (published, source) in receipt.assets.iter().zip(&assets) {
            assert_eq!(published.asset, source.asset);
            assert_eq!(
                std::fs::read(root.join(&published.path)).unwrap(),
                source.bytes
            );
        }
        let identities: Vec<_> = assets.iter().map(|a| a.asset.clone()).collect();
        let permit =
            verify_cleanup_receipt(&root, &records, &draft.id, &identities, &receipt).unwrap();
        assert_eq!(permit.composer_id(), draft.id);
        assert_eq!(permit.operation_id(), receipt.note.operation_id);
        assert_eq!(permit.assets(), identities);
        let mut wrong = identities.clone();
        wrong.reverse();
        assert!(verify_cleanup_receipt(&root, &records, &draft.id, &wrong, &receipt).is_err());
        // Completion is authoritative: later canonical edits are not overwritten
        // and do not turn one completed operation back into a new capture.
        std::fs::write(root.join(&receipt.assets[0].path), b"later edit").unwrap();
        std::fs::remove_file(root.join(&receipt.note.path)).unwrap();
        assert_eq!(
            publish(&root, &drafts, &records, &mut draft, &assets).unwrap(),
            receipt
        );
        assert_eq!(
            std::fs::read(root.join(&receipt.assets[0].path)).unwrap(),
            b"later edit"
        );
        assert!(!root.join(&receipt.note.path).exists());
        assert!(verify_cleanup_receipt(&root, &records, &draft.id, &identities, &receipt).is_ok());
        std::fs::remove_file(records.join(receipt_name(&draft.id))).unwrap();
        assert!(verify_cleanup_receipt(&root, &records, &draft.id, &identities, &receipt).is_err());
        // Positive control: note + asset receipts can durably rebuild the lost
        // aggregate, even though the canonical asset was edited and note deleted.
        assert_eq!(
            publish(&root, &drafts, &records, &mut draft, &assets).unwrap(),
            receipt
        );
        assert_eq!(
            std::fs::read(root.join(&receipt.assets[0].path)).unwrap(),
            b"later edit"
        );
        assert!(!root.join(&receipt.note.path).exists());
        assert!(verify_cleanup_receipt(&root, &records, &draft.id, &identities, &receipt).is_ok());
    }

    #[test]
    fn failed_partial_assets_keep_original_text_and_retry_one_exact_note() {
        let (_temp, root, records, drafts, mut draft) = fixture("retained draft");
        let assets = vec![
            input(ASSET, b"first\0", AssetKind::SharedFile, "one"),
            input(SECOND, b"second\xff", AssetKind::SharedFile, "two"),
        ];
        let failed = crate::unix_publish::inject(rustix::io::Errno::ACCESS, || {
            publish(&root, &drafts, &records, &mut draft, &assets)
        });
        assert!(failed.is_err());
        let saved_plan = plan(&records, &draft);
        let operation = draft.attachment_attempt.clone().unwrap();
        assert!(draft.prepare_save().is_err());
        assert!(draft.publish(&root, &drafts, &records).is_err());
        // Crash boundary: real shared writer published one asset but no receipt.
        crate::note_files::create_attachment_with_source(
            &root,
            &saved_plan.assets[0].path,
            &assets[0].bytes,
        )
        .unwrap();
        // A foreign note blocks completion after the assets have become durable.
        crate::note_files::create_with_source(&root, saved_plan.note.path(), b"external note")
            .unwrap();
        let mut recovered = super::super::recover_composers(&root, &records)
            .unwrap()
            .remove(0);
        let error = publish(&root, &drafts, &records, &mut recovered, &assets).unwrap_err();
        let partial = error.downcast_ref::<IncompletePublication>().unwrap();
        assert_eq!(partial.completed_assets, 2);
        assert!(!partial.note_acknowledged);
        assert_eq!(partial.operation_id, operation);
        assert_eq!(recovered.text, "retained draft");
        assert_eq!(
            std::fs::read(root.join(saved_plan.note.path())).unwrap(),
            b"external note"
        );
        let identities: Vec<_> = assets.iter().map(|a| a.asset.clone()).collect();
        assert!(verify_cleanup_receipt(
            &root,
            &records,
            &draft.id,
            &identities,
            &saved_plan.receipt()
        )
        .is_err());
        std::fs::remove_file(root.join(&saved_plan.assets[0].path)).unwrap();
        let error = publish(&root, &drafts, &records, &mut recovered, &assets).unwrap_err();
        assert!(error
            .to_string()
            .contains("acknowledged attachment disappeared"));
        assert!(!root.join(&saved_plan.assets[0].path).exists());
        // Explicit fixture repair is the positive control, never a product retry
        // that silently restores a user-deleted acknowledged attachment.
        crate::note_files::create_attachment_with_source(
            &root,
            &saved_plan.assets[0].path,
            &assets[0].bytes,
        )
        .unwrap();
        std::fs::remove_file(root.join(saved_plan.note.path())).unwrap();
        let receipt = publish(&root, &drafts, &records, &mut recovered, &assets).unwrap();
        assert_eq!(receipt.note.operation_id, operation);
        assert_eq!(
            std::fs::read_dir(root.join("Attachments")).unwrap().count(),
            2
        );
        assert_eq!(std::fs::read_dir(root.join("Inbox")).unwrap().count(), 1);
        assert!(super::super::recover_composers(&root, &records)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn frozen_identity_bounds_and_changed_partial_refuse_without_overwrite() {
        let (_temp, root, records, drafts, mut draft) = fixture("");
        let assets = vec![input(
            ASSET,
            b"\x89PNG\r\n\x1a\nphoto only payload",
            AssetKind::Photo,
            "photo.png",
        )];
        assert!(
            crate::unix_publish::inject(rustix::io::Errno::ACCESS, || publish(
                &root, &drafts, &records, &mut draft, &assets
            ))
            .is_err()
        );
        let original_plan = std::fs::read(records.join(plan_name(&draft.id))).unwrap();
        let projected = super::super::recover_composers(&root, &records).unwrap();
        assert_eq!(
            projected.len(),
            1,
            "photo-only draft is not an empty capture"
        );
        let mut changed = vec![input(ASSET, b"changed", AssetKind::SharedFile, "photo.bin")];
        assert!(publish(&root, &drafts, &records, &mut draft, &changed).is_err());
        changed[0].asset = assets[0].asset.clone();
        assert!(publish(&root, &drafts, &records, &mut draft, &changed).is_err());
        let duplicate = vec![
            input(ASSET, b"one", AssetKind::SharedFile, "one"),
            input(ASSET, b"two", AssetKind::SharedFile, "two"),
        ];
        assert!(publish(&root, &drafts, &records, &mut draft, &duplicate).is_err());
        assert_eq!(
            std::fs::read(records.join(plan_name(&draft.id))).unwrap(),
            original_plan
        );
        let target = &plan(&records, &draft).assets[0].path;
        crate::note_files::create_attachment_with_source(&root, target, b"foreign divergent bytes")
            .unwrap();
        assert!(publish(&root, &drafts, &records, &mut draft, &assets).is_err());
        assert_eq!(
            std::fs::read(root.join(target)).unwrap(),
            b"foreign divergent bytes"
        );
        std::fs::remove_file(root.join(target)).unwrap();
        let receipt = publish(&root, &drafts, &records, &mut draft, &assets).unwrap();
        assert!(std::fs::read_to_string(root.join(receipt.note.path))
            .unwrap()
            .starts_with('!'));
    }

    #[test]
    fn unsafe_folder_unsupported_photo_and_oversized_identity_are_explicit() {
        use std::os::unix::fs::symlink;
        let (_temp, root, records, drafts, mut draft) = fixture("text");
        let outside = tempfile::tempdir().unwrap();
        let assets = vec![input(ASSET, b"bytes", AssetKind::SharedFile, "file")];
        symlink(outside.path(), root.join("linked")).unwrap();
        let composer = draft.id.clone();
        assert!(
            Publisher::new(&root, &drafts, &records, Path::new("linked"), &mut draft)
                .publish_capture(&composer, &assets)
                .is_err()
        );
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        let bad_photo = vec![input(
            ASSET,
            b"not a supported photo",
            AssetKind::Photo,
            "guess.png",
        )];
        assert!(publish(&root, &drafts, &records, &mut draft, &bad_photo).is_err());
        assert!(draft.attachment_attempt.is_none());
        let mut too_large = vec![input(ASSET, b"small", AssetKind::SharedFile, "file")];
        too_large[0].asset.byte_length = MAX_ASSET_BYTES + 1;
        assert!(publish(&root, &drafts, &records, &mut draft, &too_large).is_err());
        assert!(!records.join(plan_name(&draft.id)).exists());
        publish(&root, &drafts, &records, &mut draft, &assets).unwrap();
        assert_eq!(
            std::fs::read_dir(root.join("Attachments")).unwrap().count(),
            1
        );
    }
    #[test]
    fn serialized_plan_bounds_refuse_before_any_canonical_write() {
        let (_temp, root, records, drafts, mut draft) = fixture(&"\0".repeat(600 * 1024));
        let assets = vec![input(ASSET, b"small", AssetKind::SharedFile, "file")];
        let error = publish(&root, &drafts, &records, &mut draft, &assets).unwrap_err();
        assert!(error
            .to_string()
            .contains("plan representation exceeds recovery bounds"));
        assert!(draft.attachment_attempt.is_none());
        assert!(!records.join(plan_name(&draft.id)).exists());
        assert_eq!(
            std::fs::read_dir(root.join("Attachments")).unwrap().count(),
            0
        );
        assert_eq!(std::fs::read_dir(root.join("Inbox")).unwrap().count(), 0);
        draft.text = "within bounds".into();
        publish(&root, &drafts, &records, &mut draft, &assets).unwrap();
        assert_eq!(
            std::fs::read_dir(root.join("Attachments")).unwrap().count(),
            1
        );
    }
    fn copy_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        PathBuf,
        ComposerDraft,
        Vec<VerifiedAsset>,
    ) {
        let (temp, root, records, drafts, mut draft) = fixture("\u{feff}frozen authored\r\n");
        std::fs::create_dir(root.join("Copies")).unwrap();
        std::fs::create_dir(root.join("CopyAssets")).unwrap();
        let assets = vec![
            input(
                ASSET,
                b"\x89PNG\r\n\x1a\nphoto",
                AssetKind::Photo,
                "../photo](url).png",
            ),
            input(SECOND, b"\0shared\xff", AssetKind::SharedFile, "file.txt"),
        ];
        assert!(
            crate::unix_publish::inject(rustix::io::Errno::ACCESS, || publish(
                &root, &drafts, &records, &mut draft, &assets
            ))
            .is_err()
        );
        (temp, root, records, drafts, draft, assets)
    }

    #[test]
    fn attachment_save_copy_preserves_full_manifest_original_intent_and_current_files() {
        let (_temp, root, records, drafts, original, assets) = copy_fixture();
        let source = plan(&records, &original);
        crate::note_files::create_attachment_with_source(
            &root,
            &source.assets[0].path,
            b"external divergent asset",
        )
        .unwrap();
        crate::note_files::create_attachment_with_source(
            &root,
            &source.assets[1].path,
            &assets[1].bytes,
        )
        .unwrap();
        crate::note_files::create_with_source(&root, source.note.path(), b"external current note")
            .unwrap();
        let intent_bytes = std::fs::read(records.join(plan_name(&original.id))).unwrap();
        let draft_bytes =
            std::fs::read(records.join(format!("{}.composer.json", original.id))).unwrap();
        let entries = std::fs::read_dir(&records).unwrap().count();
        let preview = preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("Copies"),
            Path::new("CopyAssets"),
        )
        .unwrap();
        assert_eq!(
            preview.source_operation_id(),
            source.note.receipt.operation_id
        );
        assert_eq!(preview.text(), original.text);
        assert_eq!(preview.assets(), source.assets);
        assert_eq!(
            preview.asset_observations()[0],
            AttachmentObservation::Present {
                byte_length: b"external divergent asset".len(),
                sha256: hash(b"external divergent asset")
            }
        );
        assert_eq!(
            preview.asset_observations()[1],
            AttachmentObservation::Present {
                byte_length: assets[1].bytes.len(),
                sha256: assets[1].asset.sha256.clone()
            }
        );
        assert!(preview.existing_copy().is_none());
        assert_eq!(std::fs::read_dir(&records).unwrap().count(), entries);
        assert_eq!(
            std::fs::read_dir(root.join("CopyAssets")).unwrap().count(),
            0
        );
        let copy = preview.start_copy(&root, &records).unwrap();
        assert_ne!(copy.composer_id(), original.id);
        assert!(preview_copy(
            &root,
            &records,
            copy.composer_id(),
            Path::new("Copies"),
            Path::new("CopyAssets")
        )
        .unwrap_err()
        .to_string()
        .contains("original staging composer"));
        assert_ne!(copy.operation_id(), source.note.receipt.operation_id);
        assert_eq!(
            preview.start_copy(&root, &records).unwrap(),
            copy,
            "same approved action resumes its existing IDs"
        );
        let repeated = preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("Copies"),
            Path::new("CopyAssets"),
        )
        .unwrap();
        assert_eq!(repeated.existing_copy(), Some(&copy));
        let receipt = copy
            .publish_capture(&root, &drafts, &records, &original.id, &assets)
            .unwrap();
        assert_eq!(receipt.composer_id, copy.composer_id());
        assert_eq!(receipt.note.operation_id, copy.operation_id());
        for (published, staged) in receipt.assets.iter().zip(&assets) {
            assert_eq!(published.asset, staged.asset);
            assert_eq!(
                std::fs::read(root.join(&published.path)).unwrap(),
                staged.bytes
            );
        }
        assert!(std::fs::read_to_string(root.join(&receipt.note.path))
            .unwrap()
            .starts_with(&original.text));
        assert_eq!(
            std::fs::read(root.join(&source.assets[0].path)).unwrap(),
            b"external divergent asset"
        );
        assert_eq!(
            std::fs::read(root.join(source.note.path())).unwrap(),
            b"external current note"
        );
        assert_eq!(
            std::fs::read(records.join(plan_name(&original.id))).unwrap(),
            intent_bytes
        );
        assert_eq!(
            std::fs::read(records.join(format!("{}.composer.json", original.id))).unwrap(),
            draft_bytes
        );
        let manifest: Vec<_> = assets.iter().map(|a| a.asset.clone()).collect();
        assert!(
            verify_cleanup_receipt(&root, &records, &original.id, &manifest, &receipt).is_err()
        );
        assert!(
            verify_cleanup_receipt(&root, &records, copy.composer_id(), &manifest, &receipt)
                .unwrap_err()
                .to_string()
                .contains("does not authorize source staging cleanup")
        );
        let recovered = super::super::recover_composers(&root, &records).unwrap();
        assert_eq!(recovered, vec![original]);
        assert_eq!(
            recover_copies(&root, &records, copy.source_composer_id()).unwrap(),
            vec![copy.clone()]
        );
        assert_eq!(copy.receipt(&root, &records).unwrap(), Some(receipt));
    }

    #[test]
    fn attachment_copy_preview_revision_guards_refuse_then_same_authorization_retries() {
        use std::os::unix::fs::symlink;
        let (_temp, root, records, drafts, original, assets) = copy_fixture();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.join("redirect")).unwrap();
        assert!(preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("Copies"),
            Path::new("redirect")
        )
        .is_err());
        assert!(preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("../outside"),
            Path::new("CopyAssets")
        )
        .is_err());
        let preview = preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("Copies"),
            Path::new("CopyAssets"),
        )
        .unwrap();
        let source = plan(&records, &original);
        crate::note_files::create_attachment_with_source(
            &root,
            &source.assets[0].path,
            b"changed after preview",
        )
        .unwrap();
        assert!(preview
            .start_copy(&root, &records)
            .unwrap_err()
            .to_string()
            .contains("changed after preview"));
        assert!(!copy_directory(&records, &original.id).exists());
        let fresh = preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("Copies"),
            Path::new("CopyAssets"),
        )
        .unwrap();
        let copy = fresh.start_copy(&root, &records).unwrap();
        let source_path = records.join(plan_name(&original.id));
        let saved = std::fs::read(&source_path).unwrap();
        let mut changed = saved.clone();
        changed.push(b'\n');
        std::fs::write(&source_path, &changed).unwrap();
        let error = copy
            .publish_capture(&root, &drafts, &records, &original.id, &assets)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("Frozen source intent/manifest changed"));
        assert_eq!(
            std::fs::read_dir(root.join("CopyAssets")).unwrap().count(),
            0
        );
        assert_eq!(std::fs::read(&source_path).unwrap(), changed);
        std::fs::write(&source_path, &saved).unwrap();
        let receipt = copy
            .publish_capture(&root, &drafts, &records, &original.id, &assets)
            .unwrap();
        std::fs::write(&source_path, changed).unwrap();
        std::fs::write(root.join(&receipt.assets[0].path), b"later copy edit").unwrap();
        std::fs::remove_file(root.join(&receipt.note.path)).unwrap();
        assert_eq!(
            copy.publish_capture(&root, &drafts, &records, &original.id, &assets)
                .unwrap(),
            receipt
        );
        assert_eq!(
            std::fs::read(root.join(&receipt.assets[0].path)).unwrap(),
            b"later copy edit"
        );
        assert!(!root.join(&receipt.note.path).exists());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn attachment_copy_authorization_recovers_before_checkpoint_and_keeps_staging_identity() {
        let (_temp, root, records, drafts, original, assets) = copy_fixture();
        let copy = preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("Copies"),
            Path::new("CopyAssets"),
        )
        .unwrap()
        .start_copy(&root, &records)
        .unwrap();
        // Model a crash after immutable authorization, before plan/composer install.
        std::fs::remove_file(records.join(plan_name(copy.composer_id()))).unwrap();
        std::fs::remove_file(records.join(format!("{}.composer.json", copy.composer_id())))
            .unwrap();
        let temporary = copy_directory(&records, &original.id).join(".tmpAb12xy");
        std::fs::write(&temporary, b"unpublished temporary authorization").unwrap();
        let recovered = recover_copies(&root, &records, &original.id)
            .unwrap()
            .remove(0);
        assert_eq!(
            std::fs::read(&temporary).unwrap(),
            b"unpublished temporary authorization"
        );
        assert_eq!(recovered, copy);
        assert!(recovered
            .publish_capture(&root, &drafts, &records, copy.composer_id(), &assets)
            .is_err());
        assert!(
            crate::unix_publish::inject(rustix::io::Errno::ACCESS, || recovered.publish_capture(
                &root,
                &drafts,
                &records,
                &original.id,
                &assets
            ))
            .is_err()
        );
        assert_eq!(
            super::super::recover_composers(&root, &records).unwrap(),
            vec![original.clone()]
        );
        assert_eq!(
            recover_copies(&root, &records, &original.id).unwrap(),
            vec![copy.clone()]
        );
        let reversed = vec![
            input(
                SECOND,
                &assets[1].bytes,
                assets[1].asset.kind.clone(),
                &assets[1].asset.display_name,
            ),
            input(
                ASSET,
                &assets[0].bytes,
                assets[0].asset.kind.clone(),
                &assets[0].asset.display_name,
            ),
        ];
        assert!(recovered
            .publish_capture(&root, &drafts, &records, &original.id, &reversed)
            .is_err());
        let receipt = recovered
            .publish_capture(&root, &drafts, &records, &original.id, &assets)
            .unwrap();
        assert_eq!(receipt.note.operation_id, copy.operation_id());
        assert_eq!(std::fs::read_dir(root.join("Copies")).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_dir(root.join("CopyAssets")).unwrap().count(),
            2
        );
        assert_eq!(
            std::fs::read_dir(root.join("Attachments")).unwrap().count(),
            0
        );
        assert_eq!(original.text, "\u{feff}frozen authored\r\n");
    }

    #[test]
    fn missing_copy_authorization_is_an_explicit_recovery_failure_not_empty_success() {
        let (_temp, root, records, drafts, original, assets) = copy_fixture();
        let preview = preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("Copies"),
            Path::new("CopyAssets"),
        )
        .unwrap();
        let copy = preview.start_copy(&root, &records).unwrap();
        let path = copy.authorization_path(&records).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(copy.receipt(&root, &records).is_err());
        assert!(copy
            .publish_capture(&root, &drafts, &records, &original.id, &assets)
            .is_err());
        assert!(super::super::recover_composers(&root, &records)
            .unwrap_err()
            .to_string()
            .contains("copy authorization is missing"));
        assert_eq!(
            std::fs::read_dir(root.join("CopyAssets")).unwrap().count(),
            0
        );
        // Positive control: restoring the exact authorization makes this same
        // operation recoverable, without allocating or replacing any identities.
        persist_equal(
            path.parent().unwrap(),
            path.file_name().unwrap().to_str().unwrap(),
            &copy,
        )
        .unwrap();
        assert_eq!(
            super::super::recover_composers(&root, &records).unwrap(),
            vec![original.clone()]
        );
        assert_eq!(
            copy.publish_capture(&root, &drafts, &records, &original.id, &assets)
                .unwrap()
                .note
                .operation_id,
            copy.operation_id()
        );
    }
    fn receipt_record_snapshot(directory: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(directory: &Path, values: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
            if !directory.exists() {
                return;
            }
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, values);
                } else {
                    values.insert(path.clone(), std::fs::read(path).unwrap());
                }
            }
        }
        let mut values = std::collections::BTreeMap::new();
        visit(directory, &mut values);
        values
    }

    #[test]
    fn receipt_lookup_restart_is_read_only_and_never_completes_a_missing_aggregate() {
        let (_temp, root, records, drafts, mut draft) = fixture("original\r\n");
        let assets = vec![input(ASSET, b"\0exact\xff", AssetKind::SharedFile, "file")];
        assert_eq!(recover_receipt(&root, &records, &draft.id).unwrap(), None);
        assert!(
            !records.exists(),
            "unknown lookup must not provision storage"
        );
        assert!(
            crate::unix_publish::inject(rustix::io::Errno::ACCESS, || publish(
                &root, &drafts, &records, &mut draft, &assets
            ))
            .is_err()
        );
        let pending = receipt_record_snapshot(&records);
        assert_eq!(recover_receipt(&root, &records, &draft.id).unwrap(), None);
        assert_eq!(receipt_record_snapshot(&records), pending);
        let receipt = publish(&root, &drafts, &records, &mut draft, &assets).unwrap();
        let composer = draft.id.clone();
        drop(draft); // The public lookup needs no in-memory session or input bytes.
        let complete = receipt_record_snapshot(&records);
        assert_eq!(
            recover_receipt(&root, &records, &composer).unwrap(),
            Some(receipt.clone())
        );
        assert_eq!(receipt_record_snapshot(&records), complete);
        let aggregate = records.join(receipt_name(&composer));
        let saved = std::fs::read(&aggregate).unwrap();
        std::fs::remove_file(&aggregate).unwrap();
        let incomplete = receipt_record_snapshot(&records);
        assert_eq!(recover_receipt(&root, &records, &composer).unwrap(), None);
        assert!(
            !aggregate.exists(),
            "read must not synthesize an aggregate from subordinate receipts"
        );
        assert_eq!(receipt_record_snapshot(&records), incomplete);
        std::fs::write(aggregate, saved).unwrap();
        assert_eq!(
            recover_receipt(&root, &records, &composer).unwrap(),
            Some(receipt)
        );
        assert_eq!(
            std::fs::read_dir(root.join("Attachments")).unwrap().count(),
            1
        );
        assert_eq!(std::fs::read_dir(root.join("Inbox")).unwrap().count(), 1);
    }

    #[test]
    fn receipt_lookup_holds_changed_missing_or_redirected_canonical_bytes() {
        use std::os::unix::fs::symlink;
        let (_temp, root, records, drafts, mut draft) = fixture("exact note");
        let assets = vec![input(
            ASSET,
            b"original bytes",
            AssetKind::SharedFile,
            "file",
        )];
        let receipt = publish(&root, &drafts, &records, &mut draft, &assets).unwrap();
        let operation = draft.attachment_attempt.clone();
        let original_ledger = receipt_record_snapshot(&records);
        let asset = root.join(&receipt.assets[0].path);
        std::fs::write(&asset, b"external changed asset").unwrap();
        assert!(recover_receipt(&root, &records, &draft.id)
            .unwrap_err()
            .to_string()
            .contains("canonical attachment changed"));
        assert_eq!(std::fs::read(&asset).unwrap(), b"external changed asset");
        std::fs::remove_file(&asset).unwrap();
        assert!(recover_receipt(&root, &records, &draft.id)
            .unwrap_err()
            .to_string()
            .contains("canonical attachment is missing"));
        assert!(!asset.exists(), "lookup must not recreate deleted data");
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("same.bin"), &assets[0].bytes).unwrap();
        symlink(outside.path().join("same.bin"), &asset).unwrap();
        assert!(
            recover_receipt(&root, &records, &draft.id).is_err(),
            "same bytes behind a symlink are not canonical identity"
        );
        assert_eq!(
            std::fs::read(outside.path().join("same.bin")).unwrap(),
            assets[0].bytes
        );
        std::fs::remove_file(&asset).unwrap();
        crate::note_files::create_attachment_with_source(
            &root,
            &receipt.assets[0].path,
            &assets[0].bytes,
        )
        .unwrap();
        assert_eq!(
            recover_receipt(&root, &records, &draft.id).unwrap(),
            Some(receipt.clone())
        );
        let note = root.join(&receipt.note.path);
        let saved_note = std::fs::read(&note).unwrap();
        std::fs::write(&note, b"external changed note").unwrap();
        assert!(recover_receipt(&root, &records, &draft.id)
            .unwrap_err()
            .to_string()
            .contains("canonical note changed"));
        assert_eq!(std::fs::read(&note).unwrap(), b"external changed note");
        std::fs::remove_file(&note).unwrap();
        assert!(recover_receipt(&root, &records, &draft.id)
            .unwrap_err()
            .to_string()
            .contains("canonical note is missing"));
        assert!(!note.exists());
        std::fs::write(note, saved_note).unwrap();
        assert_eq!(
            recover_receipt(&root, &records, &draft.id).unwrap(),
            Some(receipt)
        );
        assert_eq!(draft.attachment_attempt, operation);
        assert_eq!(receipt_record_snapshot(&records), original_ledger);
    }

    #[test]
    fn receipt_lookup_bounds_and_exact_ledger_binding_fail_closed() {
        let (_temp, root, records, drafts, mut draft) = fixture("text");
        let assets = vec![input(ASSET, b"bytes", AssetKind::SharedFile, "file")];
        let receipt = publish(&root, &drafts, &records, &mut draft, &assets).unwrap();
        let foreign = tempfile::tempdir().unwrap();
        assert!(recover_receipt(foreign.path(), &records, &draft.id).is_err());
        assert!(recover_receipt(&root, &records, "../escape").is_err());
        let aggregate = records.join(receipt_name(&draft.id));
        let aggregate_bytes = std::fs::read(&aggregate).unwrap();
        let mut wrong = receipt.clone();
        wrong.assets[0].asset.display_name = "different delivery".into();
        std::fs::write(&aggregate, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(recover_receipt(&root, &records, &draft.id).is_err());
        std::fs::write(&aggregate, &aggregate_bytes).unwrap();
        let asset_receipt = records.join(asset_receipt_name(&receipt.note.operation_id, ASSET));
        let saved_asset = std::fs::read(&asset_receipt).unwrap();
        std::fs::remove_file(&asset_receipt).unwrap();
        assert!(recover_receipt(&root, &records, &draft.id).is_err());
        assert!(!asset_receipt.exists());
        std::fs::write(asset_receipt, saved_asset).unwrap();
        let plan_path = records.join(plan_name(&draft.id));
        let saved_plan = std::fs::read(&plan_path).unwrap();
        std::fs::remove_file(&plan_path).unwrap();
        assert!(recover_receipt(&root, &records, &draft.id)
            .unwrap_err()
            .to_string()
            .contains("durable plan is missing"));
        std::fs::write(&plan_path, vec![b' '; MAX_PLAN_BYTES + 1]).unwrap();
        assert!(recover_receipt(&root, &records, &draft.id)
            .unwrap_err()
            .to_string()
            .contains("exceeds bounds"));
        std::fs::write(&plan_path, &saved_plan).unwrap();
        std::fs::remove_file(&aggregate).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &aggregate,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        assert!(recover_receipt(&root, &records, &draft.id)
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));
        std::fs::remove_file(&aggregate).unwrap();
        let linked = records.join("untrusted-receipt");
        std::fs::write(&linked, &aggregate_bytes).unwrap();
        std::os::unix::fs::symlink(&linked, &aggregate).unwrap();
        assert!(recover_receipt(&root, &records, &draft.id).is_err());
        assert_eq!(std::fs::read(&linked).unwrap(), aggregate_bytes);
        std::fs::remove_file(&aggregate).unwrap();
        std::fs::write(&aggregate, vec![b' '; MAX_RECEIPT_BYTES + 1]).unwrap();
        assert!(recover_receipt(&root, &records, &draft.id)
            .unwrap_err()
            .to_string()
            .contains("exceeds bounds"));
        std::fs::write(&aggregate, aggregate_bytes).unwrap();
        assert_eq!(
            recover_receipt(&root, &records, &draft.id).unwrap(),
            Some(receipt)
        );
    }
    #[test]
    fn explicit_copy_dismissal_preserves_frozen_source_and_never_authorizes_cleanup() {
        let (_temp, root, records, drafts, original, assets) = copy_fixture();
        let source_plan_bytes = std::fs::read(records.join(plan_name(&original.id))).unwrap();
        let source_draft_bytes =
            std::fs::read(records.join(format!("{}.composer.json", original.id))).unwrap();
        let source = plan(&records, &original);
        crate::note_files::create_attachment_with_source(
            &root,
            &source.assets[0].path,
            b"current external asset",
        )
        .unwrap();
        let copy = preview_copy(
            &root,
            &records,
            &original.id,
            Path::new("Copies"),
            Path::new("CopyAssets"),
        )
        .unwrap()
        .start_copy(&root, &records)
        .unwrap();
        assert!(copy.dismiss_source(&root, &records).is_err());
        assert_eq!(
            super::super::recover_composers(&root, &records).unwrap(),
            vec![original.clone()]
        );
        let receipt = copy
            .publish_capture(&root, &drafts, &records, &original.id, &assets)
            .unwrap();
        assert_eq!(
            super::super::recover_composers(&root, &records).unwrap(),
            vec![original.clone()]
        );
        copy.dismiss_source(&root, &records).unwrap();
        copy.dismiss_source(&root, &records).unwrap();
        assert!(super::super::recover_composers(&root, &records)
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read(records.join(plan_name(&original.id))).unwrap(),
            source_plan_bytes
        );
        assert_eq!(
            std::fs::read(records.join(format!("{}.composer.json", original.id))).unwrap(),
            source_draft_bytes
        );
        assert_eq!(
            std::fs::read(root.join(&source.assets[0].path)).unwrap(),
            b"current external asset"
        );
        assert_eq!(
            recover_copies(&root, &records, &original.id).unwrap(),
            vec![copy.clone()]
        );
        let manifest: Vec<_> = assets.iter().map(|a| a.asset.clone()).collect();
        assert!(
            verify_cleanup_receipt(&root, &records, copy.composer_id(), &manifest, &receipt)
                .is_err()
        );
        assert!(
            verify_cleanup_receipt(&root, &records, &original.id, &manifest, &receipt).is_err()
        );
        // Presentation marker remains valid after subsequent user edits to the
        // saved copy; those edits cannot resurrect or rewrite the old capture.
        std::fs::write(root.join(&receipt.note.path), b"later copy edit").unwrap();
        assert!(super::super::recover_composers(&root, &records)
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read(root.join(&receipt.note.path)).unwrap(),
            b"later copy edit"
        );
    }
}
