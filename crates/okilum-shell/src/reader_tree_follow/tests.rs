use super::*;
use ::core::prelude::v1::test;

fn fixture(cx: &mut TestAppContext) -> (Entity<Reader>, &mut VisualTestContext, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("Folder/Sub")).unwrap();
    std::fs::create_dir_all(root.join("Other")).unwrap();
    std::fs::create_dir_all(root.join("Big/Deep")).unwrap();
    std::fs::write(root.join("Folder/Sub/Note.md"), "# Note\n\nBody\n").unwrap();
    std::fs::write(root.join("Other/Else.md"), "# Else\n").unwrap();
    std::fs::write(root.join("Big/Deep/Target.md"), "# Target\n").unwrap();
    for ix in 0..AUTO_REVEAL_MAX_CHILDREN {
        std::fs::write(root.join(format!("Big/Item {ix:03}.md")), "# Item\n").unwrap();
    }
    // Enough top-level folders that the tree can scroll.
    for ix in 0..40 {
        std::fs::create_dir_all(root.join(format!("Pad {ix:02}"))).unwrap();
        std::fs::write(root.join(format!("Pad {ix:02}/Pad.md")), "# Pad\n").unwrap();
    }
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_ui_state::install(&temp.path().join("state"), cx);
        cx.set_global(reader_history::TestSessionDirectory(
            temp.path().join("history"),
        ));
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Folder/Sub/Note.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    session_directory: Some(temp.path().join("session")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.run_until_parked();
    visual.simulate_resize(size(px(1400.), px(600.)));
    visual.run_until_parked();
    let reader = reader.unwrap();
    reader.update(visual, |v, cx| {
        assert_eq!(v.selected_file(), "Folder/Sub/Note.md");
        v.panels.open(reader_layout::Panel::Notes);
        v.tree.collapse_all();
        v.tree.cursor = None;
        cx.notify();
    });
    visual.run_until_parked();
    (reader, visual, temp)
}

fn open(reader: &Entity<Reader>, visual: &mut VisualTestContext, note: &str) {
    reader.update_in(visual, |v, window, cx| v.open_note(note, None, window, cx));
    visual.run_until_parked();
    reader.read_with(visual, |v, _| assert_eq!(v.selected_file(), note));
}

fn expanded(reader: &Entity<Reader>, visual: &mut VisualTestContext, folder: &str) -> bool {
    reader.read_with(visual, |v, _| v.tree.expanded_paths().contains(folder))
}

fn cursor(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> Option<String> {
    reader.read_with(visual, |v, _| v.tree.cursor.clone())
}

fn set_auto_reveal(visual: &mut VisualTestContext, on: bool) {
    visual.update(|_, cx| reader_ui_state::set_tree_auto_reveal(on, cx));
    visual.run_until_parked();
}

#[gpui::test]
fn opening_a_note_leaves_the_tree_alone_by_default(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(cx);
    assert!(!visual.update(|_, cx| reader_ui_state::tree_auto_reveal(cx)));
    open(&reader, visual, "Other/Else.md");
    assert!(
        !expanded(&reader, visual, "Other"),
        "AUTOREVEAL1762: off by default, opening a note opens no folder"
    );
    assert_eq!(
        cursor(&reader, visual),
        None,
        "an unlisted note selects nothing"
    );
    // Positive control: a note whose row is already listed is selected.
    reader.update(visual, |v, cx| {
        v.tree.set_expanded("Other", true);
        cx.notify();
    });
    open(&reader, visual, "Folder/Sub/Note.md");
    assert!(!expanded(&reader, visual, "Folder"));
    open(&reader, visual, "Other/Else.md");
    assert_eq!(cursor(&reader, visual).as_deref(), Some("Other/Else.md"));
    // Reveal in sidebar still does it on request.
    open(&reader, visual, "Folder/Sub/Note.md");
    reader.update_in(visual, |v, window, cx| v.focus_current_folder(window, cx));
    visual.run_until_parked();
    assert!(expanded(&reader, visual, "Folder") && expanded(&reader, visual, "Folder/Sub"));
    assert_eq!(
        cursor(&reader, visual).as_deref(),
        Some("Folder/Sub/Note.md")
    );
}

#[gpui::test]
fn auto_reveal_opens_folders_and_selects_the_note_without_taking_focus(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(cx);
    set_auto_reveal(visual, true);
    // Switching it on shows the open note at once.
    assert!(expanded(&reader, visual, "Folder") && expanded(&reader, visual, "Folder/Sub"));
    assert_eq!(
        cursor(&reader, visual).as_deref(),
        Some("Folder/Sub/Note.md")
    );
    reader.update_in(visual, |v, window, cx| {
        v.focus_handle.focus(window, cx);
        assert!(!v.tree_focus.is_focused(window));
    });
    open(&reader, visual, "Other/Else.md");
    assert!(
        expanded(&reader, visual, "Other"),
        "AUTOREVEAL1762: folders open"
    );
    assert_eq!(cursor(&reader, visual).as_deref(), Some("Other/Else.md"));
    assert!(
        expanded(&reader, visual, "Folder"),
        "folders the user had open stay open"
    );
    reader.update_in(visual, |v, window, _| {
        assert!(
            !v.tree_focus.is_focused(window),
            "AUTOREVEAL1762: the tree never takes focus from the note"
        );
    });
    // Positive control for the focus probe: manual Reveal focuses the tree.
    reader.update_in(visual, |v, window, cx| v.focus_current_folder(window, cx));
    visual.run_until_parked();
    reader.update_in(visual, |v, window, _| {
        assert!(v.tree_focus.is_focused(window))
    });
    // A closed sidebar stays closed; its tree is ready when it opens.
    reader.update(visual, |v, cx| {
        v.panels.close(reader_layout::Panel::Notes);
        cx.notify();
    });
    visual.run_until_parked();
    open(&reader, visual, "Pad 07/Pad.md");
    reader.read_with(visual, |v, _| {
        assert!(!v.panels.visible(reader_layout::Panel::Notes, 1400.));
    });
    assert!(expanded(&reader, visual, "Pad 07"));
}

#[gpui::test]
fn auto_reveal_waits_while_the_tree_is_scrolled(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(cx);
    set_auto_reveal(visual, true);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let tree = visual
        .debug_bounds("sidebar-tree-body")
        .expect("the tree is shown");
    visual.simulate_event(ScrollWheelEvent {
        position: tree.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-80.))),
        ..Default::default()
    });
    visual.run_until_parked();
    // Positive control: the wheel over the tree was seen.
    reader.read_with(visual, |v, _| assert!(v.tree_wheel_at.is_some()));
    open(&reader, visual, "Other/Else.md");
    assert!(
        !expanded(&reader, visual, "Other"),
        "AUTOREVEAL1762: the tree does not move while it is scrolled"
    );
    visual
        .executor()
        .advance_clock(TREE_WHEEL_QUIET + Duration::from_millis(50));
    visual.run_until_parked();
    assert!(
        expanded(&reader, visual, "Other"),
        "the reveal runs once scrolling stops"
    );
    assert_eq!(cursor(&reader, visual).as_deref(), Some("Other/Else.md"));
}

#[gpui::test]
fn auto_reveal_does_not_open_a_large_closed_folder(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(cx);
    set_auto_reveal(visual, true);
    open(&reader, visual, "Big/Deep/Target.md");
    assert!(
        !expanded(&reader, visual, "Big"),
        "AUTOREVEAL1762: a folder with more entries than the limit stays closed"
    );
    assert_eq!(
        cursor(&reader, visual).as_deref(),
        Some("Big"),
        "the closed folder that holds the note is selected instead"
    );
    // Once the user opened it, the reveal continues inside.
    reader.update(visual, |v, cx| {
        v.tree.set_expanded("Big", true);
        cx.notify();
    });
    open(&reader, visual, "Other/Else.md");
    open(&reader, visual, "Big/Deep/Target.md");
    assert!(expanded(&reader, visual, "Big/Deep"));
    assert_eq!(
        cursor(&reader, visual).as_deref(),
        Some("Big/Deep/Target.md")
    );
}
