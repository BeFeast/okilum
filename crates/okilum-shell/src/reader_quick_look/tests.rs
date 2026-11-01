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
