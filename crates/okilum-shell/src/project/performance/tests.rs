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
    let feed = project.read_with(visual, |project, _| project.feed.clone());
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
            )
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
    let (last, bounds) = [238, 239]
        .into_iter()
        .find_map(|number| {
            visual
                .debug_bounds(&format!("board-expand-{number}"))
                .filter(|bounds| bounds.intersects(&viewport))
                .map(|bounds| (number, bounds))
        })
        .expect("positive control: physical Board scroll reaches a last card, not an empty spacer");
    board.read_with(visual, |board, _| {
        assert!(
            board.frame_work.borrow().iter().all(|count| *count <= 64),
            "Board scroll viewport bound: {:?}",
            board.frame_work.borrow()
        )
    });
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds(&format!("board-discuss-{last}"))
            .is_some(),
        "offscreen card source and Discuss action remain available after virtualization"
    );
}
