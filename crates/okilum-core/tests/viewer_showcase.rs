//! Public synthetic showcase data exercises the production virtual-log selector.
use okilum_core::{log::virtual_log::Definition, render, vault::Vault};
use std::path::Path;

#[test]
fn showcase_virtual_log_selects_both_sources_without_mutating_them() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/viewers");
    let path = root.join("logs/sample.okilog");
    let before = std::fs::read_to_string(&path).unwrap();
    let definition = Definition::parse(&before).unwrap();
    let selection = definition.plan(&root, &path, 0).unwrap().select(|_, _| {});
    assert!(!selection.incomplete);
    assert_eq!(selection.files.len(), 2);
    assert_eq!(selection.merged.len(), 4);
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[test]
fn showcase_jpeg_and_bmp_embeds_resolve_to_real_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/viewers");
    let vault = Vault::scan(&root).unwrap();
    let rewritten = render::reader_source(&vault, "images/README.md").unwrap();
    for file in [
        "photo.jpg",
        "progressive.jpeg",
        "oriented.JPG",
        "bitmap.bmp",
    ] {
        let url = url::Url::from_file_path(root.canonicalize().unwrap().join("images").join(file))
            .unwrap();
        assert!(rewritten.contains(url.as_str()), "{file}: {rewritten}");
    }
    assert!(!render::has_unavailable_images(&rewritten));
}
