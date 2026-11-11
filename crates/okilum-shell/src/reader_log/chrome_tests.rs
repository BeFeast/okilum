// Reader-hosted and standalone acceptance for Variant A log chrome (#2808).
const CHROME_LOG: &str = concat!(
    "{\"ts\":\"2026-11-10T10:00:00Z\",\"level\":\"info\",\"msg\":\"boot\",\"request_id\":\"first\"}\n",
    "{\"ts\":\"2026-11-10T10:01:00Z\",\"level\":\"error\",\"msg\":\"cache miss\",\"request_id\":\"second\"}\n",
    "{\"ts\":\"2026-11-10T10:02:00Z\",\"level\":\"warn\",\"msg\":\"slow\",\"request_id\":\"first\"}\n",
);

#[gpui::test]
fn toolbar2808_idle_log_has_one_header_and_no_secondary_rows(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let (_dir, reader, visual) = open_log(CHROME_LOG, cx);
    let view = log_view(&reader, visual);
    visual.simulate_resize(size(px(1440.), px(900.)));
    let bounds = reader_viewer_chrome::tests::measured_bounds;
    assert!(bounds(visual, "viewer-find-field").is_some(), "header Find is mounted");
    assert!(bounds(visual, "log-query-toggle").is_some(), "Query is reachable in the header");
    assert!(bounds(visual, "viewer-copy").is_none(), "logs put Copy in Document actions");
    assert!(bounds(visual, "viewer-compact-header").is_none(), "Reader owns the only log header");
    assert!(bounds(visual, "log-query-row").is_none(), "idle log has no second row");
    assert!(bounds(visual, "log-filter-bar").is_none());
    for id in ["log-level", "log-time", "log-follow"] {
        let control = bounds(visual, id).expect("pinned log state control");
        let header = bounds(visual, "document-header-viewport").unwrap();
        assert!(header.contains(&control.center()), "{id} belongs to the document header");
        assert!(control.size.height >= px(28.));
    }
    let source = bounds(visual, "log-view-source").unwrap();
    let status = bounds(visual, "log-status-line").unwrap();
    assert!(status.contains(&source.center()), "source path belongs to the bottom line");
    view.read_with(visual, |view, _| assert!(view.hosted_header));
    for width in [1100., 700.] {
        visual.simulate_resize(size(px(width), px(900.)));
        for id in ["log-level", "log-time", "log-follow", "log-query-toggle", "document-more"] {
            assert!(bounds(visual, id).is_some(), "{id} is pinned at {width}");
        }
        if width == 700. {
            for id in ["log-files", "log-export", "log-columns-menu", "log-saved"] {
                assert!(bounds(visual, id).is_none(), "{id} has moved into overflow at 700 px");
            }
        }
    }
}

#[gpui::test]
fn toolbar2808_header_find_typing_keeps_slots_and_side_panels_cached(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let _cached = reader_side_panel::tests::CachedPanels::on();
    let (_dir, reader, visual) = open_log(CHROME_LOG, cx);
    let view = log_view(&reader, visual);
    reader.update_in(visual, |reader, window, cx| reader.open_find(window, cx));
    visual.run_until_parked();
    visual.simulate_keystrokes("home");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let initial = reader_viewer_chrome::header_renders(reader.entity_id());
    assert!(initial > 0, "positive control: header rendered");
    struct Observer { _subscriptions: Vec<Subscription> }
    let notifications = Rc::new(std::cell::Cell::new([0; 2]));
    let panels = reader.read_with(visual, |reader, _| reader.side_panels.clone());
    let _observer = visual.update(|_, cx| cx.new(|cx: &mut Context<Observer>| Observer {
        _subscriptions: panels.into_iter().enumerate().map(|(i, panel)| {
            let notifications = notifications.clone();
            cx.observe(&panel.unwrap(), move |_, _, _| { let mut n = notifications.get(); n[i] += 1; notifications.set(n); })
        }).collect(),
    }));
    for ch in "cache".chars() {
        visual.simulate_input(&ch.to_string());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(reader_viewer_chrome::header_renders(reader.entity_id()), initial, "Find typing renders zero header slots");
        assert_eq!(notifications.get(), [0, 0], "Find never notifies SidePanels");
    }
    view.read_with(visual, |view, _| { assert_eq!(view.filter.text, "cache"); assert_eq!(view.visible_len(), 1); });
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    visual.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
    let counter = reader_viewer_chrome::tests::measured_bounds(visual, "log-match-counter").expect("counter moved to status");
    let status = reader_viewer_chrome::tests::measured_bounds(visual, "log-status-line").unwrap();
    assert!(status.contains(&counter.center()));
    reader.update_in(visual, |reader, window, cx| reader.open_find(window, cx));
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    view.read_with(visual, |view, cx| { assert!(view.filter.text.is_empty()); assert!(view.text_input.read(cx).value().is_empty()); assert_eq!(view.visible_len(), 3); });
    visual.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
}

#[gpui::test]
fn toolbar2808_query_and_regex_share_one_demand_row_and_one_clear(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let (_dir, reader, visual) = open_log(CHROME_LOG, cx);
    let view = log_view(&reader, visual);
    let bounds = reader_viewer_chrome::tests::measured_bounds;
    assert!(bounds(visual, "log-query-row").is_none());
    filter_click(visual, "log-query-toggle");
    let row = bounds(visual, "log-query-row").unwrap();
    for id in ["log-query", "log-regex", "log-chips-clear"] {
        assert!(row.contains(&bounds(visual, id).expect("required query-row control").center()));
    }
    view.update_in(visual, |view, window, cx| view.replace_query("level>=warn".into(), window, cx));
    filter_click(visual, "log-query-toggle"); // the query keeps the row open
    assert!(bounds(visual, "log-query-row").is_some());
    filter_click(visual, "log-chips-clear");
    assert!(bounds(visual, "log-query-row").is_none(), "Clear all removes the empty demand row");
    view.update_in(visual, |view, window, cx| view.begin_regex(window, cx));
    view.update_in(visual, |view, window, cx| {
        view.regex_editor.as_ref().unwrap().pattern.update(cx, |input, cx| {
            input.set_value("[", window, cx); cx.emit(InputEvent::Change);
        });
    });
    visual.run_until_parked();
    let row = bounds(visual, "log-query-row").unwrap();
    for id in ["log-regex-name", "log-regex-pattern", "log-regex-error", "log-chips-clear"] {
        assert!(row.contains(&bounds(visual, id).expect("regex editor stays inline").center()), "{id}");
    }
    assert!(bounds(visual, "log-query").is_none(), "the regex editor occupies the same row");
    filter_click(visual, "log-chips-clear");
    assert!(bounds(visual, "log-query-row").is_none());
}

#[gpui::test]
fn toolbar2808_same_file_reload_restores_header_text_filter(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let (_dir, reader, visual) = open_log(CHROME_LOG, cx);
    let view = log_view(&reader, visual);
    view.update_in(visual, |view, window, cx| {
        view.text_input.update(cx, |input, cx| { input.set_value("cache", window, cx); cx.emit(InputEvent::Change); });
    });
    visual.run_until_parked();
    assert_eq!(shown(&view, visual), [1]);
    let rel = reader.read_with(visual, |reader, _| reader.selected_file().to_owned());
    reader.update_in(visual, |reader, window, cx| reader.preview_file(&rel, window, cx));
    visual.run_until_parked();
    let restored = log_view(&reader, visual);
    assert_ne!(view.entity_id(), restored.entity_id(), "positive control: a fresh log entity");
    restored.read_with(visual, |view, cx| { assert_eq!(view.filter.text, "cache"); assert_eq!(view.text_input.read(cx).value().as_ref(), "cache"); assert!(view.hosted_header); });
    assert_eq!(shown(&restored, visual), [1]);
}
