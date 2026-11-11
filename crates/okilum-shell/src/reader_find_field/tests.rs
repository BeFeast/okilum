use super::editor::EditorTarget;
use super::*;
use ::core::prelude::v1::test;
use reader_viewer_chrome::tests::measured_bounds;

#[gpui::test]
fn external_editor_target_counts_steps_unfolds_and_clears_without_source_edits(
    cx: &mut TestAppContext,
) {
    let (reader, visual, _temp) = reader_viewer_chrome::tests::fixture(cx, "source.rs");
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    let input = reader.read_with(visual, |reader, _| {
        reader.editing.as_ref().unwrap().input().clone()
    });
    input.update_in(visual, |input, window, cx| {
        input.set_value(
            "fn first() {\n    target();\n}\nfn second() {\n    TARGET();\n}\n",
            window,
            cx,
        );
        input.set_folding(true, window, cx);
        input.apply_highlighter_fold_candidates(
            vec![
                gpui_component::input::FoldRange::new(0, 2),
                gpui_component::input::FoldRange::new(3, 5),
            ],
            cx,
        );
        input.set_all_folded(true, cx);
    });
    visual.run_until_parked();
    let stamp = input.read_with(visual, |input, _| input.source_stamp());
    let target =
        visual.update(|window, cx| cx.new(|cx| EditorTarget::new(input.clone(), window, cx)));
    target.update_in(visual, |target, window, cx| {
        target.focus(window, cx);
        target.set_query("target", window, cx);
    });
    visual.run_until_parked();
    target.read_with(visual, |target, cx| assert_eq!(target.count(cx).1, 2));
    let descriptor = visual.update(|_, cx| EditorTarget::target(&target, cx));
    assert_eq!(descriptor.scope.to_string(), "Find");
    assert!(descriptor.match_case.is_some() && descriptor.step.is_some());
    assert_eq!(
        descriptor.input,
        target.read_with(visual, |target, _| target.query.clone())
    );
    input.read_with(visual, |input, _| {
        assert!(input.search_session().external_presentation);
        assert!(!input.search_session().open);
        assert_eq!(
            input.folded_ranges().len(),
            2,
            "positive control: matches start folded"
        );
    });
    target.update(visual, |target, cx| target.step(1, cx));
    visual.run_until_parked();
    let forward = target.read_with(visual, |target, cx| target.count(cx).0);
    assert!(input.read_with(visual, |input, _| input.folded_ranges().len()) < 2);
    target.update(visual, |target, cx| target.step(-1, cx));
    visual.run_until_parked();
    assert_ne!(
        target.read_with(visual, |target, cx| target.count(cx).0),
        forward
    );
    target.update(visual, |target, cx| target.set_case_sensitive(true, cx));
    target.read_with(visual, |target, cx| assert_eq!(target.count(cx).1, 1));
    assert!(
        measured_bounds(visual, "search-panel").is_none(),
        "toolkit strip stays absent"
    );
    target.update_in(visual, |target, window, cx| target.clear(window, cx));
    visual.run_until_parked();
    input.read_with(visual, |input, _| {
        assert!(!input.search_session().external_presentation);
        assert!(input.search_session().matcher.matched_ranges().is_empty());
        assert_eq!(input.source_stamp(), stamp);
    });
    visual.update(|window, cx| assert!(input.read(cx).focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn archive_header_find_expands_left_without_moving_other_controls_or_content(
    cx: &mut TestAppContext,
) {
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "note.md");
    let path = temp.path().join("vault/archive.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    zip.start_file("package.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    std::io::Write::write_all(&mut zip, b"{\"name\":\"test\"}").unwrap();
    zip.finish().unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("archive.zip", window, cx)
    });
    visual.run_until_parked();
    let ids = [
        "viewer-copy",
        "viewer-open-with",
        "document-more",
        "archive-view",
    ];
    let before = ids.map(|id| measured_bounds(visual, id).unwrap());
    let field = measured_bounds(visual, "viewer-find-field").unwrap();
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-f"
    } else {
        "ctrl-f"
    });
    visual.run_until_parked();
    let focused = measured_bounds(visual, "viewer-find-field").unwrap();
    assert_eq!(field.right(), focused.right());
    assert!(focused.left() < field.left());
    for (id, bounds) in ids.into_iter().zip(before) {
        assert_eq!(
            measured_bounds(visual, id).unwrap(),
            bounds,
            "{id} stays put"
        );
    }
    for id in ["viewer-find-field", "find-close", "canvas-read-only"] {
        if let Some(bounds) = measured_bounds(visual, id) {
            assert!(bounds.size.height >= px(28.));
        }
    }
    let target = reader.read_with(visual, |reader, cx| reader.viewer_chrome(cx).find.unwrap());
    visual.update(|window, cx| assert!(target.input.read(cx).focus_handle(cx).is_focused(window)));
    visual.update(|window, cx| target.set_query("package", window, cx));
    visual.run_until_parked();
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        target
            .input
            .read_with(visual, |input, _| input.value().to_string()),
        "package"
    );
    reader.update_in(visual, |reader, window, cx| reader.open_find(window, cx));
    visual.run_until_parked();
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(target
        .input
        .read_with(visual, |input, _| input.value().is_empty()));
    visual.update(|window, cx| assert!(!target.input.read(cx).focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn archive_extract_enablement_and_entry_focus_follow_the_hosted_pane(cx: &mut TestAppContext) {
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "note.md");
    let mut zip =
        zip::ZipWriter::new(std::fs::File::create(temp.path().join("vault/archive.zip")).unwrap());
    zip.start_file("package.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    std::io::Write::write_all(&mut zip, b"{\"name\":\"test\"}").unwrap();
    zip.finish().unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("archive.zip", window, cx)
    });
    visual.run_until_parked();
    let actions = reader.read_with(visual, |reader, cx| reader.viewer_chrome(cx).actions);
    assert!(
        !actions
            .iter()
            .find(|a| a.id == "archive-extract-selected")
            .unwrap()
            .enabled
    );
    let all = actions
        .iter()
        .find(|a| a.id == "archive-extract-all")
        .unwrap();
    assert!(all.enabled && all.filled && all.never_dropped);
    let mark = measured_bounds(visual, "archive-mark-0").unwrap();
    visual.simulate_click(mark.center(), Modifiers::default());
    visual.run_until_parked();
    let actions = reader.read_with(visual, |reader, cx| reader.viewer_chrome(cx).actions);
    let selected = actions
        .iter()
        .find(|a| a.id == "archive-extract-selected")
        .unwrap();
    assert!(selected.enabled);
    assert_eq!(selected.label.to_string(), "Extract 1 selected…");
    let row = measured_bounds(visual, "archive-row-0").unwrap();
    visual.simulate_click(row.center(), Modifiers::default());
    visual.run_until_parked();
    let pane = measured_bounds(visual, "archive-entry-bar").unwrap();
    visual.simulate_click(pane.center(), Modifiers::default());
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-f"
    } else {
        "ctrl-f"
    });
    visual.run_until_parked();
    let target = reader.read_with(visual, |reader, cx| reader.viewer_chrome(cx).find.unwrap());
    visual.update(|window, cx| {
        assert!(
            !target.input.read(cx).focus_handle(cx).is_focused(window),
            "entry Find must not focus the archive filter"
        )
    });
    let archive = reader.read_with(visual, |reader, _| {
        reader
            .file_preview
            .as_ref()
            .unwrap()
            .archive
            .clone()
            .unwrap()
    });
    visual.update(|window, cx| {
        assert!(
            archive.read(cx).code_entry_find_is_focused(window, cx),
            "nested Ctrl+F opens and focuses the entry's actual search"
        )
    });
    // The keyboard shortcut follows entry focus; the archive's own compact
    // Find icon still opens the archive filter when clicked.
    let mut compact = None;
    for width in [520., 400.] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        compact = measured_bounds(visual, "viewer-find");
        if compact.is_some() {
            break;
        }
    }
    let compact = compact.expect("narrow archive Find becomes an icon");
    visual.simulate_click(compact.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|window, cx| {
        assert!(target.input.read(cx).focus_handle(cx).is_focused(window));
    });
    assert!(measured_bounds(visual, "viewer-find-field").is_some());
    visual.simulate_resize(size(px(700.), px(800.)));
    visual.run_until_parked();
    assert!(measured_bounds(visual, "archive-extract-all").is_some());
}

#[gpui::test]
fn standalone_image_copy_menu_copies_png_pixels_without_changing_the_file(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (_reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "image.png");
    let path = temp.path().join("vault/image.png");
    let before = std::fs::read(&path).unwrap();
    visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
    let button = measured_bounds(visual, "viewer-copy").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    visual.simulate_keystrokes("down enter");
    visual.run_until_parked();
    visual.update(|_, cx| {
        let item = cx.read_from_clipboard().unwrap();
        let png = item
            .entries()
            .iter()
            .find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(&image.bytes),
                _ => None,
            })
            .expect("Copy image uses the image clipboard representation");
        let decoded = image::load_from_memory(png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (1, 1));
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
    });
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[gpui::test]
fn file_drawing_actions_use_the_loaded_drawing_without_an_inline_toolbar(cx: &mut TestAppContext) {
    use gpui_component::WindowExt as _;
    cx.executor().allow_parking();
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "drawing.excalidraw");
    let before = std::fs::read(temp.path().join("vault/drawing.excalidraw")).unwrap();
    let started = std::time::Instant::now();
    loop {
        visual.run_until_parked();
        if reader.read_with(visual, |reader, cx| {
            reader
                .viewer_chrome(cx)
                .actions
                .iter()
                .any(|action| action.id == "drawing-expand")
        }) {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "drawing fixture never loaded"
        );
        visual.update(|window, cx| window.draw(cx).clear(cx));
        std::thread::sleep(Duration::from_millis(10));
    }
    let header = measured_bounds(visual, "document-header-viewport").unwrap();
    let expand = measured_bounds(visual, "drawing-expand").unwrap();
    let details = measured_bounds(visual, "drawing-diagnostic-details").unwrap();
    for bounds in [expand, details] {
        assert!(bounds.top() >= header.top() && bounds.bottom() <= header.bottom());
        assert!(bounds.size.width >= px(28.) && bounds.size.height >= px(28.));
    }
    // These two commands live only in menus for a file; inline note drawings
    // keep their existing command row.
    assert!(measured_bounds(visual, "drawing-open").is_none());
    assert!(measured_bounds(visual, "drawing-copy-details").is_none());
    visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
    let copy = measured_bounds(visual, "viewer-copy").unwrap();
    visual.simulate_click(copy.center(), Modifiers::default());
    visual.run_until_parked();
    visual.simulate_keystrokes("down enter");
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert_ne!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("sentinel")
        )
    });
    let expand = measured_bounds(visual, "drawing-expand").unwrap();
    visual.simulate_click(expand.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
    assert_eq!(
        std::fs::read(temp.path().join("vault/drawing.excalidraw")).unwrap(),
        before
    );
}
