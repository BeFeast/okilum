#[cfg(not(target_os = "macos"))]
pub mod exact_wayland_clipboard;

pub mod labels;
pub mod reveal;

use gpui::Global;
use gpui_component::input::clipboard::ExactClipboardProvider;
use std::sync::Arc;

pub struct ManagedClipboard(pub Arc<dyn ExactClipboardProvider>);
impl Global for ManagedClipboard {}

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
mod clipboard_process;
#[cfg(target_os = "macos")]
pub mod exact_macos_clipboard;

pub fn managed_clipboard() -> Arc<dyn ExactClipboardProvider> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(exact_macos_clipboard::MacClipboard)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(exact_wayland_clipboard::WaylandClipboard::new(
            exact_wayland_clipboard::existing_compositor(),
            std::env::var("TESSERA_CLIPBOARD_SEAT").ok(),
        ))
    }
}
