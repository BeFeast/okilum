use super::*;
use crate::remote_log::{
    fake::{record, FakeKibana, Scenario},
    result::{describe_fields, TotalRelation},
};
use std::collections::HashSet;

pub(super) fn server(count: usize, scenario: Scenario) -> FakeKibana {
    FakeKibana::start("127.0.0.1:0", count, scenario).unwrap()
}
pub(super) fn source(server: &FakeKibana) -> Source {
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
pub(super) fn client(server: &FakeKibana) -> Kibana {
    Kibana::with_endpoint(
        Url::parse(&server.endpoint).unwrap(),
        ApiKey::new(b"test-secret".to_vec()).unwrap(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap()
}
#[test]
fn injected_http_deadline_cancels_a_held_poll_and_keeps_received_rows() {
    let server = server(
        9,
        Scenario {
            never_complete: true,
            ..Default::default()
        },
    );
    let (arrived, polls) = std::sync::mpsc::channel();
    let (release, permit) = std::sync::mpsc::channel();
    let permit = Mutex::new(permit);
    server.state.lock().unwrap().after_poll = Some(Arc::new(move |count| {
        arrived.send(count).unwrap();
        permit.lock().unwrap().recv().unwrap();
    }));
    let clock = Arc::new(TestClock::new());
    let mut history = History::for_source(
        client(&server).with_test_clock(clock.clone()).unwrap(),
        &source(&server),
    )
    .unwrap();
    history.search_timeout = Duration::from_millis(1);
    let query = std::thread::spawn(move || history.page().unwrap());
    // A real async poll reached the server despite a frozen one-millisecond
    // budget. Its response stays held until AFTER virtual deadline completion.
    assert_eq!(polls.recv().unwrap(), 1);
    assert_eq!(clock.expirations(), 0);
    clock.advance(Duration::from_millis(1));
    let page = query.join().unwrap();
    assert!(page.status.timed_out);
    assert!(page.status.partial);
    assert_eq!(page.events.len(), 9);
    assert_eq!(
        clock.expirations(),
        1,
        "the injected HTTP timer actually fired"
    );
    let state = server.state.clone();
    release.send(()).unwrap();
    drop(server);
    assert_eq!(state.lock().unwrap().polls, 1);
}
#[test]
fn history_flat_envelope_boundary_replay_exact_nanos_and_identity_no_gaps_both_directions() {
    for order in ["asc", "desc"] {
        let server = server(603, Scenario::default());
        let mut same_id = record(0);
        same_id["_index"] = json!("remote-b:events");
        server.add(same_id);
        let mut source = source(&server);
        source.discover.sort[0].order = order.into();
        let mut history = History::for_source(client(&server), &source).unwrap();
        let mut rows = Vec::new();
        let mut pages = 0;
        while history.can_page() {
            let page = history.page().unwrap();
            rows.extend(page.events);
            pages += 1;
            assert_eq!(
                page.status.total.as_ref().unwrap().relation,
                TotalRelation::Eq
            );
            assert_eq!(page.status.total.as_ref().unwrap().value, 604);
            assert!(!page.status.incomplete());
            assert!(pages < 30);
        }
        assert!(
            pages > 2,
            "positive control: real paging and boundary replay fired"
        );
        let actual: HashSet<_> = rows
            .iter()
            .map(|r| {
                (
                    r["_index"].clone().to_string(),
                    r["_id"].clone().to_string(),
                )
            })
            .collect();
        assert_eq!(rows.len(), 604);
        assert_eq!(actual.len(), 604);
        for id in 0..603 {
            let hit = record(id);
            assert!(actual.contains(&(hit["_index"].to_string(), hit["_id"].to_string())));
        }
        assert!(actual.contains(&(json!("remote-b:events").to_string(), json!("0").to_string())));
        let s = server.state.lock().unwrap();
        assert!(s
            .calls
            .iter()
            .all(|c| !c.path.contains("console") && !c.body.to_string().contains("pit")));
        let searches: Vec<_> = s
            .calls
            .iter()
            .filter(|c| c.path.ends_with("/internal/search/es"))
            .collect();
        assert!(searches
            .iter()
            .all(|c| c.body["params"].get("body").is_none()));
        let cursor = searches
            .iter()
            .find_map(|c| c.body["params"].get("search_after"))
            .unwrap();
        let exact = cursor[0].as_i64().unwrap();
        assert!(exact > 9_007_199_254_740_992);
        assert_ne!(
            exact as f64 as i64, exact,
            "positive control: f64 would corrupt this cursor"
        );
        assert!(rows
            .iter()
            .all(|r| r["sort"][0].as_i64().unwrap() % 1000 == 789
                || r["sort"][0].as_i64().unwrap() % 1000 == 790
                || r["sort"][0].as_i64().unwrap() % 1000 == 791));
    }
}
#[test]
fn async_polls_same_route_same_params_including_partial_with_id() {
    for partial_with_id in [false, true] {
        let server = server(
            11,
            Scenario {
                async_polls: 3,
                partial_with_id,
                ..Default::default()
            },
        );
        let mut history = History::for_source(client(&server), &source(&server)).unwrap();
        let page = history.page().unwrap();
        assert_eq!(page.events.len(), 11);
        assert!(!page.status.incomplete());
        let s = server.state.lock().unwrap();
        assert_eq!(s.polls, 3);
        let calls: Vec<_> = s
            .calls
            .iter()
            .filter(|c| c.path.ends_with("/internal/search/es"))
            .collect();
        assert_eq!(calls.len(), 4);
        assert!(calls[0].body.get("id").is_none());
        for call in &calls[1..] {
            assert_eq!(call.method, "POST");
            assert_eq!(call.path, calls[0].path);
            assert_eq!(call.body["params"], calls[0].body["params"]);
            assert!(call.body["id"].is_string());
        }
    }
}
#[test]
fn async_deadline_retains_rows_and_timeout_status_and_no_poll_after_close() {
    let server = server(
        7,
        Scenario {
            never_complete: true,
            ..Default::default()
        },
    );
    let mut history = History::for_source(client(&server), &source(&server)).unwrap();
    history.search_timeout = Duration::from_millis(120);
    let start = std::time::Instant::now();
    let page = history.page().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(page.status.timed_out && page.status.partial && !page.complete);
    assert_eq!(page.events.len(), 7);
    history.close().unwrap();
    assert!(
        server.state.lock().unwrap().polls > 0,
        "positive control: async polling reached the server"
    );
    // A poll already sent before the timeout can reach the loopback server
    // after close. Measure new client sends, not delayed server arrivals.
    let count = history.kibana.polls_started.load(Ordering::Acquire);
    assert!(count > 0, "positive control: client started async polls");
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(history.kibana.polls_started.load(Ordering::Acquire), count);
}
#[test]
fn cancellation_stops_active_async_polling_without_any_cleanup_route() {
    let server = server(
        3,
        Scenario {
            never_complete: true,
            ..Default::default()
        },
    );
    let kibana = client(&server);
    let cancel = kibana.cancelled.clone();
    let polls_started = kibana.polls_started.clone();
    let source = source(&server);
    let thread = std::thread::spawn(move || History::for_source(kibana, &source).unwrap().page());
    let start = std::time::Instant::now();
    while server.state.lock().unwrap().polls < 2 {
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    cancel.store(true, Ordering::Release);
    assert!(thread
        .join()
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
    let count = polls_started.load(Ordering::Acquire);
    assert!(count > 0, "positive control: client started async polls");
    std::thread::sleep(Duration::from_millis(100));
    let s = server.state.lock().unwrap();
    assert_eq!(polls_started.load(Ordering::Acquire), count);
    assert!(s.calls.iter().all(|c| c.method != "DELETE"));
}
#[test]
fn partials_failed_shards_clusters_and_server_timeout_keep_labelled_rows() {
    for scenario in [
        Scenario {
            partial: true,
            ..Default::default()
        },
        Scenario {
            timed_out: true,
            ..Default::default()
        },
    ] {
        let server = server(13, scenario.clone());
        let mut history = History::for_source(client(&server), &source(&server)).unwrap();
        let page = history.page().unwrap();
        assert_eq!(page.events.len(), 13);
        assert!(!page.complete && page.status.incomplete());
        if scenario.partial {
            assert_eq!(page.status.failed_shards, 1);
            assert_eq!(page.status.partial_clusters, 1);
            assert!(page.status.label(13).contains("Partial"));
        } else {
            assert!(page.status.timed_out);
        }
        assert!(!history.can_page());
    }
}
#[test]
fn exact_and_lower_bound_totals_in_both_shapes_and_unbounded_legacy_total() {
    for legacy_total in [false, true] {
        let server = server(
            17,
            Scenario {
                legacy_total,
                ..Default::default()
            },
        );
        let mut params = source(&server)
            .discover
            .search_body_for(&source(&server).discover.time, None, 3, "@timestamp")
            .unwrap();
        params["index"] = json!("remote-*:events");
        for (track, expected, relation) in [
            (json!(true), 17, TotalRelation::Eq),
            (json!(10), 10, TotalRelation::Gte),
            (json!(100), 17, TotalRelation::Eq),
        ] {
            params["track_total_hits"] = track.clone();
            let (response, _) = client(&server)
                .search(&params, Duration::from_secs(1))
                .unwrap();
            let status = ResultStatus::parse(&response, &track, true).unwrap();
            assert_eq!(
                status.total.unwrap(),
                crate::remote_log::result::HitTotal {
                    value: expected,
                    relation
                }
            );
        }
    }
}
#[test]
fn status_denied_other_version_and_data_view_fallback_are_capability_checked() {
    let server = server(
        5,
        Scenario {
            status_denied: true,
            data_view_denied: true,
            ..Default::default()
        },
    );
    let direct = source(&server);
    let page = History::for_source(client(&server), &direct)
        .unwrap()
        .page()
        .unwrap();
    assert_eq!(page.events.len(), 5);
    assert_eq!(page.status.version, None);
    let mut imported = direct.clone();
    imported.index_pattern = None;
    let err = History::for_source(client(&server), &imported)
        .err()
        .unwrap();
    assert!(err.is::<IndexPatternRequired>());
    assert_eq!(imported.discover, direct.discover);
    server.state.lock().unwrap().scenario = Scenario::default();
    let page = History::for_source(client(&server), &imported)
        .unwrap()
        .page()
        .unwrap();
    assert_eq!(page.status.version.as_deref(), Some("10.1.2"));
    server.state.lock().unwrap().scenario.malformed = true;
    assert!(History::for_source(client(&server), &direct)
        .unwrap()
        .page()
        .unwrap_err()
        .to_string()
        .contains("Malformed"));
}
#[test]
fn fields_preserve_types_missing_counts_and_normalized_keys_generate_keyword_terms() {
    let server = server(11, Scenario::default());
    let source = source(&server);
    let client = client(&server);
    let request = |name: &str, field_type, limit| FieldValuesRequest {
        field: name.into(),
        field_type,
        limit,
    };
    let service = client
        .field_values(&source, &request("service.keyword", FieldType::String, 1))
        .unwrap();
    assert_eq!(
        service.buckets[0].value,
        FieldValue::String("checkout".into())
    );
    assert_eq!(service.buckets[0].count, 6);
    assert_eq!(service.sum_other_doc_count, 5);
    assert_eq!(service.missing_count, 0);
    assert_eq!(
        service.buckets[0].value.filter("service.keyword"),
        json!({"term":{"service.keyword":"checkout"}})
    );
    let mut selected_source = source.clone();
    selected_source.index_pattern = Some("remote-a:events".into());
    let selected = client
        .field_values(
            &selected_source,
            &request("service.keyword", FieldType::String, 10),
        )
        .unwrap();
    assert_eq!(
        selected.buckets,
        vec![FieldBucket {
            value: FieldValue::String("checkout".into()),
            count: 6
        }]
    );
    let mut filtered = source.clone();
    filtered.discover.filters =
        vec![json!({"query":service.buckets[0].value.filter("service.keyword")})];
    let values = client
        .field_values(&filtered, &request("duration", FieldType::Number, 2))
        .unwrap();
    assert!(values
        .buckets
        .iter()
        .all(|b| matches!(b.value, FieldValue::Number(_))));
    assert_eq!(values.buckets[0].value, FieldValue::Number(0.into()));
    assert_eq!(values.buckets[1].value, FieldValue::Number(2.into()));
    assert_eq!(values.missing_count, 0);
    assert_eq!(values.sum_other_doc_count, 4);
    let values = client
        .field_values(&source, &request("duration", FieldType::Number, 20))
        .unwrap();
    assert_eq!(values.missing_count, 5);
    let values = client
        .field_values(&source, &request("active", FieldType::Boolean, 20))
        .unwrap();
    assert_eq!(values.buckets[0].value, FieldValue::Boolean(true));
    for (path, expected) in [
        ("context.operation", "read"),
        ("pod.info.name", "synthetic-pod"),
    ] {
        let nested = client
            .field_values(&source, &request(path, FieldType::String, 10))
            .unwrap();
        assert_eq!(nested.missing_count, 0);
        assert_eq!(
            nested.buckets,
            vec![FieldBucket {
                value: FieldValue::String(expected.into()),
                count: 11
            }]
        );
    }
    let missing = client
        .field_values(&source, &request("unknown", FieldType::String, 10))
        .unwrap();
    assert!(missing.buckets.is_empty());
    assert_eq!(missing.missing_count, 11);
    let rows: Vec<_> = (0..11).map(|i| Arc::new(record(i))).collect();
    let fields = describe_fields(&rows);
    assert!(!fields.iter().any(|f| f.path == "message"));
    assert!(fields.iter().any(|f| f.path == "details"
        && f.types.contains(&FieldType::String)
        && f.missing_count == 0));
    assert!(fields
        .iter()
        .any(|f| f.path == "context.operation" && f.types.contains(&FieldType::String)));
    assert!(fields.iter().any(|f| f.path == "pod.info.name"));
    assert!(fields
        .iter()
        .any(|f| f.path == "duration" && f.missing_count == 5 && f.present_count == 6));
    assert_ne!(FieldValue::String("true".into()), FieldValue::Boolean(true));
    assert_ne!(FieldValue::Missing, FieldValue::String("".into()));
}
#[test]
fn fake_rejects_nested_body_and_missing_headers_with_positive_control() {
    let server = server(2, Scenario::default());
    let client = client(&server);
    let good = json!({"params":{"index":"remote-*:events","size":1,"track_total_hits":true,"query":{"match_all":{}},"sort":[{"@timestamp":"asc"},{"_doc":"asc"}]}});
    assert!(client
        .request(Method::POST, "/internal/search/es", &[], Some(&good), false)
        .is_ok());
    let bad = json!({"params":{"index":"remote-*:events","body":good["params"]}});
    assert!(client
        .request(Method::POST, "/internal/search/es", &[], Some(&bad), false)
        .is_err());
    let mut unsupported = good.clone();
    unsupported["params"]["query"] = json!({"kql":{"query":"service:checkout"}});
    assert!(client
        .request(
            Method::POST,
            "/internal/search/es",
            &[],
            Some(&unsupported),
            false
        )
        .is_err());
    let response = client
        .runtime
        .block_on(async {
            client
                .client
                .post(format!("{}/internal/search/es", server.endpoint))
                .header("Authorization", "ApiKey test-secret")
                .json(&good)
                .send()
                .await
        })
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    assert_eq!(server.state.lock().unwrap().rejected_headers, 1);
}
#[test]
fn legacy_sources_load_losslessly_and_direct_source_validation_keeps_https_in_release() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let source = Source::import(
        "Legacy",
        &crate::remote_log::tests::url("(index:events,query:(language:kuery,query:''))", "()"),
    )
    .unwrap();
    let mut legacy = serde_json::to_value(&source).unwrap();
    legacy.as_object_mut().unwrap().remove("index_pattern");
    legacy.as_object_mut().unwrap().remove("time_field");
    let path = dir.path().join("remote-log-sources");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join(format!("{}.json", crate::remote_log::vault_scope(&vault))),
        serde_json::to_vec(&vec![legacy]).unwrap(),
    )
    .unwrap();
    let loaded = crate::remote_log::load_sources(dir.path(), &vault)
        .unwrap()
        .remove(0);
    assert_eq!(loaded.id, source.id);
    assert_eq!(loaded.discover, source.discover);
    assert_eq!(loaded.time_field, "@timestamp");
    assert!(loaded.index_pattern.is_none());
    crate::remote_log::save_source(dir.path(), &vault, &loaded).unwrap();
    assert_eq!(
        crate::remote_log::load_sources(dir.path(), &vault).unwrap()[0].discover,
        source.discover
    );
    assert!(Source::direct("Test", "http://192.0.2.1", "events", "@timestamp").is_err());
    assert!(Source::direct(
        "Test",
        "https://user:secret@logs.example",
        "events",
        "@timestamp"
    )
    .is_err());
}

#[test]
fn forbidden_search_and_http_deadline_are_typed_without_echoing_the_key() {
    let server = server(
        2,
        Scenario {
            forbidden: true,
            ..Default::default()
        },
    );
    let error = History::for_source(client(&server), &source(&server))
        .unwrap()
        .page()
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AuthError>(),
        Some(&AuthError { status: 403 })
    );
    assert!(!format!("{error:#}").contains("test-secret"));
    server.state.lock().unwrap().scenario = Scenario {
        delay: Duration::from_millis(150),
        ..Default::default()
    };
    let mut history = History::for_source(client(&server), &source(&server)).unwrap();
    history.search_timeout = Duration::from_millis(30);
    assert!(history.page().unwrap_err().is::<SearchTimeout>());
    assert!(
        server.state.lock().unwrap().searches > 0,
        "positive control: the delayed server received a real search"
    );
}
#[test]
fn synchronous_optional_flags_custom_time_field_and_upper_bound_remain_supported() {
    let server = server(
        3,
        Scenario {
            omit_completion_flags: true,
            ..Default::default()
        },
    );
    let mut source = source(&server);
    source.time_field = "event_time".into();
    source.discover.sort[0].field = "event_time".into();
    {
        let mut s = server.state.lock().unwrap();
        for record in &mut s.records {
            let stamp = record["_source"]["@timestamp"].clone();
            record["_source"]
                .as_object_mut()
                .unwrap()
                .remove("@timestamp");
            record["_source"]["event_time"] = stamp;
        }
        let mut at_end = record(4);
        at_end["_source"]["event_time"] = json!(source.discover.time.to);
        s.records.push(at_end);
    }
    let mut history = History::for_source(client(&server), &source).unwrap();
    let mut rows = Vec::new();
    while history.can_page() {
        let page = history.page().unwrap();
        assert!(!page.status.incomplete());
        rows.extend(page.events);
    }
    assert_eq!(
        rows.len(),
        3,
        "half-open range excludes the document exactly at the upper bound"
    );
    assert!(server
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(
            |c| c.body["params"]["query"]["bool"]["filter"][0]["range"]["event_time"]["lt"]
                == source.discover.time.to
        ));
}

#[test]
fn redirects_query_proxy_auth_and_outage_failures_remain_distinct_and_secret_free() {
    for status in [302, 400, 401, 403, 407, 503] {
        let server = server(
            1,
            Scenario {
                failure_status: Some(status),
                ..Default::default()
            },
        );
        let error = History::for_source(client(&server), &source(&server))
            .unwrap()
            .page()
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(!message.contains("test-secret"));
        match status {
            302 => assert!(message.contains("not forwarded")),
            400 => assert!(message.contains("unsupported")),
            401 | 403 => assert_eq!(
                error.downcast_ref::<AuthError>(),
                Some(&AuthError { status })
            ),
            407 => assert!(
                message.contains("proxy requires authentication")
                    && message.contains("not supported")
            ),
            503 => assert!(error.is::<Transient>()),
            _ => unreachable!(),
        }
        assert_eq!(server.state.lock().unwrap().calls.iter().filter(|c|c.path.ends_with("/internal/search/es")).count(),1,"positive control: the real search failed once, with no implicit retry or credential-forwarding redirect");
    }
}

#[test]
fn async_deadline_without_intermediate_hits_is_still_a_visible_timeout() {
    let server = server(
        3,
        Scenario {
            never_complete: true,
            running_without_hits: true,
            ..Default::default()
        },
    );
    let (arrived, polls) = std::sync::mpsc::channel();
    let (release, permit) = std::sync::mpsc::channel();
    let permit = Mutex::new(permit);
    server.state.lock().unwrap().after_poll = Some(Arc::new(move |count| {
        arrived.send(count).unwrap();
        permit.lock().unwrap().recv().unwrap();
    }));
    let clock = Arc::new(TestClock::new());
    let mut history = History::for_source(
        client(&server).with_test_clock(clock.clone()).unwrap(),
        &source(&server),
    )
    .unwrap();
    history.search_timeout = Duration::from_millis(120);
    let query = std::thread::spawn(move || history.page().unwrap());
    // Establish a real empty async poll before expiring the same 120 ms
    // budget. Client/server scheduling is not part of this timeout semantic.
    assert_eq!(polls.recv().unwrap(), 1);
    assert_eq!(clock.expirations(), 0);
    clock.advance(Duration::from_millis(120));
    let page = query.join().unwrap();
    assert_eq!(clock.expirations(), 1, "the actual HTTP deadline must fire");
    release.send(()).unwrap();
    assert!(page.status.timed_out && page.status.partial);
    assert!(!page.complete);
    assert!(page.events.is_empty());
    assert!(page.status.total.is_none());
    assert!(
        server.state.lock().unwrap().polls > 0,
        "positive control: server returned running envelopes without hits before the deadline"
    );
}

#[test]
fn source_bound_clients_reject_another_identity_or_endpoint_before_sending_any_key() {
    let server = server(11, Scenario::default());
    let selected = source(&server);
    let client = Kibana::new(
        &selected,
        ApiKey::new(b"test-secret".to_vec()).unwrap(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let request = FieldValuesRequest {
        field: "service.keyword".into(),
        limit: 10,
        field_type: FieldType::String,
    };
    assert_eq!(
        client
            .field_values(&selected, &request)
            .unwrap()
            .buckets
            .len(),
        2,
        "positive control: the bound source performs an authenticated search"
    );
    let before = server.state.lock().unwrap().calls.len();
    let mut other = selected.clone();
    other.id = uuid::Uuid::new_v4();
    assert!(client
        .field_values(&other, &request)
        .unwrap_err()
        .to_string()
        .contains("another source"));
    other.id = selected.id;
    other.endpoint = "https://other.example".into();
    assert!(client
        .field_values(&other, &request)
        .unwrap_err()
        .to_string()
        .contains("another source"));
    assert!(History::for_source(client.clone(), &other).is_err());
    assert_eq!(
        server.state.lock().unwrap().calls.len(),
        before,
        "source mismatch cannot send the key or query to either endpoint"
    );
    let mut legacy = History::open(client, selected.discover.clone()).unwrap();
    assert!(!legacy.page().unwrap().events.is_empty());
}

#[test]
fn field_catalog_uses_advertised_scalar_mappings_and_keyword_multifields() {
    let server = server(12, Scenario::default());
    let fields = client(&server)
        .aggregatable_fields(&source(&server))
        .unwrap();
    assert!(fields
        .iter()
        .any(|f| f.name == "service.keyword" && f.field_type == FieldType::String));
    assert!(fields
        .iter()
        .any(|f| f.name == "active" && f.field_type == FieldType::Boolean));
    assert!(fields
        .iter()
        .any(|f| f.name == "duration" && f.field_type == FieldType::Number));
    assert!(
        !fields.iter().any(|f| f.name == "details"),
        "analyzed text is not an exact-value picker"
    );
    assert!(fields
        .iter()
        .any(|f| f.name == "details.keyword" && f.source_path == "details"));
    assert!(
        server.state.lock().unwrap().calls.iter().any(|c| c
            .path
            .starts_with("/internal/data_views/_fields_for_wildcard?pattern=remote-")),
        "positive control: queried the server catalog for the selected pattern"
    );
    assert_eq!(server.state.lock().unwrap().rejected_headers, 0);
}

#[test]
fn search_catalog_fallback_denied_missing_empty_malformed_with_typed_scope_and_counts() {
    for scenario in [
        Scenario {
            catalog_status: Some(401),
            ..Default::default()
        },
        Scenario {
            catalog_status: Some(403),
            ..Default::default()
        },
        Scenario {
            catalog_status: Some(404),
            ..Default::default()
        },
        Scenario {
            catalog_status: Some(400),
            ..Default::default()
        },
        Scenario {
            catalog_empty: true,
            ..Default::default()
        },
        Scenario {
            catalog_malformed: true,
            ..Default::default()
        },
    ] {
        let server = server(
            12,
            Scenario {
                legacy_total: true,
                status_denied: true,
                async_polls: 1,
                ..scenario
            },
        );
        let source = source(&server);
        let client = client(&server);
        let catalog = client.field_catalog(&source).unwrap();
        assert!(catalog.fallback);
        assert!(!catalog.limited);
        let service = catalog
            .fields
            .iter()
            .find(|f| f.source_path == "service")
            .unwrap();
        assert_eq!(
            service.name, "service.keyword",
            "base text aggregation fails"
        );
        assert!(catalog.fields.iter().any(|f| f.name == "context.operation"));
        assert!(catalog
            .observed
            .iter()
            .any(|f| f.path == "labels" && f.types.contains(&FieldType::Array)));
        assert!(catalog
            .observed
            .iter()
            .any(|f| f.path == "duration" && f.missing_count == 6));
        assert!(catalog.observed.iter().any(|f| f.path == "text_only"));
        assert!(!catalog.fields.iter().any(|f| f.source_path == "text_only"));
        let values = client
            .field_values(
                &source,
                &FieldValuesRequest {
                    field: service.name.clone(),
                    field_type: service.field_type,
                    limit: 1000,
                },
            )
            .unwrap();
        assert_eq!(
            values.buckets[0].value,
            FieldValue::String("checkout".into())
        );
        let mut scoped = source.clone();
        scoped
            .discover
            .filters
            .push(json!({"query":values.buckets[0].value.filter(&service.name)}));
        let page = History::for_source(client.clone(), &scoped)
            .unwrap()
            .page()
            .unwrap();
        assert_eq!(
            page.events.len(),
            6,
            "normalized key is compiled against confirmed multifield"
        );
        let category = catalog
            .fields
            .iter()
            .find(|f| f.source_path == "category")
            .unwrap();
        assert_eq!(
            category.name, "category",
            "standalone keyword uses its confirmed base field"
        );
        let normalized = client
            .field_values(
                &source,
                &FieldValuesRequest {
                    field: category.name.clone(),
                    field_type: category.field_type,
                    limit: 1000,
                },
            )
            .unwrap();
        assert_eq!(
            normalized.buckets[0].value,
            FieldValue::String("batch".into())
        );
        scoped.discover.filters =
            vec![json!({"query":normalized.buckets[0].value.filter(&category.name)})];
        let page = History::for_source(client.clone(), &scoped)
            .unwrap()
            .page()
            .unwrap();
        assert_eq!(page.events.len(), 6);
        assert!(page
            .events
            .iter()
            .all(|e| e["_source"]["category"] == "Batch"));
        let numbers = client
            .field_values(
                &source,
                &FieldValuesRequest {
                    field: "duration".into(),
                    field_type: FieldType::Number,
                    limit: 1000,
                },
            )
            .unwrap();
        assert_eq!(numbers.missing_count, 6);
        assert!(matches!(numbers.buckets[0].value, FieldValue::Number(_)));
        scoped.discover.filters =
            vec![json!({"query":numbers.buckets[0].value.filter("duration")})];
        assert_eq!(
            History::for_source(client.clone(), &scoped)
                .unwrap()
                .page()
                .unwrap()
                .events
                .len(),
            1
        );
        scoped.discover.filters = vec![json!({"query":FieldValue::Missing.filter("duration")})];
        assert_eq!(
            History::for_source(client.clone(), &scoped)
                .unwrap()
                .page()
                .unwrap()
                .events
                .len(),
            6
        );
        scoped.discover.filters = vec![json!({"query":FieldValue::Boolean(true).filter("active")})];
        assert_eq!(
            History::for_source(client.clone(), &scoped)
                .unwrap()
                .page()
                .unwrap()
                .events
                .len(),
            6
        );
        let details = client.connection_details().copy_text();
        assert!(details.contains("field catalog: HTTP"));
        assert!(details.contains("fallback probe: HTTP"));
        assert!(details.contains("search sample + terms probes"));
        assert!(details.contains("async poll: HTTP 200"));
        assert!(details.contains("Kibana version: not readable"));
        for private in [
            server.endpoint.as_str(),
            "remote-*:events",
            "remote-a",
            "test-secret",
            "Checkout",
            "checkout",
            "service.keyword",
        ] {
            assert!(!details.contains(private), "diagnostics leaked {private}");
        }
        let state = server.state.lock().unwrap();
        assert!(
            state.calls.iter().any(|c| c.body["params"]["aggs"]
                .as_object()
                .is_some_and(|a| a.len() > 1 && a.contains_key("probe_0"))),
            "positive control: combined bounded probe ran"
        );
        assert!(state.calls.iter().any(|c| c.body["params"]["query"]
            .to_string()
            .contains("service.keyword")));
        assert_eq!(state.rejected_headers, 0);
    }
}

#[test]
fn search_catalog_positive_control_and_empty_window_do_not_invent_mappings() {
    let server = server(12, Scenario::default());
    let client = client(&server);
    let catalog = client.field_catalog(&source(&server)).unwrap();
    assert!(!catalog.fallback);
    assert!(client
        .connection_details()
        .copy_text()
        .contains("fallback not used"));
    assert!(
        !server
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|c| c.path.ends_with("/internal/search/es")),
        "positive control: available catalog bypasses sample and probes"
    );
    server.state.lock().unwrap().scenario.catalog_empty = true;
    let mut source = source(&server);
    source.discover.time = TimeRange {
        from: "2020-01-01T00:00:00Z".into(),
        to: "2020-01-02T00:00:00Z".into(),
    };
    let catalog = client.field_catalog(&source).unwrap();
    assert!(catalog.fallback);
    assert!(catalog.fields.is_empty());
    assert!(catalog.observed.is_empty());
}

#[test]
fn search_catalog_bounds_sample_paths_batches_and_probe_requests() {
    let server = server(
        256,
        Scenario {
            catalog_empty: true,
            ..Default::default()
        },
    );
    for row in &mut server.state.lock().unwrap().records {
        for i in 0..80 {
            row["_source"][format!("candidate_{i:03}")] = json!("synthetic analyzed text");
        }
    }
    let catalog = client(&server).field_catalog(&source(&server)).unwrap();
    assert!(catalog.fallback && catalog.limited);
    assert!(
        catalog
            .fields
            .iter()
            .all(|f| !f.name.starts_with("candidate_")),
        "failed probes never become mappings"
    );
    let state = server.state.lock().unwrap();
    let probes = state
        .calls
        .iter()
        .filter(|c| c.body["params"]["aggs"].get("probe_0").is_some())
        .collect::<Vec<_>>();
    assert!(!probes.is_empty());
    assert!(probes.len() <= 64);
    assert!(probes
        .iter()
        .all(|p| p.body["params"]["aggs"].as_object().unwrap().len() <= 8));
    assert!(state.calls.iter().any(|c| c.body["params"]["size"] == 128));
    assert!(catalog.observed.iter().all(|f| f.present_count <= 128));
}

#[test]
fn search_catalog_cancelled_never_falls_back_and_probe_budget_expires() {
    let server = server(
        12,
        Scenario {
            catalog_status: Some(403),
            ..Default::default()
        },
    );
    let client = client(&server);
    client.cancelled.store(true, Ordering::Release);
    assert!(client.field_catalog(&source(&server)).is_err());
    assert!(server.state.lock().unwrap().calls.is_empty());
    client.cancelled.store(false, Ordering::Release);
    let mut requests = 0;
    let mut confirmed = Vec::new();
    let mut limited = false;
    // The very same probe is exercised successfully by the fallback tests;
    // an exhausted whole-discovery deadline must suppress all HTTP requests.
    client
        .probe_fields(
            &json!({}),
            &[super::super::result::AggregatableField {
                name: "service.keyword".into(),
                source_path: "service".into(),
                field_type: FieldType::String,
            }],
            std::time::Instant::now() - Duration::from_secs(9),
            &mut requests,
            &mut confirmed,
            &mut limited,
        )
        .unwrap();
    assert!(limited && confirmed.is_empty());
    assert_eq!(requests, 0);
    assert!(server.state.lock().unwrap().calls.is_empty());
}

#[test]
fn connection_details_last_attempt_excludes_late_http_failures_from_previous_source() {
    let server = server(
        12,
        Scenario {
            data_view_denied: true,
            delay: Duration::from_millis(100),
            ..Default::default()
        },
    );
    let (worker, _events) = crate::remote_log::live::Worker::start();
    let previous = worker.diagnostics();
    let old_client = client(&server).with_diagnostics(previous.clone());
    let mut imported = source(&server);
    imported.index_pattern = None;
    let late = std::thread::spawn(move || old_client.resolve_source(&imported));
    let began = std::time::Instant::now();
    while !server
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|c| c.path.contains("/api/data_views/"))
    {
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "positive control: previous HTTP request must fire"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    worker.begin_attempt();
    let next = client(&server).with_diagnostics(worker.diagnostics());
    next.field_values(
        &source(&server),
        &FieldValuesRequest {
            field: "duration".into(),
            field_type: FieldType::Number,
            limit: 1000,
        },
    )
    .unwrap();
    assert!(late.join().unwrap().is_err());
    assert!(
        previous
            .lock()
            .unwrap()
            .copy_text()
            .contains("data view: HTTP 403"),
        "positive control: the late failure was actually recorded"
    );
    let details = worker.diagnostics().lock().unwrap().copy_text();
    assert!(details.contains("data view: not attempted"));
    assert!(details.contains("search: HTTP 200"));
    assert!(
        !details.contains("403"),
        "a previous source cannot contaminate this attempt's diagnostic text"
    );
}
