// Pure tests: run under the Quick Look module or a harness exposing authority.
use crate::reader_plain_text::source as codec;

#[test]
fn large_quick_look_is_a_bounded_literal_prefix_and_does_not_change_source() {
    let path = std::env::temp_dir().join(format!("okilum-quick-look-{}.txt", std::process::id()));
    let original = format!("{}Жtail", "a".repeat(codec::LIMIT - 1));
    std::fs::write(&path, &original).unwrap();
    let result = codec::load(&path).unwrap();
    assert_eq!(
        result,
        codec::Preview::Content {
            text: "a".repeat(codec::LIMIT - 1),
            partial: true,
        }
    );
    assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
    std::fs::write(&path, b"binary\0payload").unwrap();
    assert_eq!(codec::load(&path).unwrap(), codec::Preview::Unsupported);
    std::fs::remove_file(path).unwrap();
}

use super::media;

#[test]
fn media_snapshot_bounds_reads_even_when_input_grows() {
    use std::io::{self, Read};
    struct Producing(usize);
    impl Read for Producing {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0 += buf.len();
            buf.fill(1);
            Ok(buf.len())
        }
    }
    let mut reader = Producing(0);
    assert!(media::snapshot(&mut reader, 32).is_err());
    assert_eq!(reader.0, 33);
    assert_eq!(media::snapshot(&b"exact"[..], 5).unwrap(), b"exact");
    assert!(media::snapshot(&b"oversize"[..], 5).is_err());
}

#[test]
fn media_admission_has_explicit_format_and_pixel_bounds() {
    assert_eq!(media::kind("Photo.PNG"), Some(media::Kind::Image));
    assert_eq!(media::kind("Book.PDF"), Some(media::Kind::Pdf));
    for name in ["image.svg", "image.heic", "file.bin"] {
        assert_eq!(media::kind(name), None);
    }
    assert!(media::dimensions_fit(4096, 4096));
    assert!(!media::dimensions_fit(4097, 1));
    assert!(!media::dimensions_fit(0, 100));
    const { assert!(media::PDF_BYTES > media::IMAGE_BYTES) };
}

#[test]
fn closing_preview_retires_queued_decode_but_live_decode_can_publish() {
    use super::authority::Authority;
    use std::sync::mpsc;
    for close in [false, true] {
        let owner = Authority::default();
        let ticket = owner.ticket();
        let (release, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            Authority::permits(&ticket).then_some("decoded selected file")
        });
        let mut owner = Some(owner);
        if close {
            owner.take();
        }
        release.send(()).unwrap();
        assert_eq!(worker.join().unwrap().is_some(), !close);
    }
}

#[test]
fn retry_retires_old_completion_and_new_attempt_reads_exact_selected_fixture() {
    use super::authority::Authority;
    let mut owner = Authority::default();
    let failed_attempt = owner.ticket();
    // Model a bounded read failure, followed by replacement of the attempt.
    struct Fails;
    impl std::io::Read for Fails {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("fixture decode input failure"))
        }
    }
    assert!(media::snapshot(Fails, media::IMAGE_BYTES).is_err());
    owner = Authority::default();
    let retry = owner.ticket();
    assert!(!Authority::permits(&failed_attempt));
    assert!(Authority::permits(&retry));
    // Exercise the same immutable selection object used by start_media.
    let path = std::env::temp_dir().join(format!("quick-look-retry-{}", std::process::id()));
    std::fs::write(&path, b"exact selected snapshot").unwrap();
    let selection = super::authority::Selection::new(path.clone());
    let _new_tree_selection = super::authority::Selection::new(path.with_extension("other"));
    let retry_selection = selection.clone();
    let bytes = media::snapshot(std::fs::File::open(retry_selection.path()).unwrap(), 64).unwrap();
    assert_eq!(bytes, b"exact selected snapshot");
    assert_eq!(retry_selection.path(), path);
    std::fs::remove_file(path).unwrap();
    drop(owner);
    assert!(!Authority::permits(&retry));
}

mod entities {
    use super::super::*;
    use ::core::prelude::v1::test;
    use std::cell::RefCell;

    struct Held {
        path: PathBuf,
        ticket: Arc<std::sync::atomic::AtomicBool>,
        complete: async_channel::Sender<DecodeResult>,
    }

    fn held_decoder() -> (Decoder, Rc<RefCell<Vec<Held>>>) {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let capture = requests.clone();
        let decoder: Decoder = Rc::new(move |path, _, ticket, cx| {
            let (complete, result) = async_channel::bounded(1);
            capture.borrow_mut().push(Held {
                path,
                ticket,
                complete,
            });
            cx.background_executor()
                .spawn(async move { result.recv().await.expect("held completion") })
        });
        (decoder, requests)
    }

    fn fresh(label: &str) -> DecodeResult {
        Ok((
            Arc::new(RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::new(1, 1),
            )])),
            label.into(),
        ))
    }

    #[gpui::test]
    fn actual_entity_rebind_retry_and_close_retire_held_decodes(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (decoder, requests) = held_decoder();
        // Accepted inventory is offline and disposable. Both selected paths
        // come from the same vault containment check used by Tree admission.
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("old.png"), b"held old fixture").unwrap();
        std::fs::write(vault.path().join("fresh.png"), b"held fresh fixture").unwrap();
        let old_path = reader_files::checked_path(vault.path(), "old.png").unwrap();
        let fresh_path = reader_files::checked_path(vault.path(), "fresh.png").unwrap();
        let mut old = None;
        let mut current = None;
        cx.add_window(|window, cx| {
            old = Some(cx.new(|cx| {
                Preview::with_decoder(
                    vault.path().into(),
                    "old.png".into(),
                    old_path.clone(),
                    decoder.clone(),
                    window,
                    cx,
                )
            }));
            current = Some(cx.new(|cx| {
                Preview::with_decoder(
                    vault.path().into(),
                    "fresh.png".into(),
                    fresh_path.clone(),
                    decoder.clone(),
                    window,
                    cx,
                )
            }));
            // The window need not render either preview to exercise its actual
            // entity tasks and release callbacks.
            gpui_component::Root::new(current.as_ref().unwrap().clone(), window, cx)
        });
        let current = current.unwrap();
        cx.run_until_parked();
        assert_eq!(requests.borrow().len(), 2);
        // Entity drops are flushed by an App update, not by driving only the
        // executor. Exercise the release callback before releasing its decode.
        let old_weak = old.as_ref().unwrap().downgrade();
        cx.update(|_| drop(old.take()));
        cx.run_until_parked();
        assert!(old_weak.upgrade().is_none(), "old entity was released");
        assert!(!authority::Authority::permits(&requests.borrow()[0].ticket));
        // Fresh completion is the positive control for publication.
        requests.borrow()[1]
            .complete
            .try_send(fresh("fresh accepted"))
            .unwrap();
        cx.run_until_parked();
        current.read_with(cx, |preview, _| {
            assert!(
                matches!(&preview.media, MediaState::Image(_, label) if label == "fresh accepted")
            );
        });
        // Completing a retired old attempt may find its receiver cancelled.
        let _ = requests.borrow()[0].complete.try_send(fresh("stale old"));
        cx.run_until_parked();
        current.read_with(cx, |preview, _| {
            assert!(
                matches!(&preview.media, MediaState::Image(_, label) if label == "fresh accepted")
            );
        });
        // Start another held attempt, then a failure exposes the same Retry
        // controller called by the button, never a new Tree-derived path.
        current.update(cx, |preview, cx| preview.start_media(cx));
        requests.borrow()[2]
            .complete
            .try_send(Err(anyhow::anyhow!("offline decode failure")))
            .unwrap();
        cx.run_until_parked();
        current.read_with(cx, |preview, _| {
            assert!(matches!(preview.media, MediaState::Message(_)))
        });
        current.update(cx, |preview, cx| preview.retry_media(cx));
        assert_eq!(requests.borrow()[3].path, fresh_path);
        assert!(!authority::Authority::permits(&requests.borrow()[2].ticket));
        requests.borrow()[3]
            .complete
            .try_send(fresh("retry accepted"))
            .unwrap();
        cx.run_until_parked();
        current.read_with(cx, |preview, _| {
            assert!(
                matches!(&preview.media, MediaState::Image(_, label) if label == "retry accepted")
            );
        });
        // A retained entity must reject a superseded attempt as well.
        current.update(cx, |preview, cx| preview.start_media(cx));
        current.update(cx, |preview, cx| preview.start_media(cx));
        assert!(!authority::Authority::permits(&requests.borrow()[4].ticket));
        requests.borrow()[5]
            .complete
            .try_send(fresh("newest accepted"))
            .unwrap();
        cx.run_until_parked();
        let _ = requests.borrow()[4]
            .complete
            .try_send(fresh("late superseded"));
        cx.run_until_parked();
        current.read_with(cx, |preview, _| {
            assert!(
                matches!(&preview.media, MediaState::Image(_, label) if label == "newest accepted")
            );
        });
    }
}

mod mounted {
    use super::super::*;
    use ::core::prelude::v1::test;
    use std::cell::RefCell;

    struct Fixture {
        reader: Entity<Reader>,
        root: PathBuf,
        directory: tempfile::TempDir,
    }

    fn draw(visual: &mut VisualTestContext) {
        visual.run_until_parked();
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
    }

    fn png(path: &Path) {
        image::RgbaImage::from_pixel(8, 8, image::Rgba([21, 34, 55, 255]))
            .save(path)
            .unwrap();
    }

    fn opts(f: &Fixture, root: &Path) -> Opts {
        Opts {
            vault: Some(root.to_owned()),
            note: Some("Start.md".into()),
            session_directory: Some(f.directory.path().join("state")),
            index_dir: Some(f.directory.path().join("index")),
            panel_settings_override: Some(f.directory.path().join("panels.json")),
            ..Default::default()
        }
    }

    fn mount(cx: &mut TestAppContext, broken: bool) -> (Fixture, &mut VisualTestContext) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Start.md"), "# Saved\n\nOriginal Ω\n").unwrap();
        if broken {
            std::fs::write(root.join("Alpha.png"), b"invalid PNG").unwrap();
        } else {
            png(&root.join("Alpha.png"));
        }
        png(&root.join("Beta.png"));
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_fonts::install(cx);
            reader_ui_state::install(&directory.path().join("state"), cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Start.md".into()),
                        session_directory: Some(directory.path().join("state")),
                        index_dir: Some(directory.path().join("index")),
                        panel_settings_override: Some(directory.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(1400.), px(900.)));
        draw(visual);
        let fixture = Fixture {
            reader: reader.unwrap(),
            root: root.canonicalize().unwrap(),
            directory,
        };
        fixture.reader.read_with(visual, |reader, _| {
            assert!(reader.document_ready());
            assert_eq!(reader.current_rel, "Start.md");
        });
        (fixture, visual)
    }

    fn select(f: &Fixture, rel: &str, visual: &mut VisualTestContext) {
        f.reader.update_in(visual, |reader, window, cx| {
            reader.reveal_in_tree(rel, window, cx);
            reader.tree.cursor = Some(rel.into());
            reader.tree_focus.focus(window, cx);
            cx.notify();
        });
        draw(visual);
        let selector = match rel {
            "Alpha.png" => "tree-row-Alpha.png",
            "Beta.png" => "tree-row-Beta.png",
            _ => panic!("unexpected fixture Tree selection"),
        };
        assert!(
            visual.debug_bounds(selector).is_some(),
            "actual Tree item painted"
        );
    }

    fn preview(f: &Fixture, visual: &mut VisualTestContext) -> Entity<Preview> {
        f.reader.read_with(visual, |reader, _| {
            assert!(reader.tree_preview.is_open());
            reader.tree_preview.preview.as_ref().unwrap().1.clone()
        })
    }

    #[gpui::test]
    fn quicklook38_tree_space_arrows_close_preserve_dirty_source_and_undo(cx: &mut TestAppContext) {
        let (f, visual) = mount(cx, false);
        let original = std::fs::read_to_string(f.root.join("Start.md")).unwrap();
        let input = f.reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader.editing.as_ref().unwrap().input().clone()
        });
        draw(visual);
        input.update_in(visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "Draft Ω\n", window, cx);
        });
        draw(visual);
        let (history, history_ix, content, stamp, range) =
            f.reader.read_with(visual, |reader, cx| {
                assert!(reader.source_is_dirty(cx), "dirty draft positive control");
                assert!(reader.file_preview.is_none());
                (
                    reader.navigation.history.clone(),
                    reader.navigation.history_ix,
                    reader.content.clone(),
                    input.read(cx).source_stamp(),
                    input.read(cx).selected_range(),
                )
            });
        select(&f, "Alpha.png", visual);
        visual.simulate_keystrokes("space");
        draw(visual);
        assert!(visual.debug_bounds("tree-readonly-quick-look").is_some());
        assert!(
            visual.debug_bounds("quick-look-image").is_some(),
            "real bounded PNG painted"
        );
        for (key, expected) in [("down", "Beta.png"), ("up", "Alpha.png")] {
            visual.simulate_keystrokes(key);
            draw(visual);
            f.reader.update_in(visual, |reader, window, cx| {
                assert_eq!(reader.tree.cursor.as_deref(), Some(expected));
                let (rel, preview) = reader.tree_preview.preview.as_ref().unwrap();
                assert_eq!(rel, expected);
                assert_eq!(preview.read(cx).selected.path(), f.root.join(expected));
                assert!(reader.tree_focus.is_focused(window));
            });
        }
        for key in ["space", "escape"] {
            visual.simulate_keystrokes(key);
            draw(visual);
            assert!(visual.debug_bounds("tree-readonly-quick-look").is_none());
            f.reader.update_in(visual, |reader, window, cx| {
                assert!(!reader.tree_preview.is_open());
                assert!(reader.tree_preview.preview.is_none());
                assert!(reader.tree_focus.is_focused(window));
                assert_eq!(reader.current_rel, "Start.md");
                assert_eq!(reader.navigation.history, history);
                assert_eq!(reader.navigation.history_ix, history_ix);
                assert_eq!(reader.content, content);
                assert!(reader.file_preview.is_none());
                assert_eq!(reader.editing.as_ref().unwrap().input(), &input);
                assert_eq!(
                    input.read(cx).value().as_ref(),
                    format!("Draft Ω\n{original}")
                );
                assert_eq!(input.read(cx).source_stamp(), stamp);
                assert_eq!(input.read(cx).selected_range(), range);
            });
            assert_eq!(
                std::fs::read_to_string(f.root.join("Start.md")).unwrap(),
                original
            );
            if key == "space" {
                visual.simulate_keystrokes("space");
                draw(visual);
                assert!(visual.debug_bounds("tree-readonly-quick-look").is_some());
            }
        }
        input.update_in(visual, |input, window, cx| {
            gpui::Focusable::focus_handle(input, cx).focus(window, cx);
        });
        visual.dispatch_action(gpui_component::input::Undo);
        draw(visual);
        input.read_with(visual, |input, _| {
            assert_eq!(input.value().as_ref(), original)
        });
        visual.dispatch_action(gpui_component::input::Redo);
        draw(visual);
        input.read_with(visual, |input, _| {
            assert_eq!(input.value().as_ref(), format!("Draft Ω\n{original}"));
        });
    }

    struct DecoderReset;
    impl Drop for DecoderReset {
        fn drop(&mut self) {
            NEXT_DECODER.with(|slot| slot.borrow_mut().take());
        }
    }

    fn inject(path: PathBuf, decoder: Decoder) -> DecoderReset {
        NEXT_DECODER.with(|slot| {
            assert!(slot.borrow().is_none(), "one pending offline injection");
            *slot.borrow_mut() = Some((path, decoder));
        });
        DecoderReset
    }

    #[gpui::test]
    fn quicklook38_actual_retry_click_uses_captured_path_without_navigation(
        cx: &mut TestAppContext,
    ) {
        let (f, visual) = mount(cx, true);
        let saved = std::fs::read(f.root.join("Start.md")).unwrap();
        let history = f
            .reader
            .read_with(visual, |reader, _| reader.navigation.history.clone());
        let paths = Rc::new(RefCell::new(Vec::new()));
        let capture = paths.clone();
        let real = decoder();
        let record: Decoder = Rc::new(move |path, kind, ticket, cx| {
            capture.borrow_mut().push(path.clone());
            real(path, kind, ticket, cx)
        });
        let path = reader_files::checked_path(&f.root, "Alpha.png").unwrap();
        let _reset = inject(path.clone(), record);
        select(&f, "Alpha.png", visual);
        visual.simulate_keystrokes("space");
        draw(visual);
        let retry = visual
            .debug_bounds("quick-look-retry")
            .expect("actual decode failure exposes painted Retry");
        assert_eq!(&*paths.borrow(), std::slice::from_ref(&path));
        png(&path);
        // A changed Tree cursor is not a new preview selection. Retry must use
        // its captured selection, rather than read this unrelated cursor.
        f.reader.update_in(visual, |reader, _, cx| {
            reader.tree.cursor = Some("Beta.png".into());
            cx.notify();
        });
        visual.simulate_click(retry.center(), Modifiers::default());
        draw(visual);
        assert_eq!(&*paths.borrow(), &[path.clone(), path]);
        assert!(
            visual.debug_bounds("quick-look-image").is_some(),
            "real repaired PNG positive control"
        );
        assert!(visual.debug_bounds("quick-look-retry").is_none());
        preview(&f, visual).read_with(visual, |preview, _| {
            assert_eq!(preview.rel, "Alpha.png");
            assert!(matches!(preview.media, MediaState::Image(_, _)));
        });
        f.reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Start.md");
            assert_eq!(reader.navigation.history, history);
            assert!(reader.file_preview.is_none());
        });
        assert_eq!(std::fs::read(f.root.join("Start.md")).unwrap(), saved);
    }

    struct Held {
        path: PathBuf,
        ticket: Arc<std::sync::atomic::AtomicBool>,
        complete: async_channel::Sender<DecodeResult>,
    }

    fn held() -> (Decoder, Rc<RefCell<Vec<Held>>>) {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let capture = requests.clone();
        let decoder: Decoder = Rc::new(move |path, _, ticket, cx| {
            let (complete, result) = async_channel::bounded(1);
            capture.borrow_mut().push(Held {
                path,
                ticket,
                complete,
            });
            cx.background_executor()
                .spawn(async move { result.recv().await.unwrap() })
        });
        (decoder, requests)
    }

    fn image_result(label: &str) -> DecodeResult {
        Ok((
            Arc::new(RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::new(1, 1),
            )])),
            label.into(),
        ))
    }

    #[gpui::test]
    fn quicklook38_held_decode_close_and_accepted_root_rebind_refuse_stale_reply(
        cx: &mut TestAppContext,
    ) {
        let (f, visual) = mount(cx, false);
        let (decoder, requests) = held();
        let path = reader_files::checked_path(&f.root, "Alpha.png").unwrap();
        let _reset = inject(path.clone(), decoder.clone());
        select(&f, "Alpha.png", visual);
        visual.simulate_keystrokes("space");
        draw(visual);
        assert_eq!(requests.borrow().len(), 1, "Tree action reached decoder");
        assert!(visual.debug_bounds("tree-readonly-quick-look").is_some());
        let closed = preview(&f, visual).downgrade();
        assert!(authority::Authority::permits(&requests.borrow()[0].ticket));
        visual.simulate_keystrokes("escape");
        draw(visual);
        assert!(
            closed.upgrade().is_none(),
            "close drops the actual Tree-owned entity"
        );
        assert!(!authority::Authority::permits(&requests.borrow()[0].ticket));
        let _ = requests.borrow()[0]
            .complete
            .try_send(image_result("closed stale"));
        draw(visual);
        assert!(visual.debug_bounds("tree-readonly-quick-look").is_none());

        let _reset = inject(path, decoder.clone());
        visual.simulate_keystrokes("space");
        draw(visual);
        assert_eq!(requests.borrow().len(), 2);
        let rebound = preview(&f, visual).downgrade();
        let next_root = f.directory.path().join("other-vault");
        std::fs::create_dir(&next_root).unwrap();
        std::fs::write(next_root.join("Start.md"), "# New root\n").unwrap();
        png(&next_root.join("Alpha.png"));
        let next_root = next_root.canonicalize().unwrap();
        f.reader.update_in(visual, |reader, window, cx| {
            reader.start_loading(opts(&f, &next_root), window, cx);
        });
        draw(visual);
        f.reader.read_with(visual, |reader, _| {
            assert!(
                reader.document_ready(),
                "accepted new-root publication positive control"
            );
            assert_eq!(reader.vault_root, next_root);
            assert!(!reader.tree_preview.is_open());
            assert!(reader.tree_preview.preview.is_none());
        });
        assert!(rebound.upgrade().is_none());
        assert!(!authority::Authority::permits(&requests.borrow()[1].ticket));
        let next_path = reader_files::checked_path(&next_root, "Alpha.png").unwrap();
        let _reset = inject(next_path.clone(), decoder);
        select(&f, "Alpha.png", visual);
        visual.simulate_keystrokes("space");
        draw(visual);
        assert_eq!(requests.borrow().len(), 3);
        assert_eq!(requests.borrow()[2].path, next_path);
        requests.borrow()[2]
            .complete
            .try_send(image_result("fresh new root"))
            .unwrap();
        draw(visual);
        assert!(visual.debug_bounds("quick-look-image").is_some());
        let _ = requests.borrow()[1]
            .complete
            .try_send(image_result("rebound stale"));
        draw(visual);
        preview(&f, visual).read_with(visual, |preview, _| {
            assert_eq!(preview.selected.path(), next_root.join("Alpha.png"));
            assert!(
                matches!(&preview.media, MediaState::Image(_, label) if label == "fresh new root")
            );
        });
    }
}
