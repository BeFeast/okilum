//! Durable content-addressed preimages and create/capture publication. Journals
//! live in private application state, never in a vault or its deletable index.
use super::*;
use crate::direct::inventory::tree::Stamp;
use rustix::fs::{mkdirat, openat, renameat_with, Mode, OFlags, RenameFlags};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const MAX_METADATA: usize = 32 * 1024 * 1024;
const MAX_TOTAL: usize = 2 * MAX_BYTES;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Blob {
    pub hash: String,
    pub size: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    pub scope: String,
    pub revision: String,
    pub files: BTreeMap<String, Blob>,
}
impl Image {
    fn from_snapshot(snapshot: &merge_preview::Snapshot) -> Self {
        Self {
            scope: snapshot.scope.clone(),
            revision: snapshot.revision.clone(),
            files: snapshot
                .files
                .iter()
                .map(|(path, bytes)| {
                    (
                        path.clone(),
                        Blob {
                            hash: wire::fingerprint(bytes),
                            size: bytes.len(),
                        },
                    )
                })
                .collect(),
        }
    }
    pub fn load(&self, store: &Store) -> Result<merge_preview::Snapshot> {
        Ok(merge_preview::Snapshot {
            scope: self.scope.clone(),
            revision: self.revision.clone(),
            files: self
                .files
                .iter()
                .map(|(path, blob)| Ok((path.clone(), store.blob(blob)?)))
                .collect::<Result<_>>()?,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Metadata {
    pub operation: Uuid,
    pub peer: Uuid,
    pub remote: Header,
    pub created: u64,
    pub here: Image,
    pub there: Image,
    pub merged: BTreeMap<String, Blob>,
}
impl Metadata {
    pub fn from_preview(operation: Uuid, preview: &Preview) -> Result<Self> {
        let (here, there) = preview.plan.originals();
        let target = merge_preview::Snapshot {
            scope: "merged".into(),
            revision: "merged".into(),
            files: preview.plan.merged_files().clone(),
        };
        Ok(Self {
            operation,
            peer: preview.peer,
            remote: preview.remote.clone(),
            created: now()?,
            here: Image::from_snapshot(here),
            there: Image::from_snapshot(there),
            merged: Image::from_snapshot(&target).files,
        })
    }
    pub fn encoded(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self)?;
        ensure!(
            bytes.len() <= MAX_METADATA,
            "first Sync plan exceeds journal bound"
        );
        Ok(bytes)
    }
    pub fn blobs(&self) -> Result<BTreeMap<String, Blob>> {
        ensure!(
            !self.operation.is_nil() && !self.peer.is_nil(),
            "merge identity missing"
        );
        let mut result = BTreeMap::new();
        let mut total = 0usize;
        for files in [&self.here.files, &self.there.files, &self.merged] {
            ensure!(
                !files.is_empty() && files.len() <= MAX_FILES,
                "invalid merge manifest bounds"
            );
            ensure!(
                files
                    .values()
                    .try_fold(0usize, |total, blob| total.checked_add(blob.size))
                    .is_some_and(|size| size <= MAX_BYTES),
                "merged inventory exceeds traversal bound"
            );
            for (path, blob) in files {
                tree::validate_path(path)?;
                ensure!(
                    !path
                        .split('/')
                        .scan(String::new(), |prefix, part| {
                            if !prefix.is_empty() {
                                prefix.push('/');
                            }
                            prefix.push_str(part);
                            Some(tree::excluded(prefix))
                        })
                        .any(|excluded| excluded),
                    "runtime metadata is not a canonical merge file"
                );
                valid_hash(&blob.hash)?;
                ensure!(blob.size <= MAX_BYTES, "merge blob exceeds bound");
                if let Some(old) = result.insert(blob.hash.clone(), blob.clone()) {
                    ensure!(old == *blob, "inconsistent content address");
                } else {
                    total = total
                        .checked_add(blob.size)
                        .context("merge size overflow")?;
                }
            }
        }
        ensure!(total <= MAX_TOTAL, "merge exceeds content bound");
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) enum Phase {
    Staging,
    Prepared,
    Publishing,
    Published,
    PeerComplete,
    Complete,
    Cancelled,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub schema: u32,
    pub operation: Uuid,
    pub peer: Uuid,
    pub source: ExistingVault,
    pub authority: okilum_write_authority::DurableBinding,
    pub remote_side: bool,
    pub revision: String,
    pub metadata_hash: String,
    pub metadata_size: usize,
    pub preview: Header,
    pub own_inventory: Header,
    pub phase: Phase,
}
pub(super) struct Store {
    pub directory: PathBuf,
    pub binding: Binding,
    metadata: std::sync::OnceLock<Arc<Metadata>>,
    blobs: std::sync::OnceLock<BTreeMap<String, Blob>>,
}
fn valid_hash(hash: &str) -> Result<()> {
    ensure!(
        hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid content address"
    );
    Ok(())
}
fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.nlink() == 1 && m.mode() & 0o077 == 0 && m.len() <= limit as u64,
        "unsafe first Sync journal file"
    );
    let mut bytes = Vec::new();
    (&mut f).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit && Stamp::from(m) == Stamp::from(f.metadata()?),
        "journal changed during read"
    );
    Ok(bytes)
}
impl Store {
    pub fn create(state: &Path, binding: Binding) -> Result<Self> {
        ensure!(
            binding.schema == 1
                && !binding.operation.is_nil()
                && binding.metadata_size <= MAX_METADATA,
            "invalid first Sync operation"
        );
        valid_hash(&binding.metadata_hash)?;
        disjoint(&binding.source.path, state)?;
        let directory = state.join(binding.operation.to_string());
        ensure!(
            !directory.try_exists()?,
            "first Sync operation already exists; recover it explicitly"
        );
        private::directory(&directory)?;
        private::directory(&directory.join("blobs"))?;
        let store = Self {
            directory,
            binding,
            metadata: Default::default(),
            blobs: Default::default(),
        };
        store.save()?;
        Ok(store)
    }
    pub fn open(state: &Path, operation: Uuid) -> Result<Self> {
        let directory = state.join(operation.to_string());
        std::fs::symlink_metadata(&directory)?;
        private::directory(&directory)?;
        let binding: Binding =
            serde_json::from_slice(&private::read(&directory.join("binding.json"))?)?;
        ensure!(
            binding.schema == 1 && binding.operation == operation,
            "first Sync journal identity changed"
        );
        binding.authority.open()?;
        Ok(Self {
            directory,
            binding,
            metadata: Default::default(),
            blobs: Default::default(),
        })
    }
    pub fn save(&self) -> Result<()> {
        private::write(
            &self.directory.join("binding.json"),
            &serde_json::to_vec(&self.binding)?,
        )
    }
    pub fn metadata(&self) -> Result<Arc<Metadata>> {
        if let Some(meta) = self.metadata.get() {
            return Ok(meta.clone());
        }
        let bytes = read(&self.directory.join("metadata"), MAX_METADATA)?;
        ensure!(
            bytes.len() == self.binding.metadata_size
                && wire::fingerprint(&bytes) == self.binding.metadata_hash,
            "first Sync plan checksum differs"
        );
        let meta: Metadata = serde_json::from_slice(&bytes)?;
        ensure!(
            meta.operation == self.binding.operation && meta.peer == self.binding.peer,
            "first Sync plan belongs to another operation or computer"
        );
        meta.blobs()?;
        let meta = Arc::new(meta);
        let _ = self.metadata.set(meta.clone());
        Ok(meta)
    }
    fn blobs(&self) -> Result<&BTreeMap<String, Blob>> {
        if self.blobs.get().is_none() {
            let _ = self.blobs.set(self.metadata()?.blobs()?);
        }
        Ok(self.blobs.get().unwrap())
    }
    pub fn chunk(&mut self, blob: Option<&str>, offset: usize, bytes: &[u8]) -> Result<usize> {
        ensure!(
            self.binding.phase == Phase::Staging && bytes.len() <= CHUNK_BYTES,
            "first Sync staging is closed or chunk exceeds bound"
        );
        let (path, expected_size, expected_hash) = if let Some(hash) = blob {
            valid_hash(hash)?;
            let blobs = self.blobs()?;
            let entry = blobs
                .get(hash)
                .context("content absent from consented plan")?;
            (
                self.directory.join("blobs").join(hash),
                entry.size,
                hash.to_owned(),
            )
        } else {
            (
                self.directory.join("metadata"),
                self.binding.metadata_size,
                self.binding.metadata_hash.clone(),
            )
        };
        ensure!(
            offset <= expected_size && bytes.len() <= expected_size - offset,
            "first Sync chunk outside manifest"
        );
        if path.try_exists()? {
            let complete = read(&path, expected_size)?;
            ensure!(
                complete.len() == expected_size
                    && wire::fingerprint(&complete) == expected_hash
                    && complete.get(offset..offset + bytes.len()) == Some(bytes),
                "replayed first Sync chunk differs"
            );
            return Ok(expected_size);
        }
        let partial = path.with_extension("partial");
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&partial)?;
        let m = f.metadata()?;
        ensure!(
            m.is_file()
                && m.nlink() == 1
                && m.mode() & 0o077 == 0
                && m.len() <= expected_size as u64,
            "unsafe partial first Sync blob"
        );
        let length = usize::try_from(m.len())?;
        ensure!(offset <= length, "first Sync chunk skipped bytes");
        if offset < length {
            ensure!(
                offset + bytes.len() <= length,
                "overlapping first Sync chunk"
            );
            f.seek(SeekFrom::Start(offset as u64))?;
            let mut existing = vec![0; bytes.len()];
            f.read_exact(&mut existing)?;
            ensure!(existing == bytes, "replayed first Sync content differs");
        } else {
            f.seek(SeekFrom::End(0))?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        let length = usize::try_from(f.metadata()?.len())?;
        if length == expected_size {
            f.seek(SeekFrom::Start(0))?;
            let mut complete = Vec::new();
            f.read_to_end(&mut complete)?;
            ensure!(
                wire::fingerprint(&complete) == expected_hash,
                "first Sync staged content checksum differs"
            );
            renameat_with(
                rustix::fs::CWD,
                &partial,
                rustix::fs::CWD,
                &path,
                RenameFlags::NOREPLACE,
            )?;
            File::open(path.parent().context("blob parent missing")?)?.sync_all()?;
        }
        Ok(length)
    }
    pub fn put(&mut self, hash: Option<&str>, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            self.chunk(hash, 0, bytes)?;
        }
        for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            self.chunk(hash, index * CHUNK_BYTES, chunk)?;
        }
        Ok(())
    }
    fn blob(&self, blob: &Blob) -> Result<Vec<u8>> {
        let bytes = read(&self.directory.join("blobs").join(&blob.hash), blob.size)?;
        ensure!(
            bytes.len() == blob.size && wire::fingerprint(&bytes) == blob.hash,
            "first Sync preimage checksum differs"
        );
        Ok(bytes)
    }
    pub fn plan(&self) -> Result<merge_preview::Plan> {
        let meta = self.metadata()?;
        let here = meta.here.load(self)?;
        let there = meta.there.load(self)?;
        let plan = merge_preview::Plan::preview(here, there)
            .map_err(|e| anyhow::anyhow!("invalid first Sync plan: {e:?}"))?;
        let merged: BTreeMap<_, _> = plan
            .merged_files()
            .iter()
            .map(|(path, bytes)| {
                (
                    path.clone(),
                    Blob {
                        hash: wire::fingerprint(bytes),
                        size: bytes.len(),
                    },
                )
            })
            .collect();
        ensure!(
            merged == meta.merged,
            "staged union differs from consented first Sync plan"
        );
        Ok(plan)
    }
    pub fn prepare(&mut self) -> Result<()> {
        self.plan()?;
        ensure!(
            self.binding.phase == Phase::Staging,
            "first Sync already prepared"
        );
        self.binding.phase = Phase::Prepared;
        self.save()
    }
    pub fn compatible(&self, recovering: bool) -> Result<Tree> {
        let root = tree::open_root(&self.binding.source.path)?;
        self.binding.authority.open()?;
        let current = Tree::collect(&root, &self.binding.source.path, true)?;
        let meta = self.metadata()?;
        let original = if self.binding.remote_side {
            meta.there.load(self)?
        } else {
            meta.here.load(self)?
        };
        if recovering {
            for (path, bytes) in &original.files {
                if !current.files.contains_key(path) {
                    let (parent, _) = parent(&root, path, false)?;
                    let retained = self.retained(path);
                    let mut file = File::from(openat(
                        &parent,
                        retained.as_str(),
                        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                        Mode::empty(),
                    )?);
                    let m = file.metadata()?;
                    ensure!(
                        m.is_file() && m.nlink() == 1 && m.len() <= MAX_BYTES as u64,
                        "unsafe retained first Sync preimage"
                    );
                    let mut captured = Vec::new();
                    (&mut file)
                        .take(MAX_BYTES as u64 + 1)
                        .read_to_end(&mut captured)?;
                    ensure!(
                        captured == *bytes,
                        "third version in retained first Sync preimage; recovery stopped"
                    );
                }
            }
            ensure!(
                current
                    .files
                    .iter()
                    .all(|(p, b)| original.files.get(p) == Some(b)
                        || meta.merged.get(p).is_some_and(|blob| blob.hash
                            == wire::fingerprint(b)
                            && blob.size == b.len())),
                "third version or missing original stops first Sync recovery"
            );
        } else {
            ensure!(
                current.files == original.files
                    && digest(&(&current.stamps, manifest(&current)))? == self.binding.revision,
                "files changed; refresh first Sync preview"
            );
        }
        Ok(current)
    }
    /// `validate` must prove local AND transport exclusion for this exact root.
    /// The retained preimages and displaced inodes survive every failure.
    pub fn publish(&mut self, validate: impl Fn() -> Result<()>) -> Result<()> {
        ensure!(
            matches!(
                self.binding.phase,
                Phase::Prepared | Phase::Publishing | Phase::Published
            ),
            "first Sync not prepared"
        );
        validate()?;
        self.compatible(self.binding.phase != Phase::Prepared)?;
        self.binding.phase = Phase::Publishing;
        self.save()?;
        let plan = self.plan()?;
        let root = tree::open_root(&self.binding.source.path)?;
        for (path, bytes) in plan.merged_files() {
            validate()?;
            tree::verify_root(&root, &self.binding.source.path)?;
            let (parent, name) = parent(&root, path, true)?;
            let existing = openat(
                &parent,
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            );
            let mut mode = 0o600;
            if let Ok(fd) = existing {
                let mut f = File::from(fd);
                let m = f.metadata()?;
                ensure!(
                    m.is_file() && m.nlink() == 1 && m.len() <= MAX_BYTES as u64,
                    "unsafe first Sync destination"
                );
                let mut current = Vec::new();
                (&mut f)
                    .take(MAX_BYTES as u64 + 1)
                    .read_to_end(&mut current)?;
                if current == *bytes {
                    continue;
                }
                let original = if self.binding.remote_side {
                    &plan.originals().1.files
                } else {
                    &plan.originals().0.files
                };
                ensure!(
                    original.get(path) == Some(&current),
                    "third version stops first Sync publication"
                );
                mode = m.mode() & 0o777;
                ensure!(
                    Stamp::from(m.clone()) == Stamp::from(f.metadata()?),
                    "source changed during first Sync read"
                );
                let retained = self.retained(path);
                renameat_with(
                    &parent,
                    name.as_str(),
                    &parent,
                    retained.as_str(),
                    RenameFlags::NOREPLACE,
                )?;
                parent.sync_all()?;
                // Verify the captured inode, not a new lookup at the canonical
                // name. An external writer cannot be silently overwritten.
                let held = File::from(openat(
                    &parent,
                    retained.as_str(),
                    OFlags::RDONLY | OFlags::NOFOLLOW,
                    Mode::empty(),
                )?);
                let mut captured = Vec::new();
                held.take(MAX_BYTES as u64 + 1).read_to_end(&mut captured)?;
                ensure!(
                    captured == current,
                    "writer changed captured first Sync preimage; recovery required"
                );
            } else if let Err(error) = existing {
                ensure!(
                    error == rustix::io::Errno::NOENT,
                    "cannot inspect first Sync destination: {error}"
                );
            }
            let stage = format!(".okilum-save-first-sync-stage-{}", Uuid::new_v4());
            let mut f = File::from(openat(
                &parent,
                stage.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
                Mode::from_raw_mode(mode),
            )?);
            f.write_all(bytes)?;
            f.set_permissions(std::fs::Permissions::from_mode(mode))?;
            f.sync_all()?;
            renameat_with(
                &parent,
                stage.as_str(),
                &parent,
                name.as_str(),
                RenameFlags::NOREPLACE,
            )?;
            parent.sync_all()?;
        }
        validate()?;
        let current = Tree::collect(&root, &self.binding.source.path, true)?;
        ensure!(
            current.files == *plan.merged_files(),
            "first Sync readback differs; recovery required"
        );
        self.binding.phase = Phase::Published;
        self.save()
    }
    fn retained(&self, path: &str) -> String {
        format!(
            ".okilum-save-first-sync-{}-{}",
            self.binding.operation,
            wire::fingerprint(path.as_bytes())
        )
    }
}
fn parent(root: &File, path: &str, create: bool) -> Result<(File, String)> {
    tree::validate_path(path)?;
    let mut parts = path.split('/').peekable();
    let mut directory = root.try_clone()?;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return Ok((directory, part.to_owned()));
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = match openat(&directory, part, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) if create => {
                mkdirat(&directory, part, Mode::from_raw_mode(0o700))?;
                directory.sync_all()?;
                openat(&directory, part, flags, Mode::empty())?
            }
            Err(error) => return Err(error.into()),
        };
        directory = File::from(fd);
    }
    anyhow::bail!("empty merge file path")
}
