//! Downstream regressions execute in Okilum CI (the vendor workspace is excluded).
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
    cx.update(gpui_component::init);
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
