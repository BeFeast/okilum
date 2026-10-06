//! Platform wording for menus, tooltips and toasts (#612).
//!
//! Every user-visible string that names a desktop component or a keyboard
//! shortcut goes through [`Os`], so macOS terms never leak to Linux or
//! Windows. The functions take an explicit [`Os`] so all three platforms are
//! testable on any host; call sites use [`Os::CURRENT`].

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

/// `label` followed by the current platform's form of `keystroke`, e.g.
/// "New File ⌘N" on macOS and "New File Ctrl+N" elsewhere.
///
/// Interned because tooltips take `&'static str` and are rebuilt every frame;
/// call sites pass literals, so the set of strings is fixed.
pub fn with_shortcut(label: &'static str, keystroke: &'static str) -> &'static str {
    static CACHE: Mutex<BTreeMap<(&str, &str), &str>> = Mutex::new(BTreeMap::new());
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.entry((label, keystroke)).or_insert_with(|| {
        let text = format!("{label} {}", Os::CURRENT.shortcut(keystroke));
        Box::leak(text.into_boxed_str())
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub const CURRENT: Os = if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else {
        Os::Linux
    };

    /// Action that shows a file selected in the system file manager.
    pub const fn reveal(self) -> &'static str {
        match self {
            Os::Mac => "Reveal in Finder",
            Os::Windows => "Show in Explorer",
            Os::Linux => "Show in File Manager",
        }
    }

    /// Display form of a GPUI keystroke such as `secondary-shift-f`.
    ///
    /// `secondary` is ⌘ on macOS and Ctrl elsewhere, matching GPUI's key
    /// binding semantics. macOS uses glyphs in the canonical ⌃⌥⇧⌘ order;
    /// Linux and Windows spell the modifiers out, joined by `+`.
    pub fn shortcut(self, keystroke: &str) -> String {
        let mut parts: Vec<&str> = keystroke.split('-').collect();
        // `secondary--` binds the minus key itself.
        let key = match parts.pop() {
            Some("") if keystroke.ends_with("--") => {
                parts.pop();
                "-"
            }
            key => key.unwrap_or_default(),
        };
        let has = |name: &str| parts.contains(&name);
        let command = has("cmd") || (has("secondary") && self == Os::Mac);
        let control = has("ctrl") || (has("secondary") && self != Os::Mac);
        let modifiers = [
            (control, "⌃", "Ctrl"),
            (has("alt"), "⌥", "Alt"),
            (has("shift"), "⇧", "Shift"),
            (command, "⌘", "Win"),
        ];
        let key = self.key_name(key);
        if self == Os::Mac {
            let mut text: String = modifiers
                .iter()
                .filter(|(on, ..)| *on)
                .map(|(_, glyph, _)| *glyph)
                .collect();
            text.push_str(&key);
            text
        } else {
            let mut words: Vec<&str> = modifiers
                .iter()
                .filter(|(on, ..)| *on)
                .map(|(.., word)| *word)
                .collect();
            words.push(&key);
            words.join("+")
        }
    }

    fn key_name(self, key: &str) -> String {
        let mac = self == Os::Mac;
        match key {
            "left" => "←".into(),
            "right" => "→".into(),
            "up" => "↑".into(),
            "down" => "↓".into(),
            "enter" if mac => "⏎".into(),
            "enter" => "Enter".into(),
            "backspace" if mac => "⌫".into(),
            "backspace" => "Backspace".into(),
            "space" => "Space".into(),
            key => key.to_uppercase(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{with_shortcut, Os};

    #[test]
    fn current_matches_target_os() {
        #[cfg(target_os = "macos")]
        assert_eq!(Os::CURRENT, Os::Mac);
        #[cfg(target_os = "windows")]
        assert_eq!(Os::CURRENT, Os::Windows);
        #[cfg(target_os = "linux")]
        assert_eq!(Os::CURRENT, Os::Linux);
    }

    #[test]
    fn with_shortcut_is_interned_for_the_current_platform() {
        let text = with_shortcut("New File", "secondary-n");
        assert_eq!(
            text,
            format!("New File {}", Os::CURRENT.shortcut("secondary-n"))
        );
        assert!(std::ptr::eq(text, with_shortcut("New File", "secondary-n")));
        #[cfg(target_os = "macos")]
        assert_eq!(text, "New File ⌘N");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(text, "New File Ctrl+N");
    }

    #[test]
    fn reveal_names_the_platform_file_manager() {
        assert_eq!(Os::Mac.reveal(), "Reveal in Finder");
        assert_eq!(Os::Windows.reveal(), "Show in Explorer");
        assert_eq!(Os::Linux.reveal(), "Show in File Manager");
        #[cfg(not(target_os = "macos"))]
        assert!(!Os::CURRENT.reveal().contains("Finder"));
    }

    #[test]
    fn macos_shortcuts_use_ordered_glyphs() {
        assert_eq!(Os::Mac.shortcut("secondary-n"), "⌘N");
        assert_eq!(Os::Mac.shortcut("secondary-shift-f"), "⇧⌘F");
        assert_eq!(Os::Mac.shortcut("shift-secondary-f"), "⇧⌘F");
        assert_eq!(Os::Mac.shortcut("alt-left"), "⌥←");
        assert_eq!(Os::Mac.shortcut("shift-enter"), "⇧⏎");
        assert_eq!(Os::Mac.shortcut("ctrl-alt-secondary-x"), "⌃⌥⌘X");
        assert_eq!(Os::Mac.shortcut("secondary--"), "⌘-");
    }

    #[test]
    fn linux_and_windows_shortcuts_spell_out_ctrl() {
        for os in [Os::Linux, Os::Windows] {
            assert_eq!(os.shortcut("secondary-n"), "Ctrl+N");
            assert_eq!(os.shortcut("secondary-s"), "Ctrl+S");
            assert_eq!(os.shortcut("secondary-shift-f"), "Ctrl+Shift+F");
            assert_eq!(os.shortcut("alt-left"), "Alt+←");
            assert_eq!(os.shortcut("alt-right"), "Alt+→");
            assert_eq!(os.shortcut("shift-enter"), "Shift+Enter");
            assert_eq!(os.shortcut("secondary-backspace"), "Ctrl+Backspace");
            assert_eq!(os.shortcut("secondary--"), "Ctrl+-");
            for glyph in ["⌘", "⌥", "⇧", "⌃", "⏎"] {
                assert!(!os
                    .shortcut("ctrl-alt-shift-secondary-enter")
                    .contains(glyph));
            }
        }
    }
}
