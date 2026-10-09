//! Disposable Reader snapshots. Canonical notes remain the only source of truth.
use super::*;
use anyhow::{bail, ensure};
pub mod incremental;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::collections::BTreeMap;
use std::io::{Read, Write};
#[cfg(any(not(windows), test))]
use std::time::UNIX_EPOCH;

// v2 excludes service sidecars and invalid UTF-8 sources from derived data.
const SCHEMA: u32 = 2;
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceRevision {
    size: u64,
    modified: u128,
    device: u64,
    inode: u64,
    changed: i128,
}
impl SourceRevision {
    /// Accepted source mtime for derived recency ordering, without additional IO.
    pub fn modified_nanoseconds(&self) -> u128 {
        self.modified
    }

    /// Imported mtime can be rounded even on a precise native filesystem. Native
    /// change time also changes on same-size writes with a restored mtime. Reuse still
    /// requires equality of the entire revision, never just either timestamp.
    pub fn is_precise(&self) -> bool {
        !self.modified.is_multiple_of(1_000_000_000) || self.precise_change_time()
    }

    fn precise_change_time(&self) -> bool {
        cfg!(any(unix, windows)) && self.changed.rem_euclid(1_000_000_000) != 0
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            Self::read_windows(path)
        }
        #[cfg(not(windows))]
        {
            let meta = std::fs::symlink_metadata(path)?;
            if !meta.is_file() {
                return Err(std::io::Error::other("Not a regular Markdown file"));
            }
            let modified = meta
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_err(std::io::Error::other)?
                .as_nanos();
            #[cfg(unix)]
            let (device, inode, changed) = {
                use std::os::unix::fs::MetadataExt;
                (
                    meta.dev(),
                    meta.ino(),
                    meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128,
                )
            };
            #[cfg(not(unix))]
            let (device, inode, changed) = (0, 0, 0);
            Ok(Self {
                size: meta.len(),
                modified,
                device,
                inode,
                changed,
            })
        }
    }

    #[cfg(windows)]
    fn read_windows(path: &Path) -> std::io::Result<Self> {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            FileBasicInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let file = std::fs::File::options()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("Not a regular Markdown file"));
        }
        let handle = file.as_raw_handle();
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let mut basic = FILE_BASIC_INFO::default();
        // Both observations refer to this owned, metadata-only handle. Opening
        // does not follow symlinks, read canonical bytes, or deny another writer.
        let ok = unsafe {
            GetFileInformationByHandle(handle, &mut info) != 0
                && GetFileInformationByHandleEx(
                    handle,
                    FileBasicInfo,
                    (&mut basic as *mut FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                ) != 0
        };
        if !ok {
            // Unsupported/denied native metadata is uncertainty, never proof of
            // reuse. Reconciliation still reads the source and leaves no stamp.
            return Err(std::io::Error::last_os_error());
        }
        let modified = basic.LastWriteTime as i128 - 116_444_736_000_000_000;
        let modified = u128::try_from(modified).map_err(std::io::Error::other)? * 100;
        Ok(Self {
            size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
            modified,
            device: u64::from(info.dwVolumeSerialNumber),
            inode: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            changed: i128::from(basic.ChangeTime) * 100,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Source {
    stamp: Option<SourceRevision>,
    // JSON arrays of bytes inflate a large vault; base64 also preserves invalid UTF-8.
    bytes: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    schema: u32,
    root: PathBuf,
    pub id: String,
    entries: Vec<VaultEntry>,
    links: HashMap<String, Vec<Backlink>>,
    sources: BTreeMap<String, Source>,
    #[serde(default)]
    unreadable: Vec<CachedUnreadable>,
    /// Basename of an immutable, completed Tantivy generation, never a path.
    pub search_generation: Option<String>,
    /// Set while every field except `search_generation` equals a source bank
    /// file on disk. Derived bookkeeping only; never serialised.
    #[serde(skip)]
    persisted: Option<Persisted>,
}

/// The source bank file this in-memory snapshot was decoded from, unmodified.
#[derive(Clone)]
struct Persisted {
    path: PathBuf,
    file: SourceRevision,
    snapshot_id: String,
    search_generation: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct CachedUnreadable {
    path: PathBuf,
    operation: String,
    error: String,
}

/// Small first-frame cache: inventory, graph and only the last displayed source.
/// The full source bank is loaded after publication for reconciliation.
#[derive(Serialize, Deserialize)]
pub struct StartupSnapshot {
    schema: u32,
    root: PathBuf,
    entries: Vec<VaultEntry>,
    // Sorted so an unchanged manifest serialises to identical bytes.
    #[serde(serialize_with = "sorted_links")]
    links: HashMap<String, Vec<Backlink>>,
    unreadable: Vec<CachedUnreadable>,
    primary: Option<(String, Source)>,
    #[serde(default)]
    source_id: Option<String>,
    pub search_generation: Option<String>,
    #[serde(skip)]
    previous: Option<Box<Snapshot>>,
    #[serde(skip)]
    latest: Option<(String, Source)>,
}

fn sorted_links<S: serde::Serializer>(
    links: &HashMap<String, Vec<Backlink>>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_map(links.iter().collect::<BTreeMap<_, _>>())
}

#[derive(Serialize, Deserialize)]
struct PrimarySnapshot {
    schema: u32,
    root: PathBuf,
    primary: (String, Source),
}

/// Called by the existing history worker after navigation. This small file is
/// independent of reconcile publication, which may finish after a newer open.
pub fn remember_primary(base: &Path, root: &Path, path: &str) -> Result<()> {
    ensure!(relative(path), "Invalid cached primary path");
    let root = root.canonicalize()?;
    let file_path = root.join(path);
    ensure!(
        file_path.canonicalize()?.starts_with(&root),
        "Primary escaped its vault"
    );
    let before = SourceRevision::read(&file_path)?;
    ensure!(before.size <= MAX_BYTES, "Primary exceeds cache size limit");
    let mut bytes = Vec::new();
    std::fs::File::open(&file_path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "Primary exceeds cache size limit"
    );
    let raw = String::from_utf8(bytes)?;
    ensure!(
        SourceRevision::read(&file_path)? == before,
        "Primary changed during cache read"
    );
    let snapshot = PrimarySnapshot {
        schema: SCHEMA,
        root,
        primary: (
            path.to_owned(),
            Source {
                stamp: Some(before),
                bytes: STANDARD.encode(raw),
            },
        ),
    };
    std::fs::create_dir_all(base)?;
    let mut file = tempfile::NamedTempFile::new_in(base)?;
    write_json(file.as_file_mut(), &snapshot)?;
    ensure!(
        file.as_file().metadata()?.len() <= MAX_BYTES,
        "Primary cache exceeds size limit"
    );
    file.flush()?;
    file.persist(base.join("reader-primary.json"))?;
    Ok(())
}

fn latest_primary(base: &Path, root: &Path) -> Option<(String, Source)> {
    let path = base.join("reader-primary.json");
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let snapshot: PrimarySnapshot = serde_json::from_slice(&bytes).ok()?;
    if snapshot.schema != SCHEMA || snapshot.root != root || !relative(&snapshot.primary.0) {
        return None;
    }
    String::from_utf8(STANDARD.decode(&snapshot.primary.1.bytes).ok()?).ok()?;
    Some(snapshot.primary)
}

fn cached_vault(
    root: &Path,
    entries: &[VaultEntry],
    links: &HashMap<String, Vec<Backlink>>,
    unreadable: &[CachedUnreadable],
) -> Vault {
    let mut vault = Vault::from_note_paths(
        entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::Markdown)
            .map(|entry| entry.path.clone()),
    );
    vault.root = root.to_path_buf();
    vault.entries = entries.to_vec();
    vault.backlink_map = links.clone();
    vault.unreadable = unreadable
        .iter()
        .map(|item| UnreadableEntry {
            path: item.path.clone(),
            operation: "previous scan",
            error: format!("{}: {}", item.operation, item.error),
        })
        .collect();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Attachment)
    {
        let path = Path::new(&entry.path);
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        vault
            .asset_map
            .entry(name)
            .or_insert_with(|| root.join(path));
    }
    vault.inventory_scanned = true;
    vault
}

fn validate_inventory(
    root: &Path,
    entries: &[VaultEntry],
    links: &HashMap<String, Vec<Backlink>>,
    unreadable: &[CachedUnreadable],
    generation: Option<&str>,
) -> Result<()> {
    for entry in entries {
        ensure!(
            relative(&entry.path),
            "Invalid inventory entry path {:?}",
            entry.path
        );
    }
    for (target, incoming) in links {
        ensure!(
            relative(target),
            "Invalid backlink destination path {target:?}"
        );
        for link in incoming {
            ensure!(
                relative(&link.path),
                "Invalid backlink source path {:?}",
                link.path
            );
        }
    }
    for item in unreadable {
        ensure!(
            item.path.strip_prefix(root).is_ok_and(|path| {
                path.components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
            }),
            "Invalid unreadable entry path {}",
            item.path.display()
        );
    }
    ensure!(
        generation.is_none_or(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())),
        "Invalid search generation {generation:?}"
    );
    Ok(())
}

// Serializing directly into File makes each JSON token a syscall. The source
// bank and graph can contain millions of tokens; always propagate final flush.
fn write_json(file: &mut std::fs::File, value: &impl Serialize) -> Result<()> {
    let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
    serde_json::to_writer(&mut writer, value)?;
    writer.flush()?;
    Ok(())
}

impl StartupSnapshot {
    pub fn load(base: &Path, root: &Path) -> Result<Self> {
        let path = base.join("reader-startup.json");
        if !path.exists() {
            // One-launch migration for caches created before the small manifest.
            let snapshot = Snapshot::load_checked(base, root)
                .context("No usable persisted Reader snapshot")?;
            return Ok(Self {
                schema: SCHEMA,
                root: snapshot.root.clone(),
                entries: snapshot.entries.clone(),
                links: snapshot.links.clone(),
                unreadable: snapshot.unreadable.clone(),
                primary: None,
                source_id: Some(snapshot.id.clone()),
                search_generation: snapshot.search_generation.clone(),
                latest: latest_primary(base, &snapshot.root),
                previous: Some(Box::new(snapshot)),
            });
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_BYTES,
            "Invalid Reader startup cache size/type"
        );
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Reader startup cache exceeds size limit"
        );
        let mut snapshot: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            snapshot.schema == SCHEMA && snapshot.root == root.canonicalize()?,
            "Reader startup cache schema/root mismatch"
        );
        let delta = incremental::Delta::load(base, &snapshot.root, snapshot.source_id.as_deref())?;
        if let Some(delta) = &delta {
            delta.apply_startup(&mut snapshot);
        }
        validate_inventory(
            &snapshot.root,
            &snapshot.entries,
            &snapshot.links,
            &snapshot.unreadable,
            snapshot.search_generation.as_deref(),
        )
        .context("Invalid Reader startup inventory")?;
        if let Some((path, source)) = &snapshot.primary {
            ensure!(
                relative(path)
                    && snapshot
                        .entries
                        .iter()
                        .any(|e| e.kind == EntryKind::Markdown && &e.path == path),
                "Invalid Reader startup primary identity"
            );
            String::from_utf8(STANDARD.decode(&source.bytes)?)?;
        }
        snapshot.latest = latest_primary(base, &snapshot.root);
        if let Some(delta) = &delta {
            delta.apply_startup(&mut snapshot);
        }
        Ok(snapshot)
    }

    pub fn vault(&self) -> Vault {
        cached_vault(&self.root, &self.entries, &self.links, &self.unreadable)
    }

    pub fn source(&self, path: &str) -> Option<String> {
        self.latest
            .as_ref()
            .filter(|(p, _)| p == path)
            .or_else(|| self.primary.as_ref().filter(|(p, _)| p == path))
            .and_then(|(_, source)| String::from_utf8(STANDARD.decode(&source.bytes).ok()?).ok())
            .or_else(|| self.previous.as_ref()?.source(path))
    }

    pub fn into_previous(self) -> Option<Box<Snapshot>> {
        self.previous
    }
}

#[derive(Default, Debug)]
pub struct ReconcileStats {
    pub read: usize,
    pub reused: usize,
    pub reuse: ReuseDiagnostics,
    /// True when the previous graph was retained, including bounded refreshes.
    pub graph_reused: bool,
    /// Bounded graph/search batch against the retained baseline. Empty on a full
    /// rebuild; callers must first check `graph_reused`.
    pub affected: std::collections::BTreeSet<String>,
}

/// Counts only; no source contents or additional filesystem probes.
#[derive(Default, Debug, Serialize)]
pub struct ReuseDiagnostics {
    pub forced: usize,
    pub missing_source: usize,
    pub invalidated_revision: usize,
    pub unavailable_metadata: usize,
    pub imprecise_revision: usize,
    pub changed_revision: usize,
    pub coarse_mtime: usize,
    pub precise_ctime: usize,
    pub mismatch: RevisionMismatches,
}

#[derive(Default, Debug, Serialize)]
pub struct RevisionMismatches {
    pub size: usize,
    pub modified: usize,
    pub device: usize,
    pub inode: usize,
    pub changed: usize,
}

impl ReuseDiagnostics {
    fn needs_read(
        &mut self,
        before: Option<&SourceRevision>,
        old: Option<&Source>,
        force: bool,
    ) -> bool {
        if let Some(stamp) = before {
            self.coarse_mtime += usize::from(stamp.modified.is_multiple_of(1_000_000_000));
            self.precise_ctime += usize::from(stamp.precise_change_time());
        }
        let counter = if force {
            &mut self.forced
        } else if old.is_none() {
            &mut self.missing_source
        } else if old.is_some_and(|source| source.stamp.is_none()) {
            &mut self.invalidated_revision
        } else if before.is_none() {
            &mut self.unavailable_metadata
        } else if let (Some(before), Some(saved)) =
            (before, old.and_then(|source| source.stamp.as_ref()))
        {
            if before != saved {
                self.mismatch.size += usize::from(before.size != saved.size);
                self.mismatch.modified += usize::from(before.modified != saved.modified);
                self.mismatch.device += usize::from(before.device != saved.device);
                self.mismatch.inode += usize::from(before.inode != saved.inode);
                self.mismatch.changed += usize::from(before.changed != saved.changed);
                &mut self.changed_revision
            } else if !before.is_precise() {
                &mut self.imprecise_revision
            } else {
                return false;
            }
        } else {
            unreachable!("metadata and saved revision checked above")
        };
        *counter += 1;
        true
    }
}

fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && (!cfg!(windows) || (!path.contains(':') && !path.contains('\\')))
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        && !super::service_path(Path::new(path))
}

impl Snapshot {
    /// Prepared outgoing identities, including ambiguous/property/Markdown
    /// targets. Scheduling this set never reads canonical files.
    pub fn linked_paths_from(&self, source: &str) -> Vec<String> {
        let mut targets: Vec<_> = self
            .links
            .iter()
            .filter(|(_, incoming)| incoming.iter().any(|link| link.path == source))
            .map(|(path, _)| path.clone())
            .collect();
        targets.sort();
        targets
    }

    pub fn load(base: &Path, root: &Path) -> Option<Self> {
        Self::load_checked(base, root).ok()
    }

    pub fn load_checked(base: &Path, root: &Path) -> Result<Self> {
        let path = base.join("reader-snapshot.json");
        let meta = std::fs::symlink_metadata(&path).context("Inspect Reader source bank")?;
        ensure!(
            meta.is_file() && meta.len() <= MAX_BYTES,
            "Invalid source bank size/type"
        );
        let before = SourceRevision::read(&path).ok();
        let mut bytes = Vec::new();
        std::fs::File::open(&path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        // A file replaced during the read is decoded but never trusted as persisted.
        let file =
            before.filter(|before| SourceRevision::read(&path).ok().as_ref() == Some(before));
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Reader source bank exceeds size limit"
        );
        let mut snapshot: Self =
            serde_json::from_slice(&bytes).context("Decode Reader source bank")?;
        ensure!(
            snapshot.schema == SCHEMA,
            "Reader source bank schema mismatch"
        );
        ensure!(
            snapshot.root == root.canonicalize()?,
            "Reader source bank root mismatch"
        );
        if let Some(delta) = incremental::Delta::load(base, &snapshot.root, Some(&snapshot.id))? {
            delta.apply_snapshot(&mut snapshot);
        } else if let Some(file) = file {
            snapshot.persisted = Some(Persisted {
                path,
                file,
                snapshot_id: snapshot.id.clone(),
                search_generation: snapshot.search_generation.clone(),
            });
        }
        validate_inventory(
            &snapshot.root,
            &snapshot.entries,
            &snapshot.links,
            &snapshot.unreadable,
            snapshot.search_generation.as_deref(),
        )
        .context("Invalid Reader source inventory")?;
        let notes: std::collections::HashSet<_> = snapshot
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Markdown)
            .map(|e| &e.path)
            .collect();
        for path in &notes {
            ensure!(
                snapshot.sources.contains_key(*path)
                    || snapshot
                        .unreadable
                        .iter()
                        .any(|item| item.path == snapshot.root.join(path)),
                "Missing cached source or unreadable entry for {path:?}"
            );
        }
        for (path, source) in &snapshot.sources {
            ensure!(
                relative(path) && notes.contains(path),
                "Invalid cached source identity {path:?}"
            );
            STANDARD
                .decode(&source.bytes)
                .with_context(|| format!("Decode cached source {path:?}"))?;
        }
        Ok(snapshot)
    }

    pub fn vault(&self) -> Vault {
        cached_vault(&self.root, &self.entries, &self.links, &self.unreadable)
    }

    pub fn invalidate_paths(&mut self, paths: &[String]) {
        for (path, source) in &mut self.sources {
            if paths.iter().any(|dirty| {
                dirty.is_empty() || path == dirty || path.starts_with(&format!("{dirty}/"))
            }) {
                source.stamp = None;
                self.persisted = None;
            }
        }
    }

    /// True only when `base` already holds exactly this snapshot: same decoded
    /// fields, same search generation, and the file is unchanged since load.
    pub fn is_persisted_in(&self, base: &Path) -> bool {
        self.persisted.as_ref().is_some_and(|persisted| {
            persisted.snapshot_id == self.id
                && persisted.search_generation == self.search_generation
                && persisted.path == base.join("reader-snapshot.json")
                && SourceRevision::read(&persisted.path).is_ok_and(|now| now == persisted.file)
        })
    }

    pub fn source_revision(&self, path: &str) -> Option<&SourceRevision> {
        self.sources.get(path)?.stamp.as_ref()
    }

    pub fn source_paths(&self) -> impl Iterator<Item = &str> {
        self.sources.keys().map(String::as_str)
    }

    pub fn sources(&self) -> Result<HashMap<String, Vec<u8>>> {
        self.sources
            .iter()
            .map(|(path, source)| Ok((path.clone(), STANDARD.decode(&source.bytes)?)))
            .collect()
    }

    /// Decode only the selected primary note for the first usable Reader frame.
    pub fn source(&self, path: &str) -> Option<String> {
        String::from_utf8(STANDARD.decode(&self.sources.get(path)?.bytes).ok()?).ok()
    }

    pub fn save(&self, base: &Path) -> Result<()> {
        // Defense in depth for callers outside Reader.
        let ancestor = base
            .ancestors()
            .find(|p| p.exists())
            .context("Cache has no ancestor")?;
        let resolved = ancestor.canonicalize()?.join(base.strip_prefix(ancestor)?);
        ensure!(
            !resolved.starts_with(&self.root),
            "Derived cache must be outside vault"
        );
        std::fs::create_dir_all(base)?;
        let mut file = tempfile::NamedTempFile::new_in(base)?;
        write_json(file.as_file_mut(), self)?;
        ensure!(
            file.as_file().metadata()?.len() <= MAX_BYTES,
            "Snapshot exceeds size limit"
        );
        file.flush()?;
        file.persist(base.join("reader-snapshot.json"))?;
        Ok(())
    }
}

/// Walk cheap metadata; only changed notes are opened. Graph parsing is skipped
/// entirely when both identities and source revisions match the saved snapshot.
pub fn reconcile(
    root: &Path,
    previous: Option<&Snapshot>,
    force_read: bool,
    checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
) -> Result<(Vault, Snapshot, ReconcileStats)> {
    reconcile_with_reader(root, previous, force_read, checkpoint, &mut |path| {
        super::read_source(path).map(String::into_bytes)
    })
}

/// Reconcile with a caller-owned source reader, retaining per-entry I/O failures.
pub fn reconcile_with_reader(
    root: &Path,
    previous: Option<&Snapshot>,
    force_read: bool,
    checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
) -> Result<(Vault, Snapshot, ReconcileStats)> {
    let (canonical, previous, mut vault) = begin_reconcile(root, previous, checkpoint)?;
    let mut bank = CheckedSources::default();
    for (count, note) in vault.notes.iter().enumerate() {
        checkpoint("Checking notes", count)?;
        bank.insert(
            &note.path,
            check_source(root, &note.path, previous, force_read, read),
        );
    }
    vault.unreadable.extend(bank.unreadable);
    finish_reconcile(
        canonical,
        vault,
        previous,
        bank.sources,
        bank.stats,
        force_read,
        checkpoint,
    )
}

/// Bound simultaneous metadata/source I/O without sharing mutable Reader state.
pub const SOURCE_READ_WORKERS: usize = 8;

pub fn reconcile_parallel(
    root: &Path,
    previous: Option<&Snapshot>,
    force_read: bool,
    checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
) -> Result<(Vault, Snapshot, ReconcileStats)> {
    reconcile_parallel_with_reader(root, previous, force_read, checkpoint, &|path| {
        std::fs::read(path)
    })
}

/// The coordinator alone calls progress/cancellation; at most eight files are
/// checked concurrently and at most sixteen completed sources wait in memory.
pub fn reconcile_parallel_with_reader(
    root: &Path,
    previous: Option<&Snapshot>,
    force_read: bool,
    checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    read: &(impl Fn(&Path) -> std::io::Result<Vec<u8>> + Sync),
) -> Result<(Vault, Snapshot, ReconcileStats)> {
    reconcile_parallel_prioritized_with_reader(root, previous, force_read, &[], checkpoint, read)
}

pub fn reconcile_parallel_prioritized(
    root: &Path,
    previous: Option<&Snapshot>,
    force_read: bool,
    priority: &[String],
    checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
) -> Result<(Vault, Snapshot, ReconcileStats)> {
    reconcile_parallel_prioritized_with_reader(
        root,
        previous,
        force_read,
        priority,
        checkpoint,
        &|path| std::fs::read(path),
    )
}

fn reconcile_parallel_prioritized_with_reader(
    root: &Path,
    previous: Option<&Snapshot>,
    force_read: bool,
    priority: &[String],
    checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    read: &(impl Fn(&Path) -> std::io::Result<Vec<u8>> + Sync),
) -> Result<(Vault, Snapshot, ReconcileStats)> {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };
    let (canonical, previous, mut vault) = begin_reconcile(root, previous, checkpoint)?;
    let mut work: Vec<_> = vault.notes.iter().collect();
    work.sort_by_key(|note| {
        priority
            .iter()
            .position(|path| {
                path == &note.path
                    || (path == "/" && !note.path.contains('/'))
                    || (path.ends_with('/') && note.path.starts_with(path))
            })
            .unwrap_or(priority.len())
    });
    let notes = &work;
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let mut bank = CheckedSources::default();
    std::thread::scope(|scope| -> Result<()> {
        let (send, receive) = mpsc::sync_channel(SOURCE_READ_WORKERS * 2);
        for _ in 0..SOURCE_READ_WORKERS.min(notes.len()) {
            let send = send.clone();
            let next = &next;
            let stopped = &stopped;
            scope.spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    let Some(note) = notes.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let result =
                        check_source(root, &note.path, previous, force_read, &mut |p| read(p));
                    if send.send((note.path.clone(), result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(send);
        let result = (|| -> Result<()> {
            let mut completed = 0;
            while completed < notes.len() {
                checkpoint("Checking notes", completed)?;
                match receive.recv_timeout(std::time::Duration::from_millis(20)) {
                    Ok((path, result)) => {
                        bank.insert(&path, result);
                        completed += 1;
                        if completed.is_multiple_of(64) {
                            std::thread::yield_now();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        bail!("Source workers stopped before reconciliation completed")
                    }
                }
            }
            checkpoint("Checking notes", completed)
        })();
        stopped.store(true, Ordering::Release);
        // Drop the receiver before scope joins: workers cannot remain blocked
        // on a full result queue when the coordinator cancels or fails.
        drop(receive);
        result
    })?;
    vault.unreadable.extend(bank.unreadable);
    finish_reconcile(
        canonical,
        vault,
        previous,
        bank.sources,
        bank.stats,
        force_read,
        checkpoint,
    )
}

fn begin_reconcile<'a>(
    root: &Path,
    previous: Option<&'a Snapshot>,
    checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
) -> Result<(PathBuf, Option<&'a Snapshot>, Vault)> {
    let canonical = root
        .canonicalize()
        .with_context(|| format!("Resolve vault directory {}", display_path(root)))?;
    let previous = previous.filter(|p| p.root == canonical && p.schema == SCHEMA);
    let vault = Vault::scan_metadata_with(root, checkpoint)?;
    Ok((canonical, previous, vault))
}

#[derive(Default)]
struct CheckedSource {
    source: Option<Source>,
    unreadable: Option<UnreadableEntry>,
    stats: ReconcileStats,
}

fn check_source(
    root: &Path,
    relative: &str,
    previous: Option<&Snapshot>,
    force_read: bool,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
) -> CheckedSource {
    let mut result = CheckedSource::default();
    let stats = &mut result.stats;
    let path = root.join(relative);
    if super::cloud_placeholder(&path) {
        result.unreadable = Some(UnreadableEntry {
            path,
            operation: "read note",
            error: "iCloud placeholder is not downloaded".into(),
        });
        return result;
    }
    let before = SourceRevision::read(&path).ok();
    let old = previous.and_then(|p| p.sources.get(relative));
    if !stats.reuse.needs_read(before.as_ref(), old, force_read) {
        result.source = Some(old.unwrap().clone());
        stats.reused += 1;
        return result;
    }
    stats.read += 1;
    match read(&path) {
        Ok(bytes) => {
            if let Err(error) = std::str::from_utf8(&bytes) {
                result.unreadable = Some(UnreadableEntry {
                    path,
                    operation: "decode note",
                    error: error.to_string(),
                });
                return result;
            }
            let after = SourceRevision::read(&path).ok();
            let stamp = (before == after).then_some(after).flatten();
            result.source = Some(Source {
                stamp,
                bytes: STANDARD.encode(bytes),
            });
        }
        Err(error) => {
            result.unreadable = Some(UnreadableEntry {
                error: super::read_error(&path, &error),
                path,
                operation: "read note",
            })
        }
    }
    result
}

#[derive(Default)]
struct CheckedSources {
    sources: BTreeMap<String, Source>,
    unreadable: Vec<UnreadableEntry>,
    stats: ReconcileStats,
}
impl CheckedSources {
    fn insert(&mut self, path: &str, checked: CheckedSource) {
        if let Some(source) = checked.source {
            self.sources.insert(path.into(), source);
        }
        self.unreadable.extend(checked.unreadable);
        self.stats.read += checked.stats.read;
        self.stats.reused += checked.stats.reused;
        let other = checked.stats.reuse;
        let counters = &mut self.stats.reuse;
        counters.forced += other.forced;
        counters.missing_source += other.missing_source;
        counters.invalidated_revision += other.invalidated_revision;
        counters.unavailable_metadata += other.unavailable_metadata;
        counters.imprecise_revision += other.imprecise_revision;
        counters.changed_revision += other.changed_revision;
        counters.coarse_mtime += other.coarse_mtime;
        counters.precise_ctime += other.precise_ctime;
        counters.mismatch.size += other.mismatch.size;
        counters.mismatch.modified += other.mismatch.modified;
        counters.mismatch.device += other.mismatch.device;
        counters.mismatch.inode += other.mismatch.inode;
        counters.mismatch.changed += other.mismatch.changed;
    }
}

fn finish_reconcile(
    canonical: PathBuf,
    mut vault: Vault,
    previous: Option<&Snapshot>,
    sources: BTreeMap<String, Source>,
    mut stats: ReconcileStats,
    force_read: bool,
    checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
) -> Result<(Vault, Snapshot, ReconcileStats)> {
    vault.finish_scan_report();
    // A reread is not necessarily a content change (replay/metadata may have
    // invalidated a stamp). Compare cached bytes, including readable membership.
    let mut affected = std::collections::BTreeSet::new();
    if let Some(old) = previous {
        for path in old.sources.keys().chain(sources.keys()) {
            if old.sources.get(path).map(|s| &s.bytes) != sources.get(path).map(|s| &s.bytes) {
                affected.insert(path.clone());
            }
        }
    }
    // Inventory changes can alter unresolved/ambiguous destinations in otherwise
    // unchanged sources. Include conservative syntax candidates only for those
    // identities; ordinary content edits never build a second all-source index.
    let topology: std::collections::BTreeSet<_> = previous.map_or_else(Default::default, |old| {
        let old_entries: std::collections::BTreeSet<_> = old
            .entries
            .iter()
            .map(|entry| (&entry.path, entry.kind as u8))
            .collect();
        let new_entries: std::collections::BTreeSet<_> = vault
            .entries
            .iter()
            .map(|entry| (&entry.path, entry.kind as u8))
            .collect();
        old_entries
            .symmetric_difference(&new_entries)
            .map(|(path, _)| (*path).clone())
            .collect()
    });
    let content_unchanged = affected.is_empty();
    let bounded = !force_read
        && previous.is_some()
        && affected.len() + topology.len() < crate::watch::BULK_THRESHOLD;
    if bounded && !topology.is_empty() {
        let candidates = crate::link_candidates::CandidateIndex::from_snapshot(previous.unwrap());
        for path in &topology {
            affected.extend(candidates.referrers_for(path));
        }
    }
    // A topology with many referrers is deliberately the bulk/rescan path.
    stats.graph_reused = bounded && affected.len() < crate::watch::BULK_THRESHOLD;
    if stats.graph_reused {
        vault.backlink_map = previous.unwrap().links.clone();
        if !affected.is_empty() {
            checkpoint("Preparing backlinks", 0)?;
            vault.refresh_backlinks_from(&affected, checkpoint, |path| {
                let bytes = STANDARD.decode(&sources.get(path)?.bytes).ok()?;
                Some(String::from_utf8_lossy(&bytes).into_owned())
            })?;
        }
        stats.affected = affected;
    } else {
        vault.build_backlinks_from(checkpoint, |path| {
            let bytes = STANDARD.decode(&sources.get(path)?.bytes).ok()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        })?;
    }
    let unreadable: Vec<_> = vault
        .unreadable
        .iter()
        .map(|item| CachedUnreadable {
            path: item.path.clone(),
            operation: item.operation.into(),
            error: item.error.clone(),
        })
        .collect();
    // An unchanged vault keeps the persisted snapshot's identity, so the source
    // bank need not be re-serialised. Identical bytes alone are not enough:
    // inventory, revisions and unreadable entries must also match.
    let unchanged = previous.filter(|old| {
        old.persisted.is_some()
            && stats.graph_reused
            && content_unchanged
            && old.root == canonical
            && old.entries == vault.entries
            && old.unreadable == unreadable
            && old
                .sources
                .iter()
                .zip(&sources)
                .all(|((old_path, old), (path, new))| old_path == path && old.stamp == new.stamp)
    });
    let snapshot = Snapshot {
        schema: SCHEMA,
        root: canonical,
        id: unchanged.map_or_else(|| uuid::Uuid::new_v4().to_string(), |old| old.id.clone()),
        entries: vault.entries.clone(),
        links: vault.backlink_map.clone(),
        sources,
        unreadable,
        search_generation: if stats.graph_reused {
            previous.and_then(|p| p.search_generation.clone())
        } else {
            None
        },
        persisted: unchanged.and_then(|old| old.persisted.clone()),
    };
    Ok((vault, snapshot, stats))
}

pub fn save_complete(snapshot: &Snapshot, vault: &Vault, base: &Path) -> Result<()> {
    if !vault.inventory_complete || !vault.unreadable.is_empty() {
        bail!("Partial inventory is not a warm baseline");
    }
    snapshot.save(base)
}

/// Partial caches are provisional, never evidence of complete reconciliation.
/// Persist available inventory so one unreadable entry cannot disable warm start.
pub fn save_provisional(
    snapshot: &Snapshot,
    vault: &Vault,
    base: &Path,
    primary: Option<&str>,
) -> Result<()> {
    ensure!(
        vault.inventory_scanned && vault.root.canonicalize()? == snapshot.root,
        "Cannot cache unfinished/foreign inventory"
    );
    if !snapshot.is_persisted_in(base) {
        snapshot.save(base)?;
    }
    let startup = StartupSnapshot {
        schema: SCHEMA,
        root: snapshot.root.clone(),
        entries: snapshot.entries.clone(),
        links: snapshot.links.clone(),
        unreadable: snapshot.unreadable.clone(),
        primary: primary.and_then(|path| {
            snapshot
                .sources
                .get(path)
                .cloned()
                .map(|source| (path.to_owned(), source))
        }),
        source_id: Some(snapshot.id.clone()),
        search_generation: snapshot.search_generation.clone(),
        previous: None,
        latest: None,
    };
    let bytes = serde_json::to_vec(&startup)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "Startup snapshot exceeds size limit"
    );
    let path = base.join("reader-startup.json");
    // The small manifest also carries the primary note; rewrite only on change.
    if std::fs::symlink_metadata(&path)
        .is_ok_and(|meta| meta.is_file() && meta.len() == bytes.len() as u64)
        && std::fs::read(&path).is_ok_and(|existing| existing == bytes)
    {
        return Ok(());
    }
    let mut file = tempfile::NamedTempFile::new_in(base)?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_reconciliation_matches_serial_sources_graph_errors_and_counters() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        for n in 0..48 {
            std::fs::write(
                root.join(format!("note-{n}.md")),
                format!("# Note {n}\n[[target]] [link](target.md)"),
            )
            .unwrap();
        }
        std::fs::write(root.join("target.md"), "target").unwrap();
        std::fs::write(root.join("denied.md"), "inaccessible").unwrap();
        std::fs::write(root.join("invalid.md"), [0xff]).unwrap();
        let read = |p: &Path| {
            if p.file_name().unwrap() == "denied.md" {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "read denial positive control",
                ))
            } else {
                std::fs::read(p)
            }
        };
        let (serial_vault, serial, serial_stats) =
            reconcile_with_reader(&root, None, false, &mut |_, _| Ok(()), &mut |p| read(p))
                .unwrap();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let calls = AtomicUsize::new(0);
        let (parallel_vault, parallel, parallel_stats) =
            reconcile_parallel_with_reader(&root, None, false, &mut |_, _| Ok(()), &|p| {
                calls.fetch_add(1, Ordering::SeqCst);
                let in_flight = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(in_flight, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                let result = read(p);
                active.fetch_sub(1, Ordering::SeqCst);
                result
            })
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            51,
            "all actual source reads observed"
        );
        assert!(
            peak.load(Ordering::SeqCst) > 1,
            "parallel I/O positive control"
        );
        assert!(peak.load(Ordering::SeqCst) <= SOURCE_READ_WORKERS);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(serial.sources().unwrap(), parallel.sources().unwrap());
        for path in serial.source_paths() {
            assert_eq!(serial.source_revision(path), parallel.source_revision(path));
        }
        assert_eq!(serial.entries, parallel.entries);
        assert_eq!(
            serde_json::to_value(serial_vault.backlink_map).unwrap(),
            serde_json::to_value(parallel_vault.backlink_map).unwrap()
        );
        assert_eq!(
            serde_json::to_value(serial_vault.unreadable).unwrap(),
            serde_json::to_value(parallel_vault.unreadable).unwrap()
        );
        assert_eq!(
            (serial_stats.read, serial_stats.reused),
            (parallel_stats.read, parallel_stats.reused)
        );
        assert_eq!(
            serde_json::to_value(serial_stats.reuse).unwrap(),
            serde_json::to_value(parallel_stats.reuse).unwrap()
        );
    }

    #[test]
    fn parallel_cancellation_drops_a_full_result_queue_before_joining() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc,
        };
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        for n in 0..200 {
            std::fs::write(root.join(format!("note-{n}.md")), "source").unwrap();
        }
        let reads = Arc::new(AtomicUsize::new(0));
        let worker_reads = reads.clone();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = reconcile_parallel_with_reader(
                &root,
                None,
                false,
                &mut |phase, _| {
                    if phase == "Checking notes" {
                        let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
                        while worker_reads.load(Ordering::SeqCst) < SOURCE_READ_WORKERS * 3
                            && std::time::Instant::now() < until
                        {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        bail!("cancellation positive control");
                    }
                    Ok(())
                },
                &|p| {
                    worker_reads.fetch_add(1, Ordering::SeqCst);
                    std::fs::read(p)
                },
            );
            send.send(result.map(|_| ())).unwrap();
        });
        let error = receive
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("cancellation must not deadlock scoped workers")
            .unwrap_err();
        assert!(error.to_string().contains("cancellation positive control"));
        worker.join().unwrap();
        assert!(
            reads.load(Ordering::SeqCst) >= SOURCE_READ_WORKERS * 2,
            "full result queue positive control"
        );
        assert!(
            reads.load(Ordering::SeqCst) <= SOURCE_READ_WORKERS * 3,
            "cancellation leaves only queued or in-flight work"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn parallel_warm_reuse_detects_same_size_restored_mtime_edits() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("source.md");
        let modified = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::write(&path, "[[Old]]").unwrap();
        std::fs::write(root.join("New.md"), "target").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let stamp = SourceRevision::read(&path).unwrap();
        assert!(
            stamp.is_precise(),
            "precise native change time positive control"
        );
        let (_, cold, _) = reconcile_parallel(&root, None, false, &mut |_, _| Ok(())).unwrap();
        let (_, warm, stats) =
            reconcile_parallel_with_reader(&root, Some(&cold), false, &mut |_, _| Ok(()), &|_| {
                panic!("unchanged sources must not be read")
            })
            .unwrap();
        assert_eq!((stats.read, stats.reused), (0, 2));
        assert!(stats.graph_reused);
        std::fs::write(&path, "[[New]]").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let (vault, updated, stats) =
            reconcile_parallel(&root, Some(&warm), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((stats.read, stats.reused), (1, 1));
        assert_eq!(stats.reuse.mismatch.modified, 0);
        assert_eq!(stats.reuse.mismatch.changed, 1);
        assert_eq!(updated.source("source.md").as_deref(), Some("[[New]]"));
        assert_eq!(vault.backlinks("New.md").len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cached_inventory_accepts_scanned_literal_posix_names() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        for path in [
            "last.md",
            "log:2026.md",
            r"literal\name.md",
            "asset:2026.png",
        ] {
            std::fs::write(root.join(path), "# Literal filename\n").unwrap();
        }
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_provisional(&snapshot, &vault, &cache, Some("last.md")).unwrap();
        let startup = StartupSnapshot::load(&cache, &root).unwrap();
        assert_eq!(startup.vault().notes.len(), 3);
        let saved =
            Snapshot::load(&cache, &root).expect("all scan-admitted identities must round-trip");
        let (_, _, stats) = reconcile(&root, Some(&saved), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(stats.read, 0);
        assert_eq!(stats.reused, 3);
    }

    #[test]
    fn rejected_inventory_reports_the_offending_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note").unwrap();
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_provisional(&snapshot, &vault, &cache, Some("note.md")).unwrap();
        let file = cache.join("reader-startup.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        value["entries"][0]["path"] = "../outside.md".into();
        std::fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
        let error = StartupSnapshot::load(&cache, &root).err().unwrap();
        assert!(format!("{error:#}").contains("../outside.md"));
        assert!(
            Snapshot::load(&cache, &root).is_some(),
            "untouched source bank positive control"
        );
    }

    #[test]
    fn graph_inventory_retains_precedence_and_occupied_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        for path in [
            "root.md",
            "notes/local.md",
            "local.md",
            "blocked.md",
            "other/suffix.md",
            "dangling.md",
        ] {
            std::fs::write(root.join(path), "# Target").unwrap();
        }
        std::fs::create_dir(root.join("notes/blocked.md")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("missing", root.join("notes/dangling.md")).unwrap();
        let absolute = root.canonicalize().unwrap().join("root.md");
        let text = format!("[Local](local.md)\n[Root](root.md)\n[Suffix](suffix.md)\n[Absolute](<{}>)\n[Root slash](/root.md)\n[Blocked](blocked.md)\n[Strict](./root.md)\n[Escape](../../root.md)\n[Dangling](dangling.md)\n", absolute.display());
        std::fs::write(root.join("notes/start.md"), text).unwrap();
        let (vault, _, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(vault.backlinks("notes/local.md").len(), 1);
        assert!(vault.backlinks("local.md").is_empty());
        // Drive-letter Markdown destinations remain unsupported on Windows.
        assert_eq!(
            vault.backlinks("root.md").len(),
            if cfg!(windows) { 2 } else { 3 }
        );
        assert_eq!(vault.backlinks("other/suffix.md").len(), 1);
        assert!(vault.backlinks("blocked.md").is_empty());
        #[cfg(unix)]
        assert!(vault.backlinks("dangling.md").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn graph_absolute_alias_and_outside_file_keep_their_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        let outside = temp.path().join("outside.md");
        std::fs::write(&outside, "# Outside").unwrap();
        let twin = outside.strip_prefix("/").unwrap();
        std::fs::create_dir_all(root.join(twin).parent().unwrap()).unwrap();
        std::fs::write(root.join(twin), "# Root namesake").unwrap();
        let alias = root.join("alias.md");
        std::os::unix::fs::symlink(root.join("target.md"), &alias).unwrap();
        let text = format!(
            "[Alias](<{}>)\n[Outside](<{}>)\n",
            alias.display(),
            outside.display()
        );
        std::fs::write(root.join("start.md"), text).unwrap();
        let (vault, _, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(vault.backlinks("target.md").len(), 1);
        assert!(
            vault.backlinks(&note_path(twin)).is_empty(),
            "outside file cannot redirect to vault-root namesake"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual paired bounded source reading with actual filesystem latency"]
    fn parallel_source_latency_profile() {
        #[link(name = "dl")]
        unsafe extern "C" {
            fn dlsym(
                handle: *mut std::ffi::c_void,
                name: *const std::ffi::c_char,
            ) -> *mut std::ffi::c_void;
        }
        let (phase, count): (
            unsafe extern "C" fn(*const std::ffi::c_char),
            unsafe extern "C" fn(i32) -> std::ffi::c_ulong,
        ) = unsafe {
            let set = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_phase".as_ptr());
            let count = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_count".as_ptr());
            assert!(
                !set.is_null() && !count.is_null(),
                "use the slow-vault-fs preload"
            );
            (
                std::mem::transmute::<
                    *mut std::ffi::c_void,
                    unsafe extern "C" fn(*const std::ffi::c_char),
                >(set),
                std::mem::transmute::<
                    *mut std::ffi::c_void,
                    unsafe extern "C" fn(i32) -> std::ffi::c_ulong,
                >(count),
            )
        };
        let set = |name: &'static std::ffi::CStr| unsafe { phase(name.as_ptr()) };
        let temp = tempfile::Builder::new()
            .prefix("okilum-source-latency-")
            .tempdir()
            .unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        for n in 0..5000 {
            std::fs::write(
                root.join(format!("notes/note-{n}.md")),
                format!(
                    "# Note {n}\n\n[[target]]\n\n[Target](target.md)\n\n{}",
                    "Source paragraph.\n\n".repeat(60)
                ),
            )
            .unwrap();
        }
        let read_ms: u64 = std::env::var("OKILUM_SLOW_FS_MS").unwrap().parse().unwrap();
        let metadata_us: u64 = std::env::var("OKILUM_SLOW_FS_METADATA_US")
            .unwrap()
            .parse()
            .unwrap();
        set(c"positive_control");
        let start = std::time::Instant::now();
        SourceRevision::read(&root.join("target.md")).unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_micros(metadata_us));
        let start = std::time::Instant::now();
        assert_eq!(std::fs::read(root.join("target.md")).unwrap(), b"# Target");
        assert!(start.elapsed() >= std::time::Duration::from_millis(read_ms));
        set(c"setup");
        let samples: usize = std::env::var("OKILUM_CLOUD_PROFILE_SAMPLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);
        for sample in 0..samples {
            let mut serial: Option<(Vault, Snapshot)> = None;
            for parallel in [false, true] {
                set(if parallel {
                    c"parallel_sources"
                } else {
                    c"serial_sources"
                });
                let before: Vec<_> = (0..4).map(|op| unsafe { count(op) }).collect();
                let start = std::time::Instant::now();
                let mut phase_name = String::new();
                let mut phase_start = start;
                let mut phases = BTreeMap::new();
                let mut checkpoint = |name: &str, _: usize| {
                    if phase_name != name {
                        if !phase_name.is_empty() {
                            phases.insert(
                                phase_name.clone(),
                                phase_start.elapsed().as_secs_f64() * 1000.,
                            );
                        }
                        phase_name = name.to_owned();
                        phase_start = std::time::Instant::now();
                    }
                    Ok(())
                };
                let (vault, snapshot, stats) = if parallel {
                    reconcile_parallel(&root, None, false, &mut checkpoint)
                } else {
                    reconcile_with_reader(&root, None, false, &mut checkpoint, &mut |p| {
                        std::fs::read(p)
                    })
                }
                .unwrap();
                let ms = start.elapsed().as_secs_f64() * 1000.;
                phases.insert(phase_name, phase_start.elapsed().as_secs_f64() * 1000.);
                let calls: Vec<_> = (0..4)
                    .map(|op| unsafe { count(op) } - before[op as usize])
                    .collect();
                set(c"setup");
                assert_eq!((stats.read, stats.reused), (5001, 0));
                assert!(vault.inventory_complete && vault.unreadable.is_empty());
                assert_eq!(vault.backlinks("target.md").len(), 10000);
                assert!(
                    calls[0] >= 5001 && calls[1] >= 5001 && calls[2] >= 10002,
                    "actual worker open/read/stat injection positive control: {calls:?}"
                );
                if let Some((old_vault, old_snapshot)) = serial.take() {
                    assert_eq!(old_snapshot.sources().unwrap(), snapshot.sources().unwrap());
                    assert_eq!(old_snapshot.entries, snapshot.entries);
                    assert_eq!(
                        serde_json::to_value(old_vault.backlink_map).unwrap(),
                        serde_json::to_value(&vault.backlink_map).unwrap()
                    );
                } else {
                    serial = Some((vault.clone(), snapshot.clone()));
                }
                eprintln!("SOURCE_LATENCY_PROFILE sample={sample} parallel={parallel} notes=5001 read={} total_ms={ms:.2} phases={} actual_calls={calls:?}; open/read delay {read_ms}ms, stat delay {metadata_us}us; same host/session, search/persist/native SMB excluded", stats.read, serde_json::to_string(&phases).unwrap());
                if parallel {
                    set(c"parallel_sources");
                    let before = unsafe { count(0) };
                    let start = std::time::Instant::now();
                    let (_, _, warm_stats) = reconcile_parallel_with_reader(
                        &root,
                        Some(&snapshot),
                        false,
                        &mut |_, _| Ok(()),
                        &|_| panic!("unchanged warm source must not be read"),
                    )
                    .unwrap();
                    let warm_ms = start.elapsed().as_secs_f64() * 1000.;
                    assert_eq!(unsafe { count(0) }, before, "no hidden warm source opens");
                    set(c"setup");
                    assert_eq!((warm_stats.read, warm_stats.reused), (0, 5001));
                    assert!(warm_stats.graph_reused);
                    eprintln!("SOURCE_LATENCY_WARM sample={sample} read=0 reused=5001 graph_reused=true reconcile_ms={warm_ms:.2}");
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual slow-filesystem complete backlink profile"]
    fn complete_backlinks_cloud_profile() {
        fn phase(name: &'static std::ffi::CStr) {
            #[link(name = "dl")]
            unsafe extern "C" {
                fn dlsym(
                    handle: *mut std::ffi::c_void,
                    name: *const std::ffi::c_char,
                ) -> *mut std::ffi::c_void;
            }
            unsafe {
                let symbol = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_phase".as_ptr());
                assert!(!symbol.is_null(), "use the slow-vault-fs preload");
                let set: unsafe extern "C" fn(*const std::ffi::c_char) =
                    std::mem::transmute(symbol);
                set(name.as_ptr());
            }
        }
        let temp = tempfile::Builder::new()
            .prefix("okilum-cloud-graph-")
            .tempdir()
            .unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        let absolute = root.canonicalize().unwrap().join("target.md");
        for n in 0..5000 {
            std::fs::write(
                root.join(format!("notes/note-{n}.md")),
                format!(
                    "# Note {n}\n\n[Absolute](<{}>)\n\n[Suffix](target.md)\n",
                    absolute.display()
                ),
            )
            .unwrap();
        }
        phase(c"positive_control");
        let delay: u64 = std::env::var("OKILUM_SLOW_FS_MS").unwrap().parse().unwrap();
        let start = std::time::Instant::now();
        std::fs::metadata(&absolute).unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_millis(delay));
        let start = std::time::Instant::now();
        assert_eq!(std::fs::read_to_string(&absolute).unwrap(), "# Target");
        assert!(start.elapsed() >= std::time::Duration::from_millis(delay));
        phase(c"setup");
        let samples: usize = std::env::var("OKILUM_CLOUD_PROFILE_SAMPLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        for sample in 0..samples {
            let mut graph_start = None;
            let (vault, _, stats) = reconcile(&root, None, true, &mut |name, _| {
                if name == "Preparing backlinks" && graph_start.is_none() {
                    phase(c"complete_graph");
                    graph_start = Some(std::time::Instant::now());
                }
                Ok(())
            })
            .unwrap();
            let ms = graph_start.unwrap().elapsed().as_secs_f64() * 1000.;
            phase(c"setup");
            assert!(vault.inventory_complete);
            assert_eq!(vault.notes.len(), 5001);
            assert_eq!(
                vault.backlinks("target.md").len(),
                10000,
                "absolute and suffix references preserved"
            );
            assert_eq!(stats.read, 5001);
            eprintln!("CLOUD_GRAPH_PROFILE sample={sample} notes=5001 links=10000 preparing_backlinks_ms={ms:.2}; actual vault filesystem delay, native Mac NOT_RUN");
        }
    }

    /// Delays are injected into real Linux open/read/stat calls by the fixture
    /// preload in scripts/probes, never into an artificial post-ready hold.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual iCloud-like first-document phase profile"]
    fn warm_primary_five_thousand_links_cloud_profile() {
        fn phase(name: &'static std::ffi::CStr) {
            #[link(name = "dl")]
            unsafe extern "C" {
                fn dlsym(
                    handle: *mut std::ffi::c_void,
                    name: *const std::ffi::c_char,
                ) -> *mut std::ffi::c_void;
            }
            unsafe {
                let symbol = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_phase".as_ptr());
                assert!(
                    !symbol.is_null(),
                    "run this probe with the slow-vault-fs preload"
                );
                let set_phase: unsafe extern "C" fn(*const std::ffi::c_char) =
                    std::mem::transmute(symbol);
                set_phase(name.as_ptr());
            }
        }
        let temp = tempfile::Builder::new()
            .prefix("okilum-cloud-link-")
            .tempdir()
            .unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let mut primary = String::from("# Last note\n\n");
        for n in 0..5000 {
            std::fs::write(
                root.join(format!("notes/note-{n}.md")),
                format!("# Note {n}\n\n{}", "A source paragraph.\n\n".repeat(350)),
            )
            .unwrap();
            primary.push_str(&format!("[Note {n}](note-{n}.md)\n\n"));
        }
        std::fs::write(root.join("last.md"), &primary).unwrap();
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_provisional(&snapshot, &vault, &cache, Some("last.md")).unwrap();
        let mut control = tempfile::NamedTempFile::new_in(&cache).unwrap();
        let start = std::time::Instant::now();
        serde_json::to_writer(&mut control, &snapshot).unwrap();
        control.flush().unwrap();
        let unbuffered_ms = start.elapsed().as_secs_f64() * 1000.;
        let mut buffered = tempfile::NamedTempFile::new_in(&cache).unwrap();
        let start = std::time::Instant::now();
        write_json(buffered.as_file_mut(), &snapshot).unwrap();
        let buffered_ms = start.elapsed().as_secs_f64() * 1000.;
        assert_eq!(
            std::fs::read(control.path()).unwrap(),
            std::fs::read(buffered.path()).unwrap()
        );
        eprintln!("CACHE_WRITE_PROFILE bytes={} unbuffered_ms={unbuffered_ms:.2} buffered_ms={buffered_ms:.2}; same JSON bytes, same host/session", control.as_file().metadata().unwrap().len());
        phase(c"positive_control");
        let delay: u64 = std::env::var("OKILUM_SLOW_FS_MS").unwrap().parse().unwrap();
        let start = std::time::Instant::now();
        std::fs::metadata(root.join("last.md")).unwrap();
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(delay),
            "actual stat latency positive control"
        );
        let start = std::time::Instant::now();
        assert_eq!(
            std::fs::read_to_string(root.join("last.md")).unwrap(),
            primary
        );
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(delay),
            "actual filesystem latency positive control"
        );
        phase(c"setup");
        let samples: usize = std::env::var("OKILUM_CLOUD_PROFILE_SAMPLES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        for sample in 0..samples {
            let startup = StartupSnapshot::load(&cache, &root).unwrap();
            let warm = startup.vault();
            assert_eq!(warm.notes.len(), 5001);
            assert!(!warm.inventory_complete);
            let raw = startup.source("last.md").unwrap();
            phase(c"warm_primary");
            let start = std::time::Instant::now();
            let document = crate::render::reader_document_from_source(&warm, "last.md", &raw);
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
            phase(c"setup");
            assert_eq!(
                document.links.len(),
                5000,
                "every authored link was processed"
            );
            assert_eq!(document.original_body, primary);
            eprintln!("CLOUD_PRIMARY_PROFILE sample={sample} notes=5001 links=5000 delay_ms={delay} primary_source_and_render_ms={elapsed_ms:.2}; cached source, injected actual filesystem calls, native Mac NOT_RUN");
        }
    }

    #[test]
    fn provisional_markdown_identities_do_not_redirect_unverified_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("local.md"), "local").unwrap();
        std::fs::write(root.join("notes/suffix.md"), "suffix").unwrap();
        std::fs::write(root.join("last.md"), "last").unwrap();
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_provisional(&snapshot, &vault, &cache, Some("last.md")).unwrap();
        let warm = StartupSnapshot::load(&cache, &root).unwrap().vault();
        assert_eq!(
            warm.resolve_markdown("local.md", "last.md"),
            Resolution::Resolved {
                path: "local.md".into()
            }
        );
        assert_eq!(
            warm.resolve_markdown("suffix.md", "last.md"),
            Resolution::Unresolved
        );
        assert_eq!(
            vault.resolve_markdown("suffix.md", "last.md"),
            Resolution::Resolved {
                path: "notes/suffix.md".into()
            }
        );
        // Current lexical occupancy can invalidate a cached suffix fallback.
        std::fs::create_dir(root.join("suffix.md")).unwrap();
        assert_eq!(
            vault.resolve_markdown("suffix.md", "last.md"),
            Resolution::Unresolved
        );
        assert_eq!(
            warm.resolve_markdown("suffix.md", "last.md"),
            Resolution::Unresolved
        );
        let absolute = warm.root.join("local.md");
        assert_eq!(
            crate::document_links::resolve(&absolute.to_string_lossy(), false, &warm, "last.md")
                .candidates,
            vec!["local.md"]
        );
        let rendered = crate::render::reader_document_from_source(
            &warm,
            "last.md",
            "[Local](local.md) [Suffix](suffix.md) [File](file.pdf)",
        );
        assert_eq!(rendered.links.len(), 3);
        assert!(rendered.rendered.contains("okilum://unresolved/"));
    }

    #[test]
    fn latest_navigation_source_is_independent_of_late_reconcile_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("First.md"), "first").unwrap();
        std::fs::write(
            root.join("Latest.md"),
            "---\r\ntitle: Latest\r\n---\r\nexact source",
        )
        .unwrap();
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        remember_primary(&cache, &root, "Latest.md").unwrap();
        // A reconcile started on First can finish after navigation to Latest.
        save_provisional(&snapshot, &vault, &cache, Some("First.md")).unwrap();
        std::fs::write(root.join("Latest.md"), [0xff]).unwrap();
        let startup = StartupSnapshot::load(&cache, &root).unwrap();
        assert_eq!(startup.source("First.md").as_deref(), Some("first"));
        assert_eq!(
            startup.source("Latest.md").as_deref(),
            Some("---\r\ntitle: Latest\r\n---\r\nexact source")
        );
        assert!(remember_primary(&cache, &root, "../outside.md").is_err());
        // Corrupt/foreign optional primary data cannot invalidate the inventory.
        let mut primary: PrimarySnapshot =
            serde_json::from_slice(&std::fs::read(cache.join("reader-primary.json")).unwrap())
                .unwrap();
        primary.root = temp.path().to_path_buf();
        std::fs::write(
            cache.join("reader-primary.json"),
            serde_json::to_vec(&primary).unwrap(),
        )
        .unwrap();
        let startup = StartupSnapshot::load(&cache, &root).unwrap();
        assert!(startup.source("Latest.md").is_none());
        assert_eq!(startup.vault().notes.len(), 2);
    }

    #[test]
    fn partial_startup_inventory_survives_an_unreadable_entry_without_loading_all_sources() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Last.md"), "exact\r\n[[Other]]").unwrap();
        std::fs::write(root.join("Other.md"), "another source").unwrap();
        std::fs::write(root.join("Denied.md"), "unavailable").unwrap();
        let (vault, snapshot, _) =
            reconcile_with_reader(&root, None, false, &mut |_, _| Ok(()), &mut |path| {
                if path.ends_with("Denied.md") {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "denied",
                    ))
                } else {
                    std::fs::read(path)
                }
            })
            .unwrap();
        assert_eq!(vault.unreadable.len(), 1, "denied read positive control");
        save_provisional(&snapshot, &vault, &cache, Some("Last.md")).unwrap();
        let loaded = Snapshot::load(&cache, &root).unwrap();
        assert_eq!(loaded.source_paths().count(), 2);
        assert_eq!(
            loaded.source("Last.md").as_deref(),
            Some("exact\r\n[[Other]]")
        );
        assert_eq!(
            loaded.source_revision("Last.md"),
            Some(&SourceRevision::read(&root.join("Last.md")).unwrap())
        );
        assert!(loaded.source_revision("Denied.md").is_none());
        // A corrupt/unavailable large source bank must not gate the startup frame.
        std::fs::write(cache.join("reader-snapshot.json"), "unavailable bank").unwrap();
        let startup = StartupSnapshot::load(&cache, &root).unwrap();
        assert_eq!(
            startup.source("Last.md").as_deref(),
            Some("exact\r\n[[Other]]")
        );
        let provisional = startup.vault();
        assert_eq!(provisional.notes.len(), 3);
        assert!(provisional.inventory_scanned);
        assert!(!provisional.inventory_complete);
        assert_eq!(provisional.unreadable[0].path, root.join("Denied.md"));
        assert_eq!(provisional.backlinks("Other.md").len(), 1);
        assert!(StartupSnapshot::load(&cache, temp.path()).is_err());
        // Missing bytes without an explicit unreadable record are corruption.
        let mut corrupt = snapshot;
        corrupt.unreadable.clear();
        corrupt.save(&cache).unwrap();
        assert!(Snapshot::load(&cache, &root).is_none());
    }

    #[test]
    fn warm_inventory_reuses_sources_and_reconciles_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(root.join("_Assets")).unwrap();
        std::fs::write(root.join("Target.md"), "# Target").unwrap();
        std::fs::write(root.join("_Assets/Заметка.md"), "[[Target]] old").unwrap();
        let (vault, cold, stats) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(stats.read, 2);
        assert_eq!(vault.backlinks("Target.md").len(), 1);
        save_complete(&cold, &vault, &cache).unwrap();
        assert!(cold.save(&root.join("cache")).is_err());
        let loaded = Snapshot::load(&cache, &root).unwrap();
        assert_eq!(loaded.vault().backlinks("Target.md").len(), 1);
        let (_, warm, stats) = reconcile(&root, Some(&loaded), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((stats.read, stats.reused), (0, 2));
        std::fs::write(root.join("_Assets/Заметка.md"), "[[Target]] updated source").unwrap();
        std::fs::write(root.join("New.md"), "[[Target]]").unwrap();
        let (vault, updated, stats) =
            reconcile(&root, Some(&warm), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((stats.read, stats.reused), (2, 1));
        assert_eq!(vault.backlinks("Target.md").len(), 2);
        std::fs::rename(root.join("New.md"), root.join("Moved.md")).unwrap();
        std::fs::remove_file(root.join("_Assets/Заметка.md")).unwrap();
        let (vault, mut moved, stats) =
            reconcile(&root, Some(&updated), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((stats.read, stats.reused), (1, 1));
        assert_eq!(vault.backlinks("Target.md")[0].path, "Moved.md");
        assert!(!moved.sources.contains_key("New.md"));
        moved.invalidate_paths(&["Moved.md".into()]);
        let (_, _, stats) = reconcile(&root, Some(&moved), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((stats.read, stats.reused), (1, 1));
        let (_, _, stats) = reconcile(&root, Some(&moved), true, &mut |_, _| Ok(())).unwrap();
        assert_eq!((stats.read, stats.reused), (2, 0));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn coarse_mtime_with_precise_change_time_reuses_sources_and_detects_preserved_mtime_edits() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Coarse.md");
        std::fs::write(&path, "same length").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
        )
        .unwrap();
        drop(file);
        let cache = temp.path().join("external-cache");
        // Keep derived files outside the canonical root.
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let path_in_vault = root.join("Coarse.md");
        std::fs::rename(&path, &path_in_vault).unwrap();
        let stamp = SourceRevision::read(&path_in_vault).unwrap();
        assert_eq!(stamp.modified % 1_000_000_000, 0);
        assert_ne!(
            stamp.changed.rem_euclid(1_000_000_000),
            0,
            "precise native change time positive control"
        );
        let (vault, cold, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_provisional(&cold, &vault, &cache, Some("Coarse.md")).unwrap();
        let cold = Snapshot::load_checked(&cache, &root).unwrap();
        let (_, warm, stats) =
            reconcile_with_reader(&root, Some(&cold), false, &mut |_, _| Ok(()), &mut |_| {
                panic!("unchanged imported source must not be opened")
            })
            .unwrap();
        assert_eq!((stats.read, stats.reused), (0, 1));
        assert!(stats.graph_reused);
        assert_eq!(stats.reuse.coarse_mtime, 1);
        std::fs::write(&path_in_vault, "edit length").unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path_in_vault)
            .unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
        )
        .unwrap();
        drop(file);
        let after = SourceRevision::read(&path_in_vault).unwrap();
        assert_eq!(
            (stamp.modified, stamp.size, stamp.inode),
            (after.modified, after.size, after.inode)
        );
        assert_ne!(
            stamp.changed, after.changed,
            "same-size preserved-mtime write positive control"
        );
        let (_, changed, stats) = reconcile(&root, Some(&warm), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((stats.read, stats.reused), (1, 0));
        assert_eq!(stats.reuse.changed_revision, 1);
        assert_eq!(stats.reuse.mismatch.modified, 0);
        assert_eq!(stats.reuse.mismatch.changed, 1);
        assert_eq!(changed.source("Coarse.md").as_deref(), Some("edit length"));
        let mut invalidated = changed.clone();
        invalidated.invalidate_paths(&["Coarse.md".into()]);
        let (_, _, dirty) =
            reconcile(&root, Some(&invalidated), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((dirty.read, dirty.reused), (1, 0));
        assert_eq!(dirty.reuse.invalidated_revision, 1);
        let (_, _, forced) = reconcile(&root, Some(&changed), true, &mut |_, _| Ok(())).unwrap();
        assert_eq!((forced.read, forced.reused), (1, 0));
        assert_eq!(forced.reuse.forced, 1);
        let replacement = temp.path().join("replacement");
        std::fs::write(&replacement, "swap length").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
            )
            .unwrap();
        std::fs::rename(&replacement, &path_in_vault).unwrap();
        assert_ne!(
            after.inode,
            SourceRevision::read(&path_in_vault).unwrap().inode,
            "replacement inode positive control"
        );
        let (_, replaced, replacement_stats) =
            reconcile(&root, Some(&changed), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((replacement_stats.read, replacement_stats.reused), (1, 0));
        assert_eq!(replacement_stats.reuse.mismatch.modified, 0);
        assert_eq!(replacement_stats.reuse.mismatch.inode, 1);
        assert_eq!(replaced.source("Coarse.md").as_deref(), Some("swap length"));
    }

    #[test]
    fn genuinely_coarse_or_missing_revisions_do_not_reuse_sources() {
        let coarse = SourceRevision {
            size: 4,
            modified: 1_700_000_000_000_000_000,
            device: 1,
            inode: 2,
            changed: 1_700_000_000_000_000_000,
        };
        let source = Source {
            stamp: Some(coarse.clone()),
            bytes: STANDARD.encode("old!"),
        };
        let mut diagnostics = ReuseDiagnostics::default();
        assert!(
            diagnostics.needs_read(Some(&coarse), Some(&source), false),
            "identical coarse metadata is not proof of identical bytes"
        );
        assert_eq!(diagnostics.imprecise_revision, 1);
        assert!(
            diagnostics.needs_read(None, Some(&source), false),
            "metadata errors cannot prove reuse"
        );
        assert_eq!(diagnostics.unavailable_metadata, 1);
        let mut precise_mtime = coarse.clone();
        precise_mtime.modified += 123;
        assert!(
            precise_mtime.is_precise(),
            "existing precise-mtime path remains supported on all platforms"
        );
        let mut precise_ctime = coarse;
        precise_ctime.changed += 123;
        assert_eq!(
            precise_ctime.is_precise(),
            cfg!(any(unix, windows)),
            "only a native change time can strengthen reuse"
        );
    }

    #[test]
    fn corrupt_foreign_and_partial_snapshots_are_not_used() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("One.md"), "one").unwrap();
        let (mut vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_complete(&snapshot, &vault, &cache).unwrap();
        assert!(Snapshot::load(&cache, &root).is_some());
        assert!(Snapshot::load(&cache, temp.path()).is_none());
        vault.inventory_complete = false;
        assert!(save_complete(&snapshot, &vault, &cache).is_err());
        std::fs::write(cache.join("reader-snapshot.json"), b"broken").unwrap();
        assert!(Snapshot::load(&cache, &root).is_none());
        let mut bad = snapshot;
        bad.entries[0].path = "../outside.md".into();
        bad.save(&cache).unwrap();
        assert!(Snapshot::load(&cache, &root).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual imported-mtime warm reconcile with real filesystem latency"]
    fn imported_mtime_warm_reconcile_profile() {
        fn phase(name: &'static std::ffi::CStr) {
            #[link(name = "dl")]
            unsafe extern "C" {
                fn dlsym(
                    handle: *mut std::ffi::c_void,
                    name: *const std::ffi::c_char,
                ) -> *mut std::ffi::c_void;
            }
            unsafe {
                let symbol = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_phase".as_ptr());
                assert!(!symbol.is_null(), "run with slow-vault-fs preload");
                let set: unsafe extern "C" fn(*const std::ffi::c_char) =
                    std::mem::transmute(symbol);
                set(name.as_ptr());
            }
        }
        let temp = tempfile::Builder::new()
            .prefix("okilum-coarse-reconcile-")
            .tempdir()
            .unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        let imported = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for n in 0..5092 {
            let path = root.join(format!("Note {n}.md"));
            std::fs::write(
                &path,
                format!(
                    "# Note {n}\n\n[[Note {}]]\n\n{}",
                    (n + 1) % 5092,
                    "A synced source paragraph.\n\n".repeat(350)
                ),
            )
            .unwrap();
            if n >= 66 {
                std::fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(imported))
                    .unwrap();
            }
        }
        let (vault, cold, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_provisional(&cold, &vault, &cache, Some("Note 0.md")).unwrap();
        let saved = Snapshot::load_checked(&cache, &root).unwrap();
        assert_eq!(
            saved
                .sources
                .values()
                .filter(|source| source
                    .stamp
                    .as_ref()
                    .is_some_and(|stamp| stamp.modified.is_multiple_of(1_000_000_000)))
                .count(),
            5026,
            "imported mtime positive control"
        );
        let metadata_us: u64 = std::env::var("OKILUM_SLOW_FS_METADATA_US")
            .unwrap()
            .parse()
            .unwrap();
        let read_ms: u64 = std::env::var("OKILUM_SLOW_FS_MS").unwrap().parse().unwrap();
        phase(c"positive_control");
        let start = std::time::Instant::now();
        SourceRevision::read(&root.join("Note 0.md")).unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_micros(metadata_us));
        let start = std::time::Instant::now();
        assert!(std::fs::read_to_string(root.join("Note 0.md"))
            .unwrap()
            .contains("# Note 0"));
        assert!(start.elapsed() >= std::time::Duration::from_millis(read_ms));
        phase(c"setup");
        let samples = std::env::var("OKILUM_CLOUD_PROFILE_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2);
        for sample in 0..samples {
            let mut graph = false;
            let start = std::time::Instant::now();
            phase(c"warm_reconcile");
            let (warm_vault, _, stats) = reconcile(&root, Some(&saved), false, &mut |name, _| {
                graph |= name == "Preparing backlinks";
                Ok(())
            })
            .unwrap();
            let ms = start.elapsed().as_secs_f64() * 1000.;
            phase(c"setup");
            assert_eq!(stats.read + stats.reused, 5092);
            assert_eq!((stats.read, stats.reused), (0, 5092));
            assert_eq!(stats.reuse.coarse_mtime, 5026);
            assert!(stats.graph_reused);
            assert_eq!(
                warm_vault.backlinks("Note 0.md").len(),
                1,
                "reused graph positive control"
            );
            eprintln!("IMPORTED_MTIME_PROFILE sample={sample} notes=5092 read={} reused={} graph_built={graph} reconcile_ms={ms:.2}; source opens/reads {read_ms}ms, metadata {metadata_us}us, actual filesystem injection; cache load/persist and search excluded, native Mac NOT_RUN", stats.read, stats.reused);
        }
        // A second process cannot reuse any parent memory. Each launch reloads
        // the persisted source bank and publishes its next cache for the next one.
        for launch in 1..=2 {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "vault::warm::tests::imported_mtime_relaunch_child",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("OKILUM_RELAUNCH_FIXTURE", temp.path())
                .env("OKILUM_RELAUNCH_SAMPLE", launch.to_string())
                .status()
                .unwrap();
            assert!(status.success(), "fresh-process warm launch {launch}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "child of the manual imported-mtime relaunch probe"]
    fn imported_mtime_relaunch_child() {
        let fixture = PathBuf::from(
            std::env::var_os("OKILUM_RELAUNCH_FIXTURE")
                .expect("run imported_mtime_warm_reconcile_profile"),
        );
        let launch = std::env::var("OKILUM_RELAUNCH_SAMPLE").unwrap();
        let root = fixture.join("vault");
        let cache = fixture.join("cache");
        let start = std::time::Instant::now();
        let startup = StartupSnapshot::load(&cache, &root).unwrap();
        assert_eq!(startup.vault().notes.len(), 5092);
        assert!(startup.source("Note 0.md").unwrap().contains("# Note 0"));
        let saved = Snapshot::load_checked(&cache, &root).unwrap();
        let cache_load_ms = start.elapsed().as_secs_f64() * 1000.;
        #[link(name = "dl")]
        unsafe extern "C" {
            fn dlsym(
                handle: *mut std::ffi::c_void,
                name: *const std::ffi::c_char,
            ) -> *mut std::ffi::c_void;
        }
        let set: unsafe extern "C" fn(*const std::ffi::c_char) = unsafe {
            let symbol = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_phase".as_ptr());
            assert!(!symbol.is_null(), "inherited preload positive control");
            std::mem::transmute(symbol)
        };
        unsafe {
            set(c"warm_reconcile".as_ptr());
        }
        let start = std::time::Instant::now();
        let (vault, snapshot, stats) =
            reconcile_with_reader(&root, Some(&saved), false, &mut |_, _| Ok(()), &mut |_| {
                panic!("fresh-process unchanged source must not be opened")
            })
            .unwrap();
        let reconcile_ms = start.elapsed().as_secs_f64() * 1000.;
        unsafe {
            set(c"setup".as_ptr());
        }
        assert_eq!((stats.read, stats.reused), (0, 5092));
        assert_eq!(stats.reuse.coarse_mtime, 5026);
        assert!(stats.graph_reused);
        assert_eq!(vault.backlinks("Note 0.md").len(), 1);
        let start = std::time::Instant::now();
        save_provisional(&snapshot, &vault, &cache, Some("Note 0.md")).unwrap();
        let persist_ms = start.elapsed().as_secs_f64() * 1000.;
        eprintln!("IMPORTED_MTIME_RELAUNCH launch={launch} read={} reused={} graph_reused={} cache_load_ms={cache_load_ms:.2} reconcile_ms={reconcile_ms:.2} persist_ms={persist_ms:.2}; fresh process, actual syscall delay inherited, no canonical source writes, search/native GUI excluded", stats.read, stats.reused, stats.graph_reused);
    }

    #[test]
    #[ignore = "manual same-host cold/warm benchmark with 5000 notes"]
    fn benchmark_5000_notes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..5000 {
            std::fs::write(
                root.join(format!("Note {i}.md")),
                format!(
                    "# Note {i}\n\n[[Note {}]]\n{}",
                    (i + 1) % 5000,
                    "A paragraph of text. ".repeat(100)
                ),
            )
            .unwrap();
        }
        let mut cold_times = Vec::new();
        let mut warm_times = Vec::new();
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let (vault, cold, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
            cold_times.push(start.elapsed().as_millis());
            save_complete(&cold, &vault, &cache).unwrap();
            let start = std::time::Instant::now();
            let saved = Snapshot::load(&cache, &root).unwrap();
            let published = saved.vault();
            let publish_ms = start.elapsed().as_millis();
            let (_, _, stats) = reconcile(&root, Some(&saved), false, &mut |_, _| Ok(())).unwrap();
            warm_times.push(start.elapsed().as_millis());
            assert_eq!(published.notes.len(), 5000);
            assert_eq!((stats.read, stats.reused), (0, 5000));
            println!(
                "warm publish {publish_ms}ms; warm sources read {}, reused {}",
                stats.read, stats.reused
            );
        }
        cold_times.sort();
        warm_times.sort();
        println!("5000 notes, inventory+graph (search excluded), same-process OS-warm files: cold {cold_times:?}ms p50={}ms; snapshot-load+reconcile warm {warm_times:?}ms p50={}ms", cold_times[2], warm_times[2]);
    }
    #[test]
    fn warm_small_changes_match_full_graph_including_topology_and_unreadable_sources() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let cache = fixture.path().join("cache");
        std::fs::create_dir_all(root.join("folder")).unwrap();
        for (path, text) in [
            (
                "a.md",
                "[[missing]] [[target]] [Target](target.md) ![[folder/target]]",
            ),
            ("b.md", "---\nlink: '[[target]]'\n---\n[[target]]"),
            ("target.md", "# Target"),
            ("folder/target.md", "# Duplicate"),
        ] {
            std::fs::write(root.join(path), text).unwrap();
        }
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_complete(&snapshot, &vault, &cache).unwrap();
        let mut previous = Snapshot::load_checked(&cache, &root).unwrap();
        for step in 0..5 {
            match step {
                0 => std::fs::write(root.join("b.md"), "[[a]] changed").unwrap(),
                1 => std::fs::write(root.join("missing.md"), "[[a]]").unwrap(),
                2 => std::fs::remove_file(root.join("target.md")).unwrap(),
                3 => {} // existing readable source becomes inaccessible
                4 => {} // and recovers on the next persisted launch
                _ => unreachable!(),
            }
            if step == 3 {
                previous.invalidate_paths(&["a.md".into()]);
            }
            let read = |path: &Path| {
                if step == 3 && path.ends_with("a.md") {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected denied",
                    ))
                } else {
                    read_source(path).map(String::into_bytes)
                }
            };
            let (vault, updated, stats) = reconcile_with_reader(
                &root,
                Some(&previous),
                false,
                &mut |_, _| Ok(()),
                &mut |path| read(path),
            )
            .unwrap();
            let (full, _, _) =
                reconcile_with_reader(&root, None, true, &mut |_, _| Ok(()), &mut |path| {
                    read(path)
                })
                .unwrap();
            assert!(stats.graph_reused, "step {step}");
            assert_eq!(
                serde_json::to_value(&vault.backlink_map).unwrap(),
                serde_json::to_value(&full.backlink_map).unwrap(),
                "step {step}"
            );
            save_provisional(&updated, &vault, &cache, Some("b.md")).unwrap();
            previous = Snapshot::load_checked(&cache, &root).unwrap();
        }
        // Invalidated metadata with identical bytes needs no graph/search update.
        previous.invalidate_paths(&["b.md".into()]);
        let (_, _, stats) = reconcile(&root, Some(&previous), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(stats.read, 1);
        assert!(stats.graph_reused);
        assert!(stats.affected.is_empty());
        let (_, _, forced) = reconcile(&root, Some(&previous), true, &mut |_, _| Ok(())).unwrap();
        assert!(!forced.graph_reused);
    }

    #[test]
    fn priority_schedules_current_folder_links_and_recent_before_rest_without_reordering_inventory()
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("z")).unwrap();
        std::fs::create_dir_all(root.join("f")).unwrap();
        let mut preferred = std::collections::BTreeSet::from([
            "z/current.md".to_string(),
            "link.md".into(),
            "recent.md".into(),
        ]);
        for n in 0..5 {
            preferred.insert(format!("f/n{n}.md"));
        }
        for path in preferred
            .iter()
            .cloned()
            .chain((0..10).map(|n| format!("other{n}.md")))
        {
            std::fs::write(root.join(path), "# Source\n\n[[link]]").unwrap();
        }
        let (send, receive) = std::sync::mpsc::channel();
        let gate = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let read_gate = gate.clone();
        let work_root = root.clone();
        let worker = std::thread::spawn(move || {
            reconcile_parallel_prioritized_with_reader(
                &work_root,
                None,
                false,
                &[
                    "z/current.md".into(),
                    "f/".into(),
                    "link.md".into(),
                    "recent.md".into(),
                ],
                &mut |_, _| Ok(()),
                &|path| {
                    send.send(note_path(path.strip_prefix(&work_root).unwrap()))
                        .unwrap();
                    let (lock, signal) = &*read_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = signal.wait(released).unwrap();
                    }
                    std::fs::read(path)
                },
            )
            .unwrap()
        });
        let first: std::collections::BTreeSet<_> = (0..SOURCE_READ_WORKERS)
            .map(|_| {
                receive
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
            })
            .collect();
        assert_eq!(
            first, preferred,
            "all first-wave workers take priority paths; positive control includes rest notes"
        );
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let (vault, snapshot, _) = worker.join().unwrap();
        let (serial, full, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(
            vault
                .notes
                .iter()
                .map(|note| &note.path)
                .collect::<Vec<_>>(),
            serial
                .notes
                .iter()
                .map(|note| &note.path)
                .collect::<Vec<_>>()
        );
        assert_eq!(snapshot.sources().unwrap(), full.sources().unwrap());
        assert_eq!(
            serde_json::to_value(vault.backlink_map).unwrap(),
            serde_json::to_value(serial.backlink_map).unwrap()
        );
    }

    /// #652: only a snapshot proven identical to the bank file skips the write.
    #[test]
    fn unchanged_reconcile_keeps_identity_only_for_untouched_persisted_bank() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.md"), "# A\n[[b]]").unwrap();
        std::fs::write(root.join("b.md"), "# B").unwrap();
        let checkpoint = &mut |_: &str, _| Ok(());
        let (vault, cold, _) = reconcile(&root, None, false, checkpoint).unwrap();
        assert!(
            !cold.is_persisted_in(&cache),
            "fresh reconcile is never persisted"
        );
        save_provisional(&cold, &vault, &cache, Some("a.md")).unwrap();

        let loaded = Snapshot::load_checked(&cache, &root).unwrap();
        assert!(loaded.is_persisted_in(&cache));
        let (_, warm, stats) = reconcile(&root, Some(&loaded), false, checkpoint).unwrap();
        assert_eq!((stats.read, stats.reused), (0, 2));
        assert_eq!(warm.id, cold.id);
        assert!(warm.is_persisted_in(&cache));
        assert!(!warm.is_persisted_in(&temp.path().join("other")));
        let mut renamed = warm.clone();
        renamed.id = uuid::Uuid::new_v4().to_string();
        assert!(
            !renamed.is_persisted_in(&cache),
            "a changed public snapshot identity must be persisted"
        );
        let mut regenerated = warm.clone();
        regenerated.search_generation = Some("a".repeat(64));
        assert!(
            !regenerated.is_persisted_in(&cache),
            "new generation is saved"
        );

        // Invalidated revisions and a replaced bank are never assumed persisted.
        let mut invalidated = Snapshot::load_checked(&cache, &root).unwrap();
        invalidated.invalidate_paths(&["a.md".into()]);
        let (_, reread, stats) = reconcile(&root, Some(&invalidated), false, checkpoint).unwrap();
        assert_eq!(stats.read, 1, "invalidation positive control");
        assert_ne!(reread.id, cold.id);
        assert!(!reread.is_persisted_in(&cache));
        std::thread::sleep(std::time::Duration::from_millis(20));
        cold.save(&cache).unwrap();
        assert!(!warm.is_persisted_in(&cache), "rewritten bank file");

        // A changed note gets a new identity and is saved.
        std::fs::write(root.join("b.md"), "# B changed").unwrap();
        let loaded = Snapshot::load_checked(&cache, &root).unwrap();
        let (vault, changed, _) = reconcile(&root, Some(&loaded), false, checkpoint).unwrap();
        assert_ne!(changed.id, cold.id);
        assert!(!changed.is_persisted_in(&cache));
        save_provisional(&changed, &vault, &cache, Some("a.md")).unwrap();
        let saved = Snapshot::load_checked(&cache, &root).unwrap();
        assert_eq!(saved.id, changed.id);
        assert_eq!(saved.source("b.md").as_deref(), Some("# B changed"));

        // A bank completed by an incremental delta differs from its file.
        let state = incremental::State::new(vault, saved);
        assert!(!state.snapshot.is_persisted_in(&cache));
        state.persist_delta(&cache).unwrap();
        let with_delta = Snapshot::load_checked(&cache, &root).unwrap();
        assert!(!with_delta.is_persisted_in(&cache));
        let (_, after_delta, _) = reconcile(&root, Some(&with_delta), false, checkpoint).unwrap();
        assert_ne!(
            after_delta.id, changed.id,
            "delta-applied bank is rewritten"
        );
    }
}
