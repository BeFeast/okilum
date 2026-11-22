//! Whole-interface zoom (#2848). One scale for every window, on top of the
//! display scale and independent of note text zoom.
//!
//! Cmd/Ctrl+Shift+wheel, Cmd/Ctrl+Shift+plus/minus/0, and the View menu.
//! Ten percent steps, 50% to 200%. Note text zoom stays on Cmd/Ctrl+wheel
//! and Cmd/Ctrl+plus/minus/0.
use super::*;
use gpui::{DispatchPhase, Pixels, ScrollDelta};

gpui::actions!(interface_zoom, [ZoomIn, ZoomOut, Reset]);

const MIN: f32 = 0.5;
const MAX: f32 = 2.;
/// Pixel wheels accumulate until they match one 10% step. The same distance
/// note text zoom treats as a full tenth.
const PIXELS_PER_STEP: f32 = 400.;

std::thread_local! {
    static WHEEL_REST: std::cell::Cell<f32> = const { std::cell::Cell::new(0.) };
}

pub(crate) fn install(cx: &mut App) {
    cx.on_action(|_: &ZoomIn, cx| change(1., cx));
    cx.on_action(|_: &ZoomOut, cx| change(-1., cx));
    cx.on_action(|_: &Reset, cx| change_to(1., cx));
    // Menus show the last binding. `=` with shift is plus on US layouts.
    cx.bind_keys([
        KeyBinding::new("secondary-shift-+", ZoomIn, None),
        KeyBinding::new("secondary-shift-=", ZoomIn, None),
        KeyBinding::new("secondary-shift--", ZoomOut, None),
        KeyBinding::new("secondary-shift-0", Reset, None),
    ]);
}

/// Capture Cmd/Ctrl+Shift+wheel for the whole window, including chrome.
pub(crate) fn watch(window: &mut Window) {
    window.on_mouse_event(|event: &ScrollWheelEvent, phase, _, cx| {
        if phase != DispatchPhase::Capture || !chord(event.modifiers) {
            return;
        }
        let steps = wheel_steps(event.delta);
        if steps != 0. {
            change(steps, cx);
        }
        cx.stop_propagation();
    });
}

/// Keep a popup inside the zoomed window. A menu taller than the viewport
/// scrolls instead of clipping its lower actions.
pub(crate) fn menu_limit(window: &Window) -> Pixels {
    (window.viewport_size().height - px(48.)).max(px(160.))
}

fn chord(modifiers: Modifiers) -> bool {
    modifiers.shift
        && !modifiers.alt
        && if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        }
}

fn wheel_steps(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Lines(lines) => {
            if lines.y.is_finite() && lines.y != 0. {
                lines.y.signum()
            } else {
                0.
            }
        }
        ScrollDelta::Pixels(pixels) => {
            let y = f32::from(pixels.y);
            if !y.is_finite() || y == 0. {
                return 0.;
            }
            WHEEL_REST.with(|rest| {
                let total = rest.get() + y / PIXELS_PER_STEP;
                let steps = total.trunc();
                rest.set(total - steps);
                steps
            })
        }
    }
}

fn change(steps: f32, cx: &mut App) {
    if steps == 0. {
        return;
    }
    let snapped = (reader_ui_state::ui_scale(cx) * 10.).round() / 10.;
    change_to((snapped + steps * 0.1).clamp(MIN, MAX), cx);
}

fn change_to(scale: f32, cx: &mut App) {
    if !reader_ui_state::set_ui_scale(scale, cx) {
        return;
    }
    #[cfg(all(unix, feature = "brain"))]
    brain::app_quit::set_menus(cx);
    #[cfg(not(all(unix, feature = "brain")))]
    reader_app_menu::set_menus(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use super::*` pulls in GPUI's `test` attribute, which expands back to
    // `#[test]`. Name the standard attribute so this module can compile.
    use ::core::prelude::v1::test;

    struct RestoreScale;
    impl Drop for RestoreScale {
        fn drop(&mut self) {
            reader_layout::note_ui_scale(1.);
        }
    }

    #[test]
    fn shift_wheel_steps_ten_percent_and_a_wide_window_stays_docked() {
        let _restore = RestoreScale;
        assert_eq!(
            chord(Modifiers {
                shift: true,
                control: true,
                ..Default::default()
            }),
            !cfg!(target_os = "macos")
        );
        assert_eq!(
            chord(Modifiers {
                shift: true,
                platform: true,
                ..Default::default()
            }),
            cfg!(target_os = "macos")
        );
        assert!(!chord(Modifiers {
            control: true,
            ..Default::default()
        }));
        assert_eq!(wheel_steps(ScrollDelta::Lines(point(0., 3.))), 1.);
        assert_eq!(wheel_steps(ScrollDelta::Lines(point(0., 0.))), 0.);
        assert_eq!(
            wheel_steps(ScrollDelta::Pixels(point(px(0.), px(PIXELS_PER_STEP)))),
            1.
        );
        reader_layout::note_ui_scale(1.5);
        assert!(
            !reader_layout::overlay(960.),
            "1440 OS-logical pixels still docks sidebars"
        );
        assert!(
            reader_layout::overlay(600.),
            "900 OS-logical pixels is compact"
        );
        reader_layout::note_ui_scale(0.8);
        assert!(
            reader_layout::overlay(1000.),
            "an 800-wide window stays compact when zoomed out"
        );
    }

    #[gpui::test]
    fn interface_zoom_persists_without_changing_note_text_zoom(cx: &mut gpui::TestAppContext) {
        let _restore = RestoreScale;
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            reader_ui_state::install(temp.path(), cx);
            reader_ui_state::set_text_zoom(1.3, cx);
            assert!(reader_ui_state::set_ui_scale(1.5, cx));
            assert_eq!(reader_ui_state::ui_scale(cx), 1.5);
            assert_eq!(cx.ui_scale(), 1.5);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.3);
            assert!(reader_ui_state::set_ui_scale(0.4, cx));
            assert_eq!(reader_ui_state::ui_scale(cx), 0.5);
            assert_eq!(cx.ui_scale(), 0.5);
            reader_ui_state::set_ui_scale(1.5, cx);
            reader_ui_state::flush(cx);
        });
        cx.update(|cx| {
            reader_ui_state::install(temp.path(), cx);
            assert_eq!(reader_ui_state::ui_scale(cx), 1.5);
            assert_eq!(cx.ui_scale(), 1.5);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.3);
            reader_ui_state::set_ui_scale(1., cx);
        });
    }
}
