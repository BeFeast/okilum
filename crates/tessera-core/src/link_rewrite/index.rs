//! Rewrite candidates derived from the existing Reader source snapshot.
//! Keys are deliberately conservative: all equal basenames participate, even
//! unresolved/ambiguous links, so inventory changes cannot hide a candidate.
use super::*;
use crate::vault::warm::{Snapshot, SourceRevision};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

#[derive(Clone, Default)]
pub struct CandidateIndex {
    root: PathBuf,
    revision: String,
    sources: BTreeMap<String, Option<SourceRevision>>,
    keys: HashMap<String, BTreeSet<String>>,
    source_keys: HashMap<String, BTreeSet<String>>,
    dirty: BTreeSet<String>,
    // Existing-file Markdown fallback is inventory-dependent. Preserve lexical
    // candidates even when their raw-space destination was unresolved at indexing.
    // Code/labels may over-match: extra reads are safe; omitted referrers are not.
    fallback_lines: BTreeMap<String, String>,
}
impl CandidateIndex {
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let vault = snapshot.vault();
        let mut index = Self {
            root: vault.root,
            revision: snapshot.id.clone(),
            ..Default::default()
        };
        for path in snapshot.source_paths() {
            if let Some(source) = snapshot.source(path) {
                index.update(path, &source, snapshot.source_revision(path).cloned());
            }
        }
        index
    }

    /// Conservative metadata-free referrers, including unresolved destinations.
    pub fn referrers_for(&self, target: &str) -> BTreeSet<String> {
        let mut selected = BTreeSet::new();
        if let Some(key) = path_key(target) {
            if let Some(paths) = self.keys.get(&key) {
                selected.extend(paths.iter().cloned());
            }
            for (path, line) in &self.fallback_lines {
                if line.contains(&key) {
                    selected.insert(path.clone());
                }
            }
        }
        selected
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn update(&mut self, path: &str, source: &str, revision: Option<SourceRevision>) {
        self.remove(path);
        let keys: BTreeSet<_> = syntax::targets(source)
            .iter()
            .filter_map(|target| key(&target.text, target.wiki))
            .collect();
        for key in &keys {
            self.keys
                .entry(key.clone())
                .or_default()
                .insert(path.into());
        }
        let fallback = source
            .lines()
            .filter(|line| line.contains("](") && line.contains(' '))
            .map(crate::document_links::decode)
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        if !fallback.is_empty() {
            self.fallback_lines.insert(path.into(), fallback);
        }
        self.source_keys.insert(path.into(), keys);
        self.sources.insert(path.into(), revision);
        self.dirty.remove(path);
    }

    pub fn remove(&mut self, path: &str) {
        if let Some(keys) = self.source_keys.remove(path) {
            for key in keys {
                if let Some(paths) = self.keys.get_mut(&key) {
                    paths.remove(path);
                    if paths.is_empty() {
                        self.keys.remove(&key);
                    }
                }
            }
        }
        self.fallback_lines.remove(path);
        self.sources.remove(path);
        self.dirty.remove(path);
    }

    pub fn invalidate_paths(&mut self, paths: &[String]) {
        for path in self.sources.keys() {
            if paths
                .iter()
                .any(|p| p.is_empty() || p == path || path.starts_with(&format!("{p}/")))
            {
                self.dirty.insert(path.clone());
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn select(
        &self,
        root: &Path,
        vault: &Vault,
        from: &str,
        to: &str,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<(BTreeSet<String>, BTreeMap<String, SourceRevision>)> {
        ensure!(
            root.canonicalize()? == self.root,
            "Link index belongs to another vault"
        );
        let mut selected = BTreeSet::new();
        let mut targets = BTreeSet::from([from.to_owned(), to.to_owned()]);
        for entry in &vault.entries {
            let next = crate::link_rewrite::moved_path(&entry.path, from, to);
            if next != entry.path {
                selected.insert(entry.path.clone());
                targets.insert(entry.path.clone());
                targets.insert(next);
            }
        }
        for target in &targets {
            if let Some(key) = path_key(target) {
                for (path, line) in &self.fallback_lines {
                    if line.contains(&key) {
                        selected.insert(path.clone());
                    }
                }
                if let Some(paths) = self.keys.get(&key) {
                    selected.extend(paths.iter().cloned());
                }
            }
        }
        let mut unchanged = BTreeMap::new();
        for (count, note) in vault.notes.iter().enumerate() {
            checkpoint("Checking link index revisions", count)?;
            let stamp = SourceRevision::read(&root.join(&note.path)).ok();
            let reusable = stamp.as_ref().is_some_and(|stamp| stamp.is_precise())
                && self.sources.get(&note.path) == Some(&stamp)
                && !self.dirty.contains(&note.path);
            if !reusable || selected.contains(&note.path) {
                selected.insert(note.path.clone());
            } else if let Some(stamp) = stamp {
                unchanged.insert(note.path.clone(), stamp);
            }
        }
        Ok((selected, unchanged))
    }
}

fn key(target: &str, wiki: bool) -> Option<String> {
    let base = if wiki {
        target.split(['#', '^']).next()?
    } else {
        target.split('#').next()?
    }
    .trim();
    let decoded = if wiki {
        base.to_owned()
    } else {
        crate::document_links::decode(base)
    };
    path_key(&decoded)
}

fn path_key(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?.to_lowercase();
    let name = name.strip_suffix(".md").unwrap_or(&name);
    (!name.is_empty()).then(|| name.to_owned())
}
