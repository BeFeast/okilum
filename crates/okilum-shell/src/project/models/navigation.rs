//! Model-picker keyboard decisions. The visible row list is the single source
//! for number hints and selection, including filtered/favorite views.
use gpui::KeyDownEvent;
use okilum_t3::models::PickerChoice;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Intent {
    Toggle,
    Close,
    Highlight(usize),
    Select(usize),
}

#[derive(Clone, Copy)]
enum Primary {
    Control,
    Command,
}
impl Primary {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Command
        } else {
            Self::Control
        }
    }
    fn matches(self, event: &KeyDownEvent) -> bool {
        let modifiers = event.keystroke.modifiers;
        match self {
            Self::Control => modifiers.control && !modifiers.platform,
            Self::Command => modifiers.platform && !modifiers.control,
        }
    }
    fn hint(self, number: usize) -> String {
        match self {
            Self::Control => format!("Ctrl+{number}"),
            Self::Command => format!("⌘{number}"),
        }
    }
}

fn logical_key(event: &KeyDownEvent) -> &str {
    event
        .keystroke
        .key_char
        .as_deref()
        // Native Ctrl combinations may carry transformed control text (for
        // example Ctrl+M is CR); shortcuts still use the key, not that text.
        .filter(|text| !text.chars().any(char::is_control))
        .unwrap_or(&event.keystroke.key)
}

fn numbered(choices: &[PickerChoice]) -> impl Iterator<Item = usize> + '_ {
    choices
        .iter()
        .enumerate()
        .filter(|(_, choice)| choice.selectable)
        .map(|(index, _)| index)
        .take(9)
}

/// The index refers to the actual row in `choices`, not an unfiltered catalog.
/// Unavailable rows receive no number and cannot consume a shortcut ordinal.
pub(super) fn hint(choices: &[PickerChoice], index: usize) -> Option<String> {
    hint_with(Primary::current(), choices, index)
}
fn hint_with(primary: Primary, choices: &[PickerChoice], index: usize) -> Option<String> {
    numbered(choices)
        .position(|row| row == index)
        .map(|ordinal| primary.hint(ordinal + 1))
}

/// Call from the active Manager and the open popup's keyboard scope. Plain
/// digits/left/right/editing keys pass through; only the platform modifier plus
/// 1…9 selects a row. The owner gates IME composition and mutation readiness.
pub(super) fn handle(
    event: &KeyDownEvent,
    open: bool,
    highlighted: usize,
    choices: &[PickerChoice],
) -> Option<Intent> {
    handle_with(Primary::current(), event, open, highlighted, choices)
}
fn handle_with(
    primary: Primary,
    event: &KeyDownEvent,
    open: bool,
    highlighted: usize,
    choices: &[PickerChoice],
) -> Option<Intent> {
    let modifiers = event.keystroke.modifiers;
    let key = logical_key(event);
    if primary.matches(event) && modifiers.shift && !modifiers.alt && key.eq_ignore_ascii_case("m")
    {
        return (!event.is_held).then_some(Intent::Toggle);
    }
    // A closed model picker must not capture thread/application number keys.
    if !open {
        return None;
    }
    if primary.matches(event) && !modifiers.shift && !modifiers.alt {
        if event.is_held {
            return None;
        }
        let ordinal = match key {
            "1" => 0,
            "2" => 1,
            "3" => 2,
            "4" => 3,
            "5" => 4,
            "6" => 5,
            "7" => 6,
            "8" => 7,
            "9" => 8,
            _ => return None,
        };
        return numbered(choices).nth(ordinal).map(Intent::Select);
    }
    if modifiers.control || modifiers.platform || modifiers.alt || modifiers.shift {
        return None;
    }
    match key {
        "escape" => Some(Intent::Close),
        "enter" if !event.is_held => choices
            .get(highlighted)
            .filter(|choice| choice.selectable)
            .map(|_| Intent::Select(highlighted)),
        "up" | "down" => {
            let eligible: Vec<_> = choices
                .iter()
                .enumerate()
                .filter_map(|(index, choice)| choice.selectable.then_some(index))
                .collect();
            if eligible.is_empty() {
                return None;
            }
            let index = match eligible.iter().position(|index| *index == highlighted) {
                Some(position) if key == "down" => eligible[(position + 1) % eligible.len()],
                Some(position) => eligible[(position + eligible.len() - 1) % eligible.len()],
                None if key == "down" => eligible[0],
                None => *eligible.last().expect("eligible rows are not empty"),
            };
            Some(Intent::Highlight(index))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::Keystroke;
    use okilum_t3::ModelSelection;

    fn choices(selectable: &[bool]) -> Vec<PickerChoice> {
        selectable
            .iter()
            .enumerate()
            .map(|(index, selectable)| PickerChoice {
                selection: ModelSelection {
                    instance_id: "exact-account".into(),
                    model: format!("model-{index}"),
                    options: vec![],
                },
                label: format!("Model {index}"),
                provider_label: "Exact account".into(),
                sub_provider: None,
                current: index == 0,
                selectable: *selectable,
            })
            .collect()
    }
    fn key(value: &str, held: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke::parse(value).unwrap(),
            is_held: held,
            prefer_character_input: false,
        }
    }

    #[test]
    fn control_text_does_not_replace_the_native_shortcut_key() {
        let mut event = key("ctrl-shift-m", false);
        event.keystroke.key_char = Some("\r".into());
        assert_eq!(
            handle_with(Primary::Control, &event, false, 0, &[]),
            Some(Intent::Toggle)
        );
    }

    #[test]
    fn number_hints_and_keys_share_filtered_selectable_row_order() {
        let rows = choices(&[false, true, false, true, true]);
        assert_eq!(hint_with(Primary::Control, &rows, 0), None);
        assert_eq!(
            hint_with(Primary::Control, &rows, 1).as_deref(),
            Some("Ctrl+1")
        );
        assert_eq!(
            hint_with(Primary::Control, &rows, 3).as_deref(),
            Some("Ctrl+2")
        );
        assert_eq!(hint_with(Primary::Command, &rows, 4).as_deref(), Some("⌘3"));
        assert_eq!(
            handle_with(Primary::Control, &key("ctrl-2", false), true, 0, &rows),
            Some(Intent::Select(3))
        );
        let filtered = rows[3..].to_vec();
        assert_eq!(
            hint_with(Primary::Control, &filtered, 0).as_deref(),
            Some("Ctrl+1")
        );
        assert_eq!(
            handle_with(Primary::Control, &key("ctrl-1", false), true, 1, &filtered),
            Some(Intent::Select(0))
        );
        let ten = choices(&[true; 10]);
        assert!(hint_with(Primary::Control, &ten, 8).is_some());
        assert!(hint_with(Primary::Control, &ten, 9).is_none());
    }

    #[test]
    fn closed_picker_held_numeric_and_plain_editing_keys_do_not_select() {
        let rows = choices(&[true, true]);
        for (value, held, open) in [
            ("ctrl-1", false, false),
            ("ctrl-1", true, true),
            ("1", false, true),
            ("shift-1", false, true),
            ("alt-1", false, true),
            ("ctrl-shift-1", false, true),
            ("ctrl-alt-1", false, true),
            ("left", false, true),
            ("right", false, true),
            ("home", false, true),
            ("shift-up", false, true),
        ] {
            assert_eq!(
                handle_with(Primary::Control, &key(value, held), open, 0, &rows),
                None,
                "{value} held={held} open={open}"
            );
        }
        assert_eq!(
            handle_with(Primary::Control, &key("enter", true), true, 0, &rows),
            None,
            "held Enter cannot duplicate a selection"
        );
        assert_eq!(
            handle_with(Primary::Control, &key("ctrl-3", false), true, 0, &rows),
            None
        );
        assert_eq!(
            handle_with(Primary::Control, &key("ctrl-0", false), true, 0, &rows),
            None
        );
    }

    #[test]
    fn arrow_navigation_skips_unavailable_rows_and_enter_targets_the_highlight() {
        let rows = choices(&[false, true, false, true]);
        assert_eq!(
            handle_with(Primary::Control, &key("down", false), true, 0, &rows),
            Some(Intent::Highlight(1))
        );
        assert_eq!(
            handle_with(Primary::Control, &key("up", false), true, 0, &rows),
            Some(Intent::Highlight(3))
        );
        assert_eq!(
            handle_with(Primary::Control, &key("down", false), true, 1, &rows),
            Some(Intent::Highlight(3))
        );
        assert_eq!(
            handle_with(Primary::Control, &key("down", true), true, 3, &rows),
            Some(Intent::Highlight(1))
        );
        assert_eq!(
            handle_with(Primary::Control, &key("enter", false), true, 0, &rows),
            None
        );
        assert_eq!(
            handle_with(Primary::Control, &key("enter", false), true, 3, &rows),
            Some(Intent::Select(3))
        );
        assert_eq!(
            handle_with(Primary::Control, &key("escape", false), true, 3, &rows),
            Some(Intent::Close)
        );
        assert_eq!(
            handle_with(
                Primary::Control,
                &key("down", false),
                true,
                0,
                &choices(&[false, false])
            ),
            None
        );
    }

    #[test]
    fn default_toggle_and_platform_hints_use_the_same_primary_modifier() {
        let rows = choices(&[true]);
        for open in [false, true] {
            assert_eq!(
                handle_with(
                    Primary::Control,
                    &key("ctrl-shift-m", false),
                    open,
                    0,
                    &rows
                ),
                Some(Intent::Toggle)
            );
            assert_eq!(
                handle_with(Primary::Command, &key("cmd-shift-m", false), open, 0, &rows),
                Some(Intent::Toggle)
            );
        }
        assert_eq!(
            handle_with(Primary::Control, &key("ctrl-shift-m", true), true, 0, &rows),
            None
        );
        assert_eq!(
            handle_with(Primary::Control, &key("alt-shift-m", false), true, 0, &rows),
            None
        );
        assert_eq!(
            handle_with(Primary::Command, &key("ctrl-1", false), true, 0, &rows),
            None
        );
        assert_eq!(
            handle_with(Primary::Command, &key("cmd-1", false), true, 0, &rows),
            Some(Intent::Select(0))
        );
    }
}
