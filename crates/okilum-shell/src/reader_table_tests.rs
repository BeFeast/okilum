//! Downstream regressions execute in Okilum CI (the vendor workspace is excluded).
#[rustfmt::skip]
#[path = "../../../vendor/gpui-component/crates/base/src/text/table_wrap.rs"]
mod table_wrap;

use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn table_rows_fit_wrapped_content_and_keep_shared_columns(cx: &mut gpui::TestAppContext) {
    struct TableFixture {
        width: f32,
    }
    impl Render for TableFixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(self.width)).child(
                TextView::markdown(
                    "wrap-fixture",
                    include_str!("../../../fixtures/reader/table-wrapping.md"),
                )
                .style(reader_text_style(cx.theme()))
                .text_size(px(BODY_FONT_SIZE)),
            )
        }
    }
    const CELLS: [[&str; 3]; 5] = [
        ["table-cell-0-0", "table-cell-0-1", "table-cell-0-2"],
        ["table-cell-1-0", "table-cell-1-1", "table-cell-1-2"],
        ["table-cell-2-0", "table-cell-2-1", "table-cell-2-2"],
        ["table-cell-3-0", "table-cell-3-1", "table-cell-3-2"],
        ["table-cell-4-0", "table-cell-4-1", "table-cell-4-2"],
    ];
    cx.update(crate::test_init);
    let (view, visual) = cx.add_window_view(|_, _| TableFixture { width: 660. });
    let mut heights = Vec::new();
    for width in [660., 420., 660.] {
        view.update(visual, |v, cx| {
            v.width = width;
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let first = visual
            .debug_bounds("table-cell-1-0")
            .expect("table rendered");
        assert!(
            first.size.height > px(2. * BODY_FONT_SIZE),
            "positive control: multiple lines"
        );
        for row in 1..=4 {
            let previous = visual.debug_bounds(CELLS[row - 1][0]).unwrap();
            let current = visual.debug_bounds(CELLS[row][0]).unwrap();
            assert!(
                current.top() >= previous.bottom(),
                "row {row} overlaps its predecessor"
            );
            for (cell, header) in CELLS[row].iter().zip(CELLS[0].iter()) {
                let a = visual.debug_bounds(cell).unwrap();
                let b = visual.debug_bounds(header).unwrap();
                assert_eq!(a.left(), b.left());
                assert_eq!(a.size.width, b.size.width);
                assert_eq!(a.top(), current.top());
                assert_eq!(a.bottom(), current.bottom());
            }
        }
        heights.push(first.size.height);
    }
    assert!(heights[1] >= heights[0]);
    assert_eq!(heights[0], heights[2], "resize restores original layout");
}

/// The Read-mode table menu hook (#2025) observes right presses anywhere on a
/// table without moving a single cell, in both table layouts.
#[gpui::test]
fn table_secondary_press_keeps_layout_and_reports_the_pressed_table(cx: &mut gpui::TestAppContext) {
    type Pressed = Arc<std::sync::Mutex<Vec<(Option<String>, Vec<String>)>>>;
    struct HookFixture {
        hook: bool,
        scroll: bool,
        pressed: Pressed,
    }
    impl Render for HookFixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let style = if self.scroll {
                reader_text_style(cx.theme())
            } else {
                TextViewStyle::default()
            };
            let view = TextView::markdown(
                "hook-fixture",
                include_str!("../../../fixtures/reader/table-wrapping.md"),
            )
            .style(style)
            .text_size(px(BODY_FONT_SIZE));
            let pressed = self.pressed.clone();
            div()
                .w(px(660.))
                .debug_selector(|| "hook-fixture".into())
                .child(if self.hook {
                    view.on_table_secondary_press(move |press, _, _, _| {
                        pressed.lock().unwrap().push((
                            press.source().map(str::to_owned),
                            press.data().headers.clone(),
                        ))
                    })
                } else {
                    view
                })
        }
    }
    const SELECTORS: [&str; 16] = [
        "table-content-frame",
        "table-cell-0-0",
        "table-cell-0-1",
        "table-cell-0-2",
        "table-cell-1-0",
        "table-cell-1-1",
        "table-cell-1-2",
        "table-cell-2-0",
        "table-cell-2-1",
        "table-cell-2-2",
        "table-cell-3-0",
        "table-cell-3-1",
        "table-cell-3-2",
        "table-cell-4-0",
        "table-cell-4-1",
        "table-cell-4-2",
    ];
    let source = include_str!("../../../fixtures/reader/table-wrapping.md");
    cx.update(gpui_component::init);
    let pressed = Pressed::default();
    let (view, visual) = cx.add_window_view(|_, _| HookFixture {
        hook: false,
        scroll: true,
        pressed: pressed.clone(),
    });
    for scroll in [true, false] {
        let mut layouts = Vec::new();
        for hook in [false, true] {
            view.update(visual, |v, cx| {
                v.hook = hook;
                v.scroll = scroll;
                cx.notify();
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let root = visual
                .debug_bounds("hook-fixture")
                .expect("fixture rendered");
            let cells = if scroll {
                SELECTORS
                    .iter()
                    .map(|s| visual.debug_bounds(s).expect("scroll table rendered"))
                    .collect()
            } else {
                Vec::new()
            };
            layouts.push((root, cells));
            pressed.lock().unwrap().clear();
            let inside = match scroll {
                true => visual.debug_bounds("table-cell-2-1").unwrap().center(),
                false => point(root.center().x, root.top() + px(BODY_FONT_SIZE * 3.)),
            };
            visual.simulate_mouse_down(inside, MouseButton::Right, Modifiers::default());
            visual.simulate_mouse_up(inside, MouseButton::Right, Modifiers::default());
            visual.run_until_parked();
            let presses = pressed.lock().unwrap().clone();
            if hook {
                assert_eq!(presses.len(), 1, "positive control: the press was observed");
                assert_eq!(
                    presses[0].0.as_deref(),
                    Some(source.trim_end_matches(['\r', '\n'])),
                    "the hook carries the exact table source"
                );
                assert_eq!(presses[0].1, ["Instrument", "Tuning", "Description"]);
            } else {
                assert!(presses.is_empty());
            }
        }
        assert_eq!(
            layouts[0], layouts[1],
            "the hook leaves the layout unchanged"
        );
        if scroll {
            assert_eq!(layouts[0].1.len(), SELECTORS.len());
        }
    }
}
