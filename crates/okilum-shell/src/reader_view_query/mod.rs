//! Read-only query results presented through the Reader's Markdown typography.
use super::*;
use okilum_core::document_links::HeadingEntry;
use okilum_core::view;
use std::collections::BTreeMap;

mod index;
mod markdown;

pub(super) use index::Index;
pub(super) use index::{load as load_index, update as update_index};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Bindings {
    pub key: String,
    pub label: String,
    pub args: BTreeMap<String, serde_json::Value>,
}

#[derive(Default)]
pub(super) struct Navigation {
    pub active: std::cell::RefCell<Option<Content>>,
    pub pending: std::cell::Cell<bool>,
}

#[derive(Clone)]
pub(super) struct Content {
    pub text: Entity<TextViewState>,
    headings: Arc<[HeadingEntry]>,
}

impl Content {
    /// Source offsets still belong to the canonical note. Resolve its heading
    /// against the generated document before scrolling the visible list.
    pub(super) fn landing(
        &self,
        source: &[HeadingEntry],
        position: ListOffset,
    ) -> Result<ListOffset, &'static str> {
        if position.item_ix == 0 {
            return Ok(position);
        }
        let Some(heading) = source.iter().find(|h| h.target.block == position.item_ix) else {
            return Err("This source position has no heading in the query view.");
        };
        let mut matches = self
            .headings
            .iter()
            .filter(|entry| entry.text == heading.text && entry.level == heading.level);
        let first = matches.next();
        match (first, matches.next()) {
            (Some(target), None) => Ok(ListOffset {
                item_ix: target.target.block,
                ..position
            }),
            _ => Err("This heading has no unique match in the query view."),
        }
    }
}

impl Reader {
    pub(super) fn publish_view_query(&mut self, index: Index, cx: &App) {
        let retired = self.view_query_index.replace(index);
        self.view_query_generation = self.view_query_generation.wrapping_add(1);
        cx.background_executor()
            .spawn(async move {
                drop(retired);
            })
            .detach();
    }

    pub(super) fn visible_markdown(&self) -> Entity<TextViewState> {
        self.pane
            .query_navigation
            .active
            .borrow()
            .as_ref()
            .map(|content| content.text.clone())
            .unwrap_or_else(|| self.pane.content.clone())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Request {
    epoch: u64,
    source: Arc<str>,
    bindings: Option<Bindings>,
}

#[derive(Default)]
struct Cache {
    request: Option<Request>,
    fallback: bool,
    content: Option<Content>,
    task: Option<Task<()>>,
}

fn evaluate(
    path: &str,
    request: &Request,
    index: &Index,
    vault: &Vault,
    searcher: Option<&Searcher>,
) -> Result<String, String> {
    let doc = view::parse(path, &request.source).map_err(|findings| {
        findings
            .into_iter()
            .map(|f| f.message)
            .collect::<Vec<_>>()
            .join(" ")
    })?;
    let projection = match &index.projection {
        Ok(projection) => projection,
        Err(error) => return Ok(markdown::unavailable(error)),
    };
    let mut run = view::RunContext::new(projection, vault, chrono::Local::now().fixed_offset());
    run.generation = request.epoch;
    run.searcher = searcher;
    if let Some(bindings) = &request.bindings {
        run.args = bindings.args.clone();
    }
    Ok(markdown::render(&view::run(&doc, &run)))
}

pub(super) fn render(
    reader: &Reader,
    source: Arc<str>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Option<AnyElement> {
    let path = reader.selected_file();
    if reader.single_file || !path.starts_with("@views/") || path.starts_with("@views/out/") {
        return None;
    }
    let cache = window.use_keyed_state(
        SharedString::from(format!(
            "query-view:{:?}:{}",
            reader.pane.id(),
            reader.vault_root.join(path).display()
        )),
        cx,
        |_, _| Cache::default(),
    );
    let request = Request {
        epoch: reader.view_query_generation,
        source,
        bindings: reader.pane.navigation.view_bindings().cloned(),
    };
    let Some(index) = &reader.view_query_index else {
        return Some(loading(reader, cx));
    };
    if cache.read(cx).request.as_ref() != Some(&request) {
        let index = index.clone();
        let vault = reader.vault.clone();
        let searcher = reader.searcher.clone();
        let lease = reader.cache_lease.clone();
        let path = path.to_owned();
        let input_path = path.clone();
        let pane = reader.pane.id();
        cache.update(cx, |cache, _| {
            if cache.request.as_ref().is_none_or(|previous| {
                previous.source != request.source || previous.bindings != request.bindings
            }) {
                cache.content = None;
                cache.fallback = false;
            }
            cache.request = Some(request.clone());
        });
        let input = request.clone();
        let worker = cx.background_executor().spawn(async move {
            let _lease = lease;
            evaluate(&input_path, &input, &index, &vault, searcher.as_deref())
                .map(|markdown| (note_outline(&markdown), markdown))
        });
        let target = cache.downgrade();
        let task = cx.spawn_in(window, async move |reader, cx| {
            let output = worker.await;
            let _ = reader.update_in_own_window(cx, |reader, window, cx| {
                let visible = reader.pane.id() == pane && reader.selected_file() == path;
                let _ = target.update(cx, |cache, cx| {
                    if cache.request.as_ref() != Some(&request) {
                        return;
                    }
                    match output {
                        Ok((headings, markdown)) => {
                            cache.fallback = false;
                            let content = cache.content.get_or_insert_with(|| Content {
                                text: cx.new(|cx| {
                                    TextViewState::markdown("", cx)
                                        .scrollable(true)
                                        .selectable(true)
                                        .retain_selection_on_layout(true)
                                }),
                                headings: Arc::default(),
                            });
                            content.headings = headings.into();
                            content
                                .text
                                .update(cx, |state, cx| state.set_text(&markdown, cx));
                        }
                        Err(reason) => {
                            if !cache.fallback && visible {
                                reader_toast::transient(
                                    format!("{reason} Showing Markdown."),
                                    window,
                                    cx,
                                );
                            }
                            cache.fallback = true;
                            cache.content = None;
                        }
                    }
                });
                cx.notify();
            });
        });
        cache.update(cx, |cache, _| cache.task = Some(task));
    }

    let cached = cache.read(cx);
    if cached.fallback {
        return None;
    }
    let Some(content) = cached.content.clone() else {
        return Some(loading(reader, cx));
    };
    if reader
        .pane
        .content
        .read(cx)
        .focus_handle()
        .is_focused(window)
    {
        let focus = content.text.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
    }
    *reader.pane.query_navigation.active.borrow_mut() = Some(content.clone());
    let mut style = reader_text_zoom::document_style(cx);
    style.heading_base_font_size = px(reader_ui_state::font_size(cx));
    style.top_padding = px(44.);
    style.bottom_padding = reader_toast::bottom_space(window, cx);
    Some(
        h_flex()
            .debug_selector(|| "native-query-view".into())
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .justify_center()
            .child(
                TextView::new(&content.text)
                    .style(style)
                    .font_family(reader_fonts::note(cx))
                    .text_size(px(reader_ui_state::font_size(cx)))
                    .scrollable(true)
                    .selectable(true)
                    .selection_format(reader.sel_format)
                    .on_link_click(|_, _, _, _| {})
                    .pl(reader_window_view::note_gutter(window))
                    .pr(px(READER_SIDE_PADDING))
                    .w_full()
                    .h_full()
                    .max_w(px(reader_ui_state::note_column_width(cx))),
            )
            .into_any_element(),
    )
}

fn loading(reader: &Reader, cx: &App) -> AnyElement {
    reader.pane.query_navigation.pending.set(true);
    div()
        .px_6()
        .pt_4()
        .text_color(cx.theme().muted_foreground)
        .child("Loading view…")
        .into_any_element()
}
