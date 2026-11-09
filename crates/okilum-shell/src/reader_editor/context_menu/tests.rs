use super::*;
use ::core::prelude::v1::test;
#[cfg(target_os = "linux")]
use gpui::{Modifiers, MouseButton, TestAppContext};
#[cfg(target_os = "linux")]
use gpui_component::input::Undo;

#[test]
fn formatting_is_only_offered_in_writable_markdown() {
    assert!(formatting_menu(false, true).is_none(), "plain/code files");
    assert!(
        formatting_menu(true, false).is_none(),
        "read-only/disabled input"
    );
    let menu = formatting_menu(true, true).unwrap();
    let actions: Vec<_> = menu
        .items
        .iter()
        .filter_map(|item| match item {
            MenuItem::Action { name, action, .. } => Some((name.as_ref(), action.name())),
            _ => None,
        })
        .collect();
    assert_eq!(actions.len(), 15);
    assert_eq!(actions[0], ("Bold", format::ToggleBold.name()));
    assert_eq!(actions[3], ("Inline Code", format::ToggleInlineCode.name()));
    assert_eq!(actions[4], ("Quote", format::ToggleQuote.name()));
    assert_eq!(actions[5], ("Heading 1", format::Heading1.name()));
    assert_eq!(actions[11], ("Paragraph", format::Paragraph.name()));
    assert_eq!(
        actions[12],
        (
            "Format code",
            super::super::code_operations::FormatCode.name()
        )
    );
    assert_eq!(
        actions[13],
        (
            "Validate code",
            super::super::code_operations::ValidateCode.name()
        )
    );
    assert_eq!(
        actions[14],
        (
            "Repair code…",
            super::super::code_operations::RepairCode.name()
        )
    );
}

// The real right-click builder and native-menu fallback, not a direct call to
// the formatter. The fallback dispatches the same typed actions as Win32/Cocoa.
#[cfg(target_os = "linux")]
fn format_selection_from_context_menu(cx: &mut TestAppContext, live: bool, quote: bool) {
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
    });
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("vault");
    std::fs::create_dir_all(&root).unwrap();
    let original = "chosen text remains\n";
    std::fs::write(root.join("note.md"), original).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    open_path: Some(root.join("note.md")),
                    index_dir: Some(directory.path().join("index")),
                    session_directory: Some(directory.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(960.)));
    visual.run_until_parked();
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx);
        reader.set_live_preview(live, window, cx);
        assert_eq!(reader.editing.as_ref().unwrap().live_preview.enabled, live);
    });
    visual.run_until_parked();
    let input = reader.read_with(visual, |reader, _| {
        reader.editing.as_ref().unwrap().input.clone()
    });
    input.update(visual, |input, cx| input.set_selected_range(0..6, cx));
    visual.run_until_parked();
    let at =
        input.read_with(visual, |input, _| input.input_bounds().origin) + point(px(20.), px(12.));
    visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        input.read_with(visual, |input, _| input.selected_range()),
        0..6,
        "right-click preserves selection"
    );
    // Table follows Format in editable Markdown; Bold is Format's first child.
    visual.simulate_keystrokes("up up");
    visual.run_until_parked();
    visual.simulate_keystrokes("right");
    visual.run_until_parked();
    if quote {
        visual.simulate_keystrokes("down down down down");
        visual.run_until_parked();
    }
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        input.read_with(visual, |input, _| input.value().to_string()),
        if quote {
            "> chosen text remains\n"
        } else {
            "**chosen** text remains\n"
        },
        "real context action must fire"
    );
    visual.dispatch_action(Undo);
    visual.run_until_parked();
    assert_eq!(
        input.read_with(visual, |input, _| input.value().to_string()),
        original,
        "one Undo restores the edit"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("note.md")).unwrap(),
        original,
        "format/Undo preserves the original fixture bytes"
    );
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn source_context_formatting_preserves_selection_and_undo(cx: &mut TestAppContext) {
    format_selection_from_context_menu(cx, false, false);
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn live_preview_context_formatting_preserves_selection_and_undo(cx: &mut TestAppContext) {
    format_selection_from_context_menu(cx, true, false);
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn quote_2625_source_context_menu_toggles_whole_line(cx: &mut TestAppContext) {
    format_selection_from_context_menu(cx, false, true);
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn quote_2625_live_context_menu_toggles_whole_line(cx: &mut TestAppContext) {
    format_selection_from_context_menu(cx, true, true);
}
