use super::*;
use ::core::prelude::v1::test;

fn fixture() -> tempfile::TempDir {
    let area = tempfile::tempdir().unwrap();
    std::fs::create_dir(area.path().join("ops")).unwrap();
    std::fs::write(
        area.path().join("ops/project.toml"),
        "name='Local'\nkind='code'\n",
    )
    .unwrap();
    std::fs::write(
        area.path().join("ops/board.toml"),
        "version=1\ntitle='Local'\nissues=[]\n",
    )
    .unwrap();
    area
}
fn open<'a>(cx: &'a mut TestAppContext, area: &Path) -> (Entity<Board>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut b = Board::with_reader(area.into(), okilum_t3::board::read_issues, window, cx);
            b.binding = Binding::read(area).unwrap();
            b.active = true;
            b.local_cards.accept(Ok(model::read(area).unwrap()), now());
            b
        });
        Root::new(board, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(1000.)));
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
// Pinned GPUI's debug lookup needs static selectors; test-process lifetime only.
fn selector(prefix: &str, id: &str) -> &'static str {
    Box::leak(format!("{prefix}{id}").into_boxed_str())
}
#[gpui::test]
fn title_only_local_card_uses_real_create_edit_remove_undo_controls_without_issue_requirements(
    cx: &mut TestAppContext,
) {
    let area = fixture();
    let (board, visual) = open(cx, area.path());
    let project = std::fs::read(area.path().join("ops/project.toml")).unwrap();
    let manifest = std::fs::read(area.path().join("ops/board.toml")).unwrap();
    click(visual, "board-local-add");
    assert!(visual.debug_bounds("board-local-title").is_some());
    assert!(visual.debug_bounds("board-field-issue").is_none());
    assert!(visual.debug_bounds("board-field-executor").is_none());
    click(visual, "board-local-save");
    assert!(visual.debug_bounds("board-local-error").is_some());
    assert!(!area.path().join("ops/board-local.toml").exists());
    board.update_in(visual, |board, window, cx| {
        board
            .local_editor
            .as_mut()
            .unwrap()
            .title
            .update(cx, |input, cx| {
                input.set_value("Read the design", window, cx)
            });
    });
    click(visual, "board-local-save");
    let initial = model::read(area.path()).unwrap();
    assert_eq!(initial.cards.len(), 1);
    let id = initial.cards[0].id.clone();
    assert!(uuid::Uuid::parse_str(&id).is_ok());
    assert!(initial.cards[0].details.is_empty());
    assert_eq!(initial.cards[0].state, model::State::Todo);
    click(visual, selector("board-local-manage-edit-", &id));
    board.update_in(visual, |board, window, cx| {
        let editor = board.local_editor.as_mut().unwrap();
        editor.title.update(cx, |input, cx| {
            input.set_value("Keep original local identity", window, cx)
        });
        editor.details.update(cx, |input, cx| {
            input.set_value(
                "First line\nSecond line with #42 as ordinary text",
                window,
                cx,
            )
        });
    });
    click(visual, "board-local-state-2");
    click(visual, "board-local-save");
    let updated = model::read(area.path()).unwrap();
    assert_eq!(updated.cards[0].id, id);
    assert_eq!(updated.cards[0].state, model::State::Done);
    assert!(updated.cards[0].details.contains('\n'));
    let before = std::fs::read(area.path().join("ops/board-local.toml")).unwrap();
    click(visual, selector("board-local-remove-", &id));
    assert!(model::read(area.path()).unwrap().cards.is_empty());
    click(visual, "board-local-undo");
    assert_eq!(
        std::fs::read(area.path().join("ops/board-local.toml")).unwrap(),
        before
    );
    click(visual, "board-local-close");
    let card = visual
        .debug_bounds(selector("board-local-card-", &id))
        .unwrap();
    let ready = visual.debug_bounds("board-lane-Ready").unwrap();
    assert!(card.left() >= ready.left() && card.right() <= ready.right());
    assert!(
        visual.debug_bounds("board-expand-42").is_none(),
        "local text never enrolls a remote issue"
    );
    assert!(
        visual.debug_bounds("board-lane-Verified").is_none(),
        "Done is not human build acceptance"
    );
    assert_eq!(
        std::fs::read(area.path().join("ops/project.toml")).unwrap(),
        project
    );
    assert_eq!(
        std::fs::read(area.path().join("ops/board.toml")).unwrap(),
        manifest
    );
}
#[gpui::test]
fn local_card_conflict_keeps_the_draft_and_external_file_and_failed_refresh_keeps_known_cards(
    cx: &mut TestAppContext,
) {
    let area = fixture();
    let (board, visual) = open(cx, area.path());
    click(visual, "board-local-add");
    board.update_in(visual, |board, window, cx| {
        board
            .local_editor
            .as_mut()
            .unwrap()
            .title
            .update(cx, |input, cx| input.set_value("Keep my draft", window, cx))
    });
    let id = uuid::Uuid::new_v4().to_string();
    let external = format!("version=1\ncards=[{{id='{id}',title='External card'}}]\n");
    std::fs::write(area.path().join("ops/board-local.toml"), &external).unwrap();
    click(visual, "board-local-save");
    assert!(visual.debug_bounds("board-local-error").is_some());
    board.read_with(visual, |board, cx| {
        assert_eq!(
            board
                .local_editor
                .as_ref()
                .unwrap()
                .title
                .read(cx)
                .value()
                .as_str(),
            "Keep my draft"
        )
    });
    assert_eq!(
        std::fs::read_to_string(area.path().join("ops/board-local.toml")).unwrap(),
        external
    );
    click(visual, "board-local-close");
    board.update_in(visual, |board, window, cx| {
        board.refresh_local_cards(window, cx)
    });
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds(selector("board-local-card-", &id))
            .is_some(),
        "positive control: real refresh reads the external card"
    );
    let last = board.read_with(visual, |board, _| board.local_cards.last_success);
    std::fs::write(area.path().join("ops/board-local.toml"), "version=[broken").unwrap();
    board.update_in(visual, |board, window, cx| {
        board.refresh_local_cards(window, cx)
    });
    visual.run_until_parked();
    assert!(visual
        .debug_bounds(selector("board-local-card-", &id))
        .is_some());
    assert!(visual.debug_bounds("board-source-problem").is_some());
    board.read_with(visual, |board, _| {
        assert_eq!(board.local_cards.last_success, last);
        assert!(board
            .local_cards
            .error
            .as_ref()
            .unwrap()
            .contains("board-local.toml"));
    });
}
#[gpui::test]
fn local_card_discuss_uses_existing_draft_only_route_without_execution_facts(
    cx: &mut TestAppContext,
) {
    let area = fixture();
    let id = uuid::Uuid::new_v4().to_string();
    model::Session::load(area.path())
        .unwrap()
        .save(&model::Change::Add(
            id.clone(),
            model::Draft {
                title: "Local task".into(),
                ..Default::default()
            },
        ))
        .unwrap();
    let (board, visual) = open(cx, area.path());
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    board.update(visual, |_, cx| {
        let seen = seen.clone();
        cx.subscribe(&board, move |_, _, action: &Action, _| {
            if let Action::Discuss(text) = action {
                seen.borrow_mut().push(text.clone());
            }
        })
        .detach();
    });
    click(visual, selector("board-local-expand-", &id));
    click(visual, selector("board-local-discuss-", &id));
    let drafts = seen.borrow();
    assert_eq!(drafts.len(), 1);
    assert!(
        drafts[0].contains(&id)
            && drafts[0].contains("ops/board-local.toml")
            && drafts[0].contains("Local task")
    );
    assert!(!drafts[0].contains("Issue:") && !drafts[0].contains("PR:"));
}
