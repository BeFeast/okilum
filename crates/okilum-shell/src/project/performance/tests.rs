//! Real Reader, native wheel/click/drag dispatch and the same reference data as native QA.
use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn reference_scroll_and_splitter_have_bounded_board_and_timeline_work(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("vault");
    std::fs::create_dir_all(vault.join("ops")).unwrap();
    std::fs::write(vault.join("ops/project.toml"), "kind = 'knowledge'\n").unwrap();
    std::fs::write(vault.join("home.md"), "# Home\n").unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("home.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        Root::new(reader, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    let reader = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Reader>().unwrap()
    });
    let (project, board) = reader.update_in(visual, |reader, window, cx| {
        let project = cx.new(|cx| {
            fixture::view(
                temp.path().join("fixture"),
                Arc::new(fixture::Fake::default()),
                window,
                cx,
            )
        });
        populate(&project, cx);
        let board = cx.new(|cx| board::Board::performance_fixture(vault.clone(), window, cx));
        board.update(cx, |board, cx| {
            board.set_manager(project.clone(), window, cx)
        });
        reader.project_navigation.active =
            Some(okilum_t3::projects::Project::read(&vault).unwrap());
        reader
            .project_navigation
            .boards
            .insert(vault, board.clone());
        reader.project_navigation.board_visible = true;
        reader.project = Some(project.clone());
        reader.project_open = true;
        reader.panels.close(reader_layout::Panel::Notes);
        reader.panels.open(reader_layout::Panel::Backlinks);
        reader.panel_widths.backlinks = 560.;
        cx.notify();
        (project, board)
    });
    visual.run_until_parked();
    // This must be Reader-mounted: its Dismiss action consumes Escape before
    // low-level key listeners, which a standalone ProjectView cannot reproduce.
    let draft = "Keep the mounted Manager draft.";
    project.update_in(visual, |project, window, cx| {
        project
            .compose
            .update(cx, |input, cx| input.set_value(draft, window, cx));
        let target = project.manager_model_target().unwrap();
        project.open_models(target, window, cx);
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-model-panel").is_some(),
        "positive control: mounted deferred picker"
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    project.update_in(visual, |project, window, cx| {
        assert!(
            !project.model_open,
            "Reader Escape must close the model picker"
        );
        assert!(project.compose.read(cx).focus_handle(cx).is_focused(window));
        assert_eq!(project.compose.read(cx).value().as_str(), draft);
        assert!(project.model_candidate.is_none());
    });
    reader.read_with(visual, |reader, _| {
        assert!(reader.panels.backlinks && reader.project_navigation.board_visible);
        assert!(
            !reader.panels.notes,
            "picker Escape must not toggle background panels"
        );
    });
    let renders = board.read_with(visual, |board, _| board.render_calls.get());
    project.update(visual, |project, cx| {
        project.identity.as_mut().unwrap().environment_id = "fixture-updated".into();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        board.read_with(visual, |board, _| board.render_calls.get()) > renders,
        "positive control: cached Board retains Manager status/progress publications"
    );
    let feed = project.read_with(visual, |project, _| project.feed.clone());
    // Reader-mounted typing must keep the existing inner Feed cache (#2410).
    // Wrapping all of Manager in another cached view forces a subtree refresh
    // and silently defeats that boundary, unlike a standalone ProjectView test.
    project.update_in(visual, |project, window, cx| {
        project.compose.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
    });
    visual.run_until_parked();
    visual.simulate_keystrokes("x");
    visual.run_until_parked();
    feed.read_with(visual, |feed, _| feed.row_calls.set(0));
    board.read_with(visual, |board, _| board.card_calls.set(0));
    visual.simulate_keystrokes("y");
    visual.run_until_parked();
    assert!(
        project.read_with(visual, |project, cx| project
            .compose
            .read(cx)
            .value()
            .ends_with("xy")),
        "positive control: real composer typing: {:?}",
        project.read_with(visual, |project, cx| project.compose.read(cx).value())
    );
    assert_eq!(
        feed.read_with(visual, |feed, _| feed.row_calls.get()),
        0,
        "composer typing must not rebuild history"
    );
    assert_eq!(
        board.read_with(visual, |board, _| board.card_calls.get()),
        0,
        "composer typing must not rebuild sibling Board cards"
    );
    let renders = board.read_with(visual, |board, _| board.render_calls.get());
    project.update(visual, |project, cx| {
        // Native reply batches can publish only Feed without notifying Project.
        project.feed.update(cx, |feed, cx| feed.apply_native(Changes {
            rows: vec![Arc::new(TurnRow {
                key: RowKey { thread: "mcp:fixture-manager".into(), item: "item-199-5".into() },
                local: true,
                item: Arc::new(json!({"id":"item-199-5","threadId":"mcp:fixture-manager","ordinal":1199,"type":"assistant_message","status":"completed","runId":"run-199","text":format!("## Updated synthetic progress\n\n{}", "A realistic paragraph wraps within the Manager reading column. ".repeat(8))})),
            })],
            ..Default::default()
        }, &[], cx));
    });
    visual.run_until_parked();
    assert!(
        board.read_with(visual, |board, _| board.render_calls.get()) > renders,
        "cached Board must observe a real Feed source publication without a parent notify"
    );
    // Native probes settle the pointer before timing. Match that here: GPUI
    // deliberately refreshes every cached view when keyboard modality changes
    // back to mouse after the Escape check above.
    visual.simulate_mouse_move(point(px(1100.), px(400.)), None, Modifiers::default());
    visual.run_until_parked();
    visual.update(|window, _| window.refresh());
    visual.run_until_parked();
    let history = visual
        .debug_bounds("project-history")
        .expect("real mounted history");
    assert!(
        visual.debug_bounds("board-card-1").is_some(),
        "real Board card positive control"
    );
    let start = feed.read_with(visual, |feed, cx| feed.performance_scroll_position(cx));
    for _ in 0..6 {
        board.read_with(visual, |board, _| {
            board.card_calls.set(0);
            board.frame_work.borrow_mut().clear();
            board.frame_layout_work.borrow_mut().clear();
        });
        feed.read_with(visual, |feed, _| feed.row_calls.set(0));
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: history.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(240.))),
            ..Default::default()
        });
        visual.run_until_parked();
        board.read_with(visual, |board, _| {
            assert_eq!(
                board.card_calls.get(),
                0,
                "Manager scrolling must not rebuild Board cards"
            )
        });
        feed.read_with(visual, |feed, _| {
            assert!(
                feed.row_calls.get() <= 96,
                "scroll must render a viewport, not the timeline: {}",
                feed.row_calls.get()
            )
        });
    }
    assert_ne!(
        feed.read_with(visual, |feed, cx| feed.performance_scroll_position(cx)),
        start,
        "positive control: physical wheel moved the source viewport"
    );
    let handle = reader.read_with(visual, |reader, _| {
        point(
            reader.body_bounds.right() - px(560.) + px(2.),
            reader.body_bounds.top() + px(100.),
        )
    });
    visual.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
    for delta in [20., 50., 80., 40., 0.] {
        board.read_with(visual, |board, _| {
            board.card_calls.set(0);
            board.frame_work.borrow_mut().clear();
            board.frame_layout_work.borrow_mut().clear();
        });
        feed.read_with(visual, |feed, _| feed.row_calls.set(0));
        visual.simulate_mouse_move(
            handle + point(px(delta), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        board.read_with(visual, |board, _| {
            assert!(
                board.card_calls.get() > 0
                    && board.frame_work.borrow().iter().all(|count| *count <= 64),
                "each splitter frame must rebuild a viewport, not all 240 cards: {:?}",
                board.frame_work.borrow()
            );
            assert!(
                board
                    .frame_layout_work
                    .borrow()
                    .iter()
                    .all(|count| *count <= 64),
                "offscreen cards must share spacers rather than one layout node per card: {:?}",
                board.frame_layout_work.borrow()
            );
        });
        feed.read_with(visual, |feed, _| {
            assert!(
                feed.row_calls.get() <= 96,
                "resize timeline work bound: {}",
                feed.row_calls.get()
            )
        });
    }
    reader.read_with(visual, |reader, _| {
        assert!(
            reader.resizing_panel.is_some(),
            "positive control: production splitter is dragging"
        )
    });
    visual.simulate_mouse_up(handle, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    // Force a complete fresh paint to recover debug bounds for cached sibling views.
    visual.update(|window, _| window.refresh());
    visual.run_until_parked();
    let card = visual.debug_bounds("board-expand-1").unwrap();
    visual.simulate_click(card.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-discuss-1").is_some(),
        "positive control: a real card click responds"
    );
    let viewport = visual.debug_bounds("project-board").unwrap();
    board.read_with(visual, |board, _| board.frame_work.borrow_mut().clear());
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: viewport.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
        ..Default::default()
    });
    visual.run_until_parked();
    let (discuss, bounds) = [
        ("board-expand-238", "board-discuss-238"),
        ("board-expand-239", "board-discuss-239"),
    ]
    .into_iter()
    .find_map(|(expand, discuss)| {
        visual
            .debug_bounds(expand)
            .filter(|bounds| bounds.intersects(&viewport))
            .map(|bounds| (discuss, bounds))
    })
    .expect("positive control: physical Board scroll reaches a last card, not an empty spacer");
    board.read_with(visual, |board, _| {
        assert!(
            board.frame_work.borrow().iter().all(|count| *count <= 64),
            "Board scroll viewport bound: {:?}",
            board.frame_work.borrow()
        )
    });
    // The tail card may extend below the scroll viewport. Click its visible
    // intersection rather than an offscreen center that native input cannot hit.
    visual.simulate_click(bounds.intersect(&viewport).center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds(discuss).is_some(),
        "offscreen card source and Discuss action remain available after virtualization"
    );
}
