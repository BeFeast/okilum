//! Read-only delimited tables; editing and recovery stay in FileEditor.
use super::*;
use gpui_component::scroll::ScrollableElement as _;
use okilum_core::delimited::{ReadLimits, Table};
use std::cell::RefCell;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation as _;

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
/// Text files the Reader can open in its editor: tables, plain text and,
/// since #998, source code.
pub(crate) fn editable(rel: &str) -> bool {
    eligible(rel)
        || crate::reader_code_file::eligible(rel)
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
    error: Option<&'static str>,
    loading: bool,
    all: bool,
    anchor: Option<Cell>,
    end: Option<Cell>,
    scroll: UniformListScrollHandle,
    horizontal: ScrollHandle,
    focus: FocusHandle,
}
impl TablePreview {
    pub(crate) fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
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
                Table::read(
                    file,
                    load_path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("tsv")),
                    if all { EXPANDED_LIMITS } else { PREVIEW_LIMITS },
                )
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(table) => {
                        this.table = Some(Arc::new(table));
                        this.all = all;
                        // The file may have changed between reads; never copy
                        // a selection against a different source snapshot.
                        this.anchor = None;
                        this.end = None;
                    }
                    Err(error) => {
                        eprintln!("Cannot read delimited file: {error:#}");
                        this.error = Some(if error.kind() == std::io::ErrorKind::InvalidData {
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
                    .hoverable_tooltip(move |window, cx| cell_tooltip(value.clone(), window, cx))
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

fn cell_tooltip(value: String, window: &mut Window, cx: &mut App) -> AnyView {
    let scroll = ScrollHandle::new();
    let rows = Rc::new(RefCell::new(None::<(f32, Vec<TooltipRow>)>));
    gpui_component::tooltip::Tooltip::element(move |window, cx| {
        // Leave room for the tooltip's padding/margin and the window edges.
        // A retained field can be 64 KiB, so wrapping alone is insufficient:
        // keep the tooltip hoverable and let the reader scroll to its end.
        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) - 64.).clamp(1., 480.);
        let height = (f32::from(viewport.height) - 64.).clamp(1., 360.);
        // GPUI's wrapped text loses RTL-first paragraphs (one stray glyph, the
        // rest off-row), so each row is wrapped here and shaped as one line.
        let family = cx.theme().font_family.clone();
        let font_size = rems(0.875).to_pixels(window.rem_size());
        let mut cached = rows.borrow_mut();
        if cached.as_ref().is_none_or(|(at, _)| *at != width) {
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
            *cached = Some((width, tooltip_rows(&value, width - 1., measure)));
        }
        let rows = cached
            .as_ref()
            .map(|(_, rows)| rows.clone())
            .unwrap_or_default();
        div()
            .id("delimited-cell-tooltip")
            .debug_selector(|| "delimited-cell-tooltip".into())
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
                .child(div().px_4().child(self.error.unwrap_or("Opening table…")))
                .into_any_element();
        };
        if table.rows.is_empty() && !table.limited {
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
        if header && !table.rows.is_empty() {
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
        if self.all && table.limited {
            notices.push(
                "This file exceeds table preview limits. Open it externally to view the rest.",
            );
        }
        if let Some(error) = self.error {
            notices.push(error);
        }
        if table.rows.is_empty() && table.limited && !self.all {
            notices.push("No complete records fit in the initial preview.");
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
                    .id("delimited-notice")
                    .debug_selector(|| "delimited-notice".into())
                    .px_4()
                    .py_1()
                    .text_color(palette.text_muted)
                    .child(notices.join(" ")),
            );
        }
        if !self.all && (count < available || table.limited) {
            view = view.child(
                h_flex()
                    .px_4()
                    .gap_3()
                    .text_color(palette.text_muted)
                    .child(format!("Showing the first {count} rows"))
                    .child(
                        Button::new("delimited-show-all")
                            .debug_selector(|| "delimited-show-all".into())
                            .ghost()
                            .small()
                            .label(if self.loading {
                                "Opening…"
                            } else {
                                "Show all"
                            })
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load(true, cx);
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
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let preview = cx.new(|cx| TablePreview::new(path.clone(), cx));
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
    fn delimited_preview_limits_keep_header_and_offer_explicit_fallback(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("wide.csv");
        let source = format!("a,b\n{}\n", ",".repeat(10_000));
        std::fs::write(&path, &source).unwrap();
        cx.update(gpui_component::init);
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| TablePreview::new(path.clone(), cx));
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
        let show = visual.debug_bounds("delimited-show-all").unwrap();
        visual.simulate_click(show.center(), Modifiers::default());
        visual.run_until_parked();
        preview.read_with(visual, |p, _| {
            assert!(p.all && !p.loading);
            assert!(p.table.as_ref().unwrap().limited);
        });
        assert!(visual.debug_bounds("delimited-row-0").is_some());
        assert!(visual.debug_bounds("delimited-notice").is_some());
        assert!(visual.debug_bounds("delimited-show-all").is_none());
        assert_eq!(std::fs::read(path).unwrap(), source.as_bytes());
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
