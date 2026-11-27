//! Shared `okilum::*` GPUI actions and key constants used across shell crates
//! (#3237, epic #3241). Action names remain `okilum::*` so user keymaps and
//! the action inventory golden list are unchanged.

gpui::actions!(okilum, [Dismiss, DismissManagerModel]);

/// Open in terminal (#2837). Not Win+Alt+T on Windows: the Xbox Game Bar
/// shows or hides its recording timer there and the key never reaches Okilum
/// (#3134). Not Ctrl+Shift+U, which is Reveal (#3133), nor Ctrl+Shift+T,
/// which is Paragraph. A is the last Ctrl+Shift letter free in every Okilum
/// context (gpui-component binds it only on macOS; B is the Pinyin emoji
/// panel and I the debug inspector).
pub const OPEN_IN_TERMINAL_KEY_WINDOWS: &str = "ctrl-shift-a";
pub const OPEN_IN_TERMINAL_KEY: &str = if cfg!(windows) {
    OPEN_IN_TERMINAL_KEY_WINDOWS
} else {
    "alt-cmd-t"
};
