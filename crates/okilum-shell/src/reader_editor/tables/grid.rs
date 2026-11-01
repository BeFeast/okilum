//! Persistent table cell inputs backed by the note editor's revision and Undo.
//! Parent owns the Reader field/render/click hooks. Keep this entity alive for
//! the lifetime of Editing; do not recreate it in the Reader render function.
use super::*;
use crate::source_presentation::{CachedProvider, ProjectionColors};
use gpui_component::input::projection::{
    ConcealBias, LineIndent, LineScale, ProjectedByte, ProjectionBlock, ProjectionMarker,
    ProjectionStyle, SourceByte, SourceStamp,
};
use gpui_component::input::{Input, InputEvent, InputState, Redo, Undo};
use gpui_component::Sizable as _;

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
    provider: Option<Arc<CachedProvider>>,
    base: Option<Arc<dyn ProjectionProvider>>,
    preparing: Option<Task<()>>,
    installed_epoch: Option<u64>,
    session: u64,
    selected: (usize, usize),
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
            provider: None,
            base: None,
            preparing: None,
            installed_epoch: None,
            session: 0,
            selected: (0, 0),
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
        reader.update(cx, |reader, _| {
            if let Some(editing) = &mut reader.editing {
                if editing.input == self.input {
                    editing.live_preview.block_focus = None;
                    editing.live_preview.opened_block = None;
                }
            }
        });
        self.error = None;
        self.installed_epoch = None;
        self.session = self.session.wrapping_add(1);
        let session = self.session;
        let source = SourceSnapshot {
            stamp,
            text: Arc::from(self.binding.as_ref().unwrap().document.as_str()),
        };
        // Existing LivePreview publication remains untouched. The grid's lease
        // has its own immutable fallback, classified off the foreground thread.
        self.preparing = Some(cx.spawn(async move |this, cx| {
            let provider = cx
                .background_executor()
                .spawn(async move { Arc::new(CachedProvider::classify(source)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.session == session && this.binding.is_some() {
                    this.provider = Some(provider);
                    this.installed_epoch = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
        true
    }

    /// Parent uses this to substitute the active rendered table, without
    /// treating stale byte offsets as a valid presentation range.
    pub(in crate::reader_editor) fn active_range(&self, cx: &App) -> Option<Range<usize>> {
        self.revision_matches(cx)
            .then(|| self.binding.as_ref().unwrap().table.clone())
    }

    pub(in crate::reader_editor) fn focus_cell(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(cell) = self.cells.get(row).and_then(|row| row.get(column)) else {
            return false;
        };
        self.selected = (row, column);
        cell.update(cx, |cell, cx| {
            cell.focus(window, cx);
            cell.select_all(window, cx);
        });
        true
    }

    /// A painted callback may outlive its source revision. Check both stamp
    /// and exact bytes before replacing the existing source click behavior.
    pub(in crate::reader_editor) fn open_cell(
        &mut self,
        range: Range<usize>,
        stamp: SourceStamp,
        source: &str,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.input.read(cx);
        if state.source_stamp() != stamp || state.value().get(range.clone()) != Some(source) {
            return false;
        }
        if !self.open(range, window, cx) {
            return false;
        }
        self.focus_cell(row, column, window, cx)
    }

    /// Reinstall only after a real presentation change (including a standard
    /// LivePreview publication); never notify continuously while idle.
    pub(in crate::reader_editor) fn sync_presentation(
        &mut self,
        live: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.binding.is_none() {
            return;
        }
        if !live || !self.revision_matches(cx) {
            self.close(cx);
            return;
        }
        // A source key/click releases the grid lease and restores the normal
        // reveal policy, including F2/double-click source editing.
        if self.input.read(cx).focus_handle(cx).is_focused(window) {
            self.close(cx);
            return;
        }
        let Some(provider) = self.provider.clone() else {
            return;
        };
        if self.installed_epoch == Some(self.input.read(cx).presentation_epoch()) {
            return;
        }
        let palette = brand::palette(cx);
        let base = provider.with_code_syntax(
            ProjectionColors {
                heading: palette.text,
                link: palette.link,
                muted: palette.text_muted,
            },
            reader_fonts::code(cx).to_string(),
            cx.theme().highlight_theme.clone(),
        );
        self.base = Some(base.clone());
        let binding = self.binding.as_ref().unwrap();
        let provider = Arc::new(GridProvider {
            base,
            source: SourceSnapshot {
                stamp: binding.stamp,
                text: Arc::from(binding.document.as_str()),
            },
            table: binding.table.clone(),
        });
        self.input.update(cx, |input, cx| {
            input.set_projection_provider(Some(provider), cx)
        });
        self.installed_epoch = Some(self.input.read(cx).presentation_epoch());
    }

    pub(in crate::reader_editor) fn close(&mut self, cx: &mut Context<Self>) {
        self.binding = None;
        self.preparing = None;
        self.session = self.session.wrapping_add(1);
        let owns_presentation =
            self.installed_epoch == Some(self.input.read(cx).presentation_epoch());
        self.installed_epoch = None;
        // Restore the standard reveal policy before dropping the lease. A
        // newer LivePreview worker may replace this cached provider normally.
        self.provider = None;
        if let Some(provider) = self.base.take().filter(|_| owns_presentation) {
            self.input.update(cx, |input, cx| {
                input.set_projection_provider(Some(provider), cx)
            });
        }
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
                    let mut state = InputState::new(window, cx).auto_grow(1, 6);
                    state.set_submit_on_enter(false, cx);
                    state.set_ime_settle(std::time::Duration::from_millis(100));
                    state.set_value(model::cell_text(&value), window, cx);
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

    fn revision_matches(&self, cx: &App) -> bool {
        let Some(binding) = &self.binding else {
            return false;
        };
        let state = self.input.read(cx);
        state.is_editable()
            && state.source_stamp() == binding.stamp
            && state.value().as_ref() == binding.document
    }

    fn current(&self, cx: &App) -> bool {
        if !self.revision_matches(cx) {
            return false;
        }
        let Some(reader) = self.reader.upgrade() else {
            return false;
        };
        let Some(editing) = &reader.read(cx).editing else {
            return false;
        };
        editing.input == self.input && editing.markdown
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
        let Some((cells, change)) = binding.cells.replace_text(source, row, col, value) else {
            self.error =
                Some("This edit would change Markdown structure. Use Source for this cell.");
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
            self.installed_epoch = None;
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
        let (r, c) = focused.unwrap_or(self.selected);
        let Some(cell) = self.cells.get(r).and_then(|row| row.get(c)) else {
            return;
        };
        let cell = cell.read(cx);
        if cell.ime_composing() || cell.ime_settling(cx) || !self.current(cx) {
            return;
        }
        let binding = self.binding.as_ref().unwrap();
        let source = &binding.document[binding.table.clone()];
        let Some(table) = model::Table::parse(source) else {
            return;
        };
        // Never discard a cell value that was refused (e.g. an unescaped pipe).
        if binding.cells.value(r, c).map(model::cell_text).as_deref() != Some(cell.value().as_ref())
        {
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
            self.selected = target;
            self.cells[target.0][target.1].update(cx, |cell, cx| {
                cell.focus(window, cx);
                cell.select_all(window, cx);
            });
            cx.stop_propagation();
        }
    }

    fn source_mode(&mut self, select: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .cells
            .iter()
            .flatten()
            .any(|cell| cell.read(cx).ime_settling(cx))
        {
            return;
        }
        let Some(range) = self.active_range(cx) else {
            return;
        };
        let binding = self.binding.as_ref().unwrap();
        if self.cells.iter().enumerate().any(|(r, row)| {
            row.iter().enumerate().any(|(c, cell)| {
                binding.cells.value(r, c).map(model::cell_text).as_deref()
                    != Some(cell.read(cx).value().as_ref())
            })
        }) {
            self.error = Some("Copy or correct the cell text before leaving the grid; it has not been saved to source.");
            cx.notify();
            return;
        }
        let document = self.input.read(cx).value().to_string();
        let caret = super::super::block_entry::caret_outside(&document, &range);
        self.close(cx);
        self.input.update(cx, |state, cx| {
            let selection = if select {
                range.clone()
            } else {
                caret.map_or_else(|| range.clone(), |at| at..at)
            };
            state.set_selected_range(selection, cx);
            state.focus(window, cx);
        });
        cx.stop_propagation();
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(selected) = self.cells.iter().enumerate().find_map(|(r, row)| {
            row.iter()
                .position(|cell| cell.read(cx).focus_handle(cx).is_focused(window))
                .map(|c| (r, c))
        }) {
            self.selected = selected;
        }
        let palette = brand::palette(cx);
        let mut root = div()
            .id("editable-table-grid")
            .debug_selector(|| "editable-table-grid".into())
            .flex()
            .flex_col()
            .font_family(reader_fonts::note(cx))
            // Nested cell input handles the press first. The outer source
            // editor must not take focus or reveal the table on the bubble.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if let Some(selected) = this.cells.iter().enumerate().find_map(|(r, row)| {
                        row.iter()
                            .position(|cell| cell.read(cx).focus_handle(cx).is_focused(window))
                            .map(|c| (r, c))
                    }) {
                        this.selected = selected;
                    }
                    cx.stop_propagation();
                }),
            );
        if self.binding.is_none() {
            return root;
        }
        let stale = !self.revision_matches(cx);
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
        let mut controls = h_flex().gap_1().flex_wrap();
        for (id, label, command) in [
            ("table-row-add", "Add row", Command::InsertRow),
            ("table-row-delete", "Delete row", Command::DeleteRow),
            ("table-column-add", "Add column", Command::InsertColumn),
            (
                "table-column-delete",
                "Delete column",
                Command::DeleteColumn,
            ),
            ("table-align-left", "Left", Command::Align(Alignment::Left)),
            (
                "table-align-center",
                "Center",
                Command::Align(Alignment::Center),
            ),
            (
                "table-align-right",
                "Right",
                Command::Align(Alignment::Right),
            ),
        ] {
            controls = controls.child(
                Button::new(id)
                    .small()
                    .label(label)
                    .disabled(stale)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.command(command, window, cx)),
                    ),
            );
        }
        controls = controls.child(
            Button::new("table-source")
                .small()
                .label("Source")
                .on_click(cx.listener(|this, _, window, cx| this.source_mode(true, window, cx))),
        );
        root = root.child(controls);
        root.on_action(
            cx.listener(|this, _: &super::super::OpenRenderedBlock, window, cx| {
                this.source_mode(true, window, cx);
            }),
        )
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                this.source_mode(false, window, cx);
            }
        }))
        .capture_action(
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

/// A presentation lease for exactly the grid-owned source revision. It only
/// substitutes the active table block; all mappings, marker policies, fonts
/// and other rendered blocks come from the regular cached provider.
struct GridProvider {
    base: Arc<dyn ProjectionProvider>,
    source: SourceSnapshot,
    table: Range<usize>,
}

impl ProjectionProvider for GridProvider {
    fn compose(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        let projection = self.base.compose(source, active)?;
        if source.stamp != self.source.stamp
            || source.text != self.source.text
            || active.composition.is_some()
            || active.replacement.is_some()
        {
            return Some(projection);
        }
        let from = projection
            .to_projected(SourceByte(self.table.start), ConcealBias::Left)?
            .0;
        let to = projection
            .to_projected(SourceByte(self.table.end), ConcealBias::Right)?
            .0;
        let text = projection.text();
        let first = text.get(..from)?.bytes().filter(|&b| b == b'\n').count();
        let last = text.get(..to)?.bytes().filter(|&b| b == b'\n').count() + 1;
        let mut blocks: Vec<_> = projection
            .blocks()
            .iter()
            .filter(|block| {
                block.source.end.0 <= self.table.start || self.table.end <= block.source.start.0
            })
            .cloned()
            .collect();
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.source.text.get(self.table.clone())?.hash(&mut hash);
        blocks.push(ProjectionBlock {
            source: SourceByte(self.table.start)..SourceByte(self.table.end),
            lines: first..last,
            key: hash.finish(),
        });
        blocks.sort_by_key(|block| block.lines.start);
        Some(Arc::new(GridProjection {
            base: projection,
            blocks,
        }))
    }
}

struct GridProjection {
    base: Arc<dyn SourceProjection>,
    blocks: Vec<ProjectionBlock>,
}

impl SourceProjection for GridProjection {
    fn source(&self) -> &SourceSnapshot {
        self.base.source()
    }
    fn text(&self) -> &str {
        self.base.text()
    }
    fn styles(&self) -> &[ProjectionStyle] {
        self.base.styles()
    }
    fn markers(&self) -> &[ProjectionMarker] {
        self.base.markers()
    }
    fn line_scales(&self) -> &[LineScale] {
        self.base.line_scales()
    }
    fn line_indents(&self) -> &[LineIndent] {
        self.base.line_indents()
    }
    fn blocks(&self) -> &[ProjectionBlock] {
        &self.blocks
    }
    fn marker_scope_is_raw(&self, range: &Range<SourceByte>) -> bool {
        self.base.marker_scope_is_raw(range)
    }
    fn source_boundary(&self, source: &SourceSnapshot, at: SourceByte) -> Option<bool> {
        self.base.source_boundary(source, at)
    }
    fn to_projected(&self, source: SourceByte, bias: ConcealBias) -> Option<ProjectedByte> {
        self.base.to_projected(source, bias)
    }
    fn to_source(&self, projected: ProjectedByte, bias: ConcealBias) -> Option<SourceByte> {
        self.base.to_source(projected, bias)
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    struct Linear(SourceSnapshot);
    impl ProjectionProvider for Linear {
        fn compose(
            &self,
            source: &SourceSnapshot,
            _: &ActiveSource,
        ) -> Option<Arc<dyn SourceProjection>> {
            Some(Arc::new(Linear(source.clone())))
        }
    }
    impl SourceProjection for Linear {
        fn source(&self) -> &SourceSnapshot {
            &self.0
        }
        fn text(&self) -> &str {
            &self.0.text
        }
        fn styles(&self) -> &[ProjectionStyle] {
            &[]
        }
        fn line_indents(&self) -> &[LineIndent] {
            &[LineIndent { line: 0, prefix: 1 }]
        }
        fn marker_scope_is_raw(&self, _: &Range<SourceByte>) -> bool {
            false
        }
        fn source_boundary(&self, source: &SourceSnapshot, at: SourceByte) -> Option<bool> {
            (source.stamp == self.0.stamp).then(|| source.text.is_char_boundary(at.0))
        }
        fn to_projected(&self, at: SourceByte, _: ConcealBias) -> Option<ProjectedByte> {
            self.0
                .text
                .is_char_boundary(at.0)
                .then_some(ProjectedByte(at.0))
        }
        fn to_source(&self, at: ProjectedByte, _: ConcealBias) -> Option<SourceByte> {
            self.0
                .text
                .is_char_boundary(at.0)
                .then_some(SourceByte(at.0))
        }
    }

    fn fixture() -> (GridProvider, SourceSnapshot, ActiveSource) {
        let source = SourceSnapshot {
            stamp: SourceStamp {
                document: 4,
                generation: 2,
            },
            text: Arc::from("Before\n\n| A | B |\n|---|---|\n| one | two |\n\nAfter"),
        };
        let table = source.text.find('|').unwrap()..source.text.find("\n\nAfter").unwrap();
        let active = ActiveSource {
            anchor: SourceByte(table.start + 2),
            head: SourceByte(table.start + 2),
            ..ActiveSource::default()
        };
        let provider = GridProvider {
            base: Arc::new(Linear(source.clone())),
            source: source.clone(),
            table,
        };
        (provider, source, active)
    }

    #[test]
    fn grid_projection_keeps_the_active_table_and_forwards_mapping_metadata() {
        let (provider, source, active) = fixture();
        let plain = provider.base.compose(&source, &active).unwrap();
        assert!(
            plain.blocks().is_empty(),
            "positive control: source caret normally reveals this table"
        );
        let grid = provider.compose(&source, &active).unwrap();
        assert_eq!(grid.blocks().len(), 1);
        assert_eq!(grid.blocks()[0].lines, 2..5);
        assert_eq!(
            grid.blocks()[0].source,
            SourceByte(provider.table.start)..SourceByte(provider.table.end)
        );
        assert_eq!(grid.text(), source.text.as_ref());
        assert_eq!(grid.line_indents()[0].prefix, 1);
        assert!(!grid.marker_scope_is_raw(&(SourceByte(0)..SourceByte(1))));
        assert_eq!(grid.source_boundary(&source, SourceByte(1)), Some(true));
        assert_eq!(
            grid.to_source(ProjectedByte(1), ConcealBias::Left),
            Some(SourceByte(1))
        );
    }

    #[test]
    fn grid_projection_never_substitutes_a_stale_revision_bytes_or_ime_range() {
        let (provider, source, active) = fixture();
        assert_eq!(
            provider.compose(&source, &active).unwrap().blocks().len(),
            1
        );
        let mut stale = source.clone();
        stale.stamp.generation += 1;
        assert!(provider
            .compose(&stale, &active)
            .unwrap()
            .blocks()
            .is_empty());
        stale = source.clone();
        stale.text = Arc::from(source.text.replace("one", "new"));
        assert!(provider
            .compose(&stale, &active)
            .unwrap()
            .blocks()
            .is_empty());
        let composing = ActiveSource {
            composition: Some(active.anchor..active.head),
            ..active
        };
        assert!(provider
            .compose(&source, &composing)
            .unwrap()
            .blocks()
            .is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn reader_cell_click_keeps_grid_edits_escaped_multiline_and_source_undo(
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
            reader.toggle_source(window, cx);
            reader.set_live_preview(true, window, cx);
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input.clone()
        });
        let grid = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().table_grid.clone()
        });
        let cell = visual
            .debug_bounds("table-cell-1-0")
            .expect("positive control: real rendered table cell");
        visual.simulate_click(cell.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("editable-table-grid").is_some(),
            "cell press substituted the grid"
        );
        let cell_id = grid.read_with(visual, |grid, _| grid.cells[1][0].entity_id());
        visual.simulate_input("edited|value\nשלום");
        visual.run_until_parked();
        let expected = source.replace("one", "edited\\|value<br>שלום");
        assert_eq!(
            input.read_with(visual, |state, _| state.value().to_string()),
            expected
        );
        assert!(
            visual.debug_bounds("editable-table-grid").is_some(),
            "typing never revealed the table source"
        );
        assert_eq!(
            grid.read_with(visual, |grid, _| grid.cells[1][0].entity_id()),
            cell_id
        );
        assert_eq!(
            reader.read_with(visual, |reader, _| reader
                .editing
                .as_ref()
                .unwrap()
                .table_grid
                .entity_id()),
            grid.entity_id()
        );
        let old_stamp = input.read_with(visual, |input, _| input.source_stamp());
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            input.read_with(visual, |state, _| state.value().to_string()),
            source
        );
        let range = source.find("| A").unwrap()..source.find("\n\nAfter").unwrap();
        grid.update_in(visual, |grid, window, cx| {
            assert!(
                !grid.open_cell(range.clone(), old_stamp, &source[range], 1, 0, window, cx),
                "a stale painted cell callback cannot bind the restored source revision"
            );
        });
        visual.dispatch_action(super::super::OpenRenderedBlock);
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("editable-table-grid").is_none(),
            "F2 retains access to exact source"
        );
        assert_eq!(
            input.read_with(visual, |state, _| state.value().to_string()),
            source
        );
    }
}
