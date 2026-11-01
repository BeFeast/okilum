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
                Board::with_reader(area.into(), okilum_t3::board::read_issues, window, cx);
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
fn explicit_empty_expansion_survives_cards_and_freed_space_goes_to_nonempty_lane(
    cx: &mut TestAppContext,
) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    let initial = visual.debug_bounds("board-lane-Ready").unwrap();
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
        cx.notify();
    });
    visual.run_until_parked();
    let full = visual
        .debug_bounds("board-lane-Ready")
        .expect("new nonempty lane auto-opens");
    assert!(
        full.size.width > initial.size.width * 2.,
        "useful lane receives the empty lanes' width"
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
    visual.simulate_keystrokes("tab enter");
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-lane-Queue").is_some(),
        "first named empty-column control works with Tab/Enter"
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
        board
            .local_cards
            .value
            .problems
            .push("Local card could not be parsed".into());
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
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-card-problem-0").is_some());
    assert!(visual.debug_bounds("board-lane-Queue").is_some());
    assert!(visual.debug_bounds("board-empty-state").is_none());
}
