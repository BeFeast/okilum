//! Explicit, byte-preserving service relocation. Backups/journals are local,
//! outside discovery and Sync. A vault marker refuses all ordinary consumers
//! until the complete transaction is committed or explicitly recovered.
use super::*;
use std::{io::Write, time::UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
    pub modified_seconds: u64,
    pub modified_nanos: u32,
    pub readonly: bool,
    pub unix_mode: Option<u32>,
    identity: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub source: ServiceLayout,
    pub target: Kind,
    pub files: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Prepared,
    Committed,
    RollingBack,
    RolledBack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub plan: Plan,
    pub phase: Phase,
    target_identity: String,
    output: Vec<Entry>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn stamp(path: &Path, name: &str) -> Result<Entry> {
    let bytes = regular_bytes(path)?;
    let meta = fs::symlink_metadata(path)?;
    let modified = meta.modified()?.duration_since(UNIX_EPOCH)?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode())
    };
    #[cfg(not(unix))]
    let mode = None;
    Ok(Entry {
        name: name.into(),
        sha256: digest(&bytes),
        bytes: bytes.len() as u64,
        modified_seconds: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        readonly: meta.permissions().readonly(),
        unix_mode: mode,
        identity: identity(path)?,
    })
}
fn same_bytes(path: &Path, entry: &Entry) -> Result<()> {
    let bytes = regular_bytes(path)?;
    ensure!(
        bytes.len() as u64 == entry.bytes && digest(&bytes) == entry.sha256,
        "{} changed; reconcile instead of overwriting newer data",
        path.display()
    );
    Ok(())
}
fn same_revision(path: &Path, entry: &Entry) -> Result<()> {
    ensure!(
        stamp(path, &entry.name)? == *entry,
        "{} revision or identity changed; reconcile instead of overwriting newer data",
        path.display()
    );
    Ok(())
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn sync(directory: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}
fn new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    sync(path.parent().context("Missing service parent")?)
}
fn persist(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    new_file(&temp, &serde_json::to_vec_pretty(value)?)?;
    fs::rename(&temp, path)?;
    sync(path.parent().context("Missing journal parent")?)
}
fn transaction(context: &Path, id: &str) -> Result<PathBuf> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string() == id),
        "Invalid service transaction identity"
    );
    Ok(super::state_directory(context)?.join(id))
}
fn stage(receipt: &Receipt) -> PathBuf {
    receipt
        .plan
        .source
        .directory()
        .join(format!(".service-relocate-{}", receipt.id))
}
fn marker(context: &Path) -> PathBuf {
    context.join(".okilum").join(MIGRATION)
}

impl Plan {
    /// No vault or application-state writes. Source locations and revisions are
    /// pinned in the reviewable plan; no contents or identities are rewritten.
    pub fn preview(context: &Path) -> Result<Self> {
        Self::towards(context, Kind::Hidden)
    }

    fn towards(context: &Path, target: Kind) -> Result<Self> {
        let context = context.canonicalize()?;
        let source = ServiceLayout::resolve(&context)?;
        ensure!(
            source.kind != target,
            "Service files already use {}",
            target.directory()
        );
        let files = inventory(&context, source.kind)?
            .iter()
            .map(|name| stamp(&source.path(name)?, name))
            .collect::<Result<Vec<_>>>()?;
        ensure!(!files.is_empty(), "No service files to relocate");
        #[cfg(windows)]
        ensure!(files.iter().all(|entry| !entry.readonly), "Read-only service files cannot be relocated safely on Windows; review their permissions first (no files changed)");
        Ok(Self {
            source,
            target,
            files,
        })
    }

    pub fn description(&self) -> String {
        let mut text = format!("Owning context: {}\n{} → {}\nNo format conversion; notes, credentials and workspace stay in place.\n",
            self.source.context.display(), self.source.kind.directory(), self.target.directory());
        for file in &self.files {
            text.push_str(&format!(
                "{} · {} bytes · SHA256 {}\n",
                file.name, file.bytes, file.sha256
            ));
        }
        text
    }

    fn check(&self) -> Result<()> {
        ensure!(
            self.source.kind != self.target,
            "Invalid relocation direction"
        );
        self.source.check_current()?;
        let actual = inventory(&self.source.context, self.source.kind)?;
        ensure!(
            actual.len() == self.files.len(),
            "Service-file set changed; create a new preview"
        );
        for entry in &self.files {
            ensure!(
                FILES.contains(&entry.name.as_str()),
                "Unknown service file in plan"
            );
            ensure!(
                stamp(&self.source.path(&entry.name)?, &entry.name)? == *entry,
                "{} changed after preview; create a new preview",
                entry.name
            );
        }
        Ok(())
    }

    pub fn apply(&self, clients_updated_and_writers_paused: bool) -> Result<Receipt> {
        self.apply_internal(clients_updated_and_writers_paused, |_| Ok(()))
    }

    fn apply_internal(
        &self,
        clients_updated_and_writers_paused: bool,
        mut boundary: impl FnMut(&str) -> Result<()>,
    ) -> Result<Receipt> {
        ensure!(clients_updated_and_writers_paused, "Confirm that every used client supports this layout and competing metadata writers/Sync are paused");
        let _admission = okilum_write_authority::operation(&self.source.context)?;
        let _lock = ServiceLayout::lock(&self.source.context)?;
        self.check()?;
        let target = self.source.context.join(self.target.directory());
        if !regular_directory(&target)? {
            fs::create_dir(&target)?;
        }
        let receipt = Receipt {
            id: uuid::Uuid::new_v4().to_string(),
            plan: self.clone(),
            phase: Phase::Prepared,
            target_identity: identity(&target)?,
            output: Vec::new(),
        };
        let local = transaction(&self.source.context, &receipt.id)?;
        fs::create_dir(&local)?;
        for entry in &self.files {
            let bytes = regular_bytes(&self.source.path(&entry.name)?)?;
            ensure!(
                digest(&bytes) == entry.sha256,
                "Source changed before backup"
            );
            new_file(&local.join(&entry.name), &bytes)?;
        }
        persist(&local.join("journal.json"), &receipt)?;
        self.check()?;
        let hidden = self.source.context.join(".okilum");
        if !regular_directory(&hidden)? {
            fs::create_dir(&hidden)?;
        }
        new_file(&marker(&self.source.context), receipt.id.as_bytes())?;
        sync(&self.source.context)?;
        boundary("prepared")?;
        finish(receipt, boundary)
    }
}

fn load(context: &Path) -> Result<Receipt> {
    let id = String::from_utf8(regular_bytes(&marker(context))?)?;
    let local = transaction(context, &id)?;
    let receipt: Receipt = serde_json::from_slice(&regular_bytes(&local.join("journal.json"))?)?;
    ensure!(
        receipt.id == id && receipt.plan.source.context == context.canonicalize()?,
        "Migration journal belongs to a different context"
    );
    for entry in &receipt.plan.files {
        ensure!(
            FILES.contains(&entry.name.as_str()),
            "Unknown journal service file"
        );
        same_bytes(&local.join(&entry.name), entry)?;
    }
    Ok(receipt)
}

pub fn pending(context: &Path) -> Result<Option<Receipt>> {
    if !exists(&marker(context))? {
        return Ok(None);
    }
    load(context).map(Some)
}

/// Resume only known, exact transaction states. Unknown edits/copies are
/// preserved as explicit conflicts, never resolved by timestamp or rollback.
pub fn resume(context: &Path, clients_updated_and_writers_paused: bool) -> Result<Receipt> {
    ensure!(
        clients_updated_and_writers_paused,
        "Confirm compatible clients and paused writers before recovery"
    );
    let _admission = okilum_write_authority::operation(context)?;
    let _lock = ServiceLayout::lock(context)?;
    let receipt = load(context)?;
    if matches!(receipt.phase, Phase::RollingBack | Phase::RolledBack) {
        return restore(receipt);
    }
    finish(receipt, |_| Ok(()))
}

fn finish(mut receipt: Receipt, mut boundary: impl FnMut(&str) -> Result<()>) -> Result<Receipt> {
    let plan = &receipt.plan;
    let context = &plan.source.context;
    ensure!(
        identity(context)? == plan.source.context_identity,
        "Owning context identity changed"
    );
    let from = plan.source.directory();
    let to = context.join(plan.target.directory());
    ensure!(
        regular_directory(&from)? && identity(&from)? == plan.source.directory_identity,
        "Original service directory changed"
    );
    ensure!(
        regular_directory(&to)? && identity(&to)? == receipt.target_identity,
        "Target service directory changed"
    );
    if receipt.phase == Phase::Committed {
        for entry in &receipt.output {
            ensure!(
                stamp(&to.join(&entry.name), &entry.name)? == *entry,
                "Committed metadata changed; reconcile before recovery"
            );
        }
    }
    let staging = stage(&receipt);
    if !regular_directory(&staging)? {
        fs::create_dir(&staging)?;
    }
    for kind in [Kind::Hidden, Kind::Legacy] {
        for name in inventory(context, kind)? {
            ensure!(
                plan.files.iter().any(|entry| entry.name == name),
                "Unexpected service file {name}; reconcile before recovery"
            );
        }
    }
    let mut ordered = plan.files.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|entry| entry.name.starts_with("project."));
    for entry in ordered {
        let source = from.join(&entry.name);
        let staged = staging.join(&entry.name);
        let target = to.join(&entry.name);
        if exists(&source)? {
            ensure!(
                !exists(&staged)? && !exists(&target)?,
                "Competing copies of {}; reconcile explicitly",
                entry.name
            );
            ensure!(
                stamp(&source, &entry.name)? == *entry,
                "Source changed before relocation: {}",
                entry.name
            );
            fs::rename(&source, &staged)?;
            sync(&from)?;
            boundary("staged")?;
        }
        if exists(&staged)? {
            same_revision(&staged, entry)?;
        }
        if exists(&target)? {
            same_revision(&target, entry)?;
        } else {
            ensure!(
                exists(&staged)?,
                "Missing source/staging for {}",
                entry.name
            );
            // Both locations are inside one owning context. A no-clobber
            // hard link retains the original inode, ACLs, xattrs, permissions
            // and dates; unlike copying, it cannot widen per-file permissions.
            ensure!(
                stamp(&staged, &entry.name)? == *entry,
                "Staged revision changed; reconcile explicitly"
            );
            fs::hard_link(&staged, &target)
                .context("Service locations must support same-filesystem no-clobber relocation")?;
            File::open(&target)?.sync_all()?;
            sync(&to)?;
        }
        boundary("published")?;
    }
    for entry in &plan.files {
        ensure!(
            !exists(&from.join(&entry.name))?,
            "Source reappeared; migration conflict"
        );
        same_revision(&to.join(&entry.name), entry)?;
    }
    ensure!(
        inventory(context, plan.source.kind)?.is_empty(),
        "Legacy metadata changed during migration"
    );
    let generation = uuid::Uuid::new_v4().to_string();
    for directory in [&from, &to] {
        let path = directory.join(GENERATION);
        persist(&path, &generation)?;
    }
    receipt.output = plan
        .files
        .iter()
        .map(|e| stamp(&to.join(&e.name), &e.name))
        .collect::<Result<_>>()?;
    receipt.phase = Phase::Committed;
    persist(
        &transaction(context, &receipt.id)?.join("journal.json"),
        &receipt,
    )?;
    persist(
        &super::state_directory(context)?.join("latest.json"),
        &receipt.id,
    )?;
    boundary("committed")?;
    fs::remove_file(marker(context))?;
    sync(&context.join(".okilum"))?;
    cleanup(&receipt)?;
    Ok(receipt)
}

fn cleanup(receipt: &Receipt) -> Result<()> {
    let staging = stage(receipt);
    for entry in &receipt.plan.files {
        let path = staging.join(&entry.name);
        if exists(&path)? {
            same_bytes(&path, entry)?;
            fs::remove_file(path)?;
        }
    }
    if exists(&staging)? {
        fs::remove_dir(staging)?;
    }
    Ok(())
}

/// A completed migration can be rolled back only while its complete output is
/// unchanged. The reverse operation receives its own preview, backup and journal.
pub fn rollback_preview(receipt: &Receipt) -> Result<Plan> {
    ensure!(
        receipt.phase == Phase::Committed,
        "Only a committed migration has a rollback preview"
    );
    let plan = Plan::towards(&receipt.plan.source.context, receipt.plan.source.kind)?;
    ensure!(
        plan.files == receipt.output,
        "Service revisions changed; reconcile before rollback"
    );
    for original in &receipt.plan.files {
        let current = plan
            .files
            .iter()
            .find(|e| e.name == original.name)
            .context("Missing rollback file")?;
        ensure!(
            current.sha256 == original.sha256 && current.bytes == original.bytes,
            "{} changed after migration; rollback requires reconciliation",
            original.name
        );
    }
    Ok(plan)
}

/// Roll back an interrupted transaction without replacing any intervening edit.
pub fn cancel_pending(context: &Path, writers_paused: bool) -> Result<Receipt> {
    ensure!(
        writers_paused,
        "Confirm paused metadata writers before rollback"
    );
    let _admission = okilum_write_authority::operation(context)?;
    let _lock = ServiceLayout::lock(context)?;
    let mut receipt = load(context)?;
    receipt.phase = Phase::RollingBack;
    persist(
        &transaction(context, &receipt.id)?.join("journal.json"),
        &receipt,
    )?;
    restore(receipt)
}

fn restore(mut receipt: Receipt) -> Result<Receipt> {
    let context = &receipt.plan.source.context;
    let source = receipt.plan.source.directory();
    let target = context.join(receipt.plan.target.directory());
    ensure!(
        identity(context)? == receipt.plan.source.context_identity
            && regular_directory(&source)?
            && identity(&source)? == receipt.plan.source.directory_identity,
        "Original context/service directory changed; reconcile before rollback"
    );
    ensure!(
        regular_directory(&target)? && identity(&target)? == receipt.target_identity,
        "Target service directory changed; reconcile before rollback"
    );
    let staging = stage(&receipt);
    let local = transaction(context, &receipt.id)?;
    for entry in &receipt.plan.files {
        let original = source.join(&entry.name);
        let staged = staging.join(&entry.name);
        let moved = target.join(&entry.name);
        if exists(&original)? {
            same_revision(&original, entry)?;
        }
        if exists(&staged)? {
            same_revision(&staged, entry)?;
        }
        if exists(&moved)? {
            same_revision(&moved, entry)?;
        }
        if !exists(&original)? {
            if exists(&staged)? {
                fs::hard_link(&staged, &original)?;
            } else {
                ensure!(exists(&moved)?, "Service file disappeared from all transaction locations; inspect the retained backup explicitly");
                fs::hard_link(&moved, &original)?;
            }
            sync(&source)?;
        }
        if exists(&moved)? {
            let quarantine = target.join(format!(".rollback-{}-{}", receipt.id, entry.name));
            ensure!(
                !exists(&quarantine)?,
                "Rollback capture already exists; reconcile explicitly"
            );
            fs::rename(&moved, &quarantine)?;
            same_revision(&quarantine, entry)?;
            fs::remove_file(quarantine)?;
        }
    }
    cleanup(&receipt)?;
    for directory in [&source, &target] {
        persist(
            &directory.join(GENERATION),
            &uuid::Uuid::new_v4().to_string(),
        )?;
    }
    receipt.phase = Phase::RolledBack;
    persist(&local.join("journal.json"), &receipt)?;
    fs::remove_file(marker(context))?;
    sync(&context.join(".okilum"))?;
    Ok(receipt)
}

/// Private local receipt, never selected from a vault-provided arbitrary path.
pub fn receipt(context: &Path, id: &str) -> Result<Receipt> {
    let value: Receipt = serde_json::from_slice(&regular_bytes(
        &transaction(context, id)?.join("journal.json"),
    )?)?;
    ensure!(
        value.plan.source.context == context.canonicalize()? && value.id == id,
        "Receipt context mismatch"
    );
    Ok(value)
}

pub fn latest(context: &Path) -> Result<Option<Receipt>> {
    let path = super::state_directory(context)?.join("latest.json");
    if !exists(&path)? {
        return Ok(None);
    }
    let id: String = serde_json::from_slice(&regular_bytes(&path)?)?;
    receipt(context, &id).map(Some)
}

#[cfg(test)]
mod tests;
