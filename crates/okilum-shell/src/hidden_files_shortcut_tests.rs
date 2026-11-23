use super::{
    HIDDEN_FILES_KEYS, HIDDEN_FILES_KEY_MAC, LIVE_PREVIEW_KEY, TREE_COLLAPSE_SUBTREE_KEY,
    TREE_EXPAND_SUBTREE_KEY,
};
use gpui::{KeybindingKeystroke, Keystroke, Modifiers};

fn binding(keys: &str) -> KeybindingKeystroke {
    KeybindingKeystroke::from_keystroke(Keystroke::parse(keys).unwrap())
}

/// #916: macOS keeps shift for letters, so ⌘⇧E arrives as cmd-shift-e
/// and matches `secondary-shift-e` resolved for the Mac.
#[test]
fn live_preview_shortcut_matches_macos_report() {
    let mac = Keystroke {
        modifiers: Modifiers {
            platform: true,
            shift: true,
            ..Default::default()
        },
        key: "e".into(),
        key_char: None,
    };
    let resolved = LIVE_PREVIEW_KEY.replace("secondary", "cmd");
    assert!(mac.should_match(&binding(&resolved)));
    // Positive control: plain ⌘E stays Edit/Read.
    let plain = Keystroke {
        modifiers: Modifiers::command(),
        key: "e".into(),
        key_char: None,
    };
    assert!(!plain.should_match(&binding(&resolved)));
}

#[test]
fn mac_binding_matches_what_macos_reports_for_shift_cmd_period() {
    // gpui's macOS backend turns ⇧⌘. into the shifted character with
    // shift cleared (gpui-macos events.rs, `chars_with_shift`).
    let typed = Keystroke {
        modifiers: Modifiers {
            platform: true,
            ..Default::default()
        },
        key: ">".into(),
        key_char: None,
    };
    assert!(typed.should_match(&binding(HIDDEN_FILES_KEY_MAC)));
    // Positive control: the binding shipped in 6023 never fired on a Mac.
    assert!(!typed.should_match(&binding("cmd-shift-.")));
}

/// The keystroke gpui's macOS backend builds for an arrow key: arrows
/// keep every modifier as pressed (gpui-macos events.rs, function keys).
fn mac_arrow(key: &str, modifiers: Modifiers) -> Keystroke {
    Keystroke {
        modifiers,
        key: key.into(),
        key_char: None,
    }
}

#[test]
fn tree_shortcuts_match_macos_reports() {
    let alt = Modifiers {
        alt: true,
        ..Default::default()
    };
    assert!(mac_arrow("right", alt).should_match(&binding(TREE_EXPAND_SUBTREE_KEY)));
    assert!(mac_arrow("left", alt).should_match(&binding(TREE_COLLAPSE_SUBTREE_KEY)));
    // Positive control: plain → must stay the one-level expand.
    assert!(
        !mac_arrow("right", Modifiers::default()).should_match(&binding(TREE_EXPAND_SUBTREE_KEY))
    );
    let focus = binding("cmd-shift-left");
    assert!(mac_arrow(
        "left",
        Modifiers {
            platform: true,
            shift: true,
            ..Default::default()
        }
    )
    .should_match(&focus));
}

#[test]
fn linux_bindings_cover_both_reports() {
    let shifted = Keystroke {
        modifiers: Modifiers {
            control: true,
            shift: true,
            ..Default::default()
        },
        key: ".".into(),
        key_char: None,
    };
    let char_only = Keystroke {
        modifiers: Modifiers::control(),
        key: ">".into(),
        key_char: None,
    };
    for typed in [shifted, char_only] {
        assert!(HIDDEN_FILES_KEYS
            .iter()
            .any(|keys| typed.should_match(&binding(keys))));
    }
}
