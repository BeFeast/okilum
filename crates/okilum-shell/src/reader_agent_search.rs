//! Transient desktop search scope, shared by explicit Agent files and ordinary search.
//!
//! Caller API contract (slot50): keep `AgentSearch` across provider refreshes;
//! call `open` only for explicit palette open and `set_filter` for chip changes.
//! Keep one `ProviderInventory::new(root)` per vault lifetime. Publish complete
//! provider generations with `refresh(revision, paths)` and requeue on true.
//! Capture `snapshot(scope)` for each worker: names mode filters with `admits`
//! before ranking, contents mode calls snapshot `search`. Before applying either
//! result, require `accepts(snapshot, scope)` AND the caller's current query/open
//! generation guard (including close/reopen and filter round trips). Preserve the
//! selected path before replacing rows, then use snapshot `selection` afterwards.
//! Missing selection returns None; never translate it into held Enter on row 0.
//! Existing `AgentSearch::{open,set_filter,filter,label,search}` signatures remain
//! stable; provider integration adds the two snapshot types without replacing them.
#![allow(dead_code, reason = "provider inventory not wired yet")]
use okilum_core::{agent_search, quick_open::FileFilter, SearchHit, Searcher};

/// Store in the search palette, independently of Tree's hidden-file setting and
/// the optional Agents executor module. Explicit palette open must call `open`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentSearch {
    filter: FileFilter,
}

impl AgentSearch {
    pub(crate) fn open(&mut self) {
        self.filter = FileFilter::Default;
    }

    /// Returns whether callers must invalidate/reissue the current query.
    pub(crate) fn set_filter(&mut self, filter: FileFilter) -> bool {
        let changed = self.filter != filter;
        self.filter = filter;
        changed
    }

    pub(crate) fn filter(self) -> FileFilter {
        self.filter
    }

    pub(crate) fn label(self) -> &'static str {
        match self.filter {
            FileFilter::Default => "Files",
            FileFilter::AgentFiles => "Agent files",
        }
    }

    /// Run off the UI thread with current canonical inventory paths. Keep the
    /// returned hit path for navigation; duplicate titles are not identities.
    pub(crate) fn search<'a>(
        self,
        searcher: &Searcher,
        inventory: impl IntoIterator<Item = &'a str>,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchHit>> {
        agent_search::search(searcher, inventory, self.filter, query, limit)
    }
}

/// One vault's provider inventory. Construct a new instance when changing vaults.
/// Revisions must come from the provider's monotonic inventory generation, not
/// from completion order. Titles never participate in source identity.
#[derive(Debug)]
pub(crate) struct ProviderInventory {
    root: std::path::PathBuf,
    revision: Option<u64>,
    paths: std::sync::Arc<Vec<String>>,
}

/// Immutable worker input and publication guard. The caller additionally guards
/// its query generation/open state; this guard covers provider refresh and scope.
#[derive(Clone, Debug)]
pub(crate) struct InventorySnapshot {
    root: std::path::PathBuf,
    paths: std::sync::Arc<Vec<String>>,
    scope: AgentSearch,
}

impl ProviderInventory {
    pub(crate) fn new(root: std::path::PathBuf) -> Self {
        Self {
            root,
            revision: None,
            paths: Default::default(),
        }
    }

    /// Publish a complete inventory; ignore late or duplicate provider results.
    /// A newer revision invalidates workers even when only source contents changed.
    /// Return true to queue a replacement query without resetting the filter.
    pub(crate) fn refresh<'a>(
        &mut self,
        revision: u64,
        paths: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        if self.revision.is_some_and(|current| revision <= current) {
            return false;
        }
        let mut paths: Vec<_> = paths.into_iter().map(str::to_owned).collect();
        paths.sort();
        paths.dedup();
        self.paths = std::sync::Arc::new(paths);
        self.revision = Some(revision);
        true
    }

    pub(crate) fn snapshot(&self, scope: AgentSearch) -> InventorySnapshot {
        InventorySnapshot {
            root: self.root.clone(),
            paths: self.paths.clone(),
            scope,
        }
    }

    /// Check on the UI thread before publishing rows, snippets or held Enter.
    pub(crate) fn accepts(&self, snapshot: &InventorySnapshot, scope: AgentSearch) -> bool {
        self.revision.is_some()
            && self.root == snapshot.root
            && std::sync::Arc::ptr_eq(&self.paths, &snapshot.paths)
            && scope == snapshot.scope
    }
}

impl InventorySnapshot {
    /// Exact paths for the name provider; apply before ranking or top-N.
    pub(crate) fn admits(&self, path: &str) -> bool {
        self.scope.filter.admits(path)
            && self
                .paths
                .binary_search_by(|candidate| candidate.as_str().cmp(path))
                .is_ok()
    }

    pub(crate) fn search(
        &self,
        searcher: &Searcher,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchHit>> {
        self.scope.search(
            searcher,
            self.paths.iter().map(String::as_str),
            query,
            limit,
        )
    }

    /// Reconcile the selected canonical path after re-ranking. A removed path
    /// clears selection; a same-title or same-basename neighbour cannot inherit it.
    pub(crate) fn selection(&self, selected: Option<&str>, rows: &[SearchHit]) -> Option<usize> {
        let selected = selected.filter(|path| self.admits(path))?;
        rows.iter().position(|row| row.path == selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_open_resets_filter_but_query_refresh_does_not() {
        let mut state = AgentSearch::default();
        assert!(state.set_filter(FileFilter::AgentFiles));
        assert!(!state.set_filter(FileFilter::AgentFiles));
        let background_query = state;
        assert_eq!(background_query.filter(), FileFilter::AgentFiles);
        assert_eq!(state.label(), "Agent files");
        state.open();
        assert_eq!(state.filter(), FileFilter::Default);
        assert_eq!(state.label(), "Files");
    }
    #[test]
    fn provider_refresh_rejects_stale_workers_and_keeps_exact_selection() {
        let mut inventory = ProviderInventory::new("vault".into());
        let mut scope = AgentSearch::default();
        scope.set_filter(FileFilter::AgentFiles);
        assert!(inventory.refresh(7, ["b/AGENTS.md", "a/AGENTS.md", "notes.md"]));
        let old = inventory.snapshot(scope);
        assert!(
            inventory.accepts(&old, scope),
            "positive publication control"
        );
        assert!(!inventory.refresh(6, ["stale.md"]));
        assert!(!inventory.refresh(7, ["stale.md"]));
        assert!(inventory.accepts(&old, scope));
        assert!(inventory.refresh(8, ["b/AGENTS.md", "a/AGENTS.md", "notes.md"]));
        assert!(
            !inventory.accepts(&old, scope),
            "content-only refresh invalidates workers"
        );
        let fresh = inventory.snapshot(scope);
        let hit = |path: &str| SearchHit {
            path: path.into(),
            title: "Same title".into(),
            score: 0.,
            snippet_html: String::new(),
            display_snippet: None,
        };
        let rows = [hit("b/AGENTS.md"), hit("a/AGENTS.md")];
        assert_eq!(fresh.selection(Some("a/AGENTS.md"), &rows), Some(1));
        assert_eq!(fresh.selection(Some("notes.md"), &rows), None);
        assert!(inventory.refresh(9, ["b/AGENTS.md"]));
        let deleted = inventory.snapshot(scope);
        assert_eq!(deleted.selection(Some("a/AGENTS.md"), &rows), None);
        assert_eq!(scope.filter(), FileFilter::AgentFiles);
        scope.open();
        assert!(!inventory.accepts(&deleted, scope));
        let another = ProviderInventory::new("other-vault".into());
        assert!(!another.accepts(&deleted, scope));
    }
}
