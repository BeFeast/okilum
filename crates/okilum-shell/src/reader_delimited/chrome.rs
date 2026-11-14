//! Table descriptions use the same slots in Reader, ZIP and standalone hosts.
use super::*;
use reader_viewer_chrome::{ViewerAction, ViewerChrome};

impl TablePreview {
    pub(crate) fn without_header(mut self) -> Self {
        self.own_header = false;
        self
    }
    pub(crate) fn bytes(&self) -> Option<u64> {
        self.file_bytes
    }
    pub(crate) fn failure(&self) -> Option<&'static str> {
        self.error
    }
    /// Column names for Contents (#2814 9d): the header row's cells, or
    /// «Column N» as the grid shows a table without one. A blank header cell
    /// reads «Column N» too.
    /// Each column as `(label, full name)`: the label is cut like a grid
    /// cell; the full name is for the tooltip, so long headers that share a
    /// prefix stay apart.
    pub(crate) fn column_names(&self) -> Vec<(String, String)> {
        let Some(table) = &self.table else {
            return Vec::new();
        };
        (0..table.columns)
            .map(|column| {
                let cell = if self.header {
                    table.cell(0, column)
                } else {
                    ""
                };
                if cell.trim().is_empty() {
                    let name = format!("Column {}", column + 1);
                    (name.clone(), name)
                } else {
                    (visible(cell), cell.to_owned())
                }
            })
            .collect()
    }
    /// Changes whenever the names may: another snapshot or header choice.
    pub(crate) fn columns_key(&self) -> (usize, bool) {
        (
            self.table
                .as_ref()
                .map_or(0, |table| Arc::as_ptr(table) as usize),
            self.header,
        )
    }
    /// The column of the selected cell.
    pub(crate) fn selected_column(&self) -> Option<usize> {
        self.end.map(|cell| cell.column)
    }
    /// A click in Contents: the grid scrolls sideways to `column` and the
    /// selection moves into it, in the same row (the first data row when
    /// nothing is selected). The rows do not scroll.
    pub(crate) fn reveal_column(
        &mut self,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(table) = &self.table else {
            return;
        };
        if self.updating || column >= table.columns {
            return;
        }
        // Rows shown now: the header row and the rows Find and sort left.
        let first = usize::from(self.header);
        let shown = first + self.order.len();
        if self.order.is_empty() {
            // No data row to select (a Find matched none): scroll only.
            self.reveal_column_offset(column);
            cx.notify();
            return;
        }
        let row = self
            .end
            .or(self.anchor)
            .map(|cell| cell.row)
            .filter(|&row| row < shown)
            .unwrap_or(first);
        let cell = Cell { row, column };
        self.anchor = Some(cell);
        self.end = Some(cell);
        self.reveal_column_offset(column);
        self.focus.focus(window, cx);
        cx.notify();
    }
    #[cfg(test)]
    pub(crate) fn select_for_tests(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(Cell { row, column }, false, window, cx);
    }
    #[cfg(test)]
    /// Through the stored choice, which the grid reads on every render.
    pub(crate) fn set_header_for_tests(&mut self, on: bool, cx: &mut Context<Self>) {
        self.header = !on;
        self.toggle_header(cx);
    }
    #[cfg(test)]
    pub(crate) fn selection_for_tests(&self) -> Option<(usize, usize)> {
        self.end.map(|cell| (cell.row, cell.column))
    }
    #[cfg(test)]
    pub(crate) fn horizontal_for_tests(&self) -> (f32, f32) {
        (
            -f32::from(self.horizontal.offset().x),
            f32::from(self.pane_bounds.get().size.width),
        )
    }
    pub(crate) fn chrome(&self, owner: Entity<Self>, live: bool, cx: &App) -> ViewerChrome {
        let mut chrome = ViewerChrome {
            find: Some(self.find_target(owner.clone(), live, cx)),
            compact_menu_id: Some("table-compact-more"),
            ..Default::default()
        };
        if !self.json() {
            let target = owner.clone();
            let mut header = ViewerAction::new(
                "table-first-row-header",
                Icon::default().path(brand::CODE_VIEW_TABLE_ICON),
                "First row is header",
                Rc::new(move |_, cx| target.update(cx, |s, cx| s.toggle_header(cx))),
            )
            .with_action(ToggleHeaderRow, cx);
            header.toggled = self.header;
            header.priority = 30;
            chrome.actions.push(header);
        }
        let target = owner.clone();
        let mut reset = ViewerAction::new(
            "table-reset-sort",
            Icon::new(IconName::RotateCw),
            "Reset sort",
            Rc::new(move |_, cx| target.update(cx, |s, cx| s.reset_sort(cx))),
        )
        .with_action(ResetTableSort, cx);
        reset.enabled = self.sort.is_some();
        reset.disabled_reason = self.sort.is_none().then(|| "Nothing is sorted".into());
        reset.priority = 31;
        chrome.actions.push(reset);
        let target = owner.clone();
        let mut copy = ViewerAction::new(
            "table-copy-cells",
            Icon::new(IconName::Copy),
            "Copy cells",
            Rc::new(move |window, cx| target.update(cx, |s, cx| s.copy_feedback(window, cx))),
        )
        .with_action(CopyCells, cx);
        copy.enabled = !self.updating && self.selection().is_some();
        copy.disabled_reason = (!copy.enabled).then(|| "Select cells to copy".into());
        chrome.copy_actions.push(copy);
        let target = owner.clone();
        let mut duck = ViewerAction::new(
            "table-duckdb",
            Icon::new(IconName::ExternalLink),
            if self.external_pending {
                "DuckDB is open"
            } else {
                "Open in DuckDB"
            },
            Rc::new(move |window, cx| target.update(cx, |s, cx| s.open_duckdb(window, cx))),
        )
        .with_action(OpenTableDuckDb, cx);
        duck.toggled = self.external_pending;
        duck.enabled = !self.external_pending;
        duck.disabled_reason = self.external_pending.then(|| "DuckDB is open".into());
        chrome.open_actions.push(duck);
        if self
            .table
            .as_ref()
            .is_some_and(|t| t.ragged || t.malformed || t.ambiguous)
        {
            let target = owner;
            let mut warning = ViewerAction::new(
                "table-warning",
                Icon::new(IconName::TriangleAlert).text_color(brand::palette(cx).warning),
                "Table has warnings",
                Rc::new(move |window, cx| target.update(cx, |s, cx| s.go_to_bad_row(window, cx))),
            )
            .with_action(GoToTableError, cx);
            warning.tooltip = self.notices().join(" ").into();
            warning.enabled = self.table.as_ref().and_then(|t| t.first_bad_row).is_some();
            warning.disabled_reason = (!warning.enabled)
                .then(|| "The table has a warning without an exact row location".into());
            chrome.state = Some(warning);
        }
        chrome
    }

    pub(super) fn toggle_header(&mut self, cx: &mut Context<Self>) {
        if self.json() {
            return;
        }
        self.header = !self.header;
        self.anchor = None;
        self.end = None;
        reader_ui_state::set_delimited_header(self.path.clone(), self.header, cx);
        self.refilter(cx);
    }
    pub(super) fn reset_sort(&mut self, cx: &mut Context<Self>) {
        if self.sort.take().is_some() {
            self.refilter(cx);
        }
    }
    pub(super) fn copy_feedback(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.updating {
            return;
        }
        if let Some((rows, columns)) = self.selection() {
            self.copy(cx);
            let cells = rows.len() * columns.len();
            reader_toast::transient(
                if cells == 1 {
                    "Copied 1 cell.".to_owned()
                } else {
                    format!("Copied {} cells.", grouped(cells))
                },
                window,
                cx,
            );
        }
    }
    pub(super) fn go_to_bad_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.table.as_ref().and_then(|t| t.first_bad_row) else {
            return;
        };
        if let Some(cell) = self.display_cell(Cell { row, column: 0 }) {
            self.select(cell, false, window, cx);
            self.reveal_cell(cell);
        } else {
            self.reveal_source = Some(Cell { row, column: 0 });
            self.query.clear();
            self.filter
                .as_ref()
                .unwrap()
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.focus.focus(window, cx);
            if !self.all && row >= INITIAL_ROWS + usize::from(self.header) {
                self.load(true, cx);
            } else {
                self.refilter(cx);
            }
        }
    }
    pub(super) fn notices(&self) -> Vec<&'static str> {
        let mut result = Vec::new();
        if let Some(table) = &self.table {
            if table.ragged {
                result.push("Some rows have a different number of columns.");
            }
            if table.malformed {
                result.push("Some rows have incomplete or unexpected quotes.");
            }
            if table.ambiguous {
                result.push("More than one separator fits this file.");
            }
            if self.all && table.limited {
                result.push(
                    "This file exceeds table preview limits. Open it externally to view the rest.",
                );
            }
            if table.rows.is_empty() && table.limited {
                result.push("No complete records fit in the initial preview.");
            }
        }
        if let Some(error) = self.error {
            result.push(error);
        }
        result
    }
    pub(super) fn status(&self, owner: Entity<Self>, cx: &App) -> reader_status_line::StatusLine {
        use reader_status_line::{StatusLine, Tone};
        let mut status = StatusLine::default();
        let notices = self.notices();
        let Some(table) = &self.table else {
            status.text = self.error.unwrap_or("Opening table…").into();
            return status;
        };
        let loaded = table.rows.len().saturating_sub(usize::from(self.header));
        let count = loaded.min(if self.all { usize::MAX } else { INITIAL_ROWS });
        let partial = table.limited || loaded > count;
        let size = self.file_bytes.map(file_size).unwrap_or_default();
        let mut text = if partial {
            format!(
                "First {} rows of a {size} file · {} columns",
                grouped(count),
                grouped(table.columns)
            )
        } else {
            format!(
                "{} rows · {} columns · {size}",
                grouped(count),
                grouped(table.columns)
            )
        };
        if let Some((column, descending)) = self.sort {
            let name = if self.header {
                table.cell(0, column).chars().take(80).collect::<String>()
            } else {
                format!("column {}", column + 1)
            };
            text.push_str(&format!(
                " · sorted by {name}, {} first",
                if descending { "largest" } else { "smallest" }
            ));
        }
        if self.updating {
            text.push_str(" · Updating rows…");
        } else if !self.query.is_empty() && self.order.is_empty() {
            text.push_str(" · No loaded rows match this filter.");
        }
        for notice in &notices {
            text.push_str(" · ");
            text.push_str(notice);
        }
        status.text = text.into();
        if !notices.is_empty() {
            status.tone = Tone::Warning;
            status.icon = Some(Icon::new(IconName::TriangleAlert));
        }
        if !self.all && partial {
            let target = owner;
            let mut more = ViewerAction::new(
                "table-load-more",
                Icon::new(IconName::ArrowDown),
                "Load more rows",
                Rc::new(move |_, cx| target.update(cx, |s, cx| s.load(true, cx))),
            )
            .with_action(LoadMoreTableRows, cx);
            more.labelled = true;
            more.enabled = !self.loading;
            status.actions.push(more);
        }
        status
    }

    #[inline(never)]
    pub(super) fn status_element(&self, cx: &Context<Self>) -> AnyElement {
        reader_status_line::render(&self.status(cx.entity(), cx), "table-status", None, cx)
    }

    #[inline(never)]
    pub(super) fn compact_header(&self, _window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let chrome = self.chrome(cx.entity(), true, cx);
        let pane = self.pane_bounds.clone();
        canvas(
            move |bounds, window, cx| {
                pane.set(bounds);
                let mut header = reader_viewer_chrome::render_compact_header(
                    &chrome,
                    bounds.size.width.as_f32(),
                    window,
                    cx,
                );
                header.layout_as_root(bounds.size.into(), window, cx);
                header.prepaint_at(bounds.origin, window, cx);
                header
            },
            |_, mut header, window, cx| header.paint(window, cx),
        )
        .w_full()
        .h(px(48.))
        .flex_none()
        .into_any_element()
    }
}

/// Decimal units as in the mockup («9.6 MB») and the ZIP viewer («6.9 KB»).
fn file_size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} bytes"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1_000.),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.),
    }
}

#[cfg(test)]
#[::core::prelude::v1::test]
fn table2812_status_size_uses_readable_units() {
    assert_eq!(file_size(512), "512 bytes");
    assert_eq!(file_size(6_900), "6.9 KB");
    assert_eq!(file_size(9_600_000), "9.6 MB");
}

fn grouped(value: usize) -> String {
    let raw = value.to_string();
    let mut result = String::new();
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && (raw.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}
