//! Persistent table cell inputs backed by the note editor's revision and Undo.
//! Parent owns the Reader field/render/click hooks. Keep this entity alive for
//! the lifetime of Editing; do not recreate it in the Reader render function.
use super::*;
use gpui_component::input::projection::SourceStamp;
use gpui_component::input::{Input, InputEvent, InputState, Redo, Undo};

struct Binding {
    stamp: SourceStamp,
    document: String,
    table: Range<usize>,
    cells: model::CellGrid,
}

pub(in crate::reader_editor) struct TableGrid {
    reader: WeakEntity<Reader>,
    input: Entity<EditorState>,
    binding: Option<Binding>,
    cells: Vec<Vec<Entity<InputState>>>,
    subscriptions: Vec<Subscription>,
    _source: Subscription,
    error: Option<&'static str>,
}

impl TableGrid {
    pub(in crate::reader_editor) fn new(
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            reader: reader.downgrade(),
            _source: cx.observe(&input, |_, _, cx| cx.notify()),
            input,
            binding: None,
            cells: Vec::new(),
            subscriptions: Vec::new(),
            error: None,
        }
    }

    /// Open only an exact currently classified top-level table. The caller may
    /// use the existing table cell press callback; source offsets stay absolute.
    pub(in crate::reader_editor) fn open(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.input.read(cx);
        let document = state.value().to_string();
        let stamp = state.source_stamp();
        let Some(reader) = self.reader.upgrade() else {
            return false;
        };
        let Some(editing) = &reader.read(cx).editing else {
            return false;
        };
        if editing.input != self.input
            || !editing.markdown
            || !state.is_editable()
            || document.len() > source_classifier::MAX_BYTES
        {
            return false;
        }
        let snapshot = Snapshot::new("table-grid", 0, document.as_str());
        let classified = source_classifier::classify(&snapshot);
        if !classified
            .decorations_for(&snapshot)
            .ok()
            .is_some_and(|markers| {
                markers
                    .iter()
                    .any(|marker| marker.kind == Kind::Table && marker.range == range)
            })
        {
            return false;
        }
        let Some(table) = document.get(range.clone()).and_then(model::Table::parse) else {
            return false;
        };
        // Do not allocate unbounded native inputs for pathological tables.
        if table.rows.iter().map(Vec::len).sum::<usize>() > 2048 {
            return false;
        }
        let Some(cells) = model::CellGrid::new(&document[range.clone()]) else {
            return false;
        };
        self.binding = Some(Binding {
            stamp,
            document,
            table: range,
            cells,
        });
        self.rebuild(table.rows, window, cx);
        self.error = None;
        cx.notify();
        true
    }

    /// Parent uses this to substitute the active rendered table, without
    /// treating stale byte offsets as a valid presentation range.
    pub(in crate::reader_editor) fn active_range(&self, cx: &App) -> Option<Range<usize>> {
        self.current(cx)
            .then(|| self.binding.as_ref().unwrap().table.clone())
    }

    pub(in crate::reader_editor) fn focus_cell(
        &self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(cell) = self.cells.get(row).and_then(|row| row.get(column)) else {
            return false;
        };
        cell.update(cx, |cell, cx| {
            cell.focus(window, cx);
            cell.select_all(window, cx);
        });
        true
    }

    pub(in crate::reader_editor) fn close(&mut self, cx: &mut Context<Self>) {
        self.binding = None;
        self.subscriptions.clear();
        self.cells.clear();
        self.error = None;
        cx.notify();
    }

    fn rebuild(&mut self, rows: Vec<Vec<String>>, window: &mut Window, cx: &mut Context<Self>) {
        self.subscriptions.clear();
        self.cells.clear();
        for (r, row) in rows.into_iter().enumerate() {
            let mut cells = Vec::new();
            for (c, value) in row.into_iter().enumerate() {
                let cell = cx.new(|cx| {
                    let mut state = InputState::new(window, cx);
                    state.set_ime_settle(std::time::Duration::from_millis(100));
                    state.set_value(value, window, cx);
                    state
                });
                self.subscriptions.push(cx.subscribe_in(
                    &cell,
                    window,
                    move |this, cell, event, window, cx| {
                        if matches!(event, InputEvent::Change) {
                            let state = cell.read(cx);
                            // Preedit is local to the cell; only committed text can
                            // reach the canonical draft or its Undo history.
                            if state.ime_composing() {
                                return;
                            }
                            let value = state.value().to_string();
                            this.replace(r, c, &value, window, cx);
                        }
                    },
                ));
                cells.push(cell);
            }
            self.cells.push(cells);
        }
    }

    fn current(&self, cx: &App) -> bool {
        let Some(binding) = &self.binding else {
            return false;
        };
        let Some(reader) = self.reader.upgrade() else {
            return false;
        };
        let Some(editing) = &reader.read(cx).editing else {
            return false;
        };
        let state = self.input.read(cx);
        editing.input == self.input
            && editing.markdown
            && state.is_editable()
            && state.source_stamp() == binding.stamp
            && state.value().as_ref() == binding.document
    }

    fn replace(
        &mut self,
        row: usize,
        col: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.current(cx) {
            self.error = Some("The note changed. Reopen the table before editing.");
            cx.notify();
            return;
        }
        let binding = self.binding.as_ref().unwrap();
        let source = &binding.document[binding.table.clone()];
        let Some((cells, change)) = binding.cells.replace(source, row, col, value) else {
            self.error = Some("Use an escaped pipe (\\|); keep each cell on one source line.");
            cx.notify();
            return;
        };
        if self.apply(change, window, cx) {
            self.binding.as_mut().unwrap().cells = cells;
        }
    }

    fn apply(
        &mut self,
        change: model::Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.current(cx) {
            return false;
        }
        let binding = self.binding.as_ref().unwrap();
        let start = binding.table.start;
        let range = binding.table.clone();
        let selection = start + change.selection.start..start + change.selection.end;
        let unchanged = binding.document.get(range.clone()) == Some(change.source.as_str());
        if unchanged {
            return true;
        }
        let Some(reader) = self.reader.upgrade() else {
            return false;
        };
        let input = self.input.clone();
        let replacement_len = change.source.len();
        let Some(cells) = model::CellGrid::new(&change.source) else {
            return false;
        };
        // The only write path: the same atomic source edit transaction as
        // list commands. The existing editor owns save/conflict/recovery.
        let applied = reader.update(cx, |reader, cx| {
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
        });
        if applied {
            let state = self.input.read(cx);
            self.binding = Some(Binding {
                stamp: state.source_stamp(),
                document: state.value().to_string(),
                table: start..start + replacement_len,
                cells,
            });
            self.error = None;
        } else {
            self.error = Some("The cell edit could not be applied. Reopen the table.");
        }
        cx.notify();
        applied
    }

    fn command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.cells.iter().enumerate().find_map(|(r, row)| {
            row.iter()
                .enumerate()
                .find(|(_, cell)| cell.read(cx).focus_handle(cx).is_focused(window))
                .map(|(c, _)| (r, c))
        });
        let Some((r, c)) = focused else {
            return;
        };
        let cell = self.cells[r][c].read(cx);
        if cell.ime_composing() || cell.ime_settling(cx) || !self.current(cx) {
            return;
        }
        let binding = self.binding.as_ref().unwrap();
        let source = &binding.document[binding.table.clone()];
        let Some(table) = model::Table::parse(source) else {
            return;
        };
        // Never discard a cell value that was refused (e.g. an unescaped pipe).
        if Some(cell.value().as_ref()) != binding.cells.value(r, c) {
            return;
        }
        let Some(change) = model::edit(source, table.ranges[r][c].clone(), command) else {
            self.error = Some("This table action cannot apply to the selected cell.");
            cx.notify();
            return;
        };
        let Some(table) = model::Table::parse(&change.source) else {
            return;
        };
        let target = table
            .ranges
            .iter()
            .enumerate()
            .find_map(|(r, row)| {
                row.iter()
                    .position(|range| *range == change.selection)
                    .map(|c| (r, c))
            })
            .unwrap_or((r, c));
        let navigation =
            matches!(command, Command::Next | Command::Previous) && change.source == source;
        if self.apply(change, window, cx) {
            if !navigation {
                self.rebuild(table.rows, window, cx);
            }
            self.cells[target.0][target.1].update(cx, |cell, cx| {
                cell.focus(window, cx);
                cell.select_all(window, cx);
            });
            cx.stop_propagation();
        }
    }

    /// Route Undo/Redo to the source editor, never the cell's private history.
    /// Source observation invalidates this binding; reopening uses fresh bytes.
    fn history(&mut self, action: Box<dyn Action>, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .cells
            .iter()
            .flatten()
            .any(|cell| cell.read(cx).ime_composing())
        {
            return;
        }
        let range = self.binding.as_ref().map(|binding| binding.table.clone());
        self.close(cx);
        self.input.update(cx, |state, cx| state.focus(window, cx));
        window.dispatch_action(action, cx);
        // Dispatch resolves after this listener. Rebind on the next frame to
        // the restored revision, keeping the table editable after Undo/Redo.
        cx.on_next_frame(window, move |this, window, cx| {
            if let Some(range) = range {
                let document = this.input.read(cx).value().to_string();
                let snapshot = Snapshot::new("table-grid-history", 0, document.as_str());
                let classified = source_classifier::classify(&snapshot);
                let restored = classified
                    .decorations_for(&snapshot)
                    .ok()
                    .and_then(|markers| {
                        markers
                            .iter()
                            .find(|marker| {
                                marker.kind == Kind::Table && marker.range.start == range.start
                            })
                            .map(|marker| marker.range.clone())
                    });
                if let Some(range) = restored {
                    if this.open(range, window, cx) {
                        this.cells[0][0].update(cx, |cell, cx| cell.focus(window, cx));
                    }
                }
            }
        });
        cx.stop_propagation();
    }
}

impl Render for TableGrid {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = brand::palette(cx);
        let mut root = div()
            .id("editable-table-grid")
            .flex()
            .flex_col()
            .font_family(reader_fonts::note(cx));
        if self.binding.is_none() {
            return root;
        }
        let stale = !self.current(cx);
        let alignment = self
            .binding
            .as_ref()
            .and_then(|binding| model::Table::parse(&binding.document[binding.table.clone()]))
            .map(|table| table.alignment)
            .unwrap_or_default();
        for (r, row) in self.cells.iter().enumerate() {
            let mut line = div().flex();
            for (c, cell) in row.iter().enumerate() {
                let input = Input::new(cell).disabled(stale);
                let input = match alignment.get(c) {
                    Some(Alignment::Center) => input.text_center(),
                    Some(Alignment::Right) => input.text_right(),
                    _ => input.text_left(),
                };
                line = line.child(
                    div()
                        .id(("editable-table-cell", r * 128 + c))
                        .flex_1()
                        .min_w(px(80.))
                        .border_1()
                        .border_color(palette.border)
                        .child(input),
                );
            }
            root = root.child(line);
        }
        if let Some(error) = self
            .error
            .or(stale.then_some("The note changed. Reopen the table before editing."))
        {
            root = root.child(div().text_sm().text_color(palette.danger).child(error));
        }
        root.capture_action(
            cx.listener(|this, _: &Undo, window, cx| this.history(Box::new(Undo), window, cx)),
        )
        .capture_action(
            cx.listener(|this, _: &Redo, window, cx| this.history(Box::new(Redo), window, cx)),
        )
        .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
            this.command(Command::Next, window, cx);
        }))
        .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
            this.command(Command::Previous, window, cx);
        }))
        .on_action(cx.listener(|this, _: &TableInsertRow, window, cx| {
            this.command(Command::InsertRow, window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableDeleteRow, window, cx| {
            this.command(Command::DeleteRow, window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableInsertColumn, window, cx| {
            this.command(Command::InsertColumn, window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableDeleteColumn, window, cx| {
            this.command(Command::DeleteColumn, window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableAlignLeft, window, cx| {
            this.command(Command::Align(Alignment::Left), window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableAlignCenter, window, cx| {
            this.command(Command::Align(Alignment::Center), window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableAlignRight, window, cx| {
            this.command(Command::Align(Alignment::Right), window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableFormat, window, cx| {
            this.command(Command::Format, window, cx)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn grid_writes_the_editor_draft_uses_source_undo_and_refuses_a_stale_binding(
        cx: &mut TestAppContext,
    ) {
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
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input.clone()
        });
        let grid =
            visual.update(|_, cx| cx.new(|cx| TableGrid::new(reader.clone(), input.clone(), cx)));
        let range = source.find("| A").unwrap()..source.find("\n\nAfter").unwrap();
        grid.update_in(visual, |grid, window, cx| {
            assert!(
                grid.open(range, window, cx),
                "positive control: classified table"
            );
            grid.replace(1, 0, "edited", window, cx);
            assert!(grid.error.is_none());
        });
        let expected = source.replace("one", "edited");
        assert_eq!(
            input.read_with(visual, |state, _| state.value().to_string()),
            expected
        );
        // Use the actual source history. Restoring old bytes must still leave
        // the grid stale: a source revision, not string equality, owns edits.
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            input.read_with(visual, |state, _| state.value().to_string()),
            source
        );
        grid.update_in(visual, |grid, window, cx| {
            assert!(!grid.current(cx));
            grid.replace(1, 1, "must not land", window, cx);
            assert!(grid.error.is_some());
        });
        assert_eq!(
            input.read_with(visual, |state, _| state.value().to_string()),
            source
        );
    }
}
