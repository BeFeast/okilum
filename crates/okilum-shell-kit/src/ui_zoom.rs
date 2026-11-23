//! Whole-UI zoom values kept in the UI state store (#2848, #3228). The zoom
//! actions and their window handling stay in the app's `ui_zoom`.
use gpui::{App, Global, Window};

/// Step choices in percent offered in Settings → Appearance (#3228).
pub const STEPS: [u32; 3] = [2, 5, 10];
/// 10% per wheel notch was reported as too coarse (#3228).
pub const DEFAULT_STEP: u32 = 5;

pub fn normalize(value: f32) -> f32 {
    if value.is_finite() {
        (value.clamp(0.5, 2.) * 100.).round() / 100.
    } else {
        1.
    }
}

pub fn normalize_step(step: u32) -> u32 {
    if STEPS.contains(&step) {
        step
    } else {
        DEFAULT_STEP
    }
}

/// Called after a window's UI scale changed; the app logs it for the
/// restored-zoom diagnostics (#3244). The kit has no diagnostics of its own.
struct ScaleObserver(fn(&'static str, &Window, &mut App));
impl Global for ScaleObserver {}

/// Install the app's observer of UI scale changes.
pub fn observe_scale(observer: fn(&'static str, &Window, &mut App), cx: &mut App) {
    cx.set_global(ScaleObserver(observer));
}

/// Report a scale change of `window` to the installed observer, if any.
pub fn scale_changed(reason: &'static str, window: &Window, cx: &mut App) {
    if let Some(observer) = cx.try_global::<ScaleObserver>().map(|o| o.0) {
        observer(reason, window, cx);
    }
}
