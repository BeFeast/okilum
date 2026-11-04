use super::*;
use ::core::prelude::v1::test;

fn fixture_document(count: usize) -> String {
    serde_json::json!({
        "version": 1,
        "title": "Synthetic tasks",
        "repository": "https://example.test/team/project",
        "issues": (1..=count).map(|n| serde_json::json!({
            "number": n,
            "expected": format!("Synthetic task {n}: keep source identity and verify the next delivery"),
            "executor": "Fixture",
            "notes": ["note.md"]
        })).collect::<Vec<_>>()
    }).to_string()
}
fn fixture(count: usize) -> Binding {
    Binding::parse(&fixture_document(count)).unwrap()
}
fn open<'a>(
    cx: &'a mut TestAppContext,
    area: &Path,
    binding: Binding,
) -> (Entity<Board>, &'a mut VisualTestContext) {
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_reader(area.into(), |_| Ok(Vec::new()), window, cx);
            let at = now();
            board.snapshot.forgejo.value = binding.issues.iter().map(|scope| {
                let issue = serde_json::from_value(serde_json::json!({
                    "number":scope.number,"title":scope.expected,"state":"open",
                    "updated_at":chrono::DateTime::from_timestamp((at-scope.number*60) as i64,0).unwrap().to_rfc3339()
                })).unwrap();
                (scope.number, issue)
            }).collect();
            board.snapshot.forgejo.last_success = Some(at);
            board.binding = Some(binding);
            board
        });
        Root::new(board, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(800.)));
    visual.run_until_parked();
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    (board, visual)
}
fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}
#[gpui::test]
fn list_2530_default_and_groups_persist_per_project(cx: &mut TestAppContext) {
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_global(reader_history::TestSessionDirectory(state.path().into()));
    });
    let area = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path(), fixture(3));
    board.read_with(visual, |board, _| {
        assert_eq!(board.list.preferences.mode, Mode::List);
        let p = board.list.prepared.as_ref().unwrap();
        assert_eq!(p.counts, [3, 0, 0, 0, 0]);
        assert_eq!(p.rows.len(), 5);
        assert!(matches!(p.rows[0], Row::Group(Column::Working)));
        assert!(matches!(p.rows[3], Row::Group(Column::Queue)));
    });
    assert!(visual.debug_bounds("board-columns").is_none());
    assert!(visual.debug_bounds("board-list-row-1").is_none());
    click(visual, "board-list-group-Queue");
    let row = visual.debug_bounds("board-list-row-1").unwrap();
    assert_eq!(row.size.height, px(38.));
    click(visual, "board-list-group-Working");
    board.update_in(visual, |board, window, cx| {
        board.selected = Some(1);
        board.focus.focus(window, cx);
        cx.notify();
    });
    // A selected issue must not steal Enter from the focused view control.
    visual.simulate_keystrokes("tab tab");
    let keystroke = gpui::Keystroke::parse("enter").unwrap();
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(gpui::KeyUpEvent { keystroke });
    visual.run_until_parked();
    board.read_with(visual, |board, _| assert!(!board.list.details_open));
    assert!(visual.debug_bounds("board-columns").is_some());
    board.read_with(visual, |board, cx| {
        let restored = State::load(area.path(), cx);
        assert_eq!(restored.preferences.mode, Mode::Board);
        assert!(!restored.preferences.collapsed[Column::Queue as usize]);
        assert!(restored.preferences.collapsed[Column::Working as usize]);
        let independent = State::load(other.path(), cx);
        assert_eq!(independent.preferences.mode, Mode::List);
        assert!(independent.preferences.collapsed[Column::Queue as usize]);
    });
    click(visual, "board-view-list");
    assert!(visual.debug_bounds("board-list-row-1").is_some());
    assert!(visual.debug_bounds("board-columns").is_none());
}
#[gpui::test]
fn list_2530_virtualises_240_rows_and_keyboard_reaches_tail_without_reprojection(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path(), fixture(240));
    click(visual, "board-list-group-Queue");
    assert!(visual.debug_bounds("board-list-row-1").is_some());
    assert!(visual.debug_bounds("board-list-row-240").is_none());
    board.update_in(visual, |board, window, cx| {
        board.focus.focus(window, cx);
        board.selected = Some(239);
        board.list.rendered_rows = 0;
        cx.notify();
    });
    visual.run_until_parked();
    let before = board.read_with(visual, |b, _| b.list.publications);
    // Arrow navigation must take focus back from a toolbar control, so Enter
    // opens the selected row rather than activating the old focused button.
    visual.simulate_keystrokes("tab down");
    visual.run_until_parked();
    board.update_in(visual, |board, window, _| {
        assert!(board.focus.is_focused(window))
    });
    assert!(visual.debug_bounds("board-list-row-240").is_some());
    assert!(visual.debug_bounds("board-list-row-1").is_none());
    board.read_with(visual, |board, _| {
        assert_eq!(board.selected, Some(240));
        assert_eq!(board.list.publications, before);
        assert!(
            board.list.rendered_rows > 0,
            "positive control: the viewport rendered rows"
        );
        assert!(
            board.list.rendered_rows < 160,
            "viewport work must not build all 240 rows"
        );
    });
    visual.simulate_keystrokes("up enter");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-list-details").is_some());
    assert!(visual.debug_bounds("board-note-239-0").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-list-details").is_none());
    board.read_with(visual, |board, _| assert_eq!(board.selected, Some(239)));
}
#[test]
fn list_2530_source_time_does_not_invent_age_and_ci_type_is_theme_role() {
    assert_eq!(
        row_bits::relative_time("1970-01-01T00:00:00Z", 3600).as_deref(),
        Some("1 ч")
    );
    assert!(row_bits::relative_time("invalid", 3600).is_none());
    assert!(row_bits::relative_time("1970-01-02T00:00:00Z", 3600).is_none());
    let issue = serde_json::from_value(serde_json::json!({"number":1,"title":"CI","state":"open","updated_at":"1970-01-01T00:00:00Z","labels":[{"name":"ci"}]})).unwrap();
    assert_eq!(appearance::kind(Some(&issue)).0, "CI");
    assert_eq!(issue.updated_at, "1970-01-01T00:00:00Z");
}

#[gpui::test]
async fn list_2530_real_reader_note_roundtrip_retains_selection_and_scrolled_viewport(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    // Reader and project keys must share macOS's canonical /private/var root.
    let temp = crate::project::fixture::canonical_tempdir();
    let vault = temp.path().join("vault");
    let area = vault.join("project");
    std::fs::create_dir_all(area.join("ops")).unwrap();
    std::fs::write(vault.join("start.md"), "# Start\n").unwrap();
    std::fs::write(
        area.join("note.md"),
        "# Related note\n\nKeep the Board viewport.\n",
    )
    .unwrap();
    std::fs::write(area.join("ops/project.toml"), "kind='code'\n").unwrap();
    std::fs::write(area.join("ops/board.yaml"), fixture_document(240)).unwrap();
    let project = okilum_t3::projects::Project::read(&area).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("start.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    reader.update_in(visual, |reader, window, cx| {
        reader.ensure_project_board_using(
            area.clone(),
            |binding| {
                Ok(binding
                    .issues
                    .iter()
                    .map(|scope| {
                        serde_json::from_value(serde_json::json!({
                            "number": scope.number, "title": scope.expected, "state": "open",
                            "labels": [{"name": if scope.number % 2 == 0 { "from-oleg" } else { "from-tester" }}]
                        }))
                        .unwrap()
                    })
                    .collect())
            },
            window,
            cx,
        );
        reader.enter_vault_project(project.clone(), window, cx);
        reader.project_navigation.boards[&area].update(cx, |board, cx| {
            board.list.preferences.collapsed[Column::Queue as usize] = false;
            cx.notify();
        });
    });
    // Project entry validates the folder asynchronously. Parking the executor
    // alone does not prove that the Reader has mounted this Board yet.
    visual
        .condition(&reader, |reader, _| {
            reader.project_navigation.board_visible
                && reader
                    .project_navigation
                    .active
                    .as_ref()
                    .is_some_and(|project| project.folder == area)
        })
        .await;
    let board = reader.read_with(visual, |r, _| r.project_navigation.boards[&area].clone());
    visual
        .condition(&board, |board, _| board.snapshot.forgejo.value.len() == 240)
        .await;
    visual.run_until_parked();
    click(visual, "board-chip-oleg");
    visual.simulate_keystrokes("/");
    visual.simulate_input("Synthetic task");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-list-count-Queue-120").is_some());
    board.update_in(visual, |board, window, cx| {
        board.selected = Some(230);
        board
            .list
            .scroll
            .scroll_to_item_strict(118, gpui::ScrollStrategy::Top);
        board.focus.focus(window, cx);
        cx.notify();
    });
    visual.run_until_parked();
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let offset = board.read_with(visual, |b, _| b.list.scroll.0.borrow().base_handle.offset());
    assert!(
        offset.y < px(-100.),
        "positive control: the actual List viewport scrolled"
    );
    click(visual, "board-note-230-0");
    // Opening the note prepares its document off the UI thread too.
    visual
        .condition(&reader, |reader, _| {
            reader.current_rel == "project/note.md" && !reader.project_navigation.board_visible
        })
        .await;
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.current_rel, "project/note.md");
        assert!(!r.project_navigation.board_visible);
    });
    assert!(visual.debug_bounds("board-list-viewport").is_none());
    click(visual, "project-return-board");
    assert!(visual.debug_bounds("board-list-viewport").is_some());
    board.read_with(visual, |b, cx| {
        assert_eq!(b.selected, Some(230));
        assert_eq!(b.filter.chip, filters::Chip::Oleg);
        assert_eq!(b.filter.search, "Synthetic task");
        assert_eq!(b.list.prepared.as_ref().unwrap().counts, [120, 0, 0, 0, 0]);
        assert_eq!(
            b.filter_ui.search.read(cx).value().as_str(),
            "Synthetic task"
        );
        assert_eq!(b.list.scroll.0.borrow().base_handle.offset(), offset);
        assert!(!b.list.preferences.collapsed[Column::Queue as usize]);
    });
    reader.read_with(visual, |r, _| assert!(r.project_navigation.board_visible));
}

#[gpui::test]
fn list_2530_ci_live_age_and_executor_filter_use_existing_evidence(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path(), fixture(4));
    board.update(visual, |board, cx| {
        let binding = board.binding.as_mut().unwrap();
        for scope in &mut binding.issues {
            scope.pull_requests = vec![100 + scope.number];
        }
        binding.issues[0].agent_thread_id = Some("synthetic-agent".into());
        binding.issues[1].executor = "Tests".into();
        let at = now();
        board.snapshot.forgejo.value = (1..=4).map(|number| {
            let mut issue: okilum_t3::board::Issue = serde_json::from_value(serde_json::json!({"number":number,"state":"open","title":format!("Source title {number}"),"labels":[{"name":"bug"},{"name":"from-oleg"},{"name":"epic: Board"}],"updated_at":"1970-01-01T00:00:00Z"})).unwrap();
            issue.open_pull_requests = vec![100 + number];
            issue.live_pulls = vec![okilum_t3::board::live::Pull { number: 100 + number, merged_at: None, checks: ["success", "pending", "failure", "unknown"][(number-1) as usize].into() }];
            (number, issue)
        }).collect();
        board.snapshot.forgejo.last_success = Some(at);
        board.snapshot.t3.value = vec![serde_json::from_value(serde_json::json!({"threadId":"synthetic-agent","title":"Not issue progress","status":"running","providerInstanceId":"codex","model":"synthetic","runtimeMode":"full-access","activeRunId":"run","updatedAt":chrono::DateTime::from_timestamp((at-4*3600-1) as i64, 0).unwrap().to_rfc3339()})).unwrap()];
        board.list.invalidate();
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    for selector in [
        "CI passed",
        "CI running",
        "CI failed",
        "CI unknown",
        "board-agent-live-dot",
        "board-list-stale-1",
        "board-list-time-2",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "missing {selector}"
        );
    }
    let dot = visual.debug_bounds("board-agent-live-dot").unwrap();
    let avatar = visual.debug_bounds("board-agent-link-1").unwrap();
    assert!(
        dot.left() >= avatar.left() && dot.right() <= avatar.right(),
        "live dot must not be clipped by button padding"
    );
    board.read_with(visual, |b, _| {
        assert_eq!(b.list.prepared.as_ref().unwrap().counts, [0, 1, 3, 0, 0]);
        let scope = &b.binding.as_ref().unwrap().issues[0];
        let active = live::agent(scope, &b.snapshot.t3.value, now(), 4 * 3600).unwrap();
        assert!(active.stuck);
        assert!(
            live::progress(scope, active.thread, None).is_none(),
            "thread titles never become progress"
        );
    });
    board.update(visual, |b, cx| {
        b.filter.executor = Some("Tests".into());
        b.filter_changed(cx);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-list-row-2").is_some());
    assert!(visual.debug_bounds("board-list-row-1").is_none());
    board.read_with(visual, |b, _| {
        assert_eq!(b.snapshot.forgejo.value.len(), 4);
        assert_eq!(b.list.prepared.as_ref().unwrap().counts, [0, 0, 1, 0, 0]);
    });
    click(visual, "board-remove-executor");
    assert!(visual.debug_bounds("board-list-row-1").is_some());
    board.read_with(visual, |b, _| {
        assert_eq!(b.list.prepared.as_ref().unwrap().counts, [0, 1, 3, 0, 0])
    });
}

#[gpui::test]
fn issue2532_list_chip_search_group_counts_and_shared_view_projection(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path(), fixture(3));
    board.update(visual, |board, cx| {
        board.list.preferences.collapsed = [false; 5];
        board.snapshot.forgejo.value = [(1, "Исправить ПОИСК", vec!["from-oleg", "bug"]),
            (2, "Tester report", vec!["from-tester", "bug"]),
            (3, "Other task", vec!["task"])]
            .into_iter().map(|(number, title, labels)| {
                let mut issue: okilum_t3::board::Issue = serde_json::from_value(serde_json::json!({"number":number,"title":title,"state":"open",
                    "labels": labels.into_iter().map(|name| serde_json::json!({"name":name})).collect::<Vec<_>>() })).unwrap();
                if number == 2 {
                    issue.live_pulls.push(okilum_t3::board::live::Pull {
                        number: 102, merged_at: None, checks: "failure".into(),
                    });
                }
                (number, issue)
            }).collect();
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    for selector in ["board-list-row-1", "board-list-row-2", "board-list-row-3"] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "positive control: all source rows rendered"
        );
    }
    assert!(visual.debug_bounds("board-list-count-Queue-2").is_some());
    assert!(visual.debug_bounds("board-list-count-Checks-1").is_some());
    let toggle = visual.debug_bounds("board-view-board").unwrap();
    let chip = visual.debug_bounds("board-chip-all").unwrap();
    assert!(
        chip.left() >= toggle.right(),
        "chip bar follows the view switch"
    );
    assert!(
        chip.top() < toggle.bottom(),
        "chip bar shares the header row"
    );
    click(visual, "board-chip-bugs");
    assert!(visual.debug_bounds("board-list-row-1").is_some());
    assert!(visual.debug_bounds("board-list-row-2").is_some());
    assert!(visual.debug_bounds("board-list-row-3").is_none());
    assert!(visual.debug_bounds("board-list-count-Queue-1").is_some());
    assert!(visual.debug_bounds("board-list-count-Checks-1").is_some());
    let prepared = board.read_with(visual, |b, _| b.list.prepared.clone().unwrap());
    board.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();
    board.read_with(visual, |b, _| {
        assert!(
            Arc::ptr_eq(&prepared, b.list.prepared.as_ref().unwrap()),
            "unchanged frames reuse the List row index"
        )
    });
    visual.simulate_keystrokes("/");
    visual.simulate_input("пОиСк");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-list-row-1").is_some());
    assert!(visual.debug_bounds("board-list-row-2").is_none());
    assert!(visual.debug_bounds("board-list-count-Queue-1").is_some());
    assert!(visual.debug_bounds("board-list-count-Checks-0").is_some());
    board.read_with(visual, |b, _| {
        assert!(
            !Arc::ptr_eq(&prepared, b.list.prepared.as_ref().unwrap()),
            "search rebuilt the row index"
        )
    });
    click(visual, "board-view-board");
    assert!(visual.debug_bounds("board-expand-1").is_some());
    assert!(visual.debug_bounds("board-expand-2").is_none());
    click(visual, "board-view-list");
    assert!(visual.debug_bounds("board-list-count-Queue-1").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    board.update_in(visual, |b, window, _| assert!(b.focus.is_focused(window)));
    assert!(visual.debug_bounds("board-list-count-Queue-1").is_some());
    assert!(visual.debug_bounds("board-list-count-Checks-1").is_some());
    visual.simulate_keystrokes("/");
    visual.simulate_input("#2");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-list-row-2").is_some());
    assert!(visual.debug_bounds("board-list-row-1").is_none());
    assert!(visual.debug_bounds("board-list-count-Queue-0").is_some());
    assert!(visual.debug_bounds("board-list-count-Checks-1").is_some());
    visual.simulate_keystrokes("escape /");
    // Set a query through the actual input after clearing the numeric search.
    visual.simulate_input("#999");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-filter-empty").is_some());
    board.update(visual, |board, cx| {
        board.snapshot.forgejo.error = Some("Synthetic source unavailable".into());
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    let center = visual.debug_bounds("project-board").unwrap();
    let status = visual.debug_bounds("board-live-status").unwrap();
    assert!(
        status.bottom() <= center.bottom() && status.top() >= center.bottom() - px(32.),
        "empty List results keep source diagnostics at the visible lower edge"
    );
    click(visual, "board-reset-filters");
    assert!(visual.debug_bounds("board-list-count-Queue-2").is_some());
    assert!(visual.debug_bounds("board-list-count-Checks-1").is_some());
}

/// #2548: exercise the production row and pill builders, including their actual
/// truncation bounds, rather than a label styled only for the test.
#[gpui::test]
fn list_2548_text_boxes_fit_descenders(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        brand::apply_theme(cx);
    });
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path(), fixture(1));
    let glyphs = "Jumpy query: g j p q y · у р д щ";
    let title = glyphs.repeat(12);
    board.update(visual, |board, cx| {
        board.binding.as_mut().unwrap().issues[0].agent_thread_id = Some("glyph-agent".into());
        let issue = serde_json::from_value(serde_json::json!({
            "number": 1, "state": "open", "title": title,
            "labels": [{"name": "bug"}, {"name": "epic: g j p q y у р д щ"}]
        }))
        .unwrap();
        board.snapshot.forgejo.value.insert(1, issue);
        board.snapshot.forgejo.last_success = Some(now());
        board.snapshot.t3.value = vec![serde_json::from_value(serde_json::json!({
            "threadId": "glyph-agent", "title": "Synthetic glyph agent",
            "updatedAt": chrono::DateTime::from_timestamp(now() as i64, 0).unwrap().to_rfc3339(),
            "status": "running", "activeRunId": "glyph-run",
            "providerInstanceId": "codex", "model": "synthetic", "runtimeMode": "full-access"
        }))
        .unwrap()];
        // Agent activity alone does not promote a backlog issue. Open Queue
        // explicitly so this typography fixture exercises the real source rules.
        board.list.preferences.collapsed[Column::Queue as usize] = false;
        board.list.invalidate();
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    for width in [1200., 900.] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        let row = visual
            .debug_bounds("board-list-row-1")
            .expect("positive control: issue rendered");
        assert_eq!(row.size.height, px(HEIGHT));
        let mut measured = 0;
        for (selector, font_size, weight) in [
            ("board-list-title-1", 13., FontWeight::NORMAL),
            ("board-state-1", 12., FontWeight::NORMAL),
            (
                "board-pill-label-epic: g j p q y у р д щ",
                12.,
                FontWeight::NORMAL,
            ),
            ("board-list-group-label-Working", 13., FontWeight::SEMIBOLD),
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector}"));
            let required = visual.update(|_, cx| {
                let mut descriptor = gpui::font(brand::SANS_FONT);
                descriptor.weight = weight;
                let font = cx.text_system().resolve_font(&descriptor);
                // Descent is signed in GPUI's test text system. Compare the
                // full extents so this check cannot accept a one-em clip box.
                cx.text_system().ascent(font, px(font_size)).abs()
                    + cx.text_system().descent(font, px(font_size)).abs()
            });
            assert!(
                required > px(font_size),
                "positive control: font metrics exceed one em"
            );
            assert!(
                bounds.size.height >= required,
                "{selector}: {:?} line box must fit ascent + descent {required:?}",
                bounds.size.height
            );
            if selector != "board-list-group-label-Working" {
                assert!(
                    bounds.top() >= row.top() && bounds.bottom() <= row.bottom(),
                    "{selector} must fit inside the 38 px row"
                );
            }
            measured += 1;
        }
        assert_eq!(
            measured, 4,
            "positive control: title, state, pill and group all measured"
        );
        let title_bounds = visual.debug_bounds("board-list-title-1").unwrap();
        let intrinsic_width = visual.update(|window, _| {
            window
                .text_system()
                .shape_line(
                    title.clone().into(),
                    px(13.),
                    &[gpui::TextRun {
                        len: title.len(),
                        font: gpui::font(brand::SANS_FONT),
                        color: gpui::black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
                .width()
        });
        assert!(
            title_bounds.size.width > px(0.) && title_bounds.size.width < intrinsic_width,
            "positive control: the long title overflows its constrained ellipsis box"
        );
    }
}

#[gpui::test]
fn list_and_cached_kanban_retain_main_ordering_after_source_publication(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        entity = Some(cx.new(|cx| {
            let mut board = Board::with_reader(area.path().into(), |_| Ok(Vec::new()), window, cx);
            board.binding = Some(fixture(5));
            board.list.preferences.collapsed[Column::Queue as usize] = false;
            for (number, origin, time) in [
                (1, "task", 500),
                (2, "bug", 400),
                (3, "from-tester", 300),
                (4, "from-oleg", 100),
                (5, "from-oleg", 200),
            ] {
                board.snapshot.forgejo.value.insert(number, serde_json::from_value(serde_json::json!({
                    "number": number, "title": "Synthetic issue", "state": "open",
                    "labels": [{"name": origin}],
                    "updated_at": chrono::DateTime::from_timestamp(time, 0).unwrap().to_rfc3339(),
                })).unwrap());
            }
            board.snapshot.forgejo.last_success = Some(now());
            board
        }));
        Root::new(cx.new(|_| gpui::EmptyView), window, cx)
    });
    visual.run_until_parked();
    let board = entity.unwrap();
    let order = |board: &mut Board| {
        let kanban = board.filtered_projection();
        let kanban_order = kanban.scopes[Column::Queue as usize]
            .iter()
            .map(|i| kanban.binding.issues[*i].number)
            .collect::<Vec<_>>();
        let list = board.prepare_list();
        let list_order = list
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Issue(i) => Some(list.binding.issues[*i].number),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            list_order, kanban_order,
            "both views retain the same main comparator"
        );
        list_order
    };
    assert_eq!(
        board.update(visual, |board, _| order(board)),
        vec![5, 4, 3, 2, 1]
    );
    let prepared = board.update(visual, |board, _| board.prepare_list());
    let reused = board.update(visual, |board, _| board.prepare_list());
    assert!(
        Arc::ptr_eq(&prepared, &reused),
        "viewport work reuses ordered rows"
    );
    board.update(visual, |board, cx| {
        board.snapshot.forgejo.value.get_mut(&4).unwrap().updated_at =
            chrono::DateTime::from_timestamp(600, 0)
                .unwrap()
                .to_rfc3339();
        board.list.invalidate();
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert_eq!(
        board.update(visual, |board, _| order(board)),
        vec![4, 5, 3, 2, 1]
    );
}

#[gpui::test]
fn filtered_list_reuses_projection_and_bounds_rows_through_resize(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path(), fixture(240));
    click(visual, "board-list-group-Queue");
    visual.simulate_keystrokes("/");
    visual.simulate_input("Synthetic task");
    visual.run_until_parked();
    let (projection, publications) = board.update(visual, |board, _| {
        assert_eq!(board.filter.search, "Synthetic task");
        let projection = board.filtered_projection();
        assert_eq!(projection.counts.iter().sum::<usize>(), 240);
        (projection, board.list.publications)
    });
    for width in [1300., 1100., 1250., 1400.] {
        board.update(visual, |board, _| board.list.rendered_rows = 0);
        visual.simulate_resize(size(px(width), px(900.)));
        visual.run_until_parked();
        board.update(visual, |board, _| {
            assert!(Arc::ptr_eq(&projection, &board.filtered_projection()));
            assert_eq!(board.list.publications, publications);
            assert!(
                board.list.rendered_rows > 0 && board.list.rendered_rows <= 64,
                "resize must build visible rows, not all 240: {}",
                board.list.rendered_rows
            );
        });
    }
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    board.read_with(visual, |board, cx| {
        assert_eq!(
            board.filter_ui.search.read(cx).selected_range(),
            0.."Synthetic task".len(),
            "positive control: the platform shortcut selected the entire query"
        );
    });
    visual.simulate_input("#240");
    visual.run_until_parked();
    let (filtered, filtered_publications) = board.update(visual, |board, _| {
        let filtered = board.filtered_projection();
        assert!(!Arc::ptr_eq(&projection, &filtered));
        assert_eq!(filtered.counts.iter().sum::<usize>(), 1);
        // simulate_input delivers one genuine change for each typed character.
        assert_eq!(
            board.list.publications,
            publications + "#240".chars().count()
        );
        (filtered, board.list.publications)
    });
    assert!(visual.debug_bounds("board-list-row-240").is_some());
    visual.simulate_resize(size(px(1200.), px(900.)));
    visual.run_until_parked();
    board.update(visual, |board, _| {
        assert!(Arc::ptr_eq(&filtered, &board.filtered_projection()));
        assert_eq!(board.list.publications, filtered_publications);
    });
}
