//! Reader OS-path boundaries, exercised on every host that runs core tests.
use okilum_core::{render, vault::note_path, Resolution, Vault};
use std::fs;

#[test]
fn scan_and_resolve_share_slash_identity_for_nested_unicode_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    fs::create_dir(&root).unwrap();
    let relative = std::path::Path::new("Заметки")
        .join("вложено")
        .join("цель.md");
    fs::create_dir_all(root.as_path().join(relative.parent().unwrap())).unwrap();
    fs::write(root.as_path().join(&relative), "# Target").unwrap();
    fs::write(root.as_path().join("Заметки/start.md"), "[[вложено/цель]]").unwrap();
    let vault = Vault::scan(root.as_path()).unwrap();
    let identity = "Заметки/вложено/цель.md";
    assert_eq!(note_path(&relative), identity);
    assert!(vault.notes.iter().any(|note| note.path == identity));
    assert_eq!(
        vault.resolve_from("вложено/цель", "Заметки/start.md"),
        Resolution::Resolved {
            path: identity.into()
        }
    );
    assert_eq!(vault.read_note(identity).unwrap(), "# Target");
    assert_eq!(vault.backlinks(identity)[0].path, "Заметки/start.md");
}

#[test]
fn local_image_urls_roundtrip_spaces_unicode_hash_and_percent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    fs::create_dir(&root).unwrap();
    let file = root.as_path().join("картинка # 100%.png");
    fs::write(&file, b"asset").unwrap();
    fs::write(
        root.as_path().join("start.md"),
        "![image](картинка%20%23%20100%25.png)",
    )
    .unwrap();
    let vault = Vault::scan(root.as_path()).unwrap();
    let expected = url::Url::from_file_path(&file).unwrap();
    let rendered =
        render::rewrite_source_images("![image](картинка%20%23%20100%25.png)", &vault, "start.md");
    assert_eq!(rendered, format!("![image]({expected})"));
    assert_eq!(
        url::Url::parse(expected.as_str())
            .unwrap()
            .to_file_path()
            .unwrap(),
        file
    );
    let html = render::render_html(&vault, "start.md", "base16-ocean.dark").unwrap();
    assert!(html.contains(expected.as_str()), "{html}");
}

#[cfg(unix)]
#[test]
fn unix_literal_backslash_is_not_a_directory_separator() {
    assert_eq!(
        note_path(std::path::Path::new(r"literal\name.md")),
        r"literal\name.md"
    );
}
