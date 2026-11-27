//! Overscan still measures and prepaints complete source rows. Mouse scrolling
//! need not paint clipped glyphs/controls; keyboard and selection keep full paint.
use super::*;

pub(super) struct RowPaint(AnyElement);
impl RowPaint {
    pub fn new(row: AnyElement) -> Self {
        Self(row)
    }
}
impl IntoElement for RowPaint {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl gpui::Element for RowPaint {
    type RequestLayoutState = ();
    type PrepaintState = bool;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, ()) {
        (self.0.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let focused = self.0.prepaint(window, cx).is_some();
        bounds.intersects(&window.content_mask().bounds)
            || focused
            || window.last_input_was_keyboard()
            || gpui_base::TextSelection::has_selection(window, cx)
    }
    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        visible: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *visible {
            self.0.paint(window, cx);
        }
    }
}
