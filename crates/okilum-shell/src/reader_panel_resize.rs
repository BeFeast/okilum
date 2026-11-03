//! Pointer-owned splitter; generic DnD forces a whole-window refresh per motion.
use super::*;
use gpui::{
    Element, GlobalElementId, Hitbox, HitboxBehavior, HitboxId, InspectorElementId, LayoutId,
};

pub(super) type Capture = std::rc::Rc<std::cell::Cell<Option<HitboxId>>>;

type Begin = std::rc::Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;
pub(super) struct Handle {
    id: ElementId,
    active: bool,
    capture: Capture,
    begin: Begin,
}
pub(super) fn handle(
    id: &'static str,
    active: bool,
    capture: Capture,
    begin: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> Handle {
    Handle {
        id: id.into(),
        active,
        capture,
        begin: std::rc::Rc::new(begin),
    }
}
impl IntoElement for Handle {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Handle {
    type RequestLayoutState = AnyElement;
    type PrepaintState = Hitbox;
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let theme = gpui_base::Theme::global(cx);
        let color = if self.active {
            theme
                .resizable
                .active_handle
                .unwrap_or(theme.tokens.colors.ring)
        } else {
            theme.resizable.handle.unwrap_or(theme.tokens.colors.border)
        };
        // Preserve the Base handle's 1px line, 4px padding and placement.
        let mut line = div()
            .absolute()
            .flex_shrink_0()
            .top_0()
            .left(px(-4.))
            .h_full()
            .w(px(1.))
            .px(px(4.))
            .child(div().flex_none().bg(color).h_full().w(px(1.)))
            .into_any_element();
        (line.request_layout(window, cx), line)
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        line: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        line.prepaint(window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::BlockMouse);
        // Transfer only our existing grab. A release or another control taking
        // capture must never be undone by a later repaint of this handle.
        if self.active
            && self.capture.get().is_some()
            && self.capture.get() == window.captured_hitbox()
        {
            window.capture_pointer(hitbox.id);
            self.capture.set(Some(hitbox.id));
        }
        hitbox
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        line: &mut AnyElement,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        line.paint(window, cx);
        window.set_cursor_style(gpui::CursorStyle::ResizeLeftRight, hitbox);
        let hitbox = hitbox.id;
        let begin = self.begin.clone();
        let capture = self.capture.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase.bubble() && event.button == MouseButton::Left && hitbox.is_hovered(window) {
                window.capture_pointer(hitbox);
                capture.set(Some(hitbox));
                window.prevent_default();
                gpui_base::GlobalState::suppress_text_selection(cx);
                begin(event, window, cx);
                cx.stop_propagation();
            }
        });
    }
}

impl Reader {
    pub(super) fn release_panel_pointer(&mut self, window: &mut Window) {
        if let Some(id) = self.panel_capture.take() {
            if window.captured_hitbox() == Some(id) {
                window.release_pointer();
            }
        }
    }
}
