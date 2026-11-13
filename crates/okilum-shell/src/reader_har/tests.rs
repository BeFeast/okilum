// Reader-hosted acceptance for the HAR viewer (#2836). Fixtures are synthetic.
use super::*;
use ::core::prelude::v1::test;
use reader_viewer_chrome::tests::{fixture_with_files, measured_bounds};

const SECRET: &str = "tok-2836-do-not-log";

fn entry(
    method: &str,
    url: &str,
    status: i64,
    started: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut entry = serde_json::json!({
        "startedDateTime": started,
        "time": 25.5,
        "request": {
            "method": method,
            "url": url,
            "httpVersion": "HTTP/1.1",
            "headers": [{ "name": "authorization", "value": format!("Bearer {SECRET}") }],
            "queryString": [],
            "cookies": [],
            "headersSize": -1,
            "bodySize": -1
        },
        "response": {
            "status": status,
            "statusText": "",
            "httpVersion": "HTTP/1.1",
            "headers": [{ "name": "content-type", "value": "application/json" }],
            "cookies": [],
            "content": { "size": 18, "mimeType": "application/json", "text": "{\"ok\":true,\"n\":1}" },
            "redirectURL": "",
            "headersSize": 100,
            "bodySize": 18
        },
        "timings": { "blocked": 1, "dns": -1, "connect": -1, "ssl": -1, "send": 0.5, "wait": 20, "receive": 4 }
    });
    if let (Some(target), Some(extra)) = (entry.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    entry
}

fn capture(entries: Vec<serde_json::Value>) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "log": { "version": "1.2", "creator": { "name": "synthetic", "version": "1" }, "entries": entries }
    }))
    .unwrap()
}

fn sample() -> String {
    capture(vec![
        entry(
            "GET",
            "https://app.example.test/index.html",
            200,
            "2026-11-13T08:00:00.000Z",
            serde_json::json!({}),
        ),
        entry(
            "POST",
            "https://api.example.test/v1/login",
            401,
            "2026-11-13T08:00:00.200Z",
            serde_json::json!({ "request": {
                "method": "POST", "url": "https://api.example.test/v1/login", "headers": [],
                "postData": { "mimeType": "application/json", "text": "{\"user\":\"ann\",\"password\":\"hunter2\"}" }
            }}),
        ),
        entry(
            "GET",
            "https://cdn.example.test/logo.png",
            0,
            "2026-11-13T08:00:00.300Z",
            serde_json::json!({ "_error": "net::ERR_FAILED" }),
        ),
    ])
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    files: &[(&str, &str)],
    name: &str,
) -> (Entity<Reader>, &'a mut VisualTestContext, tempfile::TempDir) {
    cx.executor().allow_parking();
    let (reader, visual, temp) = fixture_with_files(cx, "note.md", files);
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file(name, window, cx)
    });
    draw(visual);
    (reader, visual, temp)
}

fn draw(visual: &mut VisualTestContext) {
    for _ in 0..3 {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
    }
    visual.run_until_parked();
}

fn har(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> Entity<HarView> {
    reader.read_with(visual, |reader, _| {
        reader
            .file_preview
            .as_ref()
            .unwrap()
            .har
            .clone()
            .expect("HAR view")
    })
}

fn rows(har: &Entity<HarView>, visual: &mut VisualTestContext) -> Vec<usize> {
    har.read_with(visual, |har, _| {
        (0..har.rows.len())
            .filter_map(|p| har.rows.get(p))
            .collect()
    })
}

fn status_text(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> String {
    reader.read_with(visual, |reader, cx| {
        let status = reader.viewer_chrome(cx).status.unwrap();
        format!("{} {}", status.label, status.text)
    })
}

#[gpui::test]
fn har2836_opens_as_a_request_list_with_a_summary(cx: &mut TestAppContext) {
    let text = sample();
    let (reader, visual, _temp) = open(cx, &[("capture.har", &text)], "capture.har");
    let har = har(&reader, visual);
    assert_eq!(rows(&har, visual), [0, 1, 2]);
    assert!(measured_bounds(visual, "har-row-2").is_some());
    let status = status_text(&reader, visual);
    assert!(status.contains("3 requests"), "{status}");
    assert!(status.contains("2 errors"), "{status}");
    let views: Vec<_> = reader.read_with(visual, |reader, cx| {
        reader
            .viewer_chrome(cx)
            .views
            .iter()
            .map(|v| (v.action.id, v.active))
            .collect()
    });
    assert_eq!(
        views,
        [("har-view-requests", true), ("har-view-source", false)]
    );
    // Read-only: no Edit slot and no editor authority over the capture.
    reader.read_with(visual, |reader, _| assert!(reader.editing.is_none()));
    assert!(!reader_delimited::editable("capture.har"));
    assert!(reader_code_file::eligible("capture.har"));
}

#[gpui::test]
fn har2836_find_filters_on_a_worker_without_header_renders_or_side_panel_notifications(
    cx: &mut TestAppContext,
) {
    struct Observer {
        _subscriptions: Vec<Subscription>,
    }
    let _cached = reader_side_panel::tests::CachedPanels::on();
    let text = sample();
    let (reader, visual, _temp) = open(cx, &[("capture.har", &text)], "capture.har");
    let har = har(&reader, visual);
    reader.update_in(visual, |reader, window, cx| reader.open_find(window, cx));
    visual.run_until_parked();
    visual.simulate_keystrokes("home");
    draw(visual);
    let renders = || reader_viewer_chrome::header_renders(reader.entity_id());
    let initial = renders();
    assert!(initial > 0, "positive control: header slots were rendered");
    let panels = reader.read_with(visual, |reader, _| reader.side_panels.clone());
    let notifications = Rc::new(std::cell::Cell::new([0; 2]));
    let _observer = visual.update(|_, cx| {
        cx.new(|cx: &mut Context<Observer>| Observer {
            _subscriptions: panels
                .iter()
                .enumerate()
                .filter_map(|(i, panel)| Some((i, panel.clone()?)))
                .map(|(i, panel)| {
                    let notifications = notifications.clone();
                    cx.observe(&panel, move |_, _, _| {
                        let mut counts = notifications.get();
                        counts[i] += 1;
                        notifications.set(counts);
                    })
                })
                .collect(),
        })
    });
    visual.update(|_, cx| {
        for panel in panels.iter().flatten() {
            panel.update(cx, |_, cx| cx.notify());
        }
    });
    draw(visual);
    assert!(
        notifications.get().iter().any(|count| *count > 0),
        "positive control: the observer sees panel notifications"
    );
    notifications.set([0, 0]);
    let initial = renders();
    for ch in "login".chars() {
        visual.simulate_input(&ch.to_string());
        draw(visual);
        assert_eq!(
            renders(),
            initial,
            "typing in Find renders zero header slots"
        );
        assert_eq!(
            notifications.get(),
            [0, 0],
            "typing never notifies SidePanels"
        );
    }
    assert_eq!(rows(&har, visual), [1]);
    reader.read_with(visual, |reader, cx| {
        assert_eq!(
            reader.viewer_chrome(cx).find.unwrap().count.as_deref(),
            Some("1 of 3")
        );
    });
    // The projection is a worker result: the rows change only after it runs.
    har.update_in(visual, |har, window, cx| {
        har.filter
            .update(cx, |input, cx| input.set_value("cdn", window, cx));
        har.refresh(cx);
        assert_eq!(
            har.rows.len(),
            1,
            "stale rows stay until the worker answers"
        );
    });
    draw(visual);
    assert_eq!(rows(&har, visual), [2]);
}

#[gpui::test]
fn har2836_arriving_batches_render_no_header_slots(cx: &mut TestAppContext) {
    let text = sample();
    let (reader, visual, _temp) = open(cx, &[("capture.har", &text)], "capture.har");
    let har = har(&reader, visual);
    draw(visual);
    let initial = reader_viewer_chrome::header_renders(reader.entity_id());
    assert!(initial > 0);
    let before = status_text(&reader, visual);
    for batch in 0..3 {
        har.update(visual, |har, cx| {
            let entries = (0..50)
                .map(|i| Entry {
                    method: "GET".into(),
                    url: format!("https://more.example.test/{batch}/{i}").into(),
                    status: Some(200),
                    ..Default::default()
                })
                .collect();
            har.apply(Message::Batch(entries, 0));
            har.refresh(cx);
            cx.notify();
        });
        draw(visual);
        assert_eq!(
            reader_viewer_chrome::header_renders(reader.entity_id()),
            initial,
            "progress renders zero header slots"
        );
    }
    assert_eq!(har.read_with(visual, |har, _| har.rows.len()), 153);
    assert_ne!(
        status_text(&reader, visual),
        before,
        "control: the summary changed"
    );
}

#[gpui::test]
fn har2836_details_load_lazily_cancel_stale_reads_and_keep_focus(cx: &mut TestAppContext) {
    let text = sample();
    let (reader, visual, _temp) = open(cx, &[("capture.har", &text)], "capture.har");
    let har = har(&reader, visual);
    let filter = har.read_with(visual, |har, _| har.filter.clone());
    filter.update_in(visual, |input, window, cx| input.focus(window, cx));
    visual.run_until_parked();
    let focused = |visual: &mut VisualTestContext| {
        visual.update(|window, cx| filter.read(cx).focus_handle(cx).is_focused(window))
    };
    assert!(focused(visual), "control: the filter has focus");
    let first_cancel = har.update(visual, |har, cx| {
        har.select(2, cx);
        assert!(matches!(har.detail, Some(Detail::Loading)));
        har.detail_cancel.clone()
    });
    har.update(visual, |har, cx| har.select(1, cx));
    assert!(
        first_cancel.load(Ordering::Relaxed),
        "a newer selection cancels the older read"
    );
    draw(visual);
    assert!(focused(visual), "a finished detail read never moves focus");
    let rows = har.read_with(visual, |har, _| match &har.detail {
        Some(Detail::Ready(rows)) => rows.rows.clone(),
        _ => panic!("details are ready"),
    });
    let field = |name: &str| {
        rows.iter().find_map(|row| match row {
            Row::Field { name: n, value } if n.as_ref() == name => Some(value.to_string()),
            _ => None,
        })
    };
    assert_eq!(
        field("URL").as_deref(),
        Some("https://api.example.test/v1/login")
    );
    assert_eq!(field("Status").as_deref(), Some("401"));
    assert!(rows.iter().any(|row| matches!(
        row,
        Row::Section {
            section: Section::RequestBody,
            ..
        }
    )));
    // JSON bodies are indented for reading.
    assert!(
        rows.contains(&Row::Text("  \"password\": \"hunter2\"".into())),
        "{rows:#?}"
    );
    assert!(rows.iter().any(|row| matches!(row, Row::Timing { .. })));
    assert!(measured_bounds(visual, "har-detail").is_some());
    // Folding a section keeps its title row.
    let before = har.read_with(visual, |har, _| har.detail_visible.len());
    har.update(visual, |har, cx| har.toggle_section(Section::General, cx));
    let after = har.read_with(visual, |har, _| har.detail_visible.len());
    assert!(after < before && after > 0);
}

#[gpui::test]
fn har2836_switching_files_cancels_indexing_and_releases_the_view(cx: &mut TestAppContext) {
    let entries: Vec<_> = (0..3000)
        .map(|i| {
            entry(
                "GET",
                &format!("https://bulk.example.test/{i}"),
                200,
                "2026-11-13T08:00:00Z",
                serde_json::json!({}),
            )
        })
        .collect();
    let big = capture(entries);
    cx.executor().allow_parking();
    let (reader, visual, _temp) = fixture_with_files(cx, "note.md", &[("big.har", &big)]);
    // Control: left alone, the same file indexes completely.
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("big.har", window, cx)
    });
    draw(visual);
    let complete = har(&reader, visual);
    assert_eq!(complete.read_with(visual, |har, _| har.rows.len()), 3000);
    assert!(!complete.read_with(visual, |har, _| har.cancel.load(Ordering::Relaxed)));
    drop(complete);
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("note.md", window, cx)
    });
    draw(visual);
    // Switch away before the worker has read anything.
    let (cancel, weak) = reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("big.har", window, cx);
        let har = reader.file_preview.as_ref().unwrap().har.as_ref().unwrap();
        let state = (har.read(cx).cancel.clone(), har.downgrade());
        reader.preview_file("data.json", window, cx);
        state
    });
    draw(visual);
    assert!(weak.upgrade().is_none(), "the old view is released");
    assert!(
        cancel.load(Ordering::Relaxed),
        "its index read is cancelled"
    );
}

#[gpui::test]
fn har2836_source_is_the_json_viewer_and_requests_come_back(cx: &mut TestAppContext) {
    let text = sample();
    let (reader, visual, _temp) = open(cx, &[("capture.har", &text)], "capture.har");
    let view = har(&reader, visual).entity_id();
    visual.simulate_resize(size(px(1440.), px(900.)));
    draw(visual);
    let source = measured_bounds(visual, "har-view-source").expect("Source view");
    visual.simulate_click(source.center(), Modifiers::default());
    draw(visual);
    let ids: Vec<_> = reader.read_with(visual, |reader, cx| {
        assert!(reader.file_preview.as_ref().unwrap().code.is_some());
        assert!(!reader.code_file_editable(cx), "Source is read-only");
        reader
            .viewer_chrome(cx)
            .views
            .iter()
            .map(|v| (v.action.id, v.active))
            .collect()
    });
    assert_eq!(ids[0], ("har-view-requests", false));
    assert!(
        ids.iter()
            .any(|(id, active)| id.starts_with("code-view-") && *active),
        "{ids:?}"
    );
    assert!(measured_bounds(visual, "har-list").is_none());
    let requests = measured_bounds(visual, "har-view-requests").expect("Requests view");
    visual.simulate_click(requests.center(), Modifiers::default());
    draw(visual);
    reader.read_with(visual, |reader, _| {
        assert!(reader.file_preview.as_ref().unwrap().code.is_none())
    });
    assert_eq!(
        har(&reader, visual).entity_id(),
        view,
        "the list is kept, not re-read"
    );
    assert!(measured_bounds(visual, "har-list").is_some());
}

#[gpui::test]
fn har2836_damaged_and_foreign_files_explain_and_offer_source(cx: &mut TestAppContext) {
    let text = sample();
    let cut = text.find("cdn.example.test").unwrap();
    let partial = &text[..cut];
    let broken = r#"{"log":{"entries":[{"request":{"url":"#;
    let (reader, visual, _temp) = open(
        cx,
        &[
            ("partial.har", partial),
            ("broken.har", broken),
            ("foreign.har", "[1, 2]"),
        ],
        "partial.har",
    );
    let partial_view = har(&reader, visual);
    assert_eq!(rows(&partial_view, visual), [0, 1]);
    assert!(measured_bounds(visual, "har-banner").is_some());
    assert!(status_text(&reader, visual).contains("Damaged at byte"));
    for (name, expected) in [
        ("broken.har", "damaged and no complete request"),
        ("foreign.har", "not a HAR file"),
    ] {
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file(name, window, cx)
        });
        draw(visual);
        let message = har(&reader, visual).read_with(visual, |har, _| har.message());
        assert!(
            message.as_deref().is_some_and(|m| m.contains(expected)),
            "{message:?}"
        );
        assert!(measured_bounds(visual, "har-message").is_some());
    }
    let button = measured_bounds(visual, "har-show-source").expect("Show source");
    visual.simulate_click(button.center(), Modifiers::default());
    draw(visual);
    reader.read_with(visual, |reader, _| {
        assert!(reader.file_preview.as_ref().unwrap().code.is_some())
    });
}

#[gpui::test]
fn har2836_columns_follow_the_list_pane_not_the_window(cx: &mut TestAppContext) {
    let text = sample();
    let (reader, visual, _temp) = open(cx, &[("capture.har", &text)], "capture.har");
    visual.simulate_resize(size(px(1440.), px(900.)));
    draw(visual);
    let wide = measured_bounds(visual, "har-list").unwrap();
    assert!(wide.size.width >= px(STARTED_FROM), "{wide:?}");
    assert!(measured_bounds(visual, "har-sort-Started").is_some());
    assert!(measured_bounds(visual, "har-sort-Type").is_some());
    // A window wider than the Started threshold whose list pane is narrower.
    visual.simulate_resize(size(px(860.), px(900.)));
    draw(visual);
    let narrow = measured_bounds(visual, "har-list").unwrap();
    assert!(
        narrow.size.width < px(STARTED_FROM),
        "precondition: {narrow:?}"
    );
    assert!(measured_bounds(visual, "har-sort-Started").is_none());
    let _ = reader;
}

#[gpui::test]
fn har2836_sorting_cycles_and_arrow_keys_select(cx: &mut TestAppContext) {
    let text = sample();
    let (reader, visual, _temp) = open(cx, &[("capture.har", &text)], "capture.har");
    let har = har(&reader, visual);
    visual.simulate_resize(size(px(1440.), px(900.)));
    draw(visual);
    let mut order = Vec::new();
    for _ in 0..3 {
        let status = measured_bounds(visual, "har-sort-Status").unwrap();
        visual.simulate_click(status.center(), Modifiers::default());
        draw(visual);
        order.push(rows(&har, visual));
    }
    assert_eq!(order, [vec![2, 0, 1], vec![1, 0, 2], vec![0, 1, 2]]);
    let row = measured_bounds(visual, "har-row-0").unwrap();
    visual.simulate_click(row.center(), Modifiers::default());
    draw(visual);
    assert_eq!(har.read_with(visual, |har, _| har.selected), Some(0));
    visual.simulate_keystrokes("down down");
    draw(visual);
    assert_eq!(har.read_with(visual, |har, _| har.selected), Some(2));
    visual.simulate_keystrokes("escape");
    draw(visual);
    assert_eq!(har.read_with(visual, |har, _| har.selected), None);
}

/// HAR content (tokens, cookies, bodies) must never reach logs, diagnostics or
/// feedback bundles. These modules must not call any logging facility.
#[test]
fn har2836_viewer_sources_never_log_content() {
    for (name, source) in [
        ("reader_har.rs", include_str!("../reader_har.rs")),
        ("details.rs", include_str!("details.rs")),
        ("model.rs", include_str!("model.rs")),
        ("har.rs", include_str!("../../../okilum-core/src/har.rs")),
    ] {
        for forbidden in [
            "diagnostic_",
            "log::",
            "eprintln!",
            "println!",
            "dbg!",
            "tracing::",
        ] {
            assert!(!source.contains(forbidden), "{name} uses {forbidden}");
        }
    }
    // Control: the check sees a real logging call elsewhere.
    assert!(include_str!("../reader_files.rs").contains("diagnostic_error!"));
    let _ = SECRET;
}
