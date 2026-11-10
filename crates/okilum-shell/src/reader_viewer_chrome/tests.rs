use super::*;
use ::core::prelude::v1::test;
use gpui_component::WindowExt as _;

pub(crate) fn fixture<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
) -> (Entity<Reader>, &'a mut VisualTestContext, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("Folder/A long parent folder name")).unwrap();
    for (name, value) in [
        ("data.json", "{\"a\":1,\"a\":2e2}\r\n"),
        ("data.csv", "name,value\r\none,1\r\n"),
        ("source.rs", "fn main() {}\n"),
        ("text.txt", "hello שלום\r\n"),
        ("Folder/text.txt", "nested text\n"),
        ("Folder/A long parent folder name/file.txt", "nested text\n"),
        ("gateway.log", "2026-11-10T12:00:00Z INFO ready\n"),
        ("note.md", "# Note\n\nBody\n"),
        (
            "Folder/A very long attachment name that must keep a usable title.txt",
            "text\n",
        ),
        ("scene.canvas", "{\"nodes\":[],\"edges\":[]}"),
        (
            "drawing.excalidraw",
            r##"{"type":"excalidraw","version":2,"elements":[{"id":"tall","type":"rectangle","x":0,"y":0,"width":600,"height":4000,"strokeColor":"#000000","backgroundColor":"#0969e8","fillStyle":"solid","seed":42,"roughness":0}],"appState":{},"files":{}}"##,
        ),
    ] {
        std::fs::write(root.join(name), value).unwrap();
    }
    std::fs::write(root.join("image.svg"), "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"600\" height=\"4000\"><rect width=\"600\" height=\"4000\" fill=\"blue\"/></svg>").unwrap();
    image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
        .save(root.join("image.png"))
        .unwrap();
    // The production PDF fixture is already covered by the PDF tests and the
    // native Reader fixture; a minimal page is enough for header action tests.
    std::fs::write(
        root.join("document.pdf"),
        crate::pdf_engine::fixture::pdf(&[(200., 4000., [1., 1., 1.])]),
    )
    .unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_ui_state::install(&temp.path().join("state"), cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root),
                    note: Some(name.into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("session")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1100.), px(800.)));
    visual.run_until_parked();
    let reader = reader.unwrap();
    reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), name));
    (reader, visual, temp)
}

pub(crate) fn measured_bounds(
    visual: &mut VisualTestContext,
    selector: &'static str,
) -> Option<Bounds<Pixels>> {
    // GPUI replays cached paint/hitboxes but clears its test-only debug_bounds
    // map without replaying it. Measure a real fresh frame; all visibility,
    // size and click assertions remain unchanged.
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    visual.debug_bounds(selector)
}

fn clipboard(visual: &mut VisualTestContext) -> Option<String> {
    visual.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

fn open_copy(visual: &mut VisualTestContext) {
    let button_bounds = measured_bounds(visual, "viewer-copy").expect("Copy menu is mounted");
    visual.simulate_click(button_bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

#[gpui::test]
fn text_copy_menu_copies_disk_bytes_and_unsaved_buffer_without_writing(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(cx, "data.json");
    for name in [
        "data.json",
        "data.csv",
        "source.rs",
        "text.txt",
        "gateway.log",
    ] {
        reader.update_in(visual, |r, w, cx| r.preview_file(name, w, cx));
        visual.run_until_parked();
        let original = std::fs::read_to_string(temp.path().join("vault").join(name)).unwrap();
        visual.update(|w, cx| {
            w.clear_notifications(cx);
            cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()));
        });
        open_copy(visual);
        // Keyboard selection exercises the actual dropdown click handler.
        visual.simulate_keystrokes("down enter");
        visual.run_until_parked();
        assert_eq!(
            clipboard(visual).as_deref(),
            Some(original.as_str()),
            "{name} copies exact source, not the formatted display"
        );
        visual.update(|w, cx| {
            assert!(
                !w.notifications(cx).is_empty(),
                "copy confirms with a toast"
            )
        });
        assert_eq!(
            std::fs::read_to_string(temp.path().join("vault").join(name)).unwrap(),
            original
        );
    }
    reader.update_in(visual, |r, w, cx| r.preview_file("data.json", w, cx));
    visual.run_until_parked();
    let edit = measured_bounds(visual, "reader-edit").expect("loaded JSON can be edited");
    visual.simulate_click(edit.center(), Modifiers::default());
    visual.run_until_parked();
    reader.update_in(visual, |r, w, cx| {
        r.editing
            .as_ref()
            .unwrap()
            .set_value("{\"draft\":true}", w, cx)
    });
    visual.run_until_parked();
    // Keep focus in the editor: opening a menu may legitimately save on blur.
    // This isolates the copy command's no-write contract from that lifecycle.
    visual.update(|window, cx| window.dispatch_action(Box::new(CopyFileContent), cx));
    visual.run_until_parked();
    assert_eq!(clipboard(visual).as_deref(), Some("{\"draft\":true}"));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("vault/data.json")).unwrap(),
        "{\"a\":1,\"a\":2e2}\r\n"
    );
}

#[gpui::test]
fn binary_viewers_omit_content_and_path_items_act_on_the_open_file(cx: &mut TestAppContext) {
    // PDF has a real worker thread, as in the existing native PDF tests.
    cx.executor().allow_parking();
    let (reader, visual, temp) = fixture(cx, "image.png");
    for name in ["image.png", "document.pdf"] {
        reader.update_in(visual, |r, w, cx| {
            r.preview_file(name, w, cx);
            r.tree.cursor = Some("note.md".into());
            r.tree_focus.focus(w, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(!copy_content_available(r)));
        for (downs, expected) in [
            (
                1,
                okilum_core::vault::display_path(
                    &temp.path().join("vault").join(name).canonicalize().unwrap(),
                ),
            ),
            (2, name.to_string()),
            (3, format!("[[{name}|{}]]", Vault::title_of(name))),
        ] {
            open_copy(visual);
            visual.simulate_keystrokes(&format!("{}enter", "down ".repeat(downs)));
            visual.run_until_parked();
            assert_eq!(
                clipboard(visual),
                Some(expected),
                "content absent, correct file action at row {downs}"
            );
        }
    }
}

#[gpui::test]
fn file_header_never_wraps_and_preserves_title_edit_dirty_save_and_more(cx: &mut TestAppContext) {
    let name = "Folder/A very long attachment name that must keep a usable title.txt";
    let (reader, visual, _temp) = fixture(cx, name);
    for width in [1400., 700., 600., 1400.] {
        visual.simulate_resize(size(px(width), px(800.)));
        reader.update(visual, |r, cx| {
            r.panels.close(reader_layout::Panel::Notes);
            r.panels.close(reader_layout::Panel::Backlinks);
            cx.notify();
        });
        visual.run_until_parked();
        let header = measured_bounds(visual, "document-header-viewport").unwrap();
        assert_eq!(header.size.height, px(48.));
        let title = measured_bounds(visual, "reader-document-root").unwrap();
        assert!(
            title.size.width >= px(120.),
            "file name protected at {width}: {title:?}"
        );
        for id in ["reader-edit", "document-more"] {
            let b = measured_bounds(visual, id).unwrap();
            assert!(b.size.height >= px(28.) && b.size.width >= px(28.));
            assert!(b.left() >= header.left() && b.right() <= header.right());
            assert!(b.top() >= header.top() && b.bottom() <= header.bottom());
        }
        for obsolete in ["file-open", "file-reveal", "file-copy", "file-quicklook"] {
            assert!(measured_bounds(visual, obsolete).is_none());
        }
    }
    reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
    visual.run_until_parked();
    reader.update_in(visual, |r, w, cx| {
        r.editing.as_ref().unwrap().set_value("unsaved", w, cx)
    });
    visual.simulate_resize(size(px(600.), px(800.)));
    visual.run_until_parked();
    for width in [600., 1400., 700., 600.] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        let header = measured_bounds(visual, "document-header-viewport").unwrap();
        let read = measured_bounds(visual, "reader-read").unwrap();
        let save = measured_bounds(visual, "source-save").unwrap();
        let more = measured_bounds(visual, "document-more").unwrap();
        assert!(save.left() >= read.right(), "Save stays after Read");
        assert!(
            save.right() <= more.left(),
            "Save stays before the window actions"
        );
        assert!(save.top() >= header.top() && save.bottom() <= header.bottom());
    }
}

#[gpui::test]
fn optional_file_labels_yield_at_narrow_width_and_return_on_widening(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(cx, "source.rs");
    reader.update(visual, |reader, cx| {
        reader.panels.close(reader_layout::Panel::Notes);
        reader.panels.close(reader_layout::Panel::Backlinks);
        reader_ui_state::set_toolbar_labels(true, cx);
        cx.notify();
    });
    let ids = ["viewer-copy", "reader-edit", "viewer-open-with"];
    visual.simulate_resize(size(px(1600.), px(800.)));
    visual.run_until_parked();
    let labelled = ids.map(|id| measured_bounds(visual, id).unwrap().size.width);
    visual.simulate_resize(size(px(700.), px(800.)));
    visual.run_until_parked();
    for (id, wide) in ids.into_iter().zip(labelled) {
        assert!(
            measured_bounds(visual, id).unwrap().size.width < wide,
            "{id} label yields first"
        );
    }
    visual.simulate_resize(size(px(1600.), px(800.)));
    visual.run_until_parked();
    for (id, wide) in ids.into_iter().zip(labelled) {
        assert_eq!(
            measured_bounds(visual, id).unwrap().size.width,
            wide,
            "{id} label returns"
        );
    }
}

#[gpui::test]
fn file_header_mount_is_pinned_for_image_and_excalidraw_and_notes_keep_maximize(
    cx: &mut TestAppContext,
) {
    let (reader, visual, _temp) = fixture(cx, "image.svg");
    for name in ["image.svg", "drawing.excalidraw"] {
        reader.update_in(visual, |r, w, cx| r.preview_file(name, w, cx));
        visual.run_until_parked();
        let before = measured_bounds(visual, "document-header-viewport").unwrap();
        let marker = if name == "image.svg" {
            "reader-file-content"
        } else {
            "drawing-canvas-content"
        };
        // Asset loading starts from draw, not from Reader construction. Keep
        // the scroll control meaningful by waiting for the tall content itself.
        for _ in 0..20 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
            if measured_bounds(visual, marker).is_some_and(|bounds| {
                bounds.size.height
                    > measured_bounds(visual, "document-header-viewport")
                        .unwrap()
                        .size
                        .height
                        + px(800.)
            }) {
                break;
            }
        }
        let content_before = measured_bounds(visual, marker)
            .unwrap_or_else(|| panic!("scrollable content mounted for {name}: {marker}"));
        visual.simulate_event(ScrollWheelEvent {
            position: point(before.center().x, before.bottom() + px(100.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-800.))),
            ..Default::default()
        });
        visual.run_until_parked();
        assert!(
            measured_bounds(visual, marker).unwrap().top() < content_before.top(),
            "positive control: {name} content actually scrolled"
        );
        assert_eq!(
            measured_bounds(visual, "document-header-viewport").unwrap(),
            before
        );
    }
    reader.update_in(visual, |r, w, cx| r.open_note("note.md", None, w, cx));
    visual.run_until_parked();
    for width in [480., 700., 1400.] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        assert!(
            measured_bounds(visual, "reader-maximize-note").is_some(),
            "note Maximize stays pinned"
        );
        assert!(measured_bounds(visual, "viewer-copy").is_none());
    }
}

#[gpui::test]
fn pdf_header_buttons_and_erased_overflow_callbacks_zoom_the_active_pdf(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (reader, visual, _temp) = fixture(cx, "document.pdf");
    let pdf = reader.read_with(visual, |r, _| {
        r.file_preview.as_ref().unwrap().pdf.clone().unwrap()
    });
    let start = std::time::Instant::now();
    loop {
        visual.run_until_parked();
        if pdf.read_with(visual, |p, _| p.page_count().is_some()) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "PDF fixture never opened"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for (id, before_delta) in [("pdf-zoom-in", 1isize), ("pdf-zoom-out", -1)] {
        let before = pdf.read_with(visual, |p, _| p.zoom_step());
        let button_bounds = measured_bounds(visual, id).unwrap();
        assert!(button_bounds.size.width >= px(28.) && button_bounds.size.height >= px(28.));
        visual.simulate_click(button_bounds.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            pdf.read_with(visual, |p, _| p.zoom_step()),
            (before as isize + before_delta) as usize
        );
    }
    let actions = reader.read_with(visual, |r, cx| r.viewer_chrome(cx).actions);
    let zoom_in = actions.iter().find(|a| a.id == "pdf-zoom-in").unwrap();
    match zoom_in.menu_item() {
        PopupMenuItem::Item {
            action, handler, ..
        } => {
            assert!(action.unwrap().partial_eq(&PdfZoomIn), "live menu binding");
            let before = pdf.read_with(visual, |p, _| p.zoom_step());
            visual.update(|w, cx| handler.unwrap()(&ClickEvent::default(), w, cx));
            assert_eq!(pdf.read_with(visual, |p, _| p.zoom_step()), before + 1);
        }
        _ => panic!("PDF overflow must be an actionable menu item"),
    }
    let fit = measured_bounds(visual, "pdf-zoom-fit").unwrap();
    visual.simulate_click(fit.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(pdf.read_with(visual, |p, _| p.zoom_step()), 4);
}

#[test]
fn open_with_contains_platform_reveal_and_quick_look_only_on_macos() {
    let commands = open_commands();
    assert_eq!(commands[0].0, "Open with default app");
    assert!(commands[0].2.partial_eq(&OpenFileDefault));
    assert_eq!(commands[1].0, platform::labels::Os::CURRENT.reveal());
    assert_eq!(
        commands.iter().any(|(label, _, _)| *label == "Quick Look"),
        cfg!(target_os = "macos")
    );
    assert_eq!(
        commands.len(),
        if cfg!(target_os = "macos") { 3 } else { 2 }
    );
}

#[gpui::test]
fn open_with_button_opens_and_dismisses_the_same_header_menu(cx: &mut TestAppContext) {
    let (_, visual, _temp) = fixture(cx, "text.txt");
    OPEN_WITH_ITEMS.with(|record| record.borrow_mut().take());
    let button = measured_bounds(visual, "viewer-open-with").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|window, cx| {
        // Cached prepaint does not replay overlay measurement callbacks.
        window.refresh();
        window.draw(cx).clear(cx);
        assert!(
            !gpui_base::GlobalState::native_overlays(window, cx).is_empty(),
            "the popup actually mounted"
        );
        OPEN_WITH_ITEMS.with(|record| {
            let record = record.borrow();
            let (menu, labels) = record.as_ref().expect("Open with popup was built");
            let menu = menu.upgrade().expect("mounted popup is alive");
            assert!(menu.read(cx).focus_handle(cx).is_focused(window));
            assert!(!menu.read(cx).is_empty());
            let expected: Vec<SharedString> = open_commands()
                .into_iter()
                .map(|(label, _, _)| label.into())
                .collect();
            assert_eq!(
                *labels, expected,
                "mounted popup contains the platform commands"
            );
        });
    });
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    visual.update(|window, cx| {
        // Cached prepaint does not replay overlay measurement callbacks.
        window.refresh();
        window.draw(cx).clear(cx);
        assert!(
            gpui_base::GlobalState::native_overlays(window, cx).is_empty(),
            "popup was dismissed"
        );
        OPEN_WITH_ITEMS.with(|record| {
            if let Some(menu) = record
                .borrow()
                .as_ref()
                .and_then(|(menu, _)| menu.upgrade())
            {
                assert!(!menu.read(cx).focus_handle(cx).is_focused(window));
            }
        });
    });
    assert_eq!(measured_bounds(visual, "viewer-open-with").unwrap(), button);
}

#[gpui::test]
fn read_mode_find_keystrokes_do_not_notify_cached_side_panels(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    struct Observer {
        _subscriptions: Vec<Subscription>,
    }
    let _cached = reader_side_panel::tests::CachedPanels::on();
    let (reader, visual, temp) = fixture(cx, "note.md");
    std::fs::write(
        temp.path().join("vault/find.json"),
        format!("{{\"payload\":\"{} target\"}}", "x".repeat(100_000)),
    )
    .unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("find.json", window, cx)
    });
    visual.run_until_parked();
    let code = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    let start = std::time::Instant::now();
    while code.read_with(visual, |code, _| code.document_snapshot().is_none()) {
        visual.run_until_parked();
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(10));
    }
    reader.update_in(visual, |reader, window, cx| reader.open_find(window, cx));
    visual.run_until_parked();
    assert!(
        measured_bounds(visual, "data-whole-file-find").is_some(),
        "whole-file Find is mounted"
    );
    visual.simulate_keystrokes("home");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let panel_counts = Rc::new(std::cell::Cell::new([0; 2]));
    let code_count = Rc::new(std::cell::Cell::new(0));
    let panels = reader.read_with(visual, |reader, _| reader.side_panels.clone());
    let _observer = visual.update(|_, cx| {
        cx.new(|cx: &mut Context<Observer>| {
            let mut subscriptions = Vec::new();
            for (index, panel) in panels.into_iter().enumerate() {
                let counts = panel_counts.clone();
                subscriptions.push(cx.observe(&panel.unwrap(), move |_, _, _| {
                    let mut value = counts.get();
                    value[index] += 1;
                    counts.set(value);
                }));
            }
            let count = code_count.clone();
            subscriptions.push(cx.observe(&code, move |_, _, _| count.set(count.get() + 1)));
            Observer {
                _subscriptions: subscriptions,
            }
        })
    });
    visual.simulate_input("target");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    assert!(
        code_count.get() > 0,
        "positive control: actual whole-file Find notified the viewer"
    );
    assert_eq!(
        panel_counts.get(),
        [0, 0],
        "read-mode Find must not notify either cached side panel"
    );
}

#[gpui::test]
fn async_editable_flip_refreshes_header_on_the_next_reader_render(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(cx, "note.md");
    let code = reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("source.rs", window, cx);
        let code = reader.file_preview.as_ref().unwrap().code.clone().unwrap();
        assert!(
            code.read(cx).loading(),
            "exercise the asynchronous loading state"
        );
        assert!(!reader.code_file_editable(cx));
        code
    });
    // Draw the loading header before polling the asynchronous completion.
    // Neither this frame nor the completed one forces a window refresh.
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let loading_header = reader.read_with(visual, |reader, cx| {
        let header = reader.viewer_header.borrow().as_ref().unwrap().clone();
        assert!(!header.read(cx).key.editable);
        header.entity_id()
    });
    visual.run_until_parked();
    assert!(code.read_with(visual, |code, _| code.editable()));
    visual.update(|window, cx| window.draw(cx).clear(cx));
    reader.read_with(visual, |reader, cx| {
        let header = reader.viewer_header.borrow().as_ref().unwrap().clone();
        assert!(
            header.read(cx).key.editable,
            "async capability reaches the header key"
        );
        assert_ne!(header.entity_id(), loading_header);
    });
}

#[gpui::test]
fn nested_file_crumbs_expand_at_wide_width_and_collapse_before_title_truncation(
    cx: &mut TestAppContext,
) {
    let (reader, visual, _temp) = fixture(cx, "Folder/A long parent folder name/file.txt");
    reader.update(visual, |reader, cx| {
        reader.panels.close(reader_layout::Panel::Notes);
        reader.panels.close(reader_layout::Panel::Backlinks);
        cx.notify();
    });
    for (width, expanded) in [(1600., true), (600., false), (1600., true)] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        assert_eq!(
            measured_bounds(visual, "reader-crumb-Folder").is_some(),
            expanded
        );
        let title = measured_bounds(visual, "reader-document-root").unwrap();
        assert!(
            title.size.width >= px(50.),
            "short file title remains legible"
        );
    }
}

#[gpui::test]
fn pdf_reopen_rebinds_cached_header_zoom_to_the_new_viewer(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (reader, visual, temp) = fixture(cx, "document.pdf");
    let first = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().pdf.clone().unwrap()
    });
    let start = std::time::Instant::now();
    while first.read_with(visual, |pdf, _| pdf.page_count().is_none()) {
        visual.run_until_parked();
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(10));
    }
    // Complete the replacement before exposing it to Reader. Otherwise a
    // transient loading/disabled action changes the key for an unrelated
    // reason and hides callbacks retained by an otherwise identical header.
    let second = visual.update(|window, cx| {
        cx.new(|cx| {
            crate::reader_pdf::PdfViewer::new(temp.path().join("vault/document.pdf"), window, cx)
        })
    });
    let start = std::time::Instant::now();
    while second.read_with(visual, |pdf, _| pdf.page_count().is_none()) {
        visual.run_until_parked();
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(10));
    }
    reader.update_in(visual, |reader, window, cx| {
        reader.focus_handle.focus(window, cx)
    });
    let at = measured_bounds(visual, "pdf-zoom-in").unwrap().center();
    visual.simulate_event(MouseMoveEvent {
        position: at,
        ..Default::default()
    });
    visual.run_until_parked();
    measured_bounds(visual, "pdf-zoom-in").unwrap();
    let first_zoom = first.read_with(visual, |pdf, _| pdf.zoom_step());
    let mut original_key = reader.read_with(visual, HeaderKey::new);
    let original_header = reader.read_with(visual, |reader, _| {
        reader.viewer_header.borrow().as_ref().unwrap().entity_id()
    });
    reader.update(visual, |reader, cx| {
        reader.file_preview.as_mut().unwrap().pdf = Some(second.clone());
        cx.notify();
    });
    visual.run_until_parked();
    let mut replacement_key = reader.read_with(visual, HeaderKey::new);
    original_key.viewers.clear();
    replacement_key.viewers.clear();
    assert!(
        original_key == replacement_key,
        "positive control: only callback ownership changed, not presentation"
    );
    assert_ne!(
        first.entity_id(),
        second.entity_id(),
        "positive control: same file got a new viewer entity"
    );
    // Do not force a refresh after replacement. Also require a new cache
    // identity: incidental PDF paints may refresh callbacks in headless runs.
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert_ne!(
        reader.read_with(visual, |reader, _| {
            reader.viewer_header.borrow().as_ref().unwrap().entity_id()
        }),
        original_header,
        "replacement viewer must invalidate the callback-owning header cache; incidental repaint is insufficient"
    );
    let second_zoom = second.read_with(visual, |pdf, _| pdf.zoom_step());
    visual.simulate_click(at, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        second.read_with(visual, |pdf, _| pdf.zoom_step()),
        second_zoom + 1
    );
    assert_eq!(
        first.read_with(visual, |pdf, _| pdf.zoom_step()),
        first_zoom
    );
}

#[gpui::test]
fn edit2803_typing_in_an_open_file_does_not_render_the_header_slots(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(cx, "source.rs");
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    let input = reader.read_with(visual, |reader, _| {
        reader.editing.as_ref().unwrap().input().clone()
    });
    input.update_in(visual, |input, window, cx| input.focus(window, cx));
    visual.run_until_parked();
    // GPUI intentionally refreshes the entire window when input modality
    // switches from mouse to keyboard (window::dispatch_event). Establish
    // keyboard modality with a navigation key before measuring typing; this
    // does not mutate the source or hide an editor-triggered invalidation.
    let before_navigation = input.read_with(visual, |input, _| input.source_stamp());
    visual.simulate_keystrokes("home");
    visual.run_until_parked();
    assert_eq!(
        input.read_with(visual, |input, _| input.source_stamp()),
        before_navigation,
        "navigation establishes keyboard modality without editing"
    );
    let before = input.read_with(visual, |input, _| input.source_stamp());
    // Headless windows have no platform frame source. Commit the focus/layout
    // frame before taking the baseline, just as the native window does.
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    let header_identity = reader.read_with(visual, |reader, _| {
        reader.viewer_header.borrow().as_ref().unwrap().entity_id()
    });
    let header_renders = || {
        HEADER_RENDERS.with(|counts| {
            counts
                .borrow()
                .get(&reader.entity_id())
                .copied()
                .unwrap_or(0)
        })
    };
    let initial = header_renders();
    assert!(initial > 0, "positive control: real file header rendered");
    for _ in 0..8 {
        visual.simulate_input(" ");
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(101));
        visual.run_until_parked();
        // Exercise a real draw for every edit without forcing a full refresh.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_ne!(
            input.read_with(visual, |input, _| input.source_stamp()),
            before
        );
        assert_eq!(
            reader.read_with(visual, |reader, _| reader
                .viewer_header
                .borrow()
                .as_ref()
                .unwrap()
                .entity_id()),
            header_identity,
            "typing preserves the header presentation identity"
        );
        assert_eq!(
            header_renders(),
            initial,
            "typing must not rebuild the seven header slots"
        );
    }
    assert_ne!(
        input.read_with(visual, |input, _| input.source_stamp()),
        before
    );
    reader.read_with(visual, |reader, cx| {
        assert!(reader.source_indicator_dirty(cx))
    });
    assert!(
        measured_bounds(visual, "source-save").is_some(),
        "dirty Save updates independently"
    );
}
