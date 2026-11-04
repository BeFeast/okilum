use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn waiting_2533_three_kinds_actions_empty_and_failure_line(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_kanban_reader(
                area.path().into(),
                |_| panic!("offline fixture"),
                window,
                cx,
            );
            fixture::populate(&mut board);
            assert_eq!(
                board
                    .waiting
                    .items
                    .iter()
                    .map(|i| i.kind)
                    .collect::<Vec<_>>(),
                [model::Kind::Decision, model::Kind::Try, model::Kind::New]
            );
            assert_eq!(board.waiting.items[1].source, "Linux Beta 7552");
            assert!(board.waiting.items[1].video.is_some());
            board
        });
        Root::new(board, window, cx)
    });
    let board = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Board>().unwrap()
    });
    visual.simulate_resize(size(px(1500.), px(800.)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-waiting").is_some());
    for selector in [
        "board-waiting-item-0",
        "board-waiting-item-1",
        "board-waiting-item-2",
    ] {
        assert!(visual.debug_bounds(selector).is_some());
    }
    assert!(visual.debug_bounds("board-waiting-problem").is_none());
    let columns = visual.debug_bounds("board-columns").unwrap();
    assert!(visual.debug_bounds("board-waiting").unwrap().bottom() <= columns.top());
    let notes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = notes.clone();
    let _subscription = visual.update(|_, cx| {
        cx.subscribe(&board, move |_, event, _| {
            if let Action::Note(path) = event {
                observed.borrow_mut().push(path.clone());
            }
        })
    });
    let open = visual.debug_bounds("board-waiting-open-0").unwrap();
    visual.simulate_click(open.center(), Modifiers::default());
    assert_eq!(*notes.borrow(), vec!["ops/questions.md"]);
    let open = visual.debug_bounds("board-waiting-open-2").unwrap();
    visual.simulate_click(open.center(), Modifiers::default());
    assert_eq!(
        visual.opened_url().as_deref(),
        Some("https://git.example.test/team/project/issues/43")
    );
    // Paging caps native element construction even for a long questions source.
    board.update(visual, |board, cx| {
        let mut questions = board.project_notes.questions["ops/questions.md"]
            .value
            .clone();
        for index in 1..10 {
            let mut question = questions[0].clone();
            question.title = format!("Question {index}");
            questions.push(question);
        }
        board
            .project_notes
            .questions
            .get_mut("ops/questions.md")
            .unwrap()
            .value = questions;
        board.refresh_waiting();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-waiting-item-3").is_some());
    assert!(visual.debug_bounds("board-waiting-item-4").is_none());
    let next = visual.debug_bounds("board-waiting-next").unwrap();
    visual.simulate_click(next.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-waiting-item-4").is_some());
    assert!(visual.debug_bounds("board-waiting-item-0").is_none());
    assert!(visual.debug_bounds("board-waiting-item-8").is_none());
    board.update(visual, |board, cx| {
        board
            .project_notes
            .accept(okilum_t3::board::context::refresh(&board.area), now());
        board.refresh_waiting();
        assert_eq!(board.waiting_page, 0);
        cx.notify();
    });
    board.update(visual, |board, cx| {
        std::fs::remove_file(board.area.join("ops/questions.md")).unwrap();
        board
            .project_notes
            .accept(okilum_t3::board::context::refresh(&board.area), now());
        board.refresh_waiting();
        assert_eq!(
            board.waiting.items.len(),
            3,
            "source failure retains the last questions snapshot"
        );
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-waiting-problem").is_some());
    board.update(visual, |board, cx| {
        board.project_notes = Default::default();
        board.snapshot.forgejo.value.clear();
        board.refresh_waiting();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-waiting").is_none());
    assert!(visual.debug_bounds("board-waiting-problem").is_none());
    board.update(visual, |board, cx| {
        std::fs::write(
            board.area.join("ops/board-context.toml"),
            "version=1\nquestions_note='missing.md'\n",
        )
        .unwrap();
        board
            .project_notes
            .accept(okilum_t3::board::context::refresh(&board.area), now());
        board.refresh_waiting();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-waiting").is_none(),
        "never present an empty waiting group after a source failure"
    );
    assert!(visual.debug_bounds("board-waiting-problem").is_some());
}

#[gpui::test]
fn waiting_2533_failed_first_forgejo_read_is_one_line_without_empty_group(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_kanban_reader(
                area.path().into(),
                |_| panic!("offline fixture"),
                window,
                cx,
            );
            fixture::populate(&mut board);
            board.project_notes = Default::default();
            board.snapshot.forgejo = Default::default();
            board.snapshot.forgejo.error = Some("Comments unavailable".into());
            board.refresh_waiting();
            board
        });
        Root::new(board, window, cx)
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-waiting-problem").is_some());
    assert!(visual.debug_bounds("board-waiting").is_none());
}
