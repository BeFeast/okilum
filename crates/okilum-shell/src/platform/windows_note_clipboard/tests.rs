use super::*;
use ::core::prelude::v1::test;

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
fn routed(snapshot: Snapshot, mode: router::Mode, source: &str) -> Box<router::Routed> {
    let temp = tempfile::tempdir().unwrap();
    route_snapshot(
        snapshot,
        &router::RouteContext {
            image_options: Default::default(),
            mode,
            source,
            selection: source.len()..source.len(),
            vault_root: temp.path(),
            note_rel: Some("note.md"),
            stamp: "20261101120000",
            ask_above: None,
        },
    )
    .unwrap()
}
#[test]
fn html_anchors_keep_different_labels_destinations_and_unicode_offsets() {
    let raw = cf_html(
        "<p>Привет <a href=\"https://example.org/target?q=1&amp;x=2\">label שלום</a></p><p>Next <a href=\"/other\">second</a>.</p>",
    );
    let snapshot = Snapshot {
        plain: Some("Привет label שלום\r\nNext second.".into()),
        html: Some(raw),
        native: None,
    };
    let decoded = Snapshot::decode(&snapshot.encode()).unwrap();
    let result = routed(decoded, router::Mode::Smart, "");
    assert_eq!(result.route, router::Route::RichText);
    let text = result.insert.unwrap();
    assert!(
        text.contains("[label שלום](https://example.org/target?q=1&x=2)"),
        "{text}"
    );
    assert!(
        text.contains("[second](https://example.org/other)"),
        "{text}"
    );
    assert!(
        text.contains("\n\n"),
        "mixed paragraphs stay separate: {text}"
    );
}
#[test]
fn absent_or_malformed_html_uses_normalized_plain_router_fallback() {
    for html in [
        None,
        Some("<a href='https://example.org'>not CF_HTML</a>".into()),
        Some(
            cf_html("<a href='https://example.org'>label</a>")
                .replace("StartFragment:", "InvalidFragment:"),
        ),
    ] {
        let result = routed(
            Snapshot {
                plain: Some("label\r\nsecond".into()),
                html,
                native: None,
            },
            router::Mode::Smart,
            "",
        );
        assert_eq!(result.insert.as_deref(), Some("label\nsecond"));
    }
}
#[test]
fn plain_and_fenced_code_routes_never_convert_html() {
    for (mode, source, route) in [
        (router::Mode::Plain, "", router::Route::PlainText),
        (router::Mode::Smart, "```\n", router::Route::CodeBlockText),
    ] {
        let result = routed(
            Snapshot {
                plain: Some("label\r\nraw".into()),
                html: Some(cf_html("<a href='https://example.org/target'>label</a>")),
                native: None,
            },
            mode,
            source,
        );
        assert_eq!(result.route, route);
    }
}
#[test]
fn bad_byte_offsets_and_frames_are_not_accepted_as_html() {
    let raw = cf_html("<p>שלום</p>");
    let good_start = raw
        .lines()
        .find_map(|l| l.strip_prefix("StartFragment:"))
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let invalid = raw.replace(
        &format!("StartFragment:{good_start:010}"),
        &format!("StartFragment:{:010}", good_start + 4),
    ); // within a UTF-8 scalar
    assert!(fragment(&invalid).is_none());
    for frame in [
        "",
        "0missing-separator",
        "0text\0html",
        "1plain\0html\0extra",
    ] {
        assert!(Snapshot::decode(frame).is_err());
    }
    for plain in [
        None,
        Some(String::new()),
        Some("\u{feff}exact\r\nשלום\rline".into()),
    ] {
        let expected = plain.clone();
        let decoded = Snapshot::decode(
            &Snapshot {
                plain,
                html: None,
                native: None,
            }
            .encode(),
        )
        .unwrap();
        assert_eq!(decoded.plain, expected);
    }
}

#[test]
fn no_html_context_and_plain_flavor_limit_remain_supported() {
    let raw = cf_html("<a href='https://example.org/path'>Different label</a>");
    let start = raw
        .lines()
        .find_map(|line| line.strip_prefix("StartHTML:"))
        .unwrap();
    let end = raw
        .lines()
        .find_map(|line| line.strip_prefix("EndHTML:"))
        .unwrap();
    let raw = raw
        .replace(&format!("StartHTML:{start}"), "StartHTML:-000000001")
        .replace(&format!("EndHTML:{end}"), "EndHTML:-000000001");
    assert!(fragment(&raw).unwrap().0.contains("Different label"));
    let text = "x".repeat(MAX_BYTES);
    assert_eq!(
        Snapshot::decode(
            &Snapshot {
                plain: Some(text.clone()),
                html: None,
                native: None
            }
            .encode()
        )
        .unwrap()
        .plain,
        Some(text)
    );
    assert!(Snapshot::decode(
        &Snapshot {
            plain: Some("x".repeat(MAX_BYTES + 1)),
            html: None,
            native: None
        }
        .encode()
    )
    .is_err());
}

/// Exercise the actual note-specific async provider in a real isolated editor.
/// Only the OS acquisition function is replaced by an immutable public fixture.
#[gpui::test]
fn cfhtml_provider_preserves_link_selection_and_one_undo(cx: &mut gpui::TestAppContext) {
    use crate::*;
    use gpui_component::input::clipboard::{ClipboardReadRequest, ExactClipboardProvider};
    use gpui_component::input::{EditorState, Paste, Undo};
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
    let input: gpui::Entity<EditorState> =
        reader.read_with(visual, |r, _| r.source_input().unwrap());
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let raw = cf_html("<a href='https://example.org/deep'>Docs label</a>");
    let provider = WindowsNoteClipboard::with_reader(
        Arc::new(Plain),
        root.clone(),
        "note.md".into(),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(Default::default())),
        Arc::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Snapshot {
                plain: Some("Docs label".into()),
                html: Some(raw.clone()),
                native: None,
            })
        }),
    );
    input.update(visual, |i, cx| {
        i.set_exact_clipboard_provider(Some(Arc::new(provider)), cx);
        i.set_selected_range(7..13, cx);
    });
    visual.run_until_parked();
    visual.dispatch_action(Paste);
    visual.run_until_parked();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "actual provider acquisition ran"
    );
    assert_eq!(
        input.read_with(visual, |i, _| i.value().to_string()),
        "before [Docs label](https://example.org/deep) after\n"
    );
    visual.dispatch_action(Undo);
    visual.run_until_parked();
    assert_eq!(
        input.read_with(visual, |i, _| i.value().to_string()),
        original,
        "single Undo restores the replaced selection"
    );
    input.update(visual, |i, cx| i.set_selected_range(0..0, cx));
    visual.run_until_parked();
    visual.dispatch_action(crate::note_paste::PasteAsPlainText);
    visual.run_until_parked();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
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
        3,
        "fenced code acquires metadata; router skips CF_HTML bytes"
    );
    assert_eq!(
        input.read_with(visual, |i, _| i.value().to_string()),
        "```\nDocs label"
    );
}

#[test]
fn busy_does_not_consume_the_originating_request_followup() {
    use crate::note_paste::{
        anchor::Anchor,
        router::{FollowUp, Notice},
        Outbox,
    };
    use gpui_component::input::clipboard::{ClipboardPasteEvent, ClipboardReadRequest};
    let mut pending = Outbox {
        request: Some(ClipboardReadRequest::default()),
        notice: Some(Notice {
            text: "pending".into(),
            error: false,
        }),
        follow_up: Some(FollowUp::Title {
            url: "https://example.org".into(),
            anchor: Anchor::at("", 0),
        }),
        ..Default::default()
    };
    assert!(pending
        .take_for_event(&ClipboardPasteEvent::Failed(Error::Busy))
        .follow_up
        .is_none());
    assert!(
        pending.follow_up.is_some(),
        "Busy belongs to the second command"
    );
    assert!(pending
        .take_for_event(&ClipboardPasteEvent::Failed(Error::Unavailable))
        .notice
        .is_none());
    assert!(
        pending.follow_up.is_some(),
        "unrelated failure is not an acknowledgement"
    );
    assert!(pending
        .take_for_event(&ClipboardPasteEvent::Applied {
            request: 1,
            bytes: 0
        })
        .follow_up
        .is_some());
    assert!(
        pending.follow_up.is_none(),
        "only its accepted Input event drains the outbox"
    );
    let mut legacy = Outbox {
        notice: Some(Notice {
            text: "legacy".into(),
            error: false,
        }),
        ..Default::default()
    };
    assert!(legacy
        .take_for_event(&ClipboardPasteEvent::Failed(Error::Unavailable))
        .notice
        .is_some());
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

// Same Board and router as Windows, with a mock transport and no OS clipboard.
fn lazy_paste(
    types: &[&str],
    flavors: Vec<(&str, Vec<u8>)>,
    mode: router::Mode,
    source: &str,
    selection: std::ops::Range<usize>,
    root: &std::path::Path,
) -> (Box<router::Routed>, Vec<String>) {
    use std::sync::{Arc, Mutex};
    let reads = Arc::new(Mutex::new(Vec::new()));
    let observed = reads.clone();
    let flavors: std::collections::BTreeMap<String, Vec<u8>> = flavors
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect();
    let read: ReadFlavor = Arc::new(move |kind| {
        observed.lock().unwrap().push(kind.to_owned());
        Ok(flavors.get(kind).cloned())
    });
    let snapshot = Snapshot {
        native: Some(NativeOffer {
            sequence: 42,
            types: types.iter().map(|kind| (*kind).into()).collect(),
        }),
        ..Default::default()
    };
    let result = route_snapshot_with(
        snapshot,
        &router::RouteContext {
            image_options: crate::note_paste::image_import::Options::disabled(),
            mode,
            source,
            selection,
            vault_root: root,
            note_rel: Some("note.md"),
            stamp: "20261101120000",
            ask_above: None,
        },
        Some(read),
    )
    .unwrap();
    let reads = reads.lock().unwrap().clone();
    (result, reads)
}

#[test]
fn windows_plain_and_code_commands_read_only_unicode_even_with_images_and_html() {
    let vault = tempfile::tempdir().unwrap();
    for (mode, source, route) in [
        (router::Mode::Plain, "", router::Route::PlainText),
        (
            router::Mode::Smart,
            "\u{0060}\u{0060}\u{0060}\n",
            router::Route::CodeBlockText,
        ),
    ] {
        let (result, reads) = lazy_paste(
            &[offer::UTF8_TEXT, offer::HTML[0], "public.png"],
            vec![(offer::UTF8_TEXT, b"raw\ntext".to_vec())],
            mode,
            source,
            source.len()..source.len(),
            vault.path(),
        );
        assert_eq!(result.route, route);
        assert!(
            reads.iter().any(|kind| kind == offer::UTF8_TEXT),
            "positive acquisition control"
        );
        assert!(
            reads
                .iter()
                .all(|kind| kind == VERIFY || kind == offer::UTF8_TEXT),
            "{reads:?}"
        );
    }
}

#[test]
fn windows_without_html_routes_tsv_ide_selection_and_titled_links() {
    let vault = tempfile::tempdir().unwrap();
    for (plain, code, mode, source, selection, route, expected) in [
        (
            "a\tb\n1\t2",
            None,
            router::Mode::Smart,
            "",
            0..0,
            router::Route::Table,
            "| a | b |",
        ),
        (
            "let a = 1;\nlet b = 2;",
            Some(r#"{"mode":"rust"}"#),
            router::Mode::Smart,
            "",
            0..0,
            router::Route::IdeCode,
            "\u{0060}\u{0060}\u{0060}rust",
        ),
        (
            "https://example.org",
            None,
            router::Mode::Smart,
            "label",
            0..5,
            router::Route::SelectionLink,
            "[label](https://example.org",
        ),
        (
            "https://example.org",
            None,
            router::Mode::TitledLink,
            "",
            0..0,
            router::Route::TitledLink,
            "https://example.org",
        ),
    ] {
        let mut types = vec![offer::UTF8_TEXT];
        let mut flavors = vec![(offer::UTF8_TEXT, plain.as_bytes().to_vec())];
        if let Some(code) = code {
            types.extend([offer::VSCODE_EDITOR_DATA, offer::HTML[0]]);
            flavors.push((offer::VSCODE_EDITOR_DATA, code.as_bytes().to_vec()));
        }
        let (result, reads) = lazy_paste(&types, flavors, mode, source, selection, vault.path());
        assert_eq!(result.route, route);
        assert!(
            result.insert.as_deref().unwrap().contains(expected),
            "{result:?}"
        );
        assert!(
            !reads.iter().any(|kind| kind == offer::HTML[0]),
            "{reads:?}"
        );
    }
}

#[test]
fn windows_explorer_files_win_without_reading_text_or_image_payloads() {
    let vault = tempfile::tempdir().unwrap();
    let file = vault.path().join("diagram.png");
    std::fs::write(&file, b"existing attachment").unwrap();
    let urls = vec![url::Url::from_file_path(&file).unwrap().to_string()];
    let (result, reads) = lazy_paste(
        &[offer::FILE_URL, offer::UTF8_TEXT, "public.png"],
        vec![(offer::FILE_URL, serde_json::to_vec(&urls).unwrap())],
        router::Mode::Smart,
        "",
        0..0,
        vault.path(),
    );
    assert_eq!(result.route, router::Route::Files);
    assert!(reads.iter().any(|kind| kind == offer::FILE_URL));
    assert!(
        reads
            .iter()
            .all(|kind| kind == VERIFY || kind == offer::FILE_URL),
        "{reads:?}"
    );
}

#[test]
fn windows_png_is_saved_only_when_image_route_wins() {
    let vault = tempfile::tempdir().unwrap();
    let image = ::image::DynamicImage::ImageRgba8(::image::RgbaImage::from_pixel(
        1,
        1,
        ::image::Rgba([0, 0, 0, 255]),
    ));
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, ::image::ImageFormat::Png).unwrap();
    let png = png.into_inner();
    for (plain, expected_route) in [
        ("", router::Route::Image),
        ("actual text", router::Route::Text),
    ] {
        let (result, reads) = lazy_paste(
            &[offer::UTF8_TEXT, "public.png", "com.microsoft.bmp"],
            vec![
                (offer::UTF8_TEXT, plain.as_bytes().to_vec()),
                ("public.png", png.clone()),
            ],
            router::Mode::Smart,
            "",
            0..0,
            vault.path(),
        );
        assert_eq!(result.route, expected_route);
        assert_eq!(
            reads.iter().any(|kind| kind == "public.png"),
            expected_route == router::Route::Image
        );
        assert!(
            !reads.iter().any(|kind| kind == "com.microsoft.bmp"),
            "PNG wins over DIB"
        );
        if expected_route == router::Route::Image {
            assert_eq!(result.saved_files.len(), 1);
            assert_eq!(std::fs::read(&result.saved_files[0]).unwrap(), png);
            assert!(result.insert.unwrap().contains("![["));
        }
    }
}
