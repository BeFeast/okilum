//! Link autocomplete in the note editor (#1921). Typing `[[` and the start of
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
use super::lists::{self, Edit};
use super::*;
use gpui_component::input::{Enter, Escape, IndentInline, MoveDown, MoveUp, RopeExt as _};
use std::ops::Range;

/// Rows the list shows at most.
pub(super) const MAX_ROWS: usize = 8;
/// A longer name is no longer being typed as a link.
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
}

/// The link being typed at `caret`, if any.
pub(super) fn link_query(text: &str, caret: usize) -> Option<LinkQuery> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let line_start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
    let before = &text[line_start..caret];
    let open = before.rfind("[[")?;
    let start = line_start + open + 2;
    let query = &text[start..caret];
    if query.contains(NAME_END) || query.chars().count() > MAX_QUERY_CHARS {
        return None;
    }
    // An odd number of backticks before the `[[` on this line: inline code.
    if before[..open].matches('`').count() % 2 == 1 {
        return None;
    }
    if lists::in_code(text, line_start) {
        return None;
    }
    Some(LinkQuery {
        start,
        caret,
        query: query.to_owned(),
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
            .take(MAX_ROWS)
            .map(|(_, _, _, at)| {
                let note = &self.vault.notes[at];
                Candidate {
                    path: note.path.clone(),
                    title: note.title.clone(),
                    folder: folder_of(&note.path).to_owned(),
                    target: target_for(&self.vault, &note.path, from),
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
    if title == query {
        Some(0)
    } else if title.starts_with(query) {
        Some(1)
    } else if title
        .match_indices(query)
        .any(|(at, _)| title[..at].ends_with([' ', '-', '_', '.', '(']))
    {
        Some(2)
    } else if title.contains(query) {
        Some(3)
    } else if path.contains(query) {
        Some(4)
    } else {
        None
    }
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
    let tail = text[query.caret..]
        .find(|c: char| NAME_END.contains(&c) || c == '\n' || c == '\r')
        .map_or(text.len(), |at| query.caret + at);
    let rest = &text[tail..];
    if rest.starts_with("]]") {
        let caret = query.start + target.len() + 2;
        (vec![(query.start..tail, target.to_owned())], caret)
    } else if rest.starts_with(['|', '#', '^']) {
        let caret = query.start + target.len();
        (vec![(query.start..tail, target.to_owned())], caret)
    } else {
        let caret = query.start + target.len() + 2;
        (
            vec![(query.start..query.caret, format!("{target}]]"))],
            caret,
        )
    }
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
    seen: Option<(gpui_component::input::projection::SourceStamp, Range<usize>)>,
    pub open: Option<Open>,
    /// The `[[` whose list Escape closed (or a choice completed); it stays
    /// closed until the caret leaves that link.
    dismissed: Option<usize>,
    index: Option<Index>,
}

impl Reader {
    /// Recompute the list after the text or the caret changed.
    pub(super) fn refresh_link_complete(&mut self, cx: &mut Context<Self>) {
        let show_hidden = self.sidebar.show_hidden;
        let vault = self.vault.clone();
        let from = self.current_rel.clone();
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let input = editing.input.read(cx);
        let seen = (input.source_stamp(), input.selected_range());
        if editing.link_complete.seen.as_ref() == Some(&seen) {
            return;
        }
        let selection = seen.1.clone();
        editing.link_complete.seen = Some(seen);
        let query = (editing.markdown
            && selection.is_empty()
            && input.is_editable()
            && !input.ime_composing())
        .then(|| {
            // Most carets are on a line without `[[`: read only that line.
            let row = input.cursor_position().line as usize;
            input
                .text()
                .slice_line(row)
                .to_string()
                .contains("[[")
                .then(|| link_query(&input.value(), selection.start))
                .flatten()
        })
        .flatten();
        let state = &mut editing.link_complete;
        let was_open = state.open.is_some();
        let Some(query) = query else {
            state.dismissed = None;
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
            state.index = Some(Index::new(vault, show_hidden));
        }
        let items = state.index.as_ref().unwrap().search(&query.query, &from);
        // The same rows keep the highlighted one.
        let selected = state
            .open
            .as_ref()
            .and_then(|open| {
                items
                    .get(open.selected)
                    .filter(|item| **item == open.items[open.selected])
                    .map(|_| open.selected)
            })
            .unwrap_or(0);
        state.open = (!items.is_empty()).then_some(Open {
            query,
            items,
            selected,
        });
        cx.notify();
    }

    /// The editor while its link list is open and may take keys.
    fn link_complete_input(&self, window: &Window, cx: &App) -> Option<Entity<EditorState>> {
        let editing = self.editing.as_ref()?;
        editing.link_complete.open.as_ref()?;
        let input = editing.input.read(cx);
        (input.focus_handle(cx).is_focused(window) && input.is_editable())
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
        let rows = open.items.iter().enumerate().map(|(i, item)| {
            let selected = i == open.selected;
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
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.link_complete_accept(Some(i), Trigger::Text, window, cx);
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
        assert_eq!(query("[[Plan#Head|"), None, "in the heading");
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
        assert_eq!(found.len(), MAX_ROWS);
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
