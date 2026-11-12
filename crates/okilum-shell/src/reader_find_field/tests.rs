use super::editor::EditorTarget;
use super::*;
use ::core::prelude::v1::test;
use reader_viewer_chrome::tests::measured_bounds;

#[gpui::test]
fn canvas_state_badge_is_visible_next_to_the_title_at_both_widths(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (_reader, visual, _temp) = reader_viewer_chrome::tests::fixture(cx, "scene.canvas");
    for width in [1440., 700.] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        let title = measured_bounds(visual, "reader-document-root").unwrap();
        let name = measured_bounds(visual, "viewer-name-slot").unwrap();
        let badge = measured_bounds(visual, "canvas-read-only").unwrap();
        assert!(badge.left() >= title.right());
        assert!(
            badge.left() - title.right() <= px(8.),
            "state badge stays next to the title"
        );
        assert!(
            badge.right() <= name.right(),
            "state badge is not clipped by the name slot"
        );
        assert!(badge.size.width >= px(28.) && badge.size.height >= px(28.));
        let minus = measured_bounds(visual, "canvas-minus").unwrap();
        let percentage = measured_bounds(visual, "viewer-action-text").unwrap();
        let plus = measured_bounds(visual, "canvas-plus").unwrap();
        assert!(
            minus.right() <= percentage.left() && percentage.right() <= plus.left(),
            "Canvas percentage belongs between Zoom out and Zoom in"
        );
    }
}

#[gpui::test]
fn header_field_renders_200_128_and_icon_but_large_files_keep_a_field(cx: &mut TestAppContext) {
    struct Field {
        target: Target,
        width: f32,
        focused: bool,
    }
    impl Render for Field {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            h_flex().size_full().child(render(
                &self.target,
                Presentation::Header {
                    width: self.width,
                    focused: self.focused,
                    max_width: f32::MAX,
                },
                cx,
            ))
        }
    }
    cx.update(crate::test_init);
    let (field, visual) = cx.add_window_view(|window, cx| Field {
        target: Target {
            input: cx.new(|cx| InputState::new(window, cx).placeholder("Find in entries")),
            focus_override: None,
            scope: "Find in entries".into(),
            count: None,
            match_case: None,
            step: None,
            steps_visible: false,
            close: std::rc::Rc::new(|_, _| {}),
            large_file: false,
        },
        width: 200.,
        focused: false,
    });
    for (width, large, expected) in [
        (200., false, Some(200.)),
        (128., false, Some(128.)),
        (28., false, None),
        (28., true, Some(128.)),
        (128., true, Some(128.)),
        (200., true, Some(200.)),
    ] {
        field.update(visual, |field, cx| {
            field.width = width;
            field.target.large_file = large;
            cx.notify();
        });
        visual.run_until_parked();
        if let Some(expected) = expected {
            let bounds = measured_bounds(visual, "viewer-find-field").unwrap();
            assert_eq!(bounds.size, size(px(expected), px(28.)));
            assert!(measured_bounds(visual, "viewer-find").is_none());
        } else {
            assert!(measured_bounds(visual, "viewer-find-field").is_none());
            let icon = measured_bounds(visual, "viewer-find").unwrap();
            assert!(icon.size.width >= px(28.) && icon.size.height >= px(28.));
        }
    }
    field.update_in(visual, |field, window, cx| {
        field.width = 272.;
        field.focused = true;
        field.target.match_case = Some((false, std::rc::Rc::new(|_, _| {})));
        field.target.step = Some(std::rc::Rc::new(|_, _, _| {}));
        field.target.steps_visible = true;
        field
            .target
            .input
            .update(cx, |input, cx| input.set_value("query", window, cx));
        cx.notify();
    });
    visual.run_until_parked();
    for id in [
        "find-case-sensitive",
        "find-prev",
        "find-next",
        "find-close",
    ] {
        let bounds = measured_bounds(visual, id).expect("required header-form Find control");
        assert!(
            bounds.size.width >= px(28.) && bounds.size.height >= px(28.),
            "{id} has a full-height hit area"
        );
    }
}

#[gpui::test]
fn external_editor_target_counts_steps_unfolds_and_clears_without_source_edits(
    cx: &mut TestAppContext,
) {
    let (reader, visual, _temp) = reader_viewer_chrome::tests::fixture(cx, "source.rs");
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
    visual.run_until_parked();
    let input = reader.read_with(visual, |reader, _| {
        reader.editing.as_ref().unwrap().input().clone()
    });
    input.update_in(visual, |input, window, cx| {
        input.set_value(
            "fn first() {\n    target();\n    target();\n}\nfn second() {\n    TARGET();\n}\n",
            window,
            cx,
        );
        input.set_folding(true, window, cx);
        input.apply_highlighter_fold_candidates(
            vec![
                gpui_component::input::FoldRange::new(0, 3),
                gpui_component::input::FoldRange::new(4, 6),
            ],
            cx,
        );
        input.set_all_folded(true, cx);
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
    visual.run_until_parked();
    input.update(visual, |input, cx| input.open_search(false, cx));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
    visual.run_until_parked();
    assert!(
        measured_bounds(visual, "search-panel").is_some(),
        "positive control: the real toolkit strip mounts"
    );
    input.update(visual, |input, cx| input.close_search(cx));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
    visual.run_until_parked();
    let stamp = input.read_with(visual, |input, _| input.source_stamp());
    let target =
        visual.update(|window, cx| cx.new(|cx| EditorTarget::new(input.clone(), window, cx)));
    target.update_in(visual, |target, window, cx| {
        target.focus(window, cx);
        target.set_query("target", window, cx);
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
    visual.run_until_parked();
    target.read_with(visual, |target, cx| assert_eq!(target.count(cx).1, 3));
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
            1,
            "the first hit is unfolded on query completion"
        );
    });
    target.update(visual, |target, cx| target.step(1, cx));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
    visual.run_until_parked();
    assert_eq!(target.read_with(visual, |target, cx| target.count(cx).0), 1);
    assert_eq!(
        input.read_with(visual, |input, _| input.folded_ranges().len()),
        1
    );
    target.update(visual, |target, cx| target.step(-1, cx));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
    visual.run_until_parked();
    assert_eq!(
        target.read_with(visual, |target, cx| target.count(cx).0),
        0,
        "previous goes backwards, not forwards to match 2"
    );
    target.update(visual, |target, cx| target.step(-1, cx));
    assert_eq!(
        target.read_with(visual, |target, cx| target.count(cx).0),
        2,
        "previous wraps from zero to the last of three matches"
    );
    visual.run_until_parked();
    assert_eq!(
        input.read_with(visual, |input, _| input.folded_ranges().len()),
        0,
        "the wrapped step must unfold the second region"
    );
    target.update(visual, |target, cx| target.set_case_sensitive(true, cx));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
    visual.run_until_parked();
    target.read_with(visual, |target, cx| assert_eq!(target.count(cx).1, 2));
    assert!(
        measured_bounds(visual, "search-panel").is_none(),
        "toolkit strip stays absent"
    );
    target.update_in(visual, |target, window, cx| target.clear(window, cx));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(70));
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
    for id in ["viewer-find-field", "find-close"] {
        let bounds = measured_bounds(visual, id).expect("required ZIP Find control");
        assert!(bounds.size.width >= px(28.) && bounds.size.height >= px(28.));
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
    assert!(
        target
            .input
            .read_with(visual, |input, _| input.value().is_empty())
    );
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
    let search_before = archive
        .read_with(visual, |archive, cx| archive.code_entry_search_snapshot(cx))
        .expect("nested Ctrl+F opens the entry's actual search");
    visual.simulate_input("test");
    visual.run_until_parked();
    let search_after = archive
        .read_with(visual, |archive, cx| archive.code_entry_search_snapshot(cx))
        .unwrap();
    assert_eq!(
        search_after.0, "test",
        "typing reaches the entry Find field"
    );
    assert_eq!(
        search_after.1, search_before.1,
        "Find never edits the entry"
    );
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
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("Drawing loaded successfully")
        )
    });
    let open = reader.read_with(visual, |reader, cx| {
        reader
            .viewer_chrome(cx)
            .open_actions
            .into_iter()
            .find(|action| action.id == "drawing-open")
            .expect("the file offers Open in Excalidraw")
    });
    assert_eq!(open.label.to_string(), "Open in Excalidraw");
    visual.update(|window, cx| (open.invoke)(window, cx));
    assert_eq!(
        visual.opened_url().as_deref(),
        Some("https://draw.oklabs.uk")
    );
    visual.update(|_, cx| {
        let copied = cx.read_from_clipboard().unwrap().text().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&copied).unwrap(),
            serde_json::from_slice::<serde_json::Value>(&before).unwrap(),
            "Open in Excalidraw copies the loaded scene"
        );
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

#[gpui::test]
fn drawing_details_remains_actionable_from_overflow(cx: &mut TestAppContext) {
    use gpui_component::{WindowExt as _, menu::PopupMenuItem};
    cx.executor().allow_parking();
    let (reader, visual, _temp) = reader_viewer_chrome::tests::fixture(cx, "drawing.excalidraw");
    let started = std::time::Instant::now();
    let info = loop {
        visual.run_until_parked();
        let info = reader.read_with(visual, |reader, cx| {
            reader
                .viewer_chrome(cx)
                .actions
                .into_iter()
                .find(|a| a.id == "drawing-diagnostic-details")
        });
        if let Some(info) = info {
            break info;
        }
        assert!(started.elapsed() < Duration::from_secs(30));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        std::thread::sleep(Duration::from_millis(10));
    };
    let PopupMenuItem::Item {
        handler: Some(handler),
        ..
    } = info.menu_item()
    else {
        panic!("details is an actionable overflow item");
    };
    visual.update(|window, cx| handler(&ClickEvent::default(), window, cx));
    visual.run_until_parked();
    assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
    assert!(
        measured_bounds(visual, "drawing-details").is_some(),
        "overflow exposes the drawing details"
    );
}
