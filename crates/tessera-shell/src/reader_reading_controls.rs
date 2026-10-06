//! Global reading controls, separate from theme selection and Settings chrome.
use super::*;
use gpui_component::button::ButtonGroup;

pub(crate) fn render(cx: &App) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_4()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_4()
                .child("Reading text size")
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("reading-smaller")
                                .icon(IconName::Minus)
                                .ghost()
                                .tooltip("Smaller text")
                                .disabled(reader_ui_state::font_size(cx) <= 12.)
                                .on_click(|_, _, cx| {
                                    reader_ui_state::set_reading(
                                        reader_ui_state::font_size(cx) - 1.,
                                        reader_ui_state::reading_width(cx),
                                        cx,
                                    )
                                }),
                        )
                        .child(format!("{:.1}", reader_ui_state::font_size(cx)))
                        .child(
                            Button::new("reading-larger")
                                .icon(IconName::Plus)
                                .ghost()
                                .tooltip("Larger text")
                                .disabled(reader_ui_state::font_size(cx) >= 24.)
                                .on_click(|_, _, cx| {
                                    reader_ui_state::set_reading(
                                        reader_ui_state::font_size(cx) + 1.,
                                        reader_ui_state::reading_width(cx),
                                        cx,
                                    )
                                }),
                        ),
                ),
        )
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_4()
                .child("Reading width")
                .child(
                    ButtonGroup::new("reading-width").children(
                        [
                            ("reading-narrow", "Narrow", 620.),
                            ("reading-comfort", "Comfort", READER_MAX_WIDTH),
                            ("reading-wide", "Wide", 960.),
                        ]
                        .map(|(id, label, width)| {
                            Button::new(id)
                                .label(label)
                                .selected((reader_ui_state::reading_width(cx) - width).abs() < 1.)
                                .on_click(move |_, _, cx| {
                                    reader_ui_state::set_reading(
                                        reader_ui_state::font_size(cx),
                                        width,
                                        cx,
                                    )
                                })
                        }),
                    ),
                ),
        )
}
