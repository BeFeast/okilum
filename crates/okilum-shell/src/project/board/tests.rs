use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn invalid_card_stays_visible_beside_healthy_cards_and_binding_detail_is_preserved(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let mut source: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    source["issues"][0]["expected"] = serde_json::json!("");
    let binding = Binding::parse(&source.to_string()).unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_kanban_reader(
                area.path().into(),
                okilum_t3::board::live::read,
                window,
                cx,
            );
            board.binding = Some(binding.clone());
            board
        });
        Root::new(board, window, cx)
    });
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-card-problem-0").is_some());
    assert!(visual.debug_bounds("board-expand-1762").is_some());
    assert!(visual.debug_bounds("board-expand-1731").is_none());
    board.read_with(visual, |board, _| {
        assert!(board
            .binding
            .as_ref()
            .unwrap()
            .issues
            .iter()
            .all(|scope| scope.number != 1731))
    });
    board.update(visual, |board, cx| {
        board.binding_error =
            Some("ops/board.toml: Invalid TOML: line 7, column 2: invalid array".into());
        assert!(board
            .problem(now())
            .unwrap()
            .contains("line 7, column 2: invalid array"));
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-source-problem").is_none());
    let status = visual.debug_bounds("board-source-status").unwrap();
    visual.simulate_click(status.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-source-problem").is_some());
    assert!(visual.debug_bounds("board-expand-1762").is_some());
    assert_eq!(visual.opened_url(), None);
    board.update(visual, |board, cx| {
        board.note("https://git.oklabs.uk/BeFeast/okilum/issues/1859", cx)
    });
    assert_eq!(
        visual.opened_url().as_deref(),
        Some("https://git.oklabs.uk/BeFeast/okilum/issues/1859")
    );
    board.update(visual, |board, cx| {
        board.note("javascript:bad.md", cx);
        assert!(board
            .notice
            .as_ref()
            .unwrap()
            .contains("invalid related link"));
    });
    assert_eq!(
        visual.opened_url().as_deref(),
        Some("https://git.oklabs.uk/BeFeast/okilum/issues/1859")
    );
}

#[gpui::test]
fn source_error_and_recovery_keep_board_geometry_stable(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let binding = Binding::parse(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_kanban_reader(
                area.path().into(),
                okilum_t3::board::live::read,
                window,
                cx,
            );
            board.binding = Some(binding);
            board
        });
        Root::new(board, window, cx)
    });
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    visual.run_until_parked();
    let columns = visual.debug_bounds("board-columns").unwrap();
    let card = visual.debug_bounds("board-expand-1762").unwrap();
    let indicator = visual.debug_bounds("board-source-status").unwrap();
    assert!(visual.debug_bounds("board-source-problem").is_none());

    for error in [
        Some("T3 returned duplicate thread IDs; previous source retained"),
        Some("A much longer repeated transport diagnostic that must wrap only inside its disclosure, never above the Board's cards"),
        None,
    ] {
        board.update(visual, |board, cx| {
            board.snapshot.t3.error = error.map(str::to_owned);
            assert_eq!(board.problem(now()).is_some(), error.is_some());
            cx.notify();
        });
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("board-columns").unwrap(), columns);
        assert_eq!(visual.debug_bounds("board-expand-1762").unwrap(), card);
        assert_eq!(visual.debug_bounds("board-source-status").unwrap(), indicator);
        assert!(visual.debug_bounds("board-source-problem").is_none());
    }

    board.update(visual, |board, cx| {
        board.snapshot.t3.error = Some("Retained source diagnostic".into());
        cx.notify();
    });
    visual.run_until_parked();
    visual.simulate_click(indicator.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-source-details").is_some());
    assert!(visual.debug_bounds("board-source-problem").is_some());
    assert_eq!(visual.debug_bounds("board-columns").unwrap(), columns);
    assert_eq!(visual.debug_bounds("board-expand-1762").unwrap(), card);
}

#[gpui::test]
fn discuss_1784_actual_composer_ack_preserves_draft_and_never_sends(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let manager = cx.new(|cx| {
            #[cfg(native_manager)]
            let mut view = super::super::discuss_handoff::native_tests::view_with_draft(
                state.path(),
                "Keep my draft 📝",
                window,
                cx,
            );
            #[cfg(not(native_manager))]
            let mut view =
                super::super::fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            view.gateway = Some(gateway.clone());
            view.area = Some(state.path().to_owned());
            view.compose.update(cx, |input, cx| {
                input.set_value("Keep my draft 📝", window, cx)
            });
            view
        });
        Root::new(manager, window, cx)
    });
    let manager = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    let area = manager.read_with(visual, |view, _| view.area.clone().unwrap());
    visual.run_until_parked();
    manager.update_in(visual, |view, window, cx| {
        #[cfg(native_manager)]
        {
            view.draft_handoff_surface_visible(true, cx);
            view.append_board_context(&area, "Board context #1767".into(), window, cx)
                .unwrap();
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "Keep my draft 📝\n\n---\nBoard context #1767"
            );
        }
        #[cfg(not(native_manager))]
        {
            assert!(view
                .append_board_context(&area, "Board context #1767".into(), window, cx)
                .is_err());
            assert_eq!(view.compose.read(cx).value().as_str(), "Keep my draft 📝");
        }
        assert!(view
            .append_board_context(
                &area.join("different-project"),
                "WRONG PROJECT".into(),
                window,
                cx
            )
            .is_err());
    });
    assert!(gateway.sends.lock().unwrap().is_empty());
    #[cfg(native_manager)]
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    visual.run_until_parked();
    manager.read_with(visual, |view, cx| {
        assert_eq!(view.compose.read(cx).value().as_str(), "Keep my draft 📝")
    });
    assert!(gateway.sends.lock().unwrap().is_empty());
}

#[gpui::test]
fn hidden_board_retains_filter_selection_scroll_and_rejects_late_source(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            Board::with_kanban_reader(
                area.path().to_owned(),
                okilum_t3::board::live::read,
                window,
                cx,
            )
        });
        Root::new(board, window, cx)
    });
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    board.update_in(visual, |board, window, cx| {
        board.filter.executor = Some("Editing".into());
        board.selected = Some(1767);
        board.scroll.set_offset(point(px(0.), px(-120.)));
        // Use the production activation path, then deliver a real-generation source result late.
        board.set_visible(true, window, cx);
        board.snapshot.invalidate();
        let ticket = board.snapshot.begin().unwrap();
        board.set_visible(false, window, cx);
        board.snapshot.issues(
            &ticket,
            vec![(
                1767,
                Ok(okilum_t3::board::Issue {
                    number: 1767,
                    title: "late".into(),
                    updated_at: String::new(),
                    state: "closed".into(),
                    body: String::new(),
                    labels: Vec::new(),
                    open_pull_requests: Vec::new(),
                    live_pulls: Vec::new(),
                    report: Default::default(),
                }),
            )],
            1,
        );
        assert!(board.snapshot.forgejo.value.is_empty());
        assert_eq!(board.filter.executor.as_deref(), Some("Editing"));
        assert_eq!(board.selected, Some(1767));
        assert!(board.ticker.is_none());
        assert_eq!(board.scroll.offset().y, px(-120.));
    });
}

#[gpui::test]
fn real_reader_note_and_board_roundtrip_preserves_navigation_and_manager_draft(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    fn coordinates(position: ListOffset) -> (usize, Pixels) {
        (position.item_ix, position.offset_in_item)
    }
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = crate::project::fixture::canonical_tempdir();
    let vault = temp.path().join("vault");
    let folder = vault.join("project");
    std::fs::create_dir_all(folder.join("ops")).unwrap();
    let paragraphs = (0..60)
        .map(|i| format!("Paragraph {i}\n\n"))
        .collect::<String>();
    std::fs::write(vault.join("start.md"), format!("# Start\n\n{paragraphs}")).unwrap();
    std::fs::write(
        folder.join("note.md"),
        format!("# Related note\nActual body\n\n{paragraphs}"),
    )
    .unwrap();
    std::fs::write(folder.join("stage.md"), "# Stage metadata\n").unwrap();
    std::fs::write(
        folder.join("ops/project.yaml"),
        "display_name: Board fixture\nrepo: {host: git.oklabs.uk, full_name: BeFeast/okilum}\n",
    )
    .unwrap();
    let mut binding: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    binding["issues"]
        .as_array_mut()
        .unwrap()
        .retain(|s| s["number"] == 1767);
    binding["stage_note"] = serde_json::json!("stage.md");
    binding["attention"] = serde_json::json!([{
        "kind":"try", "title":"Try the related note", "subline":"Return to the same card afterwards", "link":"note.md"
    }]);
    binding["issues"][0]["notes"] = serde_json::json!(["note.md", "stage.md"]);
    std::fs::write(
        folder.join("ops/board.yaml"),
        serde_json::to_vec(&binding).unwrap(),
    )
    .unwrap();
    std::fs::write(
        folder.join("ops/board-context.toml"),
        "version=1\nnotes=['decision.md']\nnext_stage='next.md'\n",
    )
    .unwrap();
    std::fs::write(folder.join("decision.md"), "---\nboard_context:\n  kind: decision\n  title: Choose a safe alternative\n  reason: Waiting for owner input\n  consequence: The next action waits\n  issue: 1767\n---\n# Alternatives\nThe original **Markdown** context.\n").unwrap();
    std::fs::write(folder.join("next.md"), "---\nboard_context:\n  kind: next_stage\n  title: A later stage\n  reason: Its own goal\n  consequence: Its own scope\n---\nFuture context only.\n").unwrap();
    let project = okilum_t3::projects::Project::read(&folder).unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    open_path: Some(vault.join("start.md")),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = entity.unwrap();
    visual.simulate_resize(size(px(1400.), px(800.)));
    visual.run_until_parked();
    reader.update_in(visual, |reader, _, cx| {
        reader.content.read(cx).list_state().scroll_to(ListOffset {
            item_ix: 10,
            offset_in_item: px(3.),
        });
        reader.cancel_pending_landing();
    });
    visual.run_until_parked();
    let reading_place = reader.read_with(visual, |reader, cx| {
        reader.content.read(cx).list_state().logical_scroll_top()
    });
    assert!(
        reading_place.item_ix > 0,
        "positive control: visible document was scrolled"
    );
    reader.update_in(visual, |reader, window, cx| {
        reader.ensure_project_board_using(
            folder.clone(),
            |binding| {
                Ok(binding
                    .issues
                    .iter()
                    .map(|s| okilum_t3::board::Issue {
                        number: s.number,
                        title: format!("Fixture issue {}", s.number),
                        updated_at: String::new(),
                        state: "open".into(),
                        body: String::new(),
                        labels: vec![okilum_t3::board::IssueLabel {
                            name: "from-oleg".into(),
                        }],
                        open_pull_requests: Vec::new(),
                        live_pulls: Vec::new(),
                        report: Default::default(),
                    })
                    .collect())
            },
            window,
            cx,
        );
        reader.enter_vault_project(project.clone(), window, cx);
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-board").is_some());
    assert!(visual.debug_bounds("board-attention").is_some());
    reader.read_with(visual, |reader, _| {
        assert!(
            !reader.panels.notes,
            "project entry starts with Board and Manager; tree is opt-in"
        );
        assert!(reader.panels.backlinks);
    });
    // A source publication for the retained note must update that document
    // without silently replacing the explicit project surface (#2099).
    reader.update_in(visual, |reader, window, cx| {
        // Model the reset list behind Board after a derived replacement.
        reader
            .content
            .read(cx)
            .list_state()
            .scroll_to(ListOffset::default());
        assert_eq!(
            coordinates(reader.content.read(cx).list_state().logical_scroll_top()),
            coordinates(ListOffset::default())
        );
        assert_eq!(
            coordinates(reader.navigation.places[&(vault.clone(), "start.md".into())]),
            coordinates(reading_place)
        );
        reader.reconcile_inventory_document(
            &[(
                "start.md".to_owned(),
                format!("# Background refresh\nChanged bytes\n\n{paragraphs}"),
            )]
            .into_iter()
            .collect(),
            window,
            cx,
        );
    });
    visual.run_until_parked();
    reader.read_with(visual, |reader, _| {
        assert!(
            reader.note_source.contains("Changed bytes"),
            "positive control: background replacement was applied"
        );
        assert!(reader.project_navigation.board_visible);
        assert!(
            reader.navigation.pending_landing.is_none(),
            "an unmounted document has no pending layout landing"
        );
        assert!(
            reader.link_notice.is_none(),
            "background reconciliation does not report a navigation failure"
        );
    });
    // Cached Board paint is retained when only the background note changes.
    // Debug selectors are collected on fresh paint, unlike the native scene.
    visual.update(|window, _| window.refresh());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-board").is_some());
    reader.read_with(visual, |reader, _| {
        assert_eq!(
            coordinates(reader.navigation.places[&(vault.clone(), "start.md".into())]),
            coordinates(reading_place),
            "background replacement must retain the saved reading place"
        );
    });
    reader.update_in(visual, |reader, window, cx| {
        reader
            .content
            .read(cx)
            .list_state()
            .scroll_to(ListOffset::default());
        reader.enter_vault_project(project.clone(), window, cx);
    });
    visual.run_until_parked();
    reader.read_with(visual, |reader, _| {
        assert_eq!(
            coordinates(reader.navigation.places[&(vault.clone(), "start.md".into())]),
            coordinates(reading_place),
            "selecting Board again must not overwrite its retained document's place"
        );
    });
    reader.update_in(visual, |reader, window, cx| {
        assert!(reader.project_navigation.board_visible);
        #[cfg(native_manager)]
        {
            let manager = cx.new(|cx| {
                let mut view = super::super::discuss_handoff::native_tests::view_with_draft(
                    &temp.path().join("native-drafts"),
                    "Keep my draft",
                    window,
                    cx,
                );
                view.area = Some(folder.clone());
                view
            });
            reader.project = Some(manager);
        }
        let manager = reader.project.clone().expect("actual native Manager");
        manager.update(cx, |manager, cx| {
            manager
                .compose
                .update(cx, |input, cx| input.set_value("Keep my draft", window, cx))
        });
        reader.project_navigation.boards[&folder].update(cx, |board, cx| {
            board.list.preferences.mode = list::Mode::Board;
            board.filter.executor = Some("Editing".into());
            board.filter.chip = filters::Chip::Oleg;
            board.filter.search = "#1767".into();
            board
                .filter_ui
                .search
                .update(cx, |input, cx| input.set_value("#1767", window, cx));
            cx.notify();
        });
    });
    visual.run_until_parked();
    // Real Reader integration: opening the existing Manager must not push the
    // first or last Board lane beyond the center at a compact window width.
    reader.read_with(visual, |reader, _| {
        assert!(reader.project.is_some());
    });
    for width in [1100., 1400.] {
        visual.simulate_resize(size(px(width), px(1000.)));
        visual.run_until_parked();
        let center = visual.debug_bounds("project-board").unwrap();
        for key in ["board-columns", "board-expand-empty-Ready"] {
            let bounds = visual.debug_bounds(key).unwrap();
            assert!(
                bounds.left() >= center.left() && bounds.right() <= center.right(),
                "{key} stays inside the Reader center"
            );
        }
    }
    // Compact cards expose only the summary; links and notes live in the dock.
    assert!(visual.debug_bounds("board-plan").is_none());
    assert!(visual.debug_bounds("board-discuss-1767").is_none());
    // Queue uses the dense title/number row; full state stays in its disclosure.
    let expand = visual
        .debug_bounds("board-expand-1767")
        .expect("visible real issue card");
    visual.simulate_click(expand.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-plan").is_none(),
        "Stage note has no hidden replacement"
    );
    assert!(
        visual.debug_bounds("board-peek-content").is_some(),
        "Issue details live in the right dock"
    );
    let discuss = visual
        .debug_bounds("board-discuss-1767")
        .expect("actual draft-only context action");
    visual.simulate_click(discuss.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        let value = reader
            .project
            .as_ref()
            .unwrap()
            .read(cx)
            .compose
            .read(cx)
            .value();
        #[cfg(native_manager)]
        {
            assert!(value.starts_with("Keep my draft"));
            assert!(value.contains("issues/1767"));
        }
        #[cfg(not(native_manager))]
        assert_eq!(value.as_str(), "Keep my draft");
    });
    let tab = visual.debug_bounds("board-peek-tab").unwrap();
    visual.simulate_click(tab.center(), Modifiers::default());
    visual.run_until_parked();
    let note = visual
        .debug_bounds("board-note-1767-0")
        .expect("explicit related note action");
    visual.simulate_click(note.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        assert_eq!(reader.current_rel, "project/note.md");
        assert!(!reader.project_navigation.board_visible);
        assert!(reader
            .project
            .as_ref()
            .unwrap()
            .read(cx)
            .compose
            .read(cx)
            .value()
            .contains("Keep my draft"));
    });
    reader.update_in(visual, |reader, _, cx| {
        reader.content.read(cx).list_state().scroll_to(ListOffset {
            item_ix: 10,
            offset_in_item: px(3.),
        });
        reader.cancel_pending_landing();
        // An earlier visit's cache must not beat a newly scrolled visible note.
        reader.navigation.places.insert(
            (vault.clone(), "project/note.md".into()),
            ListOffset::default(),
        );
    });
    visual.run_until_parked();
    let fresh_place = reader.read_with(visual, |reader, cx| {
        reader.content.read(cx).list_state().logical_scroll_top()
    });
    assert!(
        fresh_place.item_ix > 0,
        "positive control: visible related note scrolled further"
    );
    let back = visual
        .debug_bounds("project-return-board")
        .expect("one-action Board return");
    visual.simulate_click(back.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        assert!(reader.project_navigation.board_visible);
        assert_eq!(
            coordinates(reader.navigation.places[&(vault.clone(), "project/note.md".into())]),
            coordinates(fresh_place),
            "Back to Board captures the visible viewport before hiding it"
        );
        assert!(reader.navigation.pending_landing.is_none());
        let board = reader.project_navigation.boards[&folder].read(cx);
        assert_eq!(board.filter.executor.as_deref(), Some("Editing"));
        assert_eq!(board.filter.search, "#1767");
        assert_eq!(board.filter.chip, filters::Chip::Oleg);
        assert_eq!(board.selected, Some(1767));
        assert!(reader
            .project
            .as_ref()
            .unwrap()
            .read(cx)
            .compose
            .read(cx)
            .value()
            .contains("Keep my draft"));
    });
    let attention = visual
        .debug_bounds("board-attention-0")
        .expect("explicit human-owned note link");
    visual.simulate_click(attention.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        assert_eq!(reader.current_rel, "project/note.md");
        assert!(!reader.project_navigation.board_visible);
        assert!(reader
            .project
            .as_ref()
            .unwrap()
            .read(cx)
            .compose
            .read(cx)
            .value()
            .contains("Keep my draft"));
    });
    let back = visual.debug_bounds("project-return-board").unwrap();
    visual.simulate_click(back.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-attention-0").is_some());
    assert!(visual.debug_bounds("board-waiting-open-0").is_some());
    assert!(visual.debug_bounds("board-note-open-0").is_none());
    // The waiting summary is immediate; detailed context and Discuss expand
    // explicitly, preserving the real Reader/Manager/Undo acceptance below.
    let details = visual.debug_bounds("board-context-details").unwrap();
    visual.simulate_click(details.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-note-open-0").is_some());
    assert!(visual.debug_bounds("board-next-stage").is_some());
    assert!(visual.debug_bounds("board-note-card-next").is_none());
    let draft_before = reader.read_with(visual, |reader, cx| {
        reader
            .project
            .as_ref()
            .unwrap()
            .read(cx)
            .compose
            .read(cx)
            .value()
            .to_string()
    });
    let discuss = visual.debug_bounds("board-note-discuss-0").unwrap();
    visual.simulate_click(discuss.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        let draft = reader
            .project
            .as_ref()
            .unwrap()
            .read(cx)
            .compose
            .read(cx)
            .value();
        #[cfg(native_manager)]
        {
            assert!(draft.starts_with(&draft_before));
            assert!(draft.contains("Choose a safe alternative"));
            assert!(draft.contains("decision.md"));
            assert!(draft.contains("issues/1767"));
            assert!(draft.contains("Note revision:"));
        }
        #[cfg(not(native_manager))]
        assert_eq!(draft.as_str(), draft_before);
    });
    #[cfg(native_manager)]
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        assert_eq!(
            reader
                .project
                .as_ref()
                .unwrap()
                .read(cx)
                .compose
                .read(cx)
                .value()
                .as_str(),
            draft_before
        );
    });
    // Composer Undo retains the cached Board scene; collect fresh selectors.
    visual.update(|window, _| window.refresh());
    visual.run_until_parked();
    let note = visual.debug_bounds("board-waiting-open-0").unwrap();
    visual.simulate_click(note.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, _| {
        assert_eq!(reader.current_rel, "project/decision.md");
        assert!(!reader.project_navigation.board_visible);
    });
    let back = visual.debug_bounds("project-return-board").unwrap();
    visual.simulate_click(back.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        let board = reader.project_navigation.boards[&folder].read(cx);
        assert_eq!(board.filter.executor.as_deref(), Some("Editing"));
        assert_eq!(board.filter.search, "#1767");
        assert_eq!(board.filter.chip, filters::Chip::Oleg);
        assert_eq!(board.selected, Some(1767));
        assert_eq!(
            reader
                .project
                .as_ref()
                .unwrap()
                .read(cx)
                .compose
                .read(cx)
                .value()
                .as_str(),
            draft_before
        );
    });
    let next = visual.debug_bounds("board-next-stage").unwrap();
    visual.simulate_click(next.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-note-card-next").is_some(),
        "positive control: explicit expansion reveals future context"
    );
    for selector in ["board-plan", "board-current-scope"] {
        assert!(visual.debug_bounds(selector).is_none());
    }
}

#[test]
fn healthy_and_initial_board_sources_are_silent() {
    let initial: Source<Vec<()>> = Source::default();
    assert!(source_problem("T3", &initial, 30, 100).is_none());
    let current = Source {
        value: vec![()],
        last_success: Some(100),
        error: None,
    };
    assert!(source_problem("T3", &current, 30, 160).is_none());
    assert!(problem_line(Vec::new()).is_none());
}

#[gpui::test]
fn board_without_registered_agents_does_not_request_a_separate_t3_connection(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let mut binding = Binding::parse(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    for scope in &mut binding.issues {
        scope.agent_thread_id = None;
        scope.current_task = false;
    }
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board =
                Board::with_kanban_reader(area.path().into(), |_| Ok(Vec::new()), window, cx);
            board.binding = Some(binding);
            board.active = true;
            board.refresh(window, cx);
            assert!(board.snapshot.t3.last_success.is_some());
            assert!(board.snapshot.t3.error.is_none());
            assert!(
                board.manager.is_none(),
                "no independent Manager/MCP sign-in is needed"
            );
            board
        });
        Root::new(board, window, cx)
    });
    visual.run_until_parked();
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    board.read_with(visual, |board, _| assert!(board.problem(now()).is_none()));
}

#[test]
fn failed_or_stale_sources_share_one_line_without_claiming_a_new_read() {
    let mut forgejo = Source {
        value: vec![()],
        last_success: Some(100),
        error: None,
    };
    let mut t3 = forgejo.clone();
    t3.accept(Err("internal transport detail\nsecond line".into()), 150);
    let failure = source_problem("T3", &t3, 30, 160).unwrap();
    assert_eq!(failure.1, Some(100));
    assert!(source_problem("Forgejo", &forgejo, 30, 160).is_none());
    let line = problem_line(vec![failure]).unwrap();
    assert!(line.starts_with("T3: internal transport detail second line · data from "));
    assert!(!line.contains('\n'));
    forgejo.accept(Ok(vec![()]), 160);
    let stale = source_problem("T3", &t3, 30, 161).unwrap();
    assert!(source_problem("Forgejo", &forgejo, 30, 161).is_none());
    t3.error = None;
    assert_eq!(
        source_problem("T3", &t3, 30, 161).unwrap().0,
        "T3 out of date"
    );
    let line = problem_line(vec![("Board unavailable".into(), Some(90)), stale]).unwrap();
    assert!(line
        .starts_with("Board unavailable · T3: internal transport detail second line · data from "));
    assert_eq!(t3.last_success, Some(100));
}

#[test]
fn card_type_origin_and_native_icons_come_from_supported_sources() {
    let mut issue: okilum_t3::board::Issue = serde_json::from_value(serde_json::json!({
        "number":1767,"title":"Board","state":"open",
        "labels":[{"name":"fr"},{"name":"from-oleg"},{"name":"from-tester"}]
    }))
    .unwrap();
    assert_eq!(appearance::kind(Some(&issue)).0, "fr");
    assert_eq!(appearance::origin(Some(&issue)), Some("Oleg / tester"));
    issue.labels = vec![okilum_t3::board::IssueLabel { name: "bug".into() }];
    assert_eq!(appearance::kind(Some(&issue)).0, "bug");
    assert_eq!(appearance::origin(Some(&issue)), None);
    assert_eq!(appearance::kind(None).0, "issue");
    for glyph in [
        "bug",
        "sparkles",
        "circle",
        "check",
        "loader-circle",
        "square",
        "circle-play",
        "circle-check",
        "user-round",
        "flask-conical",
        "git-pull-request",
        "chevron-right",
        "chevron-left",
        "list-filter",
        "circle-dot",
    ] {
        assert!(
            okilum_shell::lucide::contains(glyph),
            "missing native glyph {glyph}"
        );
    }
}

#[gpui::test]
fn look_fixed_columns_scroll_inside_center_with_manager_space_and_both_themes(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let mut binding = Binding::parse(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    binding.issues.truncate(5);
    binding.issues[3].stage = Some(okilum_t3::board::HumanStage::Ready);
    binding.issues[4].stage = Some(okilum_t3::board::HumanStage::Verified);
    binding.issues[1].stuck = true;
    binding.issues[0].agent_thread_id = Some("live-agent".into());
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_kanban_reader(
                area.path().into(),
                okilum_t3::board::live::read,
                window,
                cx,
            );
            board.binding = Some(binding.clone());
            board.snapshot.t3.value = vec![serde_json::from_value(serde_json::json!({
                "threadId":"live-agent","title":"native live fixture","status":"running","latestRunId":"run-a",
                "providerInstanceId":"codex","model":"model","runtimeMode":"full-access",
                "updatedAt":chrono::Utc::now().to_rfc3339()
            })).unwrap()];
            board.snapshot.t3.last_success = Some(now());
            board
        });
        entity = Some(board.clone());
        Root::new(board, window, cx)
    });
    let board = entity.unwrap();
    for mode in [
        gpui_component::ThemeMode::Light,
        gpui_component::ThemeMode::Dark,
    ] {
        visual.update(|_, cx| {
            Theme::change(mode, None, cx);
            brand::apply_theme(cx);
            let bug = appearance::type_color("bug", cx);
            let fr = appearance::type_color("fr", cx);
            let task = appearance::type_color("task", cx);
            assert_ne!(bug, fr);
            assert_ne!(fr, task);
        });
        // Available center widths after a normal Manager panel is opened.
        for width in [900., 600., 420.] {
            visual.simulate_resize(size(px(width), px(1000.)));
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("board-attention").is_none(),
                "empty list has no lane"
            );
            let center = visual.debug_bounds("project-board").unwrap();
            let columns = visual.debug_bounds("board-columns").unwrap();
            let filter = visual
                .debug_bounds("board-filter-bar")
                .expect("native filter bar");
            assert!(filter.left() >= center.left() && filter.right() <= center.right());
            assert!(
                columns.top() >= filter.bottom(),
                "filters precede either presentation"
            );
            assert!(visual.debug_bounds("board-refresh").is_none());
            assert!(visual.debug_bounds("board-plan").is_none());

            for key in [
                "board-lane-Queue",
                "board-lane-Working",
                "board-lane-Checks",
                "board-lane-Ready",
            ] {
                let lane = visual.debug_bounds(key).expect("actual native lane");
                assert!(lane.size.width > px(0.));
                assert_eq!(
                    lane.size.width,
                    px(if key.ends_with("Queue") { 240. } else { 268. })
                );
            }
            assert!(columns.left() >= center.left() && columns.right() <= center.right());
            assert!(visual.debug_bounds("board-lane-Verified").is_none());
            let toggle = visual.debug_bounds("board-verified-toggle").unwrap();
            visual.simulate_click(toggle.center(), Modifiers::default());
            visual.run_until_parked();
            let lane = visual
                .debug_bounds("board-lane-Verified")
                .expect("explicit verified result expands");
            assert_eq!(lane.size.width, px(268.));
            let toggle = visual.debug_bounds("board-verified-toggle").unwrap();
            visual.simulate_click(toggle.center(), Modifiers::default());
            visual.run_until_parked();
            board.read_with(visual, |board, _| assert!(!board.verified_open));
        }
    }
}

#[gpui::test]
fn delivery_summary_and_exact_evidence_do_not_turn_legacy_stage_into_acceptance(
    cx: &mut TestAppContext,
) {
    use okilum_t3::board::delivery as d;
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    std::fs::write(area.path().join("scenario.md"), "# Exact Board scenario\n").unwrap();
    let mut binding = Binding::parse(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    binding.issues.truncate(1);
    binding.issues[0].pull_requests = vec![42];
    binding.issues[0].stage = Some(okilum_t3::board::HumanStage::Verified);
    let number = binding.issues[0].number;
    let source = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let platform = if cfg!(target_os = "macos") {
        d::Platform::Macos
    } else if cfg!(target_os = "windows") {
        d::Platform::Windows
    } else {
        d::Platform::Linux
    };
    let build = d::Build {
        platform,
        channel: d::Channel::Beta,
        build: "100".into(),
        source: source.into(),
        artifact: "https://updates.example/exact-build.zip".into(),
        published_at: Some("2026-10-31T12:00:00Z".into()),
        release_url: Some("https://updates.example/releases/100".into()),
    };
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut b = Board::with_kanban_reader(
                area.path().into(),
                okilum_t3::board::live::read,
                window,
                cx,
            );
            b.binding = Some(binding.clone());
            assert_eq!(
                b.delivery_column(&binding.issues[0]),
                Column::Verified,
                "positive control: old explicit-stage projection"
            );
            b.delivery.accept(
                d::Update {
                    config: Ok(Some(d::Config {
                        version: 1,
                        feeds: vec![d::Feed {
                            platform,
                            channel: d::Channel::Beta,
                            url: "https://updates.example/feed.json".into(),
                            format: d::Format::Receipt,
                            metadata: None,
                            artifact_name: None,
                            next: "Try this exact scenario".into(),
                            blocker: None,
                        }],
                        evidence: vec!["scenario.md".into()],
                    })),
                    feeds: vec![((platform, d::Channel::Beta), Ok(Some(build.clone())))],
                    artifacts: vec![(build.artifact.clone(), Ok(d::Artifact { available: true }))],
                    ci: vec![(
                        42,
                        Ok(d::Ci {
                            pr: 42,
                            source: source.into(),
                            state: "success".into(),
                            links: vec![],
                            inclusion: std::collections::BTreeMap::from([(
                                source.into(),
                                Ok(true),
                            )]),
                        }),
                    )],
                    evidence: vec![("scenario.md".into(), Ok(Vec::new()))],
                },
                now(),
            );
            b.selected = Some(number);
            b.peek.open = true;
            assert_ne!(
                b.delivery_column(&binding.issues[0]),
                Column::Verified,
                "legacy flag cannot bypass exact evidence"
            );
            b
        });
        entity = Some(board.clone());
        Root::new(cx.new(|cx| peek::Harness::new(board, cx)), window, cx)
    });
    let board = entity.unwrap();
    visual.simulate_resize(size(px(1000.), px(1000.)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-delivery").is_some());
    assert!(visual.debug_bounds("board-delivery-details").is_some());
    assert!(visual.debug_bounds("board-delivery-line").is_some());
    assert!(visual.debug_bounds("board-delivery-details-line").is_some());
    assert!(visual.debug_bounds("delivery-problem-Mac").is_none());
    assert!(visual
        .debug_bounds("board-delivery-details-problem-Mac")
        .is_none());
    for id in ["delivery-problem-Mac-Stable", "delivery-problem-Mac-Beta"] {
        assert!(
            visual.debug_bounds(id).is_none(),
            "no duplicate channel indicators"
        );
    }
    for id in [
        "board-delivery-details-Mac-Stable",
        "board-delivery-details-Mac-Beta",
        "board-delivery-details-Windows-Stable",
        "board-delivery-details-Windows-Beta",
        "board-delivery-details-Linux-Stable",
        "board-delivery-details-Linux-Beta",
    ] {
        let expected = id == format!("board-delivery-details-{}-Beta", platform.label());
        assert_eq!(
            visual.debug_bounds(id).is_some(),
            expected,
            "issue panel delivery control: {id}"
        );
    }
    for id in [
        "board-delivery-Mac-Stable",
        "board-delivery-Mac-Beta",
        "board-delivery-Windows-Stable",
        "board-delivery-Windows-Beta",
        "board-delivery-Linux-Stable",
        "board-delivery-Linux-Beta",
    ] {
        let expected = id == format!("board-delivery-{}-Beta", platform.label());
        assert_eq!(visual.debug_bounds(id).is_some(), expected, "visible {id}");
    }
    for id in [
        "board-delivery-Mac-Sprint",
        "board-delivery-Windows-Sprint",
        "board-delivery-Linux-Sprint",
    ] {
        assert!(
            visual.debug_bounds(id).is_none(),
            "Sprint was removed: {id}"
        );
    }
    assert!(visual.debug_bounds("board-refresh").is_none());
    assert!(visual.debug_bounds("board-plan").is_none());
    board.update(visual, |board, cx| {
        board
            .delivery
            .evidence
            .get_mut("scenario.md")
            .unwrap()
            .value = vec![d::Evidence {
            issue: number,
            pr: 42,
            platform,
            channel: d::Channel::Beta,
            build: "100".into(),
            source: source.into(),
            artifact: build.artifact.clone(),
            scenario: "Open this exact Board build".into(),
            installed_at: Some("2026-10-30T12:00:00Z".into()),
            accepted_by: None,
            accepted_at: None,
            note: "scenario.md".into(),
        }];
        assert_eq!(board.delivery_column(&binding.issues[0]), Column::Ready);
        cx.notify();
    });
    visual.run_until_parked();
    let target = match platform {
        d::Platform::Macos => "board-delivery-Mac-Beta",
        d::Platform::Windows => "board-delivery-Windows-Beta",
        d::Platform::Linux => "board-delivery-Linux-Beta",
    };
    let button = visual
        .debug_bounds(target)
        .expect("real exact-build summary button");
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        visual.opened_url().as_deref(),
        build.release_url.as_deref(),
        "only explicit release open, no install/dispatch"
    );
    board.read_with(visual, |board, _| {
        assert!(board.delivery.ci[&42].value.state == "success");
        assert_eq!(
            board.delivery.evidence["scenario.md"].value[0].accepted_by, None,
            "opening never accepts"
        );
    });
}

#[gpui::test]
fn project_note_try_without_build_evidence_does_not_add_unselected_issues_or_stage_ui(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(area.path().join("ops")).unwrap();
    std::fs::write(
        area.path().join("ops/board-context.toml"),
        "version=1\nnotes=['try.md']\n",
    )
    .unwrap();
    std::fs::write(area.path().join("try.md"), "---\nboard_context:\n  kind: try\n  title: Try an unselected result\n  reason: Await exact build evidence\n  consequence: No readiness is implied\n  issue: 99999\n  platform: linux\n  channel: sprint\n---\nAn explicit scenario, without publication evidence.\n").unwrap();
    let binding = Binding::parse(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board =
                Board::with_kanban_reader(area.path().into(), |_| Ok(Vec::new()), window, cx);
            board
                .project_notes
                .accept(okilum_t3::board::context::refresh(area.path()), now());
            board.binding = Some(binding);
            board.notes_expanded = true;
            board.refresh_waiting();
            board
        });
        Root::new(board, window, cx)
    });
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-note-card-0").is_some());
    assert!(visual.debug_bounds("board-note-try-state-0").is_some());
    assert!(visual.debug_bounds("board-note-open-0").is_some());
    assert!(
        visual.debug_bounds("board-expand-1762").is_some(),
        "positive control: real selected issues still render"
    );
    assert!(visual.debug_bounds("board-expand-99999").is_none());
    assert!(visual.debug_bounds("board-next-stage").is_none());
    board.read_with(visual, |board, _| {
        assert!(!board
            .binding
            .as_ref()
            .unwrap()
            .issues
            .iter()
            .any(|s| s.number == 99999));
    });
}

#[gpui::test]
fn release_table_stays_compact_at_normal_and_wide_widths_in_both_themes(cx: &mut TestAppContext) {
    use okilum_t3::board::delivery as d;
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let binding = Binding::parse(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    let mut feeds = Vec::new();
    let mut builds = Vec::new();
    for channel in [d::Channel::Stable, d::Channel::Beta] {
        for platform in d::Platform::ALL {
            feeds.push(d::Feed {
                platform,
                channel,
                url: "https://updates.example/feed.json".into(),
                format: d::Format::Receipt,
                metadata: None,
                artifact_name: None,
                next: String::new(),
                blocker: None,
            });
            builds.push((
                (platform, channel),
                Ok(Some(d::Build {
                    platform,
                    channel,
                    build: "22231".into(),
                    source: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                    artifact: "https://updates.example/build.zip".into(),
                    published_at: Some("2026-10-31T12:00:00Z".into()),
                    release_url: Some("https://updates.example/releases/22231".into()),
                })),
            ));
        }
    }
    let (_, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_kanban_reader(
                area.path().into(),
                okilum_t3::board::live::read,
                window,
                cx,
            );
            board.binding = Some(binding);
            board.delivery.accept(
                d::Update {
                    config: Ok(Some(d::Config {
                        version: 1,
                        feeds,
                        evidence: vec![],
                    })),
                    feeds: builds,
                    artifacts: vec![(
                        "https://updates.example/build.zip".into(),
                        Ok(d::Artifact { available: true }),
                    )],
                    ci: vec![],
                    evidence: vec![],
                },
                now(),
            );
            board
        });
        Root::new(board, window, cx)
    });
    for mode in [
        gpui_component::ThemeMode::Light,
        gpui_component::ThemeMode::Dark,
    ] {
        visual.update(|_, cx| {
            Theme::change(mode, None, cx);
            brand::apply_theme(cx);
        });
        let mut normal_width = None;
        for width in [800., 1600.] {
            visual.simulate_resize(size(px(width), px(800.)));
            visual.run_until_parked();
            let line = visual
                .debug_bounds("board-delivery-line")
                .expect("delivery line");
            let mut platform_rows = std::collections::HashMap::new();
            for id in [
                "board-delivery-Mac-Stable",
                "board-delivery-Windows-Stable",
                "board-delivery-Linux-Stable",
                "board-delivery-Mac-Beta",
                "board-delivery-Windows-Beta",
                "board-delivery-Linux-Beta",
            ] {
                let button = visual.debug_bounds(id).expect("publication control");
                assert_eq!(
                    *platform_rows
                        .entry(id.split('-').nth(2).unwrap())
                        .or_insert(button.origin.y),
                    button.origin.y,
                    "channels share their platform row: {id}"
                );
                assert!(
                    button.right() <= line.right(),
                    "visible without horizontal scrolling at normal Board width: {id}"
                );
            }
            assert_eq!(platform_rows.len(), 3, "all platform rows are present");
            assert!(platform_rows["Mac"] < platform_rows["Windows"]);
            assert!(platform_rows["Windows"] < platform_rows["Linux"]);
            for id in [
                "board-delivery-Mac-Sprint",
                "board-delivery-Windows-Sprint",
                "board-delivery-Linux-Sprint",
            ] {
                assert!(visual.debug_bounds(id).is_none(), "Sprint remains absent");
            }
            let stable = visual.debug_bounds("board-delivery-Mac-Stable").unwrap();
            let beta = visual.debug_bounds("board-delivery-Mac-Beta").unwrap();
            assert!(
                beta.left() - stable.right() <= px(24.),
                "channel cells stay adjacent at {width}px in {mode:?}"
            );
            assert!(
                line.size.width <= px(640.),
                "delivery uses its content width at {width}px in {mode:?}: {line:?}"
            );
            let content_right = visual
                .debug_bounds("board-delivery-Linux-Beta")
                .unwrap()
                .right();
            assert!(
                line.right() - content_right <= px(8.),
                "no full-window trailing space"
            );
            if let Some(normal) = normal_width {
                assert!(
                    (line.size.width - normal).abs() <= px(1.),
                    "widening the window must not stretch the table"
                );
            } else {
                normal_width = Some(line.size.width);
            }
        }
    }
}

#[gpui::test]
fn peek_2531_live_board_retains_expiring_merge_and_visible_diagnostic_until_successful_recovery(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let mut binding = Binding::parse(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    binding.issues.clear();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_kanban_reader(
                area.path().into(),
                |_| panic!("hidden fixture must not access the network"),
                window,
                cx,
            );
            board.binding = Some(binding.clone());
            let mut issue: okilum_t3::board::Issue = serde_json::from_value(serde_json::json!({
                "number": 42, "title": "Delivered change", "state": "closed"
            }))
            .unwrap();
            issue.live_pulls.push(okilum_t3::board::live::Pull {
                title: String::new(),
                state: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
                number: 7,
                merged_at: Some(1),
                checks: String::new(),
            });
            board.list.invalidate();
            board.snapshot.forgejo.accept(Ok([(42, issue)].into()), 100);
            board.snapshot.forgejo.error = Some("HTTP 503; showing last good Board".into());
            board
        });
        Root::new(cx.new(|cx| peek::Harness::new(board, cx)), window, cx)
    });
    let board = root.read_with(visual, |root, cx| {
        root.view()
            .clone()
            .downcast::<peek::Harness>()
            .unwrap()
            .read(cx)
            .board
            .clone()
    });
    visual.simulate_resize(size(px(1200.), px(1000.)));
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-expand-42").is_some(),
        "offline retains the last complete membership even beyond the merge expiry"
    );
    assert!(
        visual.debug_bounds("board-mapping-42").is_none(),
        "compact summaries keep exactly three rows"
    );
    let card = visual.debug_bounds("board-expand-42").unwrap();
    visual.simulate_click(card.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-mapping-42").is_some(),
        "generated-card mapping diagnostics remain reachable in the issue dock"
    );
    assert!(visual.debug_bounds("board-peek-content").is_some());
    assert!(visual.debug_bounds("board-kanban-details").is_none());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-live-status").is_some(),
        "source failure has a quiet inline status, not only a popover"
    );
    assert!(visual.debug_bounds("board-peek-content").is_none());
    assert!(visual.debug_bounds("board-mapping-42").is_none());
    let center = visual.debug_bounds("project-board").unwrap();
    let status = visual.debug_bounds("board-live-status").unwrap();
    let columns = visual.debug_bounds("board-columns").unwrap();
    assert!(
        columns.top() >= visual.debug_bounds("board-filter-bar").unwrap().bottom(),
        "source error leaves the filter bar in place"
    );
    assert!(
        status.bottom() <= center.bottom() && status.top() >= center.bottom() - px(32.),
        "quiet source line remains at the visible lower edge"
    );

    board.update(visual, |board, cx| {
        assert_eq!(board.projection_time(), 100);
        assert!(board.problem(now()).unwrap().contains("HTTP 503"));
        board.snapshot.forgejo.error = None;
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-expand-42").is_none(),
        "positive control: a healthy read permits the expired merge to leave"
    );
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn cached_live_agent_cards_ignore_composer_notifications_and_keep_source_publications(
    cx: &mut TestAppContext,
) {
    cached_live_agent_publications(cx, list::Mode::Board, false);
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn cached_live_list_rows_ignore_composer_notifications_and_keep_source_publications(
    cx: &mut TestAppContext,
) {
    cached_live_agent_publications(cx, list::Mode::List, false);
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn peek_2531_cached_live_panel_ignores_composer_and_keeps_progress(cx: &mut TestAppContext) {
    cached_live_agent_publications(cx, list::Mode::Board, true);
}

#[cfg(target_os = "linux")]
fn cached_live_agent_publications(cx: &mut TestAppContext, mode: list::Mode, peek: bool) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    struct Host {
        board: Entity<Board>,
        manager: Entity<ProjectView>,
        peek: bool,
        renders: usize,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders += 1;
            let mut view = h_flex().size_full().child(
                div().flex_1().min_w_0().h_full().child(
                    self.board
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                ),
            );
            if self.peek {
                let dock = self.board.update(cx, |board, cx| {
                    board.render_peek_dock(div().into_any_element(), cx)
                });
                view = view.child(div().w(px(420.)).h_full().flex_none().child(dock));
            }
            view
        }
    }
    let (root, visual) = cx.add_window_view(|window, cx| {
        let manager = cx.new(|cx| {
            crate::project::fixture::view(
                temp.path().join("manager"),
                Arc::new(crate::project::fixture::Fake::default()),
                window,
                cx,
            )
        });
        manager.update(cx, |manager, _| {
            manager.generation += 1;
            manager.polling = true;
        });
        let board = cx.new(|cx| Board::performance_fixture(temp.path().join("board"), window, cx));
        Root::new(
            cx.new(|cx| {
                if peek {
                    cx.observe(&board, |_, _, cx| cx.notify()).detach();
                }
                Host {
                    board,
                    manager,
                    peek,
                    renders: 0,
                }
            }),
            window,
            cx,
        )
    });
    visual.run_until_parked();
    let (board, manager) = root.read_with(visual, |root, cx| {
        let host = root.view().clone().downcast::<Host>().unwrap();
        (host.read(cx).board.clone(), host.read(cx).manager.clone())
    });
    let thread_id = manager.update(visual, |manager, _| {
        manager.identity = Some(ServerIdentity { environment_id:"fixture".into(), label:"Fixture".into() });
        let chat = manager.chat.as_mut().unwrap();
        chat.items.insert(1, serde_json::from_value(serde_json::json!({
            "position":1,"itemId":"progress","runId":"run-live","type":"assistant_message","status":"completed","text":"First live progress\nDetails","updatedAt":"now"
        })).unwrap());
        chat.thread_id.clone()
    });
    let renders = board.read_with(visual, |board, _| board.render_calls.get());
    board.update_in(visual, |board, window, cx| {
        board.active = false;
        board.list.preferences.mode = mode;
        board.list.preferences.collapsed = [false; 5];
        if mode == list::Mode::List {
            // List groups Working before Queue; narrow the real filter so the
            // bound Working agent is visible and the dependency control fires.
            board.filter.search = "#2".into();
        }
        board.set_manager(manager.clone(), window, cx);
        let scope = &mut board.binding.as_mut().unwrap().issues[1];
        scope.current_task = true;
        scope.agent_thread_id = Some(thread_id.clone());
        scope.execution = Some(okilum_t3::board::Execution { thread_id:thread_id.clone(), run_id:"run-live".into() });
        board.snapshot.t3.value = vec![serde_json::from_value(serde_json::json!({
            "threadId":thread_id,"title":"Fixture","status":"running","activeRunId":"run-live","latestRunId":"run-live","providerInstanceId":"codex","model":"model","runtimeMode":"full-access","updatedAt":chrono::Utc::now().to_rfc3339()
        })).unwrap()];
        board.snapshot.t3.last_success = Some(now());
        cx.notify();
    });
    visual.run_until_parked();
    board.update(visual, |board, _| {
        assert!(board.render_calls.get() > renders,
            "positive control: source publication reached the cached Board before refreshing selectors");
        let projection = board.filtered_projection();
        let scope = projection.binding.issues.iter().find(|scope| scope.number == 2).unwrap();
        assert_eq!(scope.agent_thread_id.as_deref(), Some(thread_id.as_str()));
        assert_eq!(board.delivery_column(scope), Column::Working,
            "positive control: bind an already visible Working/PR card");
    });
    // A later cached paint can retain the native scene without replaying debug
    // selectors. The publication counter above must already have advanced.
    visual.update(|window, _| window.refresh());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-agent-link-2").is_some(),
        "positive control: real agent renderer reads progress"
    );
    let agent = visual.debug_bounds("board-agent-link-2").unwrap();
    visual.simulate_click(agent.center(), Modifiers::default());
    assert_eq!(
        visual.opened_url(),
        Some(
            ServerIdentity {
                environment_id: "fixture".into(),
                label: "Fixture".into()
            }
            .thread_url(&thread_id)
        ),
        "rich agent controls retain their explicit source URL"
    );
    assert_eq!(board.read_with(visual, |board, _| board.selected), None);
    if peek {
        board.update_in(visual, |board, window, cx| {
            board.selected = Some(2);
            board.open_peek(window, cx);
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("board-peek-agent").is_some(),
            "positive control: cached progress is rendered in the issue dock"
        );
    }
    let dock_renders = root.read_with(visual, |root, cx| {
        root.view()
            .clone()
            .downcast::<Host>()
            .unwrap()
            .read(cx)
            .renders
    });
    board.read_with(visual, |board, _| {
        board.render_calls.set(0);
        board.card_calls.set(0);
    });
    manager.update_in(visual, |manager, window, cx| {
        manager.compose.update(cx, |input, cx| {
            input.set_value("Keep the draft", window, cx)
        });
        cx.notify();
    });
    visual.run_until_parked();
    assert_eq!(
        board.read_with(visual, |board, _| board.render_calls.get()),
        0,
        "agent cards must not depend on the whole Manager entity"
    );
    assert_eq!(
        root.read_with(visual, |root, cx| root
            .view()
            .clone()
            .downcast::<Host>()
            .unwrap()
            .read(cx)
            .renders),
        dock_renders,
        "issue dock must not subscribe to composer notifications through the Manager entity"
    );
    manager.update(visual, |manager, cx| {
        manager
            .chat
            .as_mut()
            .unwrap()
            .items
            .get_mut(&1)
            .unwrap()
            .text = Some("Updated live progress\nDetails".into());
        manager.feed.update(cx, |feed, cx| feed.reset(cx));
        // Deliberately no parent notify: a reply may publish only Feed.
    });
    visual.run_until_parked();
    board.read_with(visual, |board, _| {
        assert!(board.render_calls.get() > 0);
        assert_eq!(
            board
                .manager_display
                .progress(
                    &board.binding.as_ref().unwrap().issues[1],
                    &board.snapshot.t3.value[0]
                )
                .as_deref(),
            Some("Updated live progress")
        );
    });
    if peek {
        assert!(
            root.read_with(visual, |root, cx| root
                .view()
                .clone()
                .downcast::<Host>()
                .unwrap()
                .read(cx)
                .renders)
                > dock_renders,
            "positive control: real Feed progress publication also refreshes the issue dock"
        );
    }
    assert_eq!(
        manager
            .read_with(visual, |manager, cx| manager.compose.read(cx).value())
            .as_str(),
        "Keep the draft"
    );
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn compact_pull_badge_opens_its_bound_pull_without_selecting_the_card(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| Board::performance_fixture(temp.path().join("board"), window, cx));
        Root::new(board, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(800.)));
    visual.run_until_parked();
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    let expected = board.update(visual, |board, _| {
        board.filtered_projection().binding.pull_url(1002)
    });
    let badge = visual
        .debug_bounds("board-pr-badge-2")
        .expect("real live PR badge");
    assert!(badge.intersects(&visual.debug_bounds("project-board").unwrap()));
    assert_ne!(visual.opened_url().as_deref(), Some(expected.as_str()));
    visual.simulate_click(badge.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(visual.opened_url().as_deref(), Some(expected.as_str()));
    board.read_with(visual, |board, _| assert_eq!(board.selected, None));
}

#[gpui::test]
fn service2772_metadata_invalidates_all_pending_board_sources(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    cx.update(reader_features::enable_test_modules);
    let area = tempfile::tempdir().unwrap();
    std::fs::create_dir(area.path().join("ops")).unwrap();
    std::fs::write(
        area.path().join("ops/project.yaml"),
        "name: Synthetic\nkind: knowledge\n",
    )
    .unwrap();
    let (board, visual) = cx.add_window_view(|window, cx| {
        Board::with_reader(area.path().into(), |_| Ok(Vec::new()), window, cx)
    });
    board.update_in(visual, |board, window, cx| {
        let old = board.snapshot.begin().unwrap();
        board.notes_loading = true;
        board.delivery_loading = true;
        board.service_metadata_changed(window, cx);
        assert!(
            !board.snapshot.matches(&old),
            "old completion must be rejected"
        );
        assert!(
            !board.notes_loading && !board.delivery_loading,
            "rejected old tickets cannot latch loading forever"
        );
        let current = board.snapshot.begin().unwrap();
        assert!(board.snapshot.matches(&current));
        board.refresh_project_notes(current, window, cx);
        assert!(
            board.notes_loading,
            "replacement note read must actually start"
        );
    });
    visual.run_until_parked();
}
