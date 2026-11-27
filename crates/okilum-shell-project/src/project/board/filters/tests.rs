use super::*;
use ::core::prelude::v1::test;
use gpui::{TestAppContext, VisualTestContext};
use std::collections::BTreeMap;

fn binding() -> Binding {
    let binding = Binding::parse(
        r#"
version: 1
title: Synthetic filter fixture
repository: https://example.invalid/synthetic/board
issues:
  - {number: 123, expected: 'Исправить поиск', executor: Editing}
  - {number: 456, expected: 'Tester report', executor: Tests}
  - {number: 789, expected: 'An unlabelled task', executor: Editing}
"#,
    )
    .unwrap();
    assert_eq!(
        binding.issues.len(),
        3,
        "the fixture must exercise actual issue scopes"
    );
    assert!(
        binding.card_problems.is_empty(),
        "invalid fixture: {:?}",
        binding.card_problems
    );
    binding
}
fn issues() -> BTreeMap<u64, Issue> {
    [(123, "Исправить ПОИСК", vec!["from-oleg", "bug", "epic: board"]),
     (456, "Tester report", vec!["from-tester", "bug"]),
     (789, "An unlabelled task", vec![])].into_iter().map(|(number, title, labels)| {
        let issue = serde_json::from_value(serde_json::json!({"number":number,"title":title,"state":"open",
            "labels":labels.into_iter().map(|name| serde_json::json!({"name":name})).collect::<Vec<_>>() })).unwrap();
        (number, issue)
    }).collect()
}
fn open<'a>(cx: &'a mut TestAppContext, area: &Path) -> (Entity<Board>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let board = cx.new(|cx| {
            let mut board =
                Board::with_reader(area.into(), |_| panic!("offline fixture"), window, cx);
            board.list.preferences.mode = list::Mode::Board;
            board.binding = Some(binding());
            board.snapshot.forgejo.value = issues();
            board
        });
        Root::new(board, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(900.)));
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

#[test]
fn issue2532_search_handles_cyrillic_case_and_exact_numbers() {
    for query in ["поиск", "ПОИСК", "#123", "123", "  пОиСк  "] {
        let query = query.trim().to_lowercase();
        assert!(
            search_matches("Исправить ПОИСК", Some(123), &query),
            "{query}"
        );
    }
    for query in ["#12", "12", "#456", "unknown"] {
        assert!(
            !search_matches("Исправить ПОИСК", Some(123), query),
            "{query}"
        );
    }
    assert!(
        search_matches("Build 123 failed", Some(456), "123"),
        "title substring also matches"
    );
}
#[test]
fn issue2532_extra_labels_and_executor_intersect_search_and_chip() {
    let scope = binding().issues.remove(0);
    let issues = issues();
    let issue = issues.get(&123);
    let mut model = Model {
        labels: ["bug".into(), "epic: board".into()].into(),
        executor: Some("Editing".into()),
        ..Default::default()
    };
    assert!(model.extras_match(&scope, issue, "поиск"));
    assert!(Chip::Oleg.matches(issue, false));
    assert!(!Chip::Testers.matches(issue, false));
    model.labels.insert("missing".into());
    assert!(!model.extras_match(&scope, issue, ""));
    model.labels.clear();
    model.executor = Some("Tests".into());
    assert!(!model.extras_match(&scope, issue, ""));
}
#[test]
fn issue2532_active_chip_requires_unique_current_task_and_active_run() {
    let mut scope = binding().issues.remove(0);
    scope.agent_thread_id = Some("agent".into());
    let mut thread: okilum_t3::ThreadSummary = serde_json::from_value(serde_json::json!({
        "threadId":"agent","title":"Fixture","status":"running","activeRunId":"run",
        "providerInstanceId":"codex","model":"model","runtimeMode":"full-access", "updatedAt":"2026-11-03T20:00:00Z"
    }))
    .unwrap();
    assert!(!active(&scope, &[thread.clone()]));
    scope.current_task = true;
    assert!(active(&scope, &[thread.clone()]));
    assert!(!active(&scope, &[thread.clone(), thread.clone()]));
    thread.settled = true;
    assert!(!active(&scope, &[thread.clone()]));
    thread.settled = false;
    thread.active_run_id = None;
    assert!(!active(&scope, &[thread.clone()]));
    thread.active_run_id = Some("run".into());
    thread.status = "idle".into();
    assert!(!active(&scope, &[thread]));
}
#[test]
fn issue2532_preferences_are_isolated_per_project_and_survive_reload() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    let model = Model {
        chip: Chip::Bugs,
        search: "ПОИСК".into(),
        labels: ["epic: board".into()].into(),
        executor: Some("Editing".into()),
    };
    model.save(directory.path(), &first).unwrap();
    assert_eq!(Model::load(directory.path(), &first), model);
    assert_eq!(Model::load(directory.path(), &second), Model::default());
    Model::default().save(directory.path(), &first).unwrap();
    assert_eq!(Model::load(directory.path(), &first), Model::default());
}
#[gpui::test]
fn issue2532_native_chip_click_search_escape_and_live_counts(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    assert!(visual.debug_bounds("board-expand-123").is_some());
    assert!(
        visual.debug_bounds("board-expand-456").is_some(),
        "positive control: both cards start visible"
    );
    assert!(visual.debug_bounds("board-chip-count-all-3").is_some());
    click(visual, "board-chip-oleg");
    assert!(visual.debug_bounds("board-expand-123").is_some());
    assert!(visual.debug_bounds("board-expand-456").is_none());
    board.read_with(visual, |board, _| assert_eq!(board.filter.chip, Chip::Oleg));
    visual.simulate_keystrokes("/");
    board.update_in(visual, |board, window, cx| {
        assert!(board
            .filter_ui
            .search
            .read(cx)
            .focus_handle(cx)
            .is_focused(window))
    });
    visual.simulate_input("пОиСк");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-expand-123").is_some());
    assert!(visual.debug_bounds("board-chip-count-bugs-1").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    board.update_in(visual, |board, window, cx| {
        assert!(board.filter.search.is_empty());
        assert!(board.focus.is_focused(window));
        assert_eq!(board.filter_ui.search.read(cx).value().as_str(), "");
    });
    click(visual, "board-chip-all");
    assert!(visual.debug_bounds("board-expand-456").is_some());
    board.update(visual, |board, cx| {
        board
            .snapshot
            .forgejo
            .value
            .get_mut(&789)
            .unwrap()
            .labels
            .push(okilum_t3::board::IssueLabel { name: "bug".into() });
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-chip-count-bugs-3").is_some(),
        "snapshot changes update counts"
    );
    visual.simulate_keystrokes("/");
    visual.simulate_input("#999");
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-filter-empty").is_some());
    click(visual, "board-reset-filters");
    assert!(visual.debug_bounds("board-expand-456").is_some());
    board.read_with(visual, |board, _| {
        assert_eq!(board.filter, Model::default())
    });
}
#[gpui::test]
fn issue2532_projection_reuses_counts_and_filters_before_grouping(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    board.update(visual, |board, _| {
        let first = board.filtered_projection();
        let second = board.filtered_projection();
        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged frames share the projection and counts"
        );
        board.filter.chip = Chip::Bugs;
        let filtered = board.filtered_projection();
        assert!(!Arc::ptr_eq(&first, &filtered));
        assert_eq!(
            filtered
                .binding
                .issues
                .iter()
                .map(|s| s.number)
                .collect::<Vec<_>>(),
            [123, 456]
        );
        assert_eq!(filtered.counts.iter().sum::<usize>(), 2);
        assert_eq!(filtered.chip_counts, [3, 1, 2, 1, 0]);
        assert!(filtered.labels.contains("epic: board"));
        assert_eq!(
            filtered.executors,
            ["Editing".into(), "Tests".into()].into()
        );
        board.filter.search = "#123".into();
        let searched = board.filtered_projection();
        assert_eq!(searched.counts.iter().sum::<usize>(), 1);
        assert_eq!(searched.chip_counts, [1, 1, 1, 0, 0]);
    });
}

#[test]
fn issue2532_local_tasks_do_not_invent_issue_labels_or_agents() {
    let card = okilum_t3::board::local::Card {
        id: "00000000-0000-4000-8000-000000000123".into(),
        title: "Проверить ПОИСК".into(),
        details: String::new(),
        state: Default::default(),
    };
    let mut model = Model {
        search: "поиск".into(),
        ..Default::default()
    };
    assert!(model.matches_local(&card));
    model.chip = Chip::Oleg;
    assert!(!model.matches_local(&card));
    model.chip = Chip::All;
    model.labels.insert("bug".into());
    assert!(!model.matches_local(&card));
    model.labels.clear();
    model.executor = Some("Editing".into());
    assert!(!model.matches_local(&card));
    model.executor = None;
    model.search = "#123".into();
    assert!(
        !model.matches_local(&card),
        "local identity is not an issue number"
    );
}

#[gpui::test]
fn issue2532_pending_refresh_is_not_an_empty_search_result(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    board.update(visual, |board, cx| {
        board.filter.search = "#999".into();
        assert!(board.snapshot.begin().is_some());
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("board-filter-empty").is_none());
    board.update(visual, |board, cx| {
        board.snapshot.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-filter-empty").is_some(),
        "completed zero matches have the reset action"
    );
    board.update(visual, |board, cx| {
        board.snapshot.forgejo.last_success = Some(now());
        assert!(board.snapshot.begin().is_some());
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("board-filter-empty").is_some(),
        "refresh retains a completed emptiness decision"
    );
}

#[test]
fn issue2532_delivery_cache_expires_at_the_exact_source_freshness_boundary() {
    let source = Source {
        value: (),
        last_success: Some(100),
        error: None,
    };
    assert!(okilum_t3::board::delivery::problem(&source, 160, 30).is_none());
    assert!(okilum_t3::board::delivery::problem(&source, 161, 30).is_some());
    assert_eq!(freshness_expiry(160, 30, [100, 120].into_iter()), 161);
    assert_eq!(freshness_expiry(161, 30, [100, 120].into_iter()), 181);
    assert_eq!(freshness_expiry(181, 30, [100, 120].into_iter()), u64::MAX);
}
#[test]
fn issue2532_merge_cache_preserves_the_exact_recent_window_and_future_boundary() {
    let pull = okilum_t3::board::live::Pull {
        number: 1,
        merged_at: Some(100),
        checks: String::new(),
        ..Default::default()
    };
    assert!(!pull.recent(99));
    assert_eq!(merge_expiry(99, 100), Some(100));
    assert!(pull.recent(86499));
    assert_eq!(merge_expiry(86499, 100), Some(86500));
    assert!(!pull.recent(86500));
    assert_eq!(merge_expiry(86500, 100), None);
}

#[gpui::test]
fn cached_kanban_membership_reuses_binding_until_filter_or_source_changes(cx: &mut TestAppContext) {
    let area = tempfile::tempdir().unwrap();
    let (board, visual) = open(cx, area.path());
    let prepared = board.read_with(visual, |board, _| board.lane_preparations.get());
    assert!(
        prepared > 0,
        "positive control: actual Kanban lanes prepare shared row membership"
    );
    let first = board.update(visual, |board, _| board.filtered_projection_at(100));
    let later = board.update(visual, |board, _| board.filtered_projection_at(101));
    assert!(
        Arc::ptr_eq(&first, &later),
        "viewport time alone must not clone the source"
    );
    board.update(visual, |board, cx| {
        board.binding.as_mut().unwrap().issues[0].expected = "Published context".into();
        board.filter_cache.invalidate();
        cx.notify();
    });
    visual.run_until_parked();
    let changed = board.update(visual, |board, _| board.filtered_projection_at(101));
    assert!(
        board.read_with(visual, |board, _| board.lane_preparations.get()) > prepared,
        "source publication prepares the new membership"
    );
    assert!(!Arc::ptr_eq(&first, &changed));
    assert_eq!(changed.binding.issues[0].expected, "Published context");
    assert_ne!(
        first.binding.issues[0].expected,
        changed.binding.issues[0].expected
    );
    let prepared = board.read_with(visual, |board, _| board.lane_preparations.get());
    click(visual, "board-chip-bugs");
    let filtered = board.update(visual, |board, _| board.filtered_projection());
    let filtered_prepared = board.read_with(visual, |board, _| board.lane_preparations.get());
    assert!(
        filtered_prepared > prepared,
        "filter interaction prepares its changed membership"
    );
    assert_eq!(filtered.counts.iter().sum::<usize>(), 2);
    for width in [1300., 1100., 1400.] {
        visual.simulate_resize(size(px(width), px(900.)));
        visual.run_until_parked();
        board.update(visual, |board, _| {
            let current = board.filtered_projection();
            assert!(Arc::ptr_eq(&filtered, &current));
            assert!(Arc::ptr_eq(&filtered.binding, &current.binding));
            assert_eq!(
                board.lane_preparations.get(),
                filtered_prepared,
                "resize must not rehash/rebuild filtered row membership"
            );
        });
    }
}
