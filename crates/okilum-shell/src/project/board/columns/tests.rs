use super::*;
use ::core::prelude::v1::test;
use gpui::{TestAppContext, VisualTestContext};

fn open<'a>(cx: &'a mut TestAppContext, area: &Path) -> (Entity<Board>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board =
                Board::with_kanban_reader(area.into(), okilum_t3::board::live::read, window, cx);
            board.binding = Some(Binding::parse("version: 1\ntitle: Empty\nissues: []\n").unwrap());
            board.local_loading = true;
            board
        });
        Root::new(board, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(900.)));
    visual.run_until_parked();
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    (board, visual)
}

fn loaded(board: &Entity<Board>, visual: &mut VisualTestContext) {
    board.update(visual, |board, cx| {
        board.local_loading = false;
        board.local_cards.last_success = Some(now());
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

#[gpui::test]
fn loading_is_not_empty_and_refresh_does_not_unfold_completed_empty_columns(
    cx: &mut TestAppContext,
) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    assert!(
        visual.debug_bounds("board-lane-Ready").is_some(),
        "initial unknown is not zero cards"
    );
    assert!(visual.debug_bounds("board-expand-empty-Ready").is_none());
    assert!(visual.debug_bounds("board-empty-state").is_none());
    loaded(&board, visual);
    assert!(visual.debug_bounds("board-lane-Ready").is_none());
    assert!(visual.debug_bounds("board-empty-state").is_some());
    assert!(
        visual.debug_bounds("board-local-add").is_some(),
        "all-empty Board retains Add card"
    );
    let compact = visual
        .debug_bounds("board-expand-empty-Ready")
        .expect("named/count control");
    for loading in [true, false, true, false] {
        board.update(visual, |board, cx| {
            board.binding_loading = loading;
            board.local_loading = loading;
            board.delivery_loading = loading;
            if loading {
                assert!(board.snapshot.begin().is_some());
            } else {
                board.snapshot.invalidate();
            }
            board.filter_cache.invalidate();
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("board-lane-Ready").is_none());
        assert_eq!(
            visual.debug_bounds("board-expand-empty-Ready").unwrap(),
            compact
        );
    }
    click(visual, "board-verified-toggle");
    assert!(
        visual.debug_bounds("board-lane-Verified").is_some(),
        "the existing explicit Verified expansion is respected even at zero cards"
    );
    click(visual, "board-collapse-empty-Verified");
    assert!(visual.debug_bounds("board-lane-Verified").is_none());
}

#[gpui::test]
fn explicit_empty_expansion_survives_cards_and_preserves_readable_lane_width(
    cx: &mut TestAppContext,
) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    assert_eq!(
        visual.debug_bounds("board-lane-Ready").unwrap().size.width,
        px(268.)
    );
    loaded(&board, visual);
    board.update(visual, |board, cx| {
        // A card arriving during a refresh is always shown immediately.
        board.local_loading = true;
        board
            .local_cards
            .value
            .cards
            .push(okilum_t3::board::local::Card {
                id: "10000000-0000-4000-8000-000000000001".into(),
                title: "Ready for a person".into(),
                details: String::new(),
                state: okilum_t3::board::local::State::Done,
            });
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    let full = visual
        .debug_bounds("board-lane-Ready")
        .expect("new nonempty lane auto-opens");
    assert_eq!(
        full.size.width,
        px(268.),
        "empty lanes never stretch readable cards"
    );
    assert!(visual
        .debug_bounds("board-local-card-10000000-0000-4000-8000-000000000001")
        .is_some());
    assert!(visual.debug_bounds("board-empty-state").is_none());
    loaded(&board, visual);
    click(visual, "board-expand-empty-Checks");
    assert!(visual.debug_bounds("board-lane-Checks").is_some());
    assert!(visual.debug_bounds("board-collapse-empty-Checks").is_some());
    board.update(visual, |board, cx| {
        board.local_cards.value.cards.clear();
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-lane-Ready").is_none(),
        "newly empty auto-folds after completed sources"
    );
    assert!(
        visual.debug_bounds("board-lane-Checks").is_some(),
        "explicit expansion stays"
    );
    for _ in 0..2 {
        board.update(visual, |board, cx| {
            board.local_loading = true;
            board.filter_cache.invalidate();
            cx.notify();
        });
        visual.run_until_parked();
        loaded(&board, visual);
        assert!(visual.debug_bounds("board-lane-Checks").is_some());
    }
    click(visual, "board-collapse-empty-Checks");
    assert!(visual.debug_bounds("board-lane-Checks").is_none());
    assert!(visual.debug_bounds("board-empty-state").is_some());
}

#[gpui::test]
fn empty_column_can_be_expanded_from_the_keyboard(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    loaded(&board, visual);
    board.update_in(visual, |board, window, cx| board.focus.focus(window, cx));
    visual.run_until_parked();
    // Nine header controls precede the empty-stage controls. Compact Board
    // presents Working, Checks and Ready before the named Queue control.
    assert!(visual.debug_bounds("board-lane-Queue").is_none());
    for _ in 0..13 {
        visual.simulate_keystrokes("tab");
    }
    let keystroke = gpui::Keystroke::parse("enter").unwrap();
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(gpui::KeyUpEvent { keystroke });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-lane-Queue").is_some(),
        "named Queue control works with Tab/Enter"
    );
    assert!(visual.debug_bounds("board-collapse-empty-Queue").is_some());
}

#[gpui::test]
fn unresolved_card_problems_are_not_treated_as_an_empty_board(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    loaded(&board, visual);
    board.update(visual, |board, cx| {
        board.columns.invalidate();
        board.list.invalidate();
        board
            .local_cards
            .value
            .problems
            .push("Local card could not be parsed".into());
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-lane-Queue").is_some());
    assert!(visual.debug_bounds("board-empty-state").is_none());
    board.update(visual, |board, cx| {
        board.local_cards.value.problems.clear();
        board
            .binding
            .as_mut()
            .unwrap()
            .card_problems
            .push(okilum_t3::board::CardProblem {
                field: "issues[0]".into(),
                number: Some(17),
                detail: "Missing expected result".into(),
            });
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-card-problem-0").is_some());
    assert!(visual.debug_bounds("board-lane-Queue").is_some());
    assert!(visual.debug_bounds("board-empty-state").is_none());
}

#[gpui::test]
fn annotation_free_repository_failure_is_unknown_until_live_discovery_succeeds(
    cx: &mut TestAppContext,
) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    board.update(visual, |board, cx| {
        board.binding.as_mut().unwrap().repository = "https://forgejo.example/team/project".into();
        board.snapshot.forgejo.error = Some("HTTP 503".into());
        board.filter_cache.invalidate();
        cx.notify();
    });
    loaded(&board, visual);
    assert!(visual.debug_bounds("board-lane-Queue").is_some());
    assert!(
        visual.debug_bounds("board-empty-state").is_none(),
        "API failure before first discovery must not claim an empty repository"
    );
    assert!(visual.debug_bounds("board-expand-empty-Queue").is_none());
    board.update(visual, |board, cx| {
        board.list.invalidate();
        board.snapshot.forgejo.accept(Ok(Default::default()), now());
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-empty-state").is_some(),
        "positive control: successful empty discovery can collapse the Board"
    );
    assert!(visual.debug_bounds("board-expand-empty-Queue").is_some());
}

#[gpui::test]
fn empty_live_columns_collapse_during_delivery_refresh_and_reopen_for_incoming_cards(
    cx: &mut TestAppContext,
) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    // Reproduce the serialized live Forgejo -> delivery path: the first paint
    // of a complete last-good projection can occur with the next delivery read
    // already in flight. No earlier render established an emptiness decision.
    board.update(visual, |board, cx| {
        board.binding.as_mut().unwrap().repository = "https://forgejo.example/team/project".into();
        board.list.invalidate();
        board.snapshot.forgejo.accept(
            Ok([(
                42,
                serde_json::from_value(serde_json::json!({
                    "number": 42, "title": "Live repository card", "state": "open"
                }))
                .unwrap(),
            )]
            .into()),
            now(),
        );
        board.local_loading = false;
        board.local_cards.last_success = Some(now());
        board.delivery_loading = true;
        board.columns.invalidate();
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-lane-Ready").is_some(),
        "initial unknown delivery is not proof of emptiness"
    );
    board.update(visual, |board, cx| {
        board.delivery.config.accept(Ok(None), now());
        board.filter_cache.invalidate();
        cx.notify();
    });
    for mode in [
        gpui_component::ThemeMode::Light,
        gpui_component::ThemeMode::Dark,
    ] {
        visual.update(|_, cx| {
            Theme::change(mode, None, cx);
            brand::apply_theme(cx);
        });
        for width in [800., 1600.] {
            board.update(visual, |board, cx| {
                board.columns.invalidate();
                board.filter_cache.invalidate();
                cx.notify();
            });
            visual.simulate_resize(size(px(width), px(900.)));
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("board-expand-42").is_some(),
                "positive control: live projection actually rendered"
            );
            for (lane, control) in [
                ("board-lane-Working", "board-expand-empty-Working"),
                ("board-lane-Checks", "board-expand-empty-Checks"),
                ("board-lane-Ready", "board-expand-empty-Ready"),
            ] {
                assert!(
                    visual.debug_bounds(lane).is_none(),
                    "empty {lane} collapses during live delivery refresh"
                );
                assert!(visual.debug_bounds(control).is_some());
            }
            let queue = visual.debug_bounds("board-lane-Queue").unwrap();
            assert_eq!(queue.size.width, px(240.));
        }
    }
    board.update(visual, |board, cx| {
        board.list.invalidate();
        let issue = board.snapshot.forgejo.value.get_mut(&42).unwrap();
        issue.open_pull_requests.push(7);
        issue.live_pulls.push(okilum_t3::board::live::Pull {
            title: String::new(),
            state: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            number: 7,
            merged_at: None,
            checks: "pending".into(),
        });
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-lane-Checks").is_some(),
        "incoming live card reopens the lane immediately"
    );
    assert!(visual.debug_bounds("board-expand-42").is_some());
    board.update(visual, |board, cx| {
        board.list.invalidate();
        board.snapshot.forgejo.accept(Err("HTTP 503".into()), now());
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-lane-Checks").is_some(),
        "failure retains last-good live cards"
    );
    assert!(
        visual.debug_bounds("board-lane-Ready").is_none(),
        "failure retains completed collapse"
    );
    let center = visual.debug_bounds("project-board").unwrap();
    let status = visual.debug_bounds("board-live-status").unwrap();
    assert!(
        status.top() > center.top() + center.size.height / 2.,
        "live status stays at the lower edge"
    );
}
