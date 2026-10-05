//! Disposable Reader snapshots. Canonical notes remain the only source of truth.
use super::*;
use anyhow::{bail, ensure};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::collections::BTreeMap;
use std::io::{Read, Write};
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
    /// Imported mtime can be rounded even on a precise native filesystem. Unix
    /// ctime also changes on same-size writes with a restored mtime. Reuse still
    /// requires equality of the entire revision, never just either timestamp.
    pub fn is_precise(&self) -> bool {
        !self.modified.is_multiple_of(1_000_000_000) || self.precise_change_time()
    }

    fn precise_change_time(&self) -> bool {
        cfg!(unix) && self.changed.rem_euclid(1_000_000_000) != 0
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
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
}

#[derive(Clone, Serialize, Deserialize)]
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
    links: HashMap<String, Vec<Backlink>>,
    unreadable: Vec<CachedUnreadable>,
    primary: Option<(String, Source)>,
    pub search_generation: Option<String>,
    #[serde(skip)]
    previous: Option<Box<Snapshot>>,
    #[serde(skip)]
    latest: Option<(String, Source)>,
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
    pub graph_reused: bool,
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
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Reader source bank exceeds size limit"
        );
        let snapshot: Self = serde_json::from_slice(&bytes).context("Decode Reader source bank")?;
        ensure!(
            snapshot.schema == SCHEMA,
            "Reader source bank schema mismatch"
        );
        ensure!(
            snapshot.root == root.canonicalize()?,
            "Reader source bank root mismatch"
        );
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
            }
        }
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
    let canonical = root
        .canonicalize()
        .with_context(|| format!("Resolve vault directory {}", display_path(root)))?;
    let previous = previous.filter(|p| p.root == canonical && p.schema == SCHEMA);
    let mut vault = Vault::scan_metadata_with(root, checkpoint)?;
    let mut sources = BTreeMap::new();
    let mut stats = ReconcileStats::default();
    for (count, note) in vault.notes.iter().enumerate() {
        checkpoint("Checking notes", count)?;
        let path = root.join(&note.path);
        if super::cloud_placeholder(&path) {
            vault.unreadable.push(UnreadableEntry {
                path,
                operation: "read note",
                error: "iCloud placeholder is not downloaded".into(),
            });
            continue;
        }
        let before = SourceRevision::read(&path).ok();
        let old = previous.and_then(|p| p.sources.get(&note.path));
        if !stats.reuse.needs_read(before.as_ref(), old, force_read) {
            sources.insert(note.path.clone(), old.unwrap().clone());
            stats.reused += 1;
            continue;
        }
        stats.read += 1;
        match read(&path) {
            Ok(bytes) => {
                if let Err(error) = std::str::from_utf8(&bytes) {
                    vault.unreadable.push(UnreadableEntry {
                        path,
                        operation: "decode note",
                        error: error.to_string(),
                    });
                    continue;
                }
                let after = SourceRevision::read(&path).ok();
                let stamp = (before == after).then_some(after).flatten();
                sources.insert(
                    note.path.clone(),
                    Source {
                        stamp,
                        bytes: STANDARD.encode(bytes),
                    },
                );
            }
            Err(error) => vault.unreadable.push(UnreadableEntry {
                path,
                operation: "read note",
                error: error.to_string(),
            }),
        }
    }
    vault.finish_scan_report();
    let same = stats.read == 0
        && previous.is_some_and(|p| p.entries == vault.entries && p.sources.len() == sources.len());
    stats.graph_reused = same;
    if same {
        vault.backlink_map = previous.unwrap().links.clone();
    } else {
        vault.build_backlinks_from(checkpoint, |path| {
            let bytes = STANDARD.decode(&sources.get(path)?.bytes).ok()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        })?;
    }
    let snapshot = Snapshot {
        schema: SCHEMA,
        root: canonical,
        id: uuid::Uuid::new_v4().to_string(),
        entries: vault.entries.clone(),
        links: vault.backlink_map.clone(),
        sources,
        unreadable: vault
            .unreadable
            .iter()
            .map(|item| CachedUnreadable {
                path: item.path.clone(),
                operation: item.operation.into(),
                error: item.error.clone(),
            })
            .collect(),
        search_generation: if same {
            previous.and_then(|p| p.search_generation.clone())
        } else {
            None
        },
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
    snapshot.save(base)?;
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
        search_generation: snapshot.search_generation.clone(),
        previous: None,
        latest: None,
    };
    let mut file = tempfile::NamedTempFile::new_in(base)?;
    write_json(file.as_file_mut(), &startup)?;
    ensure!(
        file.as_file().metadata()?.len() <= MAX_BYTES,
        "Startup snapshot exceeds size limit"
    );
    file.flush()?;
    file.persist(base.join("reader-startup.json"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                let symbol = dlsym(std::ptr::null_mut(), c"tessera_slow_fs_phase".as_ptr());
                assert!(!symbol.is_null(), "use the slow-vault-fs preload");
                let set: unsafe extern "C" fn(*const std::ffi::c_char) =
                    std::mem::transmute(symbol);
                set(name.as_ptr());
            }
        }
        let temp = tempfile::Builder::new()
            .prefix("tessera-cloud-graph-")
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
        let delay: u64 = std::env::var("TESSERA_SLOW_FS_MS")
            .unwrap()
            .parse()
            .unwrap();
        let start = std::time::Instant::now();
        std::fs::metadata(&absolute).unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_millis(delay));
        let start = std::time::Instant::now();
        assert_eq!(std::fs::read_to_string(&absolute).unwrap(), "# Target");
        assert!(start.elapsed() >= std::time::Duration::from_millis(delay));
        phase(c"setup");
        let samples: usize = std::env::var("TESSERA_CLOUD_PROFILE_SAMPLES")
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
                let symbol = dlsym(std::ptr::null_mut(), c"tessera_slow_fs_phase".as_ptr());
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
            .prefix("tessera-cloud-link-")
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
        let delay: u64 = std::env::var("TESSERA_SLOW_FS_MS")
            .unwrap()
            .parse()
            .unwrap();
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
        let samples: usize = std::env::var("TESSERA_CLOUD_PROFILE_SAMPLES")
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
        assert!(rendered.rendered.contains("tessera://unresolved/"));
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

    #[cfg(unix)]
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
            "precise ctime positive control"
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
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
        )
        .unwrap();
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
            cfg!(unix),
            "only native Unix change time can strengthen reuse"
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
                let symbol = dlsym(std::ptr::null_mut(), c"tessera_slow_fs_phase".as_ptr());
                assert!(!symbol.is_null(), "run with slow-vault-fs preload");
                let set: unsafe extern "C" fn(*const std::ffi::c_char) =
                    std::mem::transmute(symbol);
                set(name.as_ptr());
            }
        }
        let temp = tempfile::Builder::new()
            .prefix("tessera-coarse-reconcile-")
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
        let metadata_us: u64 = std::env::var("TESSERA_SLOW_FS_METADATA_US")
            .unwrap()
            .parse()
            .unwrap();
        let read_ms: u64 = std::env::var("TESSERA_SLOW_FS_MS")
            .unwrap()
            .parse()
            .unwrap();
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
        let samples = std::env::var("TESSERA_CLOUD_PROFILE_SAMPLES")
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
                .env("TESSERA_RELAUNCH_FIXTURE", temp.path())
                .env("TESSERA_RELAUNCH_SAMPLE", launch.to_string())
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
            std::env::var_os("TESSERA_RELAUNCH_FIXTURE")
                .expect("run imported_mtime_warm_reconcile_profile"),
        );
        let launch = std::env::var("TESSERA_RELAUNCH_SAMPLE").unwrap();
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
            let symbol = dlsym(std::ptr::null_mut(), c"tessera_slow_fs_phase".as_ptr());
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
}
