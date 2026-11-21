//! #3073: opening a 10 MiB note in Source froze the UI thread. The editor's
//! first frame wrapped every row of the note twice (once for the wrap width,
//! once for the wrapping indent), each time through gpui's decoded-char
//! wrapper and a rope lookup per row. It now wraps once, ASCII rows through a
//! byte-table copy of the same wrapper, and walks the rows in one pass. These
//! tests pin the single re-wrap and that the result is unchanged.
use ::core::prelude::v1::test;
use gpui::*;
use gpui_component::{
    input::{wrap_ascii_line, DisplayMap, Editor, EditorState, Rope, WrappingIndent},
    Root,
};

const FONT_SIZE: Pixels = px(13.);

fn source_font() -> Font {
    font("Okilum Test Mono")
}

/// A small deterministic generator: the same lines on every run.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % below
    }
}

/// ASCII lines that exercise every branch of the wrapper: words and spaces,
/// punctuation that is and is not a word character, leading indentation, runs
/// of spaces, words longer than the width, tabs and a CR.
fn ascii_lines() -> Vec<String> {
    const WORDS: &[&str] = &[
        "a",
        "notes",
        "field",
        "-",
        "x_y",
        "3.14",
        "I'm",
        "@who",
        "#tag",
        "100%",
        "a/b",
        "foo?b=2",
        "(see)",
        "[x]",
        "{y}",
        "plz!",
        "\"q\"",
        "<tag>",
        "a+b",
        "c*d",
        "e&f",
        "|",
        "\\",
        "`code`",
        "~",
        "^",
        ",",
        ";",
        ":",
        "=",
        "\t",
        "\r",
        "averyveryveryverylongwordwithoutanybreaks",
    ];
    let mut rng = Lcg(3073);
    let mut lines = vec![
        String::new(),
        " ".into(),
        "    ".into(),
        "x".into(),
        "\r".into(),
        "a".repeat(400),
    ];
    for _ in 0..600 {
        let mut line = " ".repeat(rng.next(4) * rng.next(3));
        for _ in 0..rng.next(40) {
            line.push_str(WORDS[rng.next(WORDS.len())]);
            line.push_str(&" ".repeat(rng.next(3)));
        }
        lines.push(line);
    }
    lines
}

/// The editor's ASCII wrap gives exactly the boundaries and indents of
/// `gpui::LineWrapper::wrap_line` for the same font, size and width. A change
/// to gpui's wrapping rules after an upgrade makes this fail.
#[gpui::test]
fn issue3073_ascii_wrap_matches_the_line_wrapper(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let font = source_font();
        let mut wrapper = cx.text_system().line_wrapper(font.clone(), FONT_SIZE);
        let em = cx
            .text_system()
            .layout_width(cx.text_system().resolve_font(&font), FONT_SIZE, 'm');
        assert!(em > px(0.), "positive control: the test font has widths");
        let (mut wrapped, mut several, mut indented) = (0, 0, 0);
        for line in ascii_lines() {
            for columns in [1., 3., 8., 20., 41.5, 80., 200.] {
                let width = em * columns;
                let expected: Vec<(usize, u32)> = wrapper
                    .wrap_line(&[LineFragment::text(&line)], width)
                    .map(|b| (b.ix, b.next_indent))
                    .collect();
                let actual = wrap_ascii_line(&font, FONT_SIZE, &line, width, cx)
                    .expect("an ASCII line takes the ASCII path");
                assert_eq!(actual, expected, "line {line:?} at {columns} columns");
                wrapped += usize::from(!expected.is_empty());
                several += usize::from(expected.len() > 2);
                indented += usize::from(expected.iter().any(|(_, indent)| *indent > 0));
            }
        }
        assert!(wrapped > 1000 && several > 500 && indented > 100, "positive control: {wrapped} wrapped, {several} with 3+ boundaries, {indented} indented");
        assert_eq!(
            wrap_ascii_line(&font, FONT_SIZE, "привет мир", em * 4., cx),
            None,
            "a non-ASCII line keeps gpui's wrapper"
        );
    });
}

/// Rows a buffer line takes, as the editor computed them before #3073: gpui's
/// wrapper per line, the first visual row of a `None` indent wrapped as is.
fn expected_rows(
    wrapper: &mut LineWrapperHandle,
    line: &str,
    width: Pixels,
    indent: WrappingIndent,
) -> usize {
    let mut boundaries = 0;
    let mut last = 0;
    match indent {
        WrappingIndent::Same => {
            for b in wrapper.wrap_line(&[LineFragment::text(line)], width) {
                boundaries += 1;
                last = b.ix;
            }
        }
        WrappingIndent::None => {
            let first = wrapper
                .wrap_line(&[LineFragment::text(line)], width)
                .next()
                .map(|b| b.ix);
            if let Some(first) = first {
                boundaries += 1;
                last = first;
                for b in wrapper.wrap_line(&[LineFragment::text(&line[first..])], width) {
                    boundaries += 1;
                    last = first + b.ix;
                }
            }
        }
    }
    boundaries + usize::from(last < line.len() || last == 0)
}

/// A note whose rows span rope chunks, end in CR, are empty, are not ASCII
/// (no emoji, so gpui wraps them as one text fragment) and end with a LF.
fn mixed_note() -> String {
    let mut rng = Lcg(653);
    let mut lines = ascii_lines();
    lines.push("Шла Саша по шоссе и сосала сушку ".repeat(30));
    lines.push("שלום עולם ".repeat(40));
    lines.push("crlf row\r".into());
    lines.push(String::new());
    for _ in 0..40 {
        let len = 2_000 + rng.next(6_000);
        lines.push("word ".repeat(len / 5));
    }
    let mut note = lines.join("\n");
    note.push('\n');
    note
}

/// Laid out through the editor's display map, every row wraps exactly as
/// gpui's wrapper wraps it, in both indent modes, and one layout change for
/// width and indent re-wraps the note once.
#[gpui::test]
fn issue3073_display_map_rows_match_the_line_wrapper(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let note = mixed_note();
        let lines: Vec<&str> = note.split('\n').collect();
        assert!(
            lines.iter().any(|line| line.len() > 4_096),
            "positive control: rows span rope chunks"
        );
        let font = source_font();
        let em =
            cx.text_system()
                .layout_width(cx.text_system().resolve_font(&font), FONT_SIZE, 'm');
        let mut wrapper = cx.text_system().line_wrapper(font.clone(), FONT_SIZE);
        for indent in [WrappingIndent::None, WrappingIndent::Same] {
            for columns in [9., 33., 96.] {
                let width = em * columns;
                let mut map = DisplayMap::new(font.clone(), FONT_SIZE, None);
                map.set_text(&Rope::from(note.as_str()), cx);
                assert!(
                    map.set_layout(Some(width), indent, cx),
                    "a new layout re-wraps"
                );
                assert!(
                    !map.set_layout(Some(width), indent, cx),
                    "the same layout does not re-wrap again"
                );
                assert_eq!(
                    map.buffer_line_count(),
                    lines.len(),
                    "one buffer line per row"
                );
                let mut total = 0;
                let mut multi = 0;
                for (ix, line) in lines.iter().enumerate() {
                    let expected = expected_rows(&mut wrapper, line, width, indent);
                    let rows = map
                        .buffer_line_to_display_row_range(ix)
                        .expect("every line is shown")
                        .len();
                    assert_eq!(
                        rows,
                        expected,
                        "line {ix} ({} bytes), {indent:?}, {columns} columns",
                        line.len()
                    );
                    total += rows;
                    multi += usize::from(rows > 1);
                }
                assert_eq!(map.display_row_count(), total);
                assert!(
                    multi > 100,
                    "positive control: {multi} lines wrap at {columns} columns"
                );
            }
        }
    });
}

struct Surface(Entity<EditorState>);
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(Editor::new(&self.0).size_full())
    }
}

/// The first frame of a Source editor (soft wrap, no wrapping indent) wraps
/// the note once and records that re-wrap; before #3073 it wrapped it twice.
#[gpui::test]
fn issue3073_first_source_frame_wraps_the_note_once(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let note: String = (0..20_000)
        .map(|i| format!("Row {i} of a long note, with words that wrap at a narrow width.\n"))
        .collect();
    let mut input = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .searchable(true)
                .soft_wrap(true)
                .wrapping_indent(WrappingIndent::None);
            state.enable_layout_diagnostics();
            state.set_value(note.clone(), window, cx);
            state
        });
        input = Some(state.clone());
        Root::new(cx.new(|_| Surface(state)), window, cx)
    });
    let input = input.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    input.read_with(visual, |state, _| {
        let diagnostics = state.layout_diagnostics().unwrap();
        assert!(
            diagnostics.layout_calls > 0,
            "positive control: the editor was laid out"
        );
        // The first layout sets the wrap width and the wrapping indent.
        assert_eq!(
            diagnostics.relayouts, 1,
            "the first frame wraps the note once"
        );
        let relayout = state
            .last_relayout()
            .expect("the first frame records its re-wrap");
        assert_eq!(relayout.rows, 20_001, "every source row was wrapped");
    });
    // The same size again: no re-wrap. A new width: one more.
    visual.update(|window, cx| window.draw(cx).clear(cx));
    input.read_with(visual, |state, _| {
        assert_eq!(
            state.layout_diagnostics().unwrap().relayouts,
            1,
            "an unchanged layout"
        );
    });
    visual.simulate_resize(size(px(500.), px(300.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    input.read_with(visual, |state, _| {
        assert_eq!(
            state.layout_diagnostics().unwrap().relayouts,
            2,
            "a new width wraps once"
        );
    });
}
