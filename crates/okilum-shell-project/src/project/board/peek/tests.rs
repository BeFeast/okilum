use super::*;
use ::core::prelude::v1::test;

fn click(cx: &mut VisualTestContext, key: &'static str) {
    let bounds = cx
        .debug_bounds(key)
        .unwrap_or_else(|| panic!("missing {key}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}
#[gpui::test]
fn peek_2531_click_space_escape_arrows_and_tabs_preserve_board_geometry(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut b = Board::with_reader(area.path().into(), |_| Ok(vec![]), window, cx);
            b.binding = Some(Binding::parse(&serde_json::json!({"version":1,"title":"Fixture","repository":"https://example.test/team/repo","issues":[{"number":1,"expected":"Typography gypд","executor":"Fixture"},{"number":2,"expected":"Preserve identity","executor":"Fixture"}]}).to_string()).unwrap());
            b.binding.as_mut().unwrap().issues[0].pull_requests = vec![101];
            let mut issue: okilum_t3::board::Issue = serde_json::from_value(serde_json::json!({"number":1,"title":"Typography gypд","state":"open"})).unwrap();
            issue.live_pulls = vec![okilum_t3::board::live::Pull { number:101,title:"Preserve the complete readable pull request title while switching between the issue details and the existing Manager conversation, including descenders gypд".into(),state:"open".into(),checks:"pending".into(),..Default::default() }];
            b.snapshot.forgejo.value.insert(1,issue);
            let reads = reads.clone();
            b.peek.reader = Some(Arc::new(move |_,_| { reads.fetch_add(1,std::sync::atomic::Ordering::SeqCst); Ok(vec![]) }));
            b.list.preferences.collapsed[Column::Queue as usize] = false;
            b
        });
        let host = cx.new(|cx| Harness::new(board,cx));
        Root::new(host,window,cx)
    });
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    let board = root.read_with(visual, |root, cx| {
        root.view()
            .clone()
            .downcast::<Harness>()
            .unwrap()
            .read(cx)
            .board
            .clone()
    });
    let height = visual.debug_bounds("board-list-row-1").unwrap().size.height;
    click(visual, "board-list-row-1");
    assert!(visual.debug_bounds("board-peek-content").is_some());
    assert!(visual.debug_bounds("board-list-details").is_none());
    assert!(
        visual.debug_bounds("board-peek-property-Связано").is_none(),
        "empty related property has no placeholder"
    );
    let stage = visual.debug_bounds("board-peek-property-Стадия").unwrap();
    assert!(
        stage.size.height >= px(20.),
        "property labels reserve a full line box"
    );
    board.update(visual, |b, cx| {
        b.binding.as_mut().unwrap().stage_note = "notes/context.md".into();
        b.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-peek-property-Связано").is_some(),
        "positive control: a real related note renders the row"
    );
    let pr_title = visual.debug_bounds("board-peek-pr-title-101").unwrap();
    let panel = visual.debug_bounds("board-peek-content").unwrap();
    assert!(
        pr_title.size.height >= px(40.),
        "long PR title has full wrapped line boxes"
    );
    assert!(
        pr_title.left() >= panel.left() && pr_title.right() <= panel.right(),
        "PR text stays inside its pane"
    );
    click(visual, "board-list-row-1");
    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "clicking the already open issue does not read it again"
    );
    assert_eq!(
        visual.debug_bounds("board-list-row-1").unwrap().size.height,
        height
    );
    board.update(visual, |b, cx| {
        b.filter.chip = filters::Chip::Oleg;
        b.filter_changed(cx);
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-list-row-1").is_none(),
        "positive control: the filter hides the row"
    );
    assert!(
        visual.debug_bounds("board-peek-pr-title-101").is_some(),
        "filtering rows retains the same issue details"
    );
    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "rendering after a filter change never reads comments"
    );
    board.update(visual, |b, cx| {
        b.filter.chip = filters::Chip::All;
        b.filter_changed(cx);
    });
    visual.run_until_parked();
    click(visual, "board-peek-manager-tab");
    assert!(visual.debug_bounds("fixture-manager").is_some());
    click(visual, "board-peek-tab");
    board.update_in(visual, |b, w, cx| b.focus.focus(w, cx));
    visual.simulate_keystrokes("down");
    visual.run_until_parked();
    board.read_with(visual, |b, _| {
        assert_eq!(b.selected, Some(2));
        assert!(b.peek.open);
    });
    visual.simulate_keystrokes("up escape");
    visual.run_until_parked();
    board.read_with(visual, |b, _| {
        assert_eq!(b.selected, Some(1));
        assert!(!b.peek.open);
    });
    visual.simulate_keystrokes("space");
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-peek-content").is_some(),
        "Space reaches actual issue panel"
    );
    click(visual, "board-peek-close");
    assert!(visual.debug_bounds("board-peek-content").is_none());
}

#[gpui::test]
fn peek_2531_generation_owner_repository_and_issue_reject_late_results(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = cx.add_window_view(|window, cx| {
        Board::with_reader(area.path().into(), |_| Ok(vec![]), window, cx)
    });
    board.update_in(visual,|b,window,cx| {
        b.binding = Some(Binding::parse("{\"version\":1,\"title\":\"Fixture\",\"repository\":\"https://example.test/team/repo\",\"issues\":[]}").unwrap());
        b.selected = Some(1);
        b.peek.open = true;
        let ticket = Ticket { area:b.area.clone(),repository:b.binding.as_ref().unwrap().repository.clone(),issue:1,generation:b.peek.generation };
        assert!(b.peek_matches(&ticket),"positive control: exact open owner accepts result");
        let comment: Comment = serde_json::from_value(serde_json::json!({"created_at":"2026-11-04T09:30:00Z","user":{"login":"fixture-owner"},"body":"Exact issue response"})).unwrap();
        b.complete_peek_comments(&ticket, Ok(vec![comment.clone()]), window, cx);
        assert_eq!(b.peek.comments.len(), 1, "positive control: production completion publishes an exact result");
        b.selected = Some(2);
        assert!(!b.peek_matches(&ticket));
        b.selected = Some(1);
        b.close_peek(cx);
        b.peek.open = true;
        assert!(!b.peek_matches(&ticket),"close and reopen reject old generation");
        let ticket = Ticket { generation:b.peek.generation,..ticket };
        b.active = true;
        b.set_visible(false,window,cx);
        assert!(!b.peek.open);
        assert!(!b.peek_matches(&ticket),"project switch invalidates outstanding read");
        b.complete_peek_comments(&ticket, Ok(vec![comment]), window, cx);
        assert!(b.peek.comments.is_empty(), "late completion from old project stays dropped");
        b.peek.open = true;
        let ticket = Ticket { generation:b.peek.generation,..ticket };
        b.area = area.path().join("other");
        assert!(!b.peek_matches(&ticket));
        b.area = ticket.area.clone();
        b.binding.as_mut().unwrap().repository = "https://example.test/other/repo".into();
        assert!(!b.peek_matches(&ticket));
    });
}
