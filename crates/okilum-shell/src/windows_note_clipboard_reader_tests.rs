//! Reader integration tests for WindowsNoteClipboard (moved from
//! `platform::windows_note_clipboard::tests` during #3234 split).
use crate::platform::windows_note_clipboard::{Snapshot, WindowsNoteClipboard};
use gpui_component::input::clipboard::ClipboardReadError as Error;

fn cf_html(fragment: &str) -> String {
    let header = |start, end, frag_start, frag_end| {
        format!(
            "Version:1.0\r\nStartHTML:{start:010}\r\nEndHTML:{end:010}\r\nStartFragment:{frag_start:010}\r\nEndFragment:{frag_end:010}\r\nSourceURL:https://example.org/chat/room\r\n"
        )
    };
    let before = "<html><body>שלום<!--StartFragment-->";
    let after = "<!--EndFragment--></body></html>";
    let start = header(0, 0, 0, 0).len();
    let frag_start = start + before.len();
    let frag_end = frag_start + fragment.len();
    format!(
        "{}{before}{fragment}{after}",
        header(start, frag_end + after.len(), frag_start, frag_end)
    )
}

/// Exercise the actual note-specific async provider in a real isolated editor.
/// Only the OS acquisition function is replaced by an immutable public fixture.
#[gpui::test]
fn cfhtml_provider_preserves_link_selection_and_one_undo(cx: &mut gpui::TestAppContext) {
    slack_editor_paste(cx, false);
}

#[gpui::test]
fn slack_paste_live_preview_preserves_links_save_and_undo(cx: &mut gpui::TestAppContext) {
    slack_editor_paste(cx, true);
}

fn slack_editor_paste(cx: &mut gpui::TestAppContext, live: bool) {
    use crate::*;
    use gpui_component::input::clipboard::{ClipboardReadRequest, ExactClipboardProvider};
    use gpui_component::input::{EditorState, Paste, Redo, Undo};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    struct Plain;
    impl ExactClipboardProvider for Plain {
        fn read_text(
            &self,
            _: ClipboardReadRequest,
            _: &gpui::App,
        ) -> gpui::Task<Result<Option<String>, Error>> {
            gpui::Task::ready(Ok(Some("plain label\r\n".into())))
        }
    }
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
        crate::note_paste::install_menu(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(&root).unwrap();
    let original = "before target after\n";
    std::fs::write(root.join("note.md"), original).unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    open_path: Some(root.join("note.md")),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = entity.unwrap();
    visual.run_until_parked();
    reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
    visual.run_until_parked();
    if live {
        reader.update_in(visual, |r, w, cx| r.toggle_live_preview(w, cx));
        visual.run_until_parked();
    }
    assert_eq!(
        reader.read_with(visual, |r, _| r.live_preview_shown()),
        live
    );
    let input: gpui::Entity<EditorState> =
        reader.read_with(visual, |r, _| r.source_input().unwrap());
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    // Keep the real Slack anchor markup; substitute only its public label/URL.
    let slack = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../okilum-paste/tests/fixtures/slack.html"
    ));
    let start = slack.find("<a target=").unwrap();
    let end = start + slack[start..].find("</a>").unwrap() + 4;
    let anchor = slack[start..end]
        .replace(">https://ci.example.com/run/42</a>", ">Docs label</a>")
        .replace(
            "href=\"https://ci.example.com/run/42\"",
            "href=\"https://example.org/deep\"",
        );
    let raw = cf_html(&anchor);
    let payload = Arc::new(Mutex::new(Snapshot {
        plain: Some("Docs label".into()),
        html: Some(raw.clone()),
        native: None,
    }));
    let fixture = payload.clone();
    let provider = WindowsNoteClipboard::with_reader(
        Arc::new(Plain),
        root.clone(),
        "note.md".into(),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(Default::default())),
        Arc::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(fixture.lock().unwrap().clone())
        }),
    );
    input.update(visual, |i, cx| {
        i.set_exact_clipboard_provider(Some(Arc::new(provider)), cx);
        i.set_selected_range(7..13, cx);
    });
    visual.run_until_parked();
    for menu in [false, true] {
        let (label, destination) = if menu {
            ("https://example.org/deep", "https://example.org/deep/")
        } else {
            ("Docs label", "https://example.org/deep")
        };
        *payload.lock().unwrap() = Snapshot {
            plain: Some(label.into()),
            html: Some(cf_html(
                &anchor
                    .replace(">Docs label</a>", &format!(">{label}</a>"))
                    .replace(
                        "href=\"https://example.org/deep\"",
                        &format!("href=\"{destination}\""),
                    ),
            )),
            native: None,
        };
        input.update(visual, |i, cx| i.set_selected_range(7..13, cx));
        if menu {
            visual.dispatch_action(crate::note_paste::PasteFromMenu);
        } else {
            visual.dispatch_action(Paste);
        }
        visual.run_until_parked();
        let expected = format!("before [{label}]({destination}) after\n");
        assert_eq!(
            input.read_with(visual, |i, _| i.value().to_string()),
            expected
        );
        reader.update(visual, |r, cx| assert!(r.save_source(cx)));
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            expected
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        assert_eq!(
            input.read_with(visual, |i, _| i.value().to_string()),
            original,
            "single Undo restores the replaced selection"
        );
        visual.dispatch_action(Redo);
        visual.run_until_parked();
        assert_eq!(
            input.read_with(visual, |i, _| i.value().to_string()),
            expected
        );
        visual.dispatch_action(Undo);
        visual.run_until_parked();
        reader.update(visual, |r, cx| assert!(r.save_source(cx)));
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            original
        );
    }
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "both paste entry points acquired the fixture"
    );
    *payload.lock().unwrap() = Snapshot {
        plain: Some("Docs label".into()),
        html: Some(raw),
        native: None,
    };
    input.update(visual, |i, cx| i.set_selected_range(0..0, cx));
    visual.run_until_parked();
    visual.dispatch_action(crate::note_paste::PasteAsPlainText);
    visual.run_until_parked();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "plain command acquires metadata; router skips CF_HTML bytes"
    );
    assert_eq!(
        input.read_with(visual, |i, _| i.value().to_string()),
        "Docs labelbefore target after\n"
    );
    visual.dispatch_action(Undo);
    visual.run_until_parked();
    input.update_in(visual, |i, window, cx| {
        i.set_value("```\n", window, cx);
        i.set_selected_range(4..4, cx);
    });
    visual.run_until_parked();
    visual.dispatch_action(Paste);
    visual.run_until_parked();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        4,
        "fenced code acquires metadata; router skips CF_HTML bytes"
    );
    assert_eq!(
        input.read_with(visual, |i, _| i.value().to_string()),
        "```\nDocs label"
    );
}

fn deferred_offer_reaches_editor(cx: &mut gpui::TestAppContext, data_image: bool) {
    use crate::*;
    use gpui_component::input::clipboard::{ClipboardReadRequest, ExactClipboardProvider};
    use gpui_component::input::{Paste, Undo};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct Plain;
    impl ExactClipboardProvider for Plain {
        fn read_text(
            &self,
            _: ClipboardReadRequest,
            _: &gpui::App,
        ) -> gpui::Task<Result<Option<String>, Error>> {
            gpui::Task::ready(Ok(Some("Docs label".into())))
        }
    }
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(&root).unwrap();
    let original = "seed\n";
    std::fs::write(root.join("note.md"), original).unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    open_path: Some(root.join("note.md")),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = entity.unwrap();
    visual.run_until_parked();
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    let input = reader.read_with(visual, |reader, _| reader.source_input().unwrap());
    let outbox = reader.read_with(visual, |reader, _| reader.source_paste_outbox());
    let raw = if data_image {
        use base64::Engine as _;
        let image = ::image::DynamicImage::ImageRgba8(::image::RgbaImage::from_pixel(
            1,
            1,
            ::image::Rgba([0, 0, 0, 255]),
        ));
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, ::image::ImageFormat::Png).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        cf_html(&format!(
            "<p><a href='https://example.org/deferred'>Docs label</a><img alt='dot' src='data:image/png;base64,{data}'></p>"
        ))
    } else {
        cf_html(&format!(
            "<p><a href='https://example.org/deferred'>Docs label</a> {}</p>",
            "a".repeat(512 * 1024)
        ))
    };
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let provider = WindowsNoteClipboard::with_reader(
        Arc::new(Plain),
        root.clone(),
        "note.md".into(),
        Arc::new(std::sync::Mutex::new(None)),
        outbox.clone(),
        Arc::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Snapshot {
                plain: Some("Docs label".into()),
                html: Some(raw.clone()),
                native: None,
            })
        }),
    );
    input.update(visual, |input, cx| {
        input.set_exact_clipboard_provider(Some(Arc::new(provider)), cx);
        input.set_selected_range(original.len()..original.len(), cx);
    });
    visual.run_until_parked();
    visual.dispatch_action(Paste);
    visual.run_until_parked();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "actual acquisition positive control"
    );
    let text = input.read_with(visual, |input, _| input.value().to_string());
    assert!(
        text.contains("[Docs label](https://example.org/deferred)"),
        "accepted deferred paste must reach event drain and insertion"
    );
    if data_image {
        assert!(text.contains("![["), "data image is stored and embedded");
    } else {
        assert!(
            text.len() > 512 * 1024,
            "large fragment is not lost or silently replaced by plain fallback"
        );
    }
    assert!(
        outbox.lock().unwrap().follow_up.is_none(),
        "the actual Reader subscriber drained this request's work"
    );
    visual.dispatch_action(Undo);
    visual.run_until_parked();
    assert_eq!(
        input.read_with(visual, |input, _| input.value().to_string()),
        original,
        "one Undo restores the empty-caret paste"
    );
}

#[gpui::test]
fn empty_caret_large_html_commits_deferred_paste(cx: &mut gpui::TestAppContext) {
    deferred_offer_reaches_editor(cx, false);
}
#[gpui::test]
fn empty_caret_data_image_commits_deferred_paste(cx: &mut gpui::TestAppContext) {
    deferred_offer_reaches_editor(cx, true);
}
