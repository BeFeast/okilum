// Acceptance for generic point-and-click filters (#2389).
fn filter_frame(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    for _ in 0..2 {
        visual.update(|window, cx| {
            // Cached header paint does not replay GPUI's test-only debug bounds.
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
    }
}
fn filter_click(visual: &mut VisualTestContext, selector: &'static str) {
    filter_frame(visual);
    let position = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"))
        .center();
    visual.simulate_mouse_move(position, None, Modifiers::default());
    filter_frame(visual);
    visual.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    filter_frame(visual);
    visual.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    filter_frame(visual);
}
const CHIP_ROWS: &str = concat!(
    "{\"msg\":\"first\",\"sample\":\"200\"}\n",
    "{\"msg\":\"second\",\"sample\":200}\n",
    "{\"msg\":\"third\",\"sample\":false}\n",
    "{\"msg\":\"fourth\",\"sample\":null}\n",
    "{\"msg\":\"fifth\",\"sample\":\"\"}\n",
    "{\"msg\":\"sixth\"}\n",
);
#[gpui::test]
fn point_filters_cell_include_exclude_and_copy_use_real_events(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(state.path(), cx));
    let (_dir, reader, visual) = open_log(CHIP_ROWS, cx);
    let view = log_view(&reader, visual);
    view.update(visual, |view, cx| view.toggle_column("sample", cx));
    filter_frame(visual);
    for (keys, expected) in [
        ("down enter", vec![0]),
        ("down down enter", vec![1, 2, 3, 4, 5]),
    ] {
        let cell = visual
            .debug_bounds("log-field-cell-0-0")
            .expect("field cell")
            .center();
        visual.simulate_mouse_down(cell, MouseButton::Right, Modifiers::default());
        filter_frame(visual);
        visual.simulate_mouse_up(cell, MouseButton::Right, Modifiers::default());
        filter_frame(visual);
        visual.simulate_keystrokes(keys);
        filter_frame(visual);
        assert_eq!(shown(&view, visual), expected);
        view.read_with(visual, |view, _| assert_eq!(view.filter_ui.chips.len(), 1));
        filter_click(visual, "log-chip-remove-0");
        assert_eq!(shown(&view, visual).len(), 6);
    }
    // The visible affordance offers exactly the same actions as right-click.
    filter_click(visual, "log-field-cell-0-0-actions");
    assert!(
        visual.debug_bounds("log-cell-menu").is_some(),
        "hover action opens its menu"
    );
    let menu = view.read_with(visual, |view, _| {
        view.cell_menu_handle().expect("owned menu")
    });
    visual.simulate_keystrokes("down down down enter");
    filter_frame(visual);
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard()
                .expect("Copy value reached the clipboard")
                .text()
                .as_deref(),
            Some("200")
        )
    });
    assert!(view.read_with(visual, |view, _| view.cell_menu_handle().is_none()));
    assert!(
        menu.upgrade().is_none(),
        "copy dismisses and releases its menu"
    );
    assert_eq!(shown(&view, visual).len(), 6);
    filter_click(visual, "log-message-cell-0-actions");
    visual.simulate_keystrokes("down down down down down down enter");
    filter_frame(visual);
    view.read_with(visual, |view, cx| {
        assert!(
            view.columns(cx).contains(&"msg".to_owned()),
            "role cells expose their underlying field as a column"
        )
    });
}
#[gpui::test]
fn point_filters_header_counts_multiselect_edit_disable_remove_and_clear(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(state.path(), cx));
    let (_dir, reader, visual) = open_log(CHIP_ROWS, cx);
    let view = log_view(&reader, visual);
    view.update(visual, |view, cx| view.toggle_column("sample", cx));
    filter_click(visual, "log-header-filter-0");
    assert!(visual.debug_bounds("log-count-scope").is_some());
    // Sorted typed values: Missing, empty string, string 200, number 200,
    // boolean false, null. Each has one matching loaded row.
    for selector in [
        "log-filter-count-0",
        "log-filter-count-1",
        "log-filter-count-2",
        "log-filter-count-3",
        "log-filter-count-4",
        "log-filter-count-5",
    ] {
        assert!(visual.debug_bounds(selector).is_some());
    }
    filter_click(visual, "log-filter-value-2");
    filter_click(visual, "log-filter-value-3");
    filter_click(visual, "log-filter-apply");
    assert_eq!(shown(&view, visual), [0, 1]);
    visual.simulate_keystrokes("escape");
    filter_frame(visual);
    filter_click(visual, "log-chip-0");
    filter_click(visual, "log-mode-exclude");
    filter_click(visual, "log-filter-apply");
    assert_eq!(shown(&view, visual), [2, 3, 4, 5]);
    filter_click(visual, "log-chip-enabled");
    filter_click(visual, "log-filter-apply");
    assert_eq!(shown(&view, visual).len(), 6);
    visual.simulate_keystrokes("escape");
    filter_frame(visual);
    filter_click(visual, "log-chip-0");
    filter_click(visual, "log-chip-enabled");
    // Changing the selected values is part of editing the same chip.
    filter_click(visual, "log-filter-value-3");
    filter_click(visual, "log-filter-apply");
    assert_eq!(shown(&view, visual), [1, 2, 3, 4, 5]);
    visual.simulate_keystrokes("escape");
    filter_frame(visual);
    filter_click(visual, "log-chips-clear");
    assert_eq!(shown(&view, visual).len(), 6);
    view.read_with(visual, |view, cx| {
        assert!(view.filter_ui.chips.is_empty());
        assert!(view.query_input.read(cx).value().is_empty());
    });
}
#[gpui::test]
fn point_filters_survive_follow_reload_columns_and_saved_views(cx: &mut TestAppContext) {
    use std::io::Write as _;
    let _shutdown = TestShutdown(cx.clone());
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(state.path(), cx));
    let (dir, reader, visual) = open_log(CHIP_ROWS, cx);
    let view = log_view(&reader, visual);
    view.update_in(visual, |view, window, cx| {
        view.toggle_column("sample", cx);
        view.add_chip(
            FilterChip::new(
                FieldRef::Key("sample".into()),
                vec![FilterValue::string("200")],
                FilterMode::Include,
            ),
            window,
            cx,
        );
        view.toggle_follow(cx);
    });
    filter_frame(visual);
    let path = dir.path().join("app.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{{\"msg\":\"append\",\"sample\":\"200\"}}").unwrap();
    drop(file);
    visual.executor().advance_clock(Duration::from_millis(500));
    filter_frame(visual);
    assert_eq!(shown(&view, visual), [0, 6]);
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("app.jsonl", window, cx)
    });
    filter_frame(visual);
    let view = log_view(&reader, visual);
    assert_eq!(shown(&view, visual), [0, 6]);
    view.update_in(visual, |view, window, cx| {
        view.begin_saving(window, cx);
        view.saving.as_ref().unwrap().input.update(cx, |input, cx| {
            input.set_value("Synthetic investigation", window, cx)
        });
        view.commit_saving(window, cx);
        view.clear_filters(window, cx);
        view.toggle_column("sample", cx);
        view.apply_saved("Synthetic investigation", window, cx);
    });
    filter_frame(visual);
    assert_eq!(shown(&view, visual), [0, 6]);
    view.read_with(visual, |view, cx| {
        assert_eq!(view.columns(cx), ["sample"]);
        assert_eq!(view.filter_ui.chips.len(), 1);
    });
    view.update_in(visual, |view, window, cx| {
        view.filter_ui.chips[0].enabled = false;
        view.filter_ui.chips[0].execution = okilum_core::log::chips::FilterExecution::Server;
        view.sync_chips(window, cx);
        view.begin_saving(window, cx);
        view.saving.as_ref().unwrap().input.update(cx, |input, cx| {
            input.set_value("Synthetic paused", window, cx)
        });
        view.commit_saving(window, cx);
        view.clear_filters(window, cx);
        view.apply_saved("Synthetic paused", window, cx);
    });
    filter_frame(visual);
    assert_eq!(shown(&view, visual).len(), 7);
    view.read_with(visual, |view, _| {
        assert!(!view.filter_ui.chips[0].enabled);
        assert_eq!(
            view.filter_ui.chips[0].execution,
            okilum_core::log::chips::FilterExecution::Server
        );
    });
}

#[gpui::test]
fn point_filters_count_provider_is_lazy_cached_and_values_are_virtualized(cx: &mut TestAppContext) {
    use okilum_core::log::chips::{ColumnCountProvider, ColumnCounts, CountSource, ValueCount};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counts(Arc<AtomicUsize>);
    impl ColumnCountProvider for Counts {
        fn count(&self, _: &FieldRef, _: &[CountSource<'_>]) -> Result<ColumnCounts, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ColumnCounts {
                label: "synthetic count scope".into(),
                values: (0..50_000)
                    .map(|at| ValueCount {
                        value: FilterValue::string(&format!("synthetic value {at}")),
                        count: at + 1,
                    })
                    .collect(),
            })
        }
    }
    let _shutdown = TestShutdown(cx.clone());
    let (_dir, reader, visual) = open_log(CHIP_ROWS, cx);
    let view = log_view(&reader, visual);
    let calls = Arc::new(AtomicUsize::new(0));
    view.update(visual, |view, cx| {
        view.set_count_provider(Arc::new(Counts(calls.clone())), cx)
    });
    filter_frame(visual);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "draw does not count");
    filter_click(visual, "log-message-filter");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "opening invokes the provider"
    );
    assert!(
        visual.debug_bounds("log-filter-count-0").is_some(),
        "positive control: visible value rendered"
    );
    assert!(
        visual.debug_bounds("log-filter-count-49999").is_none(),
        "offscreen values have no draw elements"
    );
    for _ in 0..5 {
        filter_frame(visual);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "repaints reuse the counted snapshot"
    );
    filter_click(visual, "log-value-search");
    visual.simulate_input("value 49999");
    filter_frame(visual);
    assert!(
        visual.debug_bounds("log-filter-count-49999").is_some(),
        "search finds a previously virtualized value"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "search does not recount rows"
    );
}

#[gpui::test]
fn point_filters_duplicate_field_cell_uses_the_displayed_occurrence(cx: &mut TestAppContext) {
    let _shutdown = TestShutdown(cx.clone());
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(state.path(), cx));
    let (_dir, reader, visual) = open_log(
        concat!(
            "{\"msg\":\"a\",\"sample\":200,\"sample\":\"200\"}\n",
            "{\"msg\":\"b\",\"sample\":\"200\"}\n",
            "{\"msg\":\"c\",\"sample\":200}\n",
        ),
        cx,
    );
    let view = log_view(&reader, visual);
    view.update(visual, |view, cx| view.toggle_column("sample", cx));
    filter_click(visual, "log-field-cell-0-0-actions");
    assert!(
        visual.debug_bounds("log-cell-menu").is_some(),
        "hover action opens its menu"
    );
    let menu = view.read_with(visual, |view, _| {
        view.cell_menu_handle().expect("owned menu")
    });
    visual.simulate_keystrokes("down enter");
    filter_frame(visual);
    assert_eq!(
        shown(&view, visual),
        [0, 2],
        "the displayed first occurrence is a number"
    );
    view.read_with(visual, |view, _| {
        assert_eq!(
            view.filter_chips()[0].values,
            [FilterValue::Value {
                kind: ValueKind::Number,
                text: "200".into()
            }]
        )
    });
    assert!(menu.upgrade().is_none(), "include releases the popup");
}

#[gpui::test]
fn point_filters_blank_cells_keep_hitboxes_and_distinguish_empty_from_missing(
    cx: &mut TestAppContext,
) {
    let _shutdown = TestShutdown(cx.clone());
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| reader_ui_state::install(state.path(), cx));
    let (_dir, reader, visual) = open_log(CHIP_ROWS, cx);
    visual.simulate_resize(gpui::size(px(1440.), px(900.)));
    let view = log_view(&reader, visual);
    view.update(visual, |view, cx| view.toggle_column("sample", cx));
    filter_frame(visual);
    for (row, selector) in [(4, "log-field-cell-4-0"), (5, "log-field-cell-5-0")] {
        let bounds = visual
            .debug_bounds(selector)
            .expect("blank field has a hitbox");
        assert_eq!(bounds.size.height, px(ROW_HEIGHT));
        visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
        filter_frame(visual);
        visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
        filter_frame(visual);
        let menu = view.read_with(visual, |view, _| {
            view.cell_menu_handle().expect("right-click opened a menu")
        });
        visual.simulate_keystrokes("down enter");
        filter_frame(visual);
        assert_eq!(shown(&view, visual), [row]);
        assert!(view.read_with(visual, |view, _| view.cell_menu_handle().is_none()));
        assert!(
            menu.upgrade().is_none(),
            "filtering away the trigger releases its menu"
        );
        filter_click(visual, "log-chip-remove-0");
    }
    filter_click(visual, "log-field-cell-0-0-actions");
    let menu = view.read_with(visual, |view, _| {
        view.cell_menu_handle().expect("hover menu opened")
    });
    visual.simulate_keystrokes("escape");
    filter_frame(visual);
    assert!(
        menu.upgrade().is_none(),
        "Escape releases the popup without applying a filter"
    );
    assert_eq!(shown(&view, visual).len(), 6);
}
