use super::*;
use ::core::prelude::v1::test;

fn fixture<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    text: &str,
) -> (
    Entity<CodePreview>,
    &'a mut VisualTestContext,
    tempfile::TempDir,
) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(name);
    std::fs::write(&path, text).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| CodePreview::new(temp.path().into(), name.into(), path, window, cx));
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    visual.simulate_resize(size(px(1100.), px(700.)));
    visual.run_until_parked();
    (preview.unwrap(), visual, temp)
}

#[gpui::test]
fn code2809_valid_object_has_three_views_and_no_legacy_rows(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "data.json", "{\"a\":{\"b\":1}}");
    preview.read_with(visual, |p, _| {
        assert_eq!(p.view, View::Formatted);
        for view in [View::Tree, View::Formatted, View::Raw] {
            assert_eq!(p.availability(view), Availability::Available);
        }
        assert_eq!(p.availability(View::Table), Availability::Absent);
    });
    assert!(
        visual.debug_bounds("code-view-menu").is_some(),
        "compact positive control"
    );
    assert!(
        visual.debug_bounds("code-fold-all").is_some(),
        "shared action positive control"
    );
    for selector in [
        "json-pretty",
        "json-raw",
        "json-source",
        "json-table",
        "data-structure",
        "data-source",
        "data-page-range",
        "data-open-external",
        "code-file-open",
    ] {
        assert!(
            visual.debug_bounds(selector).is_none(),
            "removed {selector}"
        );
    }
    preview.read_with(visual, |p, cx| {
        assert!(p.status(preview.clone(), cx).text.is_empty())
    });
}

#[gpui::test]
fn code2809_switches_and_pages_keep_tree_folds_and_ready_snapshots(cx: &mut TestAppContext) {
    let text = format!(
        "{{\"a\":{{\"b\":[{}]}},\"c\":2}}",
        "1,".repeat(40_000).trim_end_matches(',')
    );
    let (preview, visual, _temp) = fixture(cx, "data.json", &text);
    preview.update_in(visual, |p, window, cx| {
        assert_eq!(p.view, View::Tree);
        p.fold_all(true, cx);
        assert_eq!(p.view, View::Tree);
        assert_eq!(p.structure.visible, vec![0]);
        let raw = p.document.clone().unwrap();
        p.set_view(View::Raw, window, cx);
        p.show_page(2, window, cx);
        p.fold_all(false, cx);
        assert_eq!(p.view, View::Raw);
        assert!(matches!(p.fold_availability(), Availability::Disabled(_)));
        p.set_view(View::Tree, window, cx);
        assert_eq!(p.structure.visible, vec![0]);
        assert!(Arc::ptr_eq(&raw, p.document.as_ref().unwrap()));
        p.set_view(View::Formatted, window, cx);
    });
    visual.run_until_parked();
    preview.update_in(visual, |p, window, cx| {
        assert_eq!(p.view, View::Formatted);
        assert!(p.document.is_some());
        assert!(
            p.status(preview.clone(), cx).text.is_empty(),
            "Formatted without parts must not keep the Raw long-line reason"
        );
        p.fold_all(true, cx);
        assert_eq!(p.view, View::Formatted);
        assert!(!p.input().unwrap().read(cx).folded_ranges().is_empty());
        p.set_view(View::Tree, window, cx);
        assert_eq!(p.structure.visible, vec![0]);
    });
}

#[gpui::test]
fn code2809_invalid_paged_json_keeps_error_and_navigates_above_auto_threshold(
    cx: &mut TestAppContext,
) {
    let text = format!("{{\"a\":\"{}\",}}", "x".repeat(PRETTY_BYTES + 200));
    let (preview, visual, temp) = fixture(cx, "broken.json", &text);
    preview.update_in(visual, |p, _, cx| {
        assert_eq!(p.view, View::Raw);
        let error = p.raw_document.as_ref().unwrap().invalid.clone().unwrap();
        assert!(error.offset > PRETTY_BYTES);
        assert_eq!(error.line, 1);
        assert_eq!(error.column, text.len());
        assert!(error.message == "key must be a string");
        assert!(matches!(
            p.availability(View::Tree),
            Availability::Disabled(_)
        ));
        assert!(matches!(
            p.availability(View::Formatted),
            Availability::Disabled(_)
        ));
        let chrome = p.chrome(preview.clone(), cx);
        assert_eq!(chrome.state.as_ref().unwrap().id, "code-readonly");
        assert_eq!(chrome.badges[0].id, "code-invalid");
        let status = p.status(preview.clone(), cx);
        assert_eq!(status.label.as_ref(), "Not valid JSON.");
        assert!(status.text.contains("key must be a string"));
    });
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    let warning = visual
        .debug_bounds("code-invalid")
        .expect("warning badge is mounted in the compact header");
    visual.simulate_click(warning.center(), Modifiers::default());
    visual.run_until_parked();
    preview.read_with(visual, |p, cx| {
        let error = p.raw_document.as_ref().unwrap().invalid.as_ref().unwrap();
        assert_eq!(p.page, p.document.as_ref().unwrap().page_at(error.offset));
        assert!(p.page > 1);
        let input = p.input().unwrap();
        let input = input.read(cx);
        assert_eq!(
            input.cursor(),
            error.offset - p.document.as_ref().unwrap().pages[p.page].start
        );
        assert_eq!(
            input.diagnostics().unwrap().len(),
            1,
            "read-only diagnostics retained on the error page"
        );
    });
    assert_eq!(
        std::fs::read_to_string(temp.path().join("broken.json")).unwrap(),
        text
    );
}

#[gpui::test]
fn code2809_large_line_and_size_status_precedence_and_edit_cap(cx: &mut TestAppContext) {
    let text = format!("{{\"a\":\"{}\"}}", "x".repeat(5_200_000));
    let (preview, visual, _temp) = fixture(cx, "data.json", &text);
    preview.update_in(visual, |p, window, cx| {
        assert_eq!(p.view, View::Tree);
        assert!(!p.editable());
        let chrome = p.chrome(preview.clone(), cx);
        assert_eq!(chrome.state.as_ref().unwrap().tooltip.as_ref(), "Read-only: 5.2 MB is over the 4 MB editing limit");
        assert_eq!(p.status(preview.clone(), cx).text.as_ref(), "5.2 MB on a single line. Tree shows all of it.");
        p.set_view(View::Raw, window, cx);
        assert_eq!(p.status(preview.clone(), cx).text.as_ref(), "5.2 MB on a single line, so Raw shows it in parts. Tree, Find and Copy cover the whole file.");
        p.set_view(View::Formatted, window, cx);
        assert_eq!(p.status(preview.clone(), cx).text.as_ref(), "Formatting 5.2 MB…");
    });
    visual.run_until_parked();
    preview.read_with(visual, |p, cx| {
        assert_eq!(p.view, View::Formatted);
        // A long string remains paged after formatting; position controls apply
        // to Formatted as well as Raw.
        assert!(p.document.as_ref().unwrap().pages.len() > 1);
        assert!(p
            .status(preview.clone(), cx)
            .text
            .contains("Formatted shows it in parts"));
        assert_eq!(
            p.chrome(preview.clone(), cx).state.unwrap().id,
            "code-readonly"
        );
    });
    assert!(visual.debug_bounds("code-part-scrubber").is_some());
}

#[gpui::test]
fn code2809_above_auto_threshold_opens_raw_and_only_click_starts_formatting(
    cx: &mut TestAppContext,
) {
    let text = format!("{{\"a\":\"{}\"}}", "x".repeat(PRETTY_BYTES + 100));
    let (preview, visual, _temp) = fixture(cx, "data.json", &text);
    preview.update_in(visual, |p, window, cx| {
        assert_eq!(p.view, View::Raw);
        assert!(p.json.as_ref().unwrap().pretty.is_none());
        assert_eq!(
            p.availability(View::Formatted),
            Availability::Available,
            "8 MiB is not a refusal"
        );
        let status = p.status(preview.clone(), cx);
        assert_eq!(status.label.as_ref(), "Large file.");
        assert_eq!(
            status.text.as_ref(),
            "8.4 MB, opened as raw text to stay fast."
        );
        assert_eq!(status.actions[0].label.as_ref(), "Show formatted");
        p.set_view(View::Formatted, window, cx);
        assert_eq!(
            p.status(preview.clone(), cx).text.as_ref(),
            "Formatting 8.4 MB…"
        );
        p.set_view(View::Tree, window, cx);
    });
    visual.run_until_parked();
    preview.read_with(visual, |p, _| {
        assert_eq!(
            p.view,
            View::Tree,
            "choice made during worker formatting wins"
        );
        assert!(matches!(p.json.as_ref().unwrap().pretty, Some(Ok(_))));
    });
}

#[gpui::test]
fn code2809_single_view_formats_have_no_switch_and_xml_keeps_readonly_reason(
    cx: &mut TestAppContext,
) {
    for (name, text) in [
        ("source.rs", "fn main() {\n    println!(\"hi\");\n}\n"),
        ("text.txt", "plain text"),
        ("data.yaml", "a:\n  b: 1\n"),
        ("data.toml", "[a]\nb=1\n"),
        ("data.xml", "<root>\n <a>one</a>\n</root>"),
    ] {
        let (preview, visual, _temp) = fixture(cx, name, text);
        preview.update_in(visual, |p, window, cx| {
            assert_eq!(p.view, View::Raw);
            assert_eq!(p.availability(View::Tree), Availability::Absent);
            assert_eq!(p.availability(View::Formatted), Availability::Absent);
            assert!(p.chrome(preview.clone(), cx).views.is_empty());
            for view in [View::Table, View::Tree, View::Formatted] {
                p.set_view(view, window, cx);
                assert_eq!(p.view, View::Raw);
            }
            if name.ends_with("xml") {
                assert!(!p.editable());
                assert_eq!(
                    p.chrome(preview.clone(), cx)
                        .state
                        .unwrap()
                        .tooltip
                        .as_ref(),
                    "Read-only: XML files are view-only"
                );
                assert_eq!(
                    p.status(preview.clone(), cx).text.as_ref(),
                    "XML files are view-only"
                );
            }
        });
        assert!(visual.debug_bounds("code-view-menu").is_none());
    }
}

#[gpui::test]
fn code2809_scrubber_requests_coalesce_to_the_last_part(cx: &mut TestAppContext) {
    let text = format!("{{\"a\":\"{}\"}}", "x".repeat(120_000));
    let (preview, visual, _temp) = fixture(cx, "data.json", &text);
    preview.update_in(visual, |p, window, cx| {
        p.set_view(View::Raw, window, cx);
        for page in [1, 0, 2, 3] {
            p.request_page(page, window, cx);
        }
        assert_eq!(p.page, 0, "no shaping on each slider event");
        assert_eq!(p.requested_page, Some(3));
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.simulate_next_frame(cx);
    });
    visual.run_until_parked();
    preview.read_with(visual, |p, _| {
        assert_eq!(p.page, 3);
        assert_eq!(p.requested_page, None);
        assert!(!p.page_scheduled);
    });
}

#[test]
fn code2809_scrubber_fraction_maps_endpoints_and_nearest_part() {
    for (fraction, expected) in [
        (-1., 0),
        (0., 0),
        (0.49, 49),
        (0.5, 50),
        (1., 100),
        (2., 100),
    ] {
        assert_eq!(view::fraction_to_part(fraction, 101), expected);
    }
    assert_eq!(view::fraction_to_part(0.9, 1), 0);
}

#[gpui::test]
fn code2809_first_view_guards_precede_memory_and_guarded_choices_are_not_saved(
    cx: &mut TestAppContext,
) {
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(state.path(), cx);
        reader_ui_state::set_code_view("json", View::Raw, cx);
    });
    {
        let (preview, visual, _temp) = fixture(cx, "object.json", "{\"a\":1}");
        preview.read_with(visual, |p, _| {
            assert_eq!(p.view, View::Raw, "object uses its remembered view")
        });
    }
    {
        let (preview, visual, _temp) = fixture(cx, "array.json", "[{\"a\":1}]");
        preview.read_with(visual, |p, _| {
            assert_eq!(
                p.view,
                View::Table,
                "object memory does not leak into arrays"
            )
        });
    }
    {
        let source = format!("{{\"a\":\"{}\"}}", "x".repeat(100_000));
        let (preview, visual, _temp) = fixture(cx, "long.json", &source);
        preview.update_in(visual, |p, window, cx| {
            assert_eq!(p.view, View::Tree, "long-line guard beats remembered Raw");
            p.set_view(View::Formatted, window, cx);
        });
        visual.run_until_parked();
        preview.read_with(visual, |p, cx| {
            assert_eq!(p.view, View::Formatted);
            assert_eq!(
                reader_ui_state::code_view("json", cx),
                Some(View::Raw),
                "guarded file never writes memory"
            );
        });
    }
    cx.update(|cx| reader_ui_state::set_code_view("json", View::Formatted, cx));
    {
        let source = format!("{{\"a\":\"{}\"}}", "x".repeat(9_000_000));
        let (preview, visual, _temp) = fixture(cx, "large.json", &source);
        preview.update_in(visual, |p, window, cx| {
            assert_eq!(
                p.view,
                View::Raw,
                "size guard beats remembered Formatted and long-line Tree"
            );
            p.set_view(View::Tree, window, cx);
            assert_eq!(
                reader_ui_state::code_view("json", cx),
                Some(View::Formatted)
            );
        });
    }
    cx.update(|cx| reader_ui_state::set_code_view("xml", View::Tree, cx));
    let (preview, visual, _temp) = fixture(cx, "data.xml", "<root>one</root>");
    preview.read_with(visual, |p, _| {
        assert_eq!(p.view, View::Raw, "unavailable remembered view falls back")
    });
}

#[gpui::test]
fn code2809_real_format_refusals_explain_depth_and_output_budget(cx: &mut TestAppContext) {
    for (source, reason) in [
        (format!("{}0{}", "[".repeat(129), "]".repeat(129)), "128"),
        (
            format!(
                "{}{}0{}",
                "[".repeat(90),
                "0,".repeat(400_000),
                "]".repeat(90)
            ),
            "64 MB",
        ),
    ] {
        let (preview, visual, _temp) = fixture(cx, "deep.json", &source);
        preview.update_in(visual, |p, window, cx| {
            let Availability::Disabled(tooltip) = p.availability(View::Formatted) else {
                panic!("real refusal must be known before clicking");
            };
            assert!(tooltip.contains(reason), "{tooltip}");
            p.set_view(View::Formatted, window, cx);
            assert_ne!(p.view, View::Formatted);
            assert!(!p.json.as_ref().unwrap().formatting);
        });
    }
}

#[gpui::test]
fn code2809_readonly_status_is_last_and_invalid_wins_over_formatting(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "data.json", "{\"a\":1}");
    preview.update_in(visual, |p, _, cx| {
        p.source_bytes = Some(5_200_000);
        let status = p.status(preview.clone(), cx);
        assert_eq!(status.label.as_ref(), "Read-only.");
        assert_eq!(
            status.text.as_ref(),
            "5.2 MB is over the 4 MB editing limit"
        );
        assert_eq!(status.actions[0].label.as_ref(), "Open with default app");
        p.json.as_mut().unwrap().formatting = true;
        assert!(p
            .status(preview.clone(), cx)
            .text
            .starts_with("Formatting "));
        let raw = Arc::new(Document::for_file("{,}".into(), "json", true));
        p.raw_document = Some(raw);
        assert_eq!(
            p.status(preview.clone(), cx).label.as_ref(),
            "Not valid JSON."
        );
        assert_eq!(
            p.chrome(preview.clone(), cx).state.unwrap().id,
            "code-readonly",
            "lock reason remains available at every precedence level"
        );
    });
}

#[gpui::test]
fn code2809_preview_message_states_offer_default_app(cx: &mut TestAppContext) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("binary.rs"), [0xff, 0xfe]).unwrap();
    let huge = std::fs::File::create(temp.path().join("huge.rs")).unwrap();
    huge.set_len(MAX_BYTES + 1).unwrap();
    cx.update(crate::test_init);
    for (name, expected) in [
        ("binary.rs", "This file cannot be previewed as text."),
        ("missing.rs", "This file could not be read."),
        (
            "huge.rs",
            "Too large to preview. 67.1 MB, the limit is 64 MB.",
        ),
    ] {
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                CodePreview::new(
                    temp.path().into(),
                    name.into(),
                    temp.path().join(name),
                    window,
                    cx,
                )
            });
            preview = Some(view.clone());
            Root::new(cx.new(|_| Surface(view)), window, cx)
        });
        visual.run_until_parked();
        preview.unwrap().read_with(visual, |p, _| {
            let State::Message(message) = &p.state else {
                panic!("message state");
            };
            assert_eq!(message, expected);
        });
        assert!(
            visual.debug_bounds("code-message-open").is_some(),
            "default app action remains available"
        );
    }
}

struct GuardObserver {
    _subscriptions: Vec<Subscription>,
}
fn settle(visual: &mut VisualTestContext) {
    for _ in 0..3 {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
    }
    visual.run_until_parked();
}
#[gpui::test]
fn code2809_find_and_paging_render_zero_header_slots_and_notify_zero_side_panels(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _cached = reader_side_panel::tests::CachedPanels::on();
    let source = format!("{{\"payload\":\"{} target\"}}", "x".repeat(150_000));
    let (reader, visual, _temp) =
        reader_viewer_chrome::tests::fixture_with_files(cx, "find.json", &[("find.json", &source)]);
    settle(visual);
    let code = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    let started = Instant::now();
    while code.read_with(visual, |code, _| code.document.is_none()) {
        settle(visual);
        assert!(started.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(10));
    }
    code.update_in(visual, |code, window, cx| code.open_find(window, cx));
    visual.simulate_keystrokes("home");
    settle(visual);
    let panels = reader.read_with(visual, |reader, _| reader.side_panels.clone());
    assert_eq!(panels.iter().flatten().count(), 2);
    let notifications = std::rc::Rc::new(std::cell::Cell::new([0; 2]));
    let _observer = visual.update(|_, cx| {
        cx.new(|cx: &mut Context<GuardObserver>| GuardObserver {
            _subscriptions: panels
                .iter()
                .enumerate()
                .map(|(i, panel)| {
                    let notifications = notifications.clone();
                    cx.observe(panel.as_ref().unwrap(), move |_, _, _| {
                        let mut value = notifications.get();
                        value[i] += 1;
                        notifications.set(value);
                    })
                })
                .collect(),
        })
    });
    visual.update(|_, cx| {
        for panel in panels.into_iter().flatten() {
            panel.update(cx, |_, cx| cx.notify());
        }
    });
    settle(visual);
    assert!(
        notifications.get().into_iter().all(|n| n > 0),
        "observer positive control"
    );
    notifications.set([0, 0]);
    let renders = reader_viewer_chrome::header_renders(reader.entity_id());
    assert!(renders > 0);
    let mut query = String::new();
    for ch in "target".chars() {
        query.push(ch);
        visual.simulate_input(&ch.to_string());
        settle(visual);
        code.read_with(visual, |code, cx| {
            assert_eq!(code.find.read(cx).value().as_ref(), query);
            assert!(!code.searching);
            assert_eq!(
                code.matches.len(),
                if query == "t" { 2 } else { 1 },
                "query actually completed"
            );
        });
        assert_eq!(
            reader_viewer_chrome::header_renders(reader.entity_id()),
            renders,
            "Find renders zero slots"
        );
        assert_eq!(notifications.get(), [0, 0]);
    }
    code.update_in(visual, |code, window, cx| {
        code.set_view(View::Raw, window, cx)
    });
    settle(visual);
    notifications.set([0, 0]);
    let renders = reader_viewer_chrome::header_renders(reader.entity_id());
    for part in [1, 2, 3, 0] {
        code.update_in(visual, |code, window, cx| code.show_page(part, window, cx));
        settle(visual);
        code.read_with(visual, |code, _| assert_eq!(code.page, part));
        assert_eq!(
            reader_viewer_chrome::header_renders(reader.entity_id()),
            renders,
            "paging renders zero slots"
        );
        assert_eq!(notifications.get(), [0, 0]);
    }
}

#[gpui::test]
fn code2809_storage_reason_is_known_only_after_failed_edit_admission(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "data.json", "{\"a\":1}");
    preview.update_in(visual, |p, _, cx| {
        assert!(p.editable());
        assert!(p.chrome(preview.clone(), cx).state.is_none());
        p.record_edit_failure(&anyhow::anyhow!("unrelated error"), cx);
        assert!(p.chrome(preview.clone(), cx).state.is_none());
        p.record_edit_failure(
            &anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            cx,
        );
        assert!(!p.editable());
        assert_eq!(
            p.chrome(preview.clone(), cx)
                .state
                .unwrap()
                .tooltip
                .as_ref(),
            "Read-only: this folder cannot be written"
        );
    });
}

#[gpui::test]
fn code2809_copy_menu_disables_tree_selection_and_copies_exact_node_with_counted_toast(
    cx: &mut TestAppContext,
) {
    use gpui_component::WindowExt as _;
    let properties = (0..210)
        .map(|i| format!("  \"key{i}\": {i}"))
        .collect::<Vec<_>>()
        .join(",\n");
    let source = format!("{{\n{properties}\n}}");
    let (preview, visual, temp) = fixture(cx, "data.json", &source);
    preview.update_in(visual, |p, window, cx| p.set_view(View::Tree, window, cx));
    let chrome = preview.read_with(visual, |p, cx| p.chrome(preview.clone(), cx));
    let selection = chrome
        .copy_actions
        .iter()
        .find(|a| a.id == "code-copy-selection")
        .unwrap();
    assert!(!selection.enabled);
    assert_eq!(
        selection.disabled_reason.as_ref().unwrap().as_ref(),
        "Select text in Formatted or Raw"
    );
    let copy = chrome
        .copy_actions
        .iter()
        .find(|a| a.id == "code-copy-node")
        .unwrap();
    visual.update(|window, cx| {
        assert!(window.notifications(cx).is_empty());
        (copy.invoke)(window, cx);
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), source);
        assert_eq!(window.notifications(cx).len(), 1, "copy posts a real toast");
    });
    assert_eq!(view::copy_node_notice(&source), "Copied node · 212 lines");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("data.json")).unwrap(),
        source
    );
}

#[gpui::test]
fn code2809_raw_only_long_line_names_only_available_whole_file_tools(cx: &mut TestAppContext) {
    let text = "x".repeat(120_000);
    let (preview, visual, _temp) = fixture(cx, "text.txt", &text);
    preview.read_with(visual, |p, cx| {
        assert_eq!(p.availability(View::Tree), Availability::Absent);
        assert_eq!(p.status(preview.clone(), cx).text.as_ref(), "0.1 MB on a single line, so Raw shows it in parts. Find and Copy cover the whole file.");
    });
}

#[gpui::test]
fn code2809_table_keeps_its_own_status_without_a_second_code_line(cx: &mut TestAppContext) {
    let (preview, visual, _temp) = fixture(cx, "rows.json", "[{\"a\":1}]");
    preview.update(visual, |p, cx| {
        assert_eq!(p.view, View::Table);
        p.source_bytes = Some(5_200_000);
        cx.notify();
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    assert!(
        visual.debug_bounds("delimited-viewport").is_some(),
        "actual table is mounted"
    );
    assert!(
        visual.debug_bounds("code-readonly").is_some(),
        "read-only reason remains in the header lock"
    );
    assert!(
        visual.debug_bounds("code-status-line").is_none(),
        "Table must not get a second CodePreview bottom line"
    );
}

#[gpui::test]
fn code2809_loading_shortcuts_do_not_write_unclassified_or_guarded_memory(cx: &mut TestAppContext) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("large.json");
    std::fs::write(
        &path,
        format!("{{\"payload\":\"{}\"}}", "x".repeat(9_000_000)),
    )
    .unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(&temp.path().join("state"), cx);
        reader_ui_state::set_code_view("json", View::Formatted, cx);
    });
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx
            .new(|cx| CodePreview::new(temp.path().into(), "large.json".into(), path, window, cx));
        view.update(cx, |code, cx| {
            assert!(
                code.raw_document.is_none(),
                "positive control: metadata is not known yet"
            );
            assert!(matches!(
                code.availability(View::Raw),
                Availability::Disabled(_)
            ));
            code.set_view(View::Raw, window, cx);
            assert_eq!(
                reader_ui_state::code_view("json", cx),
                Some(View::Formatted),
                "a loading shortcut cannot persist a view before the guards/type are known"
            );
        });
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    visual.run_until_parked();
    preview.unwrap().read_with(visual, |code, cx| {
        assert_eq!(code.view, View::Raw);
        assert_eq!(
            reader_ui_state::code_view("json", cx),
            Some(View::Formatted)
        );
    });
}

#[gpui::test]
fn code2809_catalogued_copy_selection_and_error_actions_dispatch_from_custom_keys(
    cx: &mut TestAppContext,
) {
    let source = "{\n  invalid: true\n}\n";
    let (preview, visual, temp) = fixture(cx, "broken.json", source);
    preview.update_in(visual, |code, window, cx| {
        cx.bind_keys([
            KeyBinding::new("f7", GoToError, Some("ReaderCode")),
            KeyBinding::new("f8", CopySelection, Some("ReaderCode")),
        ]);
        assert_eq!(code.input().unwrap().read(cx).cursor(), 0);
        let chrome = code.chrome(preview.clone(), cx);
        assert!(
            chrome
                .state
                .as_ref()
                .unwrap()
                .tooltip
                .to_ascii_lowercase()
                .contains("f7"),
            "badge hint follows the live Go to error binding"
        );
        assert!(chrome.copy_actions[0]
            .tooltip
            .to_ascii_lowercase()
            .contains("f8"));
        code.focus.focus(window, cx);
    });
    visual.simulate_keystrokes("f7");
    visual.run_until_parked();
    preview.update(visual, |code, cx| {
        let error = code
            .raw_document
            .as_ref()
            .unwrap()
            .invalid
            .as_ref()
            .unwrap();
        let input = code.input().unwrap();
        assert_eq!(
            input.read(cx).cursor(),
            error.offset,
            "Go to error reaches the same diagnostic through a custom key"
        );
        input.update(cx, |input, cx| input.set_selected_range(4..11, cx));
    });
    visual.simulate_keystrokes("f8");
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "invalid",
            "custom Copy selection uses the real selected bytes"
        )
    });
    assert_eq!(
        std::fs::read_to_string(temp.path().join("broken.json")).unwrap(),
        source
    );
}

#[gpui::test]
fn code2809_switching_from_formatted_find_to_paged_raw_keeps_exactly_one_find_ui(
    cx: &mut TestAppContext,
) {
    let text = format!(
        "{{\"values\":[{}]}}",
        "1,".repeat(40_000).trim_end_matches(',')
    );
    let (preview, visual, _temp) = fixture(cx, "data.json", &text);
    preview.update_in(visual, |code, window, cx| {
        code.set_view(View::Formatted, window, cx)
    });
    visual.run_until_parked();
    preview.update_in(visual, |code, window, cx| {
        assert!(!code.paged);
        code.open_find(window, cx);
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    assert!(
        visual.debug_bounds("search-panel").is_some(),
        "positive control: the real toolkit Find strip opened"
    );
    assert!(visual.debug_bounds("data-whole-file-find").is_none());
    preview.update_in(visual, |code, window, cx| {
        code.set_view(View::Raw, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    assert!(
        visual.debug_bounds("data-whole-file-find").is_some(),
        "the whole-file row owns paged Raw Find"
    );
    assert!(
        visual.debug_bounds("search-panel").is_none(),
        "a viewer must never mount two Find fields"
    );
}
