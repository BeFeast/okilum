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
            har.apply(Message::Batch(entries, 0, None));
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
    // Both selections in one update: the first worker has not run yet when
    // the second selection cancels it, so it must stop rather than finish.
    let first_cancel = har.update(visual, |har, cx| {
        har.select(2, cx);
        assert!(matches!(har.detail, Some(Detail::Loading)));
        let first = har.detail_cancel.clone();
        har.select(1, cx);
        first
    });
    assert!(
        first_cancel.load(Ordering::Relaxed),
        "a newer selection cancels the older read"
    );
    draw(visual);
    // The superseded read never finished its parse and formatting: exactly
    // one detail read (the current selection) ran to the end.
    assert_eq!(
        har.read_with(visual, |har, _| har.finished_reads.load(Ordering::Relaxed)),
        1
    );
    assert!(focused(visual), "a finished detail read never moves focus");
    let rows = har.read_with(visual, |har, _| match &har.detail {
        Some(Detail::Ready(rows)) => rows.rows.clone(),
        _ => panic!("details are ready"),
    });
    let field = |name: &str| {
        rows.iter().find_map(|row| match row {
            Row::Field { name: n, value, .. } if n.as_ref() == name => Some(value.to_string()),
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
    // The columns follow the measured list pane, which is narrower than the
    // window by the panels and margins; a window-width rule would differ.
    let har = har(&reader, visual);
    for window in [1440., 1100., 900., 700.] {
        visual.simulate_resize(size(px(window), px(900.)));
        draw(visual);
        let list = measured_bounds(visual, "har-list")
            .unwrap()
            .size
            .width
            .as_f32();
        let (measured, columns) = har.read_with(visual, |har, _| (har.list_width, har.columns()));
        assert!(
            (measured - list).abs() <= 2.5, // inside the 1 px border
            "{window}: measured {measured}, pane {list}"
        );
        assert!(
            list < window,
            "control: the pane is narrower than the window"
        );
        assert_eq!(
            columns.contains(&Column::Started),
            list >= STARTED_FROM,
            "{window}"
        );
        assert_eq!(
            columns.contains(&Column::Type),
            list >= TYPE_FROM,
            "{window}"
        );
        assert_eq!(
            measured_bounds(visual, "har-sort-Started").is_some(),
            list >= STARTED_FROM
        );
    }
    assert!(
        measured_bounds(visual, "har-sort-Type").is_some(),
        "700 px keeps Type"
    );
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

/// Review of PR 2878 (P2): an equal-length replacement must not show new
/// details under an old row; the list is read again with a notice.
#[gpui::test]
fn review2878_a_replaced_file_is_read_again_before_details(cx: &mut TestAppContext) {
    let text = sample();
    let (reader, visual, temp) = open(cx, &[("capture.har", &text)], "capture.har");
    let view = har(&reader, visual);
    let url = |view: &Entity<HarView>, visual: &mut VisualTestContext, index| {
        view.read_with(visual, |har, _| {
            har.entries.get(index).unwrap().url.to_string()
        })
    };
    assert_eq!(url(&view, visual, 1), "https://api.example.test/v1/login");
    let replaced = text.replace("api.example.test", "zpi.example.test");
    assert_eq!(replaced.len(), text.len());
    let path = temp.path().join("vault/capture.har");
    // Syncthing keeps the source mtime: same size, same time, other bytes.
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, &replaced).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    view.update(visual, |har, cx| har.select(1, cx));
    draw(visual);
    view.read_with(visual, |har, _| {
        assert!(
            har.detail.is_none() && har.selected.is_none(),
            "no details from the new file under the old row"
        );
        assert!(har.notice.is_some());
    });
    assert_eq!(url(&view, visual, 1), "https://zpi.example.test/v1/login");
    assert!(measured_bounds(visual, "har-banner").is_some());
    // Selecting again reads the new revision.
    view.update(visual, |har, cx| har.select(1, cx));
    draw(visual);
    view.read_with(visual, |har, _| match &har.detail {
        Some(Detail::Ready(rows)) => assert!(rows.rows.iter().any(|row| matches!(
            row,
            Row::Field { name, value, .. } if name.as_ref() == "URL" && value.as_ref() == "https://zpi.example.test/v1/login"
        ))),
        _ => panic!("details of the new revision"),
    });
}

/// #2888: detail values can be selected and copied in full with Cmd/Ctrl+C
/// and the context menu; the row shows a bounded value, the copy is whole.
#[gpui::test]
fn fix2888_detail_values_select_and_copy_in_full(cx: &mut TestAppContext) {
    let long_url = format!("https://long.example.test/{}", "u".repeat(70_000));
    let long_header = "h".repeat(10_000);
    let text = capture(vec![entry(
        "GET",
        &long_url,
        200,
        "2026-11-13T08:00:00.000Z",
        serde_json::json!({ "request": {
            "method": "GET", "url": long_url,
            "headers": [
                { "name": "authorization", "value": format!("Bearer {SECRET}") },
                { "name": "x-long", "value": long_header }
            ]
        }}),
    )]);
    let (reader, visual, _temp) = open(cx, &[("long.har", &text)], "long.har");
    visual.simulate_resize(size(px(1440.), px(900.)));
    let har = har(&reader, visual);
    har.update(visual, |har, cx| har.select(0, cx));
    draw(visual);
    let rows = har.read_with(visual, |har, _| match &har.detail {
        Some(Detail::Ready(rows)) => rows.rows.clone(),
        _ => panic!("details are ready"),
    });
    let row_of = |wanted: &str| {
        rows.iter()
            .position(|row| matches!(row, Row::Field { name, .. } if name.as_ref() == wanted))
            .unwrap()
    };
    let (url, header) = (row_of("URL"), row_of("x-long"));
    let clipboard = |visual: &mut VisualTestContext| {
        visual.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
    };
    let sentinel = |visual: &mut VisualTestContext| {
        visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())))
    };
    // The row shows a bounded value (control: the copy is longer than the row).
    assert!(matches!(&rows[url], Row::Field { value, .. } if value.len() < long_url.len()));

    // Click selects the value; Cmd/Ctrl+C copies all of it, read from the file.
    sentinel(visual);
    let bounds = measured_bounds(
        visual,
        Box::leak(format!("har-detail-row-{url}").into_boxed_str()),
    )
    .expect("the URL row is rendered");
    visual.simulate_click(bounds.center(), Modifiers::default());
    draw(visual);
    assert_eq!(har.read_with(visual, |har, _| har.detail_row), Some(url));
    visual.simulate_keystrokes("secondary-c");
    draw(visual);
    assert_eq!(clipboard(visual).as_deref(), Some(long_url.as_str()));

    // A long header copies whole too.
    sentinel(visual);
    har.update(visual, |har, cx| {
        har.detail_row = Some(header);
        cx.notify();
    });
    visual.simulate_keystrokes("secondary-c");
    draw(visual);
    assert_eq!(clipboard(visual).as_deref(), Some(long_header.as_str()));

    // The context menu offers name, name: value and the whole section.
    sentinel(visual);
    visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
    draw(visual);
    assert_eq!(har.read_with(visual, |har, _| har.detail_row), Some(url));
    let items: Vec<String> = har.read_with(visual, |har, _| match &har.detail {
        Some(Detail::Ready(rows)) => copy_items(rows, url)
            .into_iter()
            .map(|(l, _)| l.to_string())
            .collect(),
        _ => Vec::new(),
    });
    assert_eq!(
        items,
        [
            "Copy value",
            "Copy name",
            "Copy name: value",
            "Copy all general"
        ]
    );
    visual.simulate_keystrokes("escape");
    draw(visual);
    for (index, kind, expected) in [
        (url, CopyKind::Name, "URL".to_owned()),
        (url, CopyKind::Pair, format!("URL: {long_url}")),
        (
            header,
            CopyKind::Section,
            format!("authorization: Bearer {SECRET}\nx-long: {long_header}"),
        ),
    ] {
        sentinel(visual);
        har.update_in(visual, |har, window, cx| {
            har.copy_detail(index, kind, window, cx)
        });
        draw(visual);
        assert_eq!(clipboard(visual), Some(expected), "{kind:?}");
    }
}

// --- Review of PR 2900 (#2888): each case fails on head 7ebc1903. ---

struct CopyFixture {
    long_url: String,
    long_redirect: String,
    long_name: String,
}

fn copy_capture() -> (String, CopyFixture) {
    let fixture = CopyFixture {
        long_url: format!("https://long.example.test/{}", "u".repeat(70_000)),
        long_redirect: format!("https://redirect.example.test/{}", "r".repeat(70_000)),
        long_name: format!("x-{}", "n".repeat(300)),
    };
    let text = capture(vec![
        entry(
            "GET",
            &fixture.long_url,
            301,
            "2026-11-13T08:00:00.000Z",
            serde_json::json!({
                "request": {
                    "method": "GET", "url": fixture.long_url,
                    "headers": [
                        { "name": fixture.long_name, "value": "named" },
                        { "name": "accept", "value": "*/*" }
                    ]
                },
                "response": {
                    "status": 301, "statusText": "Moved", "headers": [],
                    "content": { "size": 0, "mimeType": "text/html" },
                    "redirectURL": fixture.long_redirect
                }
            }),
        ),
        entry(
            "GET",
            "https://short.example.test/",
            200,
            "2026-11-13T08:00:01.000Z",
            serde_json::json!({}),
        ),
    ]);
    (text, fixture)
}

fn ready_rows(har: &Entity<HarView>, visual: &mut VisualTestContext) -> Vec<Row> {
    har.read_with(visual, |har, _| match &har.detail {
        Some(Detail::Ready(rows)) => rows.rows.clone(),
        _ => panic!("details are ready"),
    })
}

fn row_named(rows: &[Row], wanted: &str) -> usize {
    rows.iter()
        .position(|row| matches!(row, Row::Field { name, .. } if name.as_ref() == wanted))
        .unwrap_or_else(|| panic!("no row {wanted}"))
}

fn clipboard_text(visual: &mut VisualTestContext) -> Option<String> {
    visual.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

fn set_sentinel(visual: &mut VisualTestContext) {
    visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
}

/// P2-1: a replacement with the same size and mtime (Syncthing) must not
/// copy new bytes under stale values; the request list is read again.
#[gpui::test]
fn review2900_long_copies_verify_the_request_bytes(cx: &mut TestAppContext) {
    let (text, fixture) = copy_capture();
    let (reader, visual, temp) = open(cx, &[("copy.har", &text)], "copy.har");
    let har = har(&reader, visual);
    har.update(visual, |har, cx| har.select(0, cx));
    draw(visual);
    let url = row_named(&ready_rows(&har, visual), "URL");
    // Control: unchanged, the whole URL copies.
    set_sentinel(visual);
    har.update_in(visual, |har, window, cx| {
        har.copy_detail(url, CopyKind::Value, window, cx)
    });
    draw(visual);
    assert_eq!(clipboard_text(visual), Some(fixture.long_url.clone()));
    let path = temp.path().join("vault/copy.har");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let replaced = text.replacen("uuuu", "vvvv", 1);
    assert_eq!(replaced.len(), text.len());
    std::fs::write(&path, &replaced).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    set_sentinel(visual);
    har.update_in(visual, |har, window, cx| {
        har.copy_detail(url, CopyKind::Value, window, cx)
    });
    draw(visual);
    assert_eq!(
        clipboard_text(visual).as_deref(),
        Some("sentinel"),
        "no copy of other bytes"
    );
    har.read_with(visual, |har, _| {
        assert!(har.notice.is_some() && har.detail.is_none())
    });
}

/// P2-2: an older copy never overwrites a newer copy, another request or a
/// closed pane.
#[gpui::test]
fn review2900_stale_copies_never_reach_the_clipboard(cx: &mut TestAppContext) {
    let (text, fixture) = copy_capture();
    let (reader, visual, _temp) = open(cx, &[("copy.har", &text)], "copy.har");
    let har = har(&reader, visual);
    har.update(visual, |har, cx| har.select(0, cx));
    draw(visual);
    let rows = ready_rows(&har, visual);
    let (url, method) = (row_named(&rows, "URL"), row_named(&rows, "Method"));
    // A long read, then a short copy before the read finishes.
    set_sentinel(visual);
    har.update_in(visual, |har, window, cx| {
        har.copy_detail(url, CopyKind::Value, window, cx);
        har.copy_detail(method, CopyKind::Value, window, cx);
    });
    draw(visual);
    assert_eq!(clipboard_text(visual).as_deref(), Some("GET"));
    // A long read, then closing details.
    set_sentinel(visual);
    har.update_in(visual, |har, window, cx| {
        har.copy_detail(url, CopyKind::Value, window, cx);
        har.close_details(cx);
    });
    draw(visual);
    assert_eq!(clipboard_text(visual).as_deref(), Some("sentinel"));
    // A long read, then another request.
    har.update(visual, |har, cx| har.select(0, cx));
    draw(visual);
    set_sentinel(visual);
    har.update_in(visual, |har, window, cx| {
        har.copy_detail(url, CopyKind::Value, window, cx);
        har.select(1, cx);
    });
    draw(visual);
    assert_eq!(clipboard_text(visual).as_deref(), Some("sentinel"));
    // Control: left alone, the long read does copy.
    har.update(visual, |har, cx| har.select(0, cx));
    draw(visual);
    har.update_in(visual, |har, window, cx| {
        har.copy_detail(url, CopyKind::Value, window, cx)
    });
    draw(visual);
    assert_eq!(clipboard_text(visual), Some(fixture.long_url));
}

/// P2-3 and P2-4: General fields above 64 KiB and names above the 200-char
/// display copy whole, in every copy form.
#[gpui::test]
fn review2900_long_fields_and_names_copy_whole(cx: &mut TestAppContext) {
    let (text, fixture) = copy_capture();
    let (reader, visual, _temp) = open(cx, &[("copy.har", &text)], "copy.har");
    let har = har(&reader, visual);
    har.update(visual, |har, cx| har.select(0, cx));
    draw(visual);
    let rows = ready_rows(&har, visual);
    let redirect = row_named(&rows, "Redirect");
    let name_row = rows
        .iter()
        .position(|row| matches!(row, Row::Field { value, .. } if value.as_ref() == "named"))
        .unwrap();
    for (index, kind, expected) in [
        (redirect, CopyKind::Value, fixture.long_redirect.clone()),
        (name_row, CopyKind::Name, fixture.long_name.clone()),
        (
            name_row,
            CopyKind::Pair,
            format!("{}: named", fixture.long_name),
        ),
        (
            name_row,
            CopyKind::Section,
            format!("{}: named\naccept: */*", fixture.long_name),
        ),
    ] {
        set_sentinel(visual);
        har.update_in(visual, |har, window, cx| {
            har.copy_detail(index, kind, window, cx)
        });
        draw(visual);
        let copied = clipboard_text(visual).unwrap();
        assert!(!copied.contains('…'), "{kind:?}: no display ellipsis");
        assert_eq!(copied, expected, "{kind:?}");
    }
}

// --- #2836 part 2: filters, Copy as cURL / URL / response, Save as, images. ---

fn png_bytes() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(3, 2, image::Rgba([0, 128, 255, 255]))
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn part2_capture() -> String {
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD.encode(png_bytes());
    let mut entries = serde_json::from_str::<serde_json::Value>(&sample()).unwrap()["log"]
        ["entries"]
        .as_array()
        .unwrap()
        .clone();
    entries.push(entry(
        "GET",
        "https://cdn.example.test/pixel.png",
        200,
        "2026-11-13T08:00:00.400Z",
        serde_json::json!({ "response": {
            "status": 200, "statusText": "OK", "headers": [],
            "content": { "size": 70, "mimeType": "image/png", "encoding": "base64", "text": png }
        }}),
    ));
    capture(entries)
}

fn invoke(reader: &Entity<Reader>, visual: &mut VisualTestContext, id: &str) {
    reader.update_in(visual, |reader, window, cx| {
        reader.invoke_viewer_action(id, window, cx)
    });
    draw(visual);
}

#[gpui::test]
fn part2_header_filters_narrow_the_list_and_show_their_choice(cx: &mut TestAppContext) {
    let text = part2_capture();
    let (reader, visual, _temp) = open(cx, &[("p2.har", &text)], "p2.har");
    let har = har(&reader, visual);
    let labels = |reader: &Entity<Reader>, visual: &mut VisualTestContext| {
        reader.read_with(visual, |reader, cx| {
            reader
                .viewer_chrome(cx)
                .actions
                .iter()
                .map(|a| a.label.to_string())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(labels(&reader, visual), ["Status", "Method", "Type"]);
    har.update(visual, |har, cx| {
        har.set_filters(|f| f.status = Some(StatusClass::ClientError), cx)
    });
    draw(visual);
    assert_eq!(rows(&har, visual), [1]);
    assert_eq!(labels(&reader, visual)[0], "Status: 4xx");
    har.update(visual, |har, cx| {
        har.set_filters(
            |f| {
                f.status = None;
                f.kind = Some(har::Kind::Image);
            },
            cx,
        )
    });
    draw(visual);
    assert_eq!(rows(&har, visual), [3]);
    har.update(visual, |har, cx| {
        har.set_filters(|f| f.method = Some("POST".into()), cx)
    });
    draw(visual);
    assert_eq!(rows(&har, visual), Vec::<usize>::new());
    // The Method menu lists the capture's methods.
    assert_eq!(
        har.read_with(visual, |har, _| har.methods.clone()),
        vec!["GET".into(), "POST".into()] as Vec<Box<str>>
    );
}

#[gpui::test]
fn part2_header_copy_menu_copies_url_curl_and_response(cx: &mut TestAppContext) {
    let text = part2_capture();
    let (reader, visual, _temp) = open(cx, &[("p2.har", &text)], "p2.har");
    let har = har(&reader, visual);
    // Disabled until a request is selected.
    let enabled = reader.read_with(visual, |reader, cx| {
        reader
            .viewer_chrome(cx)
            .copy_actions
            .iter()
            .any(|a| a.enabled)
    });
    assert!(!enabled);
    har.update(visual, |har, cx| har.select(1, cx));
    draw(visual);
    for (id, expected) in [
        ("har-copy-url", "https://api.example.test/v1/login".to_owned()),
        (
            "har-copy-curl-bash",
            "curl --url 'https://api.example.test/v1/login' \\\n  --data-raw '{\"user\":\"ann\",\"password\":\"hunter2\"}'".to_owned(),
        ),
        ("har-copy-response", "{\"ok\":true,\"n\":1}".to_owned()),
    ] {
        set_sentinel(visual);
        invoke(&reader, visual, id);
        assert_eq!(clipboard_text(visual), Some(expected), "{id}");
    }
    set_sentinel(visual);
    invoke(&reader, visual, "har-copy-powershell");
    let powershell = clipboard_text(visual).unwrap();
    assert!(powershell.starts_with("Invoke-WebRequest -UseBasicParsing -Uri 'https://api.example.test/v1/login' -Method 'POST'"));
    // No cmd.exe variant anywhere: header Copy menu and the row menu.
    let ids: Vec<&str> = reader.read_with(visual, |reader, cx| {
        reader
            .viewer_chrome(cx)
            .copy_actions
            .iter()
            .map(|a| a.id)
            .collect()
    });
    assert!(!ids.iter().any(|id| id.contains("cmd")), "{ids:?}");
    assert!(RequestCopy::all()
        .iter()
        .all(|what| !what.label().contains("cmd")));
    // A binary response is not pasted as text.
    har.update(visual, |har, cx| har.select(3, cx));
    draw(visual);
    set_sentinel(visual);
    invoke(&reader, visual, "har-copy-response");
    assert_eq!(clipboard_text(visual).as_deref(), Some("sentinel"));
}

#[gpui::test]
fn part2_save_as_writes_the_whole_decoded_body(cx: &mut TestAppContext) {
    let text = part2_capture();
    let (reader, visual, temp) = open(cx, &[("p2.har", &text)], "p2.har");
    let har = har(&reader, visual);
    har.update(visual, |har, cx| har.select(3, cx));
    draw(visual);
    invoke(&reader, visual, "har-save-response");
    assert!(visual.did_prompt_for_new_path(), "asks where to save");
    let target = temp.path().join("pixel.png");
    visual.simulate_new_path_selection({
        let target = target.clone();
        move |_| Some(target)
    });
    draw(visual);
    assert_eq!(std::fs::read(&target).unwrap(), png_bytes());
    // Cancelling the dialog writes nothing.
    invoke(&reader, visual, "har-save-response");
    visual.simulate_new_path_selection(|_| None);
    draw(visual);
    assert_eq!(std::fs::read(&target).unwrap(), png_bytes());
    let leftovers: Vec<_> = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temporary file is left: {leftovers:?}"
    );
}

#[gpui::test]
fn part2_small_raster_bodies_are_previewed(cx: &mut TestAppContext) {
    let text = part2_capture();
    let (reader, visual, _temp) = open(cx, &[("p2.har", &text)], "p2.har");
    let har = har(&reader, visual);
    har.update(visual, |har, cx| har.select(3, cx));
    draw(visual);
    let rows = ready_rows(&har, visual);
    assert!(
        rows.iter()
            .any(|row| matches!(row, Row::Image(p) if (p.width, p.height) == (3, 2))),
        "{rows:?}"
    );
    // Control: a text body has no preview.
    har.update(visual, |har, cx| har.select(0, cx));
    draw(visual);
    assert!(!ready_rows(&har, visual)
        .iter()
        .any(|row| matches!(row, Row::Image(_))));
}

#[gpui::test]
fn part2_right_click_selects_the_request_for_its_menu(cx: &mut TestAppContext) {
    let text = part2_capture();
    let (reader, visual, _temp) = open(cx, &[("p2.har", &text)], "p2.har");
    visual.simulate_resize(size(px(1440.), px(900.)));
    draw(visual);
    let har = har(&reader, visual);
    let row = measured_bounds(visual, "har-row-2").unwrap();
    visual.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(row.center(), MouseButton::Right, Modifiers::default());
    draw(visual);
    assert_eq!(har.read_with(visual, |har, _| har.selected), Some(2));
    visual.simulate_keystrokes("escape");
}

/// Review of PR 2904 (P2-4): Save as never replaces the capture itself,
/// also through a symbolic link; the suggested name avoids it.
#[gpui::test]
fn review2904_save_as_refuses_the_capture_itself(cx: &mut TestAppContext) {
    let text = part2_capture();
    let (reader, visual, temp) = open(cx, &[("p2.har", &text)], "p2.har");
    let har = har(&reader, visual);
    har.update(visual, |har, cx| har.select(3, cx));
    draw(visual);
    let capture_path = temp.path().join("vault/p2.har");
    let mut targets = vec![capture_path.clone()];
    #[cfg(unix)]
    {
        let link = temp.path().join("link.har");
        std::os::unix::fs::symlink(&capture_path, &link).unwrap();
        targets.push(link);
    }
    for target in targets {
        invoke(&reader, visual, "har-save-response");
        visual.simulate_new_path_selection(move |_| Some(target));
        draw(visual);
        assert_eq!(
            std::fs::read_to_string(&capture_path).unwrap(),
            text,
            "the capture is unchanged"
        );
    }
    // Control: another name is written.
    let other = temp.path().join("other.png");
    invoke(&reader, visual, "har-save-response");
    visual.simulate_new_path_selection({
        let other = other.clone();
        move |_| Some(other)
    });
    draw(visual);
    assert_eq!(std::fs::read(&other).unwrap(), png_bytes());
    assert!(same_file(&capture_path, &capture_path) && !same_file(&capture_path, &other));
}

/// #2932: the right panel sums up the capture instead of an empty Contents.
/// Its rows drive the list's Status filter, Find and the selection; in
/// Source the Overview stays, and a row brings the request list back.
#[gpui::test]
fn issue2932_right_panel_shows_the_capture_overview(cx: &mut TestAppContext) {
    let text = sample();
    let (reader, visual, _temp) = open(cx, &[("capture.har", &text)], "capture.har");
    visual.simulate_resize(size(px(1440.), px(900.)));
    reader.update(visual, |reader, cx| {
        reader.panels.open(reader_layout::Panel::Backlinks);
        cx.notify();
    });
    draw(visual);
    let har = har(&reader, visual);
    assert!(
        measured_bounds(visual, "har-overview-heading").is_some(),
        "the Overview heading is drawn"
    );
    assert!(
        measured_bounds(visual, "file-contents-heading").is_none(),
        "no empty Contents next to the request list"
    );
    for id in [
        "har-overview-requests",
        "har-overview-transferred",
        "har-overview-errors",
        "har-overview-creator",
        "har-overview-status-2xx",
        "har-overview-status-4xx",
        "har-overview-status-Failed",
        "har-overview-host-0",
        "har-overview-slowest-0",
    ] {
        assert!(measured_bounds(visual, id).is_some(), "{id} is drawn");
    }
    assert!(measured_bounds(visual, "har-overview-status-5xx").is_none());

    // A status row toggles the Status filter.
    let click = |visual: &mut VisualTestContext, id: &'static str| {
        let bounds = measured_bounds(visual, id).unwrap_or_else(|| panic!("{id} is drawn"));
        visual.simulate_click(bounds.center(), Modifiers::default());
        draw(visual);
    };
    click(visual, "har-overview-status-4xx");
    assert_eq!(rows(&har, visual), [1]);
    assert_eq!(
        har.read_with(visual, |har, _| har.filters.status),
        Some(StatusClass::ClientError)
    );
    click(visual, "har-overview-status-4xx");
    assert_eq!(har.read_with(visual, |har, _| har.filters.status), None);
    assert_eq!(rows(&har, visual), [0, 1, 2]);

    // Hosts with equal counts and sizes are listed by name: api first. Its
    // row puts the host into Find, and a second click takes it out.
    let host = har.read_with(visual, |har, _| har.overview.top_hosts[0].0.to_string());
    assert_eq!(host, "api.example.test");
    click(visual, "har-overview-host-0");
    assert_eq!(
        har.read_with(visual, |har, cx| har.filter.read(cx).value().to_string()),
        host
    );
    assert_eq!(rows(&har, visual), [1]);
    click(visual, "har-overview-host-0");
    assert_eq!(
        har.read_with(visual, |har, cx| har.filter.read(cx).value().to_string()),
        ""
    );
    assert_eq!(rows(&har, visual), [0, 1, 2]);

    // A request row selects that request and opens its details.
    click(visual, "har-overview-slowest-2");
    assert_eq!(har.read_with(visual, |har, _| har.selected), Some(2));

    // Source keeps the Overview; a row there goes back to the requests.
    click(visual, "har-view-source");
    reader.read_with(visual, |reader, _| {
        assert!(reader.file_preview.as_ref().unwrap().code.is_some())
    });
    assert!(measured_bounds(visual, "har-overview-heading").is_some());
    assert!(measured_bounds(visual, "file-contents-heading").is_none());
    click(visual, "har-overview-status-4xx");
    reader.read_with(visual, |reader, _| {
        assert!(reader.file_preview.as_ref().unwrap().code.is_none())
    });
    assert_eq!(rows(&har, visual), [1]);
    assert!(measured_bounds(visual, "har-list").is_some());
}
