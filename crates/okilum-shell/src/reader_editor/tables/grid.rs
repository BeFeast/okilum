//! Persistent table cell inputs backed by the note editor's revision and Undo.
//! Parent owns the Reader field/render/click hooks. Keep this entity alive for
//! the lifetime of Editing; do not recreate it in the Reader render function.
use super::*;
#[path = "idle.rs"]
mod idle;
use crate::source_presentation::{CachedProvider, ProjectionColors};
use gpui_component::input::projection::{
    ConcealBias, LineIndent, LineScale, ProjectedByte, ProjectionBlock, ProjectionMarker,
    ProjectionStyle, SourceByte, SourceStamp,
};
use gpui_component::input::RopeExt as _;
use gpui_component::input::{InputEvent, Redo, Textarea, TextareaState, Undo};
use gpui_component::Sizable as _;

struct Binding {
    stamp: SourceStamp,
    document: String,
    table: Range<usize>,
    cells: model::CellGrid,
}

#[derive(Clone, Copy)]
enum HistoryDirection {
    Undo,
    Redo,
}

#[derive(Clone, Copy)]
struct HistoryGroup {
    cell: Option<(usize, usize)>,
    steps: usize,
}

/// Only transaction counts: the canonical editor retains every preimage,
/// redo record and source selection. A cell's Textarea owns no edit history.
/// `replace_source_edits` creates one Atomic source transaction per update;
/// consecutive committed updates of one cell form one user-facing cell edit.
#[derive(Default)]
struct SourceHistory {
    revision: Option<SourceStamp>,
    undo: Vec<HistoryGroup>,
    redo: Vec<HistoryGroup>,
    boundary: bool,
}

impl SourceHistory {
    // Matches the vendored UndoManager bound. Never promise a group containing
    // transactions the canonical editor may already have evicted.
    const LIMIT: usize = 1000;

    fn sync(&mut self, revision: SourceStamp) {
        if self.revision != Some(revision) {
            self.undo.clear();
            self.redo.clear();
            self.revision = Some(revision);
            self.boundary = true;
        }
    }

    fn record(&mut self, before: SourceStamp, after: SourceStamp, cell: Option<(usize, usize)>) {
        self.sync(before);
        self.redo.clear();
        if !self.boundary
            && cell.is_some()
            && self
                .undo
                .last()
                .is_some_and(|group| group.cell == cell && group.steps < Self::LIMIT)
        {
            self.undo.last_mut().unwrap().steps += 1;
        } else {
            self.undo.push(HistoryGroup { cell, steps: 1 });
        }
        while self.undo.iter().map(|group| group.steps).sum::<usize>() > Self::LIMIT {
            self.undo.remove(0);
        }
        self.revision = Some(after);
        self.boundary = cell.is_none();
    }

    fn steps(&mut self, direction: HistoryDirection, revision: SourceStamp) -> usize {
        self.sync(revision);
        let stack = match direction {
            HistoryDirection::Undo => &self.undo,
            HistoryDirection::Redo => &self.redo,
        };
        stack.last().map_or(1, |group| group.steps)
    }

    fn completed(&mut self, direction: HistoryDirection, revision: SourceStamp) {
        let group = match direction {
            HistoryDirection::Undo => self.undo.pop(),
            HistoryDirection::Redo => self.redo.pop(),
        }
        .unwrap_or(HistoryGroup {
            cell: None,
            steps: 1,
        });
        match direction {
            HistoryDirection::Undo => self.redo.push(group),
            HistoryDirection::Redo => self.undo.push(group),
        }
        self.revision = Some(revision);
        self.boundary = true;
    }
}

pub(in crate::reader_editor) struct TableGrid {
    reader: WeakEntity<Reader>,
    input: Entity<EditorState>,
    binding: Option<Binding>,
    cells: Vec<Vec<Entity<TextareaState>>>,
    subscriptions: Vec<Subscription>,
    _source: Subscription,
    _input_watch: Subscription,
    idle: Option<Task<()>>,
    seen_document: gpui_component::Rope,
    formatted_stamp: Option<SourceStamp>,
    committed_table_edit: Option<(SourceStamp, Range<usize>)>,
    pending_silent: Option<(SourceStamp, gpui_component::Rope)>,
    error: Option<&'static str>,
    provider: Option<Arc<CachedProvider>>,
    base: Option<Arc<dyn ProjectionProvider>>,
    preparing: Option<Task<()>>,
    installed_epoch: Option<u64>,
    session: u64,
    selected: (usize, usize),
    /// Rendered cell hit that opened this grid; only the matching second press
    /// may reveal source instead of selecting a word in the new cell input.
    source_press: Option<(SourceStamp, Point<Pixels>, usize)>,
    source_history: SourceHistory,
    #[cfg(test)]
    history_dispatches: usize,
}

impl TableGrid {
    pub(in crate::reader_editor) fn new(
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            reader: reader.downgrade(),
            _input_watch: cx.observe(&input, |_, _, cx| cx.notify()),
            _source: cx.subscribe_in(
                &input,
                window,
                |this, _, event: &SourceMutation, window, cx| {
                    this.source_changed(*event, window, cx);
                },
            ),
            idle: None,
            seen_document: input.read(cx).text().clone(),
            formatted_stamp: None,
            committed_table_edit: None,
            pending_silent: None,
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
            source_press: None,
            source_history: SourceHistory::default(),
            #[cfg(test)]
            history_dispatches: 0,
        }
    }

    /// Exact source APIs intentionally emit Silent mutations, just like Undo.
    /// Attribute an accepted table edit before its deferred source event runs.
    pub(super) fn mark_source_edit(&mut self, stamp: SourceStamp, range: Range<usize>) {
        self.committed_table_edit = Some((stamp, range));
    }

    /// Open only an exact currently classified top-level table. The caller may
    /// use the existing table cell press callback; source offsets stay absolute.
    pub(in crate::reader_editor) fn open(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.retain_pending_cells(cx) {
            return false;
        }
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
        self.input
            .update(cx, |state, _| state.preserve_source_anchor(range.start));
        self.source_history.sync(stamp);
        self.source_history.boundary = true;
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
        let source = self.input.read(cx).source_snapshot();
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
        self.source_history.boundary = true;
        self.selected = (row, column);
        cell.update(cx, |cell, cx| {
            cell.focus(window, cx);
            cell.select_all(window, cx);
        });
        true
    }

    /// A painted callback may outlive its source revision. Check both stamp
    /// and exact bytes before replacing the existing source click behavior.
    #[allow(clippy::too_many_arguments)]
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

    pub(in crate::reader_editor) fn remember_source_press(
        &mut self,
        stamp: SourceStamp,
        position: Point<Pixels>,
        offset: usize,
    ) {
        self.source_press = Some((stamp, position, offset));
    }

    fn continue_source_press(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((stamp, position, offset)) = self.source_press.take() else {
            return;
        };
        if event.button != MouseButton::Left
            || event.click_count != 2
            || event.modifiers != Modifiers::default()
            || (event.position.x - position.x).abs() > px(5.)
            || (event.position.y - position.y).abs() > px(5.)
            || self.input.read(cx).source_stamp() != stamp
            || !self.current(cx)
            || self.retain_pending_cells(cx)
            || self
                .cells
                .iter()
                .flatten()
                .any(|cell| cell.read(cx).ime_settling(cx))
        {
            return;
        }
        let anchor = self.binding.as_ref().unwrap().table.start;
        self.input
            .update(cx, |state, _| state.preserve_source_anchor(anchor));
        self.close(cx);
        self.input.update(cx, |state, cx| {
            state.set_selected_range_preserving_view(offset..offset, anchor, cx);
            state.focus(window, cx);
        });
        cx.stop_propagation();
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
            source: self.input.read(cx).source_snapshot(),
            table: binding.table.clone(),
        });
        let anchor = binding.table.start;
        self.input.update(cx, |input, cx| {
            // The asynchronously prepared grid changes the block's height.
            // Keep the visible block in place instead of anchoring the blurred
            // source editor's old caret below the table.
            input.preserve_source_anchor(anchor);
            input.set_projection_provider(Some(provider), cx)
        });
        self.installed_epoch = Some(self.input.read(cx).presentation_epoch());
    }

    pub(in crate::reader_editor) fn close(&mut self, cx: &mut Context<Self>) {
        if self.retain_pending_cells(cx) {
            return;
        }
        self.binding = None;
        self.source_press = None;
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
                    let mut state = TextareaState::new(window, cx).auto_grow(1, 6);
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
                            // Committed cell text is mirrored immediately into
                            // source. Discard only the nested private history;
                            // text, caret, selection and IME state stay intact.
                            cell.update(cx, |state, _| {
                                drop(state.take_edit_history());
                            });
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
        // SourceStamp includes the native input identity and every mutation,
        // including silent IME edits. Rechecking the whole note here adds no
        // validation and materializes it on each grid entry/return and repaint.
        state.is_editable() && state.source_stamp() == binding.stamp
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

    /// Rebuilding inputs must never discard preedit or a value refused by the
    /// source transaction. Check every cell, including one that lost focus.
    pub(in crate::reader_editor) fn retain_pending_cells(
        &mut self,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(binding) = &self.binding else {
            return false;
        };
        let pending = self.cells.iter().enumerate().any(|(r, row)| {
            row.iter().enumerate().any(|(c, cell)| {
                let cell = cell.read(cx);
                cell.ime_composing()
                    || cell.ime_settling(cx)
                    || binding.cells.value(r, c).map(model::cell_text).as_deref()
                        != Some(cell.value().as_ref())
            })
        });
        if pending {
            let message = Some(
                "Copy or correct the cell text before leaving the grid; it has not been saved to source.",
            );
            if self.error != message {
                self.error = message;
                cx.notify();
            }
        }
        pending
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
        if self.apply(change, Some((row, col)), window, cx) {
            self.binding.as_mut().unwrap().cells = cells;
        }
    }

    fn apply(
        &mut self,
        change: model::Change,
        typing_cell: Option<(usize, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.current(cx) {
            return false;
        }
        let binding = self.binding.as_ref().unwrap();
        let start = binding.table.start;
        let before = binding.stamp;
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
            self.source_history
                .record(before, state.source_stamp(), typing_cell);
            self.committed_table_edit =
                Some((state.source_stamp(), start..start + replacement_len));
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
        if self.retain_pending_cells(cx) {
            cx.stop_propagation();
            return;
        }
        self.source_history.boundary = true;
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
        if self.apply(change, None, window, cx) {
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
            cx.stop_propagation();
            return;
        }
        if self.retain_pending_cells(cx) {
            cx.stop_propagation();
            return;
        }
        let Some(range) = self.active_range(cx) else {
            return;
        };
        // active_range checked this binding against the current revision.
        // F2/Esc need no whole-note source materialization.
        let document = &self.binding.as_ref().unwrap().document;
        let caret = super::super::block_entry::caret_outside(document, &range);
        self.close(cx);
        self.input.update(cx, |state, cx| {
            let selection = if select {
                // Keep the caret at the block start when opening its source.
                range.end..range.start
            } else {
                caret.map_or_else(|| range.clone(), |at| at..at)
            };
            state.set_selected_range_preserving_view(selection, range.start, cx);
            state.focus(window, cx);
        });
        cx.stop_propagation();
    }

    /// Capture at the cell boundary and target the source focus node directly.
    /// Window::dispatch_action defers dispatch; its focus/next-frame interplay
    /// must not send history back to a replacement cell's private Undo handler.
    fn history(
        &mut self,
        direction: HistoryDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        #[cfg(test)]
        {
            self.history_dispatches += 1;
        }
        if self.retain_pending_cells(cx) || !self.current(cx) {
            return;
        }
        let range = self.binding.as_ref().unwrap().table.clone();
        let previous_document = self.binding.as_ref().unwrap().document.clone();
        let selected = self.selected;
        let before = self.input.read(cx).source_stamp();
        let steps = self.source_history.steps(direction, before);
        let source_focus = self.input.focus_handle(cx);
        let mut applied = 0;
        for _ in 0..steps {
            let previous = self.input.read(cx).source_stamp();
            match direction {
                HistoryDirection::Undo => source_focus.dispatch_action(&Undo, window, cx),
                HistoryDirection::Redo => source_focus.dispatch_action(&Redo, window, cx),
            }
            if self.input.read(cx).source_stamp() == previous {
                break;
            }
            applied += 1;
        }
        let stamp = self.input.read(cx).source_stamp();
        if applied == steps {
            self.source_history.completed(direction, stamp);
        } else if applied != 0 {
            // The canonical history changed outside this ledger. Retain the
            // real source result; never drain unrelated history to hit a count.
            self.source_history = SourceHistory::default();
        }
        if applied != 0 {
            // Every input was committed before dispatch. The old cells now
            // differ intentionally from the Undo result, not from a refused
            // edit, so release them before admitting the restored revision.
            self.close(cx);
            let document = self.input.read(cx).value().to_string();
            let restored_start = idle::restored_start(&previous_document, &document, range.start);
            let snapshot = Snapshot::new("table-grid-history", 0, document.as_str());
            let classified = source_classifier::classify(&snapshot);
            let restored = classified
                .decorations_for(&snapshot)
                .ok()
                .and_then(|markers| {
                    markers
                        .iter()
                        .find(|marker| {
                            marker.kind == Kind::Table && marker.range.start == restored_start
                        })
                        .map(|marker| marker.range.clone())
                });
            if let Some(restored) = restored {
                if self.open(restored, window, cx) {
                    // Undo may remove the row/column created by Tab or an
                    // insertion action. Keep focus inside the restored grid.
                    let row = selected.0.min(self.cells.len() - 1);
                    let column = selected.1.min(self.cells[row].len() - 1);
                    self.focus_cell(row, column, window, cx);
                }
            }
        }
        // Nested source dispatch resets propagation state; stop the original
        // cell action again so no Textarea bubble handler can see it.
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
            .key_context("TableGrid")
            .debug_selector(|| "editable-table-grid".into())
            .flex()
            .flex_col()
            .font_family(reader_fonts::note(cx))
            .capture_any_mouse_down(cx.listener(|this, event, window, cx| {
                this.continue_source_press(event, window, cx);
            }))
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
                    this.source_history.boundary = true;
                    cx.stop_propagation();
                }),
            );
        root = root
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    // Capture the clicked cell before the popup takes focus. Its
                    // action may dispatch while no native cell is focused.
                    if let Some(selected) = this.cells.iter().enumerate().find_map(|(r, row)| {
                        row.iter()
                            .position(|cell| cell.read(cx).focus_handle(cx).is_focused(window))
                            .map(|c| (r, c))
                    }) {
                        this.selected = selected;
                    }
                    this.source_history.boundary = true;
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation());
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
                let pending = self.binding.as_ref().is_some_and(|binding| {
                    binding.cells.value(r, c).map(model::cell_text).as_deref()
                        != Some(cell.read(cx).value().as_ref())
                });
                let input = Textarea::new(cell)
                    .disabled(stale)
                    .context_menu(|menu, _, _| {
                        menu.separator().submenu(
                            "Table",
                            gpui_component::native_menu::NativeMenu::from(super::menu()),
                        )
                    });
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
                        .when(pending, |cell| {
                            cell.border_color(palette.danger)
                                .debug_selector(move || format!("uncommitted-table-cell-{r}-{c}"))
                        })
                        .capture_action(cx.listener(|this, _: &Undo, window, cx| {
                            this.history(HistoryDirection::Undo, window, cx)
                        }))
                        .capture_action(cx.listener(|this, _: &Redo, window, cx| {
                            this.history(HistoryDirection::Redo, window, cx)
                        }))
                        .child(input),
                );
            }
            root = root.child(line);
        }
        if let Some(error) = self
            .error
            .or(stale.then_some("The note changed. Reopen the table before editing."))
        {
            root = root.child(
                div()
                    .debug_selector(|| "table-grid-error".into())
                    .text_sm()
                    .text_color(palette.danger)
                    .child(error),
            );
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
        .capture_action(cx.listener(|this, _: &Undo, window, cx| {
            this.history(HistoryDirection::Undo, window, cx)
        }))
        .capture_action(cx.listener(|this, _: &Redo, window, cx| {
            this.history(HistoryDirection::Redo, window, cx)
        }))
        .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
            this.command(Command::Next, window, cx);
        }))
        .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
            this.command(Command::Previous, window, cx);
        }))
        .on_action(cx.listener(|this, _: &TableInsertRow, window, cx| {
            this.command(Command::InsertRow, window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableMoveRowUp, window, cx| {
            this.command(Command::MoveRowUp, window, cx)
        }))
        .on_action(cx.listener(|this, _: &TableMoveRowDown, window, cx| {
            this.command(Command::MoveRowDown, window, cx)
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
        let (first, last) = if let Some(text) = projection.display_rope() {
            if from > text.len() || to > text.len() {
                return None;
            }
            (
                text.offset_to_point(from).row,
                text.offset_to_point(to).row + 1,
            )
        } else {
            let text = projection.text();
            (
                text.get(..from)?.bytes().filter(|&b| b == b'\n').count(),
                text.get(..to)?.bytes().filter(|&b| b == b'\n').count() + 1,
            )
        };
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
        if self.table.start > self.table.end
            || !self.source.text.is_char_boundary(self.table.start)
            || !self.source.text.is_char_boundary(self.table.end)
        {
            return None;
        }
        self.source
            .text
            .rope()
            .slice(self.table.clone())
            .to_string()
            .hash(&mut hash);
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
    fn display_rope(&self) -> Option<&gpui_component::Rope> {
        self.base.display_rope()
    }
    fn display_revision(&self) -> Option<u64> {
        self.base.display_revision()
    }
    fn display_edit(&self, previous: u64) -> Option<gpui_component::input::projection::EditDelta> {
        self.base.display_edit(previous)
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
mod source_history_tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn stamp(generation: u64) -> SourceStamp {
        SourceStamp {
            document: 1,
            generation,
        }
    }

    #[test]
    fn a_committed_cell_edit_routes_all_source_transactions_and_redoes_the_same_group() {
        let mut history = SourceHistory::default();
        for generation in 0..23 {
            history.record(stamp(generation), stamp(generation + 1), Some((1, 0)));
        }
        assert_eq!(history.steps(HistoryDirection::Undo, stamp(23)), 23);
        history.completed(HistoryDirection::Undo, stamp(46));
        assert_eq!(history.steps(HistoryDirection::Redo, stamp(46)), 23);
        history.completed(HistoryDirection::Redo, stamp(69));
        assert_eq!(history.steps(HistoryDirection::Undo, stamp(69)), 23);
    }

    #[test]
    fn navigation_cell_changes_and_structural_actions_are_distinct_source_boundaries() {
        let mut history = SourceHistory::default();
        history.record(stamp(0), stamp(1), Some((1, 0)));
        history.record(stamp(1), stamp(2), Some((1, 0)));
        history.boundary = true;
        history.record(stamp(2), stamp(3), Some((1, 0)));
        history.record(stamp(3), stamp(4), Some((1, 1)));
        history.record(stamp(4), stamp(5), None);
        assert_eq!(
            history
                .undo
                .iter()
                .map(|group| group.steps)
                .collect::<Vec<_>>(),
            [2, 1, 1, 1]
        );
    }

    #[test]
    fn an_external_source_revision_never_reuses_the_old_native_transaction_count() {
        let mut history = SourceHistory::default();
        for generation in 0..8 {
            history.record(stamp(generation), stamp(generation + 1), Some((1, 0)));
        }
        assert_eq!(
            history.steps(HistoryDirection::Undo, stamp(8)),
            8,
            "positive control: owned source revision"
        );
        assert_eq!(
            history.steps(HistoryDirection::Undo, stamp(10)),
            1,
            "external edit+Undo ABA invalidates the ledger"
        );
        assert!(history.undo.is_empty());
        assert!(history.redo.is_empty());
    }

    #[test]
    fn editing_after_undo_discards_only_the_old_group_mapping_for_redo() {
        let mut history = SourceHistory::default();
        history.record(stamp(0), stamp(1), Some((1, 0)));
        history.record(stamp(1), stamp(2), Some((1, 0)));
        history.completed(HistoryDirection::Undo, stamp(4));
        assert_eq!(history.redo[0].steps, 2);
        history.record(stamp(4), stamp(5), Some((1, 0)));
        assert!(history.redo.is_empty());
        assert_eq!(history.steps(HistoryDirection::Undo, stamp(5)), 1);
    }

    #[test]
    fn groups_never_include_transactions_evicted_by_the_native_history_bound() {
        let mut history = SourceHistory::default();
        for generation in 0..1100 {
            history.record(stamp(generation), stamp(generation + 1), Some((1, 0)));
        }
        assert_eq!(history.steps(HistoryDirection::Undo, stamp(1100)), 100);
        assert!(
            history.undo.iter().map(|group| group.steps).sum::<usize>() <= SourceHistory::LIMIT
        );
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn grid_wrapper_keeps_native_display_receipts_without_flattening_notes() {
        use gpui_component::{
            input::projection::{take_flattened_bytes, SourceText},
            Rope,
        };
        use okilum_core::source_projection::revision::{reset_work, take_work};
        let text = "Before **bold**\n\n| A |\n|---|\n| row |\n\nAfter\n";
        let source = SourceSnapshot {
            stamp: SourceStamp {
                document: 2403,
                generation: 1,
            },
            text: SourceText::from_rope(Rope::from_str(text)),
        };
        let table = text.find('|').unwrap()..text.find("\n\nAfter").unwrap();
        let base: Arc<dyn ProjectionProvider> = Arc::new(CachedProvider::classify(source.clone()));
        let active = ActiveSource::default();
        let before = GridProvider {
            base: base.clone(),
            source: source.clone(),
            table: table.clone(),
        }
        .compose(&source, &active)
        .unwrap();
        assert!(before.display_rope().is_some());
        let revision = before.display_revision().unwrap();
        let mut rope = source.text.rope().clone();
        rope.remove(1..2);
        rope.insert(1, "q");
        let edited = SourceSnapshot {
            stamp: SourceStamp {
                generation: 2,
                ..source.stamp
            },
            text: source.text.edited(rope, 1..2, 1),
        };
        let provider = GridProvider {
            base,
            source: edited.clone(),
            table,
        };
        reset_work();
        take_flattened_bytes();
        let after = provider.compose(&edited, &active).unwrap();
        let work = take_work();
        assert_eq!(take_flattened_bytes(), 0);
        assert_eq!(work.flattened_bytes, 0);
        assert_eq!(work.display_builds, 0);
        assert!(after.display_edit(revision).is_some());
        assert_eq!(before.blocks()[0].lines, after.blocks()[0].lines);
        assert_eq!(before.blocks()[0].key, after.blocks()[0].key);
        // Fresh uncached native text proves this probe detects a full snapshot.
        let control = SourceText::from_rope(edited.text.rope().clone());
        let _ = control.as_ref();
        assert!(take_flattened_bytes() >= text.len());
    }

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
            text: gpui_component::input::projection::SourceText::from(
                "Before\n\n| A | B |\n|---|---|\n| one | two |\n\nAfter",
            ),
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
        stale.text =
            gpui_component::input::projection::SourceText::from(source.text.replace("one", "new"));
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
        // The grid's keyboard exit reuses its checked immutable binding. Cell
        // checks may read their small values; copying the note is redundant.
        crate::reader_editor::block_entry::take_source_work_bytes();
        visual.simulate_keystrokes("f2");
        assert!(
            crate::reader_editor::block_entry::take_source_work_bytes() < source.len(),
            "F2 from the grid must not materialize the whole note"
        );
        let table = source.find("| A").unwrap()..source.find("\n\nAfter").unwrap();
        input.read_with(visual, |state, _| {
            assert_eq!(state.selected_range(), table);
            assert_eq!(state.cursor(), table.start);
        });
        grid.update_in(visual, |grid, window, cx| {
            assert!(grid.open(table, window, cx));
            assert!(grid.focus_cell(1, 0, window, cx));
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("shift-tab");
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(grid.read(cx).cells[0][1]
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
        });
        visual.simulate_keystrokes("tab");
        visual.run_until_parked();
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
        assert_eq!(
            grid.read_with(visual, |grid, _| grid.history_dispatches),
            1,
            "Undo must cross the source-routing capture handler exactly once"
        );
        visual.dispatch_action(Redo);
        visual.run_until_parked();
        assert_eq!(
            input.read_with(visual, |state, _| state.value().to_string()),
            expected,
            "Redo restores the complete encoded cell edit through source history"
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            input.read_with(visual, |state, _| state.value().to_string()),
            source
        );
        assert_eq!(grid.read_with(visual, |grid, _| grid.history_dispatches), 3);
        let range = source.find("| A").unwrap()..source.find("\n\nAfter").unwrap();
        grid.update_in(visual, |grid, window, cx| {
            assert!(
                !grid.open_cell(range.clone(), old_stamp, &source[range], 1, 0, window, cx),
                "a stale painted cell callback cannot bind the restored source revision"
            );
        });
        // A refused source edit remains in its input, even after focus moves
        // elsewhere. None of the operations that rebuild inputs may lose it.
        let pending = "cannot encode\0yet";
        let pending_cell = grid.read_with(visual, |grid, _| grid.cells[1][0].clone());
        pending_cell.update_in(visual, |cell, window, cx| {
            cell.set_value(pending, window, cx);
        });
        grid.update_in(visual, |grid, window, cx| {
            grid.replace(1, 0, pending, window, cx);
            assert!(
                grid.error.is_some(),
                "positive control: source refused the cell"
            );
            grid.focus_cell(1, 1, window, cx);
            grid.command(Command::InsertRow, window, cx);
            grid.history(HistoryDirection::Undo, window, cx);
            grid.history(HistoryDirection::Redo, window, cx);
            grid.source_mode(true, window, cx);
            let stamp = grid.input.read(cx).source_stamp();
            let table = source.find("| A").unwrap()..source.find("\n\nAfter").unwrap();
            assert!(!grid.open_cell(table.clone(), stamp, &source[table], 0, 0, window, cx));
            assert_eq!(grid.cells[1][0], pending_cell);
            assert_eq!(grid.cells[1][0].read(cx).value().as_ref(), pending);
            assert_eq!(grid.input.read(cx).value().as_ref(), source);
            assert!(grid.binding.is_some());
        });
        reader.update_in(visual, |reader, window, cx| {
            assert!(!reader.save_source(cx), "uncommitted text blocks Save");
            reader.set_live_preview(false, window, cx);
            assert!(reader.editing.as_ref().unwrap().live_preview.enabled);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("uncommitted-table-cell-1-0").is_some());
        assert!(visual.debug_bounds("table-grid-error").is_some());
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            source
        );
        // Correcting the refused input resumes source edits and source Undo;
        // the guards must not leave the persistent editor permanently blocked.
        pending_cell.update_in(visual, |cell, window, cx| {
            cell.set_value("corrected|value\nnext", window, cx);
        });
        grid.update_in(visual, |grid, window, cx| {
            grid.replace(1, 0, "corrected|value\nnext", window, cx);
            assert!(grid.error.is_none());
            assert_eq!(
                grid.input.read(cx).value().as_ref(),
                source.replace("one", "corrected\\|value<br>next")
            );
        });
        reader.update(visual, |reader, cx| assert!(reader.save_source(cx)));
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            source.replace("one", "corrected\\|value<br>next")
        );
        grid.update_in(visual, |grid, window, cx| {
            grid.history(HistoryDirection::Undo, window, cx);
            assert_eq!(grid.input.read(cx).value().as_ref(), source);
            grid.focus_cell(1, 0, window, cx);
        });
        // Structural actions must keep the grid mounted and remain canonical
        // source transactions, rather than switching to a one-shot editor.
        // Undo recreated the focused cell: settle its dispatch tree before a
        // user action can target that cell, including with retained geometry.
        visual.run_until_parked();
        for (name, action) in [
            ("insert row", TableInsertRow.boxed_clone()),
            ("insert column", TableInsertColumn.boxed_clone()),
            ("align center", TableAlignCenter.boxed_clone()),
            ("format", TableFormat.boxed_clone()),
        ] {
            visual.update(|window, cx| window.dispatch_action(action, cx));
            visual.run_until_parked();
            let changed = input.read_with(visual, |input, _| input.value().to_string());
            assert_ne!(changed, source, "{name} must reach the restored grid");
            assert!(changed.starts_with("Before\n\n"));
            assert!(changed.ends_with("\n\nAfter\n"));
            assert!(visual.debug_bounds("editable-table-grid").is_some());
            visual.dispatch_action(Undo);
            visual.run_until_parked();
            assert_eq!(
                input.read_with(visual, |input, _| input.value().to_string()),
                source
            );
        }
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
