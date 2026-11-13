//! Read-only delimited tables; editing and recovery stay in FileEditor.
use super::*;
use gpui_base::ElementExt as _;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use okilum_core::delimited::{ReadLimits, Table};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use unicode_segmentation::UnicodeSegmentation as _;
mod chrome;
mod controls;
mod find;
#[cfg(feature = "data-view-native-harness")]
pub(crate) mod native_harness;
#[cfg(test)]
mod toolbar_tests;

const ROW_HEIGHT: f32 = 30.;
const COLUMN_WIDTH: f32 = 200.;
const GUTTER: f32 = 48.;
const INITIAL_ROWS: usize = 1000;
const PREVIEW_LIMITS: ReadLimits = ReadLimits {
    bytes: 1024 * 1024,
    rows: INITIAL_ROWS + 2,
    cells: 64_000,
    columns: 256,
    field_bytes: 64 * 1024,
};
const EXPANDED_LIMITS: ReadLimits = ReadLimits {
    bytes: 32 * 1024 * 1024,
    rows: 100_001,
    cells: 500_000,
    columns: 256,
    field_bytes: 64 * 1024,
};
actions!(
    reader_delimited,
    [
        CopyCells,
        ToggleHeaderRow,
        ResetTableSort,
        OpenTableDuckDb,
        LoadMoreTableRows,
        GoToTableError
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "secondary-c",
        CopyCells,
        Some("ReaderDelimited"),
    )]);
}

pub(crate) fn eligible(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("csv") || e.eq_ignore_ascii_case("tsv"))
}
/// Text files the Reader can open in its editor: tables, plain text and,
/// since #998, source code.
pub(crate) fn editable(rel: &str) -> bool {
    eligible(rel)
        || crate::reader_code_file::editable_format(rel)
        || Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
}

/// Document admission/restoration is separate from write authority.
pub(crate) fn previewable(rel: &str) -> bool {
    editable(rel) || crate::reader_code_file::eligible(rel)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cell {
    row: usize,
    column: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Saved {
    pub(crate) query: String,
    pub(crate) sensitive: bool,
    sort: Option<(usize, bool)>,
    anchor: Option<Cell>,
    end: Option<Cell>,
    hit_index: usize,
    horizontal: Point<Pixels>,
}

pub(crate) struct TablePreview {
    path: PathBuf,
    table: Option<Arc<Table>>,
    error: Option<&'static str>,
    loading: bool,
    all: bool,
    anchor: Option<Cell>,
    end: Option<Cell>,
    scroll: UniformListScrollHandle,
    horizontal: ScrollHandle,
    focus: FocusHandle,
    order: Vec<usize>,
    sort: Option<(usize, bool)>,
    filter: Option<Entity<InputState>>,
    query: String,
    revision: u64,
    updating: bool,
    header: bool,
    projection_all: bool,
    visible_columns: Range<usize>,
    external_error: Option<String>,
    external_pending: bool,
    #[cfg(test)]
    duckdb_override: Option<Option<PathBuf>>,
    own_header: bool,
    file_bytes: Option<u64>,
    case_sensitive: bool,
    hits: Vec<Cell>,
    reveal_source: Option<Cell>,
    restored: Option<Saved>,
    hit_index: usize,
    #[cfg(test)]
    projections: usize,
    projection_task: Option<Task<()>>,
    projection_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// A scan was stopped while Table was hidden and reruns on return.
    resume_pending: bool,
    pane_bounds: Rc<std::cell::Cell<Bounds<Pixels>>>,
}
impl TablePreview {
    pub(crate) fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find in loaded rows"));
        cx.subscribe_in(
            &filter,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value().to_string();
                    if this.query != query {
                        this.query = query;
                        this.schedule_filter(cx);
                    }
                }
                InputEvent::PressEnter { shift, .. } => {
                    this.step_hit(if *shift { -1 } else { 1 }, window, cx)
                }
                _ => {}
            },
        )
        .detach();
        let mut preview = Self {
            path,
            table: None,
            error: None,
            loading: false,
            all: false,
            anchor: None,
            end: None,
            scroll: UniformListScrollHandle::new(),
            horizontal: ScrollHandle::new(),
            focus: cx.focus_handle(),
            order: Vec::new(),
            sort: None,
            filter: Some(filter),
            query: String::new(),
            revision: 0,
            updating: false,
            header: true,
            projection_all: false,
            visible_columns: 0..0,
            external_error: None,
            external_pending: false,
            #[cfg(test)]
            duckdb_override: None,
            own_header: true,
            file_bytes: None,
            case_sensitive: reader_ui_state::find_case_sensitive(cx),
            hits: Vec::new(),
            reveal_source: None,
            restored: None,
            hit_index: 0,
            #[cfg(test)]
            projections: 0,
            projection_task: None,
            projection_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            resume_pending: false,
            pane_bounds: Default::default(),
        };
        preview.load(false, cx);
        preview
    }
    fn load(&mut self, all: bool, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let load_path = self.path.clone();
        let task = cx.background_executor().spawn(async move {
            std::fs::File::open(&load_path).and_then(|file| {
                let bytes = file.metadata()?.len();
                let result = if load_path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                {
                    Table::read_json(file, if all { EXPANDED_LIMITS } else { PREVIEW_LIMITS })
                } else {
                    Table::read(
                        file,
                        load_path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("tsv")),
                        if all { EXPANDED_LIMITS } else { PREVIEW_LIMITS },
                    )
                };
                result.map(|table| (table, bytes))
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok((table, bytes)) => {
                        this.file_bytes = Some(bytes);
                        this.order.clear();
                        this.table = Some(Arc::new(table));
                        this.all = all;
                        // The file may have changed between reads; never copy
                        // a selection against a different source snapshot.
                        this.anchor = None;
                        this.end = None;
                        this.refilter(cx);
                    }
                    Err(error) => {
                        diagnostic_error!("Cannot read delimited file: {error:#}");
                        this.error = Some(if this.json() {
                            "This JSON couldn’t be shown as a table. Use Raw to inspect it."
                        } else if error.kind() == std::io::ErrorKind::InvalidData {
                            "This table couldn’t be read as UTF-8."
                        } else {
                            "This table couldn’t be read."
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn ready(&self) -> bool {
        self.table.is_some()
    }
    /// The grid or its Find input holds focus; both unmount when Table is left.
    pub(crate) fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
            || self
                .filter
                .as_ref()
                .is_some_and(|input| input.focus_handle(cx).is_focused(window))
    }
    pub(crate) fn focus_filter(&self, window: &mut Window, cx: &mut App) -> bool {
        let Some(input) = &self.filter else {
            return false;
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        true
    }
    fn json(&self) -> bool {
        self.path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    }
    fn sort_column(&mut self, column: usize, cx: &mut Context<Self>) {
        self.sort = match self.sort {
            Some((at, false)) if at == column => Some((column, true)),
            Some((at, true)) if at == column => None,
            _ => Some((column, false)),
        };
        self.refilter(cx);
    }
    fn selected(&self, cell: Cell) -> bool {
        self.selection()
            .is_some_and(|(rows, cols)| rows.contains(&cell.row) && cols.contains(&cell.column))
    }
    fn selection(&self) -> Option<(Range<usize>, Range<usize>)> {
        let (a, b) = (self.anchor?, self.end?);
        Some((
            a.row.min(b.row)..a.row.max(b.row) + 1,
            a.column.min(b.column)..a.column.max(b.column) + 1,
        ))
    }
    fn select(&mut self, cell: Cell, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.updating {
            return;
        }
        if !extend || self.anchor.is_none() {
            self.anchor = Some(cell);
        }
        self.end = Some(cell);
        self.focus.focus(window, cx);
        cx.notify();
    }
    fn copy(&self, cx: &mut Context<Self>) {
        if let (Some(table), Some((rows, columns))) = (&self.table, self.selection()) {
            let start = usize::from(self.header);
            let sources: Vec<_> = rows
                .filter_map(|row| {
                    if self.header && row == 0 {
                        Some(0)
                    } else {
                        self.order.get(row.checked_sub(start)?).copied()
                    }
                })
                .collect();
            let copied = table.copy_rows(&sources, columns);
            cx.write_to_clipboard(ClipboardItem::new_string(copied));
        }
    }
    fn row(&self, row: usize, header: bool, cx: &mut Context<Self>) -> AnyElement {
        let table = self.table.as_ref().unwrap();
        let palette = brand::palette(cx);
        let header_present = self.header;
        let source_row = if header {
            0
        } else {
            self.order[row - usize::from(self.header)]
        };
        h_flex()
            .id(("delimited-row", row))
            .debug_selector(move || format!("delimited-row-{row}"))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .w(px(GUTTER + COLUMN_WIDTH * table.columns as f32))
            .border_b_1()
            .border_color(palette.border_subtle)
            .when(header, |v| {
                v.bg(palette.surface).font_weight(FontWeight::SEMIBOLD)
            })
            .child(
                div()
                    .flex_none()
                    .w(px(GUTTER))
                    .px_2()
                    .text_align(TextAlign::Right)
                    .text_color(palette.text_muted)
                    .child(if header {
                        String::new()
                    } else {
                        (row + usize::from(!header_present)).to_string()
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(COLUMN_WIDTH * self.visible_columns.start as f32)),
            )
            .children(self.visible_columns.clone().map(|column| {
                let cell = Cell { row, column };
                let value = table.cell(source_row, column).to_owned();
                let text = if header && !self.header {
                    format!("Column {}", column + 1)
                } else {
                    visible(&value)
                };
                div()
                    .id((
                        "delimited-cell",
                        row.wrapping_mul(table.columns).wrapping_add(column),
                    ))
                    .debug_selector(move || format!("delimited-cell-{row}-{column}"))
                    .flex_none()
                    .w(px(COLUMN_WIDTH))
                    .h(px(ROW_HEIGHT))
                    .px_3()
                    .py_1()
                    .border_l_1()
                    .border_color(palette.border_subtle)
                    .text_align(alignment(&value))
                    .overflow_hidden()
                    .when(self.selected(cell), |v| v.bg(palette.selected))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1()
                            .child(div().flex_1().min_w_0().truncate().child(text))
                            .when(header, |row| {
                                row.child(
                                    Icon::new(match self.sort {
                                        Some((at, true)) if at == column => IconName::ArrowDown,
                                        Some((at, false)) if at == column => IconName::ArrowUp,
                                        _ => IconName::ChevronsUpDown,
                                    })
                                    .small(),
                                )
                            }),
                    )
                    .hoverable_tooltip(move |window, cx| cell_tooltip(value.clone(), window, cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            if header {
                                this.sort_column(column, cx);
                            } else {
                                this.select(cell, event.modifiers.shift, window, cx);
                            }
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                            if !header
                                && event.pressed_button == Some(MouseButton::Left)
                                && this.anchor.is_some()
                                && this.end != Some(cell)
                            {
                                this.select(cell, true, window, cx);
                            }
                        }),
                    )
            }))
            .child(div().flex_none().w(px(
                COLUMN_WIDTH * (table.columns - self.visible_columns.end) as f32
            )))
            .into_any_element()
    }
}

/// GPUI's horizontal-only div maps a vertical wheel onto X. Own both axes
/// here instead, before the row list can remap an X-only event onto Y.
fn table_wheel(horizontal: ScrollHandle, rows: UniformListScrollHandle) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            let hitbox = hitbox.clone();
            let horizontal = horizontal.clone();
            let vertical = rows.0.borrow().base_handle.clone();
            let owner = window.current_view();
            let line_height = window.line_height();
            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                if !phase.capture() || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let mut delta = event.delta.pixel_delta(line_height);
                if event.modifiers.shift {
                    // Windows already normalizes Shift+wheel to X; other
                    // backends can deliver the original Y. Never apply both.
                    if delta.x == px(0.) {
                        delta.x = delta.y;
                    }
                    delta.y = px(0.);
                }
                let old_x = horizontal.offset();
                let old_y = vertical.offset();
                let x = point(
                    (old_x.x + delta.x).clamp(-horizontal.max_offset().x, px(0.)),
                    old_x.y,
                );
                let y = point(
                    old_y.x,
                    (old_y.y + delta.y).clamp(-vertical.max_offset().y, px(0.)),
                );
                if x != old_x || y != old_y {
                    horizontal.set_offset(x);
                    vertical.set_offset(y);
                    cx.notify(owner);
                }
                // Consume at the edge too: an X-only wheel must never become
                // row scrolling in the uniform_list's default listener.
                if delta.x != px(0.) || delta.y != px(0.) {
                    cx.stop_propagation();
                }
            });
        },
    )
    .absolute()
    .size_full()
}

fn cell_tooltip(value: String, window: &mut Window, cx: &mut App) -> AnyView {
    let scroll = ScrollHandle::new();
    let rows = Rc::new(RefCell::new(None::<(f32, SharedString, Vec<TooltipRow>)>));
    gpui_component::tooltip::Tooltip::element(move |window, cx| {
        // Leave room for the tooltip's padding/margin and the window edges.
        // A retained field can be 64 KiB, so wrapping alone is insufficient:
        // keep the tooltip hoverable and let the reader scroll to its end.
        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) - 64.).clamp(1., 480.);
        let height = (f32::from(viewport.height) - 64.).clamp(1., 360.);
        // GPUI's wrapped text loses RTL-first paragraphs (one stray glyph, the
        // rest off-row), so each row is wrapped here and shaped as one line.
        let family = reader_fonts::note(cx);
        let font_size = rems(0.875).to_pixels(window.rem_size());
        let mut cached = rows.borrow_mut();
        if cached
            .as_ref()
            .is_none_or(|(at, font, _)| *at != width || *font != family)
        {
            let font = Font {
                family: family.clone(),
                ..window.text_style().font()
            };
            let text_system = window.text_system().clone();
            let measure = |text: &str| {
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: Hsla::default(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                f32::from(text_system.layout_line(text, font_size, &[run], None).width)
            };
            // A pixel of slack absorbs kerning across measured segments.
            *cached = Some((
                width,
                family.clone(),
                tooltip_rows(&value, width - 1., measure),
            ));
        }
        let rows = cached
            .as_ref()
            .map(|(_, _, rows)| rows.clone())
            .unwrap_or_default();
        div()
            .id("delimited-cell-tooltip")
            .debug_selector(|| "delimited-cell-tooltip".into())
            // The popup covers neighbouring cells: they must not count as
            // hovered (their tooltip would replace this one) nor take the
            // wheel (the table would scroll underneath) (#946).
            .occlude()
            .on_prepaint(gpui_base::GlobalState::record_native_overlay)
            .w(px(width))
            .max_h(px(height))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(
                v_flex()
                    .id("delimited-cell-tooltip-value")
                    .debug_selector(|| "delimited-cell-tooltip-value".into())
                    .w_full()
                    .font_family(family)
                    .text_size(font_size)
                    .children(rows.into_iter().enumerate().map(|(ix, row)| {
                        div()
                            .debug_selector(move || format!("delimited-cell-tooltip-row-{ix}"))
                            .w_full()
                            .whitespace_nowrap()
                            .text_align(if row.rtl {
                                TextAlign::Right
                            } else {
                                TextAlign::Left
                            })
                            // Keep blank lines one row tall.
                            .child(if row.text.is_empty() {
                                SharedString::from(" ")
                            } else {
                                row.text
                            })
                    })),
            )
            .vertical_scrollbar(&scroll)
    })
    .build(window, cx)
}

#[derive(Clone, Debug, PartialEq)]
struct TooltipRow {
    text: SharedString,
    /// Paragraph base direction: the first strong character decides.
    rtl: bool,
}

/// Wrap `value` into display rows no wider than `width` at UAX #14 break
/// opportunities; an unbreakable run falls back to grapheme boundaries.
/// Rows keep source order and every character; only the line breaks between
/// rows and the whitespace that ends a wrapped row are not repeated.
fn tooltip_rows(value: &str, width: f32, mut measure: impl FnMut(&str) -> f32) -> Vec<TooltipRow> {
    let mut rows = Vec::new();
    for paragraph in value.split('\n') {
        let paragraph = paragraph.strip_suffix('\r').unwrap_or(paragraph);
        let rtl = paragraph
            .chars()
            .find_map(|ch| match unicode_bidi::bidi_class(ch) {
                unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL => Some(true),
                unicode_bidi::BidiClass::L => Some(false),
                _ => None,
            })
            .unwrap_or(false);
        let mut push = |text: &str| {
            rows.push(TooltipRow {
                text: text.to_owned().into(),
                rtl,
            })
        };
        if paragraph.is_empty() {
            push("");
            continue;
        }
        let (mut start, mut end, mut row_width) = (0, 0, 0.);
        let mut previous = 0;
        for (offset, _) in unicode_linebreak::linebreaks(paragraph) {
            let segment = &paragraph[previous..offset];
            // Trailing whitespace may hang past the edge, as in a paragraph.
            let segment_width = measure(segment.trim_end());
            if end > start && row_width + segment_width > width {
                push(paragraph[start..end].trim_end());
                (start, row_width) = (end, 0.);
            }
            if segment_width > width {
                for (ix, grapheme) in segment.grapheme_indices(true) {
                    let grapheme_width = measure(grapheme);
                    let at = previous + ix;
                    if at > start && row_width + grapheme_width > width {
                        push(&paragraph[start..at]);
                        (start, row_width) = (at, 0.);
                    }
                    row_width += grapheme_width;
                }
            } else {
                row_width += measure(segment);
            }
            end = offset;
            previous = offset;
        }
        push(paragraph[start..].trim_end());
    }
    rows
}

fn visible(value: &str) -> String {
    let mut text = value.graphemes(true).take(160).collect::<String>();
    if text.len() < value.len() {
        text.push('…');
    }
    text.replace("\r\n", " ↵ ").replace(['\n', '\r'], " ↵ ")
}
fn alignment(value: &str) -> TextAlign {
    if value.trim().parse::<f64>().is_ok() {
        return TextAlign::Right;
    }
    for ch in value.chars() {
        match unicode_bidi::bidi_class(ch) {
            unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL => return TextAlign::Right,
            unicode_bidi::BidiClass::L => return TextAlign::Left,
            _ => {}
        }
    }
    TextAlign::Left
}
/// Closing the file or switching away releases the entity: a worker scan that
/// is still running sees the flag at its next row and drops its snapshot.
impl Drop for TablePreview {
    fn drop(&mut self) {
        self.projection_cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Render for TablePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.json() || reader_ui_state::delimited_header(&self.path, cx);
        if self.header != header || self.projection_all != self.all {
            self.header = header;
            self.refilter(cx);
        }
        let case = reader_ui_state::find_case_sensitive(cx);
        if self.case_sensitive != case {
            self.case_sensitive = case;
            self.refilter(cx);
        }
        let palette = brand::palette(cx);
        let pane = self.pane_bounds.clone();
        let mut view = v_flex()
            .id("reader-delimited")
            .relative()
            .child(
                canvas(move |bounds, _, _| pane.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .font_family(reader_fonts::note(cx))
            .key_context("ReaderDelimited")
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .w_full()
            .text_size(px(13.))
            .text_color(palette.text)
            .on_action(
                cx.listener(|this, _: &CopyCells, window, cx| this.copy_feedback(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleHeaderRow, _, cx| this.toggle_header(cx)))
            .on_action(cx.listener(|this, _: &ResetTableSort, _, cx| this.reset_sort(cx)))
            .on_action(
                cx.listener(|this, _: &OpenTableDuckDb, window, cx| this.open_duckdb(window, cx)),
            )
            .on_action(cx.listener(|this, _: &LoadMoreTableRows, _, cx| this.load(true, cx)))
            .on_action(
                cx.listener(|this, _: &GoToTableError, window, cx| this.go_to_bad_row(window, cx)),
            );
        if self.own_header {
            view = view.child(self.compact_header(window, cx));
        }
        let Some(table) = self.table.clone() else {
            return view
                .child(div().flex_1().min_h_0())
                .child(self.status_element(cx))
                .into_any_element();
        };
        if !table.limited && (table.rows.is_empty() || (self.json() && table.rows.len() == 1)) {
            return view
                .child(div().px_4().child(if self.json() {
                    "No data rows"
                } else {
                    "Empty file"
                }))
                .child(self.status_element(cx))
                .into_any_element();
        }
        let start = usize::from(header);
        let count = self.order.len();
        let first_column = ((-f32::from(self.horizontal.offset().x) / COLUMN_WIDTH).floor()
            as usize)
            .saturating_sub(1)
            .min(table.columns);
        // Virtualize against the measured pane; the window is only a fallback
        // before the first paint.
        let pane_width = match f32::from(self.pane_bounds.get().size.width) {
            width if width > 0. => width,
            _ => f32::from(window.viewport_size().width),
        };
        let column_count = (pane_width / COLUMN_WIDTH).ceil() as usize + 3;
        self.visible_columns = first_column..(first_column + column_count).min(table.columns);
        let table = self.table.as_ref().unwrap();
        let width = px(GUTTER + COLUMN_WIDTH * table.columns as f32);
        let mut grid = v_flex().flex_none().h_full().min_h_0().w(width);
        if !table.rows.is_empty() {
            grid = grid.child(self.row(if header { 0 } else { usize::MAX }, true, cx));
        }
        grid = grid.child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    uniform_list(
                        "delimited-rows",
                        count,
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            range
                                .map(|row| this.row(row + start, false, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full(),
                )
                .vertical_scrollbar(&self.scroll),
        );
        view = view.child(
            div()
                .id("delimited-viewport")
                .debug_selector(|| "delimited-viewport".into())
                .relative()
                .flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    div()
                        .id("delimited-horizontal")
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_hidden()
                        .track_scroll(&self.horizontal)
                        .child(grid),
                )
                // A sibling stays on the viewport when the wider grid moves.
                .child(table_wheel(self.horizontal.clone(), self.scroll.clone()))
                .horizontal_scrollbar(&self.horizontal),
        );
        view = view.child(self.status_element(cx));
        view.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::ThemeMode;

    #[gpui::test]
    fn data_table_2735_native_sort_filter_copy_and_wide_virtualization(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("table.csv");
        let source = "name,n\nTen,10\nTwo,2\nHundred,100\n";
        std::fs::write(&path, source).unwrap();
        cx.update(crate::test_init);
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| TablePreview::new(path.clone(), window, cx));
            preview = Some(view.clone());
            Root::new(cx.new(|_| TableSurface(view)), window, cx)
        });
        let preview = preview.unwrap();
        visual.simulate_resize(size(px(800.), px(500.)));
        visual.run_until_parked();
        let header = visual.debug_bounds("delimited-cell-0-1").unwrap();
        visual.simulate_click(header.center(), Modifiers::default());
        visual.run_until_parked();
        preview.read_with(visual, |p, _| {
            assert_eq!(p.order, [2, 1, 3], "native header click sorted numerically")
        });
        let first = visual.debug_bounds("delimited-cell-1-0").unwrap();
        visual.simulate_click(first.center(), Modifiers::default());
        preview.update(visual, |p, cx| p.copy(cx));
        visual.update(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "Two"));
        visual.simulate_click(header.center(), Modifiers::default());
        visual.run_until_parked();
        preview.read_with(visual, |p, _| assert_eq!(p.order, [3, 1, 2]));
        visual.simulate_click(header.center(), Modifiers::default());
        visual.run_until_parked();
        preview.read_with(visual, |p, _| {
            assert_eq!(p.order, [1, 2, 3], "third click restores source order")
        });
        preview.update_in(visual, |p, window, cx| {
            let input = p.filter.as_ref().unwrap().clone();
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        visual.simulate_input("HUND");
        // Find debounces typing (60 ms) before the worker projection.
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(70));
        visual.run_until_parked();
        preview.read_with(visual, |p, _| {
            assert_eq!(p.order, [3], "native filter subscription fired");
            assert_eq!(
                p.anchor,
                Some(Cell { row: 1, column: 0 }),
                "Find selects the first actual matching cell"
            );
        });
        // Successive requests must discard any old worker result.
        preview.update(visual, |p, cx| {
            p.query = "missing".into();
            p.refilter(cx);
            p.query = "Two".into();
            p.refilter(cx);
        });
        visual.run_until_parked();
        preview.read_with(visual, |p, _| assert_eq!(p.order, [2]));
        let wide = (0..200)
            .map(|i| format!("column{i}"))
            .collect::<Vec<_>>()
            .join(",")
            + "\n";
        std::fs::write(&path, wide.repeat(30)).unwrap();
        preview.update(visual, |p, cx| {
            p.query.clear();
            p.load(false, cx);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("delimited-cell-1-0").is_some());
        assert!(
            visual.debug_bounds("delimited-cell-1-199").is_none(),
            "offscreen columns must not mount"
        );
        preview.update(visual, |p, cx| {
            p.horizontal.set_offset(point(px(-39_400.), px(0.)));
            cx.notify();
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("delimited-cell-1-199").is_some(),
            "scroll reaches the last column"
        );
        assert!(visual.debug_bounds("delimited-cell-1-0").is_none());
    }

    // Match FilePreview's bounded flex column. Root itself is a block surface,
    // so placing a flex-sized child directly in Root gives the list no height.
    struct TableSurface(Entity<TablePreview>);
    impl Render for TableSurface {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex().size_full().min_h_0().child(self.0.clone())
        }
    }

    #[gpui::test]
    fn wide_table_routes_wheel_axes_without_waiting_for_the_horizontal_edge(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("wheel.csv");
        let source: String = (0..200)
            .map(|row| {
                (0..12)
                    .map(|column| format!("r{row}c{column}"))
                    .collect::<Vec<_>>()
                    .join(",")
                    + "\n"
            })
            .collect();
        std::fs::write(&path, &source).unwrap();
        cx.update(crate::test_init);
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| TablePreview::new(path.clone(), window, cx));
            preview = Some(entity.clone());
            Root::new(cx.new(|_| TableSurface(entity)), window, cx)
        });
        let preview = preview.unwrap();
        visual.simulate_resize(size(px(720.), px(420.)));
        let draw = |visual: &mut gpui::VisualTestContext| {
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        draw(visual);
        let viewport = visual.debug_bounds("delimited-viewport").unwrap();
        let header = visual.debug_bounds("delimited-row-0").unwrap();
        let first = visual.debug_bounds("delimited-row-1").unwrap();
        let max_x = preview.read_with(visual, |p, _| p.horizontal.max_offset().x);
        assert!(
            max_x > viewport.size.width,
            "positive control: table overflows horizontally"
        );
        assert!(
            preview.read_with(visual, |p, _| p.scroll.is_scrollable()),
            "positive control: rows overflow vertically"
        );
        let position = |y| point(viewport.center().x, y);
        let reset = |visual: &mut gpui::VisualTestContext, fraction: f32| {
            preview.update(visual, |p, cx| {
                p.horizontal.set_offset(point(-max_x * fraction, px(0.)));
                p.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
                cx.notify();
            });
            draw(visual);
        };
        for fraction in [0., 0.5, 1.] {
            for delta in [
                ScrollDelta::Lines(point(0., -3.)),
                ScrollDelta::Pixels(point(px(0.), px(-80.))),
            ] {
                // The column header must route to the rows too, not just the
                // portion where uniform_list's own hitbox is present.
                for y in [header.center().y, first.center().y] {
                    reset(visual, fraction);
                    let before = preview.read_with(visual, |p, _| p.horizontal.offset());
                    visual.simulate_event(ScrollWheelEvent {
                        position: position(y),
                        delta,
                        ..Default::default()
                    });
                    draw(visual);
                    preview.read_with(visual, |p, _| {
                        assert_eq!(
                            p.horizontal.offset(),
                            before,
                            "vertical wheel must not move columns"
                        );
                        assert!(
                            p.scroll.0.borrow().base_handle.offset().y < px(0.),
                            "vertical wheel must move rows at every horizontal offset"
                        );
                    });
                }
            }
        }
        for (delta, shift) in [
            (ScrollDelta::Pixels(point(px(-80.), px(0.))), false),
            (ScrollDelta::Lines(point(-3., 0.)), false),
            (ScrollDelta::Pixels(point(px(0.), px(-80.))), true),
            (ScrollDelta::Lines(point(0., -3.)), true),
            // Windows normalizes Shift+wheel into X before GPUI dispatch.
            (ScrollDelta::Lines(point(-3., 0.)), true),
        ] {
            reset(visual, 0.);
            visual.simulate_event(ScrollWheelEvent {
                position: position(first.center().y),
                delta,
                modifiers: Modifiers {
                    shift,
                    ..Default::default()
                },
                ..Default::default()
            });
            draw(visual);
            preview.read_with(visual, |p, _| {
                assert!(
                    p.horizontal.offset().x < px(0.),
                    "positive control: horizontal input moves columns"
                );
                assert_eq!(
                    p.scroll.0.borrow().base_handle.offset().y,
                    px(0.),
                    "horizontal input must not become row scrolling"
                );
            });
            reset(visual, 1.);
            visual.simulate_event(ScrollWheelEvent {
                position: position(first.center().y),
                delta,
                modifiers: Modifiers {
                    shift,
                    ..Default::default()
                },
                ..Default::default()
            });
            draw(visual);
            preview.read_with(visual, |p, _| {
                assert_eq!(p.horizontal.offset().x, -max_x);
                assert_eq!(
                    p.scroll.0.borrow().base_handle.offset().y,
                    px(0.),
                    "horizontal edge must not remap to rows"
                );
            });
        }
        reset(visual, 0.);
        visual.simulate_event(ScrollWheelEvent {
            position: position(first.center().y),
            delta: ScrollDelta::Pixels(point(px(-80.), px(-40.))),
            ..Default::default()
        });
        draw(visual);
        preview.read_with(visual, |p, _| {
            assert!(p.horizontal.offset().x < px(0.));
            assert!(
                p.scroll.0.borrow().base_handle.offset().y < px(0.),
                "a diagonal delta preserves the vertical component"
            );
        });
        assert_eq!(std::fs::read(path).unwrap(), source.as_bytes());
    }

    #[test]
    fn delimited_alignment_and_display_keep_full_values() {
        assert_eq!(alignment("-123.45"), TextAlign::Right);
        assert_eq!(alignment("123 שלום abc"), TextAlign::Right);
        assert_eq!(alignment("123 abc שלום"), TextAlign::Left);
        assert_eq!(visible("a\r\nb"), "a ↵ b");
        let value = "e\u{301}".repeat(200);
        assert_eq!(visible(&value), format!("{}…", "e\u{301}".repeat(160)));
        assert_eq!(value.graphemes(true).count(), 200);
        assert!(eligible("sheet.TSV"));
        assert!(!eligible("note.md"));
    }

    #[test]
    fn delimited_tooltip_rows_wrap_at_word_breaks_and_keep_every_character() {
        // Fake metrics: every character is 10 px wide.
        let measure = |text: &str| text.chars().count() as f32 * 10.;
        let value = "שלום עולם ".repeat(20) + "\r\n\nabc שלום 123\n" + &"q".repeat(95);
        let rows = tooltip_rows(&value, 200., measure);
        for row in &rows {
            assert!(measure(&row.text) <= 200., "{row:?}");
        }
        let hebrew: Vec<_> = rows.iter().take_while(|row| row.rtl).collect();
        assert!(hebrew.len() > 1);
        for row in &hebrew {
            // Break only between words: no Hebrew word is split across rows.
            assert!(
                row.text
                    .split(' ')
                    .all(|word| word == "שלום" || word == "עולם"),
                "{row:?}"
            );
        }
        let blank = rows.iter().position(|row| row.text.is_empty()).unwrap();
        assert_eq!(
            rows[blank + 1],
            TooltipRow {
                text: "abc שלום 123".into(),
                rtl: false
            }
        );
        // An unbroken token falls back to grapheme boundaries.
        let tail: String = rows[blank + 2..]
            .iter()
            .map(|row| row.text.as_ref())
            .collect();
        assert_eq!(tail, "q".repeat(95));
        assert_eq!(rows[blank + 2].text.len(), 20);
        let words: Vec<&str> = rows[..blank + 2]
            .iter()
            .flat_map(|row| row.text.split_whitespace())
            .collect();
        let expected: Vec<&str> = value
            .split_whitespace()
            .filter(|word| !word.starts_with('q'))
            .collect();
        assert_eq!(words, expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn delimited_tooltip_rows_render_hebrew_first_text_with_real_shaping() {
        let text_system = WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(
            gpui_wgpu::CosmicTextSystem::new("DejaVu Sans"),
        ))));
        let font = font("DejaVu Sans");
        let run = |len| TextRun {
            len,
            font: font.clone(),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let size = px(14.);
        let paragraph = "שלום עולם, this is mixed text 123 ".repeat(30);
        let shaped = text_system.layout_line(&paragraph, size, &[run(paragraph.len())], None);
        let glyphs: Vec<_> = shaped.runs.iter().flat_map(|run| &run.glyphs).collect();
        assert!(
            glyphs.iter().all(|glyph| glyph.id.0 != 0),
            "positive control: the shaping font covers Hebrew"
        );
        assert!(
            glyphs[0].position.x > glyphs[1].position.x,
            "positive control: real shaping places the first Hebrew glyph right to left"
        );
        // Positive control: GPUI's own wrapping of this paragraph is unusable,
        // which is what the native tooltip showed (a blank row and one letter).
        let wrapped = text_system
            .shape_text(
                paragraph.clone().into(),
                size,
                &[run(paragraph.len())],
                Some(px(300.)),
                None,
            )
            .unwrap();
        let boundaries = wrapped[0].wrap_boundaries();
        assert!(
            boundaries.len() < 3 || boundaries[0].glyph_ix <= 1,
            "GPUI wrapping became usable; this workaround may be unnecessary: {boundaries:?}"
        );
        let measure = |text: &str| {
            f32::from(
                text_system
                    .layout_line(text, size, &[run(text.len())], None)
                    .width,
            )
        };
        // Pure Hebrew, Hebrew-first mixed, Latin-first mixed (the passing
        // native control) and a long multiline Hebrew + English value.
        let cases = [
            ("שלום עולם ".repeat(40), vec![true]),
            (paragraph.clone(), vec![true]),
            ("abc שלום 123 עולם ".repeat(30), vec![false]),
            (
                format!(
                    "{}\n{}\r\n\n{}",
                    "שורה ראשונה בעברית ".repeat(80),
                    "English line with שלום inside ".repeat(40),
                    "סוף ".repeat(400)
                ),
                vec![true, false, false, true],
            ),
        ];
        for (value, directions) in cases {
            let rows = tooltip_rows(&value, 300., measure);
            assert!(rows.len() > 3, "{} rows", rows.len());
            for row in &rows {
                let line = text_system.layout_line(&row.text, size, &[run(row.text.len())], None);
                assert!(
                    f32::from(line.width) <= 300.5,
                    "{row:?} is {:?}",
                    line.width
                );
                let drawn: usize = line.runs.iter().map(|run| run.glyphs.len()).sum();
                let visible = row.text.chars().filter(|ch| !ch.is_whitespace()).count();
                assert!(drawn >= visible, "every character of {row:?} is shaped");
            }
            // Paragraph directions, in order; the blank line counts as LTR.
            let mut seen: Vec<bool> = Vec::new();
            let mut previous_blank = true;
            for row in &rows {
                if row.text.is_empty() {
                    seen.push(false);
                    previous_blank = true;
                } else if previous_blank || seen.last() != Some(&row.rtl) {
                    seen.push(row.rtl);
                    previous_blank = false;
                }
            }
            assert_eq!(seen, directions);
            let words: Vec<&str> = rows
                .iter()
                .flat_map(|row| row.text.split_whitespace())
                .collect();
            assert_eq!(words, value.split_whitespace().collect::<Vec<_>>());
        }
    }

    #[gpui::test]
    fn delimited_full_value_tooltip_wraps_and_scrolls_inside_viewport(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("long.csv");
        // Exercise word wrapping, an unbroken field and embedded newlines.
        let value = format!(
            "{}\n{}\nEND-OF-FULL-VALUE",
            "שלום world ".repeat(200),
            "q".repeat(1200)
        );
        let source = format!("value\n\"{value}\"\n");
        std::fs::write(&path, &source).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let preview = cx.new(|cx| TablePreview::new(path.clone(), window, cx));
            let surface = cx.new(|_| TableSurface(preview));
            Root::new(surface, window, cx)
        });
        visual.run_until_parked();
        for viewport in [size(px(1366.), px(768.)), size(px(360.), px(260.))] {
            visual.simulate_resize(viewport);
            // In a narrow window the popup clamps to (0, 0), so that
            // coordinate is still inside its hoverable area. Leave it fully.
            let outside = point(viewport.width - px(1.), viewport.height - px(1.));
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                visual.update(|_, cx| Theme::change(mode, None, cx));
                visual.run_until_parked();
                let cell = visual.debug_bounds("delimited-cell-1-0").unwrap();
                visual.simulate_click(cell.center(), Modifiers::default());
                visual.simulate_keystrokes("secondary-c");
                visual.read(|cx| {
                    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), value);
                });
                visual.simulate_mouse_move(outside, None, Modifiers::default());
                visual.executor().advance_clock(Duration::from_secs(1));
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
                assert!(
                    visual.debug_bounds("delimited-cell-tooltip").is_none(),
                    "positive control: leave the old popup before opening a fresh value"
                );
                visual.simulate_mouse_move(cell.center(), None, Modifiers::default());
                visual.executor().advance_clock(Duration::from_secs(1));
                visual.run_until_parked();
                let tooltip = visual.debug_bounds("delimited-cell-tooltip").unwrap();
                assert!(tooltip.left() >= px(0.) && tooltip.right() <= viewport.width);
                assert!(tooltip.top() >= px(0.) && tooltip.bottom() <= viewport.height);
                let content = visual.debug_bounds("delimited-cell-tooltip-value").unwrap();
                assert!(content.size.width <= tooltip.size.width);
                // Rows are wrapped by Okilum; GPUI must not re-wrap any of them.
                let first = visual.debug_bounds("delimited-cell-tooltip-row-0").unwrap();
                let second = visual.debug_bounds("delimited-cell-tooltip-row-1").unwrap();
                assert!(first.size.height > px(0.) && second.top() >= first.bottom());
                assert!(first.size.width <= content.size.width);
                assert!(
                    content.top() >= tooltip.top(),
                    "positive control: a fresh tooltip starts at the beginning of the value"
                );
                assert!(
                    content.size.height > tooltip.size.height,
                    "positive control: full value needs scrolling"
                );
                // Moving into the tooltip must keep it open long enough to read/scroll.
                visual.simulate_mouse_move(tooltip.center(), None, Modifiers::default());
                visual.executor().advance_clock(Duration::from_secs(1));
                visual.run_until_parked();
                assert!(visual.debug_bounds("delimited-cell-tooltip").is_some());
                visual.simulate_event(ScrollWheelEvent {
                    position: tooltip.center(),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
                    ..Default::default()
                });
                visual.run_until_parked();
                // Wheel dispatch invalidates the view; the test platform does
                // not guarantee a frame before debug_bounds is read. Observe
                // the actual next frame, as the scrollbar interaction tests do.
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let end = visual.debug_bounds("delimited-cell-tooltip-value").unwrap();
                assert!(
                    end.top() < content.top(),
                    "positive control: wheel moved the full value: {content:?} -> {end:?}"
                );
                assert!(
                    end.bottom() <= tooltip.bottom() + px(1.),
                    "the end of the full value is reachable"
                );
                visual.simulate_mouse_move(outside, None, Modifiers::default());
                visual.executor().advance_clock(Duration::from_secs(1));
                visual.run_until_parked();
            }
        }
        assert_eq!(std::fs::read(path).unwrap(), source.as_bytes());
    }

    /// #946: over an open tooltip, the cells it covers neither replace it nor
    /// scroll; another cell's tooltip still opens once the pointer rests on
    /// that cell.
    #[gpui::test]
    fn an_open_cell_tooltip_keeps_the_cells_it_covers_quiet(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("dense.csv");
        let long = "Long value ".repeat(120);
        let mut source = String::from("a,b,c,d,e,f,g,h\n");
        for row in 0..30 {
            let cells: Vec<String> = (0..8)
                .map(|col| {
                    if row == 0 && col == 0 {
                        format!("\"{long}\"")
                    } else {
                        format!("r{row}c{col}")
                    }
                })
                .collect();
            source.push_str(&cells.join(","));
            source.push('\n');
        }
        std::fs::write(&path, &source).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let preview = cx.new(|cx| TablePreview::new(path.clone(), window, cx));
            let surface = cx.new(|_| TableSurface(preview));
            Root::new(surface, window, cx)
        });
        visual.simulate_resize(size(px(1200.), px(800.)));
        visual.run_until_parked();
        let settle = |visual: &mut gpui::VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(1500));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let long_rows = |visual: &mut gpui::VisualTestContext| {
            visual
                .debug_bounds("delimited-cell-tooltip-row-3")
                .is_some()
        };
        let cell = visual.debug_bounds("delimited-cell-1-0").unwrap();
        visual.simulate_mouse_move(cell.center(), None, Modifiers::default());
        settle(visual);
        let tooltip = visual.debug_bounds("delimited-cell-tooltip").unwrap();
        assert!(
            long_rows(visual),
            "positive control: the long value's tooltip is open"
        );
        // A point inside the tooltip that lies over another cell.
        let over = (1..20)
            .flat_map(|r| (0..6).map(move |c| (r, c)))
            .filter(|&(r, c)| (r, c) != (1, 0))
            .find_map(|(r, c)| {
                let bounds = visual.debug_bounds(Box::leak(
                    format!("delimited-cell-{r}-{c}").into_boxed_str(),
                ))?;
                let inside = Bounds::new(
                    tooltip.origin + point(px(8.), px(8.)),
                    tooltip.size - size(px(16.), px(16.)),
                );
                [bounds.center()].into_iter().find(|p| inside.contains(p))
            })
            .expect("positive control: the tooltip covers another cell");
        let before = visual.debug_bounds("delimited-cell-1-1").unwrap();
        visual.simulate_mouse_move(over, None, Modifiers::default());
        settle(visual);
        assert!(
            long_rows(visual),
            "the covered cell does not replace the open tooltip"
        );
        visual.simulate_event(ScrollWheelEvent {
            position: over,
            delta: ScrollDelta::Pixels(point(px(-300.), px(0.))),
            ..Default::default()
        });
        settle(visual);
        assert_eq!(
            visual.debug_bounds("delimited-cell-1-1").unwrap(),
            before,
            "the wheel over the tooltip does not scroll the table"
        );
        // Positive control: resting on another cell outside the tooltip
        // shows that cell's own (short) value.
        let far = visual.debug_bounds("delimited-cell-20-5").unwrap();
        assert!(!tooltip.contains(&far.center()));
        visual.simulate_mouse_move(far.center(), None, Modifiers::default());
        settle(visual);
        settle(visual);
        assert!(visual.debug_bounds("delimited-cell-tooltip").is_some());
        assert!(
            !long_rows(visual),
            "another cell's tooltip opens after resting there"
        );
    }

    #[gpui::test]
    fn delimited_virtual_rows_pin_header_and_scroll_both_axes(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("large.csv");
        let source = format!(
            "name,value,third,fourth,fifth\n{}",
            (0..10_000)
                .map(|i| format!("שלום{i},{i},a,b,c\n"))
                .collect::<String>()
        );
        std::fs::write(&path, &source).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_ui_state::install(&fixture.path().join("state"), cx);
        });
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| TablePreview::new(path.clone(), window, cx));
            preview = Some(entity.clone());
            let surface = cx.new(|_| TableSurface(entity));
            Root::new(surface, window, cx)
        });
        let preview = preview.unwrap();
        visual.simulate_resize(size(px(650.), px(500.)));
        visual.run_until_parked();
        preview.read_with(visual, |p, _| {
            let table = p.table.as_ref().unwrap();
            assert!(
                table.limited,
                "the initial worker does not retain the whole file"
            );
            assert_eq!(table.rows.len(), INITIAL_ROWS + 2);
        });
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            visual.update(|_, cx| Theme::change(mode, None, cx));
            visual.run_until_parked();
            let header = visual.debug_bounds("delimited-row-0").unwrap();
            let first = visual.debug_bounds("delimited-row-1").unwrap();
            assert_eq!(header.size.height, px(ROW_HEIGHT));
            assert_eq!(first.top(), header.bottom());
            assert!(visual.debug_bounds("delimited-row-9000").is_none());
            assert!(visual.debug_bounds("table-load-more").is_some());
            preview.update(visual, |p, cx| {
                p.scroll.scroll_to_item_strict(999, ScrollStrategy::Top);
                cx.notify();
            });
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("delimited-row-1000").is_some(),
                "positive control: scroll reached the initial cap"
            );
            assert!(visual.debug_bounds("delimited-row-1001").is_none());
            assert_eq!(visual.debug_bounds("delimited-row-0").unwrap(), header);
            let show = visual.debug_bounds("table-load-more").unwrap();
            visual.simulate_click(show.center(), Modifiers::default());
            visual.run_until_parked();
            preview.update(visual, |p, cx| {
                p.scroll.scroll_to_item_strict(9999, ScrollStrategy::Bottom);
                cx.notify();
            });
            visual.run_until_parked();
            let last = visual.debug_bounds("delimited-row-10000").unwrap();
            assert!(last.bottom() <= px(500.) && last.top() >= header.bottom());
            assert_eq!(visual.debug_bounds("delimited-row-0").unwrap(), header);
            assert!(
                visual.debug_bounds("delimited-row-1").is_none(),
                "rows are virtual, not 10k mounted elements"
            );
            preview.update(visual, |p, cx| {
                p.horizontal.set_offset(point(px(-300.), px(0.)));
                cx.notify();
            });
            visual.run_until_parked();
            let moved = visual.debug_bounds("delimited-row-0").unwrap();
            assert!(
                moved.left() < header.left(),
                "positive control: horizontal scrolling moves columns"
            );
            assert_eq!(moved.top(), header.top());
            preview.update(visual, |p, cx| {
                p.all = false;
                p.horizontal.set_offset(point(px(0.), px(0.)));
                p.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
                cx.notify();
            });
            visual.run_until_parked();
        }
        assert_eq!(std::fs::read(&path).unwrap(), source.as_bytes());
    }

    #[gpui::test]
    fn delimited_preview_limits_keep_header_and_offer_explicit_fallback(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("wide.csv");
        let source = format!("a,b\n{}\n", ",".repeat(10_000));
        std::fs::write(&path, &source).unwrap();
        cx.update(crate::test_init);
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| TablePreview::new(path.clone(), window, cx));
            preview = Some(entity.clone());
            let surface = cx.new(|_| TableSurface(entity));
            Root::new(surface, window, cx)
        });
        let preview = preview.unwrap();
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("delimited-row-0").is_some(),
            "valid header positive control"
        );
        preview.read_with(visual, |p, _| {
            let table = p.table.as_ref().unwrap();
            assert!(table.limited);
            assert_eq!(table.columns, 2);
            assert_eq!(table.rows.len(), 1);
        });
        let show = visual.debug_bounds("table-load-more").unwrap();
        visual.simulate_click(show.center(), Modifiers::default());
        visual.run_until_parked();
        preview.read_with(visual, |p, _| {
            assert!(p.all && !p.loading);
            assert!(p.table.as_ref().unwrap().limited);
        });
        assert!(visual.debug_bounds("delimited-row-0").is_some());
        assert!(visual.debug_bounds("table-status-line").is_some());
        assert!(visual.debug_bounds("table-load-more").is_none());
        assert_eq!(std::fs::read(path).unwrap(), source.as_bytes());
    }

    #[gpui::test]
    fn delimited_mouse_range_and_keyboard_copy_preserve_multiline_values(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("quotes.csv");
        let source = "name,value\r\n\"שלום, world\",\"line\r\nnext\"\r\nother,42\r\n";
        std::fs::write(&path, source).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| TablePreview::new(path.clone(), window, cx));
            let surface = cx.new(|_| TableSurface(entity));
            Root::new(surface, window, cx)
        });
        visual.run_until_parked();
        let first = visual.debug_bounds("delimited-cell-1-0").unwrap();
        let last = visual.debug_bounds("delimited-cell-2-1").unwrap();
        visual.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(last.center(), Some(MouseButton::Left), Modifiers::default());
        visual.simulate_mouse_up(last.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_keystrokes("secondary-c");
        visual.run_until_parked();
        visual.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "שלום, world\t\"line\r\nnext\"\nother\t42"
            )
        });
        visual.simulate_click(first.center(), Modifiers::default());
        visual.simulate_click(
            last.center(),
            Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        visual.simulate_keystrokes("secondary-c");
        visual.run_until_parked();
        visual.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "שלום, world\t\"line\r\nnext\"\nother\t42"
            )
        });
        let multiline = visual.debug_bounds("delimited-cell-1-1").unwrap();
        visual.simulate_click(multiline.center(), Modifiers::default());
        visual.simulate_keystrokes("secondary-c");
        visual.run_until_parked();
        visual.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "line\r\nnext"
            )
        });
        assert_eq!(std::fs::read(path).unwrap(), source.as_bytes());
    }
}
