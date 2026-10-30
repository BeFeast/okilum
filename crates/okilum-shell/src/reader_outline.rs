//! Outline clicks target the visible document surface, including Source/Live.
use super::*;
use gpui_component::input::EditorState;
use okilum_core::document_links::{HeadingEntry, HeadingInventory};

const STALE_HEADING: &str =
    "This heading changed in the editor. Return to Read to refresh the outline.";

/// Match the clicked Reader heading to authored source, then to the current
/// draft. Body/frontmatter edits may shift lines without changing heading
/// identity. Changed duplicate headings are surfaced instead of guessed.
fn editor_line(
    original: &str,
    rendered: &str,
    current: &str,
    heading: &HeadingEntry,
) -> Result<u32, &'static str> {
    let line = okilum_core::deep_link::block_line(original, rendered, heading.target.block)
        .ok_or("This heading cannot be mapped to the editable source.")?;
    let line_at = |text: &str, offset: usize| -> Option<u32> {
        u32::try_from(
            text.get(..offset)?
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
        )
        .ok()
    };
    let same = |a: &HeadingEntry, b: &HeadingEntry| {
        a.text == b.text && a.level == b.level && a.supported_container == b.supported_container
    };
    let before = HeadingInventory::new(original).entries;
    let index = before
        .iter()
        .position(|entry| {
            entry.supported_container
                && line_at(original, entry.target.offset) == line.checked_sub(1)
        })
        .ok_or(STALE_HEADING)?;
    let now = HeadingInventory::new(current).entries;
    let target = if before.len() == now.len() && before.iter().zip(&now).all(|(a, b)| same(a, b)) {
        &now[index]
    } else {
        let mut matches = now.iter().filter(|entry| same(entry, &before[index]));
        let target = matches.next().ok_or(STALE_HEADING)?;
        if matches.next().is_some()
            || before
                .iter()
                .filter(|entry| same(entry, &before[index]))
                .count()
                != 1
        {
            return Err(STALE_HEADING);
        }
        target
    };
    line_at(current, target.target.offset).ok_or(STALE_HEADING)
}

impl Reader {
    pub(super) fn open_outline_heading(
        &mut self,
        heading: HeadingEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = &self.editing else {
            self.scroll_to_block(heading.target.block, cx);
            self.content
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
            return;
        };
        let input = editing.input().clone();
        let Some(original) = self.note_canonical_source.clone() else {
            return;
        };
        let rendered = self.note_source.clone();
        let current = input.read(cx).value();
        let untouched = (
            input.read(cx).source_stamp(),
            input.read(cx).selected_range(),
        );
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        self.cancel_pending_landing();
        let request = self.navigation.landing_generation;
        input.read(cx).focus_handle(cx).focus(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { editor_line(&original, &rendered, &current, &heading) })
                .await;
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                if this.navigation.landing_generation != request
                    || this.vault_root != root
                    || this.current_rel != rel
                    || this
                        .editing
                        .as_ref()
                        .is_none_or(|editing| editing.input() != &input)
                    || (
                        input.read(cx).source_stamp(),
                        input.read(cx).selected_range(),
                    ) != untouched
                {
                    return;
                }
                let line = match result {
                    Ok(line) => line,
                    Err(reason) => {
                        this.link_notice = Some(reason.into());
                        cx.notify();
                        return;
                    }
                };
                this.link_notice = None;
                let after = input.update(cx, |input, cx| {
                    place(input, line, window, cx);
                    (input.source_stamp(), input.selected_range())
                });
                // A caret can scroll only after layout. A subsequent click,
                // note/mode change, typed edit or caret move cancels this handoff.
                let reader = cx.weak_entity();
                window.on_next_frame(move |window, cx| {
                    let _ = reader.update(cx, |reader, cx| {
                        if reader.navigation.landing_generation != request
                            || reader.vault_root != root
                            || reader.current_rel != rel
                            || reader
                                .editing
                                .as_ref()
                                .is_none_or(|editing| editing.input() != &input)
                            || (
                                input.read(cx).source_stamp(),
                                input.read(cx).selected_range(),
                            ) != after
                        {
                            return;
                        }
                        input.update(cx, |input, cx| place(input, line, window, cx));
                    });
                });
                cx.notify();
            });
        })
        .detach();
    }
}

fn place(input: &mut EditorState, line: u32, window: &mut Window, cx: &mut Context<EditorState>) {
    input.set_cursor_position(
        gpui_component::input::Position { line, character: 0 },
        window,
        cx,
    );
    input.scroll_line_to_top(line as usize, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn authored_heading_lines_follow_dirty_body_edits_and_refuse_changed_duplicates() {
        let original = "---\r\ntitle: Synthetic\r\n---\r\n# Start\r\n\r\nBody\r\n\r\nЦель 🎯\r\n-------\r\n\r\nTail\r\n";
        let rendered = okilum_core::render::without_frontmatter(original);
        let heading = HeadingInventory::new(rendered).entries[1].clone();
        assert_eq!(editor_line(original, rendered, original, &heading), Ok(7));
        let changed = original.replace("Body\r\n", "Body\r\n\r\nExtra Unicode Ω paragraph\r\n");
        assert_eq!(editor_line(original, rendered, &changed, &heading), Ok(9));
        let removed = changed.replace("Цель 🎯", "Renamed");
        assert!(editor_line(original, rendered, &removed, &heading).is_err());
        let duplicate = "# Same\n\nFirst\n\n# Same\n\nSecond\n";
        let second = HeadingInventory::new(duplicate).entries[1].clone();
        assert_eq!(editor_line(duplicate, duplicate, duplicate, &second), Ok(4));
        let body_change = duplicate.replace("First", "First\n\nAnother paragraph");
        assert_eq!(
            editor_line(duplicate, duplicate, &body_change, &second),
            Ok(6)
        );
        let removed_first = duplicate.replacen("# Same", "# Different", 1);
        assert!(editor_line(duplicate, duplicate, &removed_first, &second).is_err());
        // Reader link rewriting can change heading text without changing its
        // authored line. Identity comes from the mapped block, not that label.
        let wiki = "# [[Target|Alias]]\n\nBody\n";
        let rendered = "# [Alias](Target.md)\n\nBody\n";
        let heading = HeadingInventory::new(rendered).entries[0].clone();
        assert_eq!(editor_line(wiki, rendered, wiki, &heading), Ok(0));
    }

    #[derive(Clone, Copy, Debug)]
    enum Mode {
        Read,
        Source,
        Live,
    }

    fn settle(visual: &mut gpui::VisualTestContext) {
        visual.run_until_parked();
        // Deliver the finite layout handoff just as a native display does.
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
    }

    fn click_heading(cx: &mut TestAppContext, mode: Mode) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let original = format!("---\r\ntitle: Synthetic Ω\r\n---\r\n# Top\r\n\r\n{}Цель 🎯\r\n-------\r\n\r\n{}## End\r\n\r\n{}", "A paragraph before the target.\r\n\r\n".repeat(80), "A paragraph after the target.\r\n\r\n".repeat(80), "Ending paragraph.\r\n\r\n".repeat(30));
        let path = root.join("note.md");
        std::fs::write(&path, &original).unwrap();
        let target_offset = original.find("Цель 🎯").unwrap();
        let target_line = original[..target_offset]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u32;
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(fixture.path().join("state")),
                        index_dir: Some(fixture.path().join("index")),
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
        visual.simulate_resize(size(px(1400.), px(960.)));
        settle(visual);
        let target_block = reader.update_in(visual, |reader, window, cx| {
            assert_eq!(
                reader.outline.len(),
                3,
                "outline inventory positive control"
            );
            assert_eq!(reader.outline[1].text, "Цель 🎯");
            reader.panels.open(reader_layout::Panel::Backlinks);
            match mode {
                Mode::Read => {}
                Mode::Source => reader.toggle_source(window, cx),
                Mode::Live => reader.open_live_preview(window, cx),
            }
            cx.notify();
            reader.outline[1].target.block
        });
        settle(visual);
        let input = reader.read_with(visual, |reader, _| {
            reader
                .editing
                .as_ref()
                .map(|editing| editing.input().clone())
        });
        reader.read_with(visual, |reader, _| {
            assert_eq!(
                input.is_some(),
                !matches!(mode, Mode::Read),
                "positive control: requested editing mode opened"
            );
            assert_eq!(
                reader.live_preview_shown(),
                matches!(mode, Mode::Live),
                "positive control: actual Live Preview state"
            );
        });
        if let Some(input) = &input {
            input.update_in(visual, |input, window, cx| place(input, 0, window, cx));
            settle(visual);
            input.read_with(visual, |input, _| {
                assert_eq!(input.cursor_position().line, 0);
                assert!(
                    input.scroll_offset().y >= px(-1.),
                    "positive control: editor starts at the top"
                );
            });
        }
        let row = visual
            .debug_bounds("reader-outline-row-1")
            .expect("real outline row is drawn");
        visual.simulate_click(row.center(), Modifiers::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        settle(visual);
        reader.update_in(visual, |reader, window, cx| {
            assert_eq!(reader.current_rel, "note.md");
            assert!(
                reader.link_notice.is_none(),
                "{mode:?}: {:?}",
                reader.link_notice
            );
            if let Some(input) = &input {
                let input = input.read(cx);
                assert_eq!(
                    input.cursor_position().line,
                    target_line,
                    "{mode:?}: authored CRLF/frontmatter heading line"
                );
                assert_eq!(input.cursor_position().character, 0);
                assert!(
                    input.scroll_offset().y < px(-100.),
                    "{mode:?}: visible editor must actually scroll"
                );
                assert_eq!(
                    input.top_visible_line(),
                    Some(target_line as usize),
                    "{mode:?}: heading reaches the top of the visible editor"
                );
                assert!(input.focus_handle(cx).is_focused(window));
                assert_eq!(input.value().as_ref(), original);
                assert_eq!(
                    reader
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top()
                        .item_ix,
                    0,
                    "{mode:?}: the hidden Reader is not the navigation target"
                );
            } else {
                assert_eq!(
                    reader
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top()
                        .item_ix,
                    target_block,
                    "Read retains its block landing"
                );
                assert!(reader.editing.is_none());
            }
        });
        if let Some(input) = &input {
            // Both jobs begin only after this update returns. The older result
            // must not win even if background completion order changes.
            reader.update_in(visual, |reader, window, cx| {
                reader.open_outline_heading(reader.outline[0].clone(), window, cx);
                reader.open_outline_heading(reader.outline[2].clone(), window, cx);
            });
            settle(visual);
            let end_line = original[..original.find("## End").unwrap()]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            input.read_with(visual, |input, _| {
                assert_eq!(input.cursor_position().line as usize, end_line);
                assert_eq!(input.top_visible_line(), Some(end_line));
            });
            // A user caret move while parsing is pending cancels the landing.
            reader.update_in(visual, |reader, window, cx| {
                reader.open_outline_heading(reader.outline[1].clone(), window, cx);
                input.update(cx, |input, cx| place(input, 0, window, cx));
            });
            settle(visual);
            input.read_with(visual, |input, _| {
                assert_eq!(input.cursor_position().line, 0);
                assert_eq!(input.top_visible_line(), Some(0));
                assert_eq!(input.value().as_ref(), original);
            });
        }
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            original,
            "navigation does not edit the note"
        );
    }

    #[gpui::test]
    fn outline_pointer_click_scrolls_source_and_places_caret(cx: &mut TestAppContext) {
        click_heading(cx, Mode::Source);
    }
    #[gpui::test]
    fn outline_pointer_click_scrolls_live_preview_and_places_caret(cx: &mut TestAppContext) {
        click_heading(cx, Mode::Live);
    }
    #[gpui::test]
    fn outline_pointer_click_preserves_read_navigation(cx: &mut TestAppContext) {
        click_heading(cx, Mode::Read);
    }
}
