//! Table commands in the existing note editor (#1624). Commands transform
//! exact source through the same Undo and safe-save path as list edits.
mod grid;
mod model;
pub(super) use grid::TableGrid;

use super::ime_guard::Trigger;
use super::lists::ListEdit;
use super::*;
use gpui_component::input::{IndentInline, OutdentInline};
use model::{Alignment, Command};
use okilum_core::source_classifier::{self, decorations::Kind};
use okilum_core::source_projection::Snapshot;
use std::ops::Range;

gpui::actions!(
    okilum,
    [
        TableEditGrid,
        TableInsertRow,
        TableMoveRowUp,
        TableMoveRowDown,
        TableDeleteRow,
        TableInsertColumn,
        TableDeleteColumn,
        TableAlignLeft,
        TableAlignCenter,
        TableAlignRight,
        TableFormat,
    ]
);

pub(super) fn ensure_keys(cx: &mut App) {
    struct Bound;
    impl Global for Bound {}
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    for context in ["ReaderSource > Input", "TableGrid > Input"] {
        cx.bind_keys([
            KeyBinding::new("secondary-enter", TableInsertRow, Some(context)),
            KeyBinding::new("alt-up", TableMoveRowUp, Some(context)),
            KeyBinding::new("alt-down", TableMoveRowDown, Some(context)),
        ]);
    }
}

pub(super) fn menu() -> Menu {
    Menu {
        name: "Table".into(),
        disabled: false,
        items: vec![
            MenuItem::action("Edit table as grid", TableEditGrid),
            MenuItem::separator(),
            MenuItem::action("Insert row below", TableInsertRow),
            MenuItem::action("Delete row", TableDeleteRow),
            MenuItem::action("Insert column after", TableInsertColumn),
            MenuItem::action("Delete column", TableDeleteColumn),
            MenuItem::separator(),
            MenuItem::action("Align column left", TableAlignLeft),
            MenuItem::action("Align column center", TableAlignCenter),
            MenuItem::action("Align column right", TableAlignRight),
            MenuItem::separator(),
            MenuItem::action("Format table", TableFormat),
        ],
    }
}

/// Use a fresh classification of the exact editor draft. A table-shaped code
/// fence, HTML block, quote or frontmatter must never acquire table commands.
fn change(text: &str, selection: Range<usize>, command: Command) -> Option<ListEdit> {
    if text.len() > source_classifier::MAX_BYTES
        || selection.start > selection.end
        || selection.end > text.len()
    {
        return None;
    }
    let snapshot = Snapshot::new("table-command", 0, text);
    let classified = source_classifier::classify(&snapshot);
    let table = classified
        .decorations_for(&snapshot)
        .ok()?
        .iter()
        .find(|marker| {
            marker.kind == Kind::Table
                && marker.range.start <= selection.start
                && selection.end <= marker.range.end
        })?;
    let start = table.range.start;
    let updated = model::edit(
        text.get(table.range.clone())?,
        selection.start - start..selection.end - start,
        command,
    )?;
    Some(ListEdit {
        edits: vec![(table.range.clone(), updated.source)],
        selection: start + updated.selection.start..start + updated.selection.end,
        revert: None,
    })
}

impl Reader {
    fn open_table_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ime_allows(Trigger::Tab, cx) {
            return;
        }
        let Some(input) = self.list_input(window, cx) else {
            return;
        };
        let state = input.read(cx);
        let text = state.value().to_string();
        let selection = state.selected_range();
        let stamp = state.source_stamp();
        if text.len() > source_classifier::MAX_BYTES {
            return;
        }
        let snapshot = Snapshot::new("open-table-grid", 0, text.as_str());
        let classified = source_classifier::classify(&snapshot);
        let range = classified
            .decorations_for(&snapshot)
            .ok()
            .and_then(|markers| {
                markers
                    .iter()
                    .find(|marker| {
                        marker.kind == Kind::Table
                            && marker.range.start <= selection.start
                            && selection.end <= marker.range.end
                    })
                    .map(|marker| marker.range.clone())
            });
        let Some(range) = range else {
            reader_toast::error(
                "Place the caret in a regular Markdown table to edit its cells.",
                window,
                cx,
            );
            return;
        };
        let Some(table) = model::Table::parse(&text[range.clone()]) else {
            reader_toast::error(
                "This table needs exact Source editing before it can open as a grid.",
                window,
                cx,
            );
            return;
        };
        let target = table
            .ranges
            .iter()
            .enumerate()
            .find_map(|(r, row)| {
                row.iter()
                    .position(|cell| {
                        range.start + cell.start <= selection.start
                            && selection.start <= range.start + cell.end
                    })
                    .map(|c| (r, c))
            })
            .unwrap_or((0, 0));
        let grid = self.editing.as_ref().unwrap().table_grid.clone();
        self.set_live_preview(true, window, cx);
        // Run with App after the Reader action borrow ends. Grid admission
        // reads the current Reader owner and rechecks the captured revision.
        window.on_next_frame(move |window, cx| {
            grid.update(cx, |grid, cx| {
                grid.open_cell(
                    range.clone(),
                    stamp,
                    &text[range],
                    target.0,
                    target.1,
                    window,
                    cx,
                );
            });
        });
        cx.stop_propagation();
    }

    fn table_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        // Completion, IME, focus and read-only checks match the note commands.
        if !self.ime_allows(Trigger::Tab, cx) {
            return;
        }
        let Some(input) = self.list_input(window, cx) else {
            return;
        };
        let (text, selection) = {
            let state = input.read(cx);
            (state.value(), state.selected_range())
        };
        let Some(edit) = change(&text, selection, command) else {
            if !matches!(command, Command::Next | Command::Previous) {
                reader_toast::error(
                    "Place the caret in a regular table cell. Table commands require equally sized rows; the header row and final column cannot be deleted.",
                    window,
                    cx,
                );
            }
            return;
        };
        // Navigation across an already formatted table has no text mutation
        // and must not add a spurious Undo record.
        if edit
            .edits
            .iter()
            .all(|(range, replacement)| text.get(range.clone()) == Some(replacement.as_str()))
        {
            input.update(cx, |state, cx| state.set_selected_range(edit.selection, cx));
            cx.stop_propagation();
        } else if self.apply_list_edit(&input, edit, window, cx) {
            let state = input.read(cx);
            let stamp = state.source_stamp();
            let selection = state.selected_range();
            if let Some(editing) = &self.editing {
                editing
                    .table_grid
                    .update(cx, |grid, _| grid.mark_source_edit(stamp, selection));
            }
            cx.stop_propagation();
        }
    }

    pub(super) fn table_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .on_action(
                cx.listener(|this, _: &TableEditGrid, window, cx| this.open_table_grid(window, cx)),
            )
            // Only actual Tab, not the Cmd/Ctrl+] indentation action.
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                this.table_command(Command::Next, window, cx)
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                this.table_command(Command::Previous, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableInsertRow, window, cx| {
                this.table_command(Command::InsertRow, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableMoveRowUp, window, cx| {
                this.table_command(Command::MoveRowUp, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableMoveRowDown, window, cx| {
                this.table_command(Command::MoveRowDown, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableDeleteRow, window, cx| {
                this.table_command(Command::DeleteRow, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableInsertColumn, window, cx| {
                this.table_command(Command::InsertColumn, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableDeleteColumn, window, cx| {
                this.table_command(Command::DeleteColumn, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableAlignLeft, window, cx| {
                this.table_command(Command::Align(Alignment::Left), window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableAlignCenter, window, cx| {
                this.table_command(Command::Align(Alignment::Center), window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableAlignRight, window, cx| {
                this.table_command(Command::Align(Alignment::Right), window, cx)
            }))
            .on_action(cx.listener(|this, _: &TableFormat, window, cx| {
                this.table_command(Command::Format, window, cx)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn only_a_current_top_level_table_accepts_commands() {
        let table = "| A | B |\n|---|---|\n| one | two |";
        let note = format!("Before\n\n{table}\n\nAfter\n");
        let at = note.find("one").unwrap();
        let edit = change(&note, at..at, Command::Next).expect("positive control: actual table");
        let mut edited = note.clone();
        for (range, text) in edit.edits.iter().rev() {
            edited.replace_range(range.clone(), text);
        }
        assert_eq!(&edited[edit.selection], "two");
        assert!(edited.starts_with("Before\n\n"));
        assert!(edited.ends_with("\n\nAfter\n"));
        for note in [
            format!("```md\n{table}\n```"),
            format!("~~~\n{table}\n~~~"),
            format!("---\ntitle: x\n{table}\n---\n"),
            format!("<div>\n{table}\n</div>"),
            table.lines().map(|line| format!("> {line}\n")).collect(),
        ] {
            let at = note.find("one").unwrap();
            assert!(
                change(&note, at..at, Command::Next).is_none(),
                "literal or container source: {note:?}"
            );
        }
    }

    fn exercise_keys(cx: &mut TestAppContext, live: bool) {
        use gpui_component::input::Undo;
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let source = "Before\n\n| A | B |\n|---|---|\n| one | two |\n\nAfter\n";
        std::fs::write(root.join("note.md"), source).unwrap();
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
            reader.set_live_preview(live, window, cx);
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input.clone()
        });
        let at = source.find("one").unwrap();
        input.update(visual, |input, cx| input.set_selected_range(at..at, cx));
        visual.simulate_keystrokes("tab");
        visual.run_until_parked();
        let formatted = input.read_with(visual, |input, _| {
            let text = input.value();
            assert_eq!(
                &text[input.selected_range()],
                "two",
                "real Tab reaches next cell"
            );
            assert!(text.starts_with("Before\n\n"));
            assert!(text.ends_with("\n\nAfter\n"));
            text.to_string()
        });
        visual.simulate_keystrokes("shift-tab");
        visual.run_until_parked();
        input.read_with(visual, |input, _| {
            assert_eq!(&input.value()[input.selected_range()], "one");
            assert_eq!(input.value().to_string(), formatted);
        });
        // Both navigation directions preserve the authored table bytes.
        assert_eq!(
            formatted, source,
            "navigation does not rewrite authored spacing"
        );
        visual.dispatch_action(TableInsertColumn);
        visual.run_until_parked();
        let inserted = input.read_with(visual, |input, _| input.value().to_string());
        assert_ne!(inserted, formatted, "menu action reaches the same editor");
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            input.read_with(visual, |input, _| input.value().to_string()),
            formatted
        );
    }

    #[gpui::test]
    fn table_keys_and_structural_action_use_editor_undo_in_source(cx: &mut TestAppContext) {
        exercise_keys(cx, false);
    }

    #[gpui::test]
    fn table_keys_and_structural_action_use_editor_undo_in_live_preview(cx: &mut TestAppContext) {
        exercise_keys(cx, true);
    }

    #[test]
    fn final_row_and_bare_column_deletion_keep_exact_classifier_ranges() {
        let source = "Before\n\nA | B\n--- | ---\none | two\n\nAfter\n";
        for (cell, command) in [("one", Command::DeleteRow), ("two", Command::DeleteColumn)] {
            let at = source.find(cell).unwrap();
            let edit = change(source, at..at, command)
                .expect("positive control: admitted structural edit");
            let mut updated = source.to_owned();
            for (range, replacement) in edit.edits.iter().rev() {
                updated.replace_range(range.clone(), replacement);
            }
            let snapshot = Snapshot::new("table-delete-regression", 0, updated.as_str());
            let classified = source_classifier::classify(&snapshot);
            let table = classified
                .decorations_for(&snapshot)
                .unwrap()
                .iter()
                .find(|marker| marker.kind == Kind::Table)
                .expect("the edited table remains classified");
            assert_eq!(table.range.start, "Before\n\n".len());
            assert_eq!(
                table.range.end,
                updated.find("\n\nAfter").unwrap(),
                "a grid lease excludes the surrounding newline"
            );
        }
    }

    #[test]
    fn deleting_the_first_of_three_columns_keeps_literal_block_markers_in_the_table() {
        for newline in ["\n", "\r\n"] {
            for value in ["", "# Heading 😀", "> quote", "<div>", "- item", "1. task"] {
                let source = [
                    format!("Remove|{value}|Last"),
                    "---|---|---".to_owned(),
                    format!("remove|{value}|tail"),
                ]
                .join(newline);
                let at = source.find("Remove").unwrap();
                let changed = model::edit(&source, at..at, Command::DeleteColumn).unwrap();
                let table = model::Table::parse(&changed.source).unwrap();
                assert_eq!(table.rows[0], [value, "Last"]);
                assert_eq!(table.rows[1], [value, "tail"]);
                let snapshot = Snapshot::new("delete-leading-cell", 0, changed.source.as_str());
                let classified = source_classifier::classify(&snapshot);
                assert!(
                    classified
                        .decorations_for(&snapshot)
                        .unwrap()
                        .iter()
                        .any(|marker| marker.kind == Kind::Table
                            && marker.range == (0..changed.source.len())),
                    "positive control: column deletion preserves the complete GFM table"
                );
            }
        }
    }

    #[test]
    fn editing_bare_cell_block_markers_keeps_a_classified_table_and_exact_values() {
        let source = "Name|Value\n---|---\nold|neighbor";
        for row in [0, 1] {
            for value in [
                "# Heading 😀",
                "> quote",
                "- item",
                "```",
                "<div>",
                "1. task",
                "    indented",
                "[ref]: target",
            ] {
                let (cells, changed) = model::CellGrid::new(source)
                    .unwrap()
                    .replace_text(source, row, 0, value)
                    .unwrap();
                assert_eq!(model::cell_text(cells.value(row, 0).unwrap()), value);
                let snapshot = Snapshot::new("literal-table-cell", 0, changed.source.as_str());
                let classified = source_classifier::classify(&snapshot);
                assert!(
                    classified
                        .decorations_for(&snapshot)
                        .unwrap()
                        .iter()
                        .any(|marker| marker.kind == Kind::Table
                            && marker.range == (0..changed.source.len())),
                    "positive control: {value:?} remains inside the complete GFM table"
                );
            }
        }
    }

    #[test]
    fn clearing_either_bare_cell_keeps_empty_cell_identity_and_gfm_shape() {
        let source = "Name|Value\n---|---\nold|neighbor";
        for row in [0, 1] {
            for column in [0, 1] {
                for value in ["", "    "] {
                    let (cells, changed) = model::CellGrid::new(source)
                        .unwrap()
                        .replace_text(source, row, column, value)
                        .unwrap();
                    assert_eq!(model::cell_text(cells.value(row, column).unwrap()), value);
                    let (cells, typed) = cells
                        .replace_text(&changed.source, row, column, "fresh😀")
                        .unwrap();
                    assert_eq!(
                        model::cell_text(cells.value(row, column).unwrap()),
                        "fresh😀",
                        "positive control: the empty cell accepts new text"
                    );
                    for text in [&changed.source, &typed.source] {
                        let snapshot = Snapshot::new("empty-table-cell", 0, text.as_str());
                        let classified = source_classifier::classify(&snapshot);
                        assert!(classified
                            .decorations_for(&snapshot)
                            .unwrap()
                            .iter()
                            .any(|marker| marker.kind == Kind::Table
                                && marker.range == (0..text.len())));
                    }
                }
            }
        }
    }
}
