//! Whole-UI zoom on top of OS DPI, independent of document text zoom (#2848).
use super::*;
use gpui::{canvas, DispatchPhase, ScrollDelta, ScrollWheelEvent};

gpui::actions!(ui_zoom, [ZoomIn, ZoomOut, Reset]);

pub(crate) fn normalize(value: f32) -> f32 {
    if value.is_finite() {
        (value.clamp(0.5, 2.) * 10.).round() / 10.
    } else {
        1.
    }
}

fn modifier(m: Modifiers, mac: bool) -> bool {
    m.shift
        && !m.alt
        && if mac {
            m.platform && !m.control
        } else {
            m.control && !m.platform
        }
}

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-shift-+", ZoomIn, None),
        KeyBinding::new("secondary-shift-=", ZoomIn, None),
        KeyBinding::new("secondary-shift--", ZoomOut, None),
        KeyBinding::new("secondary-shift-_", ZoomOut, None),
        KeyBinding::new("secondary-shift-0", Reset, None),
        KeyBinding::new("secondary-shift-)", Reset, None),
    ]);
    cx.on_action(|_: &ZoomIn, cx| change(0.1, cx));
    cx.on_action(|_: &ZoomOut, cx| change(-0.1, cx));
    cx.on_action(|_: &Reset, cx| reader_ui_state::set_ui_zoom(1., cx));
    cx.observe_new::<Root>(|root, window, cx| {
        if let Some(window) = window {
            window.set_ui_scale(reader_ui_state::ui_zoom(cx));
            root.add_overlay(cx.new(|_| ZoomLayer::default()));
            cx.notify();
        }
    })
    .detach();
}

fn change(delta: f32, cx: &mut App) {
    reader_ui_state::set_ui_zoom(reader_ui_state::ui_zoom(cx) + delta, cx);
}

/// A Root overlay covers all application windows and captures the wheel even
/// over menus, dialogs and editors. It paints no hitbox or bitmap overlay.
#[derive(Default)]
struct ZoomLayer {
    wheel: Rc<std::cell::Cell<f32>>,
}
impl Render for ZoomLayer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pending = self.wheel.clone();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    if !modifier(event.modifiers, cfg!(target_os = "macos")) {
                        pending.set(0.);
                        return;
                    }
                    // Shift-wheel can be reported as horizontal by the platform.
                    let delta = match event.delta {
                        ScrollDelta::Lines(p) => {
                            if p.y != 0. {
                                p.y
                            } else {
                                p.x
                            }
                        }
                        ScrollDelta::Pixels(p) => f32::from(if p.y != px(0.) { p.y } else { p.x }),
                    };
                    if delta.is_finite() && delta != 0. {
                        let precise = matches!(event.delta, ScrollDelta::Pixels(_));
                        let delta = if precise {
                            delta * window.ui_scale()
                        } else {
                            delta
                        };
                        let old = pending.get();
                        let accumulated = if old.signum() == delta.signum() {
                            old + delta
                        } else {
                            delta
                        };
                        if !precise || accumulated.abs() >= 40. {
                            change(delta.signum() * 0.1, cx);
                            pending.set(0.);
                        } else {
                            pending.set(accumulated);
                        }
                    }
                    cx.stop_propagation();
                });
            },
        )
        .size_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct Content;
    impl Render for Content {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full()
        }
    }

    #[test]
    fn bounded_discrete_scale_and_invalid_saved_values() {
        assert_eq!(normalize(0.1), 0.5);
        assert_eq!(normalize(3.), 2.);
        assert_eq!(normalize(1.299999), 1.3);
        assert_eq!(normalize(f32::NAN), 1.);
        assert_eq!(normalize(f32::INFINITY), 1.);
    }

    #[test]
    fn ui_modifier_does_not_take_document_zoom_or_plain_scroll() {
        for mac in [false, true] {
            let primary = Modifiers {
                platform: mac,
                control: !mac,
                ..Default::default()
            };
            assert!(!modifier(primary, mac));
            assert!(modifier(
                Modifiers {
                    shift: true,
                    ..primary
                },
                mac
            ));
            assert!(!modifier(
                Modifiers {
                    shift: true,
                    alt: true,
                    ..primary
                },
                mac
            ));
            assert!(!modifier(
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                mac
            ));
        }
    }
    #[gpui::test]
    fn persists_app_scale_independently_and_syncs_existing_and_new_windows(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_ui_state::install(temp.path(), cx);
            init(cx);
            reader_ui_state::set_text_zoom(1.4, cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let content = cx.new(|_| Content);
            Root::new(content, window, cx)
        });
        visual.run_until_parked();
        let first_window = visual.update(|window, _| window.window_handle());
        visual.simulate_keystrokes("secondary-shift-=");
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert_eq!(reader_ui_state::ui_zoom(cx), 1.1);
            assert_eq!(window.ui_scale(), 1.1);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            reader_ui_state::set_ui_zoom(1.5, cx);
        });
        visual.run_until_parked();
        visual.simulate_event(ScrollWheelEvent {
            position: point(px(100.), px(100.)),
            delta: ScrollDelta::Lines(point(0., 1.)),
            modifiers: Modifiers {
                shift: true,
                platform: cfg!(target_os = "macos"),
                control: !cfg!(target_os = "macos"),
                ..Default::default()
            },
            ..Default::default()
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert_eq!(window.ui_scale(), 1.6);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            reader_ui_state::set_ui_zoom(1.5, cx);
        });
        visual.run_until_parked();
        let (_, second) = cx.add_window_view(|window, cx| {
            let content = cx.new(|_| Content);
            Root::new(content, window, cx)
        });
        second.run_until_parked();
        second.update(|window, cx| {
            assert_eq!(window.ui_scale(), 1.5);
            reader_ui_state::flush(cx);
            reader_ui_state::install(temp.path(), cx);
            assert_eq!(reader_ui_state::ui_zoom(cx), 1.5);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            reader_ui_state::set_ui_zoom(1., cx);
        });
        second.run_until_parked();
        first_window
            .update(second, |_, window, _| assert_eq!(window.ui_scale(), 1.))
            .unwrap();
    }
}
