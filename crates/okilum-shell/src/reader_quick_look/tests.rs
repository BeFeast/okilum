// Also runnable directly with rustc --test: no native window or Cargo build.
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
