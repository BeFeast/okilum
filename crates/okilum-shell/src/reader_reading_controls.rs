//! Reading preferences presentation; persistence belongs to reader_ui_state.
use super::*;
use gpui_component::button::ButtonGroup;
use reader_settings::setting_row;

pub(crate) fn render(cx: &App) -> impl IntoElement {
    v_flex()
        .gap_4()
        .child(setting_row(
            "Text size",
            "Reading text across all windows.",
            h_flex()
                .gap_2()
                .child(
                    Button::new("reading-smaller")
                        .debug_selector(|| "reading-smaller".into())
                        .ghost()
                        .icon(IconName::Minus)
                        .accessibility_label("Smaller text")
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
                .child(
                    div()
                        .w(px(36.))
                        .text_center()
                        .child(format!("{:.1}", reader_ui_state::font_size(cx))),
                )
                .child(
                    Button::new("reading-larger")
                        .debug_selector(|| "reading-larger".into())
                        .ghost()
                        .icon(IconName::Plus)
                        .accessibility_label("Larger text")
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
            cx,
        ))
        .child(setting_row(
            "Reading width",
            "Space for your note’s content.",
            ButtonGroup::new("reading-width").flex_none().children(
                [
                    ("reading-narrow", "Narrow", 620.),
                    ("reading-comfort", "Comfort", 740.),
                    ("reading-wide", "Wide", 960.),
                ]
                .map(|(id, label, width)| {
                    let selected = (reader_ui_state::reading_width(cx) - width).abs() < 1.;
                    Button::new(id)
                        .debug_selector(move || id.into())
                        .ghost()
                        .label(label)
                        .selected(selected)
                        .when(selected, |button| button.primary())
                        .on_click(move |_, _, cx| {
                            reader_ui_state::set_reading(reader_ui_state::font_size(cx), width, cx)
                        })
                }),
            ),
            cx,
        ))
}
