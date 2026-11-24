//! Virtual sidebar from a vault-authored tree or the built-in domain list.
use super::*;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use okilum_core::{
    records::Projection,
    vault::{warm::Snapshot, EntryKind},
    vault_descriptor::Layout,
    view,
};
use reader_sidebar::LeftMode;
use std::collections::BTreeMap;

pub(super) type Published = Option<Result<Arc<Tree>, String>>;

pub(super) struct Tree {
    rows: Vec<Row>,
    template: Option<String>,
    notice: Option<String>,
}

struct Row {
    label: String,
    depth: usize,
    target: Target,
}

enum Target {
    Branch {
        key: String,
        count: Option<usize>,
        end: usize,
        bindings: Option<reader_view_query::Bindings>,
    },
    Note(String),
    /// A file that is not a note: it opens in the file preview.
    File(String),
}

pub(super) fn build(
    projection: Result<&Projection, &String>,
    vault: &Vault,
    snapshot: &Snapshot,
) -> Published {
    let layout = vault.folders().layout();
    let mut definitions = snapshot
        .source_paths()
        .filter(|path| vault.folders().view_source(path))
        .filter(|path| {
            layout.is_some()
                || projection
                    .as_ref()
                    .is_ok_and(|projection| projection.note(path).is_some())
        })
        .filter_map(|path| view::parse(path, &snapshot.source(path)?).ok())
        .filter(view::ViewDoc::is_sidebar_tree);
    let Some(definition) = definitions.next() else {
        return Some(build_builtin_tree(layout?, vault).map(Arc::new));
    };
    if let Some(other) = definitions.next() {
        return Some(Err(format!(
            "Multiple views define Areas: {} and {}",
            definition.path(),
            other.path()
        )));
    }
    Some(
        projection
            .map_err(String::clone)
            .and_then(|projection| build_tree(&definition, projection, vault, snapshot))
            .map(Arc::new),
    )
}

fn domain_template(vault: &Vault) -> Result<view::ViewDoc, String> {
    let source = view::builtin::domain_source(&vault.root)?;
    view::parse(view::builtin::DOMAIN_PATH, &source).map_err(|findings| findings[0].message.clone())
}

/// The built-in Areas groups the vault by its declared domains. Layout only
/// groups: it never hides a file. Each domain expands into its folder on
/// disk, and «All files» lists the whole vault as the Tree does.
fn build_builtin_tree(layout: &Layout, vault: &Vault) -> Result<Tree, String> {
    let template = domain_template(vault)?;
    let listing = Listing::new(&vault.entries);
    let mut rows = Vec::new();
    for (declared, domain) in layout
        .domains
        .iter()
        .zip(view::builtin::sidebar_domains(layout, &template))
    {
        let index = rows.len();
        rows.push(Row {
            label: domain.label.clone(),
            depth: 0,
            target: Target::Branch {
                key: domain.key.clone(),
                count: Some(listing.count(&declared.path)),
                end: 0,
                bindings: Some(reader_view_query::Bindings {
                    key: domain.key.clone(),
                    label: domain.label,
                    args: domain.args,
                }),
            },
        });
        listing.append(&mut rows, &declared.path, 1, &domain.key);
        close_branch(&mut rows, index);
    }
    let index = rows.len();
    rows.push(Row {
        label: ALL_FILES.into(),
        depth: 0,
        target: Target::Branch {
            key: ALL_FILES_KEY.into(),
            count: Some(listing.count("")),
            end: 0,
            bindings: None,
        },
    });
    listing.append(&mut rows, "", 1, ALL_FILES_KEY);
    close_branch(&mut rows, index);
    Ok(Tree {
        rows,
        template: Some(template.path().to_owned()),
        notice: None,
    })
}

const ALL_FILES: &str = "All files";
const ALL_FILES_KEY: &str = "builtin-sidebar/@all-files";

fn close_branch(rows: &mut [Row], index: usize) {
    let len = rows.len();
    if let Target::Branch { end, .. } = &mut rows[index].target {
        *end = len;
    }
}

/// The vault's folders and files as the Tree lists them: `@`/`.` items stay
/// out unless they are a folder's index note, folders come first, then names
/// without letter case. Every other inventory entry is listed.
struct Listing {
    children: BTreeMap<String, Vec<(String, EntryKind)>>,
    counts: BTreeMap<String, usize>,
}

impl Listing {
    fn new(entries: &[okilum_core::vault::VaultEntry]) -> Self {
        let shown = |path: &str| {
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
            !okilum_core::vault_conventions::hidden(parent)
                && (!okilum_core::vault_conventions::hidden(name)
                    || okilum_core::vault_conventions::index_name(name))
        };
        let mut kinds = BTreeMap::new();
        let mut counts = BTreeMap::<String, usize>::new();
        for entry in entries.iter().filter(|entry| shown(&entry.path)) {
            kinds.insert(entry.path.clone(), entry.kind);
            // Folders the inventory implies are listed even without an entry.
            let mut folder = entry.path.as_str();
            while let Some((parent, _)) = folder.rsplit_once('/') {
                kinds
                    .entry(parent.to_owned())
                    .or_insert(EntryKind::Directory);
                folder = parent;
            }
            if entry.kind != EntryKind::Directory {
                *counts.entry(String::new()).or_default() += 1;
                let mut folder = entry.path.as_str();
                while let Some((parent, _)) = folder.rsplit_once('/') {
                    *counts.entry(parent.to_owned()).or_default() += 1;
                    folder = parent;
                }
            }
        }
        let mut children = BTreeMap::<String, Vec<(String, EntryKind)>>::new();
        for (path, kind) in kinds {
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            children
                .entry(parent.to_owned())
                .or_default()
                .push((path, kind));
        }
        for list in children.values_mut() {
            list.sort_by_cached_key(|(path, kind)| {
                let label = label_of(path, *kind);
                (
                    !okilum_core::vault_conventions::index_name(path),
                    *kind != EntryKind::Directory,
                    label.to_lowercase(),
                    label,
                    path.clone(),
                )
            });
        }
        Self { children, counts }
    }

    fn count(&self, folder: &str) -> usize {
        self.counts.get(folder).copied().unwrap_or(0)
    }

    fn append(&self, rows: &mut Vec<Row>, folder: &str, depth: usize, group: &str) {
        for (path, kind) in self.children.get(folder).into_iter().flatten() {
            let label = label_of(path, *kind);
            match kind {
                EntryKind::Directory => {
                    let index = rows.len();
                    rows.push(Row {
                        label,
                        depth,
                        target: Target::Branch {
                            key: format!("{group}::{path}"),
                            count: Some(self.count(path)),
                            end: 0,
                            bindings: None,
                        },
                    });
                    self.append(rows, path, depth + 1, group);
                    close_branch(rows, index);
                }
                EntryKind::Markdown => rows.push(Row {
                    label,
                    depth,
                    target: Target::Note(path.clone()),
                }),
                EntryKind::Attachment => rows.push(Row {
                    label,
                    depth,
                    target: Target::File(path.clone()),
                }),
            }
        }
    }
}

fn label_of(path: &str, kind: EntryKind) -> String {
    match kind {
        EntryKind::Markdown => Vault::title_of(path),
        _ => path.rsplit('/').next().unwrap_or(path).to_owned(),
    }
}

fn build_tree(
    definition: &view::ViewDoc,
    projection: &Projection,
    vault: &Vault,
    snapshot: &Snapshot,
) -> Result<Tree, String> {
    let source = |path: &str| snapshot.source(path);
    let mut context = view::RunContext::new(projection, vault, chrono::Local::now().fixed_offset());
    context.read_source = Some(&source);
    let result = view::run(definition, &context);
    let section = &result.sections[0];
    if let Some(finding) = result.findings.iter().find(|finding| finding.is_error()) {
        return Err(finding.message.clone());
    }
    let nodes = section.nodes.as_deref().ok_or_else(|| {
        section.diagnostics.first().map_or_else(
            || "Areas is unavailable".into(),
            |finding| finding.message.clone(),
        )
    })?;
    let template = section
        .resolve_node_view(vault, definition.path())
        .and_then(|path| {
            let path = path.as_deref().unwrap_or(view::builtin::DOMAIN_PATH);
            if path == view::builtin::DOMAIN_PATH {
                return domain_template(vault);
            }
            let text = projection
                .note(path)
                .filter(|_| vault.folders().view_source(path))
                .and_then(|_| source(path))
                .ok_or_else(|| {
                    format!("Node view '{path}' must be in the vault’s views directory")
                })?;
            view::parse(path, &text).map_err(|findings| findings[0].message.clone())
        });
    let mut tree = Tree {
        rows: Vec::new(),
        template: template.as_ref().ok().map(|doc| doc.path().to_owned()),
        notice: template.as_ref().err().cloned().or_else(|| {
            section
                .truncated
                .then(|| "Areas results were truncated".into())
        }),
    };
    append_rows(
        &mut tree.rows,
        nodes,
        section,
        definition.path(),
        template.as_ref().ok(),
        &mut Vec::new(),
    );
    Ok(tree)
}

fn append_rows(
    rows: &mut Vec<Row>,
    nodes: &[view::TreeNode],
    section: &view::Section,
    source: &str,
    template: Option<&view::ViewDoc>,
    branch: &mut Vec<(String, serde_json::Value)>,
) {
    for node in nodes {
        let depth = branch.len();
        branch.push((node.field.clone(), node.value.clone()));
        let key = serde_json::to_string(&(source, &branch)).expect("serializable Areas branch");
        let bindings = template
            .filter(|template| {
                template.path() != view::builtin::DOMAIN_PATH
                    || branch
                        .iter()
                        .any(|(field, value)| field == "area" && !value.is_null())
            })
            .map(|template| reader_view_query::Bindings {
                key: key.clone(),
                label: node.label.clone(),
                args: template.node_args(branch),
            });
        let index = rows.len();
        rows.push(Row {
            label: node.label.clone(),
            depth,
            target: Target::Branch {
                key,
                count: Some(node.count),
                end: 0,
                bindings,
            },
        });
        append_rows(rows, &node.children, section, source, template, branch);
        for &index in &node.rows {
            let row = &section.rows[index];
            rows.push(Row {
                label: row
                    .fields
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| Vault::title_of(&row.path), str::to_owned),
                depth: depth + 1,
                target: Target::Note(row.path.clone()),
            });
        }
        let len = rows.len();
        if let Target::Branch { end, .. } = &mut rows[index].target {
            *end = len;
        }
        branch.pop();
    }
}

impl Reader {
    pub(super) fn areas_tree(&self) -> Option<&Result<Arc<Tree>, String>> {
        self.view_query_index.as_ref()?.areas.as_ref()
    }

    pub(super) fn left_sidebar_mode(&self) -> LeftMode {
        match self.sidebar.left_mode {
            LeftMode::Areas if self.areas_tree().is_none() => LeftMode::Tree,
            mode => mode,
        }
    }

    pub(super) fn render_areas_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tree) = self.areas_tree() else {
            return div().into_any_element();
        };
        let tree = match tree {
            Ok(tree) => tree.clone(),
            Err(message) => return status(message.clone(), cx).into_any_element(),
        };
        if tree.rows.is_empty() {
            let message = tree.notice.clone().unwrap_or_else(|| "No results".into());
            return status(message, cx).into_any_element();
        }
        let mut visible = Vec::new();
        let mut index = 0;
        while let Some(row) = tree.rows.get(index) {
            if let Target::Branch { key, end, .. } = &row.target {
                let expanded = self.sidebar.areas_expanded.contains(key);
                visible.push((index, expanded));
                index = if expanded { index + 1 } else { *end };
            } else {
                visible.push((index, false));
                index += 1;
            }
        }
        let current = self.selected_file().to_owned();
        let selected = self
            .pane
            .navigation
            .view_bindings()
            .map(|view| view.key.clone());
        let entity = cx.entity().downgrade();
        let epoch = self.view_query_generation;
        let notice = tree.notice.clone();
        let list = uniform_list("sidebar-areas", visible.len(), move |range, _, cx| {
            range
                .map(|slot| {
                    let position = visible[slot];
                    let row = &tree.rows[position.0];
                    let is_current = match &row.target {
                        Target::Branch { key, .. } => selected.as_ref() == Some(key),
                        Target::Note(path) | Target::File(path) => {
                            selected.is_none() && current == *path
                        }
                    };
                    area_row(
                        row,
                        position,
                        is_current,
                        tree.template.as_deref(),
                        &entity,
                        epoch,
                        cx,
                    )
                })
                .collect::<Vec<_>>()
        });
        v_flex()
            .size_full()
            .px_2()
            .pb_2()
            .overflow_hidden()
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .children(notice.map(|message| status(message, cx)))
            .child(list.flex_1().min_h_0().w_full())
            .into_any_element()
    }
}

fn status(message: String, cx: &App) -> Div {
    div()
        .px_2()
        .py_1()
        .text_color(brand::palette(cx).text_muted)
        .child(message)
}

fn area_row(
    row: &Row,
    (index, expanded): (usize, bool),
    selected: bool,
    template: Option<&str>,
    reader: &WeakEntity<Reader>,
    epoch: u64,
    cx: &App,
) -> AnyElement {
    let colors = brand::palette(cx);
    let tokens = brand::reader_palette(cx);
    let element = reader_sidebar_view::tree_row_base(format!("areas-row-{index}").into())
        .pl(px(4. + row.depth as f32 * 14.))
        .cursor_pointer()
        .when(selected, |row| {
            row.bg(colors.selected).font_weight(FontWeight::MEDIUM)
        })
        .when(!selected, |row| row.hover(move |row| row.bg(tokens.hover)));
    let label = div()
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .child(row.label.clone());
    match &row.target {
        Target::Branch {
            key,
            count,
            bindings,
            ..
        } => {
            let toggle = reader.clone();
            let open = reader.clone();
            let toggle_key = key.clone();
            let key = key.clone();
            let view = template.map(str::to_owned).zip(bindings.clone());
            element
                .when(row.depth == 0, |row| row.font_weight(FontWeight::SEMIBOLD))
                .when(count.is_some(), |row| {
                    row.child(
                        div()
                            .id("areas-disclosure")
                            .flex_none()
                            .child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .xsmall()
                                .text_color(tokens.text_faint),
                            )
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                with_areas(&toggle, epoch, cx, |reader, cx| {
                                    toggle_branch(reader, &toggle_key, cx)
                                });
                            }),
                    )
                })
                .when(row.depth > 0, |row| {
                    row.child(
                        Icon::new(if expanded {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        })
                        .small()
                        .text_color(colors.text_muted),
                    )
                })
                .child(label)
                .when_some(*count, |row, count| {
                    row.child(div().text_color(colors.text_muted).child(count.to_string()))
                })
                .on_click(move |_, window, cx| {
                    with_areas(&open, epoch, cx, |reader, cx| {
                        if let Some((path, bindings)) = &view {
                            reader.open_note_landing(
                                path,
                                None,
                                None,
                                Some(prepared_links::Landing::View(bindings.clone())),
                                window,
                                cx,
                            );
                        } else {
                            toggle_branch(reader, &key, cx);
                        }
                    })
                })
                .into_any_element()
        }
        Target::Note(path) | Target::File(path) => {
            let note = matches!(row.target, Target::Note(_));
            let open = reader.clone();
            let reveal = reader.clone();
            let open_path = path.clone();
            let reveal_path = path.clone();
            element
                .child(div().w(px(14.)).flex_none())
                .child(
                    Icon::new(if note {
                        IconName::FileText
                    } else {
                        IconName::File
                    })
                    .small()
                    .text_color(colors.text_muted),
                )
                .child(label)
                .on_click(move |_, window, cx| {
                    with_areas(&open, epoch, cx, |reader, cx| {
                        if note {
                            reader.open_note(&open_path, None, window, cx);
                        } else {
                            reader.preview_file(&open_path, window, cx);
                        }
                    })
                })
                .context_menu(move |menu, _, _| {
                    let reveal = reveal.clone();
                    let path = reveal_path.clone();
                    menu.item(PopupMenuItem::new("Reveal in Tree").on_click(
                        move |_, window, cx| {
                            with_areas(&reveal, epoch, cx, |reader, cx| {
                                reader.reveal_in_tree(&path, window, cx)
                            });
                        },
                    ))
                })
                .into_any_element()
        }
    }
}

fn with_areas(
    reader: &WeakEntity<Reader>,
    epoch: u64,
    cx: &mut App,
    action: impl FnOnce(&mut Reader, &mut Context<Reader>),
) {
    let _ = reader.update(cx, |reader, cx| {
        if reader.view_query_generation == epoch && reader.left_sidebar_mode() == LeftMode::Areas {
            action(reader, cx);
        }
    });
}

fn toggle_branch(reader: &mut Reader, key: &str, cx: &mut Context<Reader>) {
    if !reader.sidebar.areas_expanded.remove(key) {
        reader.sidebar.areas_expanded.insert(key.to_owned());
    }
    reader.save_sidebar(cx);
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_core::vault::VaultEntry;

    fn entry(path: &str, kind: EntryKind) -> VaultEntry {
        VaultEntry {
            path: path.into(),
            kind,
        }
    }

    fn listed(listing: &Listing, folder: &str) -> Vec<(String, usize)> {
        let mut rows = Vec::new();
        listing.append(&mut rows, folder, 1, "group");
        rows.into_iter()
            .map(|row| {
                let path = match row.target {
                    Target::Branch { key, .. } => key.trim_start_matches("group::").to_owned(),
                    Target::Note(path) | Target::File(path) => path,
                };
                (path, row.depth)
            })
            .collect()
    }

    /// Every file the Tree shows by default is reachable under All files,
    /// including folders the inventory only implies; a domain lists its own
    /// folder. `@`/`.` items stay out, as in the Tree, except index notes.
    #[test]
    fn layout_groups_but_every_file_stays_listed() {
        let entries = [
            entry("inbox", EntryKind::Directory),
            entry("inbox/Capture.md", EntryKind::Markdown),
            entry("state", EntryKind::Directory),
            entry("state/dev", EntryKind::Directory),
            entry("state/dev/@index.md", EntryKind::Markdown),
            entry("state/dev/AGENTS.md", EntryKind::Markdown),
            entry("state/dev/okilum/plan.md", EntryKind::Markdown),
            entry("sources/files/diagram.png", EntryKind::Attachment),
            entry("@views/questions.md", EntryKind::Markdown),
            entry("Root note.md", EntryKind::Markdown),
        ];
        let listing = Listing::new(&entries);
        let all: Vec<_> = listed(&listing, "");
        let paths: Vec<_> = all.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "inbox",
                "inbox/Capture.md",
                "sources",
                "sources/files",
                "sources/files/diagram.png",
                "state",
                "state/dev",
                "state/dev/@index.md",
                "state/dev/okilum",
                "state/dev/okilum/plan.md",
                "state/dev/AGENTS.md",
                "Root note.md",
            ]
        );
        assert_eq!(listing.count(""), 6, "all shown files");
        assert_eq!(listing.count("state/dev"), 3);
        assert_eq!(
            listed(&listing, "state/dev"),
            [
                ("state/dev/@index.md".into(), 1),
                ("state/dev/okilum".into(), 1),
                ("state/dev/okilum/plan.md".into(), 2),
                ("state/dev/AGENTS.md".into(), 1),
            ]
        );
        assert!(
            listed(&listing, "state/sap").is_empty(),
            "missing domain folder"
        );
    }
}
