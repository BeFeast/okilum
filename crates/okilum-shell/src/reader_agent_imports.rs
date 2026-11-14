//! Reader integration model for agent imports. No editing or execution actions.
#![cfg_attr(not(test), allow(dead_code, reason = "#1588, not wired yet"))]
use okilum_core::agent_imports::{self, Expansion, Failure, Import, Limits, Source};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportLink {
    pub source: PathBuf,
    pub import: Import,
    pub destination: Result<PathBuf, Failure>,
}

/// Call only for a classified agent file. The supplied source is the displayed
/// snapshot; offsets must never be applied to a different revision.
pub fn links(store: &impl Source, root: &Path, source: &Path, snapshot: &str) -> Vec<ImportLink> {
    agent_imports::parse(snapshot)
        .into_iter()
        .map(|import| {
            let destination = agent_imports::resolve(store, root, source, &import.target);
            ImportLink {
                source: source.into(),
                import,
                destination,
            }
        })
        .collect()
}

/// Explicit read-only expansion action. The reader renders each child's original
/// text with `Imported::path` as its relative-link context and keeps errors visible.
/// This returns a separate tree, never replacement Markdown for the canonical note.
pub fn expand(
    store: &impl Source,
    root: &Path,
    source: &Path,
    snapshot: &str,
    limits: Limits,
) -> Result<Vec<Expansion>, Failure> {
    agent_imports::expand(store, root, source, snapshot, limits)
}

/// Event-driven state. SourceChanged requires the reader's new displayed snapshot;
/// refreshing imported files cannot authorize reuse of stale source byte ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewState {
    Pending,
    Ready,
    SourceChanged,
    Failed(Failure),
}

/// Session-only, read-only import view. Call refresh on a reader preparation worker,
/// then render links/tree; there is no timer, watcher, persistent cache, or editor.
pub struct LiveImportView {
    revision: Arc<()>,
    root: PathBuf,
    source: PathBuf,
    snapshot: String,
    limits: Limits,
    expanded: bool,
    state: ViewState,
    links: Vec<ImportLink>,
    tree: Vec<Expansion>,
    dependencies: BTreeSet<PathBuf>,
    source_dependencies: BTreeSet<PathBuf>,
}

impl LiveImportView {
    /// Use RootedSource::root() for root and an absolute displayed source path.
    pub fn new(root: PathBuf, source: PathBuf, snapshot: String, limits: Limits) -> Self {
        Self {
            revision: Arc::new(()),
            root,
            source,
            snapshot,
            limits,
            expanded: false,
            state: ViewState::Pending,
            links: Vec::new(),
            tree: Vec::new(),
            dependencies: BTreeSet::new(),
            source_dependencies: BTreeSet::new(),
        }
    }
    pub fn state(&self) -> &ViewState {
        &self.state
    }
    pub fn links(&self) -> &[ImportLink] {
        &self.links
    }
    pub fn tree(&self) -> &[Expansion] {
        &self.tree
    }
    /// 1-based line and hint for each `@@name` line of the displayed snapshot
    /// (#2939). These are never links; the snapshot itself is the authority.
    pub fn ignored(&self) -> Vec<(usize, String)> {
        agent_imports::ignored_bounded(&self.snapshot, self.limits.imports)
            .into_iter()
            .map(|import| {
                let line = self.snapshot[..import.line.start]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
                    + 1;
                (line, agent_imports::ignored_message(&import.target))
            })
            .collect()
    }
    pub fn expanded(&self) -> bool {
        self.expanded
    }
    pub fn dependencies(&self) -> &BTreeSet<PathBuf> {
        &self.dependencies
    }

    fn clear(&mut self) {
        self.links.clear();
        self.tree.clear();
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded == expanded {
            return;
        }
        self.revision = Arc::new(());
        self.expanded = expanded;
        self.clear();
        if self.state != ViewState::SourceChanged {
            self.state = ViewState::Pending;
        }
    }

    /// Called after navigation, source edit, or the reader's revision-aware reload.
    /// It intentionally resets the view even if new text happens to be identical.
    pub fn replace_snapshot(&mut self, source: PathBuf, snapshot: String) {
        self.revision = Arc::new(());
        self.source = source;
        self.snapshot = snapshot;
        self.clear();
        self.dependencies.clear();
        self.source_dependencies.clear();
        self.state = ViewState::Pending;
    }

    /// Feed coalesced watcher changed/removed paths and directory hints here.
    /// Paths may be root-relative or absolute. Missing references are dependencies,
    /// so creating one makes its broken link live on the next refresh.
    /// On true, discard previously rendered import rows before scheduling refresh.
    pub fn invalidate(&mut self, paths: &[PathBuf], rescan: bool) -> bool {
        let paths: Vec<_> = paths
            .iter()
            .filter_map(|path| {
                if path.as_os_str().is_empty() {
                    Some(self.root.clone())
                } else if path.is_absolute() {
                    Some(path.clone())
                } else {
                    agent_imports::reference_path(&self.root.join("_"), path.to_str()?).ok()
                }
            })
            .collect();
        let matches = |dependency: &Path| paths.iter().any(|path| dependency.starts_with(path));
        let source_changed =
            rescan || matches(&self.source) || self.source_dependencies.iter().any(|p| matches(p));
        let affected = source_changed || self.dependencies.iter().any(|p| matches(p));
        if !affected {
            return false;
        }
        self.revision = Arc::new(());
        self.clear();
        if source_changed || self.state == ViewState::SourceChanged {
            self.state = ViewState::SourceChanged;
        } else {
            self.state = ViewState::Pending;
        }
        true
    }

    pub fn refresh(&mut self, store: &impl Source) -> Result<(), Failure> {
        if self.state == ViewState::SourceChanged {
            return Err(Failure::Changed);
        }
        self.revision = Arc::new(());
        self.clear();
        // Retain old dependencies until successful replacement, including failures:
        // a later create/permission repair must be able to revive this view.
        self.source_dependencies.insert(self.source.clone());
        let result = self.build(store);
        match result {
            Ok(()) => {
                self.state = ViewState::Ready;
                Ok(())
            }
            Err(failure) => {
                self.clear();
                self.state = ViewState::Failed(failure.clone());
                Err(failure)
            }
        }
    }

    fn build(&mut self, store: &impl Source) -> Result<(), Failure> {
        let root = store.canonicalize(&self.root)?;
        let source = store.canonicalize(&self.source)?;
        if !root.is_absolute() || !source.starts_with(&root) {
            return Err(Failure::OutsideRoot);
        }
        self.source_dependencies.insert(source.clone());
        let imports =
            agent_imports::parse_bounded(&self.snapshot, self.limits.imports.saturating_add(1));
        if imports.len() > self.limits.imports {
            return Err(Failure::Budget);
        }
        let links: Vec<_> = imports
            .into_iter()
            .map(|import| {
                let destination = agent_imports::resolve(store, &root, &source, &import.target);
                ImportLink {
                    source: source.clone(),
                    import,
                    destination,
                }
            })
            .collect();
        let tree = if self.expanded {
            expand(store, &root, &source, &self.snapshot, self.limits)?
        } else {
            Vec::new()
        };
        let mut dependencies = self.source_dependencies.clone();
        for link in &links {
            track_reference(store, &source, &link.import, &mut dependencies);
            if let Ok(path) = &link.destination {
                dependencies.insert(path.clone());
            }
        }
        track_tree(store, &tree, &mut dependencies);
        self.dependencies = dependencies;
        self.links = links;
        self.tree = tree;
        Ok(())
    }

    /// Capture work on the reader thread, then move the job to a preparation worker.
    /// Returning results must pass apply_prepared; callers cannot supply their own
    /// revision number or transplant offsets from a different displayed snapshot.
    pub fn preparation(&self) -> ImportPreparation {
        let mut view = Self::new(
            self.root.clone(),
            self.source.clone(),
            self.snapshot.clone(),
            self.limits,
        );
        view.expanded = self.expanded;
        view.state = self.state.clone();
        view.dependencies = self.dependencies.clone();
        view.source_dependencies = self.source_dependencies.clone();
        ImportPreparation {
            revision: self.revision.clone(),
            view,
        }
    }

    /// False means stale work was discarded, with the current state left intact.
    pub fn apply_prepared(&mut self, prepared: PreparedImports) -> Result<bool, Failure> {
        if !Arc::ptr_eq(&self.revision, &prepared.revision) {
            return Ok(false);
        }
        *self = prepared.view;
        prepared.result.map(|()| true)
    }

    /// Revalidate immediately before navigation; an outdated rendered link cannot
    /// silently become navigation to a different file after a reference change.
    pub fn open_target(&mut self, store: &impl Source, index: usize) -> Result<PathBuf, Failure> {
        if self.state != ViewState::Ready {
            return Err(Failure::Changed);
        }
        let link = self.links.get(index).ok_or(Failure::InvalidPath)?;
        let current = agent_imports::resolve(store, &self.root, &link.source, &link.import.target);
        if current != link.destination {
            self.revision = Arc::new(());
            self.clear();
            self.state = ViewState::Pending;
            return Err(Failure::Changed);
        }
        current
    }
}

/// Owned immutable input for one worker job; it performs no I/O until run.
pub struct ImportPreparation {
    revision: Arc<()>,
    view: LiveImportView,
}
pub struct PreparedImports {
    revision: Arc<()>,
    view: LiveImportView,
    result: Result<(), Failure>,
}
impl PreparedImports {
    pub fn tree(&self) -> &[Expansion] {
        self.view.tree()
    }
}
impl ImportPreparation {
    pub fn run(mut self, store: &impl Source) -> PreparedImports {
        let result = self.view.refresh(store);
        PreparedImports {
            revision: self.revision,
            view: self.view,
            result,
        }
    }
}

fn track_reference(
    store: &impl Source,
    source: &Path,
    import: &Import,
    paths: &mut BTreeSet<PathBuf>,
) {
    let Some(parent) = source.parent() else {
        return;
    };
    let candidate = parent.join(&import.target);
    // Keep the raw spelling too: changing a symlink before a /../ component
    // changes filesystem resolution even if lexical normalization hides its name.
    paths.insert(candidate.clone());
    if let Ok(path) = agent_imports::reference_path(source, &import.target) {
        paths.insert(path);
    }
    // Resolve the nearest existing ancestor for a missing target beneath a symlink.
    // E.g. @alias/missing watches both alias/missing and real-folder/missing.
    let mut ancestor = candidate.as_path();
    loop {
        if let Ok(canonical) = store.canonicalize(ancestor) {
            if let Ok(suffix) = candidate.strip_prefix(ancestor) {
                paths.insert(canonical.join(suffix));
            }
            break;
        }
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
    }
}

fn track_tree(store: &impl Source, tree: &[Expansion], paths: &mut BTreeSet<PathBuf>) {
    for node in tree {
        paths.insert(node.source.clone());
        track_reference(store, &node.source, &node.import, paths);
        if let Ok(imported) = &node.result {
            paths.insert(imported.path.clone());
            track_tree(store, &imported.children, paths);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Mock;
    impl Source for Mock {
        fn canonicalize(&self, path: &Path) -> Result<PathBuf, Failure> {
            Ok(path.into())
        }
        fn read(&self, path: &Path, _: usize) -> Result<String, Failure> {
            assert_eq!(path, Path::new("/vault/sub/AGENTS.md"));
            Ok("Read-only imported source".into())
        }
    }
    #[test]
    fn link_and_expansion_keep_the_displayed_source_coordinates() {
        let source = Path::new("/vault/sub/CLAUDE.md");
        let snapshot = "@AGENTS.md\r\n";
        let links = links(&Mock, Path::new("/vault"), source, snapshot);
        assert_eq!(links[0].destination, Ok("/vault/sub/AGENTS.md".into()));
        assert_eq!(&snapshot[links[0].import.target_bytes.clone()], "AGENTS.md");
        let tree = expand(
            &Mock,
            Path::new("/vault"),
            source,
            snapshot,
            Limits::default(),
        )
        .unwrap();
        assert_eq!(tree[0].import, links[0].import);
        assert_eq!(
            tree[0].result.as_ref().unwrap().text,
            "Read-only imported source"
        );
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
    };
    struct Store {
        files: RefCell<BTreeMap<PathBuf, String>>,
        alias: RefCell<PathBuf>,
        reads: Cell<usize>,
    }
    impl Store {
        fn new() -> Self {
            Self {
                files: RefCell::new(
                    [
                        ("/vault/main".into(), "@a".into()),
                        ("/vault/a".into(), "@b".into()),
                        ("/vault/b".into(), "old".into()),
                        ("/vault/real/a".into(), "first".into()),
                        ("/vault/second/a".into(), "second".into()),
                    ]
                    .into(),
                ),
                alias: RefCell::new("/vault/real".into()),
                reads: Cell::new(0),
            }
        }
        fn put(&self, path: &str, text: &str) {
            self.files.borrow_mut().insert(path.into(), text.into());
        }
    }
    impl Source for Store {
        fn canonicalize(&self, path: &Path) -> Result<PathBuf, Failure> {
            let path = agent_imports::reference_path(
                Path::new("/_"),
                path.to_str().ok_or(Failure::InvalidPath)?,
            )?;
            let path = if let Ok(suffix) = path.strip_prefix("/vault/alias") {
                self.alias.borrow().join(suffix)
            } else {
                path
            };
            if !path.starts_with("/vault") {
                return Err(Failure::OutsideRoot);
            }
            let files = self.files.borrow();
            if files.contains_key(&path) || files.keys().any(|p| p.starts_with(&path)) {
                Ok(path)
            } else {
                Err(Failure::Missing)
            }
        }
        fn read(&self, path: &Path, max: usize) -> Result<String, Failure> {
            self.reads.set(self.reads.get() + 1);
            let files = self.files.borrow();
            let text = files.get(path).ok_or(Failure::Missing)?;
            if text.len() > max {
                return Err(Failure::Budget);
            }
            Ok(text.clone())
        }
    }
    fn view(snapshot: &str) -> LiveImportView {
        LiveImportView::new(
            "/vault".into(),
            "/vault/main".into(),
            snapshot.into(),
            Limits::default(),
        )
    }
    #[test]
    fn nested_reference_changes_refresh_without_idle_or_unrelated_reads() {
        let store = Store::new();
        let mut view = view("@a");
        view.refresh(&store).unwrap();
        assert_eq!(view.state(), &ViewState::Ready);
        assert_eq!(store.reads.get(), 0); // Links do not load imported source.
        view.set_expanded(true);
        view.refresh(&store).unwrap();
        assert_eq!(store.reads.get(), 2);
        assert!(!view.invalidate(&["unrelated".into()], false));
        assert_eq!(store.reads.get(), 2);
        store.put("/vault/b", "new 日本");
        assert!(view.invalidate(&["b".into()], false));
        assert!(view.tree().is_empty());
        assert!(view.links().is_empty());
        view.refresh(&store).unwrap();
        let a = view.tree()[0].result.as_ref().unwrap();
        assert_eq!(a.children[0].result.as_ref().unwrap().text, "new 日本");
        // Deletion becomes a visible error; recreation revives the same import.
        store.files.borrow_mut().remove(Path::new("/vault/b"));
        assert!(view.invalidate(&["b".into()], false));
        view.refresh(&store).unwrap();
        assert_eq!(
            view.tree()[0].result.as_ref().unwrap().children[0].result,
            Err(Failure::Missing)
        );
        store.put("/vault/b", "restored");
        assert!(view.invalidate(&["b".into()], false));
        view.refresh(&store).unwrap();
        assert_eq!(
            view.tree()[0].result.as_ref().unwrap().children[0]
                .result
                .as_ref()
                .unwrap()
                .text,
            "restored"
        );
    }
    #[test]
    fn source_changes_require_a_new_snapshot_and_drop_old_references() {
        let store = Store::new();
        let mut view = view("@a");
        view.set_expanded(true);
        view.refresh(&store).unwrap();
        let reads = store.reads.get();
        assert!(view.invalidate(&["main".into()], false));
        assert_eq!(view.state(), &ViewState::SourceChanged);
        assert_eq!(view.refresh(&store), Err(Failure::Changed));
        assert_eq!(view.open_target(&store, 0), Err(Failure::Changed));
        assert_eq!(store.reads.get(), reads);
        view.replace_snapshot("/vault/main".into(), "é\n@real/a".into());
        view.refresh(&store).unwrap();
        assert_eq!(view.links()[0].import.target_bytes, 4..10);
        assert!(!view.invalidate(&["a".into(), "b".into()], false));
        assert!(view.invalidate(&[], true));
        assert_eq!(view.state(), &ViewState::SourceChanged);
    }
    #[test]
    fn missing_symlink_reference_creation_and_retarget_are_live() {
        let store = Store::new();
        let mut view = view("@alias/missing");
        view.refresh(&store).unwrap();
        assert_eq!(view.links()[0].destination, Err(Failure::Missing));
        assert!(view
            .dependencies()
            .contains(Path::new("/vault/real/missing")));
        store.put("/vault/real/missing", "created");
        assert!(view.invalidate(&["real/missing".into()], false));
        view.refresh(&store).unwrap();
        assert_eq!(
            view.open_target(&store, 0),
            Ok("/vault/real/missing".into())
        );
        store.put("/vault/second/missing", "new destination");
        *store.alias.borrow_mut() = "/vault/second".into();
        // A click before watcher delivery must not navigate to a changed target.
        assert_eq!(view.open_target(&store, 0), Err(Failure::Changed));
        assert!(view.links().is_empty());
        view.refresh(&store).unwrap();
        assert_eq!(
            view.open_target(&store, 0),
            Ok("/vault/second/missing".into())
        );
        assert!(!view.invalidate(&["real/missing".into()], false));
        assert!(view.invalidate(&["alias".into()], false));
        view.refresh(&store).unwrap();
        assert!(view.invalidate(&[PathBuf::new()], false));
        assert_eq!(view.state(), &ViewState::SourceChanged);
    }
    #[test]
    fn stale_worker_results_cannot_restore_invalidated_offsets_or_expansions() {
        let store = Store::new();
        let mut view = view("@a");
        view.set_expanded(true);
        view.refresh(&store).unwrap();
        let stale = view.preparation().run(&store);
        assert!(view.invalidate(&["b".into()], false));
        assert_eq!(view.apply_prepared(stale), Ok(false));
        assert_eq!(view.state(), &ViewState::Pending);
        assert!(view.tree().is_empty());
        let job = view.preparation();
        view.replace_snapshot("/vault/main".into(), "@real/a".into());
        assert_eq!(view.apply_prepared(job.run(&store)), Ok(false));
        let first = view.preparation();
        let second = view.preparation();
        assert_eq!(view.apply_prepared(first.run(&store)), Ok(true));
        assert_eq!(view.apply_prepared(second.run(&store)), Ok(false));
        assert_eq!(view.tree()[0].result.as_ref().unwrap().text, "first");
        let job = view.preparation();
        view.set_expanded(false);
        assert_eq!(view.apply_prepared(job.run(&store)), Ok(false));
        assert!(view.tree().is_empty());
    }
    #[test]
    fn excessive_top_level_imports_fail_visibly_before_loading_source() {
        let store = Store::new();
        let mut view = LiveImportView::new(
            "/vault".into(),
            "/vault/main".into(),
            "@a\n@b".into(),
            Limits {
                imports: 1,
                ..Limits::default()
            },
        );
        view.set_expanded(true);
        assert_eq!(view.refresh(&store), Err(Failure::Budget));
        assert_eq!(view.state(), &ViewState::Failed(Failure::Budget));
        assert!(view.links().is_empty());
        assert_eq!(store.reads.get(), 0);
    }
}
