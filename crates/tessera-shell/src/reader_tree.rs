//! Derived, per-Reader folder browsing. No filesystem I/O or document bodies.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use tessera_core::vault::{EntryKind, VaultEntry};

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
    tessera_core::projects::archived(path)
}

#[derive(Default)]
pub struct Tree {
    root: PathBuf,
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
}

impl Tree {
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
                path: tessera_core::link_rewrite::moved_path(path, from, to),
                kind: *kind,
            })
            .collect();
        self.expanded = self
            .expanded
            .iter()
            .map(|p| tessera_core::link_rewrite::moved_path(p, from, to))
            .collect();
        if let Some(cursor) = &mut self.cursor {
            *cursor = tessera_core::link_rewrite::moved_path(cursor, from, to);
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
        for entry in entries {
            if tessera_core::vault::service_path(Path::new(&entry.path)) {
                continue;
            }
            self.kinds.insert(entry.path.clone(), entry.kind);
            let parent = parent(&entry.path).unwrap_or_default();
            self.children
                .entry(parent.to_string())
                .or_default()
                .push(entry.clone());
        }
        // A progressive inventory can contain only one known note. Its path
        // supplies ancestry, but never implies that unknown siblings are absent.
        let mut ancestors = BTreeSet::new();
        for entry in entries {
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
        for children in self.children.values_mut() {
            children.sort_by(|a, b| {
                (a.kind != EntryKind::Directory, &a.path)
                    .cmp(&(b.kind != EntryKind::Directory, &b.path))
            });
        }
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

    fn flatten(&mut self) {
        let mut rows = Vec::new();
        let mut stack = vec![("".to_string(), 0)];
        // Stack items identify a directory to visit, or a row. Iterative traversal
        // avoids dependence on call stack depth for arbitrary nested vaults.
        while let Some((path, depth)) = stack.pop() {
            if !path.is_empty() {
                let kind = self.kinds[&path];
                rows.push(Row {
                    hidden: hidden(&path) && !self.template_branch(&path),
                    label: path.rsplit('/').next().unwrap_or(&path).to_string(),
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
    /// reveal applies to the tree only.
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
        self.revealed_hidden = (hidden(path) && !self.show_hidden).then(|| path.to_owned());
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

fn parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(p, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(path: &str, kind: EntryKind) -> VaultEntry {
        VaultEntry {
            path: path.into(),
            kind,
        }
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
        let vault = tessera_core::Vault::from_note_paths(["A/B/known.md".to_string()]);
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
        let preimage = "nested/.tessera-save-7d7d7698-4f47-4a9f-9a7b-2bb5e01f3718.previous";
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
            ".tessera-save-icf3uR",
            ".tessera-save-legacy.md",
            ".tessera-save-proposed.prepared",
            ".tessera-save-user.previous",
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
