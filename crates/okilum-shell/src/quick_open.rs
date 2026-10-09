//! Search palette. At most one query runs; intervening edits are coalesced.
use super::*;
use okilum_core::search_snippet::{plain_snippet, PlainSnippet};
use okilum_core::SearchHit;

use std::collections::HashMap;

fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// Folder notes (`_index`, `index`, `README`) are titled by their folder.
fn is_folder_note(path: &str) -> bool {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    is_markdown(path)
        && ["_index", "index", "readme"]
            .iter()
            .any(|name| stem.eq_ignore_ascii_case(name))
}

/// Excalidraw's compound extension identifies the format, not the drawing name.
/// Keep canonical paths intact; use this only for display labels.
pub(super) fn drawing_title(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    [".excalidraw.md", ".excalidraw"]
        .into_iter()
        .find(|suffix| lower.ends_with(suffix))
        .map(|suffix| name[..name.len() - suffix.len()].to_owned())
}

/// Folder title without filesystem reads, shared by search results and Inbox.
pub(super) fn folder_note_title(vault_name: &str, path: &str) -> Option<String> {
    if !is_folder_note(path) {
        return None;
    }
    Some(
        Path::new(path)
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or(vault_name)
            .to_owned(),
    )
}

/// Primary line (#645): the note title the backlinks panel uses (first H1,
/// then frontmatter title, then file name), never the `.md` file name. Other
/// files keep their extension. Ranking still uses the file name and path too.
/// Called on the query worker, never during rendering.
fn result_title(root: &Path, titles: &HashMap<String, String>, hit: &SearchHit) -> String {
    let path = root.join(&hit.path);
    if let Some(title) = drawing_title(&hit.path) {
        return title;
    }
    if !is_markdown(&hit.path) {
        return hit.path.rsplit('/').next().unwrap_or(&hit.path).to_owned();
    }
    let vault_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Vault");
    if let Some(folder) = folder_note_title(vault_name, &hit.path) {
        return folder;
    }
    titles
        .get(&hit.path)
        .cloned()
        .or_else(|| display_title(&path))
        .unwrap_or_else(|| hit.title.clone())
}

/// Muted second line (#645): the containing folder, not the file path. A
/// folder note is already titled by its folder, so it shows that folder's
/// parent. Notes at the top level show the vault's name.
pub(super) fn result_location(vault_name: &str, path: &str) -> String {
    let parent = |path: &str| {
        path.rsplit_once('/')
            .map_or("", |(folder, _)| folder)
            .to_owned()
    };
    let mut folder = parent(path);
    if is_folder_note(path) {
        folder = parent(&folder);
    }
    if folder.is_empty() {
        vault_name.to_owned()
    } else {
        folder.replace('/', " / ")
    }
}

/// The folder a content-search result lives in, as breadcrumb-style text
/// (#654). Empty for a note at the vault root.
fn result_folder(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.split('/').collect::<Vec<_>>().join(" › "))
        .unwrap_or_default()
}

/// Literal whole-word highlights for displayed labels. Keep complex search
/// syntax out of this presentation-only fallback; content marks come from Tantivy.
fn label_matches(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let query = query
        .strip_prefix("path:")
        .or_else(|| query.strip_prefix("title:"))
        .unwrap_or(query);
    if query.contains([':', '"', '(', ')', '[', ']', '{', '}', '^', '~', '*', '\\']) {
        return vec![];
    }
    if query
        .split_whitespace()
        .any(|term| matches!(term, "AND" | "OR" | "NOT") || term.starts_with(['-', '+']))
    {
        return vec![];
    }
    let terms: Vec<_> = query
        .split_whitespace()
        .filter(|term| !matches!(*term, "AND" | "OR" | "NOT") && !term.starts_with(['-', '+']))
        .map(|term| {
            term.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|term| !term.is_empty())
        .collect();
    let mut ranges = Vec::new();
    let mut start = None;
    for (at, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if ch.is_alphanumeric() {
            start.get_or_insert(at);
        } else if let Some(from) = start.take() {
            if terms.contains(&text[from..at].to_lowercase()) {
                ranges.push(from..at);
            }
        }
    }
    ranges
}

/// The title already has its own row. Remove only an exact leading repeat
/// from the display snippet; the original hit still drives jump-to-match.
fn without_repeated_title(mut snippet: PlainSnippet, title: &str) -> PlainSnippet {
    if title.is_empty() {
        return snippet;
    }
    let Some(rest) = snippet.text.strip_prefix(title) else {
        return snippet;
    };
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return snippet;
    }
    let start = snippet.text.len() - rest.trim_start().len();
    snippet.text.drain(..start);
    snippet.highlights = snippet
        .highlights
        .into_iter()
        .filter_map(|range| {
            (range.end > start).then(|| range.start.saturating_sub(start)..range.end - start)
        })
        .collect();
    snippet
}

fn result_snippet(
    root: &Path,
    titles: &HashMap<String, String>,
    vault: &Vault,
    hit: &SearchHit,
) -> PlainSnippet {
    let mut snippet = hit
        .display_snippet
        .clone()
        .unwrap_or_else(|| plain_snippet(&hit.snippet_html));
    if let Some(reason) = &mut snippet.hidden_match {
        reason.resolve_link_labels(|target, wiki| {
            let resolution = if wiki {
                vault.resolve_from(target, &hit.path)
            } else {
                vault.resolve_markdown(target, &hit.path)
            };
            match resolution {
                okilum_core::vault::Resolution::Resolved { path } => {
                    let vault_name = root
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Vault");
                    folder_note_title(vault_name, &path)
                        .or_else(|| titles.get(&path).cloned())
                        .unwrap_or_else(|| Vault::title_of(&path))
                }
                other => {
                    let name = target
                        .split(['#', '^'])
                        .next()
                        .unwrap_or(target)
                        .rsplit('/')
                        .next()
                        .unwrap_or(target)
                        .trim_end_matches(".md");
                    format!(
                        "{} · {name}",
                        if matches!(other, okilum_core::vault::Resolution::Ambiguous { .. }) {
                            "Ambiguous"
                        } else {
                            "Missing"
                        }
                    )
                }
            }
        });
    }
    without_repeated_title(snippet, &hit.title)
}

/// A one-line preview must reach the match before the trailing ellipsis.
/// Retain a little context and remap byte ranges on Unicode boundaries.
fn visible_snippet(snippet: &PlainSnippet) -> PlainSnippet {
    let Some(first) = snippet.highlights.first() else {
        return snippet.clone();
    };
    let start = snippet.text[..first.start]
        .char_indices()
        .rev()
        .nth(28)
        .map_or(0, |(at, _)| at);
    if start == 0 {
        return snippet.clone();
    }
    let start = snippet.text[start..first.start]
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map_or(start, |(at, ch)| start + at + ch.len_utf8());
    let prefix = "…";
    PlainSnippet {
        text: format!("{prefix}{}", &snippet.text[start..]),
        highlights: snippet
            .highlights
            .iter()
            .map(|r| r.start - start + prefix.len()..r.end - start + prefix.len())
            .collect(),
        hidden_match: snippet.hidden_match.clone(),
        property_match: snippet.property_match.clone(),
    }
}

/// Keep the reason's human prefix while bringing a distant value match into view.
fn visible_context(
    reason: &okilum_core::search_snippet::MatchContext,
) -> okilum_core::search_snippet::MatchContext {
    use unicode_segmentation::UnicodeSegmentation;
    let Some(first) = reason.highlights.first() else {
        return reason.clone();
    };
    let prefix = reason.text.find(": ").map_or(0, |at| at + 2);
    if first.start <= prefix {
        return reason.clone();
    }
    let start = reason.text[prefix..first.start]
        .grapheme_indices(true)
        .rev()
        .nth(28)
        .map_or(prefix, |(at, _)| prefix + at);
    if start == prefix {
        return reason.clone();
    }
    let lead = prefix + '…'.len_utf8();
    okilum_core::search_snippet::MatchContext {
        text: format!("{}…{}", &reason.text[..prefix], &reason.text[start..]),
        highlights: reason
            .highlights
            .iter()
            .filter_map(|r| {
                if r.end <= prefix {
                    Some(r.clone())
                } else if r.end > start {
                    Some(r.start.max(start) - start + lead..r.end - start + lead)
                } else {
                    None
                }
            })
            .collect(),
        ..Default::default()
    }
}

pub(super) struct Palette {
    pub open: bool,
    full_text: bool,
    pub(super) input: Entity<InputState>,
    pub inventory: Option<Arc<Vec<okilum_core::vault::Note>>>,
    _subscription: Subscription,
    pub(super) rows: Vec<SearchHit>,
    /// Plain-text snippet per row of a content search, same order as `rows`.
    snippets: Vec<PlainSnippet>,
    selected: usize,
    generation: u64,
    running: bool,
    pending: bool,
    message: String,
    pub recent: Vec<String>,
    scroll: ScrollHandle,
    #[cfg(test)]
    hold_query: Option<async_channel::Receiver<()>>,
}

impl Palette {
    pub fn new(window: &mut Window, cx: &mut Context<Reader>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search names and paths…"));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::Change => this.refresh_quick_open(cx),
                InputEvent::PressEnter { .. } => this.accept_quick_open(window, cx),
                _ => {}
            });
        Self {
            open: false,
            full_text: false,
            input,
            inventory: None,
            _subscription: subscription,
            rows: Vec::new(),
            snippets: Vec::new(),
            selected: 0,
            generation: 0,
            running: false,
            pending: false,
            message: String::new(),
            recent: Vec::new(),
            scroll: ScrollHandle::new(),
            #[cfg(test)]
            hold_query: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.rows.clear();
        self.snippets.clear();
        self.selected = 0;
        self.pending = false;
    }

    pub fn remember(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.push(path.to_string());
        if self.recent.len() > 100 {
            self.recent.remove(0);
        }
    }
}

impl Reader {
    pub(super) fn open_quick_open(
        &mut self,
        full_text: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find_open {
            self.close_find(window, cx);
        }
        self.shortcut_sheet.open = false;
        self.quick_open.open = true;
        self.quick_open.full_text = full_text;
        self.quick_open.input.update(cx, |input, cx| {
            input.set_placeholder(
                if full_text {
                    "Search note contents…"
                } else {
                    "Search names and paths…"
                },
                window,
                cx,
            );
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.refresh_quick_open(cx);
    }

    pub(super) fn close_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_hover(cx);
        self.quick_open.open = false;
        self.quick_open.invalidate();
        self.restore_document_focus(window, cx);
        cx.notify();
    }

    /// Focus returns to the open note, or to the Reader when none is shown.
    pub(super) fn restore_document_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_file().is_empty() || self.file_preview.is_some() {
            self.focus_handle.focus(window, cx);
        } else {
            self.content
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
        }
    }

    pub(super) fn refresh_quick_open(&mut self, cx: &mut Context<Self>) {
        self.clear_hover(cx);
        self.quick_open.invalidate();
        if !self.quick_open.open {
            return;
        }
        self.quick_open.pending = true;
        self.quick_open.message = "Searching…".into();
        self.start_quick_open_query(cx);
        cx.notify();
    }

    fn start_quick_open_query(&mut self, cx: &mut Context<Self>) {
        if self.quick_open.running || !self.quick_open.pending {
            return;
        }
        self.quick_open.running = true;
        self.quick_open.pending = false;
        let generation = self.quick_open.generation;
        let root = self.vault_root.clone();
        let inventory = self.watcher_generation;
        let vault = self.vault.clone();
        let names = self.quick_open.inventory.clone();
        let searcher = self.searcher.clone();
        let prepared = self
            .loading
            .as_ref()
            .is_some_and(|load| !load.active && load.phase == "Ready");
        let query = self.quick_open.input.read(cx).value().trim().to_string();
        let recent = self.quick_open.recent.clone();
        let full_text = self.quick_open.full_text;
        #[cfg(test)]
        let hold = self.quick_open.hold_query.take();
        let title_root = root.clone();
        let titles = self.backlink_titles.clone();
        let task = cx.background_executor().spawn(async move {
            #[cfg(test)]
            if let Some(hold) = hold {
                let _ = hold.recv().await;
            }
            let result = if full_text {
                if query.is_empty() {
                    return Ok((vec![], vec![], "Type to search note contents".to_string()));
                }
                match searcher {
                    Some(searcher) => searcher
                        .search(&query, 100)
                        .map(|rows| (rows, String::new()))
                        .map_err(|error| error.to_string()),
                    None => Ok((
                        vec![],
                        if prepared {
                            "Content search is unavailable; see unreadable paths and Retry".into()
                        } else {
                            "Search index is not ready yet".into()
                        },
                    )),
                }
            } else {
                // Before the first inventory publishes, list what the vault
                // already holds, non-Markdown files included (#686).
                let fallback;
                let files = match names.as_deref() {
                    Some(names) => names,
                    None => {
                        fallback = okilum_core::quick_open::inventory(
                            vault.notes.iter().cloned(),
                            &vault.entries,
                        );
                        &fallback
                    }
                };
                let rows =
                    okilum_core::quick_open::search_titled(files, &titles, &query, &recent, 100)
                        .into_iter()
                        .map(|note| SearchHit {
                            path: note.path,
                            title: note.title,
                            score: 0.,
                            snippet_html: String::new(),
                            display_snippet: None,
                        })
                        .collect();
                Ok((rows, String::new()))
            };
            result.map(|(mut rows, message)| {
                for hit in &mut rows {
                    hit.title = result_title(&title_root, &titles, hit);
                }
                let snippets = rows
                    .iter()
                    .map(|hit| result_snippet(&title_root, &titles, &vault, hit))
                    .collect();
                (rows, snippets, message)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.quick_open.running = false;
                if this.quick_open.open
                    && this.quick_open.generation == generation
                    && this.vault_root == root
                    && this.watcher_generation == inventory
                {
                    match result {
                        Ok((rows, snippets, message)) => {
                            this.quick_open.message = if rows.is_empty() && message.is_empty() {
                                "No matches".into()
                            } else {
                                message
                            };
                            this.quick_open.rows = rows;
                            this.quick_open.snippets = snippets;
                        }
                        Err(error) => this.quick_open.message = error,
                    }
                    this.quick_open.scroll.scroll_to_top_of_item(0);
                    cx.notify();
                }
                this.start_quick_open_query(cx);
            });
        })
        .detach();
    }

    pub(super) fn move_quick_open(&mut self, delta: isize, cx: &mut Context<Self>) {
        let palette = &mut self.quick_open;
        if !palette.open || palette.rows.is_empty() {
            return;
        }
        palette.selected =
            (palette.selected as isize + delta).rem_euclid(palette.rows.len() as isize) as usize;
        palette.scroll.scroll_to_item(palette.selected);
        cx.notify();
    }

    fn accept_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.quick_open.open {
            return;
        }
        let Some(hit) = self.quick_open.rows.get(self.quick_open.selected).cloned() else {
            return;
        };
        let jump = if self.quick_open.full_text {
            okilum_core::quick_open::matched_text(&hit.snippet_html)
        } else {
            None
        };
        self.close_quick_open(window, cx);
        self.open_note(&hit.path, jump.as_deref(), window, cx);
    }

    pub(super) fn render_quick_open(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = brand::palette(cx);
        let rows = self.quick_open.rows.clone();
        let snippets = self.quick_open.snippets.clone();
        let selected = self.quick_open.selected;
        let full_text = self.quick_open.full_text;
        let selected_bg = palette.selected;
        let muted = palette.text_muted;
        let mark = palette.accent.opacity(0.18);
        let entity = cx.entity().downgrade();
        let generation = self.quick_open.generation;
        let root = self.vault_root.clone();
        let inventory = self.watcher_generation;
        let vault_name = self
            .vault_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Results are capped at 100. Content-sized rows avoid reserving blank
        // snippet/reason lines, and keep the keyboard scroll target exact.
        let query = self.quick_open.input.read(cx).value().to_string();
        let list = v_flex()
            .id("quick-open-results")
            .debug_selector(|| "quick-open-results".to_string())
            .w_full()
            .overflow_y_scroll()
            .children(
                (0..rows.len())
                    .map(|ix| {
                        let hit = &rows[ix];
                        let location = if full_text {
                            result_folder(&hit.path)
                        } else {
                            result_location(&vault_name, &hit.path)
                        };
                        let label = |value: String| {
                            let ranges = if full_text {
                                label_matches(&value, &query)
                            } else {
                                vec![]
                            };
                            search_label::SearchLabel::new(value, ranges, mark)
                        };
                        let entity = entity.clone();
                        let root = root.clone();
                        let hover_entity = entity.clone();
                        let hover_path = hit.path.clone();
                        let hover_root = root.clone();
                        v_flex()
                            .id(("quick-open-result", ix))
                            .on_hover(move |active, window, cx| {
                                let _ = hover_entity.update(cx, |this, cx| {
                                    let key = format!("quick:{generation}:{hover_path}");
                                    if *active
                                        && this.quick_open.open
                                        && this.quick_open.generation == generation
                                        && this.vault_root == hover_root
                                        && this.watcher_generation == inventory
                                    {
                                        this.hover_note(
                                            key,
                                            reader_hover::Target {
                                                path: hover_path.clone(),
                                                heading: None,
                                            },
                                            window.mouse_position(),
                                            window,
                                            cx,
                                        );
                                    } else {
                                        this.leave_hover(&key, cx);
                                    }
                                });
                            })
                            .debug_selector(move || format!("quick-open-result-{ix}"))
                            .cursor_pointer()
                            .hover(|row| row.bg(selected_bg))
                            .on_click(move |_, window, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    if this.quick_open.open
                                        && this.quick_open.generation == generation
                                        && this.vault_root == root
                                        && this.watcher_generation == inventory
                                    {
                                        this.quick_open.selected = ix;
                                        this.accept_quick_open(window, cx);
                                    }
                                });
                            })
                            .w_full()
                            .flex_none()
                            .px_3()
                            .py_1()
                            .when(ix == selected, |row| row.bg(selected_bg))
                            .child(
                                div()
                                    .text_sm()
                                    .line_height(px(20.))
                                    .flex_none()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(label(hit.title.clone())),
                            )
                            .when(!location.is_empty(), |row| {
                                row.child(
                                    div()
                                        .text_size(px(12.))
                                        .line_height(px(18.))
                                        .flex_none()
                                        .text_color(muted)
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(label(location)),
                                )
                            })
                            .when_some(
                                snippets.get(ix).filter(|s| {
                                    full_text
                                        && (!s.text.is_empty()
                                            || s.hidden_match.is_some()
                                            || s.property_match.is_some())
                                }),
                                |row, snippet| {
                                    let snippet = visible_snippet(snippet);
                                    let mut row = row.when(!snippet.text.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .id(("quick-open-snippet", ix))
                                                .debug_selector(move || {
                                                    format!("quick-open-snippet-{ix}")
                                                })
                                                .text_size(px(12.))
                                                .line_height(px(18.))
                                                .flex_none()
                                                .text_color(muted)
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .child(search_label::SearchLabel::new(
                                                    snippet.text.clone(),
                                                    snippet.highlights.clone(),
                                                    mark,
                                                )),
                                        )
                                    });
                                    for reason in [
                                        snippet.property_match.as_ref(),
                                        snippet.hidden_match.as_ref(),
                                    ]
                                    .into_iter()
                                    .flatten()
                                    {
                                        let reason = visible_context(reason);
                                        row = row.child(
                                            div()
                                                .text_size(px(12.))
                                                .line_height(px(18.))
                                                .flex_none()
                                                .text_color(muted)
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .child(search_label::SearchLabel::new(
                                                    reason.text.clone(),
                                                    reason.highlights.clone(),
                                                    mark,
                                                )),
                                        );
                                    }
                                    row
                                },
                            )
                    })
                    .collect::<Vec<_>>(),
            )
            .track_scroll(&self.quick_open.scroll)
            .h(px(352.));
        div()
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .justify_center()
            .pt(px(56.))
            .child(
                v_flex()
                    .id("quick-open")
                    .key_context("QuickOpen")
                    .occlude()
                    .w(px(640.))
                    .max_w_full()
                    .h(px(456.))
                    .rounded(px(12.))
                    .bg(palette.surface)
                    .text_color(palette.text)
                    .border_1()
                    .border_color(palette.border)
                    .shadow_lg()
                    .child(div().px_3().py_2().text_sm().child(if full_text {
                        reader_shortcuts::hint("Search contents", &FullTextSearch, cx)
                    } else {
                        reader_shortcuts::hint("Quick open", &QuickOpen, cx)
                    }))
                    .child(
                        div()
                            .px_3()
                            .child(Input::new(&self.quick_open.input).cleanable(true)),
                    )
                    .child(list)
                    .child(div().px_3().text_size(px(12.)).text_color(muted).child(
                        if self.quick_open.message.is_empty() {
                            if self.quick_open.input.read(cx).value().is_empty() {
                                "↑ ↓ Select · Enter Open · Esc Close".to_string()
                            } else {
                                "↑ ↓ Select · Enter Open · Esc Clear".to_string()
                            }
                        } else {
                            self.quick_open.message.clone()
                        },
                    )),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn content_palette_keeps_hebrew_and_human_property_and_target_context(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Projects")).unwrap();
        std::fs::write(root.join("start.md"), "# Start").unwrap();
        std::fs::write(root.join("Projects/roadcanary.md"), "# Human Roadmap").unwrap();
        let source = "---\nqa_label: שלום\nupdated: 2026-10-08\n---\n# Source\nRead [[Projects/roadcanary.md|the overview]]. שלום.";
        std::fs::write(root.join("source.md"), source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(temp.path().join("index")),
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
        visual.run_until_parked();
        for query in ["שלום", "roadcanary", "updated"] {
            reader.update_in(visual, |v, window, cx| {
                v.open_quick_open(true, window, cx);
                v.quick_open
                    .input
                    .update(cx, |i, cx| i.set_value(query, window, cx));
                v.refresh_quick_open(cx);
            });
            visual.run_until_parked();
            reader.read_with(visual, |v, _| {
                let ix = v
                    .quick_open
                    .rows
                    .iter()
                    .position(|h| h.path == "source.md")
                    .expect("indexed source positive control");
                let snippet = &v.quick_open.snippets[ix];
                assert!(!snippet.text.contains("qa_label"));
                match query {
                    "שלום" => {
                        assert_eq!(&snippet.text[snippet.highlights[0].clone()], "שלום");
                        let property = snippet.property_match.as_ref().unwrap();
                        assert_eq!(property.text, "Property · Qa label: שלום");
                        assert_eq!(&property.text[property.highlights[0].clone()], "שלום");
                    }
                    "roadcanary" => {
                        let reason = snippet.hidden_match.as_ref().unwrap();
                        assert_eq!(reason.text, "Link target: Human Roadmap");
                        assert_eq!(&reason.text[reason.highlights[0].clone()], "Human Roadmap");
                    }
                    _ => assert_eq!(
                        snippet.property_match.as_ref().unwrap().text,
                        "Property · Updated: 8 Oct 2026"
                    ),
                }
            });
        }
        assert_eq!(
            std::fs::read_to_string(root.join("source.md")).unwrap(),
            source
        );
    }

    #[test]
    fn search_row_label_marks_are_unicode_whole_words() {
        let text = "Проверка Billing · billingual";
        let ranges = label_matches(text, "проверка billing");
        assert_eq!(
            ranges.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(),
            vec!["Проверка", "Billing"]
        );
        assert_eq!(
            label_matches("Archive › Billing", "path:billing"),
            vec![12..19]
        );
        assert_eq!(label_matches("Billing", "title:billing"), vec![0..7]);
        assert!(label_matches(text, "billing OR other").is_empty());
        assert!(label_matches(text, "-billing").is_empty());
    }

    #[test]
    fn search_row_omits_only_leading_title_and_preserves_body_marks() {
        let source = "# Связанный контекст\nRead [[<b>kara</b>|the overview]] and <b>проверка</b>.";
        let original = plain_snippet(source);
        let shown = without_repeated_title(original.clone(), "Связанный контекст");
        assert_eq!(shown.text, "Read the overview and проверка.");
        assert_eq!(
            shown
                .highlights
                .iter()
                .map(|r| &shown.text[r.clone()])
                .collect::<Vec<_>>(),
            vec!["the overview", "проверка"]
        );
        assert_eq!(shown.hidden_match, original.hidden_match);
        let title_only = without_repeated_title(plain_snippet("# <b>Title</b>"), "Title");
        assert!(title_only.text.is_empty());
        assert!(title_only.highlights.is_empty());
        let hidden_title =
            without_repeated_title(plain_snippet("[[<b>target</b>|Title]]"), "Title");
        assert!(hidden_title.text.is_empty());
        assert!(hidden_title.hidden_match.is_some());
        for (source, title) in [
            ("Knowledge workflows are useful", "Knowledge workflow"),
            ("Read Knowledge workflow next", "Knowledge workflow"),
            (
                "title: Kara roadmap Planning and milestones",
                "Kara roadmap",
            ),
            ("Anything", ""),
        ] {
            let snippet = plain_snippet(source);
            assert_eq!(without_repeated_title(snippet.clone(), title), snippet);
        }
    }

    #[test]
    fn search_row_preview_keeps_match_and_hidden_context_after_unicode_crop() {
        let original = plain_snippet(&format!(
            "{} [[target|<b>проверка</b> <b>Billing</b>]]",
            "вводный текст ".repeat(12)
        ));
        let shown = visible_snippet(&original);
        assert!(shown.text.starts_with('…'));
        assert!(shown.text[..shown.highlights[0].start].chars().count() <= 30);
        assert_eq!(
            shown
                .highlights
                .iter()
                .map(|r| &shown.text[r.clone()])
                .collect::<Vec<_>>(),
            vec!["проверка", "Billing"]
        );
        let hidden = plain_snippet("See [[<b>target</b>|alias]]");
        assert_eq!(visible_snippet(&hidden), hidden);
        assert_eq!(
            visible_snippet(&PlainSnippet::default()),
            PlainSnippet::default()
        );
    }

    #[test]
    fn property_context_keeps_human_label_and_distant_unicode_value_match_visible() {
        use okilum_core::search_snippet::MatchContext;
        let text = format!(
            "Property · Description: {}שלום and tail",
            "שָׁ text ".repeat(80)
        );
        let at = text.find("שלום").unwrap();
        let original = MatchContext {
            text,
            highlights: std::iter::once(at..at + "שלום".len()).collect(),
            ..Default::default()
        };
        let shown = visible_context(&original);
        assert!(shown.text.starts_with("Property · Description: …"));
        assert_eq!(&shown.text[shown.highlights[0].clone()], "שלום");
        assert!(shown.text[..shown.highlights[0].start].chars().count() < 65);
        assert_eq!(
            visible_context(&MatchContext::default()),
            MatchContext::default()
        );
        let key = MatchContext {
            text: "Property · Title: Value".into(),
            highlights: std::iter::once(12..17).collect(),
            ..Default::default()
        };
        assert_eq!(visible_context(&key), key);
    }

    #[test]
    fn result_titles_keep_file_identity_and_use_folder_for_indexes() {
        let root = std::env::temp_dir().join(format!("okilum-titles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Memory")).unwrap();
        for (path, body, expected) in [
            (
                "heading.md",
                "---\ntitle: Metadata\n---\n# Heading\n",
                "Heading",
            ),
            ("metadata.md", "---\ntitle: Metadata\n---\nBody", "Metadata"),
            ("plain.md", "Body", "plain"),
            ("Memory/_index.md", "# Ignored", "Memory"),
            ("Memory/index.md", "# Ignored", "Memory"),
            ("Memory/README.md", "# Ignored", "Memory"),
            ("Memory/Scan.pdf", "%PDF", "Scan.pdf"),
            ("Memory/Board.canvas", "{}", "Board.canvas"),
            ("Memory/readme.txt", "# Not a note", "readme.txt"),
            ("Memory/Схема.excalidraw.md", "# Internal metadata", "Схема"),
            ("Memory/Board.excalidraw", "{}", "Board"),
            ("Memory/Board.EXCALIDRAW.MD", "# Internal metadata", "Board"),
            (
                "Memory/Board.excalidraw.txt",
                "Text",
                "Board.excalidraw.txt",
            ),
        ] {
            std::fs::write(root.join(path), body).unwrap();
            let hit = SearchHit {
                path: path.into(),
                title: Vault::title_of(path),
                score: 0.,
                snippet_html: String::new(),
                display_snippet: None,
            };
            let title = result_title(&root, &HashMap::new(), &hit);
            assert_eq!(title, expected);
            assert!(!title.ends_with(".md"), "{path}");
            assert_eq!(hit.path, path);
        }
        // Root folder notes keep the vault name too, without reading their title.
        let root_index = SearchHit {
            path: "_index.md".into(),
            title: "Ignored".into(),
            score: 0.,
            snippet_html: String::new(),
            display_snippet: None,
        };
        assert_eq!(
            result_title(&root, &HashMap::new(), &root_index),
            root.file_name().unwrap().to_str().unwrap()
        );
        // The shared title map wins over a disk read; folder notes keep their folder.
        let titles = HashMap::from([
            ("plain.md".to_string(), "Cached title".to_string()),
            ("Memory/_index.md".to_string(), "Ignored".to_string()),
        ]);
        for (path, expected) in [("plain.md", "Cached title"), ("Memory/_index.md", "Memory")] {
            let hit = SearchHit {
                path: path.into(),
                title: Vault::title_of(path),
                score: 0.,
                snippet_html: String::new(),
                display_snippet: None,
            };
            assert_eq!(result_title(&root, &titles, &hit), expected);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_location_shows_folder_without_file_name() {
        for (path, expected) in [
            ("plain.md", "Vault"),
            ("Projects/Roadmap.md", "Projects"),
            ("Projects/2026/Q4/Plan.md", "Projects / 2026 / Q4"),
            ("Memory/_index.md", "Vault"),
            ("Projects/Memory/README.md", "Projects"),
            ("Projects/Scan.pdf", "Projects"),
        ] {
            let location = result_location("Vault", path);
            assert_eq!(location, expected, "{path}");
            assert!(!location.contains(".md"));
        }
    }

    #[test]
    fn result_folders_are_breadcrumbs_without_file_names() {
        assert_eq!(result_folder("note.md"), "");
        assert_eq!(result_folder("Memory/_index.md"), "Memory");
        assert_eq!(result_folder("Areas/okilum/Plan.md"), "Areas › okilum");
    }

    #[gpui::test]
    fn incremental_palette_5000_notes_and_superseded_results(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            v.loading = None;
            v.vault = Arc::new(Vault::from_note_paths(
                (0..5000).map(|i| format!("area/Note {i}.md")),
            ));
            v.quick_open.remember("area/Note 4999.md");
            v.open_quick_open(false, window, cx);
        });
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert_eq!(v.quick_open.rows[0].path, "area/Note 4999.md")
        });
        visual.simulate_keystrokes("down");
        reader.update(visual, |v, _| assert_eq!(v.quick_open.selected, 1));
        visual.simulate_keystrokes("up");
        reader.update(visual, |v, _| assert_eq!(v.quick_open.selected, 0));
        visual.simulate_keystrokes("escape");
        reader.update(visual, |v, _| assert!(!v.quick_open.open));
        visual.simulate_keystrokes("ctrl-k");
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.open);
            assert!(v
                .quick_open
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
        });
        for character in "Note 4999".chars() {
            visual.simulate_input(&character.to_string());
            visual.run_until_parked();
            reader.update(visual, |v, _| {
                assert!(
                    !v.quick_open.rows.is_empty(),
                    "each character publishes results"
                );
            });
        }
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.open);
            assert!(v.quick_open.input.read(cx).value().is_empty());
            assert!(v
                .quick_open
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
            assert!(
                !v.quick_open.rows.is_empty(),
                "empty query restores choices"
            );
        });
        visual.simulate_keystrokes("escape");
        reader.update_in(visual, |v, window, cx| {
            assert!(!v.quick_open.open);
            assert!(
                v.focus_handle.is_focused(window),
                "empty vault keeps a mounted focus target"
            );
            v.open_quick_open(false, window, cx);
        });
        visual.simulate_input("Note 4999");
        visual.run_until_parked();
        let (release, hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.quick_open.hold_query = Some(hold);
            assert_eq!(v.quick_open.rows[0].path, "area/Note 4999.md");
            // A real task remains in flight while subsequent edits replace pending work.
            for text in ["Note 1", "Note 25", "Note 4998"] {
                v.quick_open
                    .input
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                v.refresh_quick_open(cx);
            }
        });
        visual.run_until_parked();
        reader.update(visual, |v, cx| {
            assert_eq!(v.quick_open.input.read(cx).value().to_string(), "Note 4998");
            assert!(v.quick_open.running);
            assert!(v.quick_open.rows.is_empty());
        });
        release.try_send(()).unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, _, cx| {
            assert_eq!(v.quick_open.rows[0].path, "area/Note 4998.md");
            v.refresh_quick_open(cx);
            v.watcher_generation += 1;
        });
        visual.run_until_parked();
        reader.update(visual, |v, cx| {
            assert!(
                v.quick_open.rows.is_empty(),
                "old inventory result rejected"
            );
            v.refresh_quick_open(cx);
            v.vault_root = PathBuf::from("replacement-root");
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.rows.is_empty(), "old root result rejected");
            v.refresh_quick_open(cx);
            v.close_quick_open(window, cx);
        });
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert!(!v.quick_open.open);
            assert!(
                v.quick_open.rows.is_empty(),
                "closed palette rejects in-flight result"
            );
        });
    }
    #[gpui::test]
    fn hidden_notes_and_new_directory_arrivals_reach_both_palettes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp =
            std::env::temp_dir().join(format!("okilum-hidden-search-{}", uuid::Uuid::new_v4()));
        let root = temp.join("vault");
        let path = "_Assets/Daily Notes/2026/10/04/2026-10-04-полный-ai-стэк.md";
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join("Target.md"), "# Target").unwrap();
        std::fs::write(
            root.join(path),
            "# Полный AI

canaryhidden [[Target]]",
        )
        .unwrap();
        for directory in [".git", ".obsidian", ".trash", "node_modules"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
            std::fs::write(
                root.join(directory).join("excluded.md"),
                "canaryhidden [[Target]]",
            )
            .unwrap();
        }
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Target.md".into()),
                        index_dir: Some(temp.join("index")),
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
        visual.run_until_parked();
        for (new_note, expected_path, name, content) in [
            (false, path, "2026-10-04-полный-ai", "canaryhidden"),
            (
                true,
                "_Incoming/Новая заметка.md",
                "Новая заметка",
                "canaryarrival",
            ),
        ] {
            if new_note {
                let staged = temp.join("incoming");
                std::fs::create_dir_all(&staged).unwrap();
                std::fs::write(staged.join("Новая заметка.md"), "canaryarrival [[Target]]")
                    .unwrap();
                std::fs::rename(staged, root.join("_Incoming")).unwrap();
                reader.update_in(visual, |v, window, cx| {
                    let changes = v
                        .watcher
                        .as_mut()
                        .expect("live watcher")
                        .wait(Duration::from_secs(3))
                        .expect("directory arrival must trigger reconciliation");
                    assert!(!changes.rescan && changes.directories.contains("_Incoming"));
                    v.apply_vault_changes(changes, window, cx);
                });
                visual.run_until_parked();
            }
            reader.update(visual, |v, _| {
                assert!(v.searcher.is_some(), "published index positive control");
                assert!(v.vault.notes.iter().any(|note| note.path == expected_path));
                assert!(v
                    .vault
                    .backlinks("Target.md")
                    .iter()
                    .any(|link| link.path == expected_path));
                assert!(matches!(
                    v.vault.resolve(expected_path.trim_end_matches(".md")),
                    okilum_core::vault::Resolution::Resolved { .. }
                ));
                assert!(!v
                    .vault
                    .notes
                    .iter()
                    .any(|note| note.path.ends_with("excluded.md")));
            });
            reader.update(visual, |v, cx| {
                v.sync_tree();
                assert!(!v.tree.show_hidden());
                assert!(v.tree.rows.iter().any(|row| row.path == "Target.md"));
                assert!(!v.tree.rows.iter().any(|row| row.path.starts_with('_')));
                let inventory = v.vault.clone();
                // Exercise the same action as Show hidden files, without reveal:
                // revealing the current note would bypass the visibility filter.
                v.toggle_hidden_files(cx);
                assert!(v.tree.show_hidden());
                let mut directory = String::new();
                let components: Vec<_> = expected_path.split('/').collect();
                for component in &components[..components.len() - 1] {
                    if !directory.is_empty() {
                        directory.push('/');
                    }
                    directory.push_str(component);
                    let row = v
                        .tree
                        .rows
                        .iter()
                        .find(|row| row.path == directory)
                        .expect("every hidden ancestor must be browsable");
                    if !row.expanded {
                        v.tree.toggle(&directory);
                    }
                }
                assert!(v.tree.rows.iter().any(|row| row.path == expected_path));
                assert!(!v.tree.rows.iter().any(|row| [
                    ".git",
                    ".obsidian",
                    ".trash",
                    "node_modules"
                ]
                .contains(&row.path.as_str())));
                v.toggle_hidden_files(cx);
                assert!(!v.tree.rows.iter().any(|row| row.path.starts_with('_')));
                assert!(
                    Arc::ptr_eq(&inventory, &v.vault),
                    "visibility must not replace inventory"
                );
            });
            for (full_text, query) in [(false, name), (true, content)] {
                reader.update_in(visual, |v, window, cx| {
                    v.open_quick_open(full_text, window, cx);
                    v.quick_open
                        .input
                        .update(cx, |input, cx| input.set_value(query, window, cx));
                    v.refresh_quick_open(cx);
                });
                visual.run_until_parked();
                reader.update(visual, |v, _| {
                    assert_eq!(v.quick_open.rows.len(), 1, "query {query}");
                    assert_eq!(v.quick_open.rows[0].path, expected_path);
                });
            }
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn names_list_non_markdown_files_with_extension_and_folder(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp =
            std::env::temp_dir().join(format!("okilum-quick-files-{}", uuid::Uuid::new_v4()));
        let root = temp.join("Vault");
        std::fs::create_dir_all(root.join("Health")).unwrap();
        std::fs::write(root.join("start.md"), "# Start").unwrap();
        std::fs::write(root.join("tg.log"), "log line").unwrap();
        std::fs::write(root.join("Health/medications.md"), "# Medications").unwrap();
        std::fs::write(root.join("Health/medications-data.toml"), "dose = 1").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(temp.join("index")),
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
        visual.run_until_parked();
        let rows = |query: &str, visual: &mut VisualTestContext| {
            reader.update_in(visual, |v, window, cx| {
                v.open_quick_open(false, window, cx);
                v.quick_open
                    .input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
                v.refresh_quick_open(cx);
            });
            visual.run_until_parked();
            reader.update(visual, |v, _| {
                assert!(v.quick_open.inventory.is_some(), "published inventory");
                v.quick_open
                    .rows
                    .iter()
                    .map(|hit| {
                        (
                            hit.path.clone(),
                            hit.title.clone(),
                            result_location("Vault", &hit.path),
                        )
                    })
                    .collect::<Vec<_>>()
            })
        };
        let row = |path: &str, title: &str, folder: &str| {
            (path.to_string(), title.to_string(), folder.to_string())
        };
        assert_eq!(rows("tg", visual)[0], row("tg.log", "tg.log", "Vault"));
        // The note keeps its title; the data file keeps its extension.
        assert_eq!(
            rows("medications", visual),
            [
                row("Health/medications.md", "Medications", "Health"),
                row(
                    "Health/medications-data.toml",
                    "medications-data.toml",
                    "Health"
                ),
            ]
        );
        // Every openable file is listed before a query is typed.
        assert_eq!(rows("", visual).len(), 4);
        rows("tg.log", visual);
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert!(!v.quick_open.open);
            assert_eq!(
                v.file_preview.as_ref().map(|preview| preview.rel.as_str()),
                Some("tg.log")
            );
            assert!(!v.single_file, "vault-mode positive control");
            let preview = v.file_preview.as_ref().unwrap();
            assert!(
                preview.log.is_some(),
                "Quick Open mounts the structured log viewer"
            );
            assert!(preview.text.is_none());
        });
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn mouse_and_keyboard_open_results_and_mark_content_match(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("okilum-quick-open-{}", uuid::Uuid::new_v4()));
        let index = root.with_extension("index");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\nOrdinary text.").unwrap();
        std::fs::create_dir_all(root.join("Memory")).unwrap();
        std::fs::write(
            root.join("Memory/2026-10-06-kickoff.md"),
            "---\ntitle: Ignored\n---\n# Quarterly planning\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Memory/_index.md"),
            format!(
                "# Target\n\n{}\n\nUnique **canaryword** landing near [[start|the start]].",
                "Filler paragraph.\n\n".repeat(50)
            ),
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(index.clone()),
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
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.searcher.is_some(), "index must publish before searching");
            window.focus(&v.focus_handle, cx);
        });
        // The resolved title matches, fuzzily too, although no file name or
        // path contains it (#645).
        for query in ["Quarterly planning", "qplan"] {
            reader.update_in(visual, |v, window, cx| {
                v.open_quick_open(false, window, cx);
                v.quick_open
                    .input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
                v.refresh_quick_open(cx);
            });
            visual.run_until_parked();
            reader.update_in(visual, |v, window, cx| {
                assert_eq!(v.quick_open.rows.len(), 1, "query {query}");
                assert_eq!(v.quick_open.rows[0].path, "Memory/2026-10-06-kickoff.md");
                assert_eq!(v.quick_open.rows[0].title, "Quarterly planning");
                v.close_quick_open(window, cx);
            });
        }
        // Name/path matching remains intact even though the displayed title differs.
        visual.simulate_keystrokes("ctrl-k");
        visual.run_until_parked();
        visual.simulate_input("Memory/_index");
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert_eq!(v.quick_open.rows.len(), 1);
            assert_eq!(v.quick_open.rows[0].title, "Memory");
            assert_eq!(v.current_rel, "start.md");
        });
        let bounds = visual
            .debug_bounds("quick-open-result-0")
            .expect("visible name result");
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert!(!v.quick_open.open);
            assert_eq!(v.current_rel, "Memory/_index.md");
        });
        for mouse in [false, true] {
            reader.update_in(visual, |v, window, cx| {
                v.open_note("start.md", None, window, cx);
            });
            visual.run_until_parked();
            if mouse {
                let search = visual
                    .debug_bounds("reader-search")
                    .expect("main toolbar search");
                visual.simulate_click(search.center(), Modifiers::default());
            } else {
                visual.simulate_keystrokes("ctrl-shift-f");
            }
            visual.run_until_parked();
            visual.simulate_input("canaryword");
            visual.run_until_parked();
            reader.update(visual, |v, cx| {
                assert_eq!(
                    v.quick_open.input.read(cx).value().to_string(),
                    "canaryword"
                );
                assert!(v.searcher.is_some(), "index publication positive control");
                assert!(v.quick_open.full_text);
                assert_eq!(v.quick_open.rows.len(), 1);
                assert_eq!(v.quick_open.rows[0].title, "Memory");
                assert_eq!(v.quick_open.rows[0].path, "Memory/_index.md");
                assert!(v.quick_open.rows[0]
                    .snippet_html
                    .contains("<b>canaryword</b>"));
                // #654: the row shows plain text with the match marked.
                let snippet = &v.quick_open.snippets[0];
                assert!(
                    snippet
                        .text
                        .trim_end_matches(['…', '.'])
                        .ends_with("Unique canaryword landing near the start"),
                    "{snippet:?}"
                );
                assert!(!snippet.text.contains(['*', '[', '#']), "{snippet:?}");
                assert_eq!(
                    snippet
                        .highlights
                        .iter()
                        .map(|r| &snippet.text[r.clone()])
                        .collect::<Vec<_>>(),
                    ["canaryword"]
                );
                assert_eq!(result_folder(&v.quick_open.rows[0].path), "Memory");
            });
            let row = visual.debug_bounds("quick-open-result-0").unwrap();
            let list = visual.debug_bounds("quick-open-results").unwrap();
            assert_eq!(
                row.size.width, list.size.width,
                "selection must span the list"
            );
            assert!(
                row.size.height <= px(64.),
                "ordinary content result must be compact: {row:?}"
            );
            if mouse {
                let bounds = visual
                    .debug_bounds("quick-open-result-0")
                    .expect("visible search result");
                visual.simulate_click(bounds.center(), Modifiers::default());
            } else {
                visual.simulate_keystrokes("enter");
            }
            visual.run_until_parked();
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
            reader.update(visual, |v, cx| {
                assert!(!v.quick_open.open);
                assert_eq!(v.current_rel, "Memory/_index.md");
                assert_eq!(v.content.read(cx).search_status().1, 1);
                assert!(v.content.read(cx).list_state().logical_scroll_top().item_ix > 0);
            });
        }
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(index).unwrap();
    }
}
