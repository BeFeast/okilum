//! App-registered callbacks for text modules that need app-level services
//! (window-close diagnostics, Mermaid image element, document style, note font)
//! without depending on `Reader` (#3235, epic #3241).
use gpui::{
    img, AnyElement, App, Global, IntoElement, Pixels, RenderImage, SharedString, Size, Styled,
    Window,
};
use gpui_component::{text::TextViewStyle, ActiveTheme as _};
use std::sync::Arc;

type WindowCloseRequestedFn = fn(&Window, &mut App, &'static str);
type RenderMermaidImageFn = fn(Arc<RenderImage>, Size<Pixels>) -> AnyElement;
type DocumentStyleFn = fn(&App) -> TextViewStyle;
type NoteFontFamilyFn = fn(&App) -> SharedString;

struct TextHooks {
    window_close_requested: WindowCloseRequestedFn,
    render_mermaid_image: RenderMermaidImageFn,
    document_style: DocumentStyleFn,
    note_font_family: NoteFontFamilyFn,
}

impl Global for TextHooks {}

/// Register app-level hooks once at startup (and in `test_init`).
pub fn install(
    window_close_requested: WindowCloseRequestedFn,
    render_mermaid_image: RenderMermaidImageFn,
    document_style: DocumentStyleFn,
    note_font_family: NoteFontFamilyFn,
    cx: &mut App,
) {
    cx.set_global(TextHooks {
        window_close_requested,
        render_mermaid_image,
        document_style,
        note_font_family,
    });
}

pub(crate) fn window_close_requested(window: &Window, cx: &mut App, reason: &'static str) {
    if let Some(hooks) = cx.try_global::<TextHooks>() {
        let f = hooks.window_close_requested;
        f(window, cx, reason);
    }
}

pub(crate) fn render_mermaid_image(
    image: Arc<RenderImage>,
    logical_size: Size<Pixels>,
    cx: &App,
) -> AnyElement {
    if let Some(hooks) = cx.try_global::<TextHooks>() {
        let f = hooks.render_mermaid_image;
        return f(image, logical_size);
    }
    img(image)
        .w(logical_size.width)
        .h(logical_size.height)
        .into_any_element()
}

pub(crate) fn document_style(cx: &App) -> TextViewStyle {
    if let Some(hooks) = cx.try_global::<TextHooks>() {
        let f = hooks.document_style;
        return f(cx);
    }
    let mut style = crate::markdown_style::reader_text_style(cx.theme());
    style.code_block.text.font_size =
        Some(crate::reader_text_zoom::code_block_font_size(cx).into());
    style
}

pub(crate) fn note_font_family(cx: &App) -> SharedString {
    if let Some(hooks) = cx.try_global::<TextHooks>() {
        let f = hooks.note_font_family;
        return f(cx);
    }
    cx.theme().font_family.clone()
}
