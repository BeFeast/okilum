use super::*;
use ::core::prelude::v1::test;
use gpui_component::WindowExt as _;

fn fixture<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
) -> (Entity<Reader>, &'a mut VisualTestContext, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("Folder")).unwrap();
    for (name, value) in [
        ("data.json", "{\"a\":1,\"a\":2e2}\r\n"),
        ("data.csv", "name,value\r\none,1\r\n"),
        ("source.rs", "fn main() {}\n"),
        ("text.txt", "hello שלום\r\n"),
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

fn clipboard(visual: &mut VisualTestContext) -> Option<String> {
    visual.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

fn open_copy(visual: &mut VisualTestContext) {
    let bounds = visual
        .debug_bounds("viewer-copy")
        .expect("Copy menu is mounted");
    visual.simulate_click(bounds.center(), Modifiers::default());
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
    let edit = visual
        .debug_bounds("reader-edit")
        .expect("loaded JSON can be edited");
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
        let header = visual.debug_bounds("document-header-viewport").unwrap();
        assert_eq!(header.size.height, px(48.));
        let title = visual.debug_bounds("reader-document-root").unwrap();
        assert!(
            title.size.width >= px(120.),
            "file name protected at {width}: {title:?}"
        );
        for id in ["reader-edit", "document-more"] {
            let b = visual.debug_bounds(id).unwrap();
            assert!(b.size.height >= px(28.) && b.size.width >= px(28.));
            assert!(b.left() >= header.left() && b.right() <= header.right());
            assert!(b.top() >= header.top() && b.bottom() <= header.bottom());
        }
        for obsolete in ["file-open", "file-reveal", "file-copy", "file-quicklook"] {
            assert!(visual.debug_bounds(obsolete).is_none());
        }
    }
    reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
    visual.run_until_parked();
    reader.update_in(visual, |r, w, cx| {
        r.editing.as_ref().unwrap().set_value("unsaved", w, cx)
    });
    visual.simulate_resize(size(px(600.), px(800.)));
    visual.run_until_parked();
    for id in ["reader-read", "source-save", "document-more"] {
        assert!(visual.debug_bounds(id).is_some(), "{id} never dropped");
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
    let labelled = ids.map(|id| visual.debug_bounds(id).unwrap().size.width);
    visual.simulate_resize(size(px(700.), px(800.)));
    visual.run_until_parked();
    for (id, wide) in ids.into_iter().zip(labelled) {
        assert!(
            visual.debug_bounds(id).unwrap().size.width < wide,
            "{id} label yields first"
        );
    }
    visual.simulate_resize(size(px(1600.), px(800.)));
    visual.run_until_parked();
    for (id, wide) in ids.into_iter().zip(labelled) {
        assert_eq!(
            visual.debug_bounds(id).unwrap().size.width,
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
        let before = visual.debug_bounds("document-header-viewport").unwrap();
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
            if visual.debug_bounds(marker).is_some_and(|bounds| {
                bounds.size.height
                    > visual
                        .debug_bounds("document-header-viewport")
                        .unwrap()
                        .size
                        .height
                        + px(800.)
            }) {
                break;
            }
        }
        let content_before = visual
            .debug_bounds(marker)
            .unwrap_or_else(|| panic!("scrollable content mounted for {name}: {marker}"));
        visual.simulate_event(ScrollWheelEvent {
            position: point(before.center().x, before.bottom() + px(100.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-800.))),
            ..Default::default()
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds(marker).unwrap().top() < content_before.top(),
            "positive control: {name} content actually scrolled"
        );
        assert_eq!(
            visual.debug_bounds("document-header-viewport").unwrap(),
            before
        );
    }
    reader.update_in(visual, |r, w, cx| r.open_note("note.md", None, w, cx));
    visual.run_until_parked();
    for width in [480., 700., 1400.] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("reader-maximize-note").is_some(),
            "note Maximize stays pinned"
        );
        assert!(visual.debug_bounds("viewer-copy").is_none());
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
        let bounds = visual.debug_bounds(id).unwrap();
        assert!(bounds.size.width >= px(28.) && bounds.size.height >= px(28.));
        visual.simulate_click(bounds.center(), Modifiers::default());
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
    let fit = visual.debug_bounds("pdf-zoom-fit").unwrap();
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
    let button = visual.debug_bounds("viewer-open-with").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(visual.debug_bounds("viewer-open-with").unwrap(), button);
}

#[gpui::test]
fn header_observes_viewer_notifications_without_a_vault_event(cx: &mut TestAppContext) {
    struct Observer {
        _subscription: Subscription,
    }
    let (reader, visual, _temp) = fixture(cx, "source.rs");
    let code = reader.read_with(visual, |r, _| {
        r.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
    let receipt = notifications.clone();
    let _observer = visual.update(|_, cx| {
        cx.new(|cx| Observer {
            _subscription: cx.observe(&reader, move |_, _, _| receipt.set(receipt.get() + 1)),
        })
    });
    visual.run_until_parked();
    notifications.set(0);
    code.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();
    assert!(
        notifications.get() > 0,
        "viewer state invalidates the Reader-owned header"
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
    let before = input.read_with(visual, |input, _| input.source_stamp());
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
        visual.debug_bounds("source-save").is_some(),
        "dirty Save updates independently"
    );
}
