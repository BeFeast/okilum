//! Read-only delimited tables; editing and recovery stay in FileEditor.
use super::*;
use gpui_component::scroll::ScrollableElement as _;
use std::ops::Range;
use tessera_core::delimited::Table;
use unicode_segmentation::UnicodeSegmentation as _;

const ROW_HEIGHT: f32 = 30.;
const COLUMN_WIDTH: f32 = 200.;
const GUTTER: f32 = 48.;
const INITIAL_ROWS: usize = 1000;
actions!(reader_delimited, [CopyCells]);

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
pub(crate) fn editable(rel: &str) -> bool {
    eligible(rel)
        || Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cell {
    row: usize,
    column: usize,
}

pub(crate) struct TablePreview {
    path: PathBuf,
    table: Option<Arc<Table>>,
    failed: bool,
    all: bool,
    anchor: Option<Cell>,
    end: Option<Cell>,
    scroll: UniformListScrollHandle,
    horizontal: ScrollHandle,
    focus: FocusHandle,
}
impl TablePreview {
    pub(crate) fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let load_path = path.clone();
        let task = cx.background_executor().spawn(async move {
            std::fs::read_to_string(&load_path).map(|source| {
                Table::parse(
                    &source,
                    load_path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("tsv")),
                )
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(table) => this.table = Some(Arc::new(table)),
                    Err(error) => {
                        eprintln!("Cannot read delimited file: {error:#}");
                        this.failed = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            path,
            table: None,
            failed: false,
            all: false,
            anchor: None,
            end: None,
            scroll: UniformListScrollHandle::new(),
            horizontal: ScrollHandle::new(),
            focus: cx.focus_handle(),
        }
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
        if !extend || self.anchor.is_none() {
            self.anchor = Some(cell);
        }
        self.end = Some(cell);
        self.focus.focus(window, cx);
        cx.notify();
    }
    fn copy(&self, cx: &mut Context<Self>) {
        if let (Some(table), Some((rows, columns))) = (&self.table, self.selection()) {
            cx.write_to_clipboard(ClipboardItem::new_string(table.copy(rows, columns)));
        }
    }
    fn row(&self, row: usize, header: bool, cx: &mut Context<Self>) -> AnyElement {
        let table = self.table.as_ref().unwrap();
        let palette = brand::palette(cx);
        let header_present = reader_ui_state::delimited_header(&self.path, cx);
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
            .children((0..table.columns).map(|column| {
                let cell = Cell { row, column };
                let value = table.cell(row, column).to_owned();
                let text = visible(&value);
                div()
                    .id(("delimited-cell", row * table.columns + column))
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
                    .child(div().truncate().child(text))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(value.clone()).build(window, cx)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.select(cell, event.modifiers.shift, window, cx);
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                            if event.pressed_button == Some(MouseButton::Left)
                                && this.anchor.is_some()
                                && this.end != Some(cell)
                            {
                                this.select(cell, true, window, cx);
                            }
                        }),
                    )
            }))
            .into_any_element()
    }
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
impl Render for TablePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = brand::palette(cx);
        let mut view = v_flex()
            .id("reader-delimited")
            .key_context("ReaderDelimited")
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .w_full()
            .text_size(px(13.))
            .text_color(palette.text)
            .on_action(cx.listener(|this, _: &CopyCells, _, cx| this.copy(cx)));
        let Some(table) = self.table.as_ref() else {
            return view
                .child(div().px_4().child(if self.failed {
                    "This table couldn’t be read as UTF-8."
                } else {
                    "Opening table…"
                }))
                .into_any_element();
        };
        if table.rows.is_empty() {
            return view
                .child(div().px_4().child("Empty file"))
                .into_any_element();
        }
        let header = reader_ui_state::delimited_header(&self.path, cx);
        let start = usize::from(header);
        let available = table.rows.len().saturating_sub(start);
        let count = if self.all {
            available
        } else {
            available.min(INITIAL_ROWS)
        };
        let width = px(GUTTER + COLUMN_WIDTH * table.columns as f32);
        let mut grid = v_flex().flex_none().h_full().min_h_0().w(width);
        if header {
            grid = grid.child(self.row(0, true, cx));
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
        let mut notices = Vec::new();
        if table.ragged {
            notices.push("Some rows have a different number of columns.");
        }
        if table.malformed {
            notices.push("Some rows have incomplete or unexpected quotes.");
        }
        if table.ambiguous {
            notices.push("More than one separator fits this file.");
        }
        view = view.child(
            div()
                .id("delimited-horizontal")
                .flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_x_scroll()
                .track_scroll(&self.horizontal)
                .child(grid)
                .horizontal_scrollbar(&self.horizontal),
        );
        if !notices.is_empty() {
            view = view.child(
                div()
                    .px_4()
                    .py_1()
                    .text_color(palette.text_muted)
                    .child(notices.join(" ")),
            );
        }
        if count < available {
            view = view.child(
                h_flex()
                    .px_4()
                    .gap_3()
                    .text_color(palette.text_muted)
                    .child(format!("Showing the first {count} rows of {available}"))
                    .child(
                        Button::new("delimited-show-all")
                            .debug_selector(|| "delimited-show-all".into())
                            .ghost()
                            .small()
                            .label("Show all")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.all = true;
                                cx.notify();
                            })),
                    ),
            );
        }
        view.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::ThemeMode;

    // Match FilePreview's bounded flex column. Root itself is a block surface,
    // so placing a flex-sized child directly in Root gives the list no height.
    struct TableSurface(Entity<TablePreview>);
    impl Render for TableSurface {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex().size_full().min_h_0().child(self.0.clone())
        }
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
            gpui_component::init(cx);
            reader_ui_state::install(&fixture.path().join("state"), cx);
        });
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| TablePreview::new(path.clone(), cx));
            preview = Some(entity.clone());
            let surface = cx.new(|_| TableSurface(entity));
            Root::new(surface, window, cx)
        });
        let preview = preview.unwrap();
        visual.simulate_resize(size(px(650.), px(500.)));
        visual.run_until_parked();
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            visual.update(|_, cx| Theme::change(mode, None, cx));
            visual.run_until_parked();
            let header = visual.debug_bounds("delimited-row-0").unwrap();
            let first = visual.debug_bounds("delimited-row-1").unwrap();
            assert_eq!(header.size.height, px(ROW_HEIGHT));
            assert_eq!(first.top(), header.bottom());
            assert!(visual.debug_bounds("delimited-row-9000").is_none());
            assert!(visual.debug_bounds("delimited-show-all").is_some());
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
            let show = visual.debug_bounds("delimited-show-all").unwrap();
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
    fn delimited_mouse_range_and_keyboard_copy_preserve_multiline_values(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("quotes.csv");
        let source = "name,value\r\n\"שלום, world\",\"line\r\nnext\"\r\nother,42\r\n";
        std::fs::write(&path, source).unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| TablePreview::new(path.clone(), cx));
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
