//! A worker-owned reconciled baseline. Canonical I/O is limited to batch paths;
//! inventory-dependent referrers are reparsed from cached bytes.
use super::*;
use std::collections::BTreeSet;

pub struct State {
    pub snapshot: Snapshot,
    pub vault: Vault,
    pub candidates: std::sync::Arc<crate::link_candidates::CandidateIndex>,
    delta: Delta,
}

pub struct Batch {
    /// Includes changed paths and cached referrers whose resolution changed.
    pub affected: BTreeSet<String>,
    pub changed: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    pub read: usize,
    pub topology_changed: bool,
}

impl State {
    /// Build after Reader Ready on the background worker, never a publication gate.
    pub fn new(vault: Vault, mut snapshot: Snapshot) -> Self {
        // Batches mutate this baseline; it no longer describes the source bank file.
        snapshot.persisted = None;
        let delta = Delta {
            schema: SCHEMA,
            root: snapshot.root.clone(),
            source_id: snapshot.id.clone(),
            entries: BTreeMap::new(),
            sources: BTreeMap::new(),
            links: BTreeMap::new(),
            graph_sources: BTreeSet::new(),
            unreadable: snapshot.unreadable.clone(),
            search_generation: snapshot.search_generation.clone(),
        };
        let candidates = std::sync::Arc::new(
            crate::link_candidates::CandidateIndex::from_snapshot(&snapshot),
        );
        Self {
            vault,
            snapshot,
            candidates,
            delta,
        }
    }

    pub fn set_search_generation(&mut self, generation: Option<String>) {
        self.delta.search_generation = generation.clone();
        self.snapshot.search_generation = generation;
    }

    /// Write only cumulative changed sources/graph rows, never the full source bank.
    pub fn persist_delta(&self, base: &Path) -> Result<()> {
        let ancestor = base
            .ancestors()
            .find(|path| path.exists())
            .context("Cache has no ancestor")?;
        let resolved = ancestor.canonicalize()?.join(base.strip_prefix(ancestor)?);
        ensure!(
            !resolved.starts_with(&self.snapshot.root),
            "Derived cache must be outside vault"
        );
        std::fs::create_dir_all(base)?;
        let mut file = tempfile::NamedTempFile::new_in(base)?;
        write_json(file.as_file_mut(), &self.delta)?;
        ensure!(
            file.as_file().metadata()?.len() <= MAX_BYTES,
            "Incremental snapshot exceeds size limit"
        );
        file.flush()?;
        file.persist(base.join("reader-delta.json"))?;
        Ok(())
    }

    /// A directory notification is a hint about its immediate children, not a
    /// dropped-event signal. Existing subtrees remain untouched; only new trees
    /// are enumerated recursively. MustScan/overflow still requires reconcile.
    fn expand_topology(
        &self,
        input: &crate::Changes,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<(crate::Changes, BTreeMap<String, bool>)> {
        let mut changes = input.clone();
        let mut directories = BTreeMap::new();
        // Existing save batches need no cloned inventory lookup or directory I/O.
        if input.directories.is_empty()
            && input.changed.iter().all(|path| {
                Path::new(path)
                    .ancestors()
                    .skip(1)
                    .filter(|p| !p.as_os_str().is_empty())
                    .all(|p| {
                        self.vault.entries.iter().any(|entry| {
                            entry.path == note_path(p) && entry.kind == EntryKind::Directory
                        })
                    })
            })
        {
            return Ok((changes, directories));
        }

        let known: BTreeMap<_, _> = self
            .vault
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.kind))
            .collect();
        let mut children: BTreeMap<PathBuf, Vec<(&String, &EntryKind)>> = BTreeMap::new();
        for (name, kind) in &known {
            children
                .entry(Path::new(name).parent().unwrap_or(Path::new("")).to_owned())
                .or_default()
                .push((name, kind));
        }
        let mut pending = input.directories.clone();
        // A known mutation may create validated ancestors without a watcher.
        for path in input.changed.iter().chain(&input.directories) {
            ensure!(
                path.is_empty() || relative(path),
                "Invalid topology identity"
            );
            let mut parent = Path::new(path).parent();
            while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
                let name = note_path(p);
                if !known.contains_key(&name) {
                    pending.insert(name);
                }
                parent = p.parent();
            }
        }
        let mut visited = BTreeSet::new();
        while let Some(path) = pending.pop_first() {
            ensure!(path.is_empty() || relative(&path), "Invalid directory hint");
            if !visited.insert(path.clone()) {
                continue;
            }
            ensure!(
                changes.changed.len() + changes.removed.len() + directories.len()
                    < crate::watch::BULK_THRESHOLD,
                "Bulk topology requires reconciliation"
            );
            checkpoint("Checking changed folders", visited.len())?;
            let absolute = self.vault.root.join(&path);
            ensure!(
                path.is_empty()
                    || known.contains_key(&path)
                    || !known
                        .keys()
                        .any(|name| name.to_lowercase() == path.to_lowercase()),
                "Case-only directory identity requires reconciliation"
            );
            if known.get(&path) == Some(&EntryKind::Attachment) {
                let meta = std::fs::symlink_metadata(&absolute)?;
                if meta.is_file() && !meta.file_type().is_symlink() {
                    continue;
                }
            }
            // Validate every ancestor, including an existing directory swapped
            // for a symlink. No filesystem probe may escape the vault boundary.
            let mut ancestor = Some(Path::new(&path));
            let mut missing = false;
            while let Some(p) = ancestor.filter(|p| !p.as_os_str().is_empty()) {
                match std::fs::symlink_metadata(self.vault.root.join(p)) {
                    Ok(meta) => ensure!(
                        meta.is_dir() && !meta.file_type().is_symlink(),
                        "Unsafe directory hint requires reconciliation"
                    ),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing = true,
                    Err(e) => return Err(e.into()),
                }
                ancestor = p.parent();
            }
            if missing {
                for (name, kind) in &known {
                    if Path::new(name).starts_with(&path) {
                        match kind {
                            EntryKind::Markdown => {
                                changes.removed.insert(name.clone());
                            }
                            EntryKind::Directory => {
                                directories.insert(name.clone(), false);
                            }
                            _ => bail!("Removed non-note topology requires reconciliation"),
                        }
                    }
                }
                continue;
            }
            if !path.is_empty() && !known.contains_key(&path) {
                directories.insert(path.clone(), true);
            }
            let mut found = BTreeSet::new();
            for entry in std::fs::read_dir(&absolute)? {
                checkpoint("Checking changed folders", visited.len())?;
                ensure!(
                    changes.changed.len() + changes.removed.len() + directories.len()
                        < crate::watch::BULK_THRESHOLD,
                    "Bulk topology requires reconciliation"
                );
                let entry = entry?;
                let child = note_path(entry.path().strip_prefix(&self.vault.root)?);
                if super::super::service_path(Path::new(&child))
                    || entry
                        .file_name()
                        .as_encoded_bytes()
                        .starts_with(b".okilum-save-")
                {
                    continue;
                }
                found.insert(child.clone());
                let kind = entry.file_type()?;
                ensure!(
                    !kind.is_symlink(),
                    "Symlink topology requires reconciliation"
                );
                // Scanned placeholders have no inventory identity or cached bytes.
                // Preserve their warning without turning every parent event into
                // another full reconcile or admitting a dataless note.
                if !known.contains_key(&child)
                    && self
                        .vault
                        .unreadable
                        .iter()
                        .any(|item| item.path == entry.path())
                    && super::super::cloud_placeholder(&entry.path())
                {
                    continue;
                }
                if kind.is_dir() {
                    ensure!(
                        known
                            .get(&child)
                            .is_none_or(|kind| *kind == EntryKind::Directory),
                        "Changed entry kind requires reconciliation"
                    );
                    if !known.contains_key(&child) {
                        pending.insert(child);
                    }
                } else if kind.is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                {
                    let revision = SourceRevision::read(&entry.path()).ok();
                    if ReuseDiagnostics::default().needs_read(
                        revision.as_ref(),
                        self.snapshot.sources.get(&child),
                        false,
                    ) {
                        ensure!(
                            known.contains_key(&child)
                                || !super::super::cloud_placeholder(&entry.path()),
                            "New placeholder requires reconciliation"
                        );
                        changes.changed.insert(child);
                    }
                } else {
                    ensure!(
                        known.get(&child) == Some(&EntryKind::Attachment),
                        "New non-note topology requires reconciliation"
                    );
                }
            }
            for (name, kind) in children
                .get(Path::new(&path))
                .into_iter()
                .flatten()
                .copied()
            {
                if !found.contains(name) {
                    match kind {
                        EntryKind::Markdown => {
                            changes.removed.insert(name.clone());
                        }
                        EntryKind::Directory => {
                            pending.insert(name.clone());
                        }
                        _ => bail!("Removed non-note topology requires reconciliation"),
                    }
                }
            }
        }
        ensure!(
            changes.changed.len() + changes.removed.len() + directories.len()
                < crate::watch::BULK_THRESHOLD,
            "Bulk topology requires reconciliation"
        );
        Ok((changes, directories))
    }

    pub fn apply(
        &mut self,
        changes: &crate::Changes,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<Batch> {
        self.apply_with_reader(changes, checkpoint, &mut |path| std::fs::read(path))
    }

    pub fn apply_with_reader(
        &mut self,
        changes: &crate::Changes,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
        read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
    ) -> Result<Batch> {
        ensure!(
            self.candidates.root() == self.vault.root && self.snapshot.root == self.vault.root,
            "Foreign incremental baseline"
        );
        ensure!(
            !changes.rescan,
            "Dropped events require full reconciliation"
        );
        let (changes, directories) = self.expand_topology(changes, checkpoint)?;
        ensure!(
            !changes.removed.iter().any(|removed| changes
                .changed
                .iter()
                .any(|changed| removed != changed
                    && removed.to_lowercase() == changed.to_lowercase())),
            "Case-only rename requires reconciliation"
        );
        let touched: BTreeSet<_> = changes.changed.union(&changes.removed).cloned().collect();
        let mut affected = touched.clone();
        for path in directories.keys() {
            affected.extend(self.candidates.referrers_for(path));
        }
        let mut removed = BTreeSet::new();
        let mut changed = BTreeSet::new();
        let mut reads = 0;
        let mut topology_changed = !directories.is_empty();
        // Validate the entire batch before mutating: never follow a newly-created
        // symlink, service entry, non-note, or unobserved ancestor directory.
        let mut identities = Vec::new();
        for (count, path) in touched.iter().enumerate() {
            checkpoint("Checking changed notes", count)?;
            ensure!(
                relative(path)
                    && Path::new(path)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md")),
                "Invalid incremental note identity"
            );
            let absolute = self.vault.root.join(path);
            let mut parent = Path::new(path).parent();
            while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
                ensure!(
                    self.vault
                        .entries
                        .iter()
                        .any(|entry| entry.path == note_path(p)
                            && entry.kind == EntryKind::Directory)
                        || directories.get(&note_path(p)) == Some(&true),
                    "Unobserved parent requires full reconciliation"
                );
                match std::fs::symlink_metadata(self.vault.root.join(p)) {
                    Ok(meta) => ensure!(
                        meta.is_dir() && !meta.file_type().is_symlink(),
                        "Changed parent requires full reconciliation"
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                parent = p.parent();
            }
            let mut placeholder = false;
            let present = match std::fs::symlink_metadata(&absolute) {
                Ok(meta) => {
                    ensure!(
                        meta.is_file() && !meta.file_type().is_symlink(),
                        "Changed file type requires full reconciliation"
                    );
                    placeholder = super::super::cloud_placeholder(&absolute);
                    !placeholder
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    ensure!(
                        self.vault.notes.iter().any(|note| note.path == *path),
                        "Unreadable new identity requires full reconciliation"
                    );
                    true
                }
                Err(error) => return Err(error.into()),
            };
            let was_present = self.vault.notes.iter().any(|note| note.path == *path);
            if was_present != present {
                topology_changed = true;
                affected.extend(self.candidates.referrers_for(path));
            }
            identities.push((path.clone(), present, placeholder));
        }
        for (path, present) in &directories {
            self.vault.entries.retain(|entry| entry.path != *path);
            if *present {
                self.vault.entries.push(VaultEntry {
                    path: path.clone(),
                    kind: EntryKind::Directory,
                });
                self.vault.occupied_paths.insert(path.to_lowercase());
            } else {
                self.vault.occupied_paths.remove(&path.to_lowercase());
            }
        }
        self.vault.entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut targets: BTreeSet<_> = self
            .vault
            .backlink_map
            .iter()
            .filter(|(target, incoming)| {
                touched.contains(*target)
                    || incoming.iter().any(|link| affected.contains(&link.path))
            })
            .map(|(target, _)| target.clone())
            .collect();
        // Materialized/deleted placeholder warnings do not survive a scoped
        // directory discovery. An unchanged/denied warning remains visible.
        let before_unreadable = self.vault.unreadable.len();
        self.vault.unreadable.retain(|item| {
            let Ok(relative) = item.path.strip_prefix(&self.vault.root) else {
                return true;
            };
            let scoped = input_directory_scope(relative, &changes.directories, &directories);
            !scoped
                || !std::fs::symlink_metadata(&item.path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        });
        topology_changed |= before_unreadable != self.vault.unreadable.len();
        for (path, present, placeholder) in identities {
            checkpoint("Reading changed notes", reads)?;
            self.vault
                .unreadable
                .retain(|entry| entry.path != self.vault.root.join(&path));
            if placeholder {
                self.vault.unreadable.push(UnreadableEntry {
                    path: self.vault.root.join(&path),
                    operation: "read note",
                    error: "iCloud placeholder is not downloaded".into(),
                });
            }
            if self.vault.notes.iter().any(|note| note.path == path) != present {
                self.vault.set_note_identity(&path, present);
            }
            std::sync::Arc::make_mut(&mut self.candidates).remove(&path);
            self.snapshot.sources.remove(&path);
            if present {
                // An explicit filesystem/save event is stronger than equal stamps.
                let checked = check_source(&self.vault.root, &path, None, true, read);
                reads += checked.stats.read;
                self.vault.unreadable.extend(checked.unreadable);
                if let Some(source) = checked.source {
                    let raw = String::from_utf8(STANDARD.decode(&source.bytes)?)?;
                    std::sync::Arc::make_mut(&mut self.candidates).update(
                        &path,
                        &raw,
                        source.stamp.clone(),
                    );
                    self.snapshot.sources.insert(path.clone(), source);
                }
                changed.insert(path);
            } else {
                removed.insert(path);
            }
        }
        let snapshot = &self.snapshot;
        self.vault
            .refresh_backlinks_from(&affected, checkpoint, |path| snapshot.source(path))?;
        self.vault.inventory_complete = self.vault.unreadable.is_empty();
        self.snapshot.entries = self.vault.entries.clone();
        targets.extend(
            self.vault
                .backlink_map
                .iter()
                .filter(|(target, incoming)| {
                    touched.contains(*target)
                        || incoming.iter().any(|link| affected.contains(&link.path))
                })
                .map(|(target, _)| target.clone()),
        );
        for target in &targets {
            self.snapshot.links.remove(target);
            if let Some(incoming) = self.vault.backlink_map.get(target) {
                self.snapshot.links.insert(target.clone(), incoming.clone());
            }
        }
        self.snapshot.unreadable = self
            .vault
            .unreadable
            .iter()
            .map(|item| CachedUnreadable {
                path: item.path.clone(),
                operation: item.operation.into(),
                error: item.error.clone(),
            })
            .collect();
        // Session search is mutable, and must never be advertised as the old
        // content-addressed, completed disk generation.
        if !affected.is_empty() {
            self.set_search_generation(None);
        }
        for path in &touched {
            self.delta.entries.insert(
                path.clone(),
                self.vault
                    .entries
                    .iter()
                    .find(|entry| entry.path == *path)
                    .cloned(),
            );
            self.delta
                .sources
                .insert(path.clone(), self.snapshot.sources.get(path).cloned());
        }
        for path in directories.keys() {
            self.delta.entries.insert(
                path.clone(),
                self.vault.entries.iter().find(|e| e.path == *path).cloned(),
            );
        }
        // The durable delta stores changed-source edges only. A popular target
        // must not serialize thousands of unrelated backlinks on every save.
        self.delta.graph_sources.extend(affected.iter().cloned());
        for target in targets {
            let incoming: Vec<_> = self
                .vault
                .backlink_map
                .get(&target)
                .into_iter()
                .flatten()
                .filter(|link| self.delta.graph_sources.contains(&link.path))
                .cloned()
                .collect();
            self.delta.links.insert(target, Some(incoming));
        }
        self.delta.unreadable = self.snapshot.unreadable.clone();
        Ok(Batch {
            affected,
            changed,
            removed,
            read: reads,
            topology_changed,
        })
    }
}

fn input_directory_scope(
    path: &Path,
    hints: &BTreeSet<String>,
    directories: &BTreeMap<String, bool>,
) -> bool {
    hints
        .iter()
        .any(|hint| path.parent() == Some(Path::new(hint)))
        || directories
            .iter()
            .any(|(directory, present)| !present && path.starts_with(directory))
}

#[derive(Serialize, Deserialize)]
pub(super) struct Delta {
    schema: u32,
    root: PathBuf,
    source_id: String,
    entries: BTreeMap<String, Option<VaultEntry>>,
    sources: BTreeMap<String, Option<Source>>,
    links: BTreeMap<String, Option<Vec<Backlink>>>,
    graph_sources: BTreeSet<String>,
    unreadable: Vec<CachedUnreadable>,
    search_generation: Option<String>,
}

impl Delta {
    pub(super) fn load(base: &Path, root: &Path, id: Option<&str>) -> Result<Option<Self>> {
        let Some(id) = id else { return Ok(None) };
        let path = base.join("reader-delta.json");
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_BYTES,
            "Invalid incremental snapshot size/type"
        );
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Incremental snapshot exceeds size limit"
        );
        let delta: Self = serde_json::from_slice(&bytes)?;
        if delta.schema != SCHEMA || delta.root != root || delta.source_id != id {
            return Ok(None);
        }
        for (path, entry) in &delta.entries {
            ensure!(
                relative(path)
                    && entry.as_ref().is_none_or(|entry| entry.path == *path
                        && matches!(entry.kind, EntryKind::Markdown | EntryKind::Directory)),
                "Invalid incremental inventory identity"
            );
        }
        for (path, source) in &delta.sources {
            ensure!(relative(path), "Invalid incremental source identity");
            if let Some(source) = source {
                String::from_utf8(STANDARD.decode(&source.bytes)?)?;
            }
        }
        ensure!(
            delta.search_generation.as_ref().is_none_or(
                |name| name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
            ),
            "Invalid incremental search generation"
        );
        ensure!(
            delta.links.keys().all(|path| relative(path))
                && delta.graph_sources.iter().all(|path| relative(path)),
            "Invalid incremental graph identity"
        );
        Ok(Some(delta))
    }

    fn apply_inventory(
        &self,
        entries: &mut Vec<VaultEntry>,
        links: &mut HashMap<String, Vec<Backlink>>,
        unreadable: &mut Vec<CachedUnreadable>,
    ) {
        entries.retain(|entry| !self.entries.contains_key(&entry.path));
        entries.extend(self.entries.values().filter_map(Clone::clone));
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let notes: std::collections::HashSet<_> = entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::Markdown)
            .map(|entry| &entry.path)
            .collect();
        links.retain(|target, incoming| {
            incoming.retain(|link| !self.graph_sources.contains(&link.path));
            notes.contains(target) && !incoming.is_empty()
        });
        for (path, incoming) in &self.links {
            if notes.contains(path) {
                if let Some(incoming) = incoming {
                    let selected = links.entry(path.clone()).or_default();
                    selected.extend(incoming.iter().cloned());
                    selected.sort_by(|a, b| a.path.cmp(&b.path));
                }
            }
        }
        links.retain(|_, incoming| !incoming.is_empty());
        *unreadable = self.unreadable.clone();
    }

    pub(super) fn apply_snapshot(&self, snapshot: &mut Snapshot) {
        self.apply_inventory(
            &mut snapshot.entries,
            &mut snapshot.links,
            &mut snapshot.unreadable,
        );
        for (path, source) in &self.sources {
            snapshot.sources.remove(path);
            if let Some(source) = source {
                snapshot.sources.insert(path.clone(), source.clone());
            }
        }
        snapshot.search_generation = self.search_generation.clone();
    }

    pub(super) fn apply_startup(&self, snapshot: &mut StartupSnapshot) {
        self.apply_inventory(
            &mut snapshot.entries,
            &mut snapshot.links,
            &mut snapshot.unreadable,
        );
        for selected in [&mut snapshot.primary, &mut snapshot.latest] {
            if let Some((path, _)) = selected {
                if let Some(source) = self.sources.get(path) {
                    *selected = source.clone().map(|source| (path.clone(), source));
                }
            }
        }
        snapshot.search_generation = self.search_generation.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, State) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(
            root.join("A.md"),
            "# A\n\n[[Target]]\n\n[Relative](dir/Target.md)\n\n[[Missing]]\n\n![[Missing]]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("B.md"),
            "---\nrelated: '[[Target]]'\n---\n# B\n\n[Ref][x]\n\n[x]: dir/Target.md\n",
        )
        .unwrap();
        std::fs::write(root.join("Target.md"), "# Root target").unwrap();
        std::fs::write(root.join("dir/Target.md"), "# Directory target").unwrap();
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        (temp, State::new(vault, snapshot))
    }

    fn compare(state: &State) {
        let (full, snapshot, _) =
            reconcile(&state.vault.root, None, false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(
            state.snapshot.sources().unwrap(),
            snapshot.sources().unwrap()
        );
        assert_eq!(state.vault.entries, full.entries);
        assert_eq!(
            serde_json::to_value(&state.vault.backlink_map).unwrap(),
            serde_json::to_value(full.backlink_map).unwrap()
        );
    }

    #[test]
    fn directory_hints_discover_only_new_subtrees_and_persist_create_move_delete_undo() {
        let (temp, mut state) = fixture();
        let cache = temp.path().join("cache");
        save_provisional(&state.snapshot, &state.vault, &cache, Some("A.md")).unwrap();
        std::fs::create_dir_all(state.vault.root.join("new/child")).unwrap();
        std::fs::write(
            state.vault.root.join("new/child/Missing.md"),
            "# Born\nuniquecreatedword",
        )
        .unwrap();
        let mut reads = Vec::new();
        let batch = state
            .apply_with_reader(
                &crate::Changes {
                    directories: BTreeSet::from(["".into()]),
                    ..Default::default()
                },
                &mut |phase, _| {
                    assert_ne!(phase, "Discovering notes");
                    Ok(())
                },
                &mut |path| {
                    reads.push(path.to_owned());
                    std::fs::read(path)
                },
            )
            .unwrap();
        assert_eq!(reads, [state.vault.root.join("new/child/Missing.md")]);
        assert_eq!(batch.read, 1);
        assert!(batch.affected.contains("A.md"));
        assert_eq!(state.vault.backlinks("new/child/Missing.md").len(), 2);
        compare(&state);
        std::fs::write(
            state.vault.root.join("dir/Target.md"),
            "# Changed by an unreported sibling edit",
        )
        .unwrap();
        let sibling = state
            .apply(
                &crate::Changes {
                    directories: BTreeSet::from(["dir".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(
            sibling.read, 1,
            "directory-only hint detects precise revision changes"
        );
        compare(&state);
        let echo = state
            .apply_with_reader(
                &crate::Changes {
                    directories: BTreeSet::from(["".into(), "new/child".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
                &mut |_| panic!("precise directory echo must not open canonical sources"),
            )
            .unwrap();
        assert!(!echo.topology_changed && echo.affected.is_empty());
        // A directory move may report no individual Markdown endpoints.
        std::fs::rename(state.vault.root.join("new"), state.vault.root.join("moved")).unwrap();
        let batch = state
            .apply(
                &crate::Changes {
                    directories: BTreeSet::from(["new".into(), "moved".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(batch.read, 1);
        assert!(batch.removed.contains("new/child/Missing.md"));
        compare(&state);
        let retained = temp.path().join("retained");
        std::fs::rename(state.vault.root.join("moved"), &retained).unwrap();
        let deleted = state
            .apply(
                &crate::Changes {
                    directories: BTreeSet::from(["moved".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(deleted.read, 0);
        assert!(state.vault.backlinks("moved/child/Missing.md").is_empty());
        compare(&state);
        std::fs::rename(retained, state.vault.root.join("moved")).unwrap();
        state
            .apply(
                &crate::Changes {
                    directories: BTreeSet::from(["moved".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        compare(&state);
        state.persist_delta(&cache).unwrap();
        let loaded = Snapshot::load_checked(&cache, &state.vault.root).unwrap();
        assert_eq!(loaded.entries, state.vault.entries);
        assert_eq!(loaded.sources().unwrap(), state.snapshot.sources().unwrap());
        assert_eq!(
            serde_json::to_value(&loaded.links).unwrap(),
            serde_json::to_value(&state.snapshot.links).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_hint_rejects_symlink_ancestors_before_any_source_read_or_mutation() {
        let (temp, mut state) = fixture();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(outside.join("child")).unwrap();
        std::fs::write(outside.join("child/Secret.md"), "private").unwrap();
        std::os::unix::fs::symlink(&outside, state.vault.root.join("linked")).unwrap();
        let before = state.snapshot.sources().unwrap();
        assert!(state
            .apply_with_reader(
                &crate::Changes {
                    directories: BTreeSet::from(["linked/child".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
                &mut |_| panic!("never follow a link")
            )
            .is_err());
        assert_eq!(before, state.snapshot.sources().unwrap());
    }

    #[test]
    fn known_cloud_placeholders_and_attachment_hints_do_not_reconcile_or_admit_notes() {
        let (temp, state) = fixture();
        let root = state.vault.root.clone();
        std::fs::write(root.join(".Note.md.icloud"), "evicted stub").unwrap();
        std::fs::write(root.join(".DS_Store"), "metadata").unwrap();
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        let mut state = State::new(vault, snapshot);
        assert_eq!(state.vault.unreadable.len(), 1);
        let batch = state
            .apply_with_reader(
                &crate::Changes {
                    directories: BTreeSet::from(["".into(), ".DS_Store".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
                &mut |_| {
                    panic!("known placeholder must never open or re-admit canonical note bytes")
                },
            )
            .unwrap();
        assert!(batch.affected.is_empty());
        assert_eq!(state.vault.unreadable.len(), 1);
        assert!(!state
            .vault
            .entries
            .iter()
            .any(|entry| entry.path.contains("icloud")));
        std::fs::remove_file(root.join(".Note.md.icloud")).unwrap();
        std::fs::write(root.join("Note.md"), "# Downloaded").unwrap();
        let batch = state
            .apply(
                &crate::Changes {
                    directories: BTreeSet::from(["".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(batch.read, 1);
        assert!(state.vault.unreadable.is_empty());
        compare(&state);
        let cache = temp.path().join("cache");
        save_provisional(&state.snapshot, &state.vault, &cache, None).unwrap();
        std::fs::write(root.join(".New.md.icloud"), "new stub").unwrap();
        assert!(state
            .apply(
                &crate::Changes {
                    directories: BTreeSet::from(["".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(())
            )
            .err()
            .unwrap()
            .to_string()
            .contains("non-note topology"));
    }

    #[test]
    fn case_only_endpoints_and_bulk_topology_fall_back_before_mutation_or_reads() {
        let (_temp, mut state) = fixture();
        let before = state.snapshot.sources().unwrap();
        let entries = state.vault.entries.clone();
        let error = state
            .apply_with_reader(
                &crate::Changes {
                    changed: BTreeSet::from(["target.md".into()]),
                    removed: BTreeSet::from(["Target.md".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
                &mut |_| panic!("case-only rename needs an unambiguous inventory"),
            )
            .err()
            .unwrap();
        assert!(error.to_string().contains("Case-only"));
        std::fs::create_dir(state.vault.root.join("DIR")).unwrap();
        let error = state
            .apply_with_reader(
                &crate::Changes {
                    directories: BTreeSet::from(["DIR".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
                &mut |_| panic!("directory aliases require inventory truth"),
            )
            .err()
            .unwrap();
        assert!(error.to_string().contains("Case-only directory"));
        std::fs::create_dir(state.vault.root.join("incoming")).unwrap();
        for n in 0..crate::watch::BULK_THRESHOLD {
            std::fs::write(
                state.vault.root.join(format!("incoming/note-{n}.md")),
                "incoming",
            )
            .unwrap();
        }
        let error = state
            .apply_with_reader(
                &crate::Changes {
                    directories: BTreeSet::from(["incoming".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
                &mut |_| panic!("large incoming topology uses parallel reconciliation"),
            )
            .err()
            .unwrap();
        assert!(error.to_string().contains("Bulk topology"));
        assert_eq!(state.snapshot.sources().unwrap(), before);
        assert_eq!(state.vault.entries, entries);
    }

    #[test]
    fn existing_save_reads_only_its_source_and_matches_full_graph_and_search() {
        let (temp, mut state) = fixture();
        let documents: Vec<_> = state
            .vault
            .notes
            .iter()
            .map(|note| crate::search::SearchDocument {
                path: note.path.clone(),
                title: note.title.clone(),
                text: state.snapshot.source(&note.path).unwrap(),
            })
            .collect();
        let base = temp.path().join("search");
        crate::Searcher::build_snapshot(&state.vault, &documents, &base, &mut |_, _| Ok(()))
            .unwrap()
            .finish_build()
            .unwrap();
        let immutable = crate::Searcher::open(&base).unwrap();
        let private = immutable.fork_session().unwrap();
        std::fs::write(
            state.vault.root.join("A.md"),
            "# Updated\n\nnewword [[dir/Target]]",
        )
        .unwrap();
        let mut reads = Vec::new();
        let batch = state
            .apply_with_reader(
                &crate::Changes {
                    changed: BTreeSet::from(["A.md".into()]),
                    ..Default::default()
                },
                &mut |phase, _| {
                    assert_ne!(phase, "Discovering notes", "no inventory enumeration");
                    Ok(())
                },
                &mut |path| {
                    reads.push(path.to_path_buf());
                    std::fs::read(path)
                },
            )
            .unwrap();
        assert_eq!(reads, [state.vault.root.join("A.md")]);
        assert_eq!(batch.read, 1);
        assert_eq!(batch.affected, BTreeSet::from(["A.md".into()]));
        let docs = [crate::search::SearchDocument {
            path: "A.md".into(),
            title: "A".into(),
            text: state.snapshot.source("A.md").unwrap(),
        }];
        private
            .update_snapshot_batch(&state.vault, &docs, &[])
            .unwrap();
        assert_eq!(private.search("newword", 10).unwrap().len(), 1);
        assert!(
            immutable.search("newword", 10).unwrap().is_empty(),
            "completed generation was never mutated"
        );
        assert_eq!(
            crate::Searcher::open(&base)
                .unwrap()
                .search("newword", 10)
                .unwrap()
                .len(),
            0
        );
        compare(&state);
    }

    #[test]
    fn create_delete_and_rename_update_cached_unresolved_ambiguous_and_markdown_referrers() {
        let (_temp, mut state) = fixture();
        std::fs::write(state.vault.root.join("Missing.md"), "# Materialized").unwrap();
        let batch = state
            .apply(
                &crate::Changes {
                    changed: BTreeSet::from(["Missing.md".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(batch.read, 1);
        assert!(batch.affected.contains("A.md"));
        assert_eq!(state.vault.backlinks("Missing.md").len(), 2);
        compare(&state);
        std::fs::remove_file(state.vault.root.join("Target.md")).unwrap();
        let batch = state
            .apply(
                &crate::Changes {
                    removed: BTreeSet::from(["Target.md".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(batch.read, 0);
        assert!(batch.affected.contains("A.md") && batch.affected.contains("B.md"));
        compare(&state);
        std::fs::rename(
            state.vault.root.join("dir/Target.md"),
            state.vault.root.join("dir/Moved.md"),
        )
        .unwrap();
        let batch = state
            .apply(
                &crate::Changes {
                    changed: BTreeSet::from(["dir/Moved.md".into()]),
                    removed: BTreeSet::from(["dir/Target.md".into()]),
                    rescan: false,
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(batch.read, 1);
        assert!(batch.affected.contains("A.md") && batch.affected.contains("B.md"));
        compare(&state);
    }

    #[test]
    fn persisted_delta_overlays_sources_graph_and_primary_and_rejects_a_foreign_base() {
        let (temp, mut state) = fixture();
        let cache = temp.path().join("cache");
        save_provisional(&state.snapshot, &state.vault, &cache, Some("A.md")).unwrap();
        // Navigation's small primary cache is older than the latest source delta.
        remember_primary(&cache, &state.vault.root, "A.md").unwrap();
        let original_bank = std::fs::read(cache.join("reader-snapshot.json")).unwrap();
        for source in ["# Changed\n[[Target]]", "# Changed again\n[[Missing]]"] {
            std::fs::write(state.vault.root.join("A.md"), source).unwrap();
            state
                .apply(
                    &crate::Changes {
                        changed: BTreeSet::from(["A.md".into()]),
                        ..Default::default()
                    },
                    &mut |_, _| Ok(()),
                )
                .unwrap();
            state.persist_delta(&cache).unwrap();
        }
        assert_eq!(
            original_bank,
            std::fs::read(cache.join("reader-snapshot.json")).unwrap(),
            "full source bank remains untouched"
        );
        let loaded = Snapshot::load_checked(&cache, &state.vault.root).unwrap();
        assert_eq!(loaded.sources().unwrap(), state.snapshot.sources().unwrap());
        assert!(loaded.search_generation.is_none());
        let startup = StartupSnapshot::load(&cache, &state.vault.root).unwrap();
        assert_eq!(
            startup.source("A.md").as_deref(),
            Some("# Changed again\n[[Missing]]")
        );
        assert_eq!(
            serde_json::to_value(&loaded.links).unwrap(),
            serde_json::to_value(&state.snapshot.links).unwrap()
        );
        let (vault, new_base, stats) =
            reconcile(&state.vault.root, Some(&loaded), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(stats.read, 0, "delta retains exact reusable revisions");
        save_provisional(&new_base, &vault, &cache, Some("A.md")).unwrap();
        // Stale journal belongs to the previous base and cannot overwrite a new full reconcile.
        assert!(Delta::load(&cache, &state.vault.root, Some(&new_base.id))
            .unwrap()
            .is_none());
    }

    #[test]
    fn source_denial_drops_only_that_search_source_and_preserves_vault() {
        let (_temp, mut state) = fixture();
        let batch = state
            .apply_with_reader(
                &crate::Changes {
                    changed: BTreeSet::from(["A.md".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
                &mut |_| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "denied positive control",
                    ))
                },
            )
            .unwrap();
        assert_eq!(batch.read, 1);
        assert!(state.snapshot.source("A.md").is_none());
        assert_eq!(state.vault.unreadable.len(), 1);
        assert!(!state.vault.inventory_complete);
        assert!(state.snapshot.source("B.md").is_some());
        assert!(state
            .vault
            .backlinks("dir/Target.md")
            .iter()
            .all(|link| link.path != "A.md"));
    }

    #[test]
    fn overflow_requires_reconcile_but_valid_new_ancestors_are_incremental() {
        let (_temp, mut state) = fixture();
        let before = state.snapshot.sources().unwrap();
        assert!(state
            .apply(
                &crate::Changes {
                    rescan: true,
                    ..Default::default()
                },
                &mut |_, _| Ok(())
            )
            .is_err());
        std::fs::create_dir(state.vault.root.join("new")).unwrap();
        std::fs::write(state.vault.root.join("new/note.md"), "new").unwrap();
        assert_eq!(before, state.snapshot.sources().unwrap());
        let batch = state
            .apply(
                &crate::Changes {
                    changed: BTreeSet::from(["new/note.md".into()]),
                    ..Default::default()
                },
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert!(batch.topology_changed);
        assert_eq!(batch.read, 1);
        compare(&state);
    }

    #[test]
    fn explicit_event_rereads_same_size_preserved_mtime_and_persists_sparse_graph() {
        let (temp, mut state) = fixture();
        let cache = temp.path().join("cache");
        save_provisional(&state.snapshot, &state.vault, &cache, Some("A.md")).unwrap();
        for path in ["A.md", "B.md"] {
            let file = state.vault.root.join(path);
            let old = std::fs::metadata(&file).unwrap().modified().unwrap();
            let source = state.snapshot.source(path).unwrap();
            let replacement = source.replace("Target", "Absent");
            assert_eq!(source.len(), replacement.len());
            std::fs::write(&file, &replacement).unwrap();
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
            let batch = state
                .apply(
                    &crate::Changes {
                        changed: BTreeSet::from([path.into()]),
                        ..Default::default()
                    },
                    &mut |_, _| Ok(()),
                )
                .unwrap();
            assert_eq!(batch.read, 1);
            state.persist_delta(&cache).unwrap();
            let loaded = Snapshot::load_checked(&cache, &state.vault.root).unwrap();
            assert_eq!(loaded.sources().unwrap(), state.snapshot.sources().unwrap());
            assert_eq!(
                serde_json::to_value(loaded.links).unwrap(),
                serde_json::to_value(&state.snapshot.links).unwrap()
            );
        }
        compare(&state);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual paired 5001-note save/index filesystem-latency profile"]
    fn incremental_save_latency_profile() {
        use crate::{
            file_editor::{FileEditor, Save},
            search::SearchDocument,
            Searcher,
        };
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
            let phase = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_phase".as_ptr());
            let count = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_count".as_ptr());
            assert!(
                !phase.is_null() && !count.is_null(),
                "use slow-vault-fs preload"
            );
            (
                std::mem::transmute::<
                    *mut std::ffi::c_void,
                    unsafe extern "C" fn(*const std::ffi::c_char),
                >(phase),
                std::mem::transmute::<
                    *mut std::ffi::c_void,
                    unsafe extern "C" fn(i32) -> std::ffi::c_ulong,
                >(count),
            )
        };
        let set = |name: &'static std::ffi::CStr| unsafe { phase(name.as_ptr()) };
        let counters = || (0..4).map(|op| unsafe { count(op) }).collect::<Vec<_>>();
        let ms = |start: std::time::Instant| start.elapsed().as_secs_f64() * 1000.;
        let temp = tempfile::Builder::new()
            .prefix("okilum-incremental-profile-")
            .tempdir()
            .unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        for n in 0..5000 {
            std::fs::write(
                root.join(format!("notes/note-{n}.md")),
                format!(
                    "# Note {n}\n\n[[target]]\n\n[Target](../target.md)\n\n{}",
                    "Source paragraph.\n\n".repeat(60)
                ),
            )
            .unwrap();
        }
        set(c"positive_control");
        let before = counters();
        SourceRevision::read(&root.join("target.md")).unwrap();
        std::fs::read(root.join("target.md")).unwrap();
        let positive = counters();
        assert!(positive[0] > before[0] && positive[1] > before[1] && positive[2] > before[2]);
        set(c"setup");
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        save_provisional(&snapshot, &vault, &cache, Some("notes/note-0.md")).unwrap();
        let documents = |vault: &Vault, snapshot: &Snapshot| {
            vault
                .notes
                .iter()
                .map(|note| SearchDocument {
                    path: note.path.clone(),
                    title: note.title.clone(),
                    text: snapshot.source(&note.path).unwrap(),
                })
                .collect::<Vec<_>>()
        };
        Searcher::build_snapshot(
            &vault,
            &documents(&vault, &snapshot),
            &temp.path().join("base-search"),
            &mut |_, _| Ok(()),
        )
        .unwrap()
        .finish_build()
        .unwrap();
        let immutable = Searcher::open(&temp.path().join("base-search")).unwrap();
        let mut state = State::new(vault, snapshot);
        let mut session = None;
        let mut published_candidates = state.candidates.clone();
        let mut editor =
            FileEditor::open(&root.join("notes/note-0.md"), &temp.path().join("editor")).unwrap();
        let samples: usize = std::env::var("OKILUM_CLOUD_PROFILE_SAMPLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        for sample in 0..samples {
            assert_eq!(published_candidates.root(), state.vault.root);
            editor
                .set_text(format!("# Edited {sample}\n\nuniquesaveword [[target]]"))
                .unwrap();
            set(c"serial_sources");
            let before = counters();
            let start = std::time::Instant::now();
            assert_eq!(editor.save().unwrap(), Save::Saved);
            let save_ms = ms(start);
            let baseline_start = std::time::Instant::now();
            let (full, full_snapshot, stats) =
                reconcile_parallel(&root, Some(&state.snapshot), false, &mut |_, _| Ok(()))
                    .unwrap();
            let reconcile_ms = ms(baseline_start);
            let full_search = Searcher::build_snapshot(
                &full,
                &documents(&full, &full_snapshot),
                &temp.path().join(format!("baseline-{sample}")),
                &mut |_, _| Ok(()),
            )
            .unwrap();
            full_search.finish_build().unwrap();
            let full_ms = ms(start);
            let baseline_calls = counters()
                .iter()
                .zip(&before)
                .map(|(a, b)| a - b)
                .collect::<Vec<_>>();
            set(c"setup");
            assert_eq!(
                Searcher::open(&temp.path().join(format!("baseline-{sample}")))
                    .unwrap()
                    .search("uniquesaveword", 10)
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!((stats.read, stats.reused), (1, 5000));
            set(c"parallel_sources");
            let before = counters();
            let start = std::time::Instant::now();
            let batch = state
                .apply(
                    &crate::Changes {
                        changed: BTreeSet::from(["notes/note-0.md".into()]),
                        ..Default::default()
                    },
                    &mut |_, _| Ok(()),
                )
                .unwrap();
            let graph_ms = ms(start);
            let fork_start = std::time::Instant::now();
            if session.is_none() {
                session = Some(immutable.fork_session().unwrap());
            }
            let fork_ms = ms(fork_start);
            let search = session.as_ref().unwrap();
            let commit_start = std::time::Instant::now();
            search
                .update_snapshot_batch(
                    &state.vault,
                    &[SearchDocument {
                        path: "notes/note-0.md".into(),
                        title: "Edited".into(),
                        text: state.snapshot.source("notes/note-0.md").unwrap(),
                    }],
                    &[],
                )
                .unwrap();
            let commit_ms = ms(commit_start);
            let _published_vault = state.vault.clone();
            let _published_inventory = state.vault.notes.clone();
            published_candidates = state.candidates.clone();
            assert_eq!(published_candidates.root(), state.vault.root);
            let publication_ms = ms(start) + save_ms;
            let persist_start = std::time::Instant::now();
            let generation = format!("{:064x}", sample + 1);
            let repairs = cache
                .join("generations")
                .join(format!("{generation}.repairs"));
            std::fs::create_dir_all(&repairs).unwrap();
            let owned = tempfile::Builder::new()
                .prefix("session-")
                .tempdir_in(&repairs)
                .unwrap();
            search.copy_committed_to(owned.path()).unwrap();
            std::fs::write(owned.path().join("complete"), b"1").unwrap();
            let _ = owned.keep();
            state.set_search_generation(Some(generation));
            state.persist_delta(&cache).unwrap();
            let persist_ms = ms(persist_start);
            let total_ms = ms(start) + save_ms;
            let calls = counters()
                .iter()
                .zip(&before)
                .map(|(a, b)| a - b)
                .collect::<Vec<_>>();
            set(c"setup");
            assert_eq!(batch.read, 1);
            assert_eq!(batch.affected.len(), 1);
            assert_eq!(search.search("uniquesaveword", 10).unwrap().len(), 1);
            assert_eq!(state.vault.backlinks("target.md").len(), 9999);
            assert_eq!(
                serde_json::to_value(&state.vault.backlink_map).unwrap(),
                serde_json::to_value(&full.backlink_map).unwrap()
            );
            assert_eq!(
                calls[0], 1,
                "one canonical source open; positive control proved detector"
            );
            eprintln!("INCREMENTAL_SAVE_PROFILE sample={sample} notes=5001 baseline_ms={full_ms:.2} baseline_reconcile_ms={reconcile_ms:.2} baseline_read={} baseline_reused={} baseline_calls={baseline_calls:?} save_ms={save_ms:.2} source_graph_ms={graph_ms:.2} fork_ms={fork_ms:.2} commit_ms={commit_ms:.2} save_to_committed_ms={publication_ms:.2} persist_ms={persist_ms:.2} total_ms={total_ms:.2} read={} affected={} calls={calls:?}; same host/session; watcher debounce/UI scheduling/native save excluded",stats.read,stats.reused,batch.read,batch.affected.len());
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual paired 5001-note topology filesystem-latency profile"]
    fn incremental_topology_latency_profile() {
        use crate::{search::SearchDocument, Searcher};
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
            let phase = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_phase".as_ptr());
            let count = dlsym(std::ptr::null_mut(), c"okilum_slow_fs_count".as_ptr());
            assert!(
                !phase.is_null() && !count.is_null(),
                "use slow-vault-fs preload"
            );
            (
                std::mem::transmute::<
                    *mut std::ffi::c_void,
                    unsafe extern "C" fn(*const std::ffi::c_char),
                >(phase),
                std::mem::transmute::<
                    *mut std::ffi::c_void,
                    unsafe extern "C" fn(i32) -> std::ffi::c_ulong,
                >(count),
            )
        };
        let set = |name: &'static std::ffi::CStr| unsafe { phase(name.as_ptr()) };
        let counters = || (0..4).map(|op| unsafe { count(op) }).collect::<Vec<_>>();
        let ms = |start: std::time::Instant| start.elapsed().as_secs_f64() * 1000.;

        let temp = tempfile::Builder::new()
            .prefix("okilum-topology-profile-")
            .tempdir()
            .unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        for n in 0..5000 {
            let references = if n < 5 {
                "[[New]] [New](../New.md) ![[New]] [[Moved]]"
            } else {
                ""
            };
            std::fs::write(
                root.join(format!("notes/note-{n}.md")),
                format!(
                    "# Note {n}\n\n[[target]] [Target](../target.md)\n\n{references}\n\n{}",
                    "Source paragraph.\n\n".repeat(60)
                ),
            )
            .unwrap();
        }
        set(c"positive_control");
        let before = counters();
        SourceRevision::read(&root.join("target.md")).unwrap();
        std::fs::read(root.join("target.md")).unwrap();
        let positive = counters();
        assert!(positive[0] > before[0] && positive[1] > before[1] && positive[2] > before[2]);
        set(c"setup");
        let (vault, snapshot, _) = reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        let docs = |vault: &Vault, snapshot: &Snapshot| {
            vault
                .notes
                .iter()
                .map(|note| SearchDocument {
                    path: note.path.clone(),
                    title: note.title.clone(),
                    text: snapshot.source(&note.path).unwrap(),
                })
                .collect::<Vec<_>>()
        };
        let base = temp.path().join("base-search");
        Searcher::build_snapshot(&vault, &docs(&vault, &snapshot), &base, &mut |_, _| Ok(()))
            .unwrap()
            .finish_build()
            .unwrap();
        let immutable = Searcher::open(&base).unwrap();
        let mut state = State::new(vault, snapshot);
        let mut session = None;
        let samples: usize = std::env::var("OKILUM_CLOUD_PROFILE_SAMPLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        for sample in 0..samples {
            for operation in [
                "create",
                "delete",
                "undo",
                "rename",
                "rename_undo",
                "directory_echo",
                "large_directory_echo",
            ] {
                let mut changes = crate::Changes::default();
                let expected = match operation {
                    "create" | "undo" => {
                        std::fs::write(root.join("New.md"), "# New\nuniquetopologyword").unwrap();
                        changes.changed.insert("New.md".into());
                        Some("New.md")
                    }
                    "delete" => {
                        std::fs::remove_file(root.join("New.md")).unwrap();
                        changes.removed.insert("New.md".into());
                        None
                    }
                    "rename" => {
                        std::fs::rename(root.join("New.md"), root.join("Moved.md")).unwrap();
                        changes.changed.insert("Moved.md".into());
                        changes.removed.insert("New.md".into());
                        Some("Moved.md")
                    }
                    "rename_undo" => {
                        std::fs::rename(root.join("Moved.md"), root.join("New.md")).unwrap();
                        changes.changed.insert("New.md".into());
                        changes.removed.insert("Moved.md".into());
                        Some("New.md")
                    }
                    _ => Some("New.md"),
                };
                // Real parent-directory notifications must not force full prepare.
                changes.directories.insert(String::new());
                if operation == "large_directory_echo" {
                    changes.directories.insert("notes".into());
                }
                set(c"serial_sources");
                let before = counters();
                let start = std::time::Instant::now();
                let (full, snapshot, stats) =
                    reconcile_parallel(&root, Some(&state.snapshot), false, &mut |_, _| Ok(()))
                        .unwrap();
                let reconcile_ms = ms(start);
                let full_base = temp.path().join(format!("baseline-{sample}-{operation}"));
                Searcher::build_snapshot(
                    &full,
                    &docs(&full, &snapshot),
                    &full_base,
                    &mut |_, _| Ok(()),
                )
                .unwrap()
                .finish_build()
                .unwrap();
                let baseline_ms = ms(start);
                let baseline_calls = counters()
                    .iter()
                    .zip(&before)
                    .map(|(a, b)| a - b)
                    .collect::<Vec<_>>();
                set(c"parallel_sources");
                let before = counters();
                let start = std::time::Instant::now();
                let batch = state
                    .apply(&changes, &mut |phase, _| {
                        assert_ne!(phase, "Discovering notes");
                        Ok(())
                    })
                    .unwrap();
                let graph_ms = ms(start);
                if !batch.affected.is_empty() {
                    if session.is_none() {
                        session = Some(immutable.fork_session().unwrap());
                    }
                    let documents = batch
                        .affected
                        .iter()
                        .filter_map(|path| {
                            state.snapshot.source(path).map(|text| SearchDocument {
                                path: path.clone(),
                                title: state.vault.note_title(path),
                                text,
                            })
                        })
                        .collect::<Vec<_>>();
                    session
                        .as_ref()
                        .unwrap()
                        .update_snapshot_batch(
                            &state.vault,
                            &documents,
                            &batch.removed.iter().cloned().collect::<Vec<_>>(),
                        )
                        .unwrap();
                }
                let _published_vault = state.vault.clone();
                let _published_inventory = state.vault.notes.clone();
                let publication_ms = ms(start);
                let calls = counters()
                    .iter()
                    .zip(&before)
                    .map(|(a, b)| a - b)
                    .collect::<Vec<_>>();
                set(c"setup");
                let found = session
                    .as_ref()
                    .unwrap()
                    .search("uniquetopologyword", 10)
                    .unwrap();
                assert_eq!(found.len(), usize::from(expected.is_some()));
                if let Some(expected) = expected {
                    assert_eq!(found[0].path, expected);
                }
                assert_eq!(state.vault.entries, full.entries);
                assert_eq!(
                    serde_json::to_value(&state.vault.backlink_map).unwrap(),
                    serde_json::to_value(&full.backlink_map).unwrap()
                );
                assert_eq!(
                    batch.read,
                    usize::from(!matches!(
                        operation,
                        "delete" | "directory_echo" | "large_directory_echo"
                    ))
                );
                assert_eq!(calls[0], batch.read as u64);
                if operation == "large_directory_echo" {
                    assert!(
                        calls[2] >= 5000 && calls[2] < 5020,
                        "hinted children checked precisely once: {calls:?}"
                    );
                    assert!(
                        batch.affected.is_empty(),
                        "precise unchanged siblings never enter search/graph"
                    );
                } else {
                    assert!(
                        calls[2] < 20,
                        "root hint must not walk unchanged subtree: {calls:?}"
                    );
                }
                eprintln!("INCREMENTAL_TOPOLOGY_PROFILE sample={sample} operation={operation} notes=5001 baseline_ms={baseline_ms:.2} baseline_reconcile_ms={reconcile_ms:.2} baseline_read={} baseline_reused={} baseline_calls={baseline_calls:?} source_graph_ms={graph_ms:.2} to_committed_ms={publication_ms:.2} read={} affected={} calls={calls:?}; same host/session; 2ms canonical open/read,100us metadata; native UI scheduling, mutation syscall and watcher debounce excluded",stats.read,stats.reused,batch.read,batch.affected.len());
            }
            std::fs::remove_file(root.join("New.md")).unwrap();
            state
                .apply(
                    &crate::Changes {
                        removed: BTreeSet::from(["New.md".into()]),
                        ..Default::default()
                    },
                    &mut |_, _| Ok(()),
                )
                .unwrap();
            session
                .as_ref()
                .unwrap()
                .update_snapshot_batch(&state.vault, &[], &["New.md".into()])
                .unwrap();
        }
    }
}
