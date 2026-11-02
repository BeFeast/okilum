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

// Source Undo may restore a format in a table before the currently open one.
// Preserve the active table's location only when the changed span is strictly
// before it; overlapping edits keep the existing classifier admission path.
pub(super) fn restored_start(before: &str, after: &str, start: usize) -> usize {
    match (touched(after, before), touched(before, after)) {
        (Some(old), Some(new)) if old.end < start => start - old.len() + new.len(),
        _ => start,
    }
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
    /// Clipboard Applied follows the exact API's Silent source event. Claim
    /// its preimage now, then defer past the Reader callback's entity borrow.
    /// A later buffer revision invalidates this intent before it can arm.
    pub(in crate::reader_editor) fn claim_paste_edit(
        &mut self,
        stamp: SourceStamp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, before)) = self
            .pending_silent
            .take()
            .filter(|(pending, _)| *pending == stamp)
        else {
            return;
        };
        cx.defer_in(window, move |this, window, cx| {
            if this.input.read(cx).source_stamp() != stamp {
                return;
            }
            this.seen_document = before;
            this.source_changed(
                SourceMutation {
                    stamp,
                    reason: SourceMutationReason::Edit,
                },
                window,
                cx,
            );
        });
    }

    pub(super) fn source_changed(
        &mut self,
        event: SourceMutation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.idle = None;
        self.pending_silent = None;
        // Several native transactions may publish before subscribers run.
        // Only the event for the current stamp advances the observed preimage.
        if self.input.read(cx).source_stamp() != event.stamp {
            return;
        }
        // Compare an IME commit with the last committed source, not its
        // preedit. A candidate can commit with identical bytes to preedit.
        if event.reason == SourceMutationReason::Preedit {
            cx.notify();
            return;
        }
        // Persistent Rope clones keep whole-note copying/scanning off the
        // typing path shared with Live Preview layout (#1679).
        let text = self.input.read(cx).text().clone();
        let before = std::mem::replace(&mut self.seen_document, text.clone());
        cx.notify();
        let owned_range = self
            .committed_table_edit
            .take()
            .filter(|(stamp, _)| *stamp == event.stamp)
            .map(|(_, range)| range);
        let owned_edit = owned_range.is_some();
        if self.formatted_stamp.take() == Some(event.stamp) {
            return;
        }
        if event.reason != SourceMutationReason::Edit && !owned_edit {
            if event.reason == SourceMutationReason::Silent {
                self.pending_silent = Some((event.stamp, before));
            }
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
        if text.len() > source_classifier::MAX_BYTES {
            return;
        }
        self.idle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(PAUSE).await;
            let candidate = cx
                .background_executor()
                .spawn(async move {
                    let before = before.to_string();
                    let text = text.to_string();
                    // A structural deletion can leave its minimal diff just
                    // beyond the final table newline. Use the accepted table
                    // command's exact postimage intent, never an adjacent table
                    // inferred from an unrelated buffer edit.
                    let touched = owned_range.or_else(|| touched(&before, &text))?;
                    let range = table_range(&text, touched)?;
                    Some((text, range))
                })
                .await;
            if let Some((text, range)) = candidate {
                let _ = cx.update(|window, app| {
                    this.update(app, |this, cx| {
                        this.format_idle(event.stamp, &text, range, window, cx);
                    })
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
        // Only disjoint leases can be relocated. Never retain old cell spans
        // after replacing an overlapping, differently classified table range.
        if self.binding.as_ref().is_some_and(|binding| {
            binding.table.start < range.end && range.start < binding.table.end
        }) {
            return;
        }
        // A click into another table is not a buffer mutation. Format the
        // touched table and relocate the other grid's lease without rebuilding
        // its native inputs or discarding pending composition/text.
        if self.binding.is_some() && (!self.current(cx) || self.retain_pending_cells(cx)) {
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
        let replacement_len = change.source.len();
        let edited_range = range.clone();
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
            let state = self.input.read(cx);
            let after = state.source_stamp();
            if let Some(binding) = &mut self.binding {
                self.source_history.record(stamp, after, None);
                if edited_range.end <= binding.table.start {
                    binding.table = binding.table.start - edited_range.len() + replacement_len
                        ..binding.table.end - edited_range.len() + replacement_len;
                }
                binding.stamp = after;
                binding.document = state.value().to_string();
                self.installed_epoch = None;
            }
            self.formatted_stamp = Some(after);
            cx.notify();
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
            // Both fixture tables have the same row/column debug selectors.
            // Bind the first table's exact current revision explicitly; the
            // existing grid click regression covers the native entry route.
            let range = SOURCE.find("| Имя").unwrap()..SOURCE.find("\n\nUntouched").unwrap();
            let stamp = input.read_with(visual, |state, _| state.source_stamp());
            grid.update_in(visual, |grid, window, cx| {
                assert!(grid.open_cell(range.clone(), stamp, &SOURCE[range], 1, 0, window, cx));
            });
        } else {
            let at = SOURCE.find("old").unwrap();
            input.update(visual, |state, cx| state.set_selected_range(at..at + 3, cx));
        }
        visual.run_until_parked();
        if grid_mode {
            assert!(
                visual.debug_bounds("editable-table-grid").is_some(),
                "positive control: persistent grid mounted"
            );
        }
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
        if grid_mode {
            visual.simulate_input("Привет😀");
        } else {
            // One committed replacement supplies one prior source Undo entry;
            // the grid path above exercises grouping across real keystrokes.
            input.update_in(visual, |state, window, cx| {
                state.replace_text_in_range(None, "Привет😀", window, cx);
            });
        }
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
            assert!(
                visual.debug_bounds("editable-table-grid").is_some(),
                "formatting keeps the editable grid painted"
            );
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
            assert!(
                visual.debug_bounds("editable-table-grid").is_some(),
                "right-click keeps the cell editor mounted"
            );
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
            match index {
                1 => assert_eq!(changed.lines().count(), SOURCE.lines().count() + 1),
                2 => {
                    assert_eq!(changed.lines().count(), SOURCE.lines().count() - 1);
                    assert!(!changed.contains("old"));
                }
                3 => assert!(changed.contains("| Имя | | 值 |")),
                4 => {
                    assert!(!changed.contains("Имя"));
                    assert!(changed.contains("😀") && changed.contains("東京"));
                }
                6 => assert!(changed.contains("|:--:|--:|")),
                7 => assert!(changed.contains("|--:|--:|")),
                8 => assert!(changed.contains("| :--- | ---: |")),
                _ => {}
            }
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

    #[gpui::test]
    fn changing_grid_focus_keeps_the_old_table_format_and_the_new_cell_caret(
        cx: &mut TestAppContext,
    ) {
        let (_reader, input, grid, visual, _directory) = open(cx, true);
        visual.simulate_input("Привет😀");
        visual.run_until_parked();
        let typed = value(&input, visual);
        let at = typed.find("| keep").unwrap();
        let range = table_range(&typed, at..at).unwrap();
        let stamp = input.read_with(visual, |state, _| state.source_stamp());
        grid.update_in(visual, |grid, window, cx| {
            assert!(grid.open_cell(range.clone(), stamp, &typed[range], 1, 1, window, cx));
        });
        visual.run_until_parked();
        let cell = grid.read_with(visual, |grid, _| grid.cells[1][1].clone());
        let caret = cell.read_with(visual, |cell, _| cell.selected_range());
        wait(visual, 800);
        let formatted = value(&input, visual);
        assert_ne!(
            formatted, typed,
            "positive control: previous edited table formats after focus moved"
        );
        assert!(formatted.ends_with(&SOURCE[SOURCE.find("\n\nUntouched").unwrap()..]));
        assert!(
            visual.debug_bounds("editable-table-grid").is_some(),
            "the relocated grid remains painted"
        );
        assert_eq!(
            grid.read_with(visual, |grid, _| grid.cells[1][1].clone()),
            cell
        );
        assert_eq!(
            cell.read_with(visual, |cell, _| cell.selected_range()),
            caret
        );
        visual.update(|window, cx| assert!(cell.read(cx).focus_handle(cx).is_focused(window)));
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(value(&input, visual), typed);
        assert_eq!(
            grid.read_with(visual, |grid, cx| grid.active_range(cx).unwrap().start),
            typed.find("| keep").unwrap()
        );
    }

    #[gpui::test]
    fn raw_ime_commit_identical_to_preedit_still_arms_the_formatter(cx: &mut TestAppContext) {
        let (_reader, input, _grid, visual, _directory) = open(cx, false);
        input.update_in(visual, |state, window, cx| {
            state.replace_and_mark_text_in_range(None, "日", None, window, cx);
        });
        visual.run_until_parked();
        let preedit = value(&input, visual);
        assert_eq!(
            preedit,
            SOURCE.replace("old", "日"),
            "positive control: real source preedit"
        );
        wait(visual, 900);
        assert_eq!(value(&input, visual), preedit, "preedit is never formatted");
        input.update_in(visual, |state, window, cx| {
            let marked = state.marked_text_range(window, cx);
            assert!(marked.is_some());
            state.replace_text_in_range(marked, "日", window, cx);
        });
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual),
            preedit,
            "commit keeps identical candidate bytes"
        );
        wait(visual, 800);
        assert_ne!(
            value(&input, visual),
            preedit,
            "positive control: the committed candidate formats"
        );
    }

    #[gpui::test]
    fn exact_clipboard_paste_formats_once_and_undo_never_rearms_it(cx: &mut TestAppContext) {
        use gpui_component::input::clipboard::{
            ClipboardReadError, ClipboardReadRequest, ExactClipboardProvider,
        };
        struct Exact;
        impl ExactClipboardProvider for Exact {
            fn read_text_now(&self, _: &App) -> Option<Result<Option<String>, ClipboardReadError>> {
                Some(Ok(Some("Привет😀".into())))
            }
            fn read_text(
                &self,
                _: ClipboardReadRequest,
                _: &App,
            ) -> Task<Result<Option<String>, ClipboardReadError>> {
                Task::ready(Ok(Some("Привет😀".into())))
            }
        }
        let (_reader, input, _grid, visual, _directory) = open(cx, false);
        input.update_in(visual, |state, window, cx| {
            state.set_exact_clipboard_provider(Some(Arc::new(Exact)), cx);
            state.focus_handle(cx).focus(window, cx);
            window.activate_window();
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("secondary-v");
        visual.run_until_parked();
        let pasted = SOURCE.replace("old", "Привет😀");
        assert_eq!(
            value(&input, visual),
            pasted,
            "positive control: exact paste was applied"
        );
        wait(visual, 800);
        assert_ne!(
            value(&input, visual),
            pasted,
            "the committed Silent paste arms formatting"
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual),
            pasted,
            "one Undo removes only the format"
        );
        wait(visual, 900);
        assert_eq!(
            value(&input, visual),
            pasted,
            "an unclaimed Silent Undo never rearms"
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual),
            SOURCE,
            "the paste keeps its original atomic Undo"
        );
    }

    fn exercise_final_row_deletion(cx: &mut TestAppContext, grid_mode: bool) {
        let (_reader, input, grid, visual, _directory) = open(cx, grid_mode);
        if grid_mode {
            grid.update_in(visual, |grid, window, cx| {
                grid.focus_cell(2, 0, window, cx);
            });
        } else {
            let at = SOURCE.find("next").unwrap();
            input.update(visual, |state, cx| state.set_selected_range(at..at + 4, cx));
        }
        visual.dispatch_action(TableDeleteRow);
        visual.run_until_parked();
        let deleted = value(&input, visual);
        assert!(
            !deleted.contains("next"),
            "positive control: final row deleted"
        );
        assert!(
            !deleted.contains("| 😀 |\n\n\nUntouched"),
            "no additional blank line appears"
        );
        wait(visual, 800);
        assert_ne!(
            value(&input, visual),
            deleted,
            "the deleted table still formats"
        );
        if grid_mode {
            assert!(visual.debug_bounds("editable-table-grid").is_some());
        }
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(value(&input, visual), deleted, "format is one Undo step");
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            value(&input, visual),
            SOURCE,
            "row deletion is one earlier Undo step"
        );
    }

    #[gpui::test]
    fn deleting_the_final_row_keeps_the_idle_grid_lease_and_atomic_undo(cx: &mut TestAppContext) {
        exercise_final_row_deletion(cx, true);
    }

    #[gpui::test]
    fn deleting_the_final_source_row_still_formats_the_touched_table(cx: &mut TestAppContext) {
        exercise_final_row_deletion(cx, false);
    }

    #[gpui::test]
    fn literal_header_block_markers_stay_a_table_before_save_and_after_idle(
        cx: &mut TestAppContext,
    ) {
        let (reader, input, grid, visual, directory) = open(cx, false);
        let bare = SOURCE
            .replace("| Имя | 值 |", "Имя | 值")
            .replace("|:--|--:|", ":--|--:")
            .replace("| old | 😀 |", "old | 😀")
            .replace("| next | 東京 |", "next | 東京");
        input.update_in(visual, |state, window, cx| {
            state.set_value(bare.clone(), window, cx)
        });
        visual.run_until_parked();
        let at = bare.find("Имя").unwrap();
        let range = table_range(&bare, at..at).expect("positive control: bare table admitted");
        let stamp = input.read_with(visual, |state, _| state.source_stamp());
        grid.update_in(visual, |grid, window, cx| {
            assert!(grid.open_cell(range.clone(), stamp, &bare[range], 0, 0, window, cx));
        });
        visual.run_until_parked();
        visual.simulate_input("# Заголовок😀");
        visual.run_until_parked();
        let typed = value(&input, visual);
        let at = typed.find("# Заголовок").unwrap();
        assert!(
            table_range(&typed, at..at).is_some(),
            "literal header text cannot turn the file into a heading"
        );
        reader.update(visual, |reader, cx| assert!(reader.save_source(cx)));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("vault/note.md")).unwrap(),
            typed
        );
        let cell = grid.read_with(visual, |grid, _| grid.cells[0][0].clone());
        wait(visual, 800);
        let formatted = value(&input, visual);
        assert_ne!(
            formatted, typed,
            "positive control: the same table formats after idle"
        );
        assert!(formatted.contains("| # Заголовок😀"));
        assert_eq!(
            grid.read_with(visual, |grid, _| grid.cells[0][0].clone()),
            cell
        );
        assert!(visual.debug_bounds("editable-table-grid").is_some());
    }
}
