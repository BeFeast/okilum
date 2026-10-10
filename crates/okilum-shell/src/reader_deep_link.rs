//! Landing for external `okilum:` links (#1049, docs/deep-links.md): a line
//! scrolls the Reader to its block or puts the editor's caret on it; a
//! `#heading` or `#^block` uses the wikilink landing. A link never switches
//! between Reader and editing.

use super::*;
use gpui_component::input::RopeExt as _;
use okilum_core::deep_link::Position;

/// The editor caret a link places once its note is open.
pub(crate) struct EditLanding {
    /// The note preparation it waits for; `None` waits for the window's
    /// restored editor (a new window that was left editing).
    generation: Option<u64>,
    rel: String,
    line: Option<u32>,
    column: Option<u32>,
    fragment: bool,
}

const FRAGMENT_IN_EDITOR: &str =
    "Headings and blocks are found in the reading view. The note is open for editing.";

impl Reader {
    /// Open `rel` at a link's position. A fragment and a line together: the
    /// line wins. While editing, the caret moves without asking: unsaved
    /// edits stay, nothing is lost.
    pub(crate) fn open_link(
        &mut self,
        rel: &str,
        position: Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fragment = position
            .heading
            .clone()
            .or_else(|| position.block.as_ref().map(|block| format!("^{block}")));
        let line = position.line;
        if self.editing.is_some() {
            if self.selected_file() == rel {
                match line {
                    Some(line) => self.move_caret(line, position.column, window, cx),
                    None if fragment.is_some() => {
                        self.link_notice = Some(FRAGMENT_IN_EDITOR.into());
                        cx.notify();
                    }
                    None => {}
                }
                return;
            }
            // Another note: save this one, open the link's note, then edit it.
            self.open_note_landing(rel, None, None, None, window, cx);
            if self.editing.is_none() {
                self.pending_link_edit = Some(EditLanding {
                    generation: Some(self.navigation.preparation_generation),
                    rel: rel.to_owned(),
                    line,
                    column: position.column,
                    fragment: fragment.is_some(),
                });
            }
            return;
        }
        let fragment = fragment.filter(|_| line.is_none());
        self.open_note_landing(rel, None, fragment.as_deref(), line, window, cx);
    }

    /// Called after a prepared note is accepted: an editing link returns to
    /// the editor on the link's note.
    pub(crate) fn finish_link_edit(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(landing) = self
            .pending_link_edit
            .take_if(|l| l.generation == Some(generation))
        else {
            return;
        };
        if self.editing.is_some() || self.current_rel != landing.rel {
            return;
        }
        self.toggle_source(window, cx);
        self.land_in_editor(landing, window, cx);
    }

    /// A new window opened by a link: it opens as the vault was left. In the
    /// Reader the link lands now; a window restoring its editor lands the
    /// caret once the editor is back (`restore_ui_source`).
    pub(crate) fn open_first_link(
        &mut self,
        position: Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A bare note link is already where it should be.
        if position == Position::default() {
            return;
        }
        let rel = self.current_rel.clone();
        if self.ui_state.source.is_none() {
            self.open_link(&rel, position, window, cx);
            return;
        }
        // The saved editor scroll belonged to the last session, not the link.
        self.ui_state.source = Some([0.; 2]);
        self.ui_state.source_reader_position = None;
        self.pending_link_edit = Some(EditLanding {
            generation: None,
            fragment: position.heading.is_some() || position.block.is_some(),
            rel,
            line: position.line,
            column: position.column,
        });
    }

    /// `restore_ui_source` brought the editor back.
    pub(crate) fn land_restored_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(landing) = self.pending_link_edit.take_if(|l| l.generation.is_none()) {
            if landing.rel == self.current_rel {
                self.land_in_editor(landing, window, cx);
            }
        }
    }

    fn land_in_editor(
        &mut self,
        landing: EditLanding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_none() {
            return;
        }
        match landing.line {
            Some(line) => self.move_caret(line, landing.column, window, cx),
            None if landing.fragment => {
                self.link_notice = Some(FRAGMENT_IN_EDITOR.into());
                cx.notify();
            }
            None => {}
        }
    }

    /// Caret at a 1-based `line:column` (column in characters), scrolled into
    /// view. Out of range clamps to the last line or column.
    fn move_caret(
        &mut self,
        line: u32,
        column: Option<u32>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        let Some(input) = self.source_input() else {
            return;
        };
        let line = line.saturating_sub(1);
        let character = column.unwrap_or(1).saturating_sub(1);
        // The editor scrolls only once it has a layout.
        window.on_next_frame(move |window, cx| {
            input.update(cx, |input, cx| {
                let last = input.text().lines_len().saturating_sub(1) as u32;
                input.set_cursor_position(
                    gpui_component::input::Position {
                        line: line.min(last),
                        character,
                    },
                    window,
                    cx,
                );
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::input::RopeExt as _;
    use std::time::Duration;

    fn at(line: u32, column: Option<u32>) -> Position {
        Position {
            line: Some(line),
            column,
            ..Default::default()
        }
    }

    #[gpui::test]
    fn links_land_on_lines_in_reader_and_editor(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("okilum-link-landing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // Paragraph i is block i-1 at file line 2i+2 (three frontmatter lines).
        let body: String = (1..=60).map(|i| format!("Paragraph {i}.\n\n")).collect();
        let target = format!("---\ntitle: Target\n---\n{body}");
        std::fs::write(root.join("target.md"), &target).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\nFirst line.\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
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
        visual.simulate_resize(size(px(1000.), px(600.)));
        visual.run_until_parked();
        let settle = |visual: &mut gpui::VisualTestContext| {
            visual.run_until_parked();
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
        };
        let top =
            |v: &Reader, cx: &App| v.content.read(cx).list_state().logical_scroll_top().item_ix;

        // Reader: line 82 is paragraph 40, block 39.
        reader.update_in(visual, |v, window, cx| {
            v.open_link("target.md", at(82, None), window, cx)
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            assert_eq!(v.current_rel, "target.md");
            assert!(v.editing.is_none(), "a link never enters the editor");
            assert!(v.link_notice.is_none());
            assert_eq!(top(v, cx), 39);
        });
        // Positive control: the same note without a line opens at the top.
        reader.update_in(visual, |v, window, cx| {
            v.open_link("start.md", Position::default(), window, cx)
        });
        settle(visual);
        reader.update_in(visual, |v, window, cx| {
            v.open_link("target.md", Position::default(), window, cx)
        });
        settle(visual);
        reader.read_with(visual, |v, cx| assert_eq!(top(v, cx), 0));

        // Editor, same note, unsaved edit: the caret moves, the edit stays.
        reader.update_in(visual, |v, window, cx| {
            v.toggle_source(window, cx);
            let edited = target.replacen("Paragraph 1.", "Paragraph one.", 1);
            v.editing.as_ref().unwrap().set_value(&edited, window, cx);
            v.open_link("target.md", at(10, Some(4)), window, cx);
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            assert_eq!(v.editing_caret_line(cx), Some(10));
            let input = v.source_input().unwrap();
            let input = input.read(cx);
            assert_eq!(
                input.cursor_position().character,
                3,
                "column counts characters"
            );
            assert!(
                input.value().contains("Paragraph one."),
                "unsaved edit kept"
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("target.md")).unwrap(),
            target
        );
        // Out of range clamps to the last line.
        reader.update_in(visual, |v, window, cx| {
            v.open_link("target.md", at(9999, None), window, cx)
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            let last = v.source_input().unwrap().read(cx).text().lines_len();
            assert_eq!(v.editing_caret_line(cx), Some(last));
        });

        // Another note while editing: saved, opened, still in the editor.
        reader.update_in(visual, |v, window, cx| {
            v.open_link("start.md", at(3, Some(6)), window, cx)
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            assert_eq!(v.current_rel, "start.md");
            assert!(v.editing.is_some(), "the editor stays the mode");
            assert_eq!(v.editing_caret_line(cx), Some(3));
        });
        assert!(std::fs::read_to_string(root.join("target.md"))
            .unwrap()
            .contains("Paragraph one."));
        std::fs::remove_dir_all(root).unwrap();
    }
}
