//! Whole-UI zoom values kept in the UI state store (#2848, #3228). The zoom
//! actions and their window handling stay in the app's `ui_zoom`.

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
