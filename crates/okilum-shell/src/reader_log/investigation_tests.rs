// #2398 acceptance over actual cell/detail actions and source-aware saved views.
fn investigation_rows() -> String {
    (0..120)
        .map(|i| {
            format!(
                "{}\n",
                serde_json::json!({
                    "time": format!("2026-11-02T00:{:02}:{:02}Z", (119-i)/60, (119-i)%60),
                    "level": "info", "flow": format!("request-{}", i%3),
                    "phase": if i%2 == 0 { "parent" } else { "other" },
                    "details": format!("Event {i}\n{}", "Full wrapped message context. ".repeat(40))
                })
            )
        })
        .collect()
}
#[gpui::test]
fn investigation_cell_and_details_follow_back_restores_full_view_and_shortcut(
    cx: &mut TestAppContext,
) {
    let _shutdown = TestShutdown(cx.clone());
    let settings = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(settings.path(), cx));
    let (_dir, reader, visual) = open_log(&investigation_rows(), cx);
    visual.simulate_resize(gpui::size(px(1600.), px(1000.)));
    let view = log_view(&reader, visual);
    view.update_in(visual, |view, window, cx| {
        view.choose_field(Some("flow".into()), false, cx);
        view.choose_field(Some("details".into()), true, cx);
        view.toggle_column("flow", cx);
        view.add_chip(
            FilterChip::new(
                FieldRef::Key("phase".into()),
                vec![FilterValue::string("parent")],
                FilterMode::Include,
            ),
            window,
            cx,
        );
        view.update_filter(|filter| filter.time = TimeWindow::LastHour, cx);
    });
    filter_frame(visual);
    assert_eq!(
        shown(&view, visual).len(),
        60,
        "positive control: the parent chip filters loaded rows"
    );
    view.update(visual, |view, _| {
        view.selected = Some(6);
        view.anchor = Some(4);
        let mut scroll = view.scroll.0.borrow_mut();
        scroll.deferred_scroll_to_item = None;
        scroll.base_handle.set_offset(point(px(0.), px(-48.)));
    });
    filter_frame(visual);
    let (offset, query) = view.read_with(visual, |view, _| {
        (
            view.scroll.0.borrow().base_handle.offset(),
            view.query_text.clone(),
        )
    });
    assert!(
        offset.y < px(0.),
        "positive control: the parent was scrolled"
    );
    let cell = visual
        .debug_bounds("log-field-cell-6-0")
        .expect("selected correlation cell is drawn")
        .center();
    visual.simulate_mouse_down(cell, MouseButton::Right, Modifiers::default());
    filter_frame(visual);
    visual.simulate_mouse_up(cell, MouseButton::Right, Modifiers::default());
    filter_frame(visual);
    let menu = view.read_with(visual, |view, _| view.cell_menu_handle().unwrap());
    visual.simulate_keystrokes("down down down down enter");
    filter_frame(visual);
    assert!(menu.upgrade().is_none(), "Follow dismisses the owned menu");
    assert_eq!(
        shown(&view, visual),
        (0..120).rev().filter(|i| i % 3 == 0).collect::<Vec<_>>(),
        "request events are in time order, including rows excluded by parent chips"
    );
    view.read_with(visual, |view, _| {
        assert_eq!(
            view.investigation.request.as_ref().unwrap().value,
            "request-0"
        );
        assert!(view
            .investigation_label()
            .unwrap()
            .starts_with("Loaded rows: 40 events for id request-0"));
    });
    assert!(visual.debug_bounds("log-breadcrumb").is_some());
    view.update(visual, |view, cx| {
        view.toggle_column("phase", cx);
        view.choose_field(Some("flow".into()), true, cx);
    });
    filter_click(visual, "log-request-back");
    view.read_with(visual, |view, cx| {
        assert_eq!(view.columns(cx), ["flow"]);
        assert_eq!(view.selected, Some(6));
        assert_eq!(view.anchor, Some(4));
        assert_eq!(view.query_text, query);
        assert_eq!(view.filter.time, TimeWindow::LastHour);
        assert_eq!(view.field_choices(cx).message.as_deref(), Some("details"));
        assert_eq!(view.scroll.0.borrow().base_handle.offset(), offset);
        assert_eq!(view.filter_chips().len(), 1);
    });
    assert_eq!(shown(&view, visual).len(), 60);
    // Details use the same request action, and the native Back shortcut restores the frame.
    filter_click(visual, "log-show-request");
    assert_eq!(
        shown(&view, visual).len(),
        40,
        "positive control: details performed a drill-down"
    );
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-[");
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("alt-left");
    filter_frame(visual);
    assert_eq!(shown(&view, visual).len(), 60);
    view.read_with(visual, |view, _| {
        assert!(view.investigation.frames.is_empty());
        assert_eq!(view.selected, Some(6));
        assert_eq!(view.scroll.0.borrow().base_handle.offset(), offset);
    });
}

#[gpui::test]
fn investigation_message_choice_persists_and_saved_view_restores_source_fields_chips_and_columns(
    cx: &mut TestAppContext,
) {
    let _shutdown = TestShutdown(cx.clone());
    let settings = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(settings.path(), cx));
    let (dir, reader, visual) = open_log(&investigation_rows(), cx);
    visual.simulate_resize(gpui::size(px(1600.), px(1000.)));
    let view = log_view(&reader, visual);
    view.update_in(visual, |view, window, cx| view.select(0, true, window, cx));
    filter_frame(visual);
    let detail = view.read_with(visual, |view, _| {
        view.data()
            .unwrap()
            .record(0)
            .unwrap()
            .fields
            .iter()
            .position(|field| field.key == "details")
            .unwrap()
    });
    let selector = format!("log-detail-field-actions-{detail}");
    let button = visual
        .debug_bounds(selector.leak())
        .expect("detail field has actions")
        .center();
    visual.simulate_mouse_down(button, MouseButton::Left, Modifiers::default());
    filter_frame(visual);
    visual.simulate_mouse_up(button, MouseButton::Left, Modifiers::default());
    filter_frame(visual);
    visual.simulate_keystrokes("down enter");
    filter_frame(visual);
    view.read_with(visual, |view, cx| {
        assert_eq!(view.message_field(cx).as_deref(), Some("details"))
    });
    assert!(
        visual.debug_bounds("log-full-message").is_some(),
        "chosen field has a wrapped full-text detail"
    );
    let (source, saved) = view.update_in(visual, |view, window, cx| {
        view.choose_field(Some("flow".into()), false, cx);
        view.toggle_column("flow", cx);
        view.add_chip(
            FilterChip::new(
                FieldRef::Key("phase".into()),
                vec![FilterValue::string("parent")],
                FilterMode::Include,
            ),
            window,
            cx,
        );
        view.begin_saving(window, cx);
        view.saving
            .as_ref()
            .unwrap()
            .input
            .update(cx, |input, cx| input.set_value("Investigation", window, cx));
        view.commit_saving(window, cx);
        let saved = reader_ui_state::saved_log_filters(cx)
            .get("Investigation")
            .unwrap()
            .clone();
        (view.history_key().unwrap(), saved)
    });
    filter_frame(visual);
    assert_eq!(saved.file.as_deref(), Some(source.as_str()));
    assert_eq!(saved.message_field.as_deref(), Some("details"));
    assert_eq!(saved.correlation_field.as_deref(), Some("flow"));
    visual.executor().advance_clock(Duration::from_secs(1));
    filter_frame(visual);
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(settings.path().join("reader-ui.json")).unwrap())
            .unwrap();
    assert_eq!(persisted["log_fields"][&source]["message"], "details");
    assert_eq!(persisted["log_fields"][&source]["correlation"], "flow");
    // Persistence survives constructing another LogView for the same canonical file.
    let file = Arc::new(LogFile::open(Path::new(&source)).unwrap());
    let replacement = visual.update(|window, cx| {
        cx.new(|cx| {
            LogView::ready(
                LogDocument {
                    rel: "app.jsonl".into(),
                    file,
                    elapsed: Duration::ZERO,
                },
                window,
                cx,
            )
        })
    });
    replacement.read_with(visual, |view, cx| {
        assert_eq!(view.message_field(cx).as_deref(), Some("details"))
    });
    drop(replacement);
    // The same saved view also reopens its source after the user inspects another file.
    let other = dir.path().join("other.jsonl");
    std::fs::write(&other, "{\"message\":\"different source\"}\n").unwrap();
    view.update_in(visual, |view, window, cx| {
        let file = Arc::new(LogFile::open(&other).unwrap());
        view.state = State::Ready(LogDocument {
            rel: "other.jsonl".into(),
            file,
            elapsed: Duration::ZERO,
        });
        view.clear_filters(window, cx);
        view.apply_saved("Investigation", window, cx);
    });
    filter_frame(visual);
    view.read_with(visual, |view, cx| {
        assert_eq!(view.history_key().as_deref(), Some(source.as_str()));
        assert_eq!(view.columns(cx), ["flow"]);
        assert_eq!(view.field_choices(cx).message.as_deref(), Some("details"));
        assert_eq!(view.field_choices(cx).correlation.as_deref(), Some("flow"));
        assert_eq!(view.filter_chips(), saved.filter_chips);
        assert_eq!(view.visible_len(), 60);
    });
}

#[gpui::test]
fn investigation_file_live_keeps_request_chips_message_and_columns(cx: &mut TestAppContext) {
    use std::io::Write as _;
    let _shutdown = TestShutdown(cx.clone());
    let settings = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(settings.path(), cx));
    let (dir, reader, visual) = open_log(&investigation_rows(), cx);
    let view = log_view(&reader, visual);
    view.update_in(visual, |view, window, cx| {
        view.choose_field(Some("flow".into()), false, cx);
        view.choose_field(Some("details".into()), true, cx);
        view.toggle_column("flow", cx);
        view.follow_request(
            correlation::RequestId {
                key: "flow".into(),
                value: "request-0".into(),
            },
            window,
            cx,
        );
        view.add_chip(
            FilterChip::new(
                FieldRef::Key("phase".into()),
                vec![FilterValue::string("parent")],
                FilterMode::Include,
            ),
            window,
            cx,
        );
        view.toggle_follow(cx);
    });
    filter_frame(visual);
    assert_eq!(
        shown(&view, visual).len(),
        20,
        "positive control: request AND chip apply to initial rows"
    );
    view.update_in(visual, |view, window, cx| {
        view.replace_query("phase=parent".into(), window, cx)
    });
    filter_frame(visual);
    assert_eq!(
        shown(&view, visual).len(),
        20,
        "editing the child query keeps the request scope"
    );
    view.update_in(visual, |view, window, cx| {
        view.add_chip(
            FilterChip::new(
                FieldRef::Key("phase".into()),
                vec![FilterValue::string("parent")],
                FilterMode::Include,
            ),
            window,
            cx,
        )
    });
    filter_frame(visual);
    let chips = view.read_with(visual, |view, _| view.filter_chips().to_vec());
    assert_eq!(
        chips.len(),
        1,
        "positive control: an active chip is retained during follow"
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("app.jsonl"))
        .unwrap();
    for (flow, phase, details) in [
        ("request-0", "parent", "shown new event"),
        ("request-1", "parent", "other request"),
        ("request-0", "other", "excluded by chip"),
    ] {
        writeln!(file,"{}",serde_json::json!({"time":"2026-11-02T00:03:00Z","flow":flow,"phase":phase,"details":details})).unwrap();
    }
    drop(file);
    visual.executor().advance_clock(Duration::from_millis(500));
    filter_frame(visual);
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.total(),
            123,
            "positive control: every appended line was read"
        );
        assert_eq!(
            view.visible_len(),
            21,
            "only the matching request and chip were admitted"
        );
        assert_eq!(view.filter_chips(), chips);
        assert_eq!(view.columns(cx), ["flow"]);
        assert_eq!(view.message_field(cx).as_deref(), Some("details"));
        assert!(view.investigation.last_update.is_some());
        let data = view.data().unwrap();
        let record = data.record(0).unwrap();
        let row = row_text_message(data, 0, Some(&record), Some("details"));
        assert!(row.text.starts_with("Event 0"));
        assert!(!row.text.contains('\n'));
        assert!(
            record
                .fields
                .iter()
                .find(|f| f.key == "details")
                .unwrap()
                .value
                .len()
                > row.text.len(),
            "full details survive row truncation"
        );
    });
    visual.executor().advance_clock(Duration::from_millis(500));
    filter_frame(visual);
    view.read_with(visual, |view, _| {
        assert_eq!(
            view.visible_len(),
            21,
            "unchanged after a positive append/poll control"
        )
    });
    view.update(visual, |view, cx| view.toggle_follow(cx));
}
