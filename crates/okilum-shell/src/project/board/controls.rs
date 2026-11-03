//! Card disclosures use Base's complete button interaction without the themed
//! Button's extra 100%-height content wrapper inside an auto-height rich card.
use super::*;
use gpui_component::{Colorize as _, ThemeStyled as _};

pub(super) fn disclosure(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> (gpui_base::Button, FocusHandle) {
    let id = id.into();
    let focus = window
        .use_keyed_state((id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    let secondary = cx.theme().secondary;
    let hover = if cx.theme().mode.is_dark() {
        secondary.lighten(0.1)
    } else {
        secondary.darken(0.1)
    }
    .opacity(0.8);
    let active = if cx.theme().mode.is_dark() {
        secondary.lighten(0.2)
    } else {
        secondary.darken(0.2)
    }
    .opacity(0.8);
    let button = gpui_base::Button::new(id)
        .track_focus(&focus)
        .cursor_default()
        .flex_shrink_0()
        .h_8()
        .px_2p5()
        .text_base()
        .whitespace_nowrap()
        .rounded(cx.theme().radius)
        .bg(cx.theme().transparent)
        .text_color(cx.theme().secondary_foreground)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(active))
        .when(focus.is_focused(window), |button| {
            button.focus_ring_style(window, cx)
        })
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            gpui_base::GlobalState::suppress_text_selection(cx);
        });
    (button, focus)
}
