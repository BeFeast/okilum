// Pure tests: run under the Quick Look module or a harness exposing authority.
#[path = "../reader_plain_text/source.rs"]
mod codec;

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

#[path = "media.rs"]
mod media;

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
    assert!(media::PDF_BYTES > media::IMAGE_BYTES);
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
        old.take();
        cx.run_until_parked();
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
