use super::*;
use ::core::prelude::v1::test;

fn fixture(cx: &mut TestAppContext) -> (Entity<Reader>, &mut VisualTestContext, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("Folder/Sub")).unwrap();
    std::fs::create_dir_all(root.join("Other")).unwrap();
    std::fs::write(root.join("Folder/Sub/Note.md"), "# Note\n\nBody\n").unwrap();
    std::fs::write(root.join("Other/Else.md"), "# Else\n").unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
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
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    let reader = reader.unwrap();
    reader.read_with(visual, |v, _| {
        assert_eq!(v.selected_file(), "Folder/Sub/Note.md");
        assert!(v
            .vault
            .entries
            .iter()
            .any(|entry| entry.path == v.selected_file()));
    });
    (reader, visual, temp)
}

#[gpui::test]
fn breadcrumb_reveal_opens_sidebar_and_selects_note_at_wide_and_compact_widths(
    cx: &mut TestAppContext,
) {
    let (reader, visual, _temp) = fixture(cx);
    for width in [1400., 700.] {
        visual.simulate_resize(size(px(width), px(900.)));
        reader.update(visual, |v, cx| {
            v.tree.collapse_all();
            v.tree.cursor = None;
            v.panels.close(reader_layout::Panel::Notes);
            cx.notify();
        });
        visual.run_until_parked();
        let header = visual.debug_bounds("document-header").unwrap();
        let title = visual.debug_bounds("reader-document-root").unwrap();
        let button = visual
            .debug_bounds("reader-reveal-current")
            .expect("REVEAL1756: the note toolbar has Reveal beside its breadcrumb");
        assert!(button.left() >= header.left() && button.right() <= title.left());
        assert!(button.top() >= header.top() && button.bottom() <= header.bottom());
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("sidebar-header").is_some(),
            "positive header control"
        );
        assert!(
            visual.debug_bounds("sidebar-reveal-current").is_none(),
            "Reveal left the sidebar header"
        );
        reader.read_with(visual, |v, _| {
            assert!(v.panels.visible(reader_layout::Panel::Notes, width));
            assert_eq!(v.tree.cursor.as_deref(), Some("Folder/Sub/Note.md"));
            for path in ["Folder", "Folder/Sub"] {
                assert!(v
                    .tree
                    .rows
                    .iter()
                    .any(|row| row.path == path && row.expanded));
            }
            assert!(v
                .tree
                .rows
                .iter()
                .any(|row| row.path == "Folder/Sub/Note.md"));
            assert!(!v
                .tree
                .rows
                .iter()
                .any(|row| row.path == "Other" && row.expanded));
        });
    }
}

#[gpui::test]
fn breadcrumb_folder_click_expands_and_selects_that_folder_without_opening_a_note(
    cx: &mut TestAppContext,
) {
    let (reader, visual, _temp) = fixture(cx);
    reader.update(visual, |v, cx| {
        v.tree.collapse_all();
        v.tree.cursor = None;
        v.panels.close(reader_layout::Panel::Notes);
        cx.notify();
    });
    visual.run_until_parked();
    let crumb = visual
        .debug_bounds("reader-crumb-Folder/Sub")
        .expect("folder breadcrumb is an actual clickable control");
    visual.simulate_click(crumb.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |v, _| {
        assert!(v.panels.visible(reader_layout::Panel::Notes, 1400.));
        assert_eq!(v.tree.cursor.as_deref(), Some("Folder/Sub"));
        assert!(v
            .tree
            .rows
            .iter()
            .any(|row| row.path == "Folder/Sub" && row.expanded));
        assert_eq!(v.selected_file(), "Folder/Sub/Note.md");
    });
}

#[gpui::test]
fn breadcrumb_reveal_is_disabled_for_a_document_outside_the_tree(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(cx);
    reader.update(visual, |v, cx| {
        v.current_rel = "Absent.md".into();
        v.current_title = "Absent".into();
        v.panels.close(reader_layout::Panel::Notes);
        cx.notify();
    });
    visual.run_until_parked();
    let button = visual
        .debug_bounds("reader-reveal-current")
        .expect("REVEAL1756: keep the unavailable action visible");
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |v, _| {
        assert!(
            !v.panels.notes,
            "unavailable Reveal cannot open the sidebar"
        );
        assert_ne!(v.tree.cursor.as_deref(), Some("Absent.md"));
    });
}
