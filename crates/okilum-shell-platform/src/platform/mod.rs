#[cfg(target_os = "linux")]
pub mod exact_wayland_clipboard;
#[cfg(any(windows, test, feature = "test-support"))]
pub mod exact_windows_clipboard;
#[cfg(target_os = "linux")]
pub mod exact_x11_clipboard;
#[cfg(target_os = "linux")]
pub mod wayland_note_clipboard;
#[cfg(any(windows, test, feature = "test-support"))]
pub mod windows_note_clipboard;

pub mod labels;
pub mod rich_clipboard;

/// Right-to-left glyph order for the Windows text system (#1121).
#[cfg(any(windows, test, feature = "test-support"))]
pub mod bidi_layout;
#[cfg(target_os = "linux")]
pub mod linux_emoji;
pub mod reveal;
/// The shaped-line cache in front of the platform shaper (#1312).
pub mod shaped_line_cache;
/// GPUI's platform with Okilum's text system (#1121, #1312).
pub mod text_platform;
#[cfg(windows)]
pub mod windows_bidi;

use gpui::Global;
use gpui_component::input::clipboard::ExactClipboardProvider;
use std::sync::Arc;

pub struct ManagedClipboard(pub Arc<dyn ExactClipboardProvider>);
impl Global for ManagedClipboard {}

#[cfg(any(
    target_os = "macos",
    windows,
    all(any(test, feature = "test-support"), target_os = "linux")
))]
mod clipboard_process;
#[cfg(target_os = "macos")]
pub mod exact_macos_clipboard;

pub fn managed_clipboard() -> Arc<dyn ExactClipboardProvider> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(exact_macos_clipboard::MacClipboard)
    }
    #[cfg(windows)]
    {
        Arc::new(exact_windows_clipboard::WindowsClipboard)
    }
    #[cfg(target_os = "linux")]
    {
        Arc::new(exact_wayland_clipboard::WaylandClipboard::new(
            exact_wayland_clipboard::existing_compositor(),
            std::env::var("OKILUM_CLIPBOARD_SEAT").ok(),
        ))
    }
}

#[cfg(all(test, windows))]
mod windows_clipboard_tests;

pub mod text_diagnostics;
