//! Only a committed edit can arm formatting. Every mutation cancels the old
//! task; both the document revision and its bytes are checked after background
//! classification. Caret-only movement does not authorize another table.
use super::*;
use gpui_component::input::projection::SourceMutationReason;

const PAUSE: std::time::Duration = std::time::Duration::from_millis(750);

fn touched(before: &str, after: &str) -> Option<Range<usize>> {
    if before == after {
        return None;
    }
    let start = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let suffix = before[start..]
        .chars()
        .rev()
        .zip(after[start..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    Some(start..after.len() - suffix)
}

fn table_range(text: &str, touched: Range<usize>) -> Option<Range<usize>> {
    if text.len() > source_classifier::MAX_BYTES {
        return None;
    }
    let snapshot = Snapshot::new("table-idle", 0, text);
    let classified = source_classifier::classify(&snapshot);
    classified
        .decorations_for(&snapshot)
        .ok()?
        .iter()
        .find(|marker| {
            marker.kind == Kind::Table
                && marker.range.start <= touched.start
                && touched.end <= marker.range.end
        })
        .map(|marker| marker.range.clone())
}

impl TableGrid {
    pub(super) fn source_changed(
        &mut self,
        event: SourceMutation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.idle = None;
        // Several native transactions may publish before subscribers run.
        // Only the event for the current stamp advances the observed preimage.
        if self.input.read(cx).source_stamp() != event.stamp {
            return;
        }
        let text = self.input.read(cx).value().to_string();
        let before = std::mem::replace(&mut self.seen_document, text.clone());
        cx.notify();
        if self.formatted_stamp.take() == Some(event.stamp)
            || event.reason != SourceMutationReason::Edit
        {
            return;
        }
        let Some(reader) = self.reader.upgrade() else {
            return;
        };
        if !reader.read(cx).editing.as_ref().is_some_and(|editing| {
            editing.input == self.input && editing.markdown && editing.live_preview.enabled
        }) {
            return;
        }
        let Some(touched) = touched(&before, &text) else {
            return;
        };
        self.idle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(PAUSE).await;
            let document = text.clone();
            let range = cx
                .background_executor()
                .spawn(async move { table_range(&document, touched) })
                .await;
            if let Some(range) = range {
                let _ = this.update_in_own_window(cx, |this, window, cx| {
                    this.format_idle(event.stamp, &text, range, window, cx);
                });
            }
        }));
    }

    fn format_idle(
        &mut self,
        stamp: SourceStamp,
        text: &str,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.input.read(cx);
        if state.source_stamp() != stamp
            || state.value().as_ref() != text
            || !state.is_editable()
            || state.ime_composing()
            || state.ime_settling(cx)
        {
            return;
        }
        let Some(reader) = self.reader.upgrade() else {
            return;
        };
        if !reader.read(cx).editing.as_ref().is_some_and(|editing| {
            editing.input == self.input && editing.markdown && editing.live_preview.enabled
        }) {
            return;
        }
        if self
            .binding
            .as_ref()
            .is_some_and(|binding| binding.table == range)
        {
            if !self.current(cx) || self.retain_pending_cells(cx) {
                return;
            }
            // Keep the same native inputs, caret, selection and authored spaces.
            let Some((cells, change)) = self
                .binding
                .as_ref()
                .unwrap()
                .cells
                .format(self.selected.0, self.selected.1)
            else {
                return;
            };
            if change.source == text[range] {
                return;
            }
            if self.apply(change, None, window, cx) {
                self.binding.as_mut().unwrap().cells = cells;
                self.formatted_stamp = Some(self.input.read(cx).source_stamp());
            }
            return;
        }
        // A different open grid owns native input. Do not invalidate its lease.
        if self.binding.is_some() {
            return;
        }
        let selection = self.input.read(cx).selected_range();
        let source = &text[range.clone()];
        let Some(table) = model::Table::parse(source) else {
            return;
        };
        let inside = range.start <= selection.start && selection.end <= range.end;
        let relative = if inside {
            selection.start - range.start..selection.end - range.start
        } else {
            table.ranges[0][0].clone()
        };
        let Some(change) = model::edit(source, relative, Command::Format) else {
            return;
        };
        if change.source == source {
            return;
        }
        let map = |at: usize| {
            if at >= range.end {
                at - range.len() + change.source.len()
            } else {
                at
            }
        };
        let selection = if inside {
            range.start + change.selection.start..range.start + change.selection.end
        } else if selection.start < range.end && selection.end > range.start {
            return;
        } else {
            map(selection.start)..map(selection.end)
        };
        let input = self.input.clone();
        if reader.update(cx, |reader, cx| {
            reader.apply_list_edit(
                &input,
                ListEdit {
                    edits: vec![(range, change.source)],
                    selection,
                    revert: None,
                },
                window,
                cx,
            )
        }) {
            self.formatted_stamp = Some(self.input.read(cx).source_stamp());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    const SOURCE: &str = "Before\n\n| Имя | 值 |\n|:--|--:|\n| old | 😀 |\n| next | 東京 |\n\nUntouched\n\n| keep | this |\n|--|--|\n| byte | exact |\n";

    fn open(
        cx: &mut TestAppContext,
        grid_mode: bool,
    ) -> (
        Entity<Reader>,
        Entity<EditorState>,
        Entity<TableGrid>,
        &mut VisualTestContext,
        tempfile::TempDir,
    ) {
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), SOURCE).unwrap();
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
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader.set_live_preview(true, window, cx);
        });
        visual.run_until_parked();
        let (input, grid) = reader.read_with(visual, |reader, _| {
            let editing = reader.editing.as_ref().unwrap();
            (editing.input.clone(), editing.table_grid.clone())
        });
        if grid_mode {
            let bounds = visual
                .debug_bounds("table-cell-1-0")
                .expect("positive control: rendered cell");
            visual.simulate_click(bounds.center(), Modifiers::default());
        } else {
            let at = SOURCE.find("old").unwrap();
            input.update(visual, |state, cx| state.set_selected_range(at..at + 3, cx));
        }
        visual.run_until_parked();
        (reader, input, grid, visual, directory)
    }

    fn wait(visual: &mut VisualTestContext, millis: u64) {
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(millis));
        visual.run_until_parked();
    }

    fn value(input: &Entity<EditorState>, visual: &mut VisualTestContext) -> String {
        input.read_with(visual, |state, _| state.value().to_string())
    }

    fn exercise_idle(cx: &mut TestAppContext, grid_mode: bool) {
        let (reader, input, grid, visual, directory) = open(cx, grid_mode);
        wait(visual, 900);
        assert_eq!(
            value(&input, visual),
            SOURCE,
            "opening or focusing never formats untouched source"
        );
        visual.simulate_input("Привет😀");
        visual.run_until_parked();
        let typed = SOURCE.replace("old", "Привет😀");
        assert_eq!(
            value(&input, visual),
            typed,
            "positive control: edit reached canonical source"
        );
        let cell = if grid_mode {
            Some(grid.read_with(visual, |grid, _| grid.cells[1][0].clone()))
        } else {
            None
        };
        let selected = cell
            .as_ref()
            .map(|cell| cell.read_with(visual, |cell, _| cell.selected_range()));
        wait(visual, 600);
        assert_eq!(value(&input, visual), typed, "before the deadline");
        wait(visual, 200);
        let formatted = value(&input, visual);
        assert_ne!(formatted, typed, "positive control: idle formatter fired");
        let untouched = &SOURCE[SOURCE.find("\n\nUntouched").unwrap()..];
        assert!(formatted.ends_with(untouched));
        assert!(
            formatted.contains("| :------- | ---: |"),
            "display-width delimiter: {formatted}"
        );
        if let Some(cell) = cell {
            assert_eq!(
                grid.read_with(visual, |grid, _| grid.cells[1][0].clone()),
                cell
            );
            assert_eq!(
                cell.read_with(visual, |cell, _| cell.selected_range()),
                selected.unwrap()
            );
            visual.update(|window, cx| assert!(cell.read(cx).focus_handle(cx).is_focused(window)));
        } else {
            let caret = input.read_with(visual, |state, _| state.selected_range());
            assert!(caret.is_empty());
            assert!(formatted[..caret.start].ends_with("Привет😀"));
        }
        reader.update(visual, |reader, cx| assert!(reader.save_source(cx)));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("vault/note.md")).unwrap(),
            formatted,
            "safe save writes the canonical formatted draft"
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual),
            typed,
            "formatting is exactly one Undo step"
        );
        wait(visual, 900);
        assert_eq!(
            value(&input, visual),
            typed,
            "Undo does not rearm formatting"
        );
        visual.dispatch_action(Redo);
        visual.run_until_parked();
        assert_eq!(value(&input, visual), formatted);
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual),
            SOURCE,
            "typing keeps its own Undo group"
        );
    }

    #[gpui::test]
    fn live_grid_idle_format_preserves_native_caret_and_source_undo(cx: &mut TestAppContext) {
        exercise_idle(cx, true);
    }

    #[gpui::test]
    fn live_source_idle_format_preserves_cell_caret_and_source_undo(cx: &mut TestAppContext) {
        exercise_idle(cx, false);
    }

    #[gpui::test]
    fn a_new_buffer_edit_cancels_the_old_format_and_stale_revisions_are_refused(
        cx: &mut TestAppContext,
    ) {
        let (_reader, input, grid, visual, _directory) = open(cx, false);
        visual.simulate_input("Привет😀");
        visual.run_until_parked();
        let typed = value(&input, visual);
        let stale = input.read_with(visual, |state, _| state.source_stamp());
        let range = table_range(
            &typed,
            typed.find("Привет").unwrap()..typed.find("Привет").unwrap(),
        )
        .unwrap();
        wait(visual, 500);
        // A real edit outside any table cancels the pending table format.
        input.update(visual, |state, cx| state.set_selected_range(0..0, cx));
        visual.simulate_input("changed ");
        visual.run_until_parked();
        wait(visual, 900);
        let changed = format!("changed {typed}");
        assert_eq!(value(&input, visual), changed);
        grid.update_in(visual, |grid, window, cx| {
            grid.format_idle(stale, &typed, range, window, cx)
        });
        assert_eq!(value(&input, visual), changed, "stale worker refused");
        let at = changed.find("Привет").unwrap();
        input.update(visual, |state, cx| state.set_selected_range(at..at, cx));
        visual.simulate_input("x");
        visual.run_until_parked();
        let typed = value(&input, visual);
        wait(visual, 800);
        assert_ne!(
            value(&input, visual),
            typed,
            "positive control: a new table edit arms a current format"
        );
    }

    #[gpui::test]
    fn an_edit_and_undo_to_identical_bytes_still_cancels_the_idle_revision(
        cx: &mut TestAppContext,
    ) {
        let (_reader, input, grid, visual, _directory) = open(cx, false);
        visual.simulate_input("Привет😀");
        visual.run_until_parked();
        let typed = value(&input, visual);
        let stale = input.read_with(visual, |state, _| state.source_stamp());
        let at = typed.find("Привет").unwrap();
        let range = table_range(&typed, at..at).unwrap();
        wait(visual, 100);
        input.update(visual, |state, cx| state.set_selected_range(0..0, cx));
        visual.simulate_input("x");
        visual.run_until_parked();
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual),
            typed,
            "positive control: Undo restored exactly the old bytes"
        );
        assert_ne!(
            input.read_with(visual, |state, _| state.source_stamp()),
            stale
        );
        grid.update_in(visual, |grid, window, cx| {
            grid.format_idle(stale, &typed, range, window, cx)
        });
        wait(visual, 900);
        assert_eq!(
            value(&input, visual),
            typed,
            "old revision never formats an ABA buffer"
        );
        input.update(visual, |state, cx| state.set_selected_range(at..at, cx));
        visual.simulate_input("y");
        visual.run_until_parked();
        let current = value(&input, visual);
        wait(visual, 800);
        assert_ne!(
            value(&input, visual),
            current,
            "positive control: current revision formats"
        );
    }

    #[gpui::test]
    fn grid_row_keys_skip_header_move_rows_append_and_undo(cx: &mut TestAppContext) {
        let (_reader, input, grid, visual, _directory) = open(cx, true);
        visual.simulate_keystrokes("alt-down");
        visual.run_until_parked();
        let moved = value(&input, visual);
        assert!(
            moved.find("next").unwrap() < moved.find("old").unwrap(),
            "positive control: Alt+Down moved data row"
        );
        visual.simulate_keystrokes("alt-up");
        visual.run_until_parked();
        assert_eq!(value(&input, visual), SOURCE, "Alt+Up restores the row");
        visual.simulate_keystrokes("alt-up");
        visual.run_until_parked();
        assert_eq!(value(&input, visual), SOURCE, "header is never moved");
        visual.simulate_keystrokes("secondary-enter");
        visual.run_until_parked();
        assert_eq!(
            grid.read_with(visual, |grid, _| grid.cells.len()),
            4,
            "Cmd/Ctrl+Enter inserts row"
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(value(&input, visual), SOURCE);
        grid.update_in(visual, |grid, window, cx| {
            grid.focus_cell(0, 1, window, cx);
        });
        visual.simulate_keystrokes("tab");
        visual.run_until_parked();
        assert_eq!(
            grid.read_with(visual, |grid, _| grid.selected),
            (1, 0),
            "Tab skips delimiter"
        );
        visual.simulate_keystrokes("shift-tab");
        visual.run_until_parked();
        assert_eq!(grid.read_with(visual, |grid, _| grid.selected), (0, 1));
        grid.update_in(visual, |grid, window, cx| {
            grid.focus_cell(2, 1, window, cx);
        });
        visual.simulate_keystrokes("tab");
        visual.run_until_parked();
        assert_eq!(
            grid.read_with(visual, |grid, _| grid.cells.len()),
            4,
            "final Tab appends"
        );
        assert_eq!(grid.read_with(visual, |grid, _| grid.selected), (3, 0));
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(value(&input, visual), SOURCE);
    }

    #[gpui::test]
    fn raw_table_row_keys_and_final_tab_use_the_source_history(cx: &mut TestAppContext) {
        let (_reader, input, _grid, visual, _directory) = open(cx, false);
        visual.simulate_keystrokes("alt-down");
        visual.run_until_parked();
        let moved = value(&input, visual);
        assert!(
            moved.find("next").unwrap() < moved.find("old").unwrap(),
            "positive control: source row key fired"
        );
        visual.simulate_keystrokes("alt-up");
        visual.run_until_parked();
        assert_eq!(value(&input, visual), SOURCE);
        visual.simulate_keystrokes("secondary-enter");
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual).lines().count(),
            SOURCE.lines().count() + 1
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(value(&input, visual), SOURCE);
        let end = SOURCE.find("東京").unwrap() + "東京".len();
        input.update(visual, |state, cx| state.set_selected_range(end..end, cx));
        visual.simulate_keystrokes("tab");
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual).lines().count(),
            SOURCE.lines().count() + 1
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(value(&input, visual), SOURCE);
    }

    #[gpui::test]
    fn grid_preedit_blocks_idle_format_and_row_keys_until_commit(cx: &mut TestAppContext) {
        let (_reader, input, grid, visual, _directory) = open(cx, true);
        visual.simulate_input("Привет");
        visual.run_until_parked();
        let typed = value(&input, visual);
        let cell = grid.read_with(visual, |grid, _| grid.cells[1][0].clone());
        cell.update_in(visual, |state, window, cx| {
            state.replace_and_mark_text_in_range(None, "に", None, window, cx)
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("alt-down");
        visual.run_until_parked();
        wait(visual, 900);
        assert_eq!(
            value(&input, visual),
            typed,
            "preedit neither formats nor moves a row"
        );
        cell.update_in(visual, |state, window, cx| {
            let marked = state.marked_text_range(window, cx);
            assert!(marked.is_some(), "positive control: native preedit exists");
            state.replace_text_in_range(marked, "日", window, cx);
        });
        visual.run_until_parked();
        let committed = value(&input, visual);
        assert_ne!(
            committed, typed,
            "positive control: committed text reaches source"
        );
        wait(visual, 800);
        assert_ne!(
            value(&input, visual),
            committed,
            "committed text rearms idle format"
        );
        visual.simulate_keystrokes("alt-down");
        visual.run_until_parked();
        let moved = value(&input, visual);
        assert!(moved.find("next").unwrap() < moved.find("Привет").unwrap());
    }

    #[cfg(target_os = "linux")]
    #[gpui::test]
    fn grid_context_menu_row_column_alignment_actions_round_trip_through_undo(
        cx: &mut TestAppContext,
    ) {
        let (_reader, input, grid, visual, _directory) = open(cx, true);
        for index in 1..=8 {
            let cell = grid.read_with(visual, |grid, _| grid.cells[1][0].clone());
            let at = cell.read_with(visual, |cell, _| cell.input_bounds().center());
            visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
            visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
            visual.run_until_parked();
            visual.simulate_keystrokes("up");
            visual.run_until_parked();
            visual.simulate_keystrokes("right");
            visual.run_until_parked();
            for _ in 0..index {
                visual.simulate_keystrokes("down");
                visual.run_until_parked();
            }
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            let changed = value(&input, visual);
            // Left alignment is already authored in SOURCE, so it is a no-op.
            if index != 5 {
                assert_ne!(
                    changed, SOURCE,
                    "positive control: native Table action {index} fired"
                );
                visual.dispatch_action(Undo);
                visual.run_until_parked();
            }
            assert_eq!(
                value(&input, visual),
                SOURCE,
                "Table action {index} uses source Undo"
            );
        }
    }
}
