//! Attachment transactions over the existing capture and shared create-only writer.
//! The mobile staging owner maps its frozen VerifiedAsset fields to these values.
//! This module never opens provider paths, deletes staging, or widens a protocol.
use super::{
    persist_equal, prepare_records, read_destination_bounded, Capture, ComposerDraft, Receipt,
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
fn read<T: serde::de::DeserializeOwned>(path: &Path, max: usize) -> Result<Option<T>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take((max + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= max,
        "Attachment operational record exceeds bounds"
    );
    Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
        format!("Invalid attachment record: {}", path.display())
    })?))
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
}
