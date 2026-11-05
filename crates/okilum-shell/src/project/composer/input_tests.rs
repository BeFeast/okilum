//! Focused native editing through the same actions used by AppKit's Edit menu.
use super::*;
use ::core::prelude::v1::test;
use gpui::{TestAppContext, VisualTestContext};

pub(in crate::project) fn menu(visual: &mut VisualTestContext, label: &str) {
    let action = crate::note_paste::edit_menu()
        .items
        .into_iter()
        .find_map(|item| match item {
            gpui::MenuItem::Action { name, action, .. } if name.as_ref() == label => Some(action),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Edit menu must expose {label}"));
    visual.update(|window, cx| window.dispatch_action(action, cx));
    visual.run_until_parked();
}

#[gpui::test]
fn issue2521_native_text_paste_replaces_selection_and_undo_restores_it(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = super::super::fixture::canonical_tempdir();
    let mut project = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            view.composer.update(cx, |composer, cx| {
                composer.set_native(true, false, cx);
                composer.set_controls(true, false, cx);
            });
            view.compose.update(cx, |input, cx| {
                input.set_value("before old after", window, cx);
                input.set_selected_range(7..10, cx);
                input.focus(window, cx);
            });
            view
        });
        let composer = view.read(cx).composer.clone();
        project = Some(view);
        Root::new(composer, window, cx)
    });
    let view = project.unwrap();
    visual.run_until_parked();
    visual
        .update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("new\ntext".into())));
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-v"
    } else {
        "ctrl-v"
    });
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "before new\ntext after"
        )
    });
    visual.dispatch_action(gpui_component::input::Undo);
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert_eq!(view.compose.read(cx).value().as_str(), "before old after")
    });
}

#[gpui::test]
fn issue2521_edit_menu_and_command_shortcuts_follow_composer_focus(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    cx.update(crate::note_paste::install_menu);
    let temp = super::super::fixture::canonical_tempdir();
    let mut project = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            view.composer.update(cx, |composer, cx| {
                composer.set_native(true, false, cx);
                composer.set_controls(true, false, cx);
            });
            view.compose.update(cx, |input, cx| {
                input.set_value("draft 🧠", window, cx);
                input.focus(window, cx);
            });
            view
        });
        let composer = view.read(cx).composer.clone();
        project = Some(view);
        Root::new(composer, window, cx)
    });
    let view = project.unwrap();
    let value = |visual: &VisualTestContext| {
        view.read_with(visual, |view, cx| view.compose.read(cx).value().to_string())
    };
    visual.run_until_parked();
    for keyboard in [false, true] {
        let keys = if cfg!(target_os = "macos") {
            ["cmd-a", "cmd-c", "cmd-x", "cmd-v", "cmd-z", "cmd-shift-z"]
        } else {
            ["ctrl-a", "ctrl-c", "ctrl-x", "ctrl-v", "ctrl-z", "ctrl-y"]
        };
        for (index, label) in ["Select All", "Copy", "Cut", "Paste", "Undo", "Redo"]
            .into_iter()
            .enumerate()
        {
            if keyboard {
                visual.simulate_keystrokes(keys[index]);
                visual.run_until_parked();
            } else {
                menu(visual, label);
            }
            match label {
                "Copy" | "Cut" => visual.update(|_, cx| {
                    assert_eq!(
                        cx.read_from_clipboard().unwrap().text().as_deref(),
                        Some("draft 🧠")
                    )
                }),
                _ => {}
            }
            match label {
                "Cut" | "Undo" => assert_eq!(value(visual), ""),
                _ => assert_eq!(value(visual), "draft 🧠"),
            }
        }
    }
}
