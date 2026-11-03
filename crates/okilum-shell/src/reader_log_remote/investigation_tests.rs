use super::*;
use ::core::prelude::v1::test;
use okilum_core::remote_log::fake::{investigation_record, FakeKibana, Scenario};

fn settle(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
}
fn wait_for(
    visual: &mut VisualTestContext,
    panel: &Entity<Panel>,
    predicate: impl Fn(&Panel) -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        settle(visual);
        if panel.read_with(visual, |panel, _| predicate(panel)) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "remote worker did not finish: {}",
            panel.read_with(visual, |panel, _| panel.message.clone())
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn click(visual: &mut VisualTestContext, selector: &'static str) {
    settle(visual);
    let position = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"))
        .center();
    visual.simulate_mouse_move(position, None, Modifiers::default());
    settle(visual);
    visual.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    settle(visual);
    visual.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    settle(visual);
}

#[gpui::test]
fn request_details_back_live_and_saved_view_use_fake_server_and_keep_investigation(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let server = FakeKibana::start("127.0.0.1:0", 603, Scenario::default()).unwrap();
    server.state.lock().unwrap().records = (0..603).map(investigation_record).collect();
    let mut source = Source::direct(
        "Synthetic",
        &server.endpoint,
        "remote-*:events",
        "@timestamp",
    )
    .unwrap();
    source.discover.time = TimeRange {
        from: "2026-11-01T00:00:00Z".into(),
        to: "2026-11-05T00:00:00Z".into(),
    };
    source.discover.filters = vec![serde_json::json!({"query":{"term":{"service":"Checkout"}}})];
    source.discover.sort[0].order = "desc".into();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
        reader_ui_state::install(&state, cx);
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        entity = Some(panel.clone());
        Root::new(panel, window, cx)
    });
    visual.simulate_resize(size(px(1600.), px(1600.)));
    let panel = entity.unwrap();
    settle(visual);
    panel.update_in(visual, |panel, window, cx| {
        panel.sources = vec![source.clone()];
        panel.guided.test_key = Some(b"test-secret".to_vec());
        panel.choose(source.clone(), window, cx);
        panel.apply(window, cx);
    });
    wait_for(visual, &panel, |panel| {
        !panel.busy
            && panel.view.is_some()
            && panel.guided.catalog.as_ref().is_some_and(Result::is_ok)
    });
    let view = panel.read_with(visual, |panel, _| panel.view.clone().unwrap());
    let initial = panel.read_with(visual, |panel, _| panel.data.clone().unwrap());
    assert_eq!(
        initial.events.len(),
        200,
        "positive control: initial history is a bounded first page"
    );
    let selected = initial
        .events
        .iter()
        .position(|event| event["_source"]["flow"] == "request-0")
        .unwrap();
    view.update_in(visual, |view, window, cx| {
        view.choose_field(Some("flow".into()), false, cx);
        view.choose_field(Some("details".into()), true, cx);
        view.toggle_column("flow", cx);
        view.add_chip(
            okilum_core::log::chips::FilterChip::new(
                okilum_core::log::query::FieldRef::Key("active".into()),
                vec![okilum_core::log::chips::FilterValue::Value {
                    kind: okilum_core::log::ValueKind::Bool,
                    text: "true".into(),
                }],
                okilum_core::log::chips::FilterMode::Include,
            ),
            window,
            cx,
        );
        view.select(selected, true, window, cx);
    });
    settle(visual);
    wait_for(visual, &panel, |panel| {
        !panel.busy && panel.guided.catalog.as_ref().is_some_and(Result::is_ok)
    });
    let initial = panel.read_with(visual, |panel, _| panel.data.clone().unwrap());
    let parent_chips = view.read_with(visual, |view, _| view.filter_chips().to_vec());
    click(visual, "log-show-request");
    wait_for(visual, &panel, |panel| {
        !panel.busy
            && panel
                .applied
                .as_ref()
                .is_some_and(|s| s.discover.filters.len() == 2)
    });
    panel.read_with(visual, |panel, cx| {
        let applied = panel.applied.as_ref().unwrap();
        assert_eq!(applied.discover.time, source.discover.time);
        assert_eq!(applied.discover.filters[0], source.discover.filters[0]);
        assert_eq!(applied.discover.sort[0].order, "asc");
        assert_eq!(panel.data.as_ref().unwrap().events.len(), 101);
        let view = view.read(cx);
        assert!(view
            .investigation_label()
            .unwrap()
            .starts_with("Kibana: 101 events for id request-0 in "));
        assert_eq!(view.columns(cx), ["flow"]);
    });
    click(visual, "log-request-back");
    wait_for(visual, &panel, |panel| {
        !panel.busy
            && panel
                .applied
                .as_ref()
                .is_some_and(|s| s.discover.sort[0].order == "desc")
    });
    assert!(panel.read_with(visual, |panel, _| Arc::ptr_eq(
        panel.data.as_ref().unwrap(),
        &initial
    )));
    view.read_with(visual, |view, cx| {
        assert_eq!(view.selected, Some(selected));
        assert_eq!(view.filter_chips(), parent_chips);
        assert_eq!(view.columns(cx), ["flow"]);
        assert_eq!(view.message_field(cx).as_deref(), Some("details"));
    });
    click(visual, "log-show-request");
    wait_for(visual, &panel, |panel| {
        !panel.busy
            && panel
                .applied
                .as_ref()
                .is_some_and(|s| s.discover.filters.len() == 2)
    });
    // Live keeps a newly added chip and layout, and deduplicates document identities.
    view.update_in(visual, |view, window, cx| {
        view.add_chip(
            okilum_core::log::chips::FilterChip::new(
                okilum_core::log::query::FieldRef::Key("active".into()),
                vec![okilum_core::log::chips::FilterValue::Value {
                    kind: okilum_core::log::ValueKind::Bool,
                    text: "true".into(),
                }],
                okilum_core::log::chips::FilterMode::Include,
            ),
            window,
            cx,
        );
    });
    wait_for(visual, &panel, |panel| {
        !panel.busy && panel.guided.catalog.as_ref().is_some_and(Result::is_ok)
    });
    let chips = view.read_with(visual, |view, _| view.filter_chips().to_vec());
    assert_eq!(
        chips.len(),
        1,
        "positive control: live starts with an active chip"
    );
    let mut fresh = investigation_record(606);
    fresh["_id"] = serde_json::json!("new-live-event");
    server.add(fresh.clone());
    server.add(fresh);
    click(visual, "remote-start");
    click(visual, "remote-confirm-fixed");
    wait_for(visual, &panel, |panel| {
        !panel.busy && panel.following && panel.data.as_ref().is_some_and(|d| d.events.len() == 102)
    });
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel
                .data
                .as_ref()
                .unwrap()
                .events
                .iter()
                .filter(|event| event["_id"] == "new-live-event")
                .count(),
            1
        )
    });
    view.read_with(visual, |view, cx| {
        assert_eq!(view.filter_chips(), chips);
        assert_eq!(view.columns(cx), ["flow"]);
        assert!(view.investigation.live && view.investigation.last_update.is_some());
        assert_eq!(view.visible_len(), 102);
    });
    click(visual, "remote-stop");
    view.update_in(visual, |view, window, cx| {
        view.begin_saving(window, cx);
        view.saving
            .as_ref()
            .unwrap()
            .input
            .update(cx, |i, cx| i.set_value("Request view", window, cx));
        view.commit_saving(window, cx);
    });
    settle(visual);
    let saved = visual.update(|_, cx| {
        reader_ui_state::saved_log_filters(cx)
            .get("Request view")
            .unwrap()
            .clone()
    });
    assert_eq!(
        saved.remote_source.as_ref().unwrap().discover.filters.len(),
        3
    );
    assert_eq!(saved.remote_range.as_ref(), Some(&source.discover.time));
    assert_eq!(saved.message_field.as_deref(), Some("details"));
    assert_eq!(saved.correlation_field.as_deref(), Some("flow"));
    assert_eq!(saved.filter_chips, chips);
    // Round-trip through JSON and reapply using the same OS credential identity.
    let encoded = serde_json::to_string(&saved).unwrap();
    assert!(!encoded.contains("test-secret"));
    let restored: okilum_core::log::saved_query::SavedQuery =
        serde_json::from_str(&encoded).unwrap();
    assert_eq!(restored, saved);
    view.update_in(visual, |view, window, cx| {
        view.choose_field(None, true, cx);
        view.apply_saved("Request view", window, cx);
    });
    wait_for(visual, &panel, |panel| !panel.busy);
    panel.read_with(visual, |panel, cx| {
        assert!(
            panel.error.is_none(),
            "same-source saved views reuse the scoped worker credential"
        );
        assert_eq!(panel.requested.as_ref().unwrap().discover.filters.len(), 3);
        assert_eq!(view.read(cx).message_field(cx).as_deref(), Some("details"));
        assert_eq!(view.read(cx).filter_chips(), chips);
        assert_eq!(panel.data.as_ref().unwrap().events.len(), 102);
    });
    cx.quit();
}

#[gpui::test]
fn request_labels_report_partial_and_truncated_results(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(dir.path(), cx);
    });
    let mut source =
        Source::direct("Synthetic", "https://logs.example", "events", "@timestamp").unwrap();
    source.discover.filters.push(serde_json::json!({"query":okilum_core::log::correlation::server_query("request-0","flow")}));
    let loaded = Arc::new(remote_log::live::Loaded {
        source: source.clone(),
        range: source.discover.time.clone(),
        more: false,
        quality: remote_log::result::ResultStatus {
            partial: true,
            truncated: true,
            ..Default::default()
        },
        window: Arc::new(
            EventWindow::new(vec![Arc::new(investigation_record(0))], &source).unwrap(),
        ),
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| reader_log::LogView::remote(loaded.window.clone(), window, cx));
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    view.update_in(visual, |view, window, cx| {
        view.remote_context(loaded.clone(), false, false, cx);
        view.follow_request(
            okilum_core::log::correlation::RequestId {
                key: "flow".into(),
                value: "request-0".into(),
            },
            window,
            cx,
        );
    });
    settle(visual);
    view.read_with(visual, |view, _| {
        let label = view.investigation_label().unwrap();
        assert!(label.contains("1 events"));
        assert!(label.contains("Partial"), "{label}");
        assert!(label.contains("Truncated"), "{label}");
    });
    assert!(
        visual.debug_bounds("log-request-count").is_some(),
        "quality label is actually drawn"
    );
    cx.quit();
}

#[gpui::test]
fn default_message_field_uses_full_remote_schema_beyond_the_first_page(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let source =
        Source::direct("Synthetic", "https://logs.example", "events", "@timestamp").unwrap();
    let events = (0..201)
        .map(|index| {
            let mut event = investigation_record(index);
            if index == 200 {
                event["_source"]["message"] =
                    serde_json::json!("Full text from a sparse message field");
            }
            Arc::new(event)
        })
        .collect();
    let data = Arc::new(EventWindow::new(events, &source).unwrap());
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| reader_log::LogView::remote(data, window, cx));
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    view.update_in(visual, |view, window, cx| {
        view.select(200, true, window, cx)
    });
    settle(visual);
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.total(),
            201,
            "positive control: a second page is loaded"
        );
        assert_eq!(view.message_field(cx).as_deref(), Some("message"));
    });
    assert!(
        visual.debug_bounds("log-full-message").is_some(),
        "the default message has a full wrapped detail"
    );
    cx.quit();
}
