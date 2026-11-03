use super::*;
use crate::remote_log::{
    discover::TimeRange,
    fake::{record, FakeKibana, Scenario},
    result::AuthError,
    MAX_ROWS,
};
use serde_json::json;

fn make_source(server: &FakeKibana) -> Source {
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
    source
}
fn key() -> ApiKey {
    ApiKey::new(b"test-secret".to_vec()).unwrap()
}
fn timing() -> Timing {
    Timing {
        poll: Duration::from_millis(40),
        retry: Duration::from_millis(20),
        cycle: Duration::from_secs(20),
        idle: Duration::from_secs(60),
        after_page: None,
    }
}
fn receive(rx: &async_channel::Receiver<Update>, predicate: impl Fn(&Update) -> bool) -> Update {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match rx.try_recv() {
            Ok(update) if predicate(&update) => return update,
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "worker update never arrived");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}
#[test]
fn live_reconciles_every_page_new_and_late_documents_once_without_timestamp_watermark() {
    let server = FakeKibana::start("127.0.0.1:0", 603, Scenario::default()).unwrap();
    let source = make_source(&server);
    let (worker, rx) = Worker::spawn(timing(), None);
    let generation = worker.apply(source.clone(), key());
    let initial = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    });
    assert_eq!(initial.loaded.unwrap().window.events.len(), 200);
    let generation = worker.start_live(Mode::Fixed(source.discover.time.clone()), &source);
    let first = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Complete { .. })
    });
    let old = first.loaded.unwrap();
    assert_eq!(old.window.events.len(), 603);
    let mut late = record(9001);
    late["_source"]["@timestamp"] = json!("2026-11-02T00:00:00Z");
    late["sort"][0] = json!(1793577600000000001i64);
    let mut current = record(9002);
    current["_source"]["@timestamp"] = json!("2026-11-04T00:00:00Z");
    current["sort"][0] = json!(1793750400000000001i64);
    server.add(late);
    server.add(current);
    let updated = receive(&rx, |u| {
        u.generation == generation
            && matches!(u.status, Status::Complete { .. })
            && u.loaded
                .as_ref()
                .is_some_and(|l| l.window.events.len() == 605)
    });
    let loaded = updated.loaded.unwrap();
    for id in ["9001", "9002"] {
        assert_eq!(
            loaded
                .window
                .events
                .iter()
                .filter(|h| h["_id"] == id)
                .count(),
            1
        );
    }
    assert_eq!(
        old.window.events.len(),
        603,
        "immutable loaded snapshot is retained by existing details/export owners"
    );
    let start = server.state.lock().unwrap().searches;
    receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Complete { .. })
    });
    assert!(
        server.state.lock().unwrap().searches > start,
        "positive control: another full reconciliation ran"
    );
    let generation = worker.stop();
    receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Stopped { .. })
    });
    assert!(server
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .all(|c| !c.path.contains("console") && !c.path.contains("_pit")));
}
#[test]
fn typed_auth_then_replacement_replays_same_query_retaining_loaded_state_and_ignoring_new_draft() {
    let server = FakeKibana::start("127.0.0.1:0", 11, Scenario::default()).unwrap();
    let mut source = make_source(&server);
    source.discover.columns = vec!["service".into(), "request_id".into()];
    source.discover.filters = vec![json!({"query":{"term":{"service.keyword":"checkout"}}})];
    let (worker, rx) = Worker::spawn(timing(), None);
    let generation = worker.apply(source.clone(), key());
    let old = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap();
    let mut failed = source.clone();
    failed.discover.filters = vec![json!({"query":{"term":{"level":"INFO"}}})];
    let generation = worker.apply(failed.clone(), ApiKey::new(b"bad-key".to_vec()).unwrap());
    let denied = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::AuthError(_))
    });
    assert!(Arc::ptr_eq(&denied.loaded.unwrap().window, &old.window));
    assert!(matches!(
        denied.status,
        Status::AuthError(AuthError { status: 401 })
    ));
    let generation = worker.replace_key(key());
    let restored = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap();
    assert_eq!(restored.source.id, failed.id);
    assert_eq!(restored.source.endpoint, failed.endpoint);
    assert_eq!(restored.source.discover, failed.discover);
    assert_eq!(restored.window.events.len(), 11);
    assert_eq!(old.window.events.len(), 6);
}
#[test]
fn stop_and_drop_cancel_real_async_polling_and_stale_generations_cannot_replace_rows() {
    let server = FakeKibana::start("127.0.0.1:0", 9, Scenario::default()).unwrap();
    let source = make_source(&server);
    let (worker, rx) = Worker::spawn(timing(), None);
    let generation = worker.apply(source.clone(), key());
    let old = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap();
    server.state.lock().unwrap().scenario.never_complete = true;
    worker.apply(source.clone(), key());
    let start = Instant::now();
    while server.state.lock().unwrap().polls < 2 {
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(2));
    }
    let generation = worker.stop();
    let stopped = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Stopped { .. })
    });
    assert!(Arc::ptr_eq(&stopped.loaded.unwrap().window, &old.window));
    let before = server.state.lock().unwrap().polls;
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(server.state.lock().unwrap().polls, before);
    worker.apply(source, key());
    let start = Instant::now();
    while server.state.lock().unwrap().polls <= before {
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(worker);
    let start = Instant::now();
    while !rx.is_closed() {
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(2));
    }
    let before = server.state.lock().unwrap().polls;
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(server.state.lock().unwrap().polls, before);
}
#[test]
fn live_partial_rows_are_shown_and_following_stops_instead_of_hiding_them() {
    let server = FakeKibana::start("127.0.0.1:0", 11, Scenario::default()).unwrap();
    let source = make_source(&server);
    let (worker, rx) = Worker::spawn(timing(), None);
    let generation = worker.apply(source.clone(), key());
    receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    });
    server.add(record(101));
    server.state.lock().unwrap().scenario.partial = true;
    let generation = worker.start_live(Mode::Fixed(source.discover.time.clone()), &source);
    let update = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    });
    assert!(!update.following);
    let loaded = update.loaded.unwrap();
    assert_eq!(loaded.window.events.len(), 12);
    assert!(loaded.quality.partial);
    assert_eq!(loaded.quality.failed_shards, 1);
}
#[test]
fn reconnect_after_observed_outage_reconciles_the_whole_window() {
    let server = FakeKibana::start("127.0.0.1:0", 7, Scenario::default()).unwrap();
    let source = make_source(&server);
    let (worker, rx) = Worker::spawn(timing(), None);
    let generation = worker.apply(source.clone(), key());
    let initial = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap();
    server.state.lock().unwrap().scenario.unavailable = true;
    let generation = worker.start_live(Mode::Fixed(source.discover.time.clone()), &source);
    let outage = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Reconnecting { .. })
    });
    assert!(Arc::ptr_eq(&outage.loaded.unwrap().window, &initial.window));
    server.add(record(8));
    server.state.lock().unwrap().scenario.unavailable = false;
    let recovered = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Complete { .. })
    });
    assert_eq!(recovered.loaded.unwrap().window.events.len(), 8);
}
#[test]
fn row_cap_and_cycle_deadline_are_explicit_and_leave_visible_bounded_rows() {
    let server = FakeKibana::start("127.0.0.1:0", MAX_ROWS + 1, Scenario::default()).unwrap();
    let source = make_source(&server);
    let client = Kibana::new(&source, key(), Arc::new(AtomicBool::new(false))).unwrap();
    let mut history = History::for_source(client, &source).unwrap();
    let mut count = 0;
    while history.can_page() {
        let page = history.page().unwrap();
        count += page.events.len();
        if !history.can_page() {
            assert!(page.status.truncated);
            assert!(!page.complete);
        }
    }
    assert_eq!(count, MAX_ROWS);
    assert!(server.state.lock().unwrap().searches < 130,
        "bounded boundary replay must not shrink to the remaining unique-row count and repeatedly re-read duplicate prefixes");
    let server = FakeKibana::start("127.0.0.1:0", 21, Scenario::default()).unwrap();
    let source = make_source(&server);
    let mut short = timing();
    short.cycle = Duration::from_millis(1);
    short.after_page = Some(Arc::new(|_| std::thread::sleep(Duration::from_millis(5))));
    let (worker, rx) = Worker::spawn(short, None);
    let generation = worker.apply(source.clone(), key());
    receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    });
    let generation = worker.start_live(Mode::Fixed(source.discover.time.clone()), &source);
    let limited = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    });
    assert!(!limited.following);
    assert!(limited.loaded.unwrap().quality.timed_out);
}
#[test]
fn fixed_rolling_modes_keep_absolute_bounds_and_reject_zero_duration() {
    let range = TimeRange {
        from: "2026-11-02T00:00:00Z".into(),
        to: "2026-11-02T00:01:00Z".into(),
    };
    let now = OffsetDateTime::now_utc();
    assert_eq!(
        Mode::Fixed(range.clone()).range(now).unwrap(),
        range.resolve(now).unwrap()
    );
    let rolling = Mode::rolling_same_duration(&range)
        .unwrap()
        .range(now)
        .unwrap();
    assert_eq!(
        OffsetDateTime::parse(&rolling.to, &time::format_description::well_known::Rfc3339).unwrap(),
        now
    );
    assert!(Mode::Rolling { nanos: 0 }.range(now).is_err());
}

#[test]
fn replacing_key_preserves_the_loaded_extent_of_a_multi_page_investigation() {
    let server = FakeKibana::start("127.0.0.1:0", 603, Scenario::default()).unwrap();
    let source = make_source(&server);
    let (worker, rx) = Worker::spawn(timing(), None);
    let mut generation = worker.apply(source.clone(), key());
    let mut loaded = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap();
    while loaded.more {
        generation = worker.more();
        loaded = receive(&rx, |u| {
            u.generation == generation && matches!(u.status, Status::History { .. })
        })
        .loaded
        .unwrap();
    }
    assert_eq!(
        loaded.window.events.len(),
        603,
        "positive control: investigation contains more than one page before the key expires"
    );
    let retry_range = TimeRange {
        from: "2026-11-01T00:00:01Z".into(),
        to: "2026-11-04T00:00:00Z".into(),
    };
    server.state.lock().unwrap().scenario.failure_status = Some(401);
    generation = worker.start_live(Mode::Fixed(retry_range.clone()), &source);
    let denied = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::AuthError(_))
    });
    assert!(Arc::ptr_eq(&denied.loaded.unwrap().window, &loaded.window));
    server.state.lock().unwrap().scenario.failure_status = None;
    generation = worker.replace_key(key());
    let refreshed = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap();
    assert_eq!(refreshed.window.events.len(), 603);
    let mut expected = source.discover.clone();
    expected.time = retry_range;
    assert_eq!(refreshed.source.discover, expected);
    let ids: std::collections::HashSet<_> = refreshed
        .window
        .events
        .iter()
        .map(|e| e["_id"].to_string())
        .collect();
    assert_eq!(ids.len(), 603);
}

#[test]
fn navigate_reads_every_request_page_and_back_restores_without_http_then_live_keeps_the_request() {
    let server = FakeKibana::start("127.0.0.1:0", 1503, Scenario::default()).unwrap();
    {
        let mut state = server.state.lock().unwrap();
        state.records = (0..1503)
            .map(crate::remote_log::fake::investigation_record)
            .collect();
    }
    let mut source = make_source(&server);
    source.discover.sort[0].order = "desc".into();
    source
        .discover
        .filters
        .push(json!({"query":{"term":{"service":"Checkout"}}}));
    let (worker, rx) = Worker::spawn(timing(), None);
    let generation = worker.apply(source.clone(), key());
    let previous = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap();
    assert_eq!(
        previous.window.events.len(),
        200,
        "positive control: parent is one history page"
    );
    let mut request = source.clone();
    request.discover.sort[0].order = "asc".into();
    request
        .discover
        .filters
        .push(json!({"query":crate::log::correlation::server_query("request-0", "flow")}));
    let generation = worker.navigate(request.clone());
    let loaded = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    })
    .loaded
    .unwrap();
    assert_eq!(
        loaded.window.events.len(),
        251,
        "all pages, including events absent from the parent's first page"
    );
    assert!(loaded
        .window
        .events
        .iter()
        .all(|event| event["_source"]["flow"] == "request-0"
            && event["_source"]["service"] == "Checkout"));
    assert!(!loaded.more && !loaded.quality.incomplete());
    assert!(loaded
        .window
        .events
        .windows(2)
        .all(|pair| pair[0]["sort"][0].as_i64() <= pair[1]["sort"][0].as_i64()));
    let searches = server.state.lock().unwrap().searches;
    let generation = worker.restore(previous.clone());
    let restored = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Stopped { .. })
    })
    .loaded
    .unwrap();
    assert!(Arc::ptr_eq(&previous.window, &restored.window));
    assert_eq!(restored.source.discover, source.discover);
    assert_eq!(
        server.state.lock().unwrap().searches,
        searches,
        "Back performs no HTTP after the positive query control"
    );
    // A restored query is usable by Start, including its original sort/scope.
    let generation = worker.start_live(Mode::Fixed(source.discover.time.clone()), &source);
    let parent_live = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Complete { .. })
    })
    .loaded
    .unwrap();
    assert_eq!(parent_live.window.events.len(), 752);
    let generation = worker.navigate(request.clone());
    receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::History { .. })
    });
    let mut fresh = crate::remote_log::fake::investigation_record(1506);
    fresh["_id"] = json!("new-request-event");
    server.add(fresh.clone());
    server.add(fresh); // Same identity is emitted twice by the server.
    let generation = worker.start_live(Mode::Fixed(request.discover.time.clone()), &request);
    let live = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Complete { .. })
    })
    .loaded
    .unwrap();
    assert_eq!(live.window.events.len(), 252);
    assert_eq!(
        live.window
            .events
            .iter()
            .filter(|event| event["_id"] == "new-request-event")
            .count(),
        1
    );
    let mut other = request;
    other.endpoint = "https://other.example".into();
    let generation = worker.navigate(other);
    let denied = receive(&rx, |u| {
        u.generation == generation && matches!(u.status, Status::Error(_))
    });
    assert!(
        matches!(denied.status, Status::Error(ref error) if error.contains("credential scope"))
    );
    assert!(Arc::ptr_eq(&denied.loaded.unwrap().window, &live.window));
}

#[test]
fn request_drill_down_retains_partial_and_truncated_quality() {
    for scenario in [
        Scenario {
            partial: true,
            ..Default::default()
        },
        Scenario {
            lower_bound: true,
            ..Default::default()
        },
    ] {
        let server = FakeKibana::start("127.0.0.1:0", 7, scenario).unwrap();
        let source = make_source(&server);
        let (worker, rx) = Worker::spawn(timing(), None);
        let generation = worker.apply(source.clone(), key());
        receive(&rx, |u| {
            u.generation == generation && matches!(u.status, Status::History { .. })
        });
        let mut request = source;
        request.discover.filters.push(
            json!({"query":crate::log::correlation::server_query("request-0", "request_id")}),
        );
        let generation = worker.navigate(request);
        let loaded = receive(&rx, |u| {
            u.generation == generation && matches!(u.status, Status::History { .. })
        })
        .loaded
        .unwrap();
        assert_eq!(
            loaded.window.events.len(),
            1,
            "positive control: request query reached the fake server"
        );
        assert!(
            loaded.quality.partial
                || loaded.quality.total.as_ref().unwrap().relation
                    == crate::remote_log::result::TotalRelation::Gte
        );
        assert!(!loaded.quality.label(1).is_empty());
    }
}
