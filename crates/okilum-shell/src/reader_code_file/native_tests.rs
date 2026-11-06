use super::*;
use ::core::prelude::v1::test;

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
        Root::new(view, window, cx)
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
    let fold = visual.debug_bounds("data-fold-all").expect("Fold all");
    visual.simulate_click(fold.center(), Modifiers::default());
    visual.run_until_parked();
    input.read_with(visual, |s, _| {
        assert!(
            !s.folded_ranges().is_empty(),
            "native XML grammar supplies folds"
        )
    });
    let unfold = visual.debug_bounds("data-unfold-all").expect("Unfold all");
    visual.simulate_click(unfold.center(), Modifiers::default());
    visual.run_until_parked();
    input.update(visual, |s, cx| {
        assert!(s.folded_ranges().is_empty());
        s.set_selected_range(
            source.find("שלום").unwrap()..source.find("שלום").unwrap(),
            cx,
        );
    });
    let copy = visual.debug_bounds("data-copy-node").expect("Copy node");
    visual.simulate_click(copy.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "<value>שלום</value>"
        )
    });
    input.update(visual, |s, cx| s.set_selected_range(0..6, cx));
    let copy = visual.debug_bounds("data-copy-selection").unwrap();
    visual.simulate_click(copy.center(), Modifiers::default());
    visual.run_until_parked();
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
        Root::new(view, window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    let started = Instant::now();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let draw = started.elapsed();
    eprintln!(
        "DATA_VIEW_2669 minified_bytes={} first_draw_ms={}",
        source.len(),
        draw.as_millis()
    );
    assert!(
        draw < std::time::Duration::from_secs(2),
        "bounded page first draw: {draw:?}"
    );
    // Draw the real Raw handoff before its replacement AST is available.
    preview.update_in(visual, |p, window, cx| {
        assert!(p.structure_view);
        p.show_json(false, window, cx);
        assert!(!p.structure_view);
        assert!(p.document.is_none());
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    let fold = visual
        .debug_bounds("data-fold-all")
        .expect("Fold all for the complete minified AST");
    visual.simulate_click(fold.center(), Modifiers::default());
    visual.run_until_parked();
    preview.read_with(visual, |p, _| {
        assert!(p.structure_view);
        assert_eq!(p.structure.visible, vec![0]);
    });
    let unfold = visual.debug_bounds("data-unfold-all").unwrap();
    visual.simulate_click(unfold.center(), Modifiers::default());
    visual.run_until_parked();
    preview.read_with(visual, |p, _| {
        assert_eq!(
            p.structure.visible.len(),
            p.document.as_ref().unwrap().structure.len()
        )
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
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
        assert!(!p.editable());
        assert!(p.input().unwrap().read(cx).value().len() <= 32 * 1024);
        assert!(p.document.as_ref().unwrap().pages.len() > 100);
        p.find.update(cx, |s, cx| s.set_value("needle", window, cx));
        p.search(window, cx);
    });
    visual.run_until_parked();
    preview.update_in(visual, |p, _, cx| {
        assert_eq!(p.matches.len(), 1);
        assert!(p.page > 0);
        assert_eq!(
            p.input().unwrap().read(cx).value()[p.input().unwrap().read(cx).selected_range()]
                .to_owned(),
            "NEEDLE"
        );
        p.copy_node(cx);
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
        Root::new(view, window, cx)
    });
    let preview = preview.unwrap();
    visual.run_until_parked();
    let started = Instant::now();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let draw = started.elapsed();
    eprintln!(
        "DATA_VIEW_2669 multiline_bytes={} first_draw_ms={}",
        source.len(),
        draw.as_millis()
    );
    assert!(
        draw < std::time::Duration::from_secs(2),
        "multi-line first draw: {draw:?}"
    );
    preview.update_in(visual, |p, _, cx| {
        assert!(!p.editable());
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
