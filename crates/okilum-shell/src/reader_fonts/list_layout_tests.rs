//! #2239: measure actual rendered list geometry without changing note source.
use super::*;
use ::core::prelude::v1::test;
use gpui::{TestAppContext, VisualTestContext};

struct Document {
    text: Entity<TextViewState>,
    reader_style: bool,
}

impl Render for Document {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = if self.reader_style {
            text_style(cx)
        } else {
            reader_text_style(cx.theme())
        };
        div()
            .size_full()
            .text_size(px(14.))
            .child(TextView::new(&self.text).style(style).scrollable(true))
    }
}

fn frame(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.simulate_next_frame(cx));
    visual.run_until_parked();
}

// Each paragraph is one distinct segment; probe the painted region rather
// than guessing the renderer's pointer-derived segment IDs.
fn segments(text: &TextViewState) -> Vec<(usize, Point<Pixels>)> {
    let bounds = text.bounds();
    let mut found = std::collections::HashMap::new();
    for y in (0..f32::from(bounds.size.height) as usize).step_by(2) {
        if let Some((id, _, _)) = text.rendered_source_at(Some(point(
            bounds.left() + px(10.),
            bounds.top() + px(y as f32),
        ))) {
            if let Some(start) = text.rendered_position_for_source(id, 0) {
                found.insert(id, start);
            }
        }
    }
    let mut found = found.into_iter().collect::<Vec<_>>();
    found.sort_by(|a, b| f32::from(a.1.y).total_cmp(&f32::from(b.1.y)));
    found
}

#[gpui::test]
fn root_list_indent_moves_numbered_bulleted_nested_and_wrapped_text_as_one_unit(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let long = "words that wrap onto multiple lines without changing their source ".repeat(3);
    let source = format!(
        "Before\n\n1. Numbered {long}\n   - Nested bullet {long}\n2. Second item\n\nAfter\n\n- Bullet {long}\n  1. Nested number {long}\n\nTail\n"
    );
    let mut document = None;
    let mut text = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| TextViewState::markdown(&source, cx));
        text = Some(state.clone());
        let entity = cx.new(|_| Document {
            text: state,
            reader_style: false,
        });
        document = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let document = document.unwrap();
    let text = text.unwrap();
    visual.simulate_resize(size(px(360.), px(1600.)));
    frame(visual);
    let before = text.read_with(visual, |text, _| segments(text));
    assert_eq!(
        before.len(),
        8,
        "all paragraphs/list bodies must be painted"
    );
    let rem = visual.update(|window, _| window.rem_size());
    document.update(visual, |document, cx| {
        document.reader_style = true;
        cx.notify();
    });
    frame(visual);
    let after = text.read_with(visual, |text, _| segments(text));
    assert_eq!(after.len(), before.len());
    for (index, ((_, old), (id, new))) in before.iter().zip(&after).enumerate() {
        let expected = if [0, 4, 7].contains(&index) {
            px(0.)
        } else {
            rem
        };
        assert!(
            (new.x - old.x - expected).abs() < px(0.1),
            "segment{index}: before={old:?} after={new:?} expected indent={expected:?}"
        );
        if [1, 2, 5, 6].contains(&index) {
            text.read_with(visual, |text, _| {
                let positions = (0..1000)
                    .filter_map(|offset| text.rendered_position_for_source(*id, offset))
                    .collect::<Vec<_>>();
                assert!(
                    positions
                        .iter()
                        .all(|point| point.x <= text.bounds().right() + px(1.)),
                    "list text stays inside the viewport"
                );
                let mut rows = std::collections::BTreeMap::<u32, Pixels>::new();
                for point in positions.into_iter().filter(|point| point.y > new.y) {
                    rows.entry(f32::from(point.y).to_bits())
                        .and_modify(|left| *left = (*left).min(point.x))
                        .or_insert(point.x);
                }
                assert!(!rows.is_empty(), "long item must actually wrap");
                // GPUI's position_for_index has inclusive wrap-end affinity:
                // the first byte of a new row maps to the preceding row's end.
                // With the headless uniform NoopTextSystem, its first available
                // position is therefore exactly one glyph past the row origin.
                let advance = text.rendered_position_for_source(*id, 1).unwrap().x - new.x;
                assert!(advance > px(0.), "glyph-width positive control");
                for left in rows.values() {
                    assert!((*left - new.x - advance).abs() < px(0.1),
                        "segment{index}: wrapped row left={left:?}, body={new:?}, glyph={advance:?}");
                }
            });
        }
    }
    assert!(after[2].1.x > after[1].1.x, "nested bullet remains nested");
    assert!(after[6].1.x > after[5].1.x, "nested number remains nested");
    text.update(visual, |text, cx| {
        text.set_selection_format(gpui_component::text::SelectionFormat::Source, cx);
        text.select_all(cx);
        assert_eq!(
            text.selected_text(),
            source,
            "layout never inserts source whitespace"
        );
    });
}
