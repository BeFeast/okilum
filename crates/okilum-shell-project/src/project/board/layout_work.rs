//! Count actual row layout requests independently of row construction. The
//! production uniform lists own virtualization; this probe adds no release work.
use super::*;

pub(super) struct Row {
    pub child: AnyElement,
    pub board: WeakEntity<Board>,
}
impl IntoElement for Row {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl gpui::Element for Row {
    type RequestLayoutState = ();
    type PrepaintState = ();
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
        if let Some(board) = self.board.upgrade() {
            *board
                .read(cx)
                .frame_layout_work
                .borrow_mut()
                .last_mut()
                .expect("Board render frame") += 1;
        }
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }
    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}
