//! Stable inventory ownership with immutable semantic snapshots. Mutable index
//! baselines clone a frozen view; publishing semantics never mutates those views.
use super::*;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

pub(super) fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A cheap immutable alias view, borrowing an unpublished baseline or pinning
/// the current semantic snapshot. Neither path copies the alias inventory.
pub struct Aliases<'a>(AliasView<'a>);
enum AliasView<'a> {
    Borrowed(&'a std::collections::BTreeMap<String, Vec<String>>),
    Published(Arc<Graph>),
}
impl std::ops::Deref for Aliases<'_> {
    type Target = std::collections::BTreeMap<String, Vec<String>>;
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            AliasView::Borrowed(aliases) => aliases,
            AliasView::Published(graph) => &graph.aliases,
        }
    }
}

pub(super) struct Publication {
    current: std::sync::RwLock<Arc<Graph>>,
    // Keep retired graph/alias allocations off the UI thread even if a short
    // foreground reader overlaps publication. The next worker collects them.
    retired: std::sync::Mutex<Vec<Arc<Graph>>>,
}

pub(super) struct Graph {
    pub aliases: Arc<std::collections::BTreeMap<String, Vec<String>>>,
    pub alias_map: HashMap<String, Vec<String>>,
    pub backlinks: HashMap<String, Vec<Backlink>>,
    pub inventory_revision: String,
    revision: u64,
    resolution: u64,
}

impl Clone for Vault {
    /// A clone is a frozen graph snapshot, not another mutable publication handle.
    fn clone(&self) -> Self {
        let graph = self.published_graph();
        Self {
            single_file: self.single_file,
            inventory_complete: self.inventory_complete,
            inventory_scanned: self.inventory_scanned,
            unreadable: self.unreadable.clone(),
            cloud_placeholders: self.cloud_placeholders.clone(),
            root: self.root.clone(),
            notes: self.notes.clone(),
            entries: self.entries.clone(),
            suffix_map: self.suffix_map.clone(),
            path_map: self.path_map.clone(),
            asset_map: self.asset_map.clone(),
            aliases: graph
                .as_ref()
                .map(|g| g.aliases.as_ref().clone())
                .unwrap_or_else(|| self.aliases.clone()),
            alias_map: graph
                .as_ref()
                .map(|g| g.alias_map.clone())
                .unwrap_or_else(|| self.alias_map.clone()),
            backlink_map: graph
                .as_ref()
                .map(|g| g.backlinks.clone())
                .unwrap_or_else(|| self.backlink_map.clone()),
            occupied_paths: self.occupied_paths.clone(),
            non_directory_paths: self.non_directory_paths.clone(),
            symlink_paths: self.symlink_paths.clone(),
            graph_root: self.graph_root.clone(),
            graph_os_paths: self.graph_os_paths.clone(),
            publication: Default::default(),
            publication_id: next_id(),
        }
    }
}

impl Vault {
    pub(super) fn alias_view(&self) -> Aliases<'_> {
        Aliases(match self.published_graph() {
            Some(graph) => AliasView::Published(graph),
            None => AliasView::Borrowed(&self.aliases),
        })
    }

    pub(super) fn freeze_publication(&mut self) {
        if let Some(slot) = self.publication.take() {
            let graph = slot.current.into_inner().unwrap();
            self.aliases = graph.aliases.as_ref().clone();
            self.alias_map = graph.alias_map.clone();
            self.backlink_map = graph.backlinks.clone();
            self.publication_id = next_id();
        }
    }

    pub(super) fn published_graph(&self) -> Option<Arc<Graph>> {
        self.publication
            .get()
            .map(|slot| slot.current.read().unwrap().clone())
    }

    /// Worker-only publication. True means the immutable inventory stayed valid;
    /// only the small semantic snapshot was replaced. No UI cache walk is needed.
    pub fn publish_semantics(&self, next: &Self) -> bool {
        if !self.same_inventory(next) {
            return false;
        }
        let old = self.published_graph();
        let (aliases, alias_map, backlinks) = old
            .as_ref()
            .map(|g| (g.aliases.as_ref(), &g.alias_map, &g.backlinks))
            .unwrap_or((&self.aliases, &self.alias_map, &self.backlink_map));
        let unchanged = aliases == &next.aliases
            && alias_map == &next.alias_map
            && backlinks == &next.backlink_map;
        if unchanged && old.is_some() {
            return true;
        }
        // Even the first body-only save prepares shared alias storage here.
        // Foreground Quick Open must never clone an initial full alias map.

        let graph = Arc::new(Graph {
            aliases: Arc::new(next.aliases.clone()),
            alias_map: next.alias_map.clone(),
            backlinks: next.backlink_map.clone(),
            inventory_revision: next.link_inventory_revision(),
            revision: if unchanged {
                0
            } else {
                old.as_ref().map_or(1, |g| g.revision.wrapping_add(1))
            },
            resolution: old.as_ref().map_or(0, |g| g.resolution)
                + u64::from(alias_map != &next.alias_map),
        });
        if old.is_none() {
            self.publication.get_or_init(|| Publication {
                current: std::sync::RwLock::new(graph),
                retired: Default::default(),
            });
        } else {
            let slot = self.publication.get().unwrap();
            let retired = std::mem::replace(&mut *slot.current.write().unwrap(), graph);
            drop(old);
            let mut pending = slot.retired.lock().unwrap();
            pending.push(retired);
            pending.retain(|graph| {
                Arc::strong_count(graph) > 1 || Arc::strong_count(&graph.aliases) > 1
            });
        }
        true
    }

    pub fn publication_identity(&self) -> (u64, u64) {
        (self.publication_id, self.publication_revision())
    }

    pub fn publication_revision(&self) -> u64 {
        self.published_graph().map_or(0, |g| g.revision)
    }

    /// Per-block lazy dependency key: backlink contexts/titles do not change
    /// Markdown resolution. Inventory/alias changes do.
    pub fn resolution_identity(&self) -> (u64, u64) {
        (
            self.publication_id,
            self.published_graph().map_or(0, |g| g.resolution),
        )
    }

    pub(super) fn resolve_published(
        &self,
        target: &str,
        from: &str,
        aliases: &HashMap<String, Vec<String>>,
    ) -> Resolution {
        let target = target
            .split(['#', '^'])
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let key = target.trim_end_matches(".md").trim_matches('/');
        if key.is_empty() {
            return Resolution::Unresolved;
        }
        if key.starts_with("./") || key.starts_with("../") {
            return self
                .walk_relative(key, from)
                .and_then(|abs| self.path_map.get(&abs))
                .map_or(Resolution::Unresolved, |path| Resolution::Resolved {
                    path: path.clone(),
                });
        }
        if let Some(path) = self.path_map.get(key) {
            return Resolution::Resolved { path: path.clone() };
        }
        match aliases.get(key).or_else(|| self.suffix_map.get(key)) {
            None => Resolution::Unresolved,
            Some(paths) if paths.len() == 1 => Resolution::Resolved {
                path: paths[0].clone(),
            },
            Some(paths) => Resolution::Ambiguous {
                candidates: paths.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_publication_keeps_inventory_identity_and_frozen_baselines() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::write(
            root.join("note.md"),
            "---\naliases: [Before]\n---\n[[Old]] old context\n",
        )
        .unwrap();
        std::fs::write(root.join("Old.md"), "old target").unwrap();
        std::fs::write(root.join("New.md"), "new target").unwrap();
        let published = Arc::new(Vault::scan(root).unwrap());
        let original = published.clone();
        let frozen = (*published).clone();
        let identity = published.resolution_identity();
        std::fs::write(
            root.join("note.md"),
            "---\naliases: [After]\n---\n[[New]] changed context\n",
        )
        .unwrap();
        let next = Vault::scan(root).unwrap();
        assert!(published.publish_semantics(&next));
        assert!(Arc::ptr_eq(&original, &published));
        assert_ne!(identity, published.resolution_identity());
        assert_eq!(published.resolve("After").path(), Some("note.md"));
        assert!(published.resolve("Before").path().is_none());
        assert_eq!(published.aliases()["note.md"], ["After"]);
        assert!(published.backlinks("Old.md").is_empty());
        assert_eq!(published.backlinks("New.md").len(), 1);
        assert_eq!(frozen.resolve("Before").path(), Some("note.md"));
        assert_eq!(frozen.backlinks("Old.md").len(), 1);
        assert_ne!(
            frozen.link_inventory_revision(),
            published.link_inventory_revision()
        );
        let resolved = published.resolution_identity();
        let revision = published.publication_revision();
        std::fs::write(
            root.join("note.md"),
            "---\naliases: [After]\n---\n[[New]] prose edit beside unchanged link\n",
        )
        .unwrap();
        assert!(published.publish_semantics(&Vault::scan(root).unwrap()));
        assert_eq!(
            resolved,
            published.resolution_identity(),
            "backlink context does not invalidate Markdown resolution"
        );
        assert!(published.publication_revision() > revision);
        assert!(published.backlinks("New.md")[0]
            .context
            .contains("prose edit"));
        let held_aliases = published.aliases();
        std::fs::write(
            root.join("note.md"),
            "---\naliases: [Later]\n---\n[[New]] newer context\n",
        )
        .unwrap();
        assert!(published.publish_semantics(&Vault::scan(root).unwrap()));
        assert_eq!(held_aliases["note.md"], ["After"]);
        assert!(
            !published
                .publication
                .get()
                .unwrap()
                .retired
                .lock()
                .unwrap()
                .is_empty(),
            "positive control: an overlapping alias reader pins retirement"
        );
        drop(held_aliases);
        std::fs::write(
            root.join("note.md"),
            "---\naliases: [Later]\n---\n[[New]] final context\n",
        )
        .unwrap();
        assert!(published.publish_semantics(&Vault::scan(root).unwrap()));
        assert!(
            published
                .publication
                .get()
                .unwrap()
                .retired
                .lock()
                .unwrap()
                .is_empty(),
            "a worker collects retired graph allocations"
        );
        std::fs::write(root.join("Another.md"), "new identity").unwrap();
        assert!(
            !published.publish_semantics(&Vault::scan(root).unwrap()),
            "inventory changes need a new inventory publication"
        );
    }
}
