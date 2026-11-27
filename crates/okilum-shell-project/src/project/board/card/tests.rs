use super::*;
use ::core::prelude::v1::test;
use gpui::{TestAppContext, VisualTestContext};

fn open<'a>(cx: &'a mut TestAppContext, area: &Path) -> (Entity<Board>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        cx.set_global(reader_history::TestSessionDirectory(
            area.join("presentation-state"),
        ));
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board = Board::with_kanban_reader(area.into(), |_| Ok(Vec::new()), window, cx);
            board.binding = Some(Binding::parse(&serde_json::json!({
                "version":1,"title":"Synthetic Board","repository":"https://example.test/team/repo",
                "issues":(1..=240).map(|number| serde_json::json!({
                    "number":number,"expected":"Expected result stays in the issue dock","executor":"Synthetic agent",
                    "stage":if number == 240 { Some("verified") } else { None },
                    "pull_requests":if number <= 16 { vec![1000+number] } else { vec![] },
                    "agent_thread_id":if number == 1 { Some("synthetic-agent") } else { None }
                })).collect::<Vec<_>>()
            }).to_string()).unwrap());
            let at = now();
            board.snapshot.forgejo.value = (1..=240).map(|number| {
                let mut issue: okilum_t3::board::Issue = serde_json::from_value(serde_json::json!({
                    "number":number,"state":"open",
                    "updated_at":chrono::DateTime::from_timestamp((at-number*60) as i64,0).unwrap().to_rfc3339(),
                    "title":"Typography gypj: a deliberately long source title wraps over several lines but the compact card must reserve only two complete line boxes with descenders",
                    "labels":[{"name":"bug"},{"name":"from-tester"},{"name":"epic: Board"}]
                })).unwrap();
                if number <= 16 {
                    issue.open_pull_requests = vec![1000+number];
                    issue.live_pulls = vec![okilum_t3::board::live::Pull { number:1000+number,merged_at:None,
                        checks:if number == 2 { "failure" } else { "success" }.into(), ..Default::default() }];
                }
                (number,issue)
            }).collect();
            board.snapshot.forgejo.last_success = Some(at);
            board.snapshot.t3.value = vec![serde_json::from_value(serde_json::json!({
                "threadId":"synthetic-agent","title":"Not issue progress","status":"running","activeRunId":"run",
                "providerInstanceId":"codex","model":"synthetic","runtimeMode":"full-access",
                "updatedAt":chrono::DateTime::from_timestamp((at-4*3600-1) as i64,0).unwrap().to_rfc3339()
            })).unwrap()];
            board
        });
        Root::new(cx.new(|cx| peek::Harness::new(board,cx)), window, cx)
    });
    visual.simulate_resize(size(px(1600.), px(1000.)));
    visual.run_until_parked();
    let board = root.read_with(visual, |r, cx| {
        r.view()
            .clone()
            .downcast::<peek::Harness>()
            .unwrap()
            .read(cx)
            .board
            .clone()
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
fn kanban_2534_three_rows_two_complete_title_lines_and_viewport_work(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    let meta = visual.debug_bounds("board-card-meta-1").unwrap();
    let title = visual.debug_bounds("board-card-title-1").unwrap();
    let now = visual.debug_bounds("board-card-now-1").unwrap();
    let card = visual.debug_bounds("board-card-1").unwrap();
    assert_eq!(
        title.size.height,
        px(39.),
        "exactly two full 19.5 px line boxes"
    );
    assert!(meta.bottom() <= title.top() && title.bottom() <= now.top());
    assert!(meta.size.height >= px(18.) && now.size.height >= px(16.));
    assert!(now.bottom() < card.bottom());
    assert!(card.size.height <= px(84.));
    // Measure production truncation boxes against full UI font metrics. Shared
    // pills are clipped by their metadata container if it is shorter than 20 px.
    let required = visual.update(|_, cx| {
        let font = cx.text_system().resolve_font(&gpui::font(brand::SANS_FONT));
        cx.text_system().ascent(font, px(12.)).abs() + cx.text_system().descent(font, px(12.)).abs()
    });
    assert!(required > px(12.), "positive font-metric control");
    assert!(meta.size.height >= required + px(2.));
    assert!(now.size.height >= required);

    assert_eq!(
        visual
            .debug_bounds("board-lane-Working")
            .unwrap()
            .size
            .width,
        px(268.)
    );
    assert!(
        visual.debug_bounds("board-card-16").is_none(),
        "only viewport cards are built"
    );
    assert!(
        visual.debug_bounds("board-card-1").is_some(),
        "positive rendered-card control"
    );
    for selector in [
        "board-agent-live-dot",
        "board-card-stale-1",
        "CI failed",
        "CI passed",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "missing evidence renderer: {selector}"
        );
    }
    let tenth = visual
        .debug_bounds("board-card-11")
        .expect("ten full cards on a 1000 px window");
    let columns = visual.debug_bounds("board-columns").unwrap();
    assert!(
        tenth.bottom() <= columns.bottom(),
        "the tenth card is fully visible"
    );
    let publications = board.read_with(visual, |b, _| b.list.publication_count());
    board.update(visual, |b, cx| {
        b.kanban.lanes[Column::Working as usize]
            .scroll_to_item_strict(10, gpui::ScrollStrategy::Top);
        cx.notify();
    });
    visual.run_until_parked();
    let offset = board.read_with(visual, |b, _| {
        b.kanban.lanes[Column::Working as usize]
            .0
            .borrow()
            .base_handle
            .offset()
    });
    assert!(
        offset.y < px(-100.),
        "positive control: the actual lane scrolled"
    );
    assert!(visual.debug_bounds("board-card-16").is_some());
    click(visual, "board-view-list");
    assert!(visual.debug_bounds("board-list-viewport").is_some());
    click(visual, "board-view-board");
    board.read_with(visual, |b, _| {
        assert_eq!(
            b.kanban.lanes[Column::Working as usize]
                .0
                .borrow()
                .base_handle
                .offset(),
            offset
        )
    });
    assert!(
        visual.debug_bounds("board-card-16").is_some(),
        "the returning viewport uses the retained handle"
    );
    board.update(visual, |b, cx| {
        b.kanban.lanes[Column::Working as usize]
            .scroll_to_item_strict(0, gpui::ScrollStrategy::Top);
        cx.notify();
    });
    visual.run_until_parked();

    click(visual, "board-expand-1");
    assert!(visual.debug_bounds("board-discuss-1").is_some());
    assert!(visual.debug_bounds("board-peek-content").is_some());
    assert!(visual.debug_bounds("board-kanban-details").is_none());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-kanban-details").is_none());
    board.read_with(visual, |b, _| {
        assert_eq!(b.list.publication_count(), publications)
    });
}
#[gpui::test]
fn kanban_2534_queue_eight_rows_more_opens_expanded_list_and_saves(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    assert_eq!(
        visual.debug_bounds("board-lane-Queue").unwrap().size.width,
        px(240.)
    );
    for selector in [
        "board-queue-row-0",
        "board-queue-row-1",
        "board-queue-row-2",
        "board-queue-row-3",
        "board-queue-row-4",
        "board-queue-row-5",
        "board-queue-row-6",
        "board-queue-row-7",
    ] {
        assert_eq!(visual.debug_bounds(selector).unwrap().size.height, px(30.));
    }
    assert!(visual.debug_bounds("board-queue-row-8").is_none());
    assert!(visual.debug_bounds("board-card-17").is_none());
    assert_eq!(
        visual.debug_bounds("board-expand-17").unwrap().center().y,
        visual.debug_bounds("board-queue-row-0").unwrap().center().y
    );
    // A source publication promotes an Oleg request in both presentations.
    board.update(visual, |board, cx| {
        board
            .snapshot
            .forgejo
            .value
            .get_mut(&24)
            .unwrap()
            .labels
            .push(okilum_t3::board::IssueLabel {
                name: "from-oleg".into(),
            });
        // Leave one visible Queue neighbour with a different origin so the
        // active Bug chip still exercises the shared origin-first comparator.
        board
            .snapshot
            .forgejo
            .value
            .get_mut(&23)
            .unwrap()
            .labels
            .push(okilum_t3::board::IssueLabel {
                name: "from-oleg".into(),
            });
        board
            .snapshot
            .forgejo
            .value
            .get_mut(&17)
            .unwrap()
            .labels
            .retain(|label| label.name != "bug");
        board.filter_cache.invalidate();
        board.list.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert_eq!(
        visual.debug_bounds("board-expand-23").unwrap().center().y,
        visual.debug_bounds("board-queue-row-0").unwrap().center().y
    );
    // Exercise the actual shared chip control, not just the filter model.
    assert!(
        visual.debug_bounds("board-expand-17").is_some(),
        "unfiltered positive control"
    );
    click(visual, "board-chip-bugs");
    assert!(
        visual.debug_bounds("board-expand-17").is_none(),
        "chip removes a non-bug from compact Queue"
    );
    assert_eq!(
        visual.debug_bounds("board-expand-23").unwrap().center().y,
        visual.debug_bounds("board-queue-row-0").unwrap().center().y,
        "shared origin then recency order survives filtering"
    );
    assert!(
        visual.debug_bounds("board-expand-23").unwrap().top()
            < visual.debug_bounds("board-expand-24").unwrap().top()
    );
    board.update(visual, |b, _| {
        let prepared = b.prepare_list();
        assert_eq!(prepared.counts, b.filtered_projection().counts);
        assert_eq!(prepared.counts[Column::Queue as usize], 222);
    });
    click(visual, "board-queue-more");
    board.read_with(visual, |b, cx| {
        assert_eq!(b.filter.chip, filters::Chip::Bugs);
        assert_eq!(b.list.preferences.mode, list::Mode::List);
        assert!(!b.list.preferences.collapsed[Column::Queue as usize]);
        let restored = list::State::load(area.path(), cx);
        assert_eq!(restored.preferences.mode, list::Mode::List);
        assert!(!restored.preferences.collapsed[Column::Queue as usize]);
    });
    assert!(visual.debug_bounds("board-list-group-Queue").is_some());
    assert!(visual.debug_bounds("board-list-row-17").is_none());
    assert!(
        visual.debug_bounds("board-list-row-23").unwrap().top()
            < visual.debug_bounds("board-list-row-24").unwrap().top()
    );
    assert!(visual.debug_bounds("board-columns").is_none());
}
#[gpui::test]
fn kanban_2534_verified_rail_expands_and_remains_in_narrow_viewport(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (_, visual) = open(cx, area.path());
    assert!(visual.debug_bounds("board-lane-Verified").is_none());
    click(visual, "board-verified-toggle");
    assert_eq!(
        visual
            .debug_bounds("board-lane-Verified")
            .unwrap()
            .size
            .width,
        px(268.)
    );
    assert!(visual.debug_bounds("board-card-240").is_some());
    visual.simulate_resize(size(px(600.), px(1000.)));
    visual.run_until_parked();
    let center = visual.debug_bounds("project-board").unwrap();
    let rail = visual.debug_bounds("board-verified-toggle").unwrap();
    assert_eq!(rail.size.width, px(34.));
    assert!(rail.right() <= center.right());
    click(visual, "board-verified-toggle");
    assert!(visual.debug_bounds("board-lane-Verified").is_none());
}

#[gpui::test]
fn kanban_2534_live_dot_requires_unsettled_active_run(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    assert!(visual.debug_bounds("board-agent-live-dot").is_some());
    board.update(visual, |board, cx| {
        board.snapshot.t3.value[0].active_run_id = None;
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-agent-link-1").is_some(),
        "bound avatar remains"
    );
    assert!(
        visual.debug_bounds("board-agent-live-dot").is_none(),
        "assignment alone never implies a live run"
    );
}
