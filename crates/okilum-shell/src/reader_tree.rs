//! Derived, per-Reader folder browsing. No filesystem I/O or document bodies.
use okilum_core::vault::{EntryKind, VaultEntry};
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Row {
    pub path: String,
    pub label: String,
    pub kind: EntryKind,
    pub depth: usize,
    pub expanded: bool,
    /// Inside an Archive folder: rendered muted (docs/design/reader.md, #369).
    pub archived: bool,
    /// A `_`/`.` item shown only because hidden files are on or because it
    /// leads to the current note (#395): rendered muted with a marker.
    pub hidden: bool,
    /// The folder's index note (`_index.md`, #1512): listed first in its
    /// folder, muted, with its own icon, and shown without Show hidden
    /// files. Otherwise an ordinary note row.
    pub index: bool,
}

/// Ancestors already scrolled out above the first visible row. Paths retain
/// vault identity; a similarly named sibling can never become a sticky parent.
pub(crate) fn sticky_ancestors(rows: &[Option<Row>], first: usize) -> Vec<Row> {
    let Some(current) = rows.iter().skip(first).find_map(Option::as_ref) else {
        return Vec::new();
    };
    rows.iter()
        .take(first)
        .filter_map(Option::as_ref)
        .filter(|row| {
            row.kind == EntryKind::Directory && current.path.starts_with(&format!("{}/", row.path))
        })
        .cloned()
        .collect()
}

/// Ordinary `_`/`.` paths are hidden in the browser until Show hidden files
/// or navigation reveals them. They remain in inventory for links and search;
/// service directories are excluded by the scanner.
pub fn hidden(path: &str) -> bool {
    path.split('/')
        .any(|segment| segment.starts_with('_') || segment.starts_with('.'))
}

/// PARA archive folders, with or without a numeric prefix ("4 Archive").
pub fn archived(path: &str) -> bool {
    okilum_core::projects::archived(path)
}

#[derive(Clone, Default)]
pub struct Tree {
    root: PathBuf,
    scope: String,
    children: BTreeMap<String, Vec<VaultEntry>>,
    kinds: BTreeMap<String, EntryKind>,
    expanded: BTreeSet<String>,
    pub rows: Rc<Vec<Row>>,
    pub cursor: Option<String>,
    /// Show `_`/`.` items (per-vault preference, #395).
    show_hidden: bool,
    /// A hidden path revealed for navigation; its branch is shown until the
    /// reveal moves elsewhere.
    revealed_hidden: Option<String>,
    templates_folder: String,
    titles: Arc<HashMap<String, String>>,
}

impl Tree {
    /// Inventory membership includes folded/hidden rows, limited to the current scope.
    pub(crate) fn can_reveal(&self, path: &str) -> bool {
        self.kinds.contains_key(path)
            && (self.scope.is_empty()
                || path
                    .strip_prefix(&self.scope)
                    .is_some_and(|tail| tail.starts_with('/')))
    }

    pub(crate) fn is_directory(&self, path: &str) -> bool {
        self.kinds.get(path) == Some(&EntryKind::Directory)
    }

    pub(crate) fn expanded_paths(&self) -> &BTreeSet<String> {
        &self.expanded
    }

    pub(crate) fn restore_expanded(&mut self, expanded: BTreeSet<String>, cursor: Option<String>) {
        self.expanded = expanded;
        self.cursor = cursor;
        self.flatten();
    }

    /// Keep consecutive local operations visible until the watcher catches up.
    #[cfg(any(unix, windows))]
    pub fn entry_created(&mut self, path: &str, kind: EntryKind) {
        let identity = okilum_core::vault::note_path(Path::new(path));
        let path = identity.as_str();
        let mut entries: Vec<_> = self
            .kinds
            .iter()
            .filter(|(p, _)| p.as_str() != path)
            .map(|(path, kind)| VaultEntry {
                path: path.clone(),
                kind: *kind,
            })
            .collect();
        entries.push(VaultEntry {
            path: path.to_owned(),
            kind,
        });
        let root = self.root.clone();
        self.refresh(&root, &entries);
    }

    /// Publish a completed local move before watcher inventory delivery.
    #[cfg(any(unix, windows))]
    pub fn note_moved(&mut self, from: &str, to: &str) {
        let entries: Vec<_> = self
            .kinds
            .iter()
            .map(|(path, kind)| VaultEntry {
                path: okilum_core::link_rewrite::moved_path(path, from, to),
                kind: *kind,
            })
            .collect();
        self.expanded = self
            .expanded
            .iter()
            .map(|p| okilum_core::link_rewrite::moved_path(p, from, to))
            .collect();
        if let Some(cursor) = &mut self.cursor {
            *cursor = okilum_core::link_rewrite::moved_path(cursor, from, to);
        }
        let root = self.root.clone();
        self.refresh(&root, &entries);
    }

    pub fn refresh(&mut self, root: &Path, entries: &[VaultEntry]) {
        if self.root != root {
            *self = Self::default();
            self.root = root.to_path_buf();
            self.templates_folder = "_Assets/Templates".into();
        }
        self.children.clear();
        self.kinds.clear();
        // Hidden items are kept and filtered at flatten time, so a reveal or
        // the Show hidden toggle can bring them back without a rescan.
        let mut rejected = Vec::new();
        for entry in entries {
            if okilum_core::vault::service_path(Path::new(&entry.path)) {
                continue;
            }
            // The root itself, a leading slash or an empty segment would make
            // "" its own child and the traversal endless (#922).
            if !tree_path(&entry.path) {
                rejected.push(entry.path.as_str());
                continue;
            }
            self.kinds.insert(entry.path.clone(), entry.kind);
            let parent = parent(&entry.path).unwrap_or_default();
            self.children
                .entry(parent.to_string())
                .or_default()
                .push(entry.clone());
        }
        // Refresh runs per inventory change; keep one bounded line per refresh.
        if let Some(examples) = rejected
            .get(..rejected.len().min(3))
            .filter(|e| !e.is_empty())
        {
            eprintln!(
                "File tree ignored {} entries with invalid vault paths, e.g. {examples:?}",
                rejected.len()
            );
        }
        // A progressive inventory can contain only one known note. Its path
        // supplies ancestry, but never implies that unknown siblings are absent.
        let mut ancestors = BTreeSet::new();
        for entry in entries.iter().filter(|entry| tree_path(&entry.path)) {
            let mut next = parent(&entry.path);
            while let Some(path) = next {
                if !self.kinds.contains_key(path) {
                    ancestors.insert(path.to_string());
                }
                next = parent(path);
            }
        }
        for path in ancestors {
            self.kinds.insert(path.clone(), EntryKind::Directory);
            self.children
                .entry(parent(&path).unwrap_or_default().to_string())
                .or_default()
                .push(VaultEntry {
                    path,
                    kind: EntryKind::Directory,
                });
        }
        self.sort_children();
        self.expanded
            .retain(|p| self.kinds.get(p) == Some(&EntryKind::Directory));
        if self
            .cursor
            .as_ref()
            .is_some_and(|p| !self.kinds.contains_key(p))
        {
            self.cursor = None;
        }
        self.flatten();
    }

    /// Titles are an existing source projection; never read files while browsing.
    pub(crate) fn set_titles(&mut self, titles: Arc<HashMap<String, String>>) {
        if Arc::ptr_eq(&self.titles, &titles) {
            return;
        }
        self.titles = titles;
        self.sort_children();
        self.flatten();
    }

    fn sort_children(&mut self) {
        let titles = &self.titles;
        for children in self.children.values_mut() {
            // The folder's index note leads its folder, above subfolders (#1512).
            let index = match index_among(children.iter()) {
                FolderIndex::One(path) => Some(path),
                _ => None,
            };
            children.sort_by_cached_key(|entry| {
                let label = if entry.kind == EntryKind::Markdown {
                    titles
                        .get(&entry.path)
                        .cloned()
                        .unwrap_or_else(|| okilum_core::Vault::title_of(&entry.path))
                } else {
                    entry
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&entry.path)
                        .to_owned()
                };
                (
                    index.as_deref() != Some(entry.path.as_str()),
                    entry.kind != EntryKind::Directory,
                    label,
                    entry.path.clone(),
                )
            });
        }
    }

    pub(crate) fn scope_to(&mut self, path: &str) {
        self.scope = path.to_owned();
        self.flatten();
    }

    /// The folder the tree lists: the vault root («»), or a project's folder.
    /// New note / New folder with nothing selected land here (#1512).
    pub(crate) fn scope(&self) -> &str {
        &self.scope
    }

    /// Every folder below the tree's root in path order, hidden ones only
    /// while Show hidden is on: the inline create row's destinations (#1512).
    #[cfg(any(unix, windows))]
    pub(crate) fn folders(&self) -> Vec<String> {
        let prefix = if self.scope.is_empty() {
            String::new()
        } else {
            format!("{}/", self.scope)
        };
        self.kinds
            .range(prefix.clone()..)
            .take_while(|(path, _)| path.starts_with(&prefix))
            .filter(|(path, kind)| {
                **kind == EntryKind::Directory
                    && (self.show_hidden || !hidden(&path[prefix.len()..]))
            })
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn flatten(&mut self) {
        let mut rows = Vec::new();
        let mut stack = vec![(self.scope.clone(), 0)];
        // Each path is visited once even if an inventory ever lists it twice.
        let mut visited = BTreeSet::new();
        // Stack items identify a directory to visit, or a row. Iterative traversal
        // avoids dependence on call stack depth for arbitrary nested vaults.
        while let Some((path, depth)) = stack.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            if path != self.scope {
                let kind = self.kinds[&path];
                let index = self.is_index_note(&path);
                rows.push(Row {
                    hidden: hidden(&path)
                        && !self.template_branch(&path)
                        && !self.shown_index(&path),
                    index,
                    label: if kind == EntryKind::Markdown {
                        self.titles
                            .get(&path)
                            .cloned()
                            .unwrap_or_else(|| okilum_core::Vault::title_of(&path))
                    } else {
                        path.rsplit('/').next().unwrap_or(&path).to_string()
                    },
                    expanded: self.expanded.contains(&path),
                    kind,
                    archived: archived(&path),
                    path: path.clone(),
                    depth: depth - 1,
                });
                if kind != EntryKind::Directory || !self.expanded.contains(&path) {
                    continue;
                }
            }
            if let Some(children) = self.children.get(&path) {
                stack.extend(
                    children
                        .iter()
                        .rev()
                        .filter(|e| self.visible(&e.path))
                        .map(|e| (e.path.clone(), depth + 1)),
                );
            }
        }
        self.rows = Rc::new(rows);
        // Never fall back to a neighbour: a cursor that is not shown is
        // cleared rather than moved to an unrelated row (#395).
        if self
            .cursor
            .as_ref()
            .is_some_and(|p| !self.rows.iter().any(|r| &r.path == p))
        {
            self.cursor = None;
        }
    }

    fn visible(&self, path: &str) -> bool {
        self.show_hidden
            || self.template_branch(path)
            || !hidden(path)
            || self.shown_index(path)
            || self
                .revealed_hidden
                .as_deref()
                .is_some_and(|target| target == path || target.starts_with(&format!("{path}/")))
    }

    fn template_branch(&self, path: &str) -> bool {
        !self.templates_folder.is_empty()
            && self.kinds.get(&self.templates_folder) == Some(&EntryKind::Directory)
            && (path == self.templates_folder
                || self.templates_folder.starts_with(&format!("{path}/"))
                || path.starts_with(&format!("{}/", self.templates_folder)))
    }

    /// `folder`'s index note, read from the tree inventory (no I/O): the
    /// same rule as [`folder_index`] (#1099).
    pub(crate) fn index_of(&self, folder: &str) -> FolderIndex {
        self.children
            .get(folder)
            .map_or(FolderIndex::None, |children| index_among(children.iter()))
    }

    /// Whether `path` is its folder's (only) index note.
    pub(crate) fn is_index_note(&self, path: &str) -> bool {
        index_name(path)
            && self.kinds.get(path) == Some(&EntryKind::Markdown)
            && self.index_of(parent(path).unwrap_or("")) == FolderIndex::One(path.to_owned())
    }

    /// An index note is listed whenever its folder is (#1512): its own `_`
    /// does not hide it, a hidden folder above it still does.
    fn shown_index(&self, path: &str) -> bool {
        parent(path).is_none_or(|folder| !hidden(folder)) && self.is_index_note(path)
    }

    #[cfg(any(unix, windows))]
    pub fn set_templates_folder(&mut self, folder: String) {
        if self.templates_folder != folder {
            self.templates_folder = folder;
            self.flatten();
        }
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Whether Show hidden files being off hides `path`. Recent, Pinned and
    /// Inbox use this so they list what the tree lists (#635); a navigation
    /// reveal and a folder's index row (#1512) apply to the tree only.
    pub fn hidden_by_preference(&self, path: &str) -> bool {
        !self.show_hidden && hidden(path) && !self.template_branch(path)
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        if self.show_hidden != show {
            self.show_hidden = show;
            self.flatten();
        }
    }

    pub fn reveal(&mut self, path: &str) -> Option<usize> {
        // A previously revealed hidden branch goes away again (#395).
        self.revealed_hidden =
            (hidden(path) && !self.show_hidden && !self.shown_index(path)).then(|| path.to_owned());
        if !self.kinds.contains_key(path) {
            // Not in this inventory: select nothing rather than a neighbour.
            self.cursor = None;
            self.flatten();
            return None;
        }
        let mut ancestor = parent(path);
        while let Some(p) = ancestor {
            if self.kinds.get(p) == Some(&EntryKind::Directory) {
                self.expanded.insert(p.to_string());
            }
            ancestor = parent(p);
        }
        self.cursor = Some(path.to_string());
        self.flatten();
        self.cursor_index()
    }

    pub fn cursor_index(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| Some(&r.path) == self.cursor.as_ref())
    }

    pub fn step(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.cursor = None;
            return;
        }
        let ix = self
            .cursor_index()
            .map(|i| (i as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize)
            .unwrap_or(if delta < 0 { self.rows.len() - 1 } else { 0 });
        self.cursor = Some(self.rows[ix].path.clone());
    }

    pub fn toggle(&mut self, path: &str) {
        if self.kinds.get(path) != Some(&EntryKind::Directory) {
            return;
        }
        self.cursor = Some(path.to_string());
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_string());
        }
        self.flatten();
    }

    /// The chevron (#1512): open or close `path` and leave the selection
    /// where it is; a selection inside a closed folder moves up to it rather
    /// than disappearing.
    pub fn set_expanded(&mut self, path: &str, expand: bool) {
        if self.kinds.get(path) != Some(&EntryKind::Directory) {
            return;
        }
        if expand {
            self.expanded.insert(path.to_owned());
        } else {
            self.expanded.remove(path);
            if self
                .cursor
                .as_ref()
                .is_some_and(|c| c.starts_with(&format!("{path}/")))
            {
                self.cursor = Some(path.to_owned());
            }
        }
        self.flatten();
    }

    /// Folders header «Collapse all» (#410).
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.flatten();
    }

    /// «Focus current» (#410): only the path to `path` stays open.
    pub fn focus(&mut self, path: &str) -> Option<usize> {
        self.expanded.clear();
        self.reveal(path)
    }

    /// Expand or collapse `path` together with every folder below it
    /// (⌥-click, ⌥→/⌥←, folder context menu; #410). Only the expanded set
    /// changes: rows are built for what is visible, so a large subtree costs
    /// one pass over its folder names, not a render of its notes.
    pub fn set_subtree(&mut self, path: &str, expand: bool) {
        if self.kinds.get(path) != Some(&EntryKind::Directory) {
            return;
        }
        let prefix = format!("{path}/");
        if expand {
            let below = self
                .kinds
                .range(prefix.clone()..)
                .take_while(|(p, _)| p.starts_with(&prefix))
                .filter(|(p, kind)| **kind == EntryKind::Directory && self.visible(p))
                .map(|(p, _)| p.clone())
                .collect::<Vec<_>>();
            self.expanded.insert(path.to_owned());
            self.expanded.extend(below);
        } else {
            self.expanded
                .retain(|p| p != path && !p.starts_with(&prefix));
            // The cursor may now be inside a closed folder: keep it on the
            // folder rather than clearing it.
            if self.cursor.as_ref().is_some_and(|c| c.starts_with(&prefix)) {
                self.cursor = Some(path.to_owned());
            }
        }
        self.flatten();
    }

    /// Closest existing directory for a proposed new path, including collapsed branches.
    #[cfg(any(unix, windows))]
    pub fn creation_parent(&self, path: &Path) -> String {
        path.parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .filter_map(Path::to_str)
            .find(|p| self.kinds.get(*p) == Some(&EntryKind::Directory))
            .unwrap_or("")
            .to_owned()
    }

    /// Destination for creation from the selected tree row, even in a collapsed branch.
    #[cfg(any(unix, windows))]
    pub fn selected_creation_folder(&self) -> Option<String> {
        let cursor = self.cursor.as_deref()?;
        match self.kinds.get(cursor)? {
            EntryKind::Directory => Some(cursor.to_owned()),
            _ => Some(parent(cursor).unwrap_or("").to_owned()),
        }
    }

    /// The folder ⌥→/⌥← act on: the cursor folder, or a note's parent.
    pub fn cursor_folder(&self) -> Option<String> {
        let cursor = self.cursor.as_deref()?;
        if self.kinds.get(cursor) == Some(&EntryKind::Directory) {
            Some(cursor.to_owned())
        } else {
            parent(cursor).map(str::to_owned)
        }
    }

    pub fn right(&mut self) {
        let Some(ix) = self.cursor_index() else {
            self.step(1);
            return;
        };
        let row = &self.rows[ix];
        if row.kind == EntryKind::Directory {
            if !row.expanded {
                let path = row.path.clone();
                self.toggle(&path);
            } else if self.rows.get(ix + 1).is_some_and(|r| r.depth > row.depth) {
                self.step(1);
            }
        }
    }

    pub fn left(&mut self) {
        let Some(ix) = self.cursor_index() else {
            return;
        };
        let row = &self.rows[ix];
        if row.kind == EntryKind::Directory && row.expanded {
            let path = row.path.clone();
            self.toggle(&path);
        } else {
            self.cursor = parent(&row.path)
                .map(str::to_string)
                .or(self.cursor.clone());
        }
    }
}

/// A vault-relative tree path: non-empty, `/`-separated, no empty or dot segments.
/// A folder's index note (#1099): `_index.md` directly inside `folder`
/// (`""` is the vault root), any letter case.
#[derive(Debug, PartialEq, Eq)]
pub enum FolderIndex {
    None,
    One(String),
    /// Several spellings on a case-sensitive file system: never picked.
    Several,
}

pub fn folder_index(folder: &str, entries: &[VaultEntry]) -> FolderIndex {
    index_among(
        entries
            .iter()
            .filter(|entry| parent(&entry.path).unwrap_or("") == folder),
    )
}

fn index_name(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("_index.md"))
}

/// The index note among one folder's direct children.
fn index_among<'a>(children: impl Iterator<Item = &'a VaultEntry>) -> FolderIndex {
    let mut found =
        children.filter(|entry| entry.kind == EntryKind::Markdown && index_name(&entry.path));
    match (found.next(), found.next()) {
        (None, _) => FolderIndex::None,
        (Some(entry), None) => FolderIndex::One(entry.path.clone()),
        _ => FolderIndex::Several,
    }
}

fn tree_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(p, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn sidebar_titles_follow_reconciled_sources_and_keep_note_identity(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::*;
        cx.update(gpui_component::init);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        for (name, text) in [
            ("24q4.md", "---\ntitle: Old title\n---\n# Quarterly plans\n"),
            ("legacy.md", "---\ntitle: Legacy title\n---\nBody\n"),
            ("plain.md", "No heading or metadata.\n"),
            ("duplicate.md", "# Quarterly plans\n"),
            ("Upper.MD", "# Upper title\n"),
        ] {
            std::fs::write(root.join(name), text).unwrap();
        }
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("24q4.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1200.), px(800.)));
        visual.run_until_parked();
        reader.update(visual, |reader, cx| {
            reader.panels.open(reader_layout::Panel::Notes);
            assert_eq!(
                reader.backlink_titles.get("24q4.md").map(String::as_str),
                Some("Quarterly plans"),
                "reconciled title positive control"
            );
            // Recent and Pinned (title bar, #1508) label notes the same way.
            assert_eq!(reader.note_label("24q4.md"), "Quarterly plans");
            assert_eq!(reader.note_label("Upper.MD"), "Upper title");
            assert!(!reader.tree.is_directory("Upper.MD"));
            assert_eq!(reader.note_label("legacy.md"), "Legacy title");
            assert_eq!(
                reader
                    .tree
                    .rows
                    .iter()
                    .map(|r| (r.path.as_str(), r.label.as_str()))
                    .collect::<Vec<_>>(),
                [
                    ("legacy.md", "Legacy title"),
                    ("24q4.md", "Quarterly plans"),
                    ("duplicate.md", "Quarterly plans"),
                    ("Upper.MD", "Upper title"),
                    ("plain.md", "plain"),
                ]
            );
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.simulate_next_frame(cx));
        let row = visual
            .debug_bounds("tree-row-duplicate.md")
            .expect("rendered title row positive control");
        visual.simulate_click(row.center(), gpui::Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "duplicate.md")
        });

        // Updating metadata and adding a sibling exercises the topology refresh
        // that must retain titles belonging to the same root.
        std::fs::write(
            root.join("legacy.md"),
            "---\ntitle: A revised title\n---\nBody\n",
        )
        .unwrap();
        std::fs::write(root.join("added.md"), "# Added title\n").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.apply_vault_changes(
                okilum_core::Changes {
                    changed: BTreeSet::from(["legacy.md".into(), "added.md".into()]),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.note_label("legacy.md"), "A revised title");
            assert_eq!(reader.note_label("24q4.md"), "Quarterly plans");
            assert_eq!(reader.tree.rows[0].label, "A revised title");
            assert_eq!(reader.current_rel, "duplicate.md");
        });
        assert_eq!(
            std::fs::read_to_string(root.join("24q4.md")).unwrap(),
            "---\ntitle: Old title\n---\n# Quarterly plans\n"
        );
    }

    #[test]
    fn reveal_eligibility_includes_hidden_folded_entries_but_respects_scope() {
        let mut tree = Tree::default();
        tree.refresh(
            Path::new("/fixture"),
            &[
                VaultEntry {
                    path: "Project/_Hidden.md".into(),
                    kind: EntryKind::Markdown,
                },
                VaultEntry {
                    path: "Other.md".into(),
                    kind: EntryKind::Markdown,
                },
            ],
        );
        tree.collapse_all();
        assert!(tree.can_reveal("Project/_Hidden.md"));
        assert!(tree.can_reveal("Other.md"));
        assert!(!tree.can_reveal("Missing.md"));
        tree.scope_to("Project");
        assert!(tree.can_reveal("Project/_Hidden.md"));
        assert!(!tree.can_reveal("Other.md"));
        tree.scope_to("Proj");
        assert!(
            !tree.can_reveal("Project/_Hidden.md"),
            "whole path segments only"
        );
    }

    #[test]
    fn cached_titles_sort_by_display_name_keep_paths_and_refresh_without_io() {
        let mut tree = Tree::default();
        let entries = vec![
            entry("z.md", EntryKind::Markdown),
            entry("a.md", EntryKind::Markdown),
            entry("b.md", EntryKind::Markdown),
            entry("plain.md", EntryKind::Markdown),
            entry("Folder", EntryKind::Directory),
            entry("photo.png", EntryKind::Attachment),
        ];
        // A nonexistent root is a positive control against filesystem reads.
        tree.refresh(Path::new("/nonexistent-title-fixture"), &entries);
        let titles = Arc::new(HashMap::from([
            ("z.md".into(), "Alpha".into()),
            ("a.md".into(), "Same.md".into()),
            ("b.md".into(), "Same.md".into()),
        ]));
        tree.set_titles(titles.clone());
        assert_eq!(
            tree.rows
                .iter()
                .map(|r| (r.path.as_str(), r.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("Folder", "Folder"),
                ("z.md", "Alpha"),
                ("a.md", "Same.md"),
                ("b.md", "Same.md"),
                ("photo.png", "photo.png"),
                ("plain.md", "plain"),
            ]
        );
        tree.cursor = Some("b.md".into());
        let rows = tree.rows.clone();
        tree.set_titles(titles);
        assert!(
            Rc::ptr_eq(&rows, &tree.rows),
            "unchanged titles do not rebuild rows"
        );
        tree.set_titles(Arc::new(HashMap::from([("b.md".into(), "AAA".into())])));
        assert_eq!(tree.rows[1].path, "b.md");
        assert_eq!(tree.cursor.as_deref(), Some("b.md"));
        tree.refresh(Path::new("/another-vault"), &entries);
        assert_eq!(
            tree.rows[1].label, "a",
            "titles cannot leak to another vault"
        );
    }
    fn entry(path: &str, kind: EntryKind) -> VaultEntry {
        VaultEntry {
            path: path.into(),
            kind,
        }
    }
    #[test]
    fn scoped_tree_keeps_vault_paths_and_sticky_ancestors_exclude_siblings() {
        use EntryKind::{Directory, Markdown};
        let entries = [
            entry("Music", Directory),
            entry("Music/Areas", Directory),
            entry("Music/Areas/Studio", Directory),
            entry("Music/Areas/Studio/note.md", Markdown),
            entry("Music/Areas/Studio2", Directory),
            entry("Music/Areas/Studio2/other.md", Markdown),
        ];
        let mut tree = Tree::default();
        tree.refresh(Path::new("/vault"), &entries);
        tree.set_subtree("Music", true);
        let original = tree.rows.iter().map(|r| r.path.clone()).collect::<Vec<_>>();
        let rows: Vec<_> = tree.rows.iter().cloned().map(Some).collect();
        let first = tree
            .rows
            .iter()
            .position(|r| r.path == "Music/Areas/Studio2/other.md")
            .unwrap();
        assert_eq!(
            sticky_ancestors(&rows, first)
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            ["Music", "Music/Areas", "Music/Areas/Studio2"]
        );
        assert!(sticky_ancestors(&rows, 0).is_empty());
        let saved = tree.clone();
        tree.scope_to("Music/Areas/Studio");
        assert_eq!(tree.rows.len(), 1);
        assert_eq!(tree.rows[0].path, "Music/Areas/Studio/note.md");
        assert_eq!(tree.rows[0].depth, 0);
        tree.refresh(Path::new("/vault"), &entries);
        assert_eq!(
            tree.rows.len(),
            1,
            "inventory refresh preserves the selected scope"
        );
        tree = saved;
        assert_eq!(
            tree.rows.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
            original
        );
    }

    #[test]
    fn folder_index_is_a_direct_child_in_any_case_and_never_guessed() {
        use EntryKind::{Directory, Markdown};
        let entries = [
            entry("Dev", Directory),
            entry("Dev/Areas", Directory),
            entry("Dev/Areas/apertune", Directory),
            entry("Dev/Areas/apertune/_index.md", Markdown),
            entry("Dev/Areas/apertune/specs/_index.md", Markdown),
            entry("Dev/Areas/other", Directory),
            entry("Dev/Areas/other/index.md", Markdown),
            entry("Dev/Areas/twice", Directory),
            entry("Dev/Areas/twice/_index.md", Markdown),
            entry("Dev/Areas/twice/_Index.md", Markdown),
            entry("_Index.md", Markdown),
        ];
        assert_eq!(
            folder_index("Dev/Areas/apertune", &entries),
            FolderIndex::One("Dev/Areas/apertune/_index.md".into())
        );
        // Positive control: a nested index or another name is not this folder's.
        assert_eq!(folder_index("Dev/Areas", &entries), FolderIndex::None);
        assert_eq!(folder_index("Dev/Areas/other", &entries), FolderIndex::None);
        assert_eq!(
            folder_index("Dev/Areas/twice", &entries),
            FolderIndex::Several
        );
        assert_eq!(
            folder_index("", &entries),
            FolderIndex::One("_Index.md".into())
        );
    }

    #[test]
    fn index_rows_lead_their_folder_stay_listed_and_the_chevron_keeps_the_selection() {
        use EntryKind::{Directory, Markdown};
        let entries = [
            entry("Area", Directory),
            entry("Area/Alpha.md", Markdown),
            entry("Area/sub", Directory),
            entry("Area/sub/deep.md", Markdown),
            entry("Area/_index.md", Markdown),
            entry("Area/_draft.md", Markdown),
            entry("Twice", Directory),
            entry("Twice/_index.md", Markdown),
            entry("Twice/_Index.md", Markdown),
            entry("_System", Directory),
            entry("_System/_index.md", Markdown),
        ];
        let mut t = Tree::default();
        t.refresh(Path::new("/v"), &entries);
        // The title would sort it after «Alpha»; the index still leads.
        t.set_titles(Arc::new(HashMap::from([(
            "Area/_index.md".into(),
            "Zeta".into(),
        )])));
        t.set_expanded("Area", true);
        t.set_expanded("Twice", true);
        assert_eq!(
            paths(&t),
            [
                "Area",
                "Area/_index.md",
                "Area/sub",
                "Area/Alpha.md",
                "Twice"
            ],
            "index first and listed without Show hidden; `_draft` and the ambiguous pair stay hidden"
        );
        let index = &t.rows[1];
        assert!(index.index && !index.hidden && index.label == "Zeta");
        assert!(t.rows.iter().filter(|r| r.index).count() == 1);
        assert!(t.is_index_note("Area/_index.md"));
        assert!(!t.is_index_note("Twice/_index.md"), "two spellings: none");
        // Recent and Pinned keep following Show hidden files for it.
        assert!(t.hidden_by_preference("Area/_index.md"));
        assert_eq!(
            t.index_of("Area"),
            FolderIndex::One("Area/_index.md".into())
        );
        assert_eq!(t.index_of("Area/sub"), FolderIndex::None);
        // Revealing the index does not count as a hidden-branch reveal.
        assert_eq!(t.reveal("Area/_index.md"), Some(1));
        assert!(t.revealed_hidden.is_none());
        // With Show hidden on, the index of a hidden folder is still flagged.
        t.set_show_hidden(true);
        t.set_expanded("_System", true);
        let system = t
            .rows
            .iter()
            .find(|r| r.path == "_System/_index.md")
            .unwrap();
        assert!(system.index && system.hidden);
        t.set_show_hidden(false);

        // The chevron: the selection does not follow the toggled folder ...
        t.cursor = Some("Twice".into());
        t.set_expanded("Area/sub", true);
        assert_eq!(t.cursor.as_deref(), Some("Twice"));
        t.set_expanded("Area/sub", false);
        assert_eq!(t.cursor.as_deref(), Some("Twice"));
        // ... unless it would vanish inside the folder being closed.
        t.set_expanded("Area/sub", true);
        t.cursor = Some("Area/sub/deep.md".into());
        t.set_expanded("Area", false);
        assert_eq!(t.cursor.as_deref(), Some("Area"));
        assert_eq!(paths(&t), ["Area", "Twice"]);
        // Notes are not folders.
        t.set_expanded("Area/Alpha.md", true);
        assert_eq!(paths(&t), ["Area", "Twice"]);
    }

    #[test]
    fn invalid_entry_paths_cannot_make_the_root_its_own_child() {
        let valid = [
            entry("notes", EntryKind::Directory),
            entry("notes/a.md", EntryKind::Markdown),
        ];
        let mut tree = Tree::default();
        // Each of these made "" an ancestor or child of itself (#922).
        for bad in ["", "/x.md", "notes//b.md", "./c.md", "notes/../d.md"] {
            let mut entries = valid.to_vec();
            entries.push(entry(bad, EntryKind::Markdown));
            tree.refresh(Path::new("/vault"), &entries);
            assert!(!tree.kinds.contains_key(""), "{bad:?}");
            assert!(
                tree.children
                    .values()
                    .flatten()
                    .all(|child| !child.path.is_empty()),
                "{bad:?}"
            );
            tree.expanded.insert("notes".into());
            tree.flatten();
            let paths: Vec<_> = tree.rows.iter().map(|row| row.path.as_str()).collect();
            assert_eq!(paths, ["notes", "notes/a.md"], "{bad:?}");
        }
        // Positive control: the same shape without the bad entry is shown.
        tree.refresh(Path::new("/vault"), &valid);
        tree.expanded.insert("notes".into());
        tree.flatten();
        assert_eq!(tree.rows.len(), 2);
    }

    #[test]
    fn flatten_visits_a_repeated_path_once() {
        let mut tree = Tree::default();
        let note = entry("a.md", EntryKind::Markdown);
        tree.refresh(Path::new("/vault"), &[note.clone(), note]);
        let paths: Vec<_> = tree.rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths, ["a.md"]);
    }

    #[test]
    fn windows_vault_paths_tree_creation_publishes_canonical_ancestry() {
        let mut tree = Tree::default();
        tree.refresh(Path::new("/vault"), &[]);
        let parent = Path::new("Dev").join("Projects").join("okilum-qa");
        let folder = parent.join("win-qa-folder");
        tree.entry_created(folder.to_str().unwrap(), EntryKind::Directory);
        let note = folder.join("win-qa-b.md");
        tree.entry_created(note.to_str().unwrap(), EntryKind::Markdown);
        assert!(tree
            .reveal("Dev/Projects/okilum-qa/win-qa-folder/win-qa-b.md")
            .is_some());
        assert!(tree.kinds.keys().all(|path| !path.contains('\\')));
        assert_eq!(
            tree.children["Dev/Projects/okilum-qa"][0].path,
            "Dev/Projects/okilum-qa/win-qa-folder"
        );
        assert_eq!(
            tree.children["Dev/Projects/okilum-qa/win-qa-folder"][0].path,
            "Dev/Projects/okilum-qa/win-qa-folder/win-qa-b.md"
        );
        assert_eq!(tree.rows.last().unwrap().label, "win-qa-b");
    }
    #[test]
    fn templates_are_visible_without_revealing_other_hidden_branches() {
        let entries: Vec<_> = [
            "_Assets",
            "_Assets/Templates",
            "_Assets/Private",
            "_Assets/Templates/Note.md",
            "_Assets/Private/secret.md",
        ]
        .iter()
        .map(|path| VaultEntry {
            path: (*path).into(),
            kind: if path.ends_with(".md") {
                EntryKind::Markdown
            } else {
                EntryKind::Directory
            },
        })
        .collect();
        let mut tree = Tree::default();
        tree.refresh(Path::new("/vault"), &entries);
        assert!(tree.rows.iter().any(|r| r.path == "_Assets" && !r.hidden));
        tree.toggle("_Assets");
        assert!(tree
            .rows
            .iter()
            .any(|r| r.path == "_Assets/Templates" && !r.hidden));
        assert!(!tree.rows.iter().any(|r| r.path == "_Assets/Private"));
        tree.toggle("_Assets/Templates");
        assert!(tree
            .rows
            .iter()
            .any(|r| r.path == "_Assets/Templates/Note.md" && !r.hidden));
    }

    #[test]
    fn hierarchy_reveal_keyboard_refresh_and_root_isolation() {
        use EntryKind::*;
        let entries = vec![
            entry("Dev", Directory),
            entry("Dev/Areas", Directory),
            entry("Dev/Areas/Заметка.md", Markdown),
            entry("Empty", Directory),
            entry("Заметка.md", Markdown),
            entry("Dev/pic.png", Attachment),
        ];
        let mut t = Tree::default();
        t.refresh(Path::new("/a"), &entries);
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.reveal("Dev/Areas/Заметка.md"), Some(2));
        t.left();
        assert_eq!(t.cursor.as_deref(), Some("Dev/Areas"));
        t.left();
        assert!(!t.rows.iter().any(|r| r.path == "Dev/Areas/Заметка.md"));
        t.right();
        t.right();
        assert_eq!(t.cursor.as_deref(), Some("Dev/Areas/Заметка.md"));
        t.refresh(Path::new("/a"), &entries[..2]);
        assert!(t.cursor.is_none());
        t.refresh(Path::new("/b"), &entries);
        assert_eq!(t.rows.len(), 3);
        assert!(t.cursor.is_none());
        assert!(t.reveal("missing.md").is_none());
        assert!(t.reveal("Заметка.md").is_some());
    }
    #[test]
    fn system_folders_hidden_and_archive_marked() {
        use EntryKind::*;
        let entries = vec![
            entry("_Assets", Directory),
            entry("_Assets/logo.png", Attachment),
            entry("Work", Directory),
            entry("Work/4 Archive", Directory),
            entry("Work/4 Archive/old.md", Markdown),
            entry("Work/Projects", Directory),
            entry("Work/Projects/plan.md", Markdown),
        ];
        let mut t = Tree::default();
        t.refresh(Path::new("/v"), &entries);
        // Positive control: the visible root exists; the system folder does not.
        assert_eq!(
            t.rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            ["Work"]
        );
        t.toggle("Work");
        let archive = t.rows.iter().find(|r| r.path == "Work/4 Archive").unwrap();
        assert!(archive.archived && !archive.expanded);
        assert!(
            !t.rows
                .iter()
                .find(|r| r.path == "Work/Projects")
                .unwrap()
                .archived
        );
    }

    #[test]
    fn hidden_notes_reveal_temporarily_and_never_select_a_neighbour() {
        use EntryKind::*;
        let entries = vec![
            entry("Life", Directory),
            entry("Life/plan.md", Markdown),
            entry("_AgentContract.md", Markdown),
            entry("_System", Directory),
            entry("_System/tool.md", Markdown),
        ];
        let mut t = Tree::default();
        t.refresh(Path::new("/v"), &entries);
        let paths = |t: &Tree| t.rows.iter().map(|r| r.path.clone()).collect::<Vec<_>>();
        assert_eq!(
            paths(&t),
            ["Life"],
            "positive control: hidden items start hidden"
        );
        // Navigating to a hidden root note shows it (marked) and selects it,
        // never the neighbouring folder.
        assert_eq!(t.reveal("_AgentContract.md"), Some(1));
        assert_eq!(t.cursor.as_deref(), Some("_AgentContract.md"));
        assert!(t.rows[1].hidden);
        // A hidden folder's branch opens for a nested hidden note.
        assert!(t.reveal("_System/tool.md").is_some());
        assert!(paths(&t).contains(&"_System".to_string()));
        assert!(
            !paths(&t).contains(&"_AgentContract.md".to_string()),
            "earlier reveal gone"
        );
        // Moving to a visible note hides the branch again.
        assert!(t.reveal("Life/plan.md").is_some());
        assert!(!paths(&t).iter().any(|p| p.starts_with('_')));
        // An unknown note selects nothing.
        assert!(t.reveal("missing.md").is_none());
        assert_eq!(t.cursor, None);
        // Show hidden files lists them all.
        t.set_show_hidden(true);
        assert!(paths(&t).contains(&"_AgentContract.md".to_string()));
        assert!(paths(&t).contains(&"_System".to_string()));
    }

    #[test]
    fn hidden_by_preference_matches_tree_visibility() {
        use EntryKind::*;
        let entries = [
            entry("Life", Directory),
            entry("Life/plan.md", Markdown),
            entry(".dot.md", Markdown),
            entry("_System", Directory),
            entry("_System/tool.md", Markdown),
            entry("_Assets", Directory),
            entry("_Assets/Templates", Directory),
            entry("_Assets/Templates/Daily.md", Markdown),
        ];
        let mut t = Tree::default();
        t.refresh(Path::new("/vault"), &entries);
        for path in ["Life", "_System", "_Assets"] {
            t.toggle(path);
        }
        t.toggle("_Assets/Templates");
        let rows = paths(&t);
        for e in &entries {
            assert_eq!(
                t.hidden_by_preference(&e.path),
                !rows.contains(&e.path.as_str()),
                "{} must match the tree",
                e.path
            );
        }
        assert!(t.hidden_by_preference(".dot.md"));
        assert!(t.hidden_by_preference("_System/tool.md"));
        assert!(!t.hidden_by_preference("_Assets/Templates/Daily.md"));
        // A navigation reveal is tree-only and does not unhide sections.
        t.reveal("_System/tool.md");
        assert!(t.hidden_by_preference("_System/tool.md"));
        t.set_show_hidden(true);
        assert!(!entries.iter().any(|e| t.hidden_by_preference(&e.path)));
    }

    #[test]
    fn partial_note_inventory_reveals_known_ancestry_without_io() {
        let vault = okilum_core::Vault::from_note_paths(["A/B/known.md".to_string()]);
        let mut tree = Tree::default();
        tree.refresh(Path::new("/partial"), &vault.entries);
        assert_eq!(tree.reveal("A/B/known.md"), Some(2));
        assert!(tree.reveal("A/B/unknown.md").is_none());
    }

    #[test]
    fn five_thousand_paths_have_no_flat_cap_and_only_expanded_rows() {
        use EntryKind::*;
        let mut entries = vec![
            entry("Projects", Directory),
            entry("Projects/deep", Directory),
        ];
        entries.extend((0..5000).map(|i| entry(&format!("Projects/deep/{i:04}.md"), Markdown)));
        let mut t = Tree::default();
        t.refresh(Path::new("/vault"), &entries);
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.reveal("Projects/deep/4999.md"), Some(5001));
        t.step(-4999);
        assert_eq!(t.cursor.as_deref(), Some("Projects/deep/0000.md"));
        t.toggle("Projects");
        assert_eq!(t.rows.len(), 1);
    }

    fn nested() -> Tree {
        use EntryKind::*;
        let entries = vec![
            entry("A", Directory),
            entry("A/B", Directory),
            entry("A/B/C", Directory),
            entry("A/B/C/deep.md", Markdown),
            entry("A/B/b.md", Markdown),
            entry("A/_sys", Directory),
            entry("A/_sys/x.md", Markdown),
            entry("AB", Directory),
            entry("AB/other.md", Markdown),
            entry("Z", Directory),
            entry("Z/z.md", Markdown),
        ];
        let mut t = Tree::default();
        t.refresh(Path::new("/v"), &entries);
        t
    }

    fn paths(t: &Tree) -> Vec<&str> {
        t.rows.iter().map(|r| r.path.as_str()).collect()
    }

    #[test]
    fn subtree_expand_and_collapse_stay_inside_the_folder() {
        let mut t = nested();
        t.set_subtree("A", true);
        assert_eq!(
            paths(&t),
            ["A", "A/B", "A/B/C", "A/B/C/deep.md", "A/B/b.md", "AB", "Z"],
            "`AB` shares the prefix but is not inside `A`; `_sys` stays hidden"
        );
        t.cursor = Some("A/B/C/deep.md".into());
        t.set_subtree("A", false);
        assert_eq!(paths(&t), ["A", "AB", "Z"]);
        assert_eq!(t.cursor.as_deref(), Some("A"), "cursor lands on the folder");
        // Collapsed recursively: reopening only `A` shows its direct children.
        t.toggle("A");
        assert_eq!(paths(&t), ["A", "A/B", "AB", "Z"]);
        // Notes are not folders: nothing happens.
        t.set_subtree("Z/z.md", true);
        assert_eq!(paths(&t), ["A", "A/B", "AB", "Z"]);
    }

    #[test]
    fn collapse_all_and_focus_current() {
        let mut t = nested();
        t.set_subtree("A", true);
        t.toggle("Z");
        t.collapse_all();
        assert_eq!(paths(&t), ["A", "AB", "Z"]);
        t.toggle("Z");
        t.set_subtree("A", true);
        assert_eq!(t.focus("A/B/b.md"), Some(3));
        assert_eq!(paths(&t), ["A", "A/B", "A/B/C", "A/B/b.md", "AB", "Z"]);
        assert_eq!(t.cursor.as_deref(), Some("A/B/b.md"));
        assert_eq!(t.cursor_folder().as_deref(), Some("A/B"));
    }

    #[test]
    fn large_subtree_expands_in_one_pass() {
        use EntryKind::*;
        let mut entries = vec![entry("Big", Directory)];
        for d in 0..2500 {
            entries.push(entry(&format!("Big/{d:04}"), Directory));
            for n in 0..20 {
                entries.push(entry(&format!("Big/{d:04}/{n:02}.md"), Markdown));
            }
        }
        let mut t = Tree::default();
        t.refresh(Path::new("/v"), &entries);
        let started = std::time::Instant::now();
        t.set_subtree("Big", true);
        let expanded = started.elapsed();
        assert_eq!(t.rows.len(), 1 + 2500 * 21);
        let started = std::time::Instant::now();
        t.set_subtree("Big", false);
        eprintln!(
            "52 501-row subtree: expand {expanded:?}, collapse {:?}",
            started.elapsed()
        );
        assert_eq!(t.rows.len(), 1);
    }
}

#[cfg(test)]
mod preimage_visibility_tests {
    use super::*;
    #[test]
    fn windows_history_tree_hides_native_preimages_even_when_hidden_files_are_enabled() {
        let mut tree = Tree::default();
        let preimage = "nested/.okilum-save-7d7d7698-4f47-4a9f-9a7b-2bb5e01f3718.previous";
        let mut entries = vec![
            VaultEntry {
                path: "nested".into(),
                kind: EntryKind::Directory,
            },
            VaultEntry {
                path: "nested/Visible.md".into(),
                kind: EntryKind::Markdown,
            },
            VaultEntry {
                path: preimage.into(),
                kind: EntryKind::Attachment,
            },
            VaultEntry {
                path: "nested/.ordinary.md".into(),
                kind: EntryKind::Markdown,
            },
        ];
        let service_names = [
            ".okilum-save-icf3uR",
            ".okilum-save-legacy.md",
            ".okilum-save-proposed.prepared",
            ".okilum-save-user.previous",
        ];
        entries.extend(service_names.into_iter().map(|name| VaultEntry {
            path: format!("nested/{name}"),
            kind: EntryKind::Attachment,
        }));
        tree.refresh(Path::new("vault"), &entries);
        tree.set_show_hidden(true);
        tree.set_subtree("nested", true);
        assert!(tree.rows.iter().any(|row| row.path == "nested/Visible.md"));
        assert!(tree
            .rows
            .iter()
            .any(|row| row.path == "nested/.ordinary.md"));
        for name in service_names {
            let path = format!("nested/{name}");
            assert!(!tree.kinds.contains_key(&path));
            assert!(tree.reveal(&path).is_none());
        }
        assert!(!tree.rows.iter().any(|row| row.path == preimage));
        assert!(tree.reveal(preimage).is_none());
        assert!(!tree.kinds.contains_key(preimage));
    }
}
