//! Whole-UI zoom on top of OS DPI, independent of document text zoom (#2848).
use super::*;
use gpui::{canvas, DispatchPhase, ScrollDelta, ScrollWheelEvent};

gpui::actions!(ui_zoom, [ZoomIn, ZoomOut, Reset]);

pub(crate) use okilum_shell_kit::ui_zoom::{normalize, normalize_step, DEFAULT_STEP, STEPS};

/// The next whole multiple of `step` percent in `direction`, so 103% with a
/// 5% step goes to 105% / 100%, not 108% / 98%.
fn stepped(zoom: f32, step: u32, direction: f32) -> f32 {
    let step = normalize_step(step) as f32;
    let units = (zoom * 100.).round() / step;
    let next = if direction > 0. {
        (units + 1e-3).floor() + 1.
    } else {
        (units - 1e-3).ceil() - 1.
    };
    normalize(next * step / 100.)
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
        // Linux (xkb) reports Ctrl+Shift+= as `ctrl-+`: Shift is dropped for
        // symbol keys, so the shifted forms below never match there (#3229).
        KeyBinding::new("secondary-+", ZoomIn, None),
        KeyBinding::new("secondary-_", ZoomOut, None),
        KeyBinding::new("secondary-)", Reset, None),
        // The last binding per action is what menus show.
        KeyBinding::new("secondary-shift-+", ZoomIn, None),
        KeyBinding::new("secondary-shift-=", ZoomIn, None),
        KeyBinding::new("secondary-shift-_", ZoomOut, None),
        KeyBinding::new("secondary-shift--", ZoomOut, None),
        KeyBinding::new("secondary-shift-)", Reset, None),
        KeyBinding::new("secondary-shift-0", Reset, None),
    ]);
    cx.on_action(|_: &ZoomIn, cx| change(1., cx));
    cx.on_action(|_: &ZoomOut, cx| change(-1., cx));
    cx.on_action(|_: &Reset, cx| reader_ui_state::set_ui_zoom(1., cx));
    cx.observe_new::<Root>(|root, window, cx| {
        if let Some(window) = window {
            window.set_ui_scale(reader_ui_state::ui_zoom(cx));
            // #3244: record the restored scale, the first drawn frame (after
            // the window is registered, so native callbacks reach it) and every
            // later scale or display change.
            crate::ui_zoom_diagnostics::note("open", window, cx);
            window.on_next_frame(|window, cx| {
                crate::ui_zoom_diagnostics::note("first_frame", window, cx)
            });
            cx.observe_window_bounds(window, |_, window, cx| {
                crate::ui_zoom_diagnostics::note("bounds", window, cx)
            })
            .detach();
            root.add_overlay(cx.new(|_| ZoomLayer::default()));
            cx.notify();
        }
    })
    .detach();
}

fn change(direction: f32, cx: &mut App) {
    let zoom = stepped(
        reader_ui_state::ui_zoom(cx),
        reader_ui_state::ui_zoom_step(cx),
        direction,
    );
    reader_ui_state::set_ui_zoom(zoom, cx);
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
                            change(delta.signum(), cx);
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
        assert_eq!(normalize(1.0499), 1.05);
        assert_eq!(normalize(f32::NAN), 1.);
        assert_eq!(normalize(f32::INFINITY), 1.);
    }

    #[test]
    fn steps_follow_the_chosen_size_and_land_on_its_multiples() {
        assert_eq!(DEFAULT_STEP, 5, "default is finer than the reported 10%");
        assert_eq!(stepped(1., 5, 1.), 1.05);
        assert_eq!(stepped(1., 5, -1.), 0.95);
        assert_eq!(stepped(1., 2, 1.), 1.02);
        assert_eq!(stepped(1., 10, 1.), 1.1);
        assert_eq!(stepped(1.03, 5, 1.), 1.05);
        assert_eq!(stepped(1.03, 5, -1.), 1.);
        assert_eq!(stepped(1.1, 5, 1.), 1.15);
        assert_eq!(stepped(1.3, 10, -1.), 1.2);
        assert_eq!(stepped(2., 5, 1.), 2.);
        assert_eq!(stepped(0.5, 5, -1.), 0.5);
        // Unknown step values use the default.
        assert_eq!(normalize_step(7), DEFAULT_STEP);
        assert_eq!(stepped(1., 7, 1.), 1.05);
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
    /// Linux (xkb) drops Shift from symbol keystrokes: Ctrl+Shift+= arrives as
    /// `ctrl-+`, Ctrl+Shift+- as `ctrl-_`, Ctrl+Shift+0 as `ctrl-)` (#3229).
    #[gpui::test]
    fn shortcuts_match_shift_stripped_linux_keystrokes(cx: &mut TestAppContext) {
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
        // Default 5% step first, then the chosen 10% step (#3228).
        for (step, keys, expected) in [
            (None, "secondary-+", 1.05),
            (None, "secondary-+", 1.1),
            (None, "secondary-_", 1.05),
            (None, "secondary-)", 1.),
            (None, "secondary-shift-=", 1.05),
            (None, "secondary-shift--", 1.),
            (Some(10), "secondary-+", 1.1),
            (None, "secondary-_", 1.),
        ] {
            if let Some(step) = step {
                visual.update(|_, cx| reader_ui_state::set_ui_zoom_step(step, cx));
            }
            visual.simulate_keystrokes(keys);
            visual.run_until_parked();
            visual.update(|window, cx| {
                assert_eq!(reader_ui_state::ui_zoom(cx), expected, "{keys}");
                assert_eq!(window.ui_scale(), expected, "{keys}");
                assert_eq!(reader_ui_state::text_zoom(cx), 1.4, "{keys}");
            });
        }
        // Menus show the shifted plus/minus/0 forms.
        visual.update(|_, cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let shown = |action: &dyn Action| {
                keymap
                    .bindings()
                    .rfind(|binding| binding.action().partial_eq(action))
                    .map(|binding| binding.keystrokes()[0].unparse())
            };
            let secondary = if cfg!(target_os = "macos") {
                "cmd"
            } else {
                "ctrl"
            };
            assert_eq!(shown(&ZoomIn), Some(format!("{secondary}-shift-=")));
            assert_eq!(shown(&ZoomOut), Some(format!("{secondary}-shift--")));
            assert_eq!(shown(&Reset), Some(format!("{secondary}-shift-0")));
        });
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
            assert_eq!(reader_ui_state::ui_zoom(cx), 1.05, "default 5% step");
            assert_eq!(window.ui_scale(), 1.05);
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
            assert_eq!(window.ui_scale(), 1.55, "one wheel notch is one 5% step");
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            reader_ui_state::set_ui_zoom_step(10, cx);
        });
        visual.simulate_event(ScrollWheelEvent {
            position: point(px(100.), px(100.)),
            delta: ScrollDelta::Lines(point(0., -1.)),
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
            assert_eq!(window.ui_scale(), 1.5, "the chosen 10% step applies");
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
            assert_eq!(reader_ui_state::ui_zoom_step(cx), 10, "step persists");
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            reader_ui_state::set_ui_zoom(1., cx);
        });
        second.run_until_parked();
        first_window
            .update(second, |_, window, _| assert_eq!(window.ui_scale(), 1.))
            .unwrap();
    }
}
