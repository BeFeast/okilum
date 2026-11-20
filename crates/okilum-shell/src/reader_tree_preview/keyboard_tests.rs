//! #2988: real key dispatch, not direct calls to activation/rename callbacks.
use super::*;
use ::core::prelude::v1::test;
use std::cell::Cell;

fn mount<'a>(
    cx: &'a mut TestAppContext,
    directory: &Path,
) -> (Entity<Reader>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = directory.join("vault");
    std::fs::create_dir_all(root.join("Folder")).unwrap();
    for (name, text) in [
        ("Start.md", "# Start"),
        ("Folder/@index.md", "# Folder"),
        ("Folder/Child.md", "# Child"),
    ] {
        std::fs::write(root.join(name), text).unwrap();
    }
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/viewers/images");
    std::fs::copy(fixtures.join("bitmap.bmp"), root.join("Alpha.bmp")).unwrap();
    std::fs::copy(fixtures.join("photo.jpg"), root.join("Beta.jpg")).unwrap();
    std::fs::write(root.join("Unknown.bin"), [0, 1, 2]).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Start.md".into()),
                    index_dir: Some(directory.join("index")),
                    session_directory: Some(directory.join("state")),
                    panel_settings_override: Some(directory.join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    (reader.unwrap(), visual)
}

fn focus(reader: &Entity<Reader>, visual: &mut VisualTestContext, path: &str) {
    reader.update_in(visual, |r, window, cx| r.reveal_in_tree(path, window, cx));
    visual.run_until_parked();
    assert!(visual.debug_bounds("sidebar-tree-body").is_some());
    reader.update_in(visual, |r, window, _| {
        assert_eq!(r.tree.cursor.as_deref(), Some(path));
        assert!(r.tree_focus.is_focused(window));
    });
}

fn teardown(reader: Entity<Reader>, visual: &mut VisualTestContext, directory: tempfile::TempDir) {
    // A selected image owns a real filesystem watcher. Release the Reader and
    // drain its task cancellation before deleting the watched fixture files.
    drop(reader);
    visual.update(|window, _| window.remove_window());
    visual.run_until_parked();
    directory.close().unwrap();
}

#[gpui::test]
fn issue2988_return_opens_focused_attachments_and_retains_note_folder_rename(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let (reader, visual) = mount(cx, directory.path());
    reader.update_in(visual, |r, window, cx| r.focus_handle.focus(window, cx));
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-l"
    } else {
        "ctrl-shift-l"
    });
    visual.run_until_parked();
    reader.update_in(visual, |r, window, _| {
        assert_eq!(r.tree.cursor.as_deref(), Some("Start.md"));
        assert!(
            r.tree_focus.is_focused(window),
            "keyboard entry into the Tree"
        );
    });
    visual.simulate_keystrokes("up up");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.tree.cursor.as_deref(), Some("Alpha.bmp"))
    });
    visual.simulate_keystrokes("down");
    visual.run_until_parked();
    reader.update_in(visual, |r, window, _| {
        assert_eq!(r.tree.cursor.as_deref(), Some("Beta.jpg"));
        assert_eq!(
            r.pane.current_rel, "Start.md",
            "arrows only move the cursor"
        );
        assert!(r.pane.file_preview.is_none());
        assert!(r.tree_focus.is_focused(window));
    });
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    reader.update_in(visual, |r, window, cx| {
        assert_eq!(r.selected_file(), "Beta.jpg");
        assert!(r.pane.file_preview.as_ref().unwrap().image);
        assert!(r.renaming.is_none());
        assert!(r.tree_focus.is_focused(window));
        assert!(Root::read(window, cx)
            .notification
            .read(cx)
            .notifications()
            .is_empty());
    });
    focus(&reader, visual, "Unknown.bin");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.selected_file(), "Unknown.bin");
        assert!(!r.pane.file_preview.as_ref().unwrap().image);
        assert!(r.renaming.is_none());
    });
    assert!(
        visual.opened_url().is_none(),
        "activation never launches another app"
    );
    // Keep existing note/folder Return and F2 rename, including Input precedence.
    for path in ["Start.md", "Folder"] {
        focus(&reader, visual, path);
        for key in ["enter", "f2"] {
            visual.simulate_keystrokes(key);
            visual.run_until_parked();
            reader.read_with(visual, |r, _| assert!(r.renaming.is_some()));
            visual.simulate_keystrokes("space");
            visual.run_until_parked();
            reader.read_with(visual, |r, _| {
                assert!(r.renaming.is_some(), "Input owns Space");
                assert!(!r.tree_preview.is_open());
            });
            visual.simulate_keystrokes("escape");
            visual.run_until_parked();
        }
    }
    visual.simulate_keystrokes("right right left left");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.tree.cursor.as_deref(), Some("Folder"));
        assert!(
            !r.tree
                .rows
                .iter()
                .find(|row| row.path == "Folder")
                .unwrap()
                .expanded
        );
    });
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-down"
    } else {
        "ctrl-down"
    });
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.selected_file(), "Folder/@index.md")
    });
    assert_eq!(
        std::fs::read_to_string(directory.path().join("vault/Start.md")).unwrap(),
        "# Start"
    );
    teardown(reader, visual, directory);
}

#[gpui::test]
fn issue2988_system_quick_look_refusal_falls_back_and_arrows_preview_without_navigation(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let (reader, visual) = mount(cx, directory.path());
    focus(&reader, visual, "Alpha.bmp");
    let calls = Rc::new(Cell::new(0));
    reader.update(visual, |r, _| {
        let calls = calls.clone();
        r.tree_preview.native_presenter = Some(Rc::new(move |_| {
            calls.set(calls.get() + 1);
            anyhow::bail!("Quick Look could not load this file")
        }));
    });
    let history = reader.read_with(visual, |r, _| r.navigation.history.clone());
    visual.simulate_keystrokes("space");
    visual.run_until_parked();
    assert_eq!(
        calls.get(),
        1,
        "the refused system provider was actually called"
    );
    assert!(visual.debug_bounds("tree-readonly-quick-look").is_some());
    assert!(
        visual.debug_bounds("quick-look-image").is_some(),
        "actual BMP preview is painted"
    );
    reader.update_in(visual, |r, window, cx| {
        assert!(r.tree_preview.is_open());
        assert!(r.tree_focus.is_focused(window));
        assert_eq!(r.pane.current_rel, "Start.md");
        assert!(r.pane.file_preview.is_none());
        assert_eq!(r.navigation.history, history);
        assert!(Root::read(window, cx)
            .notification
            .read(cx)
            .notifications()
            .is_empty());
    });
    visual.simulate_keystrokes("down");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.tree.cursor.as_deref(), Some("Beta.jpg"));
        assert_eq!(r.tree_preview.preview.as_ref().unwrap().0, "Beta.jpg");
        assert_eq!(r.pane.current_rel, "Start.md");
        assert_eq!(r.navigation.history, history);
    });
    assert_eq!(
        calls.get(),
        1,
        "fallback session does not retry the unavailable system per arrow"
    );
    assert!(
        visual.debug_bounds("quick-look-image").is_some(),
        "actual JPEG preview is painted"
    );
    visual.simulate_keystrokes("space");
    visual.run_until_parked();
    assert!(visual.debug_bounds("tree-readonly-quick-look").is_none());
    visual.simulate_keystrokes("space escape");
    visual.run_until_parked();
    assert_eq!(
        calls.get(),
        2,
        "a new explicit preview may retry the system"
    );
    reader.update_in(visual, |r, window, _| {
        assert!(!r.tree_preview.is_open());
        assert_eq!(r.pane.current_rel, "Start.md");
        assert!(r.tree_focus.is_focused(window));
    });
    // Ordinary activation after preview commits the selected file to Reader.
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.selected_file(), "Beta.jpg");
        assert!(!r.tree_preview.is_open());
    });
    for (name, original) in [("Alpha.bmp", "bitmap.bmp"), ("Beta.jpg", "photo.jpg")] {
        assert_eq!(
            std::fs::read(directory.path().join("vault").join(name)).unwrap(),
            std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../okilum-core/tests/fixtures/viewers/images")
                    .join(original)
            )
            .unwrap()
        );
    }
    teardown(reader, visual, directory);
}
