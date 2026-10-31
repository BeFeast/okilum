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
            let mut board = Board::with_reader(
                area.path().into(),
                okilum_t3::board::read_issues,
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
    assert!(visual.debug_bounds("board-expand-1733").is_some());
    assert!(visual.debug_bounds("board-expand-1731").is_none());
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
    assert!(visual.debug_bounds("board-source-problem").is_some());
    assert!(visual.debug_bounds("board-expand-1733").is_some());
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
fn board_context_appends_to_the_actual_composer_and_never_sends(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let manager = cx.new(|cx| {
            let mut view =
                super::super::fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
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
    manager.update_in(visual, |view, window, cx| {
        assert!(view.append_board_context(&area, "Board context #1767".into(), window, cx));
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Keep my draft 📝\n\n---\nBoard context #1767"
        );
        assert!(!view.append_board_context(
            &area.join("different-project"),
            "WRONG PROJECT".into(),
            window,
            cx
        ));
    });
    assert!(gateway.sends.lock().unwrap().is_empty());
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
            Board::with_reader(
                area.path().to_owned(),
                okilum_t3::board::read_issues,
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
        board.filter = Some("Editing".into());
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
                    state: "closed".into(),
                    body: String::new(),
                    labels: Vec::new(),
                    open_pull_requests: Vec::new(),
                }),
            )],
            1,
        );
        assert!(board.snapshot.forgejo.value.is_empty());
        assert_eq!(board.filter.as_deref(), Some("Editing"));
        assert_eq!(board.selected, Some(1767));
        assert!(board.ticker.is_none());
        assert_eq!(board.scroll.offset().y, px(-120.));
    });
}

#[gpui::test]
fn real_reader_note_and_board_roundtrip_preserves_navigation_and_manager_draft(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("vault");
    let folder = vault.join("project");
    std::fs::create_dir_all(folder.join("ops")).unwrap();
    std::fs::write(vault.join("start.md"), "# Start\n").unwrap();
    std::fs::write(folder.join("note.md"), "# Related note\nActual body\n").unwrap();
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
    visual.run_until_parked();
    reader.update_in(visual, |reader, window, cx| {
        reader.ensure_project_board_using(
            folder.clone(),
            |binding| {
                binding
                    .issues
                    .iter()
                    .map(|s| {
                        (
                            s.number,
                            Ok(okilum_t3::board::Issue {
                                number: s.number,
                                title: format!("Fixture issue {}", s.number),
                                state: "open".into(),
                                body: String::new(),
                                labels: Vec::new(),
                                open_pull_requests: Vec::new(),
                            }),
                        )
                    })
                    .collect()
            },
            window,
            cx,
        );
        reader.enter_vault_project(project.clone(), window, cx);
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-board").is_some());
    assert!(visual.debug_bounds("board-attention").is_some());
    reader.update_in(visual, |reader, window, cx| {
        assert!(reader.project_navigation.board_visible);
        let manager = reader.project.clone().expect("actual native Manager");
        manager.update(cx, |manager, cx| {
            manager
                .compose
                .update(cx, |input, cx| input.set_value("Keep my draft", window, cx))
        });
        reader.project_navigation.boards[&folder].update(cx, |board, cx| {
            board.filter = Some("Editing".into());
            cx.notify();
        });
    });
    visual.run_until_parked();
    // Real Reader integration: opening the existing Manager must not push the
    // first or last Board lane beyond the center at a compact window width.
    reader.read_with(visual, |reader, _| {
        assert!(reader.project_navigation.manager_panel.is_some());
    });
    for width in [1100., 1400.] {
        visual.simulate_resize(size(px(width), px(1000.)));
        visual.run_until_parked();
        let center = visual.debug_bounds("project-board").unwrap();
        for key in ["board-lane-Queue", "board-lane-Ready"] {
            let lane = visual.debug_bounds(key).unwrap();
            assert!(lane.left() >= center.left() && lane.right() <= center.right());
        }
    }
    // Compact cards expose only the summary; links and notes need expansion.
    assert!(visual.debug_bounds("board-plan").is_none());
    assert!(visual.debug_bounds("board-discuss-1767").is_none());
    assert!(visual.debug_bounds("board-agent-1767").is_some());
    assert!(visual.debug_bounds("board-state-1767").is_some());
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
        visual.debug_bounds("board-note-1767-1").is_none(),
        "Stage note alias is not a Note button"
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
        assert!(value.starts_with("Keep my draft"));
        assert!(value.contains("issues/1767"));
    });
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
    let back = visual
        .debug_bounds("project-return-board")
        .expect("one-action Board return");
    visual.simulate_click(back.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        assert!(reader.project_navigation.board_visible);
        let board = reader.project_navigation.boards[&folder].read(cx);
        assert_eq!(board.filter.as_deref(), Some("Editing"));
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
            let mut board = Board::with_reader(area.path().into(), |_| Vec::new(), window, cx);
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
fn look_columns_fit_the_center_with_manager_space_and_both_themes(cx: &mut TestAppContext) {
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
            let mut board = Board::with_reader(
                area.path().into(),
                okilum_t3::board::read_issues,
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
            assert!(
                visual.debug_bounds("board-agent-link-1731").is_some(),
                "real bound-agent renderer"
            );
            let center = visual.debug_bounds("project-board").unwrap();
            let columns = visual.debug_bounds("board-columns").unwrap();
            assert!(
                columns.top() <= center.top() + px(20.),
                "no header block above columns"
            );
            let filter = visual
                .debug_bounds("board-agent-filter")
                .expect("compact agent control");
            assert!(filter.size.width <= px(36.));
            assert!(filter.top() >= columns.top() && filter.top() <= columns.top() + px(4.));
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
                assert!(
                    lane.left() >= center.left() && lane.right() <= center.right(),
                    "{key} clipped at {width}"
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
            assert!(lane.left() >= center.left() && lane.right() <= center.right());
            let toggle = visual.debug_bounds("board-verified-toggle").unwrap();
            visual.simulate_click(toggle.center(), Modifiers::default());
            visual.run_until_parked();
            board.read_with(visual, |board, _| assert!(!board.verified_open));
        }
    }
}
