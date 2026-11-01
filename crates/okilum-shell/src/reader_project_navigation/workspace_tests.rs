use super::*;
use ::core::prelude::v1::test;
use gpui::{Modifiers, TestAppContext};

#[gpui::test]
fn files_and_notes_shortcut_restore_the_global_tree_after_project_switches(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("vault");
    for folder in ["", "Work/App", "Music/Studio"] {
        let root = vault.join(folder);
        std::fs::create_dir_all(root.join("ops")).unwrap();
        std::fs::write(root.join("ops/project.toml"), "kind = 'knowledge'\n").unwrap();
        std::fs::write(root.join("plan.md"), "# Project plan\n").unwrap();
    }
    std::fs::create_dir_all(vault.join("Files")).unwrap();
    for index in 0..100 {
        std::fs::write(vault.join(format!("Files/note-{index:03}.md")), "# File\n").unwrap();
    }
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("plan.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("widths.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = entity.unwrap();
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    reader.update(visual, |reader, cx| {
        reader.sync_tree();
        reader.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        reader.tree.set_subtree("Files", true);
        reader.tree.cursor = Some("Files/note-020.md".into());
        reader.panels.open(reader_layout::Panel::Notes);
        reader.panel_widths.notes = 352.;
        reader.panel_widths.backlinks = 268.;
        cx.notify();
    });
    visual.run_until_parked();
    reader.update(visual, |reader, _| {
        reader
            .tree_scroll
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(0.), px(-300.)));
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sidebar-tree-body").is_some(),
        "global Files positive control"
    );
    let before = reader.read_with(visual, |reader, _| {
        let offset = reader.tree_scroll.0.borrow().base_handle.offset();
        assert!(offset.y < px(0.), "the original tree was scrolled");
        (
            reader
                .tree
                .rows
                .iter()
                .map(|row| row.path.clone())
                .collect::<Vec<_>>(),
            reader.tree.expanded_paths().clone(),
            reader.tree.cursor.clone(),
            offset,
            reader.panel_widths.clone(),
        )
    });
    // Switching nested projects and the vault-root project keeps one original
    // Files snapshot, rather than capturing each preceding scoped tree.
    for folder in ["Work/App", "Music/Studio", ""] {
        let project = Project::read(&vault.join(folder)).unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.finish_enter_vault_project(project, window, cx);
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-board").is_some(),
            "Board is still available"
        );
        assert!(
            visual.debug_bounds("reader-backlinks-panel").is_some(),
            "Manager is still available"
        );
        assert!(visual.debug_bounds("reader-notes-panel").is_none());
        assert!(visual.debug_bounds("sidebar-tree-body").is_none());
    }
    let files = visual
        .debug_bounds("reader-files")
        .expect("explicit Files control");
    visual.simulate_click(files.center(), Modifiers::none());
    visual.run_until_parked();
    reader.read_with(visual, |reader, _| {
        assert!(reader.project_navigation.active.is_none());
        assert_eq!(reader.sidebar.left_mode, reader_sidebar::LeftMode::Tree);
        assert_eq!(
            reader
                .tree
                .rows
                .iter()
                .map(|row| row.path.clone())
                .collect::<Vec<_>>(),
            before.0
        );
        assert_eq!(reader.tree.expanded_paths(), &before.1);
        assert_eq!(reader.tree.cursor, before.2);
        assert_eq!(reader.tree_scroll.0.borrow().base_handle.offset(), before.3);
        assert_eq!(reader.panel_widths, before.4);
    });
    assert!(visual.debug_bounds("sidebar-tree-body").is_some());
    assert!(
        visual.debug_bounds("sidebar-header").is_some(),
        "the normal Files header returns; creation below exercises its file operations"
    );
    assert!(visual.debug_bounds("reader-files").is_none());
    let project = Project::read(&vault.join("Work/App")).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.finish_enter_vault_project(project, window, cx);
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-board").is_some());
    visual.dispatch_action(ToggleNotes);
    visual.run_until_parked();
    reader.read_with(visual, |reader, _| {
        assert!(
            reader.project_navigation.active.is_none(),
            "Notes requests Files, never a scoped tree"
        );
        assert_eq!(reader.tree.expanded_paths(), &before.1);
        assert_eq!(reader.tree.cursor, before.2);
        assert_eq!(reader.tree_scroll.0.borrow().base_handle.offset(), before.3);
        assert_eq!(reader.panel_widths, before.4);
    });
    assert!(visual.debug_bounds("sidebar-tree-body").is_some());
    #[cfg(any(unix, windows))]
    {
        reader.update_in(visual, |reader, window, cx| {
            reader.new_folder(None, window, cx);
            let create = reader
                .creation
                .as_ref()
                .expect("global file action is available");
            assert_eq!(
                create.folder, "Files",
                "creation follows the restored global cursor"
            );
            create.input.update(cx, |input, cx| {
                input.set_value("After project", window, cx);
            });
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(vault.join("Files/After project").is_dir());
        assert!(!vault.join("Work/App/After project").exists());
    }
}
