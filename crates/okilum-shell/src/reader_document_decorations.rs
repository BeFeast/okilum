//! Read-only document decorations shared by Reader, imported documents and
//! Live Preview. Canonical source and its editor projection stay independent.
use super::*;
use okilum_core::agent_imports::Expansion;
use reader_agent_imports::ImportLink;
use std::collections::BTreeMap;

#[derive(Clone)]
struct Directive {
    target: String,
    occurrence: usize,
}
#[derive(Clone)]
struct DecoratedParagraph {
    directives: Vec<Directive>,
}

/// Install once on each document surface. Imported documents override the
/// context with their own canonical path; decorations never inherit the host's
/// relative-link origin. This hook also leaves ordinary Markdown parts intact.
pub(super) fn install(
    view: TextView,
    reader: WeakEntity<Reader>,
    source: Option<PathBuf>,
) -> TextView {
    view.markdown_block_parser(parse).markdown_block_renderer(
        "agent-imports",
        move |node, window, cx| {
            let Some(data) = node.data::<DecoratedParagraph>() else {
                return div().into_any_element();
            };
            let entity = reader.upgrade();
            let this = entity.as_ref().map(|entity| entity.read(cx));
            let current = this.and_then(|this| this.agent_document.as_ref());
            let origin = current.map(|current| {
                source
                    .clone()
                    .unwrap_or_else(|| current.root.join(&current.path))
            });
            let mut column = v_flex().w_full();
            for (index, directive) in data.directives.iter().enumerate() {
                column =
                    column.child(node.render_part(&format!("part-{index}"), |s| s, window, cx));
                if let Some((this, link)) = this.zip(
                    current
                        .zip(origin.as_ref())
                        .and_then(|(current, origin)| find_link(current, origin, directive)),
                ) {
                    column = column.child(render_line(this, link, reader.clone(), cx));
                } else {
                    column = column.child(format!("@{}", directive.target));
                }
            }
            column
                .child(node.render_part(
                    &format!("part-{}", data.directives.len()),
                    |s| s,
                    window,
                    cx,
                ))
                .into_any_element()
        },
    )
}

fn parse(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Paragraph(_) = node else {
        return None;
    };
    let source = cx.node_source(node)?;
    let base = node.position()?.start.offset;
    // The whole fragment proves fence/indent exclusion; parsing only the
    // paragraph could turn an indented or nested code example into a directive.
    let imports: Vec<_> = okilum_core::agent_imports::parse(cx.source())
        .into_iter()
        .filter(|import| base <= import.line.start && import.line.end <= base + source.len())
        .collect();
    if imports.is_empty() {
        return None;
    }
    let all = okilum_core::agent_imports::parse(cx.source());
    let directives = imports
        .iter()
        .map(|import| Directive {
            target: import.target.clone(),
            occurrence: all
                .iter()
                .take_while(|other| other.line.start < import.line.start)
                .filter(|other| other.target == import.target)
                .count(),
        })
        .collect();
    let mut result = MarkdownNode::new("agent-imports", DecoratedParagraph { directives })
        .text(source.to_owned())
        .markdown(source.to_owned());
    let mut cursor = 0;
    for (index, import) in imports.iter().enumerate() {
        let start = import.line.start - base;
        let end = import.line.end - base;
        result = result.markdown_part(format!("part-{index}"), source[cursor..start].to_owned());
        cursor = end;
    }
    Some(result.markdown_part(
        format!("part-{}", imports.len()),
        source[cursor..].to_owned(),
    ))
}

fn nodes_for<'a>(nodes: &'a [Expansion], source: &Path) -> Option<&'a [Expansion]> {
    if nodes.first().is_some_and(|node| node.source == source) {
        return Some(nodes);
    }
    nodes
        .iter()
        .filter_map(|node| node.result.as_ref().ok())
        .find_map(|imported| nodes_for(&imported.children, source))
}
fn find_link(current: &AgentDocument, source: &Path, directive: &Directive) -> Option<ImportLink> {
    if source == current.root.join(&current.path) {
        current
            .imports
            .links()
            .iter()
            .filter(|link| link.import.target == directive.target)
            .nth(directive.occurrence)
            .cloned()
    } else {
        let node = nodes_for(current.imports.tree(), source)?
            .iter()
            .filter(|node| node.import.target == directive.target)
            .nth(directive.occurrence)?;
        Some(ImportLink {
            source: node.source.clone(),
            import: node.import.clone(),
            destination: node
                .result
                .as_ref()
                .map(|imported| imported.path.clone())
                .map_err(Clone::clone),
        })
    }
}
fn expansion<'a>(nodes: &'a [Expansion], link: &ImportLink) -> Option<&'a Expansion> {
    nodes_for(nodes, &link.source)?
        .iter()
        .find(|node| node.import == link.import)
}

pub(super) fn prepare_imported_documents(
    vault: &Vault,
    root: &Path,
    nodes: &[Expansion],
) -> BTreeMap<PathBuf, SharedString> {
    let mut documents = BTreeMap::new();
    fn visit(
        vault: &Vault,
        root: &Path,
        nodes: &[Expansion],
        documents: &mut BTreeMap<PathBuf, SharedString>,
    ) {
        for imported in nodes.iter().filter_map(|node| node.result.as_ref().ok()) {
            if let Ok(path) = imported.path.strip_prefix(root) {
                let rel = okilum_core::vault::note_path(path);
                documents.entry(imported.path.clone()).or_insert_with(|| {
                    okilum_core::render::reader_document_from_source(vault, &rel, &imported.text)
                        .rendered
                        .into()
                });
                visit(vault, root, &imported.children, documents);
            }
        }
    }
    visit(vault, root, nodes, &mut documents);
    documents
}

pub(super) fn render_line(
    this: &Reader,
    link: ImportLink,
    reader: WeakEntity<Reader>,
    cx: &App,
) -> AnyElement {
    let Some(current) = &this.agent_document else {
        return div()
            .child(format!("@{}", link.import.target))
            .into_any_element();
    };
    let key = (link.source.clone(), link.import.line.start);
    let expanded = current.expand_all ^ current.expanded_lines.contains(&key);
    let token = current.token.clone();
    let open_token = token.clone();
    let open_reader = reader.clone();
    let open_link = link.clone();
    let toggle_reader = reader.clone();
    let id = format!("{}:{}", link.source.display(), link.import.line.start);
    let selector = format!("agent-line-{}", link.import.target);
    let toggle_selector = format!("agent-disclosure-{}", link.import.target);
    let mut column = v_flex().w_full().child(
        h_flex()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!("import-fold-{id}")))
                    .ghost()
                    .xsmall()
                    .icon(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .tooltip(if expanded {
                        "Collapse import"
                    } else {
                        "Expand import (read-only)"
                    })
                    .debug_selector(move || toggle_selector.clone())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = toggle_reader.update(cx, |this, cx| {
                            this.sync_agent_document(cx);
                            let Some(current) = &mut this.agent_document else {
                                return;
                            };
                            if !Arc::ptr_eq(&token, &current.token) {
                                return;
                            }
                            if !current.expanded_lines.remove(&key) {
                                current.expanded_lines.insert(key.clone());
                            }
                            if !current.imports.expanded() {
                                current.imports.set_expanded(true);
                                this.prepare_agent_document(cx);
                            }
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("import-open-{id}")))
                    .debug_selector(move || selector.clone())
                    .cursor_pointer()
                    .text_color(brand::palette(cx).link)
                    .child(format!("@{}", link.import.target))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = open_reader.update(cx, |this, cx| {
                            this.open_agent_import(
                                open_token.clone(),
                                open_link.clone(),
                                window,
                                cx,
                            )
                        });
                    }),
            ),
    );
    if let Err(error) = &link.destination {
        column = column.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{error:?}")),
        );
    }
    if expanded {
        let body = match expansion(current.imports.tree(), &link).map(|node| &node.result) {
            Some(Ok(imported)) => match current.imported_documents.get(&imported.path) {
                Some(markdown) => {
                    let view = reader_plugins(
                        this.vault_root.clone(),
                        TextView::markdown(
                            SharedString::from(format!("import-body-{id}")),
                            markdown.clone(),
                        ),
                        reader.clone(),
                        SelectionFormat::default(),
                        this.link_presentations.clone(),
                        &this.link_identities,
                    );
                    install(view, reader, Some(imported.path.clone()))
                        .style(reader_text_zoom::document_style(cx))
                        .font_family(reader_fonts::note(cx))
                        .text_size(px(reader_ui_state::font_size(cx)))
                        .w_full()
                        .into_any_element()
                }
                None => div()
                    .child("Preparing imported document…")
                    .into_any_element(),
            },
            Some(Err(error)) => div()
                .child(format!("Import unavailable: {error:?}"))
                .into_any_element(),
            None => div().child("Loading import…").into_any_element(),
        };
        column = column.child(
            div()
                .debug_selector({
                    let target = link.import.target.clone();
                    move || format!("agent-expanded-{target}")
                })
                .ml_3()
                .pl_3()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(body),
        );
    }
    column.into_any_element()
}

impl Reader {
    pub(super) fn agent_document_enabled(&self, cx: &App) -> bool {
        reader_features::enabled(reader_features::Feature::Agents, cx)
            && !self.single_file
            && okilum_core::agent_imports::is_agent_document(&self.current_rel)
    }

    pub(super) fn open_agent_import(
        &mut self,
        token: Arc<()>,
        link: ImportLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_agent_document(cx);
        let Some(current) = &self.agent_document else {
            return;
        };
        if !Arc::ptr_eq(&token, &current.token)
            || current.imports.state() != &reader_agent_imports::ViewState::Ready
        {
            return;
        }
        let root = current.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read_root = root.clone();
            let target = cx
                .background_executor()
                .spawn(async move {
                    let store = okilum_core::agent_imports::RootedSource::new(&read_root)?;
                    let resolved = okilum_core::agent_imports::resolve(
                        &store,
                        store.root(),
                        &link.source,
                        &link.import.target,
                    );
                    if resolved != link.destination {
                        return Err(okilum_core::agent_imports::Failure::Changed);
                    }
                    resolved
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sync_agent_document(cx);
                let Some(current) = &mut this.agent_document else {
                    return;
                };
                if !Arc::ptr_eq(&token, &current.token) {
                    return;
                }
                match target.and_then(|path| {
                    path.strip_prefix(&root)
                        .map(Path::to_path_buf)
                        .map_err(|_| okilum_core::agent_imports::Failure::OutsideRoot)
                }) {
                    Ok(path) => this.open_note_landing(
                        &okilum_core::vault::note_path(&path),
                        None,
                        None,
                        None,
                        window,
                        cx,
                    ),
                    Err(error) => {
                        current.notice = Some(format!("Import changed or unavailable: {error:?}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
