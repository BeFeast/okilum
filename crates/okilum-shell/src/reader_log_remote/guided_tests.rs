//! Real GPUI clicks drive the same HTTP worker and loopback transport as production.
use super::*;
use ::core::prelude::v1::test;
use okilum_core::remote_log::fake::{FakeKibana, Scenario};

fn guided_frame(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
}
fn guided_click(visual: &mut VisualTestContext, selector: &'static str) {
    guided_frame(visual);
    let position = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"))
        .center();
    visual.simulate_mouse_move(position, None, Modifiers::default());
    guided_frame(visual);
    visual.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    guided_frame(visual);
    visual.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    guided_frame(visual);
}

#[gpui::test]
async fn guided_source_preset_scope_refresh_and_mid_session_auth_keep_state(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let server = FakeKibana::start(
        "127.0.0.1:0",
        12,
        Scenario {
            async_polls: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let stamp = (now - time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    for record in &mut server.state.lock().unwrap().records {
        record["_source"]["@timestamp"] = serde_json::json!(stamp);
        record["sort"][0] =
            serde_json::json!((now - time::Duration::hours(1)).unix_timestamp_nanos() as i64);
    }
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
    let mut source = Source::direct(
        "Synthetic",
        &server.endpoint,
        "remote-*:events",
        "@timestamp",
    )
    .unwrap();
    source.discover.time = TimeRange {
        from: (now - time::Duration::hours(2))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        to: now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
    };
    let base_filters = vec![serde_json::json!({
        "meta":{"controlledBy":"okilum-guided","alias":"Opaque imported filter"},
        "query":{"exists":{"field":"@timestamp"}}
    })];
    source.discover.filters = base_filters.clone();
    remote_log::save_source(&state, &vault, &source).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
        reader_ui_state::install(&state, cx);
    });
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1600.), px(1200.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    visual
        .condition(&panel, |panel, _| panel.sources_loaded)
        .await;
    panel.update(visual, |panel, _| {
        panel.guided.test_key = Some(b"test-secret".to_vec())
    });
    visual.run_until_parked();
    let bounds = visual.debug_bounds("remote-source-0").unwrap();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual
        .condition(&panel, |panel, _| {
            !panel.busy && panel.data.is_some() && panel.guided.catalog.is_some()
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(panel.data.as_ref().unwrap().events.len(), 12);
        assert!(panel
            .guided
            .catalog
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .iter()
            .any(|f| f.name == "service.keyword"));
        assert!(panel.guided.connection.starts_with("Connected"));
        assert!(
            !panel.guided.advanced,
            "guided controls are the initial path"
        );
    });
    let searches = server.state.lock().unwrap().searches;
    visual.run_until_parked();
    let bounds = visual.debug_bounds("remote-time-4").unwrap();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual
        .condition(&panel, |panel, _| {
            !panel.busy
                && panel
                    .applied
                    .as_ref()
                    .is_some_and(|s| s.discover.time.from == "now-24h")
        })
        .await;
    assert!(
        server.state.lock().unwrap().searches > searches,
        "positive control: preset re-queried the server"
    );
    visual
        .condition(&panel, |panel, _| panel.guided.catalog.is_some())
        .await;
    guided_click(visual, "remote-pin-scope");
    let field_index = panel.read_with(visual, |panel, _| {
        panel
            .guided
            .catalog
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .iter()
            .position(|f| f.name == "service.keyword")
            .unwrap()
    });
    let keys = std::iter::repeat_n("down", field_index + 1)
        .chain(std::iter::once("enter"))
        .collect::<Vec<_>>()
        .join(" ");
    visual.simulate_keystrokes(&keys);
    guided_frame(visual);
    visual
        .condition(&panel, |panel, _| {
            panel.guided.counts.contains_key("service.keyword")
        })
        .await;
    panel.read_with(visual, |panel, _| {
        let counts = panel.guided.counts["service.keyword"].as_ref().unwrap();
        assert_eq!(counts.buckets.len(), 2);
        assert_eq!(counts.buckets[0].count, 6);
    });
    visual.run_until_parked();
    let bounds = visual.debug_bounds("remote-scope-value-0").unwrap();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual
        .condition(&panel, |panel, _| {
            !panel.busy && panel.data.as_ref().is_some_and(|d| d.events.len() == 6)
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert!(panel
            .data
            .as_ref()
            .unwrap()
            .events
            .iter()
            .all(|e| e["_source"]["service"] == "Checkout"));
        assert_eq!(
            panel
                .quality
                .as_ref()
                .unwrap()
                .total
                .as_ref()
                .unwrap()
                .value,
            6
        );
    });
    assert_eq!(
        remote_log::load_sources(&state, &vault).unwrap()[0].scope_fields,
        vec!["service.keyword"]
    );
    assert_eq!(
        remote_log::load_sources(&state, &vault).unwrap()[0]
            .discover
            .filters,
        base_filters,
        "opaque base filters survive; compiled session filters are not persisted"
    );
    assert!(
        server.state.lock().unwrap().calls.iter().any(|call| {
            call.body
                .to_string()
                .contains("\"exists\":{\"field\":\"@timestamp\"}")
        }),
        "positive control: imported filter reached the actual search DSL"
    );
    visual
        .condition(&panel, |panel, _| panel.guided.catalog.is_some())
        .await;
    visual.run_until_parked();
    let bounds = visual.debug_bounds("remote-scope-0").unwrap();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual
        .condition(&panel, |panel, _| {
            panel.guided.counts.contains_key("service.keyword")
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel.guided.counts["service.keyword"]
                .as_ref()
                .unwrap()
                .buckets
                .len(),
            1,
            "server counts changed with scope"
        )
    });
    point_and_click_server_chips(&panel, visual, &server).await;
    guided_follow_back_live_saved(&panel, visual, &server).await;
    // A real 401 after a successful six-row query. Retention has a positive control:
    // the HTTP server saw the failed refresh, then replacement performs a new search.
    let data = panel.read_with(visual, |panel, _| panel.data.clone().unwrap());
    server.state.lock().unwrap().scenario.failure_status = Some(401);
    panel.update(visual, |panel, cx| {
        panel.guided.open_scope = None;
        cx.notify();
    });
    visual.run_until_parked();
    let calls = server.state.lock().unwrap().calls.len();
    let bounds = visual.debug_bounds("remote-refresh").unwrap();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual
        .condition(&panel, |panel, _| panel.auth_error && !panel.busy)
        .await;
    assert!(server.state.lock().unwrap().calls.len() > calls);
    panel.read_with(visual, |panel, _| {
        assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(), &data));
        assert_eq!(panel.guided.scope.len(), 1);
        assert_eq!(
            panel.draft.as_ref().unwrap().scope_fields,
            vec!["service.keyword"]
        );
    });
    server.state.lock().unwrap().scenario.failure_status = None;
    visual.run_until_parked();
    let bounds = visual.debug_bounds("remote-replace-key").unwrap();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual
        .condition(&panel, |panel, _| !panel.busy && !panel.auth_error)
        .await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(panel.data.as_ref().unwrap().events.len(), 6);
        assert!(
            !Arc::ptr_eq(panel.data.as_ref().unwrap(), &data),
            "positive control: replacement returned a fresh snapshot"
        );
        assert_eq!(panel.guided.scope.len(), 1);
    });
    // Catalog permission failures degrade over the working search route; they
    // must not turn a usable connection into an API-key replacement flow.
    server.state.lock().unwrap().scenario.catalog_status = Some(401);
    guided_click(visual, "remote-refresh");
    visual
        .condition(&panel, |panel, _| {
            !panel.busy
                && panel.guided.catalog.as_ref().is_some_and(Result::is_ok)
                && panel.guided.discovery.as_ref().is_some_and(|d| d.fallback)
        })
        .await;
    assert!(!panel.read_with(visual, |panel, _| panel.auth_error));
    panel.read_with(visual, |panel, cx| {
        assert_eq!(panel.data.as_ref().unwrap().events.len(), 6);
        assert_eq!(
            panel.view.as_ref().unwrap().read(cx).columns(cx),
            vec!["service", "duration"]
        );
    });
    // Navigation keeps the working fallback mapping.
    let view = panel.read_with(visual, |panel, _| panel.view.clone().unwrap());
    view.update_in(visual, |view, window, cx| view.select(0, true, window, cx));
    guided_click(visual, "log-show-request");
    loaded(&panel, visual, 1).await;
    assert!(!panel.read_with(visual, |panel, _| panel.auth_error));
    server.state.lock().unwrap().scenario.catalog_status = None;
    guided_click(visual, "remote-refresh");
    loaded(&panel, visual, 1).await;
    assert!(!panel.read_with(visual, |panel, _| panel.auth_error));
    guided_click(visual, "log-request-back");
    loaded(&panel, visual, 6).await;
    assert_eq!(server.state.lock().unwrap().rejected_headers, 0);
    // A new mapping catalog may succeed while history for an edited dataset
    // fails. Retained rows must never aggregate through that new catalog.
    let retained = panel.read_with(visual, |panel, _| panel.data.clone().unwrap());
    server.state.lock().unwrap().scenario.failure_status = Some(400);
    panel.update_in(visual, |panel, window, cx| {
        // This test deliberately starts a different dataset; its old scope is
        // not a confirmed mapping for that dataset.
        panel.guided.scope.clear();
        panel.pattern.update(cx, |input, cx| {
            input.set_value("other-*:events", window, cx)
        });
        panel.apply(window, cx);
    });
    visual
        .condition(&panel, |panel, _| {
            !panel.busy
                && panel.guided.catalog.as_ref().is_some_and(Result::is_ok)
                && panel.error.is_some()
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(), &retained));
        assert!(!panel.catalog_matches_applied());
        assert_ne!(
            panel.guided.catalog_scope.as_ref(),
            Some(&retained.identity_scope)
        );
    });
    assert!(
        server
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|call| { call.body["params"]["index"] == "other-*:events" }),
        "positive control: the edited dataset reached HTTP"
    );
    let calls = server.state.lock().unwrap().calls.len();
    guided_click(visual, "log-header-filter-0");
    let view = panel.read_with(visual, |panel, _| panel.view.clone().unwrap());
    let picker = view.read_with(visual, |view, _| view.filter_picker_for_test().unwrap());
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    picker.read_with(visual, |picker, _| {
        assert!(picker
            .counts_for_test()
            .unwrap()
            .unwrap_err()
            .contains("does not match"));
    });
    assert_eq!(server.state.lock().unwrap().calls.len(), calls);
    guided_click(visual, "log-count-loaded");
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    picker.read_with(visual, |picker, _| {
        let counts = picker.counts_for_test().unwrap().unwrap();
        assert!(counts.label.to_lowercase().contains("loaded"));
        assert!(counts.values.iter().any(|value| value.count == 6));
    });
    visual.simulate_keystrokes("escape");
    guided_frame(visual);
    panel.update_in(visual, |panel, window, cx| {
        panel.restore_applied(window, cx)
    });
    panel.read_with(visual, |panel, cx| {
        let filters: Vec<serde_json::Value> =
            serde_json::from_str(&panel.filters.read(cx).value()).unwrap();
        assert_eq!(
            filters, base_filters,
            "restoration must not duplicate session filters"
        );
    });
    cx.quit();
}

#[gpui::test]
fn custom_absolute_range_and_source_switch_are_explicit(cx: &mut TestAppContext) {
    // Panel's real worker sends stop notifications from its OS thread.
    cx.executor().allow_parking();
    let server = FakeKibana::start("127.0.0.1:0", 1, Scenario::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    cx.update(crate::test_init);
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(dir.path().into(), dir.path().into(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1500.), px(1100.)));
    guided_frame(visual);
    let panel = panel.unwrap();
    let previous_cancel = panel.update_in(visual, |panel, window, cx| {
        let mut source = Source::direct("First", &server.endpoint, "events", "").unwrap();
        source.scope_fields = vec!["service.keyword".into()];
        panel.choose(source.clone(), window, cx);
        panel.open_custom(window, cx);
        for (at, date) in [
            (0, chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()),
            (1, chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap()),
        ] {
            panel.guided.dates[at].update(cx, |state, cx| state.set_date(date, window, cx));
            panel.guided.hours[at] = 0;
            panel.guided.minutes[at] = 0;
        }
        panel.set_custom_range(window, cx);
        let candidate = panel.candidate(cx).unwrap();
        assert!(candidate.discover.time.absolute());
        assert_eq!(candidate.discover.time.from, "2026-11-01T00:00:00Z");
        panel.guided.scope.insert(
            "service.keyword".into(),
            remote_log::result::FieldValue::String("Example".into()),
        );
        assert!(
            panel.candidate(cx).is_err(),
            "scope must not compile without a confirmed mapping"
        );
        panel.guided.catalog = Some(Ok(vec![remote_log::result::AggregatableField {
            name: "service.keyword".into(),
            source_path: "service".into(),
            field_type: remote_log::result::FieldType::String,
        }]));
        panel.guided.catalog_scope = Some(source.identity_scope().unwrap());
        let before = panel.candidate(cx).unwrap();
        assert_eq!(before.discover.filters.len(), 1);
        let mut changed = source.clone();
        changed.index_pattern = Some("another-pattern".into());
        assert!(
            panel.guided_candidate(&mut changed, cx).is_err(),
            "another dataset cannot reuse the confirmed scope mapping"
        );
        panel.guided.scope.insert(
            "service.keyword".into(),
            remote_log::result::FieldValue::Number(1.into()),
        );
        assert!(
            panel.candidate(cx).is_err(),
            "scope must retain the confirmed scalar type"
        );
        panel.guided.count_client = Some(
            remote_log::transport::Kibana::new(
                &source,
                ApiKey::new(b"test-secret".to_vec()).unwrap(),
                panel.guided.cancel.clone(),
            )
            .unwrap(),
        );
        assert!(
            panel.guided.count_client.is_some(),
            "positive control: a keyed client existed for the previous source"
        );
        panel.guided.cancel.clone()
    });
    assert!(!previous_cancel.load(std::sync::atomic::Ordering::Acquire));
    guided_frame(visual);
    assert!(
        visual.debug_bounds("remote-custom-apply").is_some(),
        "positive control: the fixed range editor was actually rendered"
    );
    panel.update_in(visual, |panel, window, cx| {
        panel.choose(
            Source::direct("Second", "https://second.example", "other", "").unwrap(),
            window,
            cx,
        );
        assert!(panel.guided.scope.is_empty());
        assert!(panel.candidate(cx).unwrap().discover.filters.is_empty());
        assert!(
            panel.guided.count_client.is_none(),
            "another source cannot reuse the previous keyed client"
        );
    });
    guided_frame(visual);
    assert!(
        previous_cancel.load(std::sync::atomic::Ordering::Acquire),
        "owned requests for the previous source were cancelled"
    );
    assert!(
        visual.debug_bounds("remote-custom-apply").is_none(),
        "the editor cannot display another source's dates"
    );
    cx.quit();
}

fn menu_choice(visual: &mut VisualTestContext, index: usize) {
    let keys = std::iter::repeat_n("down", index + 1)
        .chain(std::iter::once("enter"))
        .collect::<Vec<_>>()
        .join(" ");
    visual.simulate_keystrokes(&keys);
    guided_frame(visual);
}
async fn loaded(panel: &Entity<Panel>, visual: &mut VisualTestContext, rows: usize) {
    visual
        .condition(panel, |panel, _| {
            !panel.busy
                && panel
                    .data
                    .as_ref()
                    .is_some_and(|data| data.events.len() == rows)
                && panel.guided.catalog.is_some()
        })
        .await;
    guided_frame(visual);
}
async fn point_and_click_server_chips(
    panel: &Entity<Panel>,
    visual: &mut VisualTestContext,
    server: &FakeKibana,
) {
    use okilum_core::log::chips::{FilterExecution, FilterMode, FilterValue};
    let view = panel.read_with(visual, |panel, _| panel.view.clone().unwrap());
    panel.update(visual, |panel, cx| {
        panel.guided.open_scope = None;
        cx.notify();
    });
    for field in ["service", "duration"] {
        guided_click(visual, "log-columns-menu");
        let index = view.read_with(visual, |view, _| {
            view.field_names()
                .iter()
                .position(|name| name == field)
                .unwrap()
        });
        menu_choice(visual, index);
    }
    view.read_with(visual, |view, cx| {
        assert_eq!(view.columns(cx), vec!["service", "duration"])
    });
    guided_click(visual, "log-header-filter-0");
    let picker = view.read_with(visual, |view, _| view.filter_picker_for_test().unwrap());
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    picker.read_with(visual, |picker, _| {
        let counts = picker.counts_for_test().unwrap().unwrap();
        assert!(counts.label.starts_with("Kibana counts for window"));
        assert!(counts
            .values
            .iter()
            .any(|item| item.value == FilterValue::string("checkout") && item.count == 6));
    });
    let calls = server.state.lock().unwrap().calls.len();
    guided_click(visual, "log-count-loaded");
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    picker.read_with(visual, |picker, _| {
        let counts = picker.counts_for_test().unwrap().unwrap();
        assert!(counts.label.to_lowercase().contains("loaded"));
        assert!(counts
            .values
            .iter()
            .any(|item| item.value == FilterValue::string("Checkout") && item.count == 6));
    });
    assert_eq!(
        server.state.lock().unwrap().calls.len(),
        calls,
        "positive control: loaded counts completed with original casing and made no HTTP request"
    );
    guided_click(visual, "log-count-server");
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    assert!(
        server.state.lock().unwrap().calls.len() > calls,
        "server toggle really requested a window aggregation"
    );
    guided_click(visual, "log-filter-value-0");
    guided_click(visual, "log-filter-apply");
    visual.simulate_keystrokes("escape");
    loaded(panel, visual, 6).await;
    visual.condition(&view, |view, _| !view.filtering).await;
    view.read_with(visual, |view, _| {
        assert_eq!(view.filter_chips()[0].execution, FilterExecution::Server);
        assert_eq!(
            view.visible_len(),
            6,
            "normalized server keys must not be matched against original source casing locally"
        );
    });
    let queries = history_requests(server);
    guided_click(visual, "log-chip-execution-0");
    loaded(panel, visual, 6).await;
    assert!(
        history_requests(server) > queries,
        "execution switching submitted a real history query"
    );
    visual.condition(&view, |view, _| !view.filtering).await;
    view.read_with(visual, |view, _| {
        assert_eq!(view.filter_chips()[0].execution, FilterExecution::Local);
        assert_eq!(view.visible_len(), 0);
    });
    let queries = history_requests(server);
    guided_click(visual, "log-chip-execution-0");
    loaded(panel, visual, 6).await;
    assert!(
        history_requests(server) > queries,
        "execution switching submitted a real history query"
    );
    visual.condition(&view, |view, _| !view.filtering).await;
    view.read_with(visual, |view, _| assert_eq!(view.visible_len(), 6));
    let queries = history_requests(server);
    guided_click(visual, "log-chip-remove-0");
    loaded(panel, visual, 6).await;
    assert!(
        history_requests(server) > queries,
        "removing a redundant server chip still re-queried history"
    );

    // A real cell menu Include narrows the server, not merely the downloaded rows.
    guided_frame(visual);
    let cell = visual.debug_bounds("log-field-cell-0-1").unwrap().center();
    visual.simulate_mouse_down(cell, MouseButton::Right, Modifiers::default());
    guided_frame(visual);
    visual.simulate_mouse_up(cell, MouseButton::Right, Modifiers::default());
    guided_frame(visual);
    let menu = view.read_with(visual, |view, _| view.cell_menu_handle().unwrap());
    menu_choice(visual, 0);
    assert!(view.read_with(visual, |view, _| view.cell_menu_handle().is_none()));
    assert!(
        menu.upgrade().is_none(),
        "the real menu was dismissed and released"
    );
    loaded(panel, visual, 1).await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel
                .quality
                .as_ref()
                .unwrap()
                .total
                .as_ref()
                .unwrap()
                .value,
            1
        )
    });
    guided_click(visual, "log-header-filter-1");
    let picker = view.read_with(visual, |view, _| view.filter_picker_for_test().unwrap());
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    picker.read_with(visual, |picker, _| {
        let counts = picker.counts_for_test().unwrap().unwrap();
        assert_eq!(
            counts.values.iter().filter(|item| item.count > 0).count(),
            1
        );
        assert_eq!(
            counts.values.iter().map(|item| item.count).sum::<usize>(),
            1,
            "server counts changed with the cell filter"
        );
    });
    visual.simulate_keystrokes("escape");
    guided_frame(visual);
    let cell = visual.debug_bounds("log-field-cell-0-1").unwrap().center();
    visual.simulate_mouse_down(cell, MouseButton::Right, Modifiers::default());
    guided_frame(visual);
    visual.simulate_mouse_up(cell, MouseButton::Right, Modifiers::default());
    guided_frame(visual);
    menu_choice(visual, 1);
    loaded(panel, visual, 0).await;
    view.read_with(visual, |view, _| {
        assert_eq!(view.filter_chips()[1].mode, FilterMode::Exclude)
    });
    guided_click(visual, "log-chip-1");
    guided_click(visual, "log-mode-include");
    guided_click(visual, "log-filter-apply");
    visual.simulate_keystrokes("escape");
    loaded(panel, visual, 1).await;
    let calls = server.state.lock().unwrap().calls.len();
    guided_click(visual, "log-chip-remove-1");
    loaded(panel, visual, 1).await;
    assert!(
        server.state.lock().unwrap().calls.len() > calls,
        "removing an equivalent second chip still applied a new server query"
    );
    guided_click(visual, "log-chips-clear");
    loaded(panel, visual, 6).await;
    view.read_with(visual, |view, _| assert!(view.filter_chips().is_empty()));
    // Role headers bind to their real field once, so an edit can recover even
    // when the preceding server query loaded zero records.
    guided_click(visual, "log-level-filter");
    let picker = view.read_with(visual, |view, _| view.filter_picker_for_test().unwrap());
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    guided_click(visual, "log-filter-value-0");
    guided_click(visual, "log-filter-apply");
    visual.simulate_keystrokes("escape");
    loaded(panel, visual, 6).await;
    view.read_with(visual, |view, _| {
        assert_eq!(
            view.filter_chips()[0].field,
            okilum_core::log::query::FieldRef::Key("level".into())
        )
    });
    guided_click(visual, "log-chip-0");
    guided_click(visual, "log-mode-exclude");
    guided_click(visual, "log-filter-apply");
    visual.simulate_keystrokes("escape");
    loaded(panel, visual, 0).await;
    guided_click(visual, "log-chip-0");
    guided_click(visual, "log-mode-include");
    guided_click(visual, "log-filter-apply");
    visual.simulate_keystrokes("escape");
    loaded(panel, visual, 6).await;
    guided_click(visual, "log-chips-clear");
    loaded(panel, visual, 6).await;
    let queries = history_requests(server);
    let manual = okilum_core::log::chips::FilterChip::new(
        okilum_core::log::query::FieldRef::Key("duration".into()),
        vec![FilterValue::Value {
            kind: okilum_core::log::ValueKind::Number,
            text: "0".into(),
        }],
        FilterMode::Include,
    )
    .expr()
    .to_string();
    view.update_in(visual, |view, window, cx| {
        view.replace_query(manual, window, cx)
    });
    visual
        .condition(&view, |view, _| !view.filtering && view.visible_len() == 1)
        .await;
    view.read_with(visual, |view, _| {
        assert_eq!(view.filter_chips()[0].execution, FilterExecution::Local)
    });
    assert_eq!(
        history_requests(server),
        queries,
        "positive control: the manual loaded-row query changed visible rows without a server query"
    );
    guided_click(visual, "log-chips-clear");
    visual
        .condition(&view, |view, _| !view.filtering && view.visible_len() == 6)
        .await;
    assert_eq!(
        history_requests(server),
        queries,
        "clearing local-only chips did not refresh the server"
    );
    assert!(!panel.read_with(visual, |panel, _| panel.auth_error));
}

fn history_requests(server: &FakeKibana) -> usize {
    server
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|call| {
            call.path == "/internal/search/es"
                && call.body["id"].is_null()
                // Full navigation deliberately replays its timestamp boundary;
                // only a search without a replay offset starts a primary traversal.
                && call.body["params"].get("from").is_none()
                && call.body["params"]["size"]
                    .as_u64()
                    .is_some_and(|size| size > 0)
        })
        .count()
}

fn save_guided_view(
    view: &Entity<reader_log::LogView>,
    visual: &mut VisualTestContext,
    name: &str,
) {
    let count = visual.update(|_, cx| reader_ui_state::saved_log_filters(cx).len());
    guided_click(visual, "log-saved");
    // PopupMenu's first Down selects index 0 even when it is disabled.
    // With no saved names, step past the disabled empty-state item.
    menu_choice(visual, count.max(1));
    assert!(visual.debug_bounds("log-saving-name").is_some());
    view.update_in(visual, |view, window, cx| {
        view.saving
            .as_ref()
            .unwrap()
            .input
            .update(cx, |input, cx| input.set_value(name, window, cx));
    });
    guided_click(visual, "log-saving-save");
}

async fn guided_follow_back_live_saved(
    panel: &Entity<Panel>,
    visual: &mut VisualTestContext,
    server: &FakeKibana,
) {
    use okilum_core::log::{
        chips::{FilterChip, FilterMode, FilterValue},
        query::FieldRef,
    };
    let view = panel.read_with(visual, |panel, _| panel.view.clone().unwrap());
    view.update_in(visual, |view, window, cx| {
        view.choose_field(Some("details".into()), true, cx);
        view.choose_field(Some("request_id".into()), false, cx);
        view.add_chip(
            FilterChip::new(
                FieldRef::Key("service".into()),
                vec![FilterValue::string("Checkout")],
                FilterMode::Include,
            ),
            window,
            cx,
        );
    });
    loaded(panel, visual, 6).await;
    let parent = panel.read_with(visual, |panel, _| panel.data.clone().unwrap());
    let parent_chips = view.read_with(visual, |view, _| view.filter_chips().to_vec());
    view.update_in(visual, |view, window, cx| view.select(0, true, window, cx));
    guided_frame(visual);
    save_guided_view(&view, visual, "Guided parent");
    let before = server.state.lock().unwrap().calls.len();
    let queries = history_requests(server);
    guided_click(visual, "log-show-request");
    loaded(panel, visual, 1).await;
    assert_eq!(history_requests(server), queries+1, "one navigation owns the server query; clearing parent chips must not dispatch another request");
    assert!(server.state.lock().unwrap().calls[before..].iter().any(|call| call.body["params"]["from"].as_u64() == Some(0)), "positive control: required boundary replay is present and distinguished from a new primary traversal");
    let range = panel.read_with(visual, |panel, cx| {
        assert_eq!(panel.applied.as_ref().unwrap().id, panel.sources[0].id);
        assert_eq!(panel.from.read(cx).value().as_str(), "now-24h");
        assert_eq!(
            panel.guided.scope["service.keyword"],
            remote_log::result::FieldValue::String("checkout".into())
        );
        panel.range.clone().unwrap()
    });
    view.read_with(visual, |view, cx| {
        assert!(
            view.filter_chips().is_empty(),
            "parent table chips are owned by Back"
        );
        assert_eq!(view.columns(cx), ["service", "duration"]);
        assert!(view
            .investigation_label()
            .unwrap()
            .starts_with("Kibana: 1 events for id "));
    });
    let queries = history_requests(server);
    guided_click(visual, "log-request-back");
    loaded(panel, visual, 6).await;
    assert_eq!(
        history_requests(server),
        queries,
        "positive control: Back restored six rows from one without HTTP"
    );
    assert!(panel.read_with(visual, |panel, _| Arc::ptr_eq(
        panel.data.as_ref().unwrap(),
        &parent
    )));
    view.read_with(visual, |view, _| {
        assert_eq!(view.filter_chips(), parent_chips);
        assert_eq!(view.selected, Some(0));
    });
    // The native Back shortcut follows the same remote restoration path.
    guided_click(visual, "log-show-request");
    loaded(panel, visual, 1).await;
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-[");
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("alt-left");
    loaded(panel, visual, 6).await;
    guided_click(visual, "log-show-request");
    loaded(panel, visual, 1).await;
    assert_eq!(
        panel.read_with(visual, |panel, _| panel.range.clone().unwrap()),
        range
    );
    let mut fresh = panel.read_with(visual, |panel, _| {
        panel.data.as_ref().unwrap().events[0].as_ref().clone()
    });
    fresh["_id"] = serde_json::json!("guided-live-event");
    server.add(fresh.clone());
    server.add(fresh);
    guided_click(visual, "remote-start");
    guided_click(visual, "remote-confirm-fixed");
    visual
        .condition(panel, |panel, _| {
            !panel.busy
                && panel.following
                && panel
                    .data
                    .as_ref()
                    .is_some_and(|data| data.events.len() == 2)
        })
        .await;
    guided_frame(visual);
    view.read_with(visual, |view, cx| {
        assert_eq!(view.columns(cx), ["service", "duration"]);
        assert_eq!(view.visible_len(), 2);
        assert!(view.investigation.live && view.investigation.last_update.is_some());
    });
    guided_click(visual, "remote-stop");
    visual.condition(panel, |panel, _| !panel.following).await;
    save_guided_view(&view, visual, "Guided request");
    let saved = visual.update(|_, cx| {
        reader_ui_state::saved_log_filters(cx)
            .get("Guided request")
            .unwrap()
            .clone()
    });
    let context = saved.remote_guided.as_ref().unwrap();
    assert_eq!(context.source.discover.time.from, "now-24h");
    assert_eq!(context.source.scope_fields, ["service.keyword"]);
    assert_eq!(
        context.scope["service.keyword"],
        remote_log::result::FieldValue::String("checkout".into())
    );
    assert_eq!(
        context.source.discover.filters.len(),
        1,
        "base filters exclude scope and request DSL"
    );
    assert!(saved.request.is_some());
    assert_eq!(saved.message_field.as_deref(), Some("details"));
    assert_eq!(saved.correlation_field.as_deref(), Some("request_id"));
    let encoded = serde_json::to_string(&saved).unwrap();
    assert!(!encoded.contains("test-secret"));
    let decoded: okilum_core::log::saved_query::SavedQuery =
        serde_json::from_str(&encoded).unwrap();
    assert_eq!(saved, decoded);
    guided_click(visual, "log-saved");
    menu_choice(visual, 0);
    loaded(panel, visual, 7).await;
    // Restore across environments, not only within an already selected source.
    let mut other = context.source.clone();
    other.id = uuid::Uuid::new_v4();
    other.name = "Other environment".into();
    panel.update_in(visual, |panel, window, cx| {
        panel.select_source(other.clone(), window, cx)
    });
    loaded(panel, visual, 13).await;
    assert!(panel.read_with(visual, |panel, _| panel.guided.scope.is_empty()));
    view.update(visual, |view, cx| {
        view.choose_field(Some("service".into()), true, cx);
        view.choose_field(Some("flow".into()), false, cx);
    });
    let queries = history_requests(server);
    guided_click(visual, "log-saved");
    menu_choice(visual, 1);
    loaded(panel, visual, 2).await;
    assert_eq!(
        history_requests(server),
        queries + 1,
        "saved-view navigation must not race chip callbacks"
    );
    panel.read_with(visual, |panel, cx| {
        assert_eq!(panel.from.read(cx).value().as_str(), "now-24h");
        assert_eq!(panel.guided.scope, context.scope);
        assert_eq!(panel.applied.as_ref().unwrap().id, context.source.id);
        let other_fields = reader_ui_state::log_fields(&format!("kibana-{}.jsonl", other.id), cx);
        assert_eq!(other_fields.message.as_deref(), Some("service"));
        assert_eq!(other_fields.correlation.as_deref(), Some("flow"));
        assert_eq!(
            panel.applied.as_ref().unwrap().discover.filters.len(),
            3,
            "base, scope and independent request each appear once"
        );
    });
    server
        .state
        .lock()
        .unwrap()
        .records
        .retain(|event| event["_id"] != "guided-live-event");
    guided_click(visual, "log-saved");
    menu_choice(visual, 0);
    loaded(panel, visual, 6).await;
    guided_click(visual, "log-chips-clear");
    loaded(panel, visual, 6).await;
}

async fn fallback_scope_header_and_connection_details(cx: &mut TestAppContext, scenario: Scenario) {
    cx.executor().allow_parking();
    let server = FakeKibana::start("127.0.0.1:0", 12, scenario).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
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
    remote_log::save_source(&state, &vault, &source).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
        reader_ui_state::install(&state, cx);
    });
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1800.), px(1400.)));
    let panel = panel.unwrap();
    visual
        .condition(&panel, |panel, _| panel.sources_loaded)
        .await;
    panel.update(visual, |panel, _| {
        panel.guided.test_key = Some(b"test-secret".to_vec())
    });
    guided_click(visual, "remote-source-0");
    loaded(&panel, visual, 12).await;
    panel.read_with(visual, |panel, _| {
        assert!(!panel.auth_error);
        let discovery = panel.guided.discovery.as_ref().unwrap();
        assert!(discovery.fallback);
        assert!(discovery.fields.iter().any(|f| f.name == "service.keyword"));
        assert!(!discovery
            .fields
            .iter()
            .any(|f| f.source_path == "text_only"));
    });
    guided_click(visual, "remote-connection-details");
    guided_click(visual, "remote-copy-details");
    let copied = visual.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert!(copied.contains("field catalog: HTTP"));
    assert!(copied.contains("search sample + terms probes"));
    assert!(copied.contains("fallback probe: HTTP"));
    for private in [
        server.endpoint.as_str(),
        "remote-*:events",
        "test-secret",
        "Checkout",
    ] {
        assert!(!copied.contains(private));
    }
    guided_click(visual, "remote-connection-details");
    guided_click(visual, "remote-pin-scope");
    let index = panel.read_with(visual, |panel, _| {
        panel
            .guided
            .catalog
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .iter()
            .position(|f| f.name == "service.keyword")
            .unwrap()
    });
    menu_choice(visual, index);
    visual
        .condition(&panel, |panel, _| {
            panel.guided.counts.contains_key("service.keyword")
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(
            panel.guided.counts["service.keyword"]
                .as_ref()
                .unwrap()
                .buckets[0]
                .value,
            remote_log::result::FieldValue::String("checkout".into())
        );
    });
    let probes = || {
        server
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.body["params"]["aggs"].get("probe_0").is_some())
            .count()
    };
    let before = probes();
    assert!(before > 0, "positive control: fallback actually probed");
    guided_click(visual, "remote-scope-value-0");
    loaded(&panel, visual, 6).await;
    assert_eq!(
        probes(),
        before,
        "source/pattern session cache avoids probing again"
    );
    assert!(server
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|c| c.body["params"]["query"]
            .to_string()
            .contains("\"service.keyword\":\"checkout\"")));
    // Reuse the full real header-count/chip exercise, including numeric counts,
    // normalized keys, local/server toggles and menu dismissal.
    point_and_click_server_chips(&panel, visual, &server).await;
    visual.simulate_keystrokes("escape");
    cx.quit();
}

#[gpui::test]
async fn catalog_403_fallback_populates_scope_header_counts_and_details(cx: &mut TestAppContext) {
    fallback_scope_header_and_connection_details(
        cx,
        Scenario {
            catalog_status: Some(403),
            ..Default::default()
        },
    )
    .await;
}
#[gpui::test]
async fn catalog_404_fallback_populates_scope_header_counts_and_details(cx: &mut TestAppContext) {
    fallback_scope_header_and_connection_details(
        cx,
        Scenario {
            catalog_status: Some(404),
            ..Default::default()
        },
    )
    .await;
}
#[gpui::test]
async fn catalog_empty_cross_cluster_fallback_populates_scope_header_counts_and_details(
    cx: &mut TestAppContext,
) {
    fallback_scope_header_and_connection_details(
        cx,
        Scenario {
            catalog_empty: true,
            ..Default::default()
        },
    )
    .await;
}
#[gpui::test]
async fn catalog_malformed_fallback_populates_scope_header_counts_and_details(
    cx: &mut TestAppContext,
) {
    fallback_scope_header_and_connection_details(
        cx,
        Scenario {
            catalog_malformed: true,
            ..Default::default()
        },
    )
    .await;
}
