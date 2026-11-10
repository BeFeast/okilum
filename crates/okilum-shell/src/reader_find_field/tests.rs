use super::editor::EditorTarget;
use super::*;

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
        visual.debug_bounds("search-panel").is_none(),
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
    let before = ids.map(|id| visual.debug_bounds(id).unwrap());
    let field = visual.debug_bounds("viewer-find-field").unwrap();
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-f"
    } else {
        "ctrl-f"
    });
    visual.run_until_parked();
    let focused = visual.debug_bounds("viewer-find-field").unwrap();
    assert_eq!(field.right(), focused.right());
    assert!(focused.left() < field.left());
    for (id, bounds) in ids.into_iter().zip(before) {
        assert_eq!(visual.debug_bounds(id).unwrap(), bounds, "{id} stays put");
    }
    for id in ["viewer-find-field", "find-close", "canvas-read-only"] {
        if let Some(bounds) = visual.debug_bounds(id) {
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
    let mark = visual.debug_bounds("archive-mark-0").unwrap();
    visual.simulate_click(mark.center(), Modifiers::default());
    visual.run_until_parked();
    let actions = reader.read_with(visual, |reader, cx| reader.viewer_chrome(cx).actions);
    let selected = actions
        .iter()
        .find(|a| a.id == "archive-extract-selected")
        .unwrap();
    assert!(selected.enabled);
    assert_eq!(selected.label.as_ref(), "Extract 1 selected…");
    let row = visual.debug_bounds("archive-row-0").unwrap();
    visual.simulate_click(row.center(), Modifiers::default());
    visual.run_until_parked();
    let pane = visual.debug_bounds("archive-entry-bar").unwrap();
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
    visual.simulate_resize(size(px(700.), px(800.)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("archive-extract-all").is_some());
}
