//! Test helpers for kit tests and for crates above it (feature `test-support`).
use gpui::App;

/// The toolkit globals every kit view test needs. The app's own `test_init`
/// calls this first and adds the Reader-level globals.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
}
