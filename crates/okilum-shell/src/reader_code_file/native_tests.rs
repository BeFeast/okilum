use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn code2771_invalid_json_formatted_is_disabled_and_never_writes_source(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("invalid.json");
    let source = "{ a: 1 }";
    std::fs::write(&path, source).unwrap();
    cx.update(crate::test_init);
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            CodePreview::new(
                fixture.path().to_owned(),
                "invalid.json".into(),
                path.clone(),
                window,
                cx,
            )
        });
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    let before = preview.read_with(visual, |p, _| p.document_revision);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    preview.update_in(visual, |p, window, cx| {
        assert_eq!(
            p.availability(View::Formatted),
            Availability::Disabled("Not available: the file is not valid JSON".into())
        );
        assert_eq!(
            p.availability(View::Tree),
            Availability::Disabled("Not available: the file is not valid JSON".into())
        );
        p.set_view(View::Formatted, window, cx);
        assert!(
            !p.json.as_ref().unwrap().formatting,
            "disabled segment never starts a worker"
        );
        assert_eq!(p.document_revision, before);
        assert_eq!(p.view, View::Raw);
        assert_eq!(p.input().unwrap().read(cx).value().as_ref(), source);
    });
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[gpui::test]
fn data_view_2669_native_xml_and_text_are_admitted_without_new_write_authority(
    cx: &mut TestAppContext,
) {
    let fixture = tempfile::tempdir().unwrap();
    for (name, text) in [
        ("data.xml", "<root><child>value</child></root>"),
        ("note.txt", "literal **text**"),
    ] {
        std::fs::write(fixture.path().join(name), text).unwrap();
        assert!(eligible(name));
        assert!(reader_open::OpenIntent::validate(&fixture.path().join(name), None, None).is_ok());
    }
    assert!(!reader_delimited::editable("data.xml"));
    assert!(reader_delimited::editable("note.txt")); // Existing explicit editor stays.
    cx.update(crate::test_init);
}

#[gpui::test]
fn data_view_2669_native_fold_copy_find_and_readonly_source(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("data.xml");
    let source =
        "<root>\n  <child>\n    <value>שלום</value>\n    <other>two</other>\n  </child>\n</root>\n";
    std::fs::write(&path, source).unwrap();
    cx.update(crate::test_init);
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            CodePreview::new(
                fixture.path().to_owned(),
                "data.xml".into(),
                path.clone(),
                window,
                cx,
            )
        });
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let input = preview.read_with(visual, |p, _| {
        assert!(p.document.is_some());
        assert!(!p.editable());
        p.input().expect("XML opens in the code viewer")
    });
    input.read_with(visual, |s, _| {
        assert!(!s.is_editable());
        assert_eq!(s.value().as_ref(), source);
    });
    let fold = visual.debug_bounds("code-fold-all").expect("Fold all");
    visual.simulate_click(fold.center(), Modifiers::default());
    visual.run_until_parked();
    input.read_with(visual, |s, _| {
        assert!(
            !s.folded_ranges().is_empty(),
            "native XML grammar supplies folds"
        )
    });
    let unfold = visual.debug_bounds("code-unfold-all").expect("Unfold all");
    visual.simulate_click(unfold.center(), Modifiers::default());
    visual.run_until_parked();
    input.update(visual, |s, cx| {
        assert!(s.folded_ranges().is_empty());
        s.set_selected_range(
            source.find("שלום").unwrap()..source.find("שלום").unwrap(),
            cx,
        );
    });
    preview.update_in(visual, |p, window, cx| p.copy_node(window, cx));
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "<value>שלום</value>"
        )
    });
    input.update(visual, |s, cx| s.set_selected_range(0..6, cx));
    let selection = preview.read_with(visual, |p, cx| {
        p.chrome(preview.clone(), cx)
            .copy_actions
            .into_iter()
            .find(|a| a.id == "code-copy-selection")
            .unwrap()
    });
    visual.update(|window, cx| (selection.invoke)(window, cx));
    visual.update(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "<root>"));
    input.update(visual, |s, cx| {
        s.set_search_query("other", false, cx);
        assert_eq!(s.search_session().matcher.matched_ranges().len(), 2);
    });
    assert_eq!(std::fs::read(&path).unwrap(), source.as_bytes());
}

#[gpui::test]
fn data_view_2669_native_32_mib_minified_file_has_bounded_layout_and_global_find(
    cx: &mut TestAppContext,
) {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("big.json");
    let source = format!(
        "{{\"long\":\"{}\",\"last\":\"NEEDLE\"}}",
        "x".repeat(32 * 1024 * 1024)
    );
    std::fs::write(&path, &source).unwrap();
    cx.update(crate::test_init);
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            CodePreview::new(
                fixture.path().to_owned(),
                "big.json".into(),
                path.clone(),
                window,
                cx,
            )
        });
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    let started = Instant::now();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let draw = started.elapsed();
    diagnostic_error!(
        "DATA_VIEW_2669 minified_bytes={} first_draw_ms={}",
        source.len(),
        draw.as_millis()
    );
    assert!(
        draw < std::time::Duration::from_secs(2),
        "bounded page first draw: {draw:?}"
    );
    // Above the automatic Pretty budget, a repeated Raw request is a no-op.
    preview.update_in(visual, |p, window, cx| {
        assert!(p.view != View::Formatted);
        let revision = p.document_revision;
        p.set_view(View::Raw, window, cx);
        assert_eq!(p.document_revision, revision);
        assert!(p.document.is_some());
        p.set_view(View::Formatted, window, cx);
    });
    visual.run_until_parked();
    // Switching immutable snapshots keeps a ready document, and text folding
    // never silently changes the selected view.
    preview.update_in(visual, |p, window, cx| {
        assert_eq!(p.view, View::Formatted);
        p.set_view(View::Raw, window, cx);
        assert_eq!(p.view, View::Raw);
        assert!(p.document.is_some());
        p.fold_all(true, cx);
        assert_eq!(p.view, View::Raw);
        p.set_view(View::Tree, window, cx);
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let fold = visual
        .debug_bounds("code-fold-all")
        .expect("Fold all for the complete minified AST");
    visual.simulate_click(fold.center(), Modifiers::default());
    visual.run_until_parked();
    preview.read_with(visual, |p, _| {
        assert!(p.view == View::Tree);
        assert_eq!(p.structure.visible, vec![0]);
    });
    let unfold = visual.debug_bounds("code-unfold-all").unwrap();
    visual.simulate_click(unfold.center(), Modifiers::default());
    visual.run_until_parked();
    preview.read_with(visual, |p, _| {
        assert_eq!(
            p.structure.visible.len(),
            p.document.as_ref().unwrap().structure.len()
        )
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let viewport = visual
        .debug_bounds("data-structure-viewport")
        .expect("bounded structure viewport");
    assert!(
        viewport.size.height >= px(96.),
        "three real rows fit: {viewport:?}"
    );
    let copy = visual
        .debug_bounds("data-node-copy-2")
        .expect("whole footer node beyond the first page");
    visual.simulate_click(copy.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "\"last\":\"NEEDLE\""
        )
    });
    preview.update_in(visual, |p, window, cx| {
        assert!(
            !p.editable(),
            "UI Edit has a 4 MiB cap; the independent source-tool budget stays 64 MiB"
        );
        assert!(!p.input().unwrap().read(cx).is_editable());
        assert!(p.input().unwrap().read(cx).value().len() <= 32 * 1024);
        assert!(p.document.as_ref().unwrap().pages.len() > 100);
        p.find.update(cx, |s, cx| s.set_value("needle", window, cx));
        p.search(window, cx);
    });
    visual.run_until_parked();
    preview.update_in(visual, |p, window, cx| {
        assert_eq!(p.matches.len(), 1);
        assert!(p.view == View::Tree, "Find retains the whole-file Tree");
        assert_eq!(p.structure.match_node, Some(2));
        assert_eq!(p.page, 0, "Tree matches never navigate byte pages");
        // Source still selects the same whole-file hit on the right raw page.
        p.set_view(View::Raw, window, cx);
        p.show_page(0, window, cx);
        p.show_match(window, cx);
        assert!(p.page > 0);
        assert_eq!(
            p.input().unwrap().read(cx).value()[p.input().unwrap().read(cx).selected_range()]
                .to_owned(),
            "NEEDLE"
        );
        p.copy_node(window, cx);
    });
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "\"last\":\"NEEDLE\""
        )
    });
    assert_eq!(std::fs::read(&path).unwrap(), source.as_bytes());
}

#[gpui::test]
fn json2794_tree_find_unfolds_ancestors_and_keyboard_search_covers_the_whole_file(
    cx: &mut TestAppContext,
) {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("api.json");
    let values = "\"public OpenAPI fixture value\",".repeat(125_000);
    let source = format!(
        "{{\"components\":[{}],\"tail\":{{\"nested\":{{\"marker\":\"NEEDLE\"}}}}}}",
        values.trim_end_matches(',')
    );
    assert!(source.len() > 3_500_000);
    std::fs::write(&path, &source).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
    });
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            CodePreview::new(
                fixture.path().to_owned(),
                "api.json".into(),
                path.clone(),
                window,
                cx,
            )
        });
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("code-view-menu").is_some(),
        "Tree is available for the complete Pretty snapshot"
    );
    preview.read_with(visual, |preview, _| {
        assert_eq!(
            preview.availability(View::Tree),
            Availability::Available,
            "the menu's existence alone cannot prove Tree availability"
        );
    });
    preview.update_in(visual, |p, window, cx| p.set_view(View::Raw, window, cx));
    visual.run_until_parked();
    preview.update_in(visual, |p, window, cx| {
        p.show_page(0, window, cx);
        p.input()
            .unwrap()
            .update(cx, |input, cx| input.focus(window, cx));
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-f");
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-f");
    visual.run_until_parked();
    preview.update_in(visual, |p, window, cx| {
        assert!(
            p.find.read(cx).focus_handle(cx).is_focused(window),
            "real shortcut focuses whole-file Find, not the page editor"
        );
    });
    visual.simulate_input("NEEDLE");
    visual.run_until_parked();
    preview.read_with(visual, |p, _| {
        assert_eq!(p.matches.len(), 1);
        let shared = p.document_snapshot().unwrap();
        assert!(Arc::ptr_eq(&shared, p.document.as_ref().unwrap()));
        assert_eq!(
            shared.row_at(&p.matches[0]),
            shared.structure.len().checked_sub(1)
        );
        assert!(
            p.page > 100,
            "positive control: Source reached the footer beyond the first page"
        );
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    preview.update_in(visual, |p, window, cx| p.set_view(View::Tree, window, cx));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("code-part-scrubber").is_none(),
        "Tree has no byte-page controls"
    );
    assert!(
        visual.debug_bounds("data-tree-match-label").is_some(),
        "matching node is scrolled into the actual viewport"
    );
    // Tree controls must leave a mounted keyboard target: the hidden Source
    // editor can no longer own focus after folding or view changes.
    preview.update_in(visual, |p, window, cx| {
        p.set_view(View::Raw, window, cx);
        p.show_match(window, cx);
    });
    preview.update_in(visual, |p, window, cx| {
        p.input()
            .unwrap()
            .update(cx, |input, cx| input.focus(window, cx));
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    preview.update_in(visual, |p, window, cx| p.set_view(View::Tree, window, cx));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let fold = visual.debug_bounds("code-fold-all").unwrap();
    visual.simulate_click(fold.center(), Modifiers::default());
    visual.run_until_parked();
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-f");
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-f");
    preview.update_in(visual, |p, window, cx| {
        assert!(p.find.read(cx).focus_handle(cx).is_focused(window));
        p.show_match(window, cx);
    });
    preview.update_in(visual, |p, window, cx| {
        assert!(p.view == View::Tree);
        let doc = p.document.as_ref().unwrap();
        let index = p.structure.match_node.unwrap();
        assert!(doc.row_label(index).contains("NEEDLE"));
        let mut parent = doc.structure[index].parent;
        while let Some(index) = parent {
            assert!(p.structure.visible.contains(&index));
            assert!(!p.structure.folded(index));
            parent = doc.structure[index].parent;
        }
        assert!(
            p.structure.visible.len() < 10,
            "only the hit path is expanded, not 125,000 array items"
        );
        p.copy_node(window, cx);
    });
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "\"marker\":\"NEEDLE\""
        )
    });
    preview.update_in(visual, |p, window, cx| {
        p.set_view(View::Formatted, window, cx)
    });
    visual.run_until_parked();
    preview.read_with(visual, |p, _| {
        assert!(
            p.view == View::Formatted,
            "Formatted is the authoritative selected view"
        );
        assert_eq!(p.document.as_ref().unwrap().pages.len(), 1);
        assert!(p.structure.selected.is_some());
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    preview.update_in(visual, |p, window, cx| {
        p.set_view(View::Raw, window, cx);
        p.show_match(window, cx);
    });
    visual.run_until_parked();
    preview.read_with(visual, |p, cx| {
        assert!(p.view != View::Tree);
        let input = p.input().unwrap().read(cx);
        assert_eq!(
            &input.value()[input.selected_range()],
            "NEEDLE",
            "Source retains the hit from Tree even without byte pages"
        );
    });
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[gpui::test]
fn data_view_2669_native_32_mib_multiline_file_keeps_full_find_and_numbered_editor(
    cx: &mut TestAppContext,
) {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("big.txt");
    let line = format!("{}\n", "abcdefgh".repeat(31));
    let source = format!("{}NEEDLE\n", line.repeat(32 * 1024 * 1024 / line.len()));
    std::fs::write(&path, &source).unwrap();
    cx.update(crate::test_init);
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            CodePreview::new(
                fixture.path().to_owned(),
                "big.txt".into(),
                path.clone(),
                window,
                cx,
            )
        });
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    let started = Instant::now();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let draw = started.elapsed();
    diagnostic_error!(
        "DATA_VIEW_2669 multiline_bytes={} first_draw_ms={}",
        source.len(),
        draw.as_millis()
    );
    assert!(
        draw < std::time::Duration::from_secs(2),
        "multi-line first draw: {draw:?}"
    );
    preview.update_in(visual, |p, _, cx| {
        assert!(
            !p.editable(),
            "CodePreview UI uses the shared 4 MiB editing cap; FileEditor remains independently bounded"
        );
        assert_eq!(p.document.as_ref().unwrap().pages.len(), 1);
        p.input().unwrap().update(cx, |input, cx| {
            assert!(!input.is_editable());
            assert_eq!(input.text().len(), source.len());
            input.set_search_query("NEEDLE", false, cx);
            assert_eq!(input.search_session().matcher.matched_ranges().len(), 1);
            assert_eq!(
                input.search_session().matcher.matched_ranges()[0].start,
                source.find("NEEDLE").unwrap()
            );
        });
    });
    assert_eq!(std::fs::read(&path).unwrap(), source.as_bytes());
}

#[gpui::test]
fn data_view_2669_native_standalone_xml_mounts_and_restores_a_preview_without_markdown_or_editing(
    cx: &mut TestAppContext,
) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    let source = "<root><value>read-only</value></root>\n";
    let path = root.join("data.xml");
    std::fs::write(&path, source).unwrap();
    cx.update(crate::test_init);
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    open_path: Some(path.clone()),
                    single_file: true,
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let reader = reader.unwrap();
    visual.run_until_parked();
    reader.update_in(visual, |reader, window, cx| {
        assert!(reader.single_file);
        assert_eq!(reader.selected_file(), "data.xml");
        let input = reader
            .code_view_input(cx)
            .expect("direct XML is not projected as Markdown");
        assert_eq!(input.read(cx).value().as_ref(), source);
        assert!(!input.read(cx).is_editable());
        assert!(reader.editing.is_none());
        assert!(!reader.code_file_editable(cx));
        // This must not widen the existing writable-format gate.
        reader.toggle_source(window, cx);
        assert!(reader.editing.is_none());
        reader.record_usable_document(cx);
    });
    assert_eq!(std::fs::read(&path).unwrap(), source.as_bytes());
}

#[gpui::test]
fn json2787_multi_megabyte_pretty_raw_toggles_keep_complete_valid_snapshot(
    cx: &mut TestAppContext,
) {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("__test2.json");
    let source = format!(
        "{{\"openapi\":\"3.0.4\",\"paths\":[{}],\"tail\":\"NEEDLE\"}}",
        "{\"description\":\"public fixture\"},"
            .repeat(120_000)
            .trim_end_matches(',')
    );
    assert!(source.len() > 3_500_000);
    std::fs::write(&path, &source).unwrap();
    cx.update(crate::test_init);
    let mut preview = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            CodePreview::new(
                fixture.path().to_owned(),
                "__test2.json".into(),
                path.clone(),
                window,
                cx,
            )
        });
        preview = Some(view.clone());
        Root::new(cx.new(|_| Surface(view)), window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    for round in 0..20 {
        preview.update_in(visual, |p, window, cx| {
            p.set_view(
                if round % 2 == 0 {
                    View::Formatted
                } else {
                    View::Raw
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        preview.read_with(visual, |p, _| {
            assert!(
                p.table.is_none(),
                "object JSON must never start an array table reader"
            );
            let json = p.json.as_ref().unwrap();
            assert!(matches!(json.pretty, Some(Ok(_))));
            assert_eq!(p.view == View::Formatted, round % 2 == 0);
            let doc = p.document.as_ref().unwrap();
            assert!(okilum_core::code_operations::json_validation(&doc.text).is_ok());
            assert_eq!(doc.find("NEEDLE").len(), 1);
            assert!(doc.find("NEEDLE")[0].start > 32 * 1024);
        });
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}
