//! Virtual sidebar from a vault-authored tree or the built-in domain list.
use super::*;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use okilum_core::{records::Projection, vault::warm::Snapshot, vault_descriptor::Layout, view};
use reader_sidebar::LeftMode;

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

fn build_builtin_tree(layout: &Layout, vault: &Vault) -> Result<Tree, String> {
    let template = domain_template(vault)?;
    let rows = view::builtin::sidebar_domains(layout, &template)
        .enumerate()
        .map(|(index, domain)| Row {
            label: domain.label.clone(),
            depth: 0,
            target: Target::Branch {
                key: domain.key.clone(),
                count: None,
                end: index + 1,
                bindings: Some(reader_view_query::Bindings {
                    key: domain.key,
                    label: domain.label,
                    args: domain.args,
                }),
            },
        })
        .collect();
    Ok(Tree {
        rows,
        template: Some(template.path().to_owned()),
        notice: None,
    })
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
                        Target::Note(path) => selected.is_none() && current == *path,
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
        Target::Note(path) => {
            let open = reader.clone();
            let reveal = reader.clone();
            let open_path = path.clone();
            let reveal_path = path.clone();
            element
                .child(div().w(px(14.)).flex_none())
                .child(
                    Icon::new(IconName::FileText)
                        .small()
                        .text_color(colors.text_muted),
                )
                .child(label)
                .on_click(move |_, window, cx| {
                    with_areas(&open, epoch, cx, |reader, cx| {
                        reader.open_note(&open_path, None, window, cx);
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
