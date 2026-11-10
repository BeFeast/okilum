//! #1641: measure native Live Preview geometry and the actual painted quote bars.
use super::*;
use ::core::prelude::v1::test;
use gpui_component::input::WrappingIndent;

struct Surface(Entity<EditorState>);
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(Editor::new(&self.0).size_full())
    }
}

fn frame(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
}

fn editor<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
) -> (Entity<EditorState>, &'a mut VisualTestContext) {
    cx.update(crate::test_init);
    let mut input = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .searchable(false)
                .wrapping_indent(WrappingIndent::None);
            state.set_value(text.to_owned(), window, cx);
            state.set_projection_provider(
                Some(Arc::new(CachedProvider::classify(state.source_snapshot()))),
                cx,
            );
            state.focus_handle(cx).focus(window, cx);
            state
        });
        input = Some(state.clone());
        Root::new(cx.new(|_| Surface(state)), window, cx)
    });
    (input.unwrap(), visual)
}

// Sample every source character, including row ends: an overflow assertion must
// measure the right edge too, rather than only testing the beginning of each row.
fn positions(input: &EditorState, range: std::ops::Range<usize>) -> Vec<Point<Pixels>> {
    let text = input.value();
    text[range.clone()]
        .char_indices()
        .map(|(at, _)| {
            let at = range.start + at;
            input
                .range_to_bounds(&(at..at))
                .expect("painted source byte")
                .origin
        })
        .collect()
}

fn assert_rows(input: &EditorState, body: std::ops::Range<usize>) -> Vec<Point<Pixels>> {
    let positions = positions(input, body);
    let first = positions[0];
    let mut rows = std::collections::BTreeMap::<u32, Pixels>::new();
    for position in &positions {
        assert!(
            position.x <= input.input_bounds().right() + px(1.),
            "body {position:?} overflows editor {:?}",
            input.input_bounds()
        );
        rows.entry(f32::from(position.y).to_bits())
            .and_modify(|left| *left = (*left).min(position.x))
            .or_insert(position.x);
    }
    assert!(
        rows.len() >= 3,
        "positive control: item actually wraps: {rows:?}"
    );
    for left in rows.values() {
        assert!(
            (*left - first.x).abs() <= px(1.),
            "continuation {left:?} must align under item text {first:?}"
        );
    }
    positions
}

#[gpui::test]
fn gutter_narrow_deep_items_stay_inside_and_align(cx: &mut TestAppContext) {
    for depth in [0, 4, 10, 20] {
        let parents = (0..depth)
            .map(|d| format!("{}- parent\n", "  ".repeat(d)))
            .collect::<String>();
        for marker in ["- ", "10. ", "- [ ] "] {
            let prefix = format!("{}{marker}", "  ".repeat(depth));
            let words = "Anchor words with **bold** and Unicode café 🧠 ".repeat(15);
            let text = format!("Outside\n\n{parents}{prefix}{words}\n\nTail\n");
            let start = text.find("Anchor").unwrap();
            let (input, visual) = editor(cx, &text);
            for width in [240., 360., 600.] {
                visual.simulate_resize(size(px(width), px(2200.)));
                input.update(visual, |i, cx| i.set_selected_range(0..0, cx));
                frame(visual);
                input.read_with(visual, |i, _| {
                    assert_rows(i, start..start + words.len());
                    assert_eq!(i.value().as_ref(), text, "wrap never edits source");
                });
            }
        }
    }
}

#[gpui::test]
fn gutter_quote_bars_are_painted_on_every_wrapped_row(cx: &mut TestAppContext) {
    for depth in [1, 3, 10] {
        let words = "Anchor quoted words and more words ".repeat(15);
        let text = format!("Outside\n\n{}{words}\n\nTail\n", "> ".repeat(depth));
        let start = text.find("Anchor").unwrap();
        let (input, visual) = editor(cx, &text);
        visual.simulate_resize(size(px(240.), px(2200.)));
        frame(visual);
        let row_starts = |points: Vec<Point<Pixels>>| {
            let mut rows = Vec::new();
            for p in points {
                if rows.last().is_none_or(|last: &Point<Pixels>| last.y != p.y) {
                    rows.push(p);
                }
            }
            rows
        };
        let (rows, body_rows) = input.read_with(visual, |i, _| {
            // The first source row owns the marker even if a broken wrapper
            // puts the entire body on another row. Detect that first bar
            // independently, rather than mistaking this bug for a dead probe.
            let quote_start = start - depth * 2;
            (
                row_starts(positions(i, quote_start..start + words.len())),
                row_starts(positions(i, start..start + words.len())),
            )
        });
        assert!(rows.len() >= 3, "positive control: quote really wraps");
        let row_height = rows[1].y - rows[0].y;
        visual.update(|window, _| {
            let scale = window.scale_factor();
            let bars = window
                .painted_quads()
                .into_iter()
                .filter(|q| {
                    (px(q.bounds.size.width.as_f32() / scale) - px(2.)).abs() < px(0.1)
                        && px(q.bounds.size.height.as_f32() / scale) >= row_height - px(1.)
                })
                .collect::<Vec<_>>();
            let bar_columns = |p: Point<Pixels>| {
                let mut columns = bars
                    .iter()
                    .filter(|q| {
                        (px(q.bounds.top().as_f32() / scale) - p.y).abs() < px(1.)
                            && q.bounds.intersect(&q.content_mask.bounds).size.width
                                > gpui::ScaledPixels(0.)
                    })
                    .map(|q| px(q.bounds.left().as_f32() / scale))
                    .collect::<Vec<_>>();
                columns.sort_by(|a, b| f32::from(*a).total_cmp(&f32::from(*b)));
                columns
            };
            let first = bar_columns(rows[0]);
            assert!(
                !first.is_empty(),
                "positive control: first quote bar painted"
            );
            for row in &rows[1..] {
                let columns = bar_columns(*row);
                assert_eq!(
                    columns.len(),
                    first.len(),
                    "every continuation row paints the same quote bars: {row:?}"
                );
                for (actual, expected) in columns.iter().zip(&first) {
                    assert!(
                        (*actual - *expected).abs() <= px(1.),
                        "quote bar stays vertical: {actual:?} vs {expected:?}"
                    );
                }
            }
            for body in &body_rows {
                for q in bars.iter().filter(|q| {
                    (px(q.bounds.top().as_f32() / scale) - body.y).abs() < px(1.)
                        && q.bounds.intersect(&q.content_mask.bounds).size.width
                            > gpui::ScaledPixels(0.)
                }) {
                    assert!(
                        px(q.bounds.right().as_f32() / scale) <= body.x + px(1.),
                        "quote bars remain in the gutter, left of body glyphs"
                    );
                }
            }
            diagnostic_error!(
                "GUTTER_PAINT depth={depth} rows={} visible_bars={}",
                rows.len(),
                first.len()
            );
        });
    }
}

#[gpui::test]
fn gutter_large_live_preview_caret_reveal_moves_text_at_most_one_pixel(cx: &mut TestAppContext) {
    let words = "Anchor plain wrapped words with **bold** and `code` ".repeat(6);
    let head = format!(
        "Outside\n\n- {words}\n  - {words}\n\n1. {words}\n\n- [ ] {words}\n\n> {words}\n> > {words}\n\n"
    );
    // Stay below the structural-node cap: thousands of tiny styled paragraphs
    // would deliberately fall back to raw Source and could fake a stable caret.
    let padding = format!("Filler paragraph {}.\n\n", "plain words ".repeat(120));
    let text = format!("{head}{}", padding.repeat(80));
    assert!(
        text.len() > 64 * 1024,
        "large publication path positive control"
    );
    let (reader, visual, _directory) = super::tests::fixture(cx, &text);
    visual.simulate_resize(size(px(1100.), px(2400.)));
    reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
    frame(visual);
    let input = reader.read_with(visual, |r, cx| {
        let e = r.editing.as_ref().unwrap();
        assert!(e.live_preview.accepted.is_some(), "actual worker adopted");
        assert_eq!(e.input.read(cx).value().as_ref(), text);
        e.input.clone()
    });
    let mut line_start = 0;
    let starts = head
        .lines()
        .filter_map(|line| {
            let start = line.find("Anchor").map(|at| line_start + at);
            line_start += line.len() + 1;
            start
        })
        .collect::<Vec<_>>();
    let mut max_shift = 0.0_f32;
    for start in starts {
        input.update(visual, |i, cx| i.set_selected_range(0..0, cx));
        frame(visual);
        let before = input.read_with(visual, |i, _| assert_rows(i, start..start + words.len()));
        let marker = start + words.find("**").unwrap();
        let hidden_width = input.read_with(visual, |i, _| {
            i.range_to_bounds(&(marker..marker + 2)).unwrap().size.width
        });
        assert!(hidden_width < px(1.),
            "positive control: Live Preview conceals inline syntax on item {start}: {hidden_width:?}");
        // Enter and leave the marker, body, and the inline raw reveal scope.
        for caret in [start - 1, start, start + words.find("bold").unwrap(), 0] {
            input.update(visual, |i, cx| i.set_selected_range(caret..caret, cx));
            frame(visual);
            input.read_with(visual, |i, _| {
                assert_rows(i, start..start + words.len());
                if caret == marker + 2 {
                    let revealed_width =
                        i.range_to_bounds(&(marker..marker + 2)).unwrap().size.width;
                    assert!(
                        revealed_width > hidden_width + px(1.),
                        "positive control: item {start}, caret {caret} revealed inline markers: {hidden_width:?} -> {revealed_width:?}"
                    );
                }
                let after = positions(i, start..start + "Anchor".len());
                for (old, new) in before.iter().zip(after) {
                    max_shift = max_shift.max(f32::from((old.x - new.x).abs()));
                    assert!(
                        (old.x - new.x).abs() <= px(1.),
                        "caret {caret} moved item text: {old:?} -> {new:?}"
                    );
                }
            });
        }
    }
    diagnostic_error!(
        "GUTTER_LARGE_NOTE bytes={} max_text_shift_px={max_shift}",
        text.len()
    );
}
