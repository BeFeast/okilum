//! The app's Light/Dark/System choice and where it is saved. Applying it to
//! windows stays in the app.
use std::path::PathBuf;

/// Application-only preference. Never changes the desktop's appearance setting.
#[derive(Default, Clone, Copy)]
pub struct AppearancePreference(pub Option<gpui_component::ThemeMode>);
impl gpui::Global for AppearancePreference {}

pub fn appearance_settings_path() -> Option<PathBuf> {
    crate::reader_layout::config_base().map(|base| base.join("okilum/appearance.json"))
}
