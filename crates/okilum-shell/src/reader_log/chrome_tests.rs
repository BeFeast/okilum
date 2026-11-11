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
    visual.simulate_resize(gpui::size(px(1440.), px(900.)));
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
        visual.simulate_resize(gpui::size(px(width), px(900.)));
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

fn toolbar2808_open_copy_menu(visual: &mut VisualTestContext) {
    filter_click(visual, "document-more");
    // At 1,600 px every log action is inline: Reveal, then the Copy submenu.
    visual.simulate_keystrokes("down down right");
    visual.run_until_parked();
    visual.update(|window, cx| assert!(!gpui_base::GlobalState::native_overlays(window, cx).is_empty(), "Copy popup mounted"));
}

#[gpui::test]
fn toolbar2808_document_copy_submenu_uses_live_selection_and_confirms_every_payload(cx: &mut TestAppContext) {
    use gpui_component::menu::PopupMenuItem;
    use gpui_component::WindowExt as _;
    let _shutdown = TestShutdown(cx.clone());
    let (_dir, reader, visual) = open_log(CHROME_LOG, cx);
    let view = log_view(&reader, visual);
    visual.simulate_resize(gpui::size(px(1600.), px(900.)));
    let raw = reader.read_with(visual, |reader, cx| reader.viewer_chrome(cx).copy_actions.remove(0));
    let PopupMenuItem::Item { disabled, label, .. } = raw.menu_item() else { panic!("raw Copy item"); };
    assert!(disabled && label.contains("Select a record first"), "disabled raw copy explains why");
    visual.update(|window, cx| { window.clear_notifications(cx); cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())); });
    toolbar2808_open_copy_menu(visual);
    visual.simulate_keystrokes("down enter"); // disabled raw is skipped
    visual.run_until_parked();
    visual.update(|window, cx| {
        assert_eq!(cx.read_from_clipboard().unwrap().text().as_deref(), Some(CHROME_LOG), "Copy file content is reachable while raw Copy is disabled");
        assert!(!window.notifications(cx).is_empty());
    });
    view.update_in(visual, |view, window, cx| view.select(1, true, window, cx));
    let (root, rel) = reader.read_with(visual, |reader, _| (reader.vault_root.clone(), reader.selected_file().to_owned()));
    let raw = CHROME_LOG.lines().nth(1).unwrap().to_owned();
    let wiki = format!("[[{rel}]]");
    for (index, expected) in [raw.as_str(), CHROME_LOG, root.join(&rel).to_str().unwrap(), rel.as_str(), wiki.as_str()].into_iter().enumerate() {
        visual.update(|window, cx| { window.clear_notifications(cx); cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())); });
        toolbar2808_open_copy_menu(visual);
        visual.simulate_keystrokes(&("down ".repeat(index + 1) + "enter"));
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert_eq!(cx.read_from_clipboard().unwrap().text().as_deref(), Some(expected), "actual Copy submenu command {index}");
            assert!(!window.notifications(cx).is_empty(), "Copy command {index} confirms with a toast");
        });
    }
    assert_eq!(std::fs::read_to_string(root.join(rel)).unwrap(), CHROME_LOG);
}

#[gpui::test]
fn toolbar2808_columns_menu_contains_the_live_message_field_chooser(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let settings = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(settings.path(), cx));
    let (_dir, reader, visual) = open_log(CHROME_LOG, cx);
    let view = log_view(&reader, visual);
    visual.simulate_resize(gpui::size(px(1600.), px(900.)));
    let (columns, choice) = view.read_with(visual, |view, _| (view.field_names().len(), view.string_fields().iter().position(|key| key == "request_id").unwrap()));
    filter_click(visual, "log-columns-menu");
    visual.simulate_keystrokes(&("down ".repeat(columns + 1) + "right"));
    visual.run_until_parked();
    visual.simulate_keystrokes(&("down ".repeat(choice + 2) + "enter"));
    visual.run_until_parked();
    view.read_with(visual, |view, cx| assert_eq!(view.field_choices(cx).message.as_deref(), Some("request_id"), "message chooser remains reachable inside Columns"));
    assert!(reader_viewer_chrome::tests::measured_bounds(visual, "log-query-row").is_none());
}

#[gpui::test]
fn toolbar2808_investigation_breadcrumb_back_and_count_share_the_demand_row(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let (_dir, reader, visual) = open_log(CHROME_LOG, cx);
    let view = log_view(&reader, visual);
    for width in [1100., 700.] {
        visual.simulate_resize(gpui::size(px(width), px(900.)));
        view.update_in(visual, |view, window, cx| {
            view.replace_query("level=error".into(), window, cx);
            view.follow_request(correlation::RequestId { key: "request_id".into(), value: "first".into() }, window, cx);
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [0, 2], "request temporarily replaces the parent filter");
        let bounds = reader_viewer_chrome::tests::measured_bounds;
        let row = bounds(visual, "log-query-row").unwrap();
        assert_eq!(row.size.height, px(40.), "investigation adds only the one secondary row");
        for id in ["log-request-back", "log-breadcrumb", "log-request-count"] {
            let control = bounds(visual, id).expect("required investigation control");
            assert!(row.contains(&control.center()), "{id} stays inside the single row at {width}");
        }
        filter_click(visual, "log-request-back");
        assert_eq!(shown(&view, visual), [1], "Back restores the exact parent filter");
        view.read_with(visual, |view, _| assert_eq!(view.query_text, "level=error"));
        view.update_in(visual, |view, window, cx| view.clear_filters(window, cx));
    }
}

#[gpui::test]
fn toolbar2808_standalone_host_uses_the_shared_slots_and_find_alias(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    cx.update(|cx| { crate::test_init(cx); crate::bind_keys(cx); });
    let (_dir, file) = log(CHROME_LOG);
    let file_path = file.path().to_owned();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| LogView::ready(LogDocument { rel: "standalone.log".into(), file: Arc::new(file), elapsed: Duration::ZERO }, window, cx));
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    visual.simulate_resize(gpui::size(px(1100.), px(900.)));
    let bounds = reader_viewer_chrome::tests::measured_bounds;
    let header = bounds(visual, "viewer-compact-header").unwrap();
    for id in ["viewer-find-field", "log-level", "log-time", "log-follow", "log-query-toggle", "log-more"] {
        assert!(header.contains(&bounds(visual, id).expect("shared standalone control").center()));
    }
    assert!(bounds(visual, "log-query-row").is_none());
    visual.update(|window, cx| { view.read(cx).focus.focus(window, cx); window.dispatch_action(Box::new(FocusLogFilter), cx); });
    visual.simulate_input("cache");
    visual.run_until_parked();
    assert_eq!(shown(&view, visual), [1], "the retained alias reaches the shared field");
    visual.simulate_keystrokes("enter");
    visual.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
    visual.update(|window, cx| window.dispatch_action(Box::new(FindInNote), cx));
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(shown(&view, visual), [0, 1, 2]);
    assert_eq!(std::fs::read_to_string(file_path).unwrap(), CHROME_LOG);
}

#[gpui::test]
fn toolbar2808_active_state_controls_are_filled_and_order_is_viewer_owned(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let (_dir, reader, visual) = open_log(CHROME_LOG, cx);
    let view = log_view(&reader, visual);
    view.update(visual, |view, cx| view.update_filter(|filter| { filter.level.min = Some(Level::Warn); filter.time = TimeWindow::LastHour; }, cx));
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        let chrome = reader.viewer_chrome(cx);
        for id in ["log-level", "log-time"] {
            let action = chrome.actions.iter().find(|action| action.id == id).unwrap();
            assert!(action.filled && action.toggled, "active {id} is a filled state control");
            assert!(action.caret && action.never_dropped);
        }
        let priority = |id| chrome.actions.iter().find(|action| action.id == id).unwrap().priority;
        assert!(priority("log-files") < priority("log-export"));
        assert!(priority("log-export") < priority("log-columns-menu"));
        assert!(priority("log-columns-menu") < priority("log-saved"));
        let collapse = |id| chrome.actions.iter().find(|action| action.id == id).unwrap().collapse_priority;
        assert!(priority("log-saved") < collapse("log-follow"));
        assert!(collapse("log-follow") < collapse("log-time") && collapse("log-time") < collapse("log-level"));
        assert!(collapse("log-level") < chrome.overflow_order.open_with);
    });
}
