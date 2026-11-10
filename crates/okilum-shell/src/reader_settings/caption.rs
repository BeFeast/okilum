//! Settings-only caption. AppKit receives the original press synchronously;
//! macOS 27.2 uses a scoped native-event workaround for its drag handoff.
use super::*;

#[cfg(any(target_os = "macos", test))]
mod manual;

#[derive(Default)]
pub(super) struct Caption {
    #[cfg(target_os = "macos")]
    native: Option<manual::NativeDrag>,
    #[cfg(target_os = "macos")]
    native_initialized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeAction {
    Move,
    DoubleClick,
}

fn native_action(action: NativeAction, window: &mut Window, cx: &mut App) {
    gpui_base::TextSelection::end(window, cx);
    cx.stop_active_drag(window);
    #[cfg(test)]
    if raw_window_handle::HasWindowHandle::window_handle(window).is_err() {
        // GPUI TestPlatform has no AppKit window and start_window_move panics.
        // Record only the actual dispatch point, never simulated window motion.
        REQUESTS.with(|requests| requests.borrow_mut().push(action));
        return;
    }
    match action {
        NativeAction::Move => window.start_window_move(),
        NativeAction::DoubleClick => window.titlebar_double_click(),
    }
}

impl Caption {
    pub(super) fn render(&mut self, close: Button, window: &mut Window, _: &mut App) -> Div {
        #[cfg(not(target_os = "macos"))]
        let _ = window;
        #[cfg(target_os = "macos")]
        let native = {
            if !self.native_initialized {
                self.native_initialized = true;
                self.native = manual::NativeDrag::install(window);
            }
            self.native.clone()
        };
        h_flex()
            .debug_selector(|| "settings-window-actions".into())
            .flex_none()
            .h(gpui_component::TITLE_BAR_HEIGHT)
            .items_center()
            // Match the standard native traffic-light inset. They retain their
            // AppKit hit targets; the client caption starts to their right.
            .pl(px(80.))
            .pr_3()
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                let action = if event.click_count == 2 {
                    NativeAction::DoubleClick
                } else {
                    NativeAction::Move
                };
                #[cfg(target_os = "macos")]
                if action == NativeAction::Move && native.as_ref().is_some_and(|drag| drag.begin())
                {
                    gpui_base::TextSelection::end(window, cx);
                    cx.stop_active_drag(window);
                    cx.stop_propagation();
                    return;
                }
                // This callback runs inside GPUI's synchronous native input
                // dispatch, so NSApplication.currentEvent is still this press.
                // Inactive first presses are accepted, without a focus-only click.
                native_action(action, window, cx);
                cx.stop_propagation();
            })
            .child(div().flex_1().child("Settings"))
            .child(
                div()
                    .debug_selector(|| "settings-caption-close-hit".into())
                    // Stop only the button's exact area. Caption padding on either
                    // side still bubbles to the original-press drag handler.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(close),
            )
    }
}

#[cfg(test)]
thread_local! {
    static REQUESTS: std::cell::RefCell<Vec<NativeAction>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(super) fn take_requests() -> Vec<NativeAction> {
    REQUESTS.with(|requests| std::mem::take(&mut *requests.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::{cell::Cell, rc::Rc};

    struct Surface(Rc<Cell<usize>>, Caption);
    impl Render for Surface {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let closed = self.0.clone();
            self.1
                .render(
                    Button::new("fixture-close")
                        .debug_selector(|| "fixture-caption-close".into())
                        .small()
                        .ghost()
                        .label("Close")
                        .on_click(move |_, _, _| closed.set(closed.get() + 1)),
                    window,
                    cx,
                )
                .w(px(780.))
        }
    }

    fn requests() -> Vec<NativeAction> {
        take_requests()
    }

    #[gpui::test]
    fn issue2770_caption_requests_native_move_on_original_press_including_first_mouse(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let closed = Rc::new(Cell::new(0));
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Surface(closed.clone(), Caption::default()));
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let row = visual.debug_bounds("settings-window-actions").unwrap();
        let close = visual.debug_bounds("fixture-caption-close").unwrap();
        assert_eq!(
            visual.debug_bounds("settings-caption-close-hit"),
            Some(close)
        );
        requests();
        // Several blank hit regions, including the right padding that the old
        // full-height Close wrapper swallowed. No move event precedes assertions.
        for (position, first_mouse) in [
            (row.origin + point(px(100.), px(12.)), false),
            (row.center(), false),
            (point(row.right() - px(3.), row.center().y), false),
            (point(close.left() - px(3.), row.center().y), true),
        ] {
            visual.simulate_event(MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                first_mouse,
                modifiers: Modifiers::default(),
            });
            assert_eq!(requests(), [NativeAction::Move]);
            visual.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
        }
        assert_eq!(closed.get(), 0);
        // Positive control: a real button click works without requesting a move.
        visual.simulate_click(close.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(requests().is_empty());
        assert_eq!(closed.get(), 1);
    }

    #[gpui::test]
    fn issue2770_caption_double_press_delegates_system_preference_once(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let closed = Rc::new(Cell::new(0));
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Surface(closed, Caption::default()));
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let row = visual.debug_bounds("settings-window-actions").unwrap();
        requests();
        visual.simulate_event(MouseDownEvent {
            position: row.center(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
            modifiers: Modifiers::default(),
        });
        assert_eq!(requests(), [NativeAction::DoubleClick]);
        visual.simulate_mouse_up(row.center(), MouseButton::Left, Modifiers::default());
        assert!(requests().is_empty(), "release must not dispatch twice");
    }
}
