//! Pure keymap shortcut formatting and key-cap helpers shared by viewer
//! chrome, viewers, and the app's shortcut sheet (#3238, epic #3241).
use gpui::{div, px, Action, App, Div, ParentElement as _, SharedString, Styled as _};
use okilum_shell_kit::brand;
use okilum_shell_platform::platform::labels::Os;

pub fn keys_for(keymap: &gpui::Keymap, action: &str, os: Os) -> Vec<String> {
    let mut payloads: Vec<&dyn Action> = Vec::new();
    for binding in keymap.bindings() {
        let bound = binding.action();
        if bound.name() == action && !payloads.iter().any(|seen| seen.partial_eq(bound)) {
            payloads.push(bound);
        }
    }
    display_keys(
        payloads
            .into_iter()
            .flat_map(|payload| keymap.bindings_for_action(payload).rev()),
        os,
    )
}

/// The keys of exactly this `action` value, highest precedence first.
pub fn formatted_keys(keymap: &gpui::Keymap, action: &dyn Action, os: Os) -> Vec<String> {
    display_keys(keymap.bindings_for_action(action).rev(), os)
}

fn display_keys<'a>(bindings: impl Iterator<Item = &'a gpui::KeyBinding>, os: Os) -> Vec<String> {
    let mut keys = Vec::new();
    for binding in bindings {
        let text = binding
            .keystrokes()
            .iter()
            .map(|key| os.shortcut(&key.unparse()))
            .collect::<Vec<_>>()
            .join(" ");
        if !keys.contains(&text) {
            keys.push(text);
        }
    }
    if os == Os::Mac {
        let all = keys.clone();
        keys.retain(|key| mac_native(key).is_none_or(|cmd| !all.contains(&cmd)));
    }
    keys
}

/// The ⌘ form of a macOS ⌃ shortcut without ⌘: "⌃⇧." → "⇧⌘.".
fn mac_native(key: &str) -> Option<String> {
    const GLYPHS: [char; 4] = ['⌃', '⌥', '⇧', '⌘'];
    let modifiers: String = key.chars().take_while(|c| GLYPHS.contains(c)).collect();
    if !modifiers.contains('⌃') || modifiers.contains('⌘') {
        return None;
    }
    let rest = &key[modifiers.len()..];
    Some(format!("{}⌘{rest}", modifiers.replace('⌃', "")))
}

/// The key shown next to `action` in a tooltip or a hint, if it is bound.
pub fn shortcut(action: &dyn Action, cx: &App) -> Option<String> {
    formatted_keys(&cx.key_bindings().borrow(), action, Os::CURRENT)
        .into_iter()
        .next()
}

/// The bound key of `action` as key caps (#1517): «Ctrl» «Tab», the key the
/// tooltip [`shortcut`] names, split into its keys.
pub fn shortcut_caps(action: &dyn Action, cx: &App) -> Option<Vec<String>> {
    let bindings = cx.key_bindings();
    let keymap = bindings.borrow();
    let shown = keys_for(&keymap, action.name(), Os::CURRENT)
        .into_iter()
        .next()?;
    let caps = keymap
        .bindings()
        .rev()
        .filter(|binding| binding.action().name() == action.name())
        .map(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|key| key.unparse())
                .collect::<Vec<_>>()
        })
        .find(|keys| {
            keys.iter()
                .map(|key| Os::CURRENT.shortcut(key))
                .collect::<Vec<_>>()
                .join(" ")
                == shown
        })
        .map(|keys| {
            keys.iter()
                .flat_map(|key| Os::CURRENT.key_caps(key))
                .collect()
        });
    caps
}

/// One key as a small muted cap, the shortcut sheet's style; hints that name
/// a key (the Recent menu footer, #1517) use the same cap.
pub fn key_cap(
    key: impl Into<SharedString>,
    palette: brand::Palette,
    tokens: brand::ReaderPalette,
) -> Div {
    div()
        .h(px(20.))
        .px(px(6.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(4.))
        .bg(tokens.hover)
        .text_size(px(12.))
        .text_color(palette.text_muted)
        .child(key.into())
}

/// `label` followed by the bound key of `action`, e.g. "Back ⌥←".
pub fn all_shortcuts(action: &dyn Action, cx: &App) -> String {
    let bindings = cx.key_bindings();
    let map = bindings.borrow();
    keys_for(&map, action.name(), Os::CURRENT).join(" · ")
}

/// Contextual menu hints must omit bindings shadowed in that content mode.
pub fn content_shortcuts(action: &dyn Action, editing: bool, cx: &App) -> String {
    let contexts = if editing {
        ["Reader", "ReaderSource", "Input"]
    } else {
        ["Reader", "ReaderRead", "TextView"]
    };
    let contexts: Vec<_> = contexts
        .iter()
        .filter_map(|s| gpui::KeyContext::parse(s).ok())
        .collect();
    let map = cx.key_bindings();
    let map = map.borrow();
    let mut shown = Vec::new();
    for binding in map.bindings_for_action(action).rev() {
        let (effective, _) = map.bindings_for_input(binding.keystrokes(), &contexts);
        if !effective
            .first()
            .is_some_and(|candidate| candidate.action().partial_eq(action))
        {
            continue;
        }
        let key = binding
            .keystrokes()
            .iter()
            .map(|key| Os::CURRENT.shortcut(&key.unparse()))
            .collect::<Vec<_>>()
            .join(" ");
        if !shown.contains(&key) {
            shown.push(key);
        }
    }
    shown.join(" · ")
}

pub fn hint(label: &str, action: &dyn Action, cx: &App) -> SharedString {
    match shortcut(action, cx) {
        Some(key) => format!("{label} {key}").into(),
        None => SharedString::from(label.to_owned()),
    }
}
