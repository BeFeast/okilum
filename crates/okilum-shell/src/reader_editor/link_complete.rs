//! Shared inline completion in the note editor (#1921, #1638). Notes, headings,
//! emoji, fence languages and slash blocks reuse one popup and ranker.
//! Typing `[[` and the start of
//! a name opens a list of matching notes under the caret; Up and Down move
//! through it, Enter or Tab writes the link, Escape closes it for this link,
//! and a click chooses a row. Typing on without choosing keeps the text as
//! typed: a link to a note that does not exist yet stays possible.
//!
//! The written target is the shortest name that resolves to the chosen note
//! and no other (`[[Plan]]`, or `[[Work/Plan]]` when two notes are called
//! Plan): Okilum never writes a link it would itself report as ambiguous. An
//! alias or heading already typed after the name (`|…`, `#…`) is kept; a
//! closing `]]` is added only when the link has none. The completion is one
//! source edit and one Undo step. Nothing opens in a fenced code block, the
//! front matter or an inline code span, while an input method composes, or
//! in a code or text file.
use super::ime_guard::Trigger;
use super::inline_complete::{self as providers, Kind};
use super::lists::{self, Edit};
use super::*;
use gpui_component::input::projection::SourceStamp;
use gpui_component::input::{Enter, Escape, IndentInline, MoveDown, MoveUp, RopeExt as _};
use std::ops::Range;

/// Rows the list shows at most.
pub(super) const MAX_ROWS: usize = 8;
/// A longer name is no longer being typed as a link.
#[cfg(test)]
const MAX_QUERY_CHARS: usize = 120;
/// Characters that end a wikilink name.
const NAME_END: [char; 5] = ['[', ']', '|', '#', '^'];

/// An open `[[` before the caret on the caret's line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkQuery {
    /// Just after the `[[`.
    pub start: usize,
    /// The caret.
    pub caret: usize,
    /// The name typed so far.
    pub query: String,
    pub kind: Kind,
}

/// The link being typed at `caret`, if any.
pub(super) fn link_query(text: &str, caret: usize) -> Option<LinkQuery> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let line_start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
    let before = &text[line_start..caret];
    let (kind, offset, query) = providers::trigger(before, true)?;
    let start = line_start + offset;
    let delimiter = match kind {
        Kind::Note | Kind::Heading(_) => offset - 2,
        _ => offset,
    };
    if !matches!(kind, Kind::Language) && before[..delimiter].matches('`').count() % 2 == 1 {
        return None;
    }
    if lists::in_code(text, line_start) {
        return None;
    }
    Some(LinkQuery {
        start,
        caret,
        query,
        kind,
    })
}

/// A note the list offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    /// Vault-relative path, with `.md`.
    pub path: String,
    /// The note's name, as the sidebar shows it.
    pub title: String,
    /// Its folder, for telling namesakes apart; empty at the vault root.
    pub folder: String,
    /// What goes between `[[` and `]]`.
    pub target: String,
    pub badge: Option<String>,
}

/// The vault's notes, lowercased once for matching as the user types.
pub(super) struct Index {
    vault: Arc<Vault>,
    show_hidden: bool,
    /// (lowercase title, lowercase path without `.md`, index into `vault.notes`)
    notes: Vec<(String, String, usize)>,
}

impl Index {
    pub(super) fn new(vault: Arc<Vault>, show_hidden: bool) -> Self {
        let notes = vault
            .notes
            .iter()
            .enumerate()
            .filter(|(_, note)| show_hidden || !reader_tree::hidden(&note.path))
            // A path holding link syntax cannot be written as a wikilink.
            .filter(|(_, note)| !note.path.contains(NAME_END))
            .map(|(at, note)| {
                (
                    note.title.to_lowercase(),
                    strip_md(&note.path).to_lowercase(),
                    at,
                )
            })
            .collect();
        Self {
            vault,
            show_hidden,
            notes,
        }
    }

    fn current(&self, vault: &Arc<Vault>, show_hidden: bool) -> bool {
        Arc::ptr_eq(&self.vault, vault) && self.show_hidden == show_hidden
    }

    /// The best matches for `query`, written from the note at `from`.
    pub(super) fn search(&self, query: &str, from: &str) -> Vec<Candidate> {
        let query = query.trim().to_lowercase();
        let from_folder = folder_of(from);
        let mut ranked: Vec<(u8, usize, &str, usize)> = self
            .notes
            .iter()
            .filter(|(_, _, at)| self.vault.notes[*at].path != from)
            .filter_map(|(title, path, at)| {
                let score = if query.is_empty() {
                    // Nothing typed yet: the note's neighbours first.
                    u8::from(folder_of(&self.vault.notes[*at].path) != from_folder)
                } else {
                    score(&query, title, path)?
                };
                Some((score, title.chars().count(), path.as_str(), *at))
            })
            .collect();
        ranked.sort_unstable();
        ranked
            .into_iter()
            .map(|(_, _, _, at)| {
                let note = &self.vault.notes[at];
                Candidate {
                    path: note.path.clone(),
                    title: note.title.clone(),
                    folder: folder_of(&note.path).to_owned(),
                    target: target_for(&self.vault, &note.path, from),
                    badge: None,
                }
            })
            .collect()
    }
}

fn strip_md(path: &str) -> &str {
    path.strip_suffix(".md")
        .or_else(|| path.strip_suffix(".MD"))
        .or_else(|| path.strip_suffix(".Md"))
        .or_else(|| path.strip_suffix(".mD"))
        .unwrap_or(path)
}

fn folder_of(path: &str) -> &str {
    path.rfind('/').map_or("", |at| &path[..at])
}

/// Lower is better; `None` does not match. Both sides are lowercase.
fn score(query: &str, title: &str, path: &str) -> Option<u8> {
    providers::score(query, title, path)
}

/// The shortest trailing part of the note's path that resolves to it alone
/// from `from`; the whole path when none does.
pub(super) fn target_for(vault: &Vault, path: &str, from: &str) -> String {
    let name = strip_md(path);
    let mut at = name.len();
    loop {
        let start = name[..at].rfind('/').map_or(0, |slash| slash + 1);
        let suffix = &name[start..];
        if matches!(
            vault.resolve_from_ref(suffix, from),
            okilum_core::vault::ResolutionRef::Resolved(found) if found == path
        ) {
            return suffix.to_owned();
        }
        if start == 0 {
            return name.to_owned();
        }
        at = start - 1;
    }
}

/// The edit that writes `target` for the link at `query`, and the caret
/// after it. The name around the caret is replaced up to the link's `]]`,
/// `|` or `#`; with none on the line, `]]` closes the link.
pub(super) fn completion(text: &str, query: &LinkQuery, target: &str) -> (Vec<Edit>, usize) {
    let (range, replacement, caret) =
        providers::replacement(text, query.start, query.caret, &query.kind, target);
    (vec![(range, replacement)], caret)
}

/// Reuse the same rows, key handling and acceptance path for every provider.
/// A source edit can temporarily empty an async provider's rows. Remember the
/// user's choice by identity across that gap, never by its old numeric position.
fn publish(state: &mut State, query: LinkQuery, items: Vec<Candidate>) {
    let preferred = state
        .open
        .as_ref()
        .filter(|open| open.query.kind == query.kind && open.query.start == query.start)
        .and_then(|open| open.items.get(open.selected))
        .map(|item| {
            (
                query.kind.clone(),
                query.start,
                item.path.clone(),
                item.target.clone(),
            )
        })
        .or_else(|| state.preferred.clone())
        .filter(|(kind, start, _, _)| *kind == query.kind && *start == query.start);
    let selected = preferred
        .as_ref()
        .and_then(|(_, _, path, target)| {
            items
                .iter()
                .position(|item| item.path == *path && item.target == *target)
        })
        .unwrap_or(0);
    state.preferred = items
        .get(selected)
        .map(|item| {
            (
                query.kind.clone(),
                query.start,
                item.path.clone(),
                item.target.clone(),
            )
        })
        .or(preferred);
    state.open = (!items.is_empty()).then_some(Open {
        query,
        items,
        selected,
    });
}

/// A click belongs to the row that was painted, including its source revision.
/// A source edit or asynchronous reorder must never retarget an old click.
struct RowIntent {
    query: LinkQuery,
    source: SourceStamp,
    path: String,
    target: String,
}

impl RowIntent {
    fn choice(&self, open: &Open, source: SourceStamp) -> Option<usize> {
        if self.source != source || self.query != open.query {
            return None;
        }
        open.items
            .iter()
            .position(|item| item.path == self.path && item.target == self.target)
    }
}

fn row(title: &str, target: &str, folder: &str) -> Candidate {
    Candidate {
        path: String::new(),
        title: title.into(),
        folder: folder.into(),
        target: target.into(),
        badge: None,
    }
}

fn catalog(query: &LinkQuery) -> Vec<Candidate> {
    let items = match query.kind {
        Kind::Language => providers::LANGUAGES
            .iter()
            .map(|name| row(name, name, "Language"))
            .chain(
                okilum_core::code_language::ALIASES
                    .iter()
                    .map(|(alias, name)| row(alias, alias, name)),
            )
            .collect(),
        Kind::Emoji => providers::EMOJI
            .iter()
            .map(|(name, emoji)| row(name, emoji, emoji))
            .collect(),
        Kind::Block => {
            let mut rows: Vec<_> = providers::BLOCKS
                .iter()
                .map(|(name, source)| row(name, source, "Markdown"))
                .collect();
            rows.push(row(
                "date",
                &chrono::Local::now().format("%Y-%m-%d").to_string(),
                "Today",
            ));
            rows
        }
        _ => Vec::new(),
    };
    rank_rows(&query.query, items)
}

fn rank_rows(query: &str, items: Vec<Candidate>) -> Vec<Candidate> {
    let query = query.to_lowercase();
    let mut ranked: Vec<_> = items
        .into_iter()
        .filter_map(|item| {
            let score = score(
                &query,
                &item.title.to_lowercase(),
                &item.title.to_lowercase(),
            )?;
            Some((score, item))
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.title.cmp(&b.1.title)));
    ranked.into_iter().map(|(_, row)| row).collect()
}

/// Metadata reads for the first visible page and Markdown parsing run on the background executor.
/// Current-note headings use the editor's unsaved source, never the disk copy.
fn enrich(
    vault: &Vault,
    from: &str,
    source: Option<&str>,
    query: &LinkQuery,
    mut items: Vec<Candidate>,
) -> Vec<Candidate> {
    use okilum_core::{
        document_links::HeadingInventory,
        properties::{self, PropertyValue},
        vault::ResolutionRef,
    };
    if let Kind::Heading(target) = &query.kind {
        let path = if target.is_empty() {
            from
        } else {
            match vault.resolve_from_ref(target, from) {
                ResolutionRef::Resolved(path) => path,
                _ => return Vec::new(), // Never guess an ambiguous note.
            }
        };
        let loaded;
        let text = if path == from {
            source.unwrap_or("")
        } else {
            loaded = vault.read_note(path);
            let Ok(text) = &loaded else {
                return Vec::new();
            };
            text
        };
        let inventory = HeadingInventory::new(text);
        items = inventory
            .entries
            .iter()
            .filter(|h| !h.text.contains(NAME_END) && inventory.locate(&h.text).is_ok())
            .map(|h| row(&h.text, &format!("{target}#{}", h.text), path))
            .collect();
        return rank_rows(&query.query, items);
    }
    for item in items.iter_mut().take(MAX_ROWS) {
        let Ok(text) = vault.read_note(&item.path) else {
            continue;
        };
        item.badge = properties::frontmatter_block(&text)
            .and_then(|yaml| properties::parse(yaml).ok())
            .and_then(|props| props.into_iter().find(|p| p.key == "type"))
            .and_then(|p| match p.value {
                PropertyValue::Text(value) => Some(value),
                _ => None,
            });
    }
    items
}

/// The list while it is open.
pub(super) struct Open {
    pub query: LinkQuery,
    pub items: Vec<Candidate>,
    pub selected: usize,
}

/// Per editor: what the list shows, and what it was computed for.
#[derive(Default)]
pub(super) struct State {
    seen: Option<(
        gpui_component::input::projection::SourceStamp,
        Range<usize>,
        bool,
    )>,
    pub open: Option<Open>,
    /// The `[[` whose list Escape closed (or a choice completed); it stays
    /// closed until the caret leaves that link.
    dismissed: Option<usize>,
    index: Option<Arc<Index>>,
    task: Option<Task<()>>,
    from: String,
    composing: bool,
    editable: bool,
    markdown: bool,
    preferred: Option<(Kind, usize, String, String)>,
}

impl Reader {
    /// Recompute the list after the text or the caret changed.
    pub(super) fn refresh_link_complete(&mut self, cx: &mut Context<Self>) {
        let emoji = reader_ui_state::emoji_completion(cx);
        let show_hidden = self.sidebar.show_hidden;
        let vault = self.vault.clone();
        let from = self.current_rel.clone();
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let input = editing.input.read(cx);
        let seen = (input.source_stamp(), input.selected_range(), emoji);
        if editing.link_complete.seen.as_ref() == Some(&seen)
            && editing.link_complete.composing == input.ime_composing()
            && editing.link_complete.editable == input.is_editable()
            && editing.link_complete.markdown == editing.markdown
            && editing.link_complete.from == from
            && editing
                .link_complete
                .index
                .as_ref()
                .is_some_and(|index| index.current(&vault, show_hidden))
        {
            return;
        }
        let was_open = editing.link_complete.open.is_some();
        // A different document, vault snapshot or visibility policy starts a
        // new completion session even if the source stamp/query looks equal.
        if editing.link_complete.from != from
            || !editing
                .link_complete
                .index
                .as_ref()
                .is_some_and(|index| index.current(&vault, show_hidden))
        {
            editing.link_complete.open = None;
            editing.link_complete.preferred = None;
            editing.link_complete.dismissed = None;
        }
        let selection = seen.1.clone();
        let expected_seen = seen.clone();
        editing.link_complete.seen = Some(seen);
        editing.link_complete.composing = input.ime_composing();
        editing.link_complete.editable = input.is_editable();
        editing.link_complete.markdown = editing.markdown;
        editing.link_complete.from = from.clone();
        let query = (editing.markdown
            && selection.is_empty()
            && input.is_editable()
            && !input.ime_composing())
        .then(|| {
            let row = input.cursor_position().line as usize;
            let line = input.text().slice_line(row).to_string();
            (line.contains("[[")
                || line.contains(':')
                || line.starts_with('/')
                || line.starts_with("```"))
            .then(|| link_query(&input.value(), selection.start))
            .flatten()
            .filter(|query| emoji || !matches!(query.kind, Kind::Emoji))
        })
        .flatten();
        let source = query
            .as_ref()
            .filter(|q| matches!(q.kind, Kind::Heading(_)))
            .map(|_| input.value().to_string());
        let input_entity = editing.input.clone();
        let state = &mut editing.link_complete;
        state.task = None;
        let Some(query) = query else {
            state.dismissed = None;
            state.preferred = None;
            state.open = None;
            if was_open {
                cx.notify();
            }
            return;
        };
        if state.dismissed.is_some_and(|start| start != query.start) {
            state.dismissed = None;
        }
        if state.dismissed.is_some() {
            state.open = None;
            if was_open {
                cx.notify();
            }
            return;
        }
        if !state
            .index
            .as_ref()
            .is_some_and(|index| index.current(&vault, show_hidden))
        {
            state.index = Some(Arc::new(Index::new(vault, show_hidden)));
        }
        let items = match &query.kind {
            Kind::Note => Vec::new(),
            Kind::Heading(_) => Vec::new(),
            _ => catalog(&query),
        };
        publish(state, query.clone(), items.clone());
        if matches!(query.kind, Kind::Note | Kind::Heading(_)) {
            let expected_vault = self.vault.clone();
            let vault = expected_vault.clone();
            let from_expected = from.clone();
            let index = state.index.as_ref().unwrap().clone();
            state.task = Some(cx.spawn(async move |this, cx| {
                let worker_query = query.clone();
                let rows = cx
                    .background_executor()
                    .spawn(async move {
                        let items = if matches!(worker_query.kind, Kind::Note) {
                            index.search(&worker_query.query, &from)
                        } else {
                            items
                        };
                        enrich(&vault, &from, source.as_deref(), &worker_query, items)
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if !Arc::ptr_eq(&this.vault, &expected_vault)
                        || this.current_rel != from_expected
                    {
                        return;
                    }
                    let Some(editing) = this.editing.as_mut() else {
                        return;
                    };
                    if editing.input != input_entity {
                        return;
                    }
                    let input = editing.input.read(cx);
                    let current = (
                        input.source_stamp(),
                        input.selected_range(),
                        reader_ui_state::emoji_completion(cx),
                    );
                    if current != expected_seen
                        || editing.link_complete.seen.as_ref() != Some(&expected_seen)
                        || this.sidebar.show_hidden != show_hidden
                        || !editing.markdown
                        || input.ime_composing()
                        || !input.is_editable()
                        || editing.link_complete.dismissed == Some(query.start)
                    {
                        return;
                    }
                    publish(&mut editing.link_complete, query, rows);
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    /// The editor while its link list is open and may take keys.
    fn link_complete_input(&self, window: &Window, cx: &App) -> Option<Entity<EditorState>> {
        let editing = self.editing.as_ref()?;
        editing.link_complete.open.as_ref()?;
        let input = editing.input.read(cx);
        let current = (
            input.source_stamp(),
            input.selected_range(),
            reader_ui_state::emoji_completion(cx),
        );
        if !editing.markdown
            || editing.link_complete.seen.as_ref() != Some(&current)
            || editing.link_complete.from != self.current_rel
            || !editing
                .link_complete
                .index
                .as_ref()
                .is_some_and(|index| index.current(&self.vault, self.sidebar.show_hidden))
        {
            return None;
        }
        if matches!(editing.link_complete.open.as_ref()?.query.kind, Kind::Emoji)
            && !reader_ui_state::emoji_completion(cx)
        {
            return None;
        }
        (input.focus_handle(cx).is_focused(window)
            && input.is_editable()
            && input.selected_range().is_empty()
            && !input.ime_composing())
        .then(|| editing.input.clone())
    }

    fn link_complete_step(&mut self, delta: isize, window: &Window, cx: &mut Context<Self>) {
        if self.link_complete_input(window, cx).is_none() {
            return;
        }
        let Some(open) = self
            .editing
            .as_mut()
            .and_then(|editing| editing.link_complete.open.as_mut())
        else {
            return;
        };
        let count = open.items.len() as isize;
        open.selected = (open.selected as isize + delta).rem_euclid(count) as usize;
        cx.stop_propagation();
        cx.notify();
    }

    fn link_complete_dismiss(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.link_complete_input(window, cx).is_none() {
            return;
        }
        if let Some(editing) = self.editing.as_mut() {
            let state = &mut editing.link_complete;
            state.dismissed = state.open.take().map(|open| open.query.start);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn link_complete_accept_row(
        &mut self,
        intent: &RowIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(choice) = self.editing.as_ref().and_then(|editing| {
            intent.choice(
                editing.link_complete.open.as_ref()?,
                editing.input.read(cx).source_stamp(),
            )
        }) else {
            return;
        };
        self.link_complete_accept(Some(choice), Trigger::Text, window, cx);
    }

    /// Write the chosen note's link: row `choice`, or the highlighted one.
    pub(super) fn link_complete_accept(
        &mut self,
        choice: Option<usize>,
        trigger: Trigger,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ime_allows(trigger, cx) {
            return;
        }
        let Some(input) = self.link_complete_input(window, cx) else {
            return;
        };
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let Some(open) = editing.link_complete.open.take() else {
            return;
        };
        let Some(item) = open.items.get(choice.unwrap_or(open.selected)) else {
            return;
        };
        editing.link_complete.dismissed = Some(open.query.start);
        cx.stop_propagation();
        let text = input.read(cx).value();
        // The list was computed for this text and caret; anything else is stale.
        if link_query(&text, input.read(cx).selected_range().start).as_ref() != Some(&open.query) {
            cx.notify();
            return;
        }
        let (edits, caret) = completion(&text, &open.query, &item.target);
        input.update(cx, |state, cx| {
            state.replace_source_edits(&edits, caret..caret, window, cx)
        });
        cx.notify();
    }

    /// Up, Down, Enter, Tab and Escape go to the list while it is open,
    /// before the list keys, code comforts and the input see them.
    pub(super) fn link_complete_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .capture_action(
                cx.listener(|this, _: &MoveUp, window, cx| this.link_complete_step(-1, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &MoveDown, window, cx| {
                    this.link_complete_step(1, window, cx)
                }),
            )
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if !action.secondary && !action.shift {
                    this.link_complete_accept(None, Trigger::Enter, window, cx)
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                this.link_complete_accept(None, Trigger::Tab, window, cx)
            }))
            .capture_action(
                cx.listener(|this, _: &Escape, window, cx| this.link_complete_dismiss(window, cx)),
            )
    }

    /// The list under the caret, while the editor has focus.
    pub(super) fn render_link_complete(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let input = self.link_complete_input(window, cx)?;
        let open = self.editing.as_ref()?.link_complete.open.as_ref()?;
        let state = input.read(cx);
        let caret = state.range_to_bounds(&(open.query.caret..open.query.caret))?;
        let p = brand::palette(cx);
        let visible = providers::visible_rows(open.items.len(), open.selected, MAX_ROWS);
        let rows = open
            .items
            .iter()
            .enumerate()
            .skip(visible.start)
            .take(visible.len())
            .map(|(i, item)| {
                let selected = i == open.selected;
                let intent = RowIntent {
                    query: open.query.clone(),
                    source: state.source_stamp(),
                    path: item.path.clone(),
                    target: item.target.clone(),
                };
                h_flex()
                    .id(("link-complete-row", i))
                    .debug_selector(move || format!("link-complete-{i}"))
                    .h(px(28.))
                    .px_2()
                    .gap_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(p.selected))
                    .hover(move |row| row.bg(p.selected))
                    .child(
                        Icon::new(IconName::FileText)
                            .small()
                            .text_color(p.text_muted),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(px(220.))
                            .text_ellipsis()
                            .child(item.title.clone()),
                    )
                    .when(!item.folder.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .text_color(p.text_muted)
                                .child(item.folder.replace('/', " › ")),
                        )
                    })
                    .children(item.badge.as_ref().map(|kind| {
                        let tones = [p.info, p.success, p.warning, p.accent];
                        let tone =
                            tones[kind.bytes().fold(0usize, |n, b| n.wrapping_add(b as usize))
                                % tones.len()];
                        div()
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .bg(tone.opacity(0.10))
                            .text_color(tone)
                            .child(kind.clone())
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.link_complete_accept_row(&intent, window, cx);
                        }),
                    )
            });
        Some(
            deferred(
                anchored()
                    .position(point(caret.left(), caret.bottom() + px(4.)))
                    .snap_to_window_with_margin(px(12.))
                    .child(
                        v_flex()
                            .id("link-complete")
                            .debug_selector(|| "link-complete".into())
                            .occlude()
                            .on_prepaint(gpui_base::GlobalState::record_native_overlay)
                            .w(px(360.))
                            .p_1()
                            .rounded_lg()
                            .shadow_lg()
                            .bg(p.surface)
                            .text_color(p.text)
                            .text_size(px(13.))
                            .children(rows),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::input::Undo;

    #[test]
    fn source_refresh_keeps_identity_across_pending_rows_and_reorders() {
        let query = LinkQuery {
            start: 2,
            caret: 4,
            query: "go".into(),
            kind: Kind::Note,
        };
        let a = row("Goals", "Goals", "");
        let b = row("Good", "Good", "");
        let mut state = State::default();
        publish(&mut state, query.clone(), vec![a.clone(), b.clone()]);
        state.open.as_mut().unwrap().selected = 1;
        publish(&mut state, query.clone(), Vec::new());
        assert!(
            state.open.is_none(),
            "pending rows cannot accept stale source"
        );
        // Multiple source edits may supersede workers before any rows return.
        publish(&mut state, query.clone(), Vec::new());
        publish(&mut state, query.clone(), vec![a.clone(), b.clone()]);
        assert_eq!(
            state.open.as_ref().unwrap().selected,
            1,
            "pending source refresh preserves the selected second row"
        );
        publish(&mut state, query.clone(), Vec::new());
        publish(&mut state, query.clone(), vec![b.clone(), a.clone()]);
        assert_eq!(state.open.as_ref().unwrap().selected, 0);
        assert_eq!(state.open.as_ref().unwrap().items[0].target, "Good");
        // If that choice disappears, select the first remaining current row.
        publish(&mut state, query.clone(), Vec::new());
        publish(&mut state, query.clone(), vec![a.clone()]);
        assert_eq!(state.open.as_ref().unwrap().items[0].target, "Goals");
        // A new trigger does not inherit the previous session's choice.
        let next = LinkQuery {
            start: 12,
            caret: 14,
            ..query
        };
        publish(&mut state, next, vec![b, a]);
        assert_eq!(state.open.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn painted_click_cannot_retarget_after_source_edit_or_async_reorder() {
        let query = LinkQuery {
            start: 2,
            caret: 4,
            query: "go".into(),
            kind: Kind::Note,
        };
        let source = SourceStamp {
            document: 7,
            generation: 1,
        };
        let intent = RowIntent {
            query: query.clone(),
            source,
            path: String::new(),
            target: "Good".into(),
        };
        let mut open = Open {
            query,
            items: vec![row("Goals", "Goals", ""), row("Good", "Good", "")],
            selected: 0,
        };
        assert_eq!(intent.choice(&open, source), Some(1), "positive control");
        open.items.swap(0, 1);
        assert_eq!(
            intent.choice(&open, source),
            Some(0),
            "identity follows a reorder"
        );
        assert_eq!(
            intent.choice(
                &open,
                SourceStamp {
                    generation: 2,
                    ..source
                }
            ),
            None
        );
        assert_eq!(
            intent.choice(
                &open,
                SourceStamp {
                    document: 8,
                    ..source
                }
            ),
            None
        );
        open.query.query.push('d');
        assert_eq!(intent.choice(&open, source), None, "another query");
        open.query = intent.query.clone();
        open.items.remove(0);
        assert_eq!(intent.choice(&open, source), None, "removed candidate");
    }

    /// `|` is the caret.
    fn at(text: &str) -> (String, usize) {
        let caret = text.find('|').unwrap();
        (text.replacen('|', "", 1), caret)
    }

    /// The caret is the last `|`, so an alias bar can come before it.
    fn query(text: &str) -> Option<String> {
        let caret = text.rfind('|').unwrap();
        let text = format!("{}{}", &text[..caret], &text[caret + 1..]);
        link_query(&text, caret).map(|q| q.query)
    }

    #[test]
    fn a_name_after_double_brackets_is_a_query() {
        assert_eq!(query("See [[|").as_deref(), Some(""));
        assert_eq!(query("See [[Pla|").as_deref(), Some("Pla"));
        assert_eq!(query("See [[Pla|]]").as_deref(), Some("Pla"));
        assert_eq!(query("See [[Work/Pl|an]] x").as_deref(), Some("Work/Pl"));
        assert_eq!(query("![[Diag|").as_deref(), Some("Diag"), "an embed too");
        assert_eq!(query("[[a]] and [[ש|").as_deref(), Some("ש"));
        // Positive controls for every refusal below.
        assert_eq!(query("[[Plan]] |"), None, "after a closed link");
        assert_eq!(query("[[Plan|alias|"), None, "in the alias");
        assert_eq!(
            query("[[Plan#Head|").as_deref(),
            Some("Head"),
            "in the heading"
        );
        assert_eq!(query("[[\nPlan|"), None, "another line");
        assert_eq!(query("Plan|"), None, "no brackets");
        assert_eq!(query("[Plan|"), None, "one bracket");
        assert_eq!(query("`[[Pla|`"), None, "inline code");
        assert_eq!(query("``` \n[[Pla|\n```"), None, "fenced code");
        assert_eq!(query("---\na: [[Pla|\n---\n"), None, "front matter");
        assert_eq!(
            query("`x` [[Pla|").as_deref(),
            Some("Pla"),
            "after a code span"
        );
        assert_eq!(
            query("```\nx\n```\n[[Pla|").as_deref(),
            Some("Pla"),
            "after a fence"
        );
        let long = format!("[[{}|", "a".repeat(MAX_QUERY_CHARS + 1));
        assert_eq!(query(&long), None);
    }

    fn vault(paths: &[&str]) -> Arc<Vault> {
        Arc::new(Vault::from_note_paths(paths.iter().map(|p| p.to_string())))
    }

    fn titles(found: &[Candidate]) -> Vec<&str> {
        found.iter().map(|c| c.path.as_str()).collect()
    }

    #[test]
    fn notes_rank_by_how_their_name_matches() {
        let index = Index::new(
            vault(&[
                "Archive/Old roadmap.md",
                "Road.md",
                "Roadmap 2027.md",
                "Projects/Railroad.md",
                "Projects/roadcanary/notes.md",
                "Unrelated.md",
                "Here.md",
                ".hidden/Road trip.md",
            ]),
            false,
        );
        assert_eq!(
            titles(&index.search("road", "Here.md")),
            [
                "Road.md",
                "Roadmap 2027.md",
                "Archive/Old roadmap.md",
                "Projects/Railroad.md",
                "Projects/roadcanary/notes.md",
            ]
        );
        assert_eq!(
            titles(&index.search("ROADM", "Here.md")).len(),
            2,
            "any case"
        );
        assert!(index.search("zzz", "Here.md").is_empty());
        // The note being edited is not offered; hidden folders stay hidden.
        assert!(titles(&index.search("here", "Here.md")).is_empty());
        assert_eq!(titles(&index.search("here", "Road.md")), ["Here.md"]);
        let shown = Index::new(index.vault.clone(), true);
        assert!(titles(&shown.search("trip", "Here.md")).contains(&".hidden/Road trip.md"));
        // Nothing typed: the note's own folder first, at most a page.
        let many: Vec<String> = (0..20).map(|n| format!("Other/n{n:02}.md")).collect();
        let mut paths: Vec<&str> = many.iter().map(String::as_str).collect();
        paths.extend(["Mine/Here.md", "Mine/Sibling.md"]);
        let found = Index::new(vault(&paths), false).search("", "Mine/Here.md");
        assert_eq!(found.len(), 21);
        assert_eq!(found[0].path, "Mine/Sibling.md");
        assert_eq!(found[0].folder, "Mine");
    }

    #[test]
    fn the_target_is_the_shortest_unambiguous_name() {
        let v = vault(&[
            "Work/Plan.md",
            "Home/Plan.md",
            "Work/Solo.md",
            "a/b/Deep.md",
        ]);
        assert_eq!(target_for(&v, "Work/Solo.md", ""), "Solo");
        assert_eq!(target_for(&v, "Work/Plan.md", ""), "Work/Plan");
        assert_eq!(target_for(&v, "Home/Plan.md", ""), "Home/Plan");
        assert_eq!(target_for(&v, "a/b/Deep.md", ""), "Deep");
        let found = Index::new(v.clone(), false).search("plan", "Work/Solo.md");
        assert_eq!(
            found.iter().map(|c| c.target.as_str()).collect::<Vec<_>>(),
            ["Home/Plan", "Work/Plan"]
        );
        // Every written target resolves back to the chosen note (positive control).
        for c in &found {
            assert!(matches!(
                v.resolve_from_ref(&c.target, "Work/Solo.md"),
                okilum_core::vault::ResolutionRef::Resolved(p) if p == c.path
            ));
        }
    }

    fn complete(text: &str, target: &str) -> String {
        let (text, caret) = at(text);
        let query = link_query(&text, caret).unwrap();
        let (edits, caret) = completion(&text, &query, target);
        let mut out = lists::apply(&text, &edits);
        out.insert(caret, '|');
        out
    }

    #[test]
    fn completion_writes_the_name_and_closes_the_link_once() {
        assert_eq!(complete("See [[Pl|", "Plan"), "See [[Plan]]|");
        assert_eq!(complete("See [[Pl|]] more", "Plan"), "See [[Plan]]| more");
        assert_eq!(complete("[[Pl|xx]]", "Work/Plan"), "[[Work/Plan]]|");
        assert_eq!(complete("[[Pl|an|alias]]", "Plan"), "[[Plan||alias]]");
        assert_eq!(complete("[[Pl|#Goals]]", "Plan"), "[[Plan|#Goals]]");
        assert_eq!(
            complete("[[Pl| rest of line", "Plan"),
            "[[Plan]]| rest of line"
        );
        assert_eq!(complete("[[Pl|\r\nnext", "Plan"), "[[Plan]]|\r\nnext");
        assert_eq!(complete("[[ש|", "שלום"), "[[שלום]]|");
    }

    #[test]
    fn headings_use_unsaved_source_and_never_choose_ambiguous_targets() {
        let v = vault(&["note.md", "a/Plan.md", "b/Plan.md"]);
        let (text, caret) = at("# Goals\n\n[[#Go|");
        let q = link_query(&text, caret).unwrap();
        let rows = enrich(&v, "note.md", Some(&text), &q, Vec::new());
        assert_eq!(
            rows.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(),
            ["#Goals"]
        );
        let (edits, _) = completion(&text, &q, &rows[0].target);
        assert_eq!(lists::apply(&text, &edits), "# Goals\n\n[[#Goals]]");
        let ambiguous = link_query("[[Plan#Go", 9).unwrap();
        assert!(enrich(&v, "note.md", Some(&text), &ambiguous, Vec::new()).is_empty());
        let duplicate = "# Goals\n# Goals\n[[#Go";
        let q = link_query(duplicate, duplicate.len()).unwrap();
        assert!(enrich(&v, "note.md", Some(duplicate), &q, Vec::new()).is_empty());
        for marked in ["` :smi|`", "```\n/ta|\n```", "---\ntype: :smi|\n---"] {
            let (text, caret) = at(marked);
            assert!(link_query(&text, caret).is_none(), "{marked}");
        }
    }

    #[test]
    fn note_rows_read_type_metadata_and_heading_rows_read_the_resolved_note() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("Plan.md"),
            "---\ntype: project\n---\n# Goals\n",
        )
        .unwrap();
        let mut v = Vault::from_note_paths(["Plan.md".into(), "note.md".into()]);
        v.root = directory.path().into();
        let v = Arc::new(v);
        let q = link_query("[[Pla", 5).unwrap();
        let candidates = Index::new(v.clone(), false).search("Pla", "note.md");
        let rows = enrich(&v, "note.md", None, &q, candidates);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].badge.as_deref(), Some("project"));
        let q = link_query("[[Plan#Go", 9).unwrap();
        let rows = enrich(&v, "note.md", None, &q, Vec::new());
        assert_eq!(rows[0].target, "Plan#Goals");
        let q = link_query("/dat", 4).unwrap();
        let rows = catalog(&q);
        assert_eq!(rows[0].title, "date");
        assert!(chrono::NaiveDate::parse_from_str(&rows[0].target, "%Y-%m-%d").is_ok());
    }

    struct Note {
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        _directory: tempfile::TempDir,
    }

    fn open(cx: &mut TestAppContext) -> (Note, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        for folder in ["Projects", "Archive"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(root.join("note.md"), "\n").unwrap();
        for note in ["Recipes.md", "Projects/Roadmap.md", "Archive/Roadmap.md"] {
            std::fs::write(root.join(note), "# x\n").unwrap();
        }
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
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
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        (
            Note {
                reader,
                input,
                _directory: directory,
            },
            visual,
        )
    }

    fn set(note: &Note, visual: &mut VisualTestContext, text: &str) {
        let (text, caret) = at(text);
        note.input
            .update_in(visual, |i, window, cx| i.set_value(text, window, cx));
        note.input
            .update(visual, |i, cx| i.set_selected_range(caret..caret, cx));
        visual.run_until_parked();
    }

    fn shown(note: &Note, visual: &mut VisualTestContext) -> String {
        note.input.read_with(visual, |i, _| {
            let mut text = i.value().to_string();
            text.insert(i.selected_range().start, '|');
            text
        })
    }

    fn typed(visual: &mut VisualTestContext, text: &str) {
        for ch in text.chars() {
            visual.simulate_input(&ch.to_string());
            visual.run_until_parked();
        }
    }

    fn keys(visual: &mut VisualTestContext, keys: &str) {
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
    }

    /// The rows on offer, by path; `None` while the list is closed.
    fn offered(note: &Note, visual: &mut VisualTestContext) -> Option<Vec<String>> {
        note.reader.read_with(visual, |r, _| {
            let open = r.editing.as_ref()?.link_complete.open.as_ref()?;
            Some(open.items.iter().map(|c| c.path.clone()).collect())
        })
    }

    fn undo(visual: &mut VisualTestContext) {
        visual.dispatch_action(Undo);
        visual.run_until_parked();
    }

    #[gpui::test]
    fn source_changes_with_the_same_query_cannot_accept_old_headings(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "# Old goals\n\n[[#|");
        assert!(
            offered(&note, visual).is_some(),
            "positive control: headings loaded"
        );
        note.input.update_in(visual, |input, window, cx| {
            let text = "# New goals\n\n[[#";
            input.set_value(text, window, cx);
            input.set_selected_range(text.len()..text.len(), cx);
        });
        // Before queued refresh/worker publication, the query and caret are
        // unchanged but the candidate belongs to a different source revision.
        note.reader.update_in(visual, |reader, window, cx| {
            assert!(reader.link_complete_input(window, cx).is_none());
            reader.link_complete_accept(None, Trigger::Tab, window, cx);
        });
        assert_eq!(shown(&note, visual), "# New goals\n\n[[#|");
        visual.run_until_parked();
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "# New goals\n\n[[#New goals]]|");
    }

    #[gpui::test]
    fn keyboard_reaches_catalog_tail_in_the_same_popup(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for source in [":|", "```r|", "/|"] {
            set(&note, visual, source);
            let target = note.reader.read_with(visual, |reader, _| {
                let open = reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .link_complete
                    .open
                    .as_ref()
                    .unwrap();
                open.items.last().unwrap().target.clone()
            });
            keys(visual, "up");
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let last = note.reader.read_with(visual, |reader, _| {
                let open = reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .link_complete
                    .open
                    .as_ref()
                    .unwrap();
                assert_eq!(open.selected, open.items.len() - 1);
                open.selected
            });
            assert!(
                visual
                    .debug_bounds(&format!("link-complete-{last}"))
                    .is_some()
            );
            keys(visual, "tab");
            assert!(shown(&note, visual).contains(&target));
            undo(visual);
            assert_eq!(shown(&note, visual), source);
        }
    }

    #[gpui::test]
    fn providers_share_keyboard_acceptance_and_one_step_undo(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for (source, expected) in [
            ("Hello :smi|", "Hello 😄|"),
            ("```rus|\n\n```", "```rust|\n\n```"),
            ("/tas|", "- [ ] |"),
            ("# Goals\n\n[[#Go|", "# Goals\n\n[[#Goals]]|"),
        ] {
            set(&note, visual, source);
            assert!(offered(&note, visual).is_some(), "{source}");
            keys(visual, "tab");
            assert_eq!(shown(&note, visual), expected);
            undo(visual);
            assert_eq!(shown(&note, visual), source, "one Undo step");
        }
        set(&note, visual, "/tas|");
        keys(visual, "escape");
        assert!(offered(&note, visual).is_none());
        typed(visual, "k");
        assert!(
            offered(&note, visual).is_none(),
            "dismissed until leaving this command"
        );
    }

    #[gpui::test]
    fn typing_a_link_name_offers_notes_and_enter_writes_the_link(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "See |");
        assert_eq!(
            offered(&note, visual),
            None,
            "positive control: no `[[` yet"
        );
        typed(visual, "[[");
        // Nothing typed yet: every other note is offered, not this one.
        let all = offered(&note, visual).expect("`[[` opens the list");
        assert_eq!(all.len(), 3);
        assert!(!all.contains(&"note.md".to_owned()));
        typed(visual, "rec");
        assert_eq!(offered(&note, visual), Some(vec!["Recipes.md".into()]));
        // The list is painted under the caret.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let list = visual
            .debug_bounds("link-complete")
            .expect("the list is painted");
        let caret = note.input.read_with(visual, |i, _| {
            let c = i.cursor();
            i.range_to_bounds(&(c..c)).unwrap()
        });
        assert!(list.top() >= caret.bottom(), "{list:?} below {caret:?}");
        keys(visual, "enter");
        // Brackets may have closed themselves; either way one `]]` closes it.
        assert_eq!(shown(&note, visual), "See [[Recipes]]|");
        assert_eq!(offered(&note, visual), None);
        undo(visual);
        let undone = shown(&note, visual);
        assert!(undone.starts_with("See [[rec|"), "one Undo step: {undone}");

        // Namesakes: the folder tells them apart and is written into the link.
        set(&note, visual, "|");
        typed(visual, "[[road");
        assert_eq!(
            offered(&note, visual),
            Some(vec![
                "Archive/Roadmap.md".into(),
                "Projects/Roadmap.md".into()
            ])
        );
        keys(visual, "down");
        assert_eq!(
            shown(&note, visual).matches('|').count(),
            1,
            "the caret stays"
        );
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "[[Projects/Roadmap]]|");

        // A click on a row writes that row's link.
        set(&note, visual, "|");
        typed(visual, "[[road");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let row = visual
            .debug_bounds("link-complete-1")
            .expect("the second row is painted");
        visual.simulate_click(row.center(), Modifiers::none());
        visual.run_until_parked();
        assert_eq!(shown(&note, visual), "[[Projects/Roadmap]]|");

        // Escape closes the list for this link: Enter is a new line again.
        set(&note, visual, "|");
        typed(visual, "[[rec");
        assert!(offered(&note, visual).is_some());
        keys(visual, "escape");
        assert_eq!(offered(&note, visual), None);
        typed(visual, "i");
        assert_eq!(offered(&note, visual), None, "stays closed for this link");
        keys(visual, "enter");
        assert!(
            shown(&note, visual).contains("\n"),
            "Enter typed a new line"
        );

        // No match: nothing opens, and typing a new name stays plain.
        set(&note, visual, "|");
        typed(visual, "[[Brand new");
        assert_eq!(offered(&note, visual), None);

        // Up and Down outside the list still move the caret (positive control).
        set(&note, visual, "one\ntw|o");
        keys(visual, "up");
        assert_eq!(shown(&note, visual), "on|e\ntwo");
        assert!(std::fs::read_to_string(note._directory.path().join("vault/note.md")).is_ok());
    }
}
