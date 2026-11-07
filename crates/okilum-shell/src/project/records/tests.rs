use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn records_1892_actual_project_tabs_preserve_board_and_append_only_to_visible_manager(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_features::enable_test_modules(cx);
    });
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let vault = temp.path().join("vault");
    let area = vault.join("project");
    for folder in ["ops/decisions", "ops/daily", "handovers"] {
        std::fs::create_dir_all(area.join(folder)).unwrap();
    }
    std::fs::write(vault.join("start.md"), "# Start\n").unwrap();
    std::fs::write(area.join("ops/project.toml"), "kind = 'knowledge'\n").unwrap();
    let source = area.join("ops/decisions/2026-11-06-choice.md");
    let original = "# Pick a direction\n\nKeep the original rationale.\n";
    std::fs::write(&source, original).unwrap();
    std::fs::write(
        area.join("ops/daily/2026-11-07.md"),
        "# Delivery report\nSaved beta 123.\n",
    )
    .unwrap();
    std::fs::write(
        area.join("handovers/2026-11-02.md"),
        "# Continuation context\nHistorical.\n",
    )
    .unwrap();
    let mut retained = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    open_path: Some(vault.join("start.md")),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("widths.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        retained = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = retained.unwrap();
    visual.simulate_resize(size(px(1500.), px(1000.)));
    visual.run_until_parked();
    let manager = reader.update_in(visual, |reader, window, cx| {
        reader.project_navigation.active = Some(okilum_t3::projects::Project::read(&area).unwrap());
        reader.project_navigation.section = reader_project_navigation::Section::Reader;
        reader.project_open = true;
        let manager = cx.new(|cx| {
            let mut view = ProjectView::empty(Some(temp.path().join("drafts")), window, cx);
            view.area = Some(area.clone());
            view.compose
                .update(cx, |input, cx| input.set_value("Keep my draft", window, cx));
            view
        });
        reader.project = Some(manager.clone());
        reader.panels.open(reader_layout::Panel::Backlinks);
        reader.panel_widths.backlinks = 560.;
        reader.ensure_project_board(area.clone(), window, cx);
        cx.notify();
        manager
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-board").is_some(),
        "positive control: actual Board mounted"
    );
    let board = reader.read_with(visual, |r, _| {
        r.project_navigation.boards[&area].entity_id()
    });
    for label in ["Decisions", "Reports", "Handovers", "Decisions"] {
        let bounds = visual
            .debug_bounds(&format!("project-tab-{label}"))
            .expect("real project tab");
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-record-detail").is_some(),
            "record rendered through real read"
        );
        assert!(
            visual.debug_bounds("reader-backlinks-panel").is_some(),
            "Manager stays on the right"
        );
        assert!(visual.debug_bounds("project-board").is_none());
        manager.read_with(visual, |view, cx| {
            assert_eq!(view.compose.read(cx).value().as_str(), "Keep my draft")
        });
    }
    let discuss = visual.debug_bounds("project-record-discuss").unwrap();
    visual.simulate_click(discuss.center(), Modifiers::default());
    visual.run_until_parked();
    manager.read_with(visual, |view, cx| {
        let text = view.compose.read(cx).value();
        assert!(text.starts_with("Keep my draft\n\n---\nDiscuss Decisions: Pick a direction"));
        assert!(text.contains("ops/decisions/2026-11-06-choice.md"));
        assert!(text.contains("Read revision (SHA-256):"));
        assert!(
            view.chat.is_none() && view.records.is_empty() && view.gateway.is_none(),
            "no connection, send or message journal was created"
        );
    });
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
    let back = visual.debug_bounds("project-tab-Board").unwrap();
    visual.simulate_click(back.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.project_navigation.boards[&area].entity_id(),
            board,
            "same retained Board"
        )
    });
    assert!(visual.debug_bounds("project-board").is_some());
    let decisions = visual.debug_bounds("project-tab-Decisions").unwrap();
    visual.simulate_click(decisions.center(), Modifiers::default());
    visual.run_until_parked();
    let open = visual.debug_bounds("project-record-open").unwrap();
    visual.simulate_click(open.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.current_rel, "project/ops/decisions/2026-11-06-choice.md")
    });
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
}

#[gpui::test]
fn records_1892_refresh_retains_selected_identity_and_failure_never_looks_empty(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let area = temp.path().to_owned();
    std::fs::create_dir_all(area.join("ops/decisions")).unwrap();
    let source = area.join("ops/decisions/2026-11-07.md");
    std::fs::write(&source, "# Original\n").unwrap();
    let (view, visual) = cx.add_window_view(|window, cx| {
        let mut view = RecordsView::new(area.clone(), window, cx);
        view.select(Some(Kind::Decisions), window, cx);
        view.set_visible(true, cx);
        view
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.current().unwrap().title, "Original")
    });
    std::fs::write(&source, "# Updated\n").unwrap();
    view.update(visual, |v, cx| v.changed(cx));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.current().unwrap().title, "Updated")
    });
    std::fs::write(&source, "---\ntitle: [invalid\n---\n").unwrap();
    view.update(visual, |v, cx| v.changed(cx));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.current().unwrap().title, "Updated");
        assert!(!v.snapshot.problems.is_empty());
    });
    std::fs::remove_file(&source).unwrap();
    view.update(visual, |v, cx| v.changed(cx));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(
            v.current().is_none(),
            "confirmed deletion removes the retained record"
        )
    });
    view.update(visual, |v, cx| v.set_visible(false, cx));
    std::fs::write(&source, "# Hidden change\n").unwrap();
    view.update(visual, |v, cx| v.changed(cx));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(v.current().is_none(), "hidden view performs no reads")
    });
}
