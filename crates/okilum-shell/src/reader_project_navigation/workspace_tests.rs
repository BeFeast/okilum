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
    let temp = canonical_tempdir();
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
        visual.update(|window, _| window.refresh());
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
    // Re-entry may retain the cached Board scene. Collect fresh debug selectors
    // before testing its presence; the native scene already remains visible.
    visual.update(|window, _| window.refresh());
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

/// Recreate the UI store from disk between Readers; no installed profile or app.
#[gpui::test]
fn persisted_project_restart_and_explicit_files_world(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = canonical_tempdir();
    let vault = temp.path().join("vault");
    let state = temp.path().join("state");
    std::fs::create_dir_all(vault.join("Work/App/ops")).unwrap();
    std::fs::write(
        vault.join("Work/App/ops/project.toml"),
        "kind = 'knowledge'\n",
    )
    .unwrap();
    std::fs::write(vault.join("home.md"), "# Home\n").unwrap();
    // A healthy alternative makes selecting an arbitrary discovered project observable.
    std::fs::create_dir_all(vault.join("Other/ops")).unwrap();
    std::fs::write(vault.join("Other/ops/project.toml"), "kind = 'knowledge'\n").unwrap();
    fn open<'a>(
        cx: &'a mut TestAppContext,
        vault: &Path,
        state: &Path,
        base: &Path,
        explicit: bool,
    ) -> (Entity<Reader>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| reader_ui_state::install(state, cx));
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.to_path_buf()),
                        note: Some("home.md".into()),
                        restore_workspace: !explicit,
                        index_dir: Some(base.join("index")),
                        session_directory: Some(state.to_path_buf()),
                        panel_settings_override: Some(base.join("widths.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.run_until_parked();
        (entity.unwrap(), visual)
    }
    let (first, visual) = open(cx, &vault, &state, temp.path(), false);
    first.update_in(visual, |reader, window, cx| {
        reader.panels.open(reader_layout::Panel::Notes);
        reader.panels.close(reader_layout::Panel::Backlinks);
        reader.panel_widths.notes = 333.;
        reader.panel_widths.backlinks = 244.;
        reader.tree.set_subtree("Work", true);
        reader.tree.cursor = Some("home.md".into());
        reader.finish_enter_vault_project(
            Project::read(&vault.join("Work/App")).unwrap(),
            window,
            cx,
        );
        assert!(!reader.workspace_panels().notes);
        reader.panel_widths.backlinks = 830.;
        reader.panels.close(reader_layout::Panel::Backlinks);
        reader.leave_vault_project(window, cx);
        assert!(reader.panels.notes);
        assert!(!reader.panels.backlinks);
        assert_eq!(reader.panel_widths.notes, 333.);
        assert_eq!(reader.panel_widths.backlinks, 244.);
        reader.finish_enter_vault_project(
            Project::read(&vault.join("Work/App")).unwrap(),
            window,
            cx,
        );
        assert!(
            !reader.panels.backlinks,
            "explicit Project visibility survives a switch"
        );
        assert_eq!(reader.panel_widths.backlinks, 830.);
        reader.panels.open(reader_layout::Panel::Backlinks);
        reader.record_ui_state(true, cx);
        reader_ui_state::flush(cx);
    });
    first.update_in(visual, |_, window, _| window.remove_window());
    let persisted = std::fs::read(state.join("reader-ui.json")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(
        json["vaults"][vault.to_str().unwrap()]["project"],
        "Work/App"
    );
    let (restarted, restart_visual) = open(cx, &vault, &state, temp.path(), false);
    restarted.read_with(restart_visual, |reader, _| {
        assert_eq!(
            reader.project_navigation.active.as_ref().unwrap().folder,
            vault.join("Work/App")
        );
        assert!(!reader.workspace_panels().notes);
        assert!(reader.panels.backlinks);
        assert_eq!(
            reader.panel_widths.backlinks, 830.,
            "Project width survives restart"
        );
        assert_eq!(reader.current_rel, "home.md");
    });
    restart_visual.update(|window, _| window.refresh());
    restart_visual.run_until_parked();
    assert!(restart_visual.debug_bounds("project-board").is_some());
    assert!(restart_visual.debug_bounds("sidebar-tree-body").is_none());
    restarted.update_in(restart_visual, |reader, window, cx| {
        reader.show_vault_files(window, cx);
        assert!(reader.panels.notes);
        assert!(!reader.panels.backlinks);
        assert_eq!(reader.panel_widths.notes, 333.);
        assert_eq!(reader.panel_widths.backlinks, 244.);
        reader.panel_widths.notes = 375.;
        assert_eq!(reader.tree.cursor.as_deref(), Some("home.md"));
        reader.record_ui_state(true, cx);
        reader_ui_state::flush(cx);
    });
    restarted.update_in(restart_visual, |_, window, _| window.remove_window());
    let (world, world_visual) = open(cx, &vault, &state, temp.path(), false);
    world.read_with(world_visual, |reader, _| {
        assert!(reader.project_navigation.active.is_none());
        assert_eq!(
            reader.panel_widths.notes, 375.,
            "Tree has its independent saved width"
        );
        assert!(!reader.panels.backlinks);
    });
    assert!(world_visual.debug_bounds("sidebar-tree-body").is_some());
    world.update_in(world_visual, |reader, window, cx| {
        reader.finish_enter_vault_project(Project::read(&vault.join("Other")).unwrap(), window, cx);
        assert_eq!(
            reader.panel_widths.backlinks, 830.,
            "Project presentation is shared by existing Project mode"
        );
        reader.leave_vault_project(window, cx);
        assert_eq!(reader.panel_widths.notes, 375.);
        assert_eq!(reader.panel_widths.backlinks, 244.);
    });
    world.update_in(world_visual, |_, window, _| window.remove_window());
    // Restore the same saved Project and verify an explicit document wins.
    std::fs::write(state.join("reader-ui.json"), &persisted).unwrap();
    let (explicit, explicit_visual) = open(cx, &vault, &state, temp.path(), true);
    explicit.read_with(explicit_visual, |reader, _| {
        assert!(reader.project_navigation.active.is_none());
        assert_eq!(reader.current_rel, "home.md");
    });
    explicit.update_in(explicit_visual, |_, window, _| window.remove_window());
    std::fs::create_dir_all(vault.join("ops")).unwrap();
    std::fs::write(vault.join("ops/project.toml"), "kind = 'knowledge'\n").unwrap();
    for relative in ["Other", "", "../outside"] {
        let mut saved: serde_json::Value = serde_json::from_slice(&persisted).unwrap();
        saved["vaults"][vault.to_str().unwrap()]["project"] = relative.into();
        std::fs::write(
            state.join("reader-ui.json"),
            serde_json::to_vec(&saved).unwrap(),
        )
        .unwrap();
        let (restored, visual) = open(cx, &vault, &state, temp.path(), false);
        restored.read_with(visual, |reader, _| {
            assert_eq!(
                reader
                    .project_navigation
                    .active
                    .as_ref()
                    .map(|project| project.folder.clone()),
                (relative != "../outside").then(|| vault.join(relative))
            );
        });
        restored.update_in(visual, |_, window, _| window.remove_window());
    }
    // Deleted project is a visible problem and safely retains World.
    std::fs::remove_file(vault.join("Work/App/ops/project.toml")).unwrap();
    std::fs::write(state.join("reader-ui.json"), &persisted).unwrap();
    let (deleted, deleted_visual) = open(cx, &vault, &state, temp.path(), false);
    deleted.read_with(deleted_visual, |reader, _| {
        assert!(reader.project_navigation.active.is_none());
        assert!(!reader.project_navigation.discovery.errors.is_empty());
    });
    assert_eq!(
        std::fs::read_to_string(vault.join("home.md")).unwrap(),
        "# Home\n"
    );
}

/// Force user intent between scheduling manifest validation and its completion.
#[gpui::test]
fn user_intent_cancels_inflight_workspace_restore(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = canonical_tempdir();
    let vault = temp.path().join("vault");
    let state = temp.path().join("state");
    std::fs::create_dir_all(vault.join("Work/ops")).unwrap();
    std::fs::write(vault.join("Work/ops/project.toml"), "kind = 'knowledge'\n").unwrap();
    std::fs::write(vault.join("home.md"), "# Home\n").unwrap();
    std::fs::write(vault.join("target.md"), "# Target\n").unwrap();
    cx.update(|cx| reader_ui_state::install(&state, cx));
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("home.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(state.clone()),
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
    visual.run_until_parked();
    for (index, intent) in ["file", "same-note-link", "projects"]
        .into_iter()
        .enumerate()
    {
        reader.update_in(visual, |reader, window, cx| {
            // Start the same asynchronous validation as restore_workspace_mode,
            // but hold the event loop until the newer user action is dispatched.
            reader.ui_state.restoring_workspace = true;
            reader.enter_project_folder(vault.join("Work"), window, cx);
            assert!(
                reader.ui_state.restoring_workspace,
                "positive in-flight control"
            );
            let restoring_generation = reader.project_navigation.generation;
            match intent {
                "file" => reader.open_note("target.md", None, window, cx),
                "same-note-link" => {
                    let current = reader.current_rel.clone();
                    reader.open_link(
                        &current,
                        okilum_core::deep_link::Position::default(),
                        window,
                        cx,
                    );
                }
                _ => reader.show_app_section(Section::Projects, window, cx),
            }
            assert!(!reader.ui_state.restoring_workspace);
            assert_ne!(reader.project_navigation.generation, restoring_generation);
        });
        visual.run_until_parked();
        reader.update(visual, |reader, cx| {
            assert!(
                reader.project_navigation.active.is_none(),
                "old validation must not enter Project"
            );
            if intent == "file" {
                assert_eq!(reader.current_rel, "target.md");
            }
            reader.panel_widths.notes = 321. + index as f32;
            reader.record_ui_state(true, cx);
            reader_ui_state::flush(cx);
        });
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join("reader-ui.json")).unwrap()).unwrap();
        assert_eq!(
            saved["vaults"][vault.to_str().unwrap()]["widths"]["notes"],
            321. + index as f32
        );
        assert!(
            saved["vaults"][vault.to_str().unwrap()]["project"].is_null(),
            "World persists after cancellation"
        );
    }
}
