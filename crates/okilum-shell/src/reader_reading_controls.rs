//! Reading preferences presentation; persistence belongs to reader_ui_state.
use super::*;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use reader_settings::setting_row;

pub(crate) fn render(window: &mut Window, cx: &mut App) -> AnyElement {
    let width_slider = reading_width_slider(window, cx);
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
                        .disabled(reader_ui_state::base_font_size(cx) <= 12.)
                        .on_click(|_, _, cx| {
                            reader_ui_state::set_reading(
                                reader_ui_state::base_font_size(cx) - 1.,
                                reader_ui_state::reading_width(cx),
                                cx,
                            )
                        }),
                )
                .child(
                    div()
                        .w(px(36.))
                        .text_center()
                        .child(format!("{:.1}", reader_ui_state::base_font_size(cx))),
                )
                .child(
                    Button::new("reading-larger")
                        .debug_selector(|| "reading-larger".into())
                        .ghost()
                        .icon(IconName::Plus)
                        .accessibility_label("Larger text")
                        .tooltip("Larger text")
                        .disabled(reader_ui_state::base_font_size(cx) >= 24.)
                        .on_click(|_, _, cx| {
                            reader_ui_state::set_reading(
                                reader_ui_state::base_font_size(cx) + 1.,
                                reader_ui_state::reading_width(cx),
                                cx,
                            )
                        }),
                ),
            cx,
        ))
        .child(setting_row(
            "Normal note width",
            "Width of the note column when full note width is off.",
            width_slider,
            cx,
        ))
        .into_any_element()
}

/// 560–1200 px in 10 px steps; the value applies while dragging.
fn reading_width_slider(window: &mut Window, cx: &mut App) -> AnyElement {
    let width = reader_ui_state::reading_width(cx);
    let state = window.use_keyed_state("reading-width-slider", cx, |_, cx| {
        cx.subscribe_self(|_, event: &SliderEvent, cx| {
            let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
            reader_ui_state::set_reading_width(value.start(), cx);
        })
        .detach();
        SliderState::new()
            .min(560.)
            .max(1200.)
            .step(10.)
            .default_value(width)
    });
    h_flex()
        .gap_3()
        .items_center()
        .child(
            div()
                .debug_selector(|| "reading-width".into())
                .w(px(200.))
                .child(Slider::new(&state).horizontal()),
        )
        .child(
            div()
                .w(px(64.))
                .text_right()
                .child(format!("{width:.0} px")),
        )
        .into_any_element()
}
