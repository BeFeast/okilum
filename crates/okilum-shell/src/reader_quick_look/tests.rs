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
