//! Card disclosures use Base's complete button interaction without the themed
//! Button's extra 100%-height content wrapper inside an auto-height rich card.
use super::*;
use gpui_component::{Colorize as _, ThemeStyled as _};

pub(super) fn disclosure(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> (gpui_base::Button, FocusHandle) {
    ghost_button(id, false, None, window, cx)
}

/// Queue keeps its existing surface hover/press color. GPUI requires each
/// interaction style to be installed once, rather than overwritten later.
pub(super) fn queue_disclosure(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> (gpui_base::Button, FocusHandle) {
    ghost_button(id, false, Some(brand::palette(cx).surface), window, cx)
}

fn ghost_button(
    id: impl Into<ElementId>,
    selected: bool,
    surface: Option<Hsla>,
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
    let hover = surface.unwrap_or(hover);
    let active = surface.unwrap_or(active);
    let button = gpui_base::Button::new(id)
        .track_focus(&focus)
        .cursor_default()
        .flex_shrink_0()
        .h_8()
        .px_2p5()
        .text_base()
        .whitespace_nowrap()
        .rounded(cx.theme().radius)
        .selected(selected)
        .bg(if selected {
            cx.theme().tokens.secondary_active.into()
        } else {
            cx.theme().transparent
        })
        .text_color(cx.theme().secondary_foreground)
        .when(!selected, |button| {
            button
                .hover(move |style| style.bg(hover))
                .active(move |style| style.bg(active))
        })
        .when(focus.is_focused(window), |button| {
            button.focus_ring_style(window, cx)
        })
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            gpui_base::GlobalState::suppress_text_selection(cx);
        });
    (button, focus)
}

/// Compact links retain the same Ghost colors/focus/keyboard contract without
/// adding a second full-size flex container around an icon and a label.
pub(super) fn compact_action(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> gpui_base::Button {
    disclosure(id, window, cx).0.h_6().px_2().text_sm().gap_1()
}

/// Small Ghost options share the styled Button's geometry and interaction,
/// with their label/count laid out directly instead of a full-size inner row.
pub(super) fn compact_option(
    id: impl Into<ElementId>,
    selected: bool,
    window: &mut Window,
    cx: &mut App,
) -> gpui_base::Button {
    ghost_button(id, selected, None, window, cx)
        .0
        .h_6()
        .px_2()
        .text_sm()
        .gap_1()
}
