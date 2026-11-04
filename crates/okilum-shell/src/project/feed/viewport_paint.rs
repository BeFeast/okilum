//! Overscan still measures complete source rows. Mouse scrolling/resize need
//! not prepaint clipped controls; history focus, keyboard and selection retain
//! the complete interaction tree.
use super::*;

pub(super) struct RowPaint {
    child: AnyElement,
    viewport_focus: FocusHandle,
}
impl RowPaint {
    pub fn new(child: AnyElement, viewport_focus: FocusHandle) -> Self {
        Self {
            child,
            viewport_focus,
        }
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
        (self.child.request_layout(window, cx), ())
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
        let visible = bounds.intersects(&window.content_mask().bounds)
            || self.viewport_focus.contains_focused(window, cx)
            || window.last_input_was_keyboard()
            || gpui_base::TextSelection::has_selection(window, cx);
        if visible {
            self.child.prepaint(window, cx);
        }
        visible
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
            self.child.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::{cell::Cell, rc::Rc};

    struct Probe {
        top: Pixels,
        viewport_focus: FocusHandle,
        child_focus: FocusHandle,
        prepaints: Rc<Cell<usize>>,
    }
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let prepaints = self.prepaints.clone();
            let child = div()
                .track_focus(&self.child_focus)
                .w(px(80.))
                .h(px(30.))
                .on_prepaint(move |_, _, _| prepaints.set(prepaints.get() + 1))
                .child("Measured row")
                .into_any_element();
            div()
                .track_focus(&self.viewport_focus)
                .relative()
                .w(px(100.))
                .h(px(100.))
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .top(self.top)
                        .child(RowPaint::new(child, self.viewport_focus.clone())),
                )
        }
    }

    #[gpui::test]
    fn clipped_mouse_rows_skip_prepaint_but_history_focus_and_keyboard_keep_it(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let prepaints = Rc::new(Cell::new(0));
        let (view, visual) = cx.add_window_view(|_, cx| Probe {
            top: px(0.),
            viewport_focus: cx.focus_handle(),
            child_focus: cx.focus_handle(),
            prepaints: prepaints.clone(),
        });
        visual.simulate_mouse_move(point(px(200.), px(200.)), None, Modifiers::default());
        visual.run_until_parked();
        assert!(
            prepaints.get() > 0,
            "positive control: visible row prepaints"
        );
        prepaints.set(0);
        view.update(visual, |view, cx| {
            view.top = px(1000.);
            cx.notify();
        });
        visual.run_until_parked();
        assert_eq!(
            prepaints.get(),
            0,
            "clipped mouse overscan has no interaction work"
        );

        // Establish the real descendant focus path while visible, then clip it.
        view.update_in(visual, |view, window, cx| {
            view.top = px(0.);
            view.child_focus.focus(window, cx);
            cx.notify();
        });
        visual.run_until_parked();
        prepaints.set(0);
        view.update_in(visual, |view, window, cx| {
            assert!(view.viewport_focus.contains_focused(window, cx));
            view.top = px(1000.);
            cx.notify();
        });
        visual.run_until_parked();
        assert!(
            prepaints.get() > 0,
            "focused offscreen controls retain their tree"
        );
        view.update_in(visual, |view, window, cx| {
            assert!(view.child_focus.is_focused(window));
            window.blur(cx);
            cx.notify();
        });
        visual.run_until_parked();
        prepaints.set(0);
        visual.simulate_keystrokes("tab");
        view.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        assert!(
            prepaints.get() > 0,
            "keyboard traversal retains offscreen controls"
        );
    }
}
