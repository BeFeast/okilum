//! Real APFS preflight, exclusive rename, link rewrite and crash-safe Undo.
use super::*;
use crate::link_rewrite::{Operation, Preview};
use std::{collections::BTreeMap, fs};

fn stored_names(root: &Path) -> Vec<OsString> {
    fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect()
}

#[test]
fn file_case_rename_keeps_resolvable_links_and_undo_uses_stored_spelling() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let body = "\u{feff}# Test\r\n";
    fs::write(root.path().join("test.md"), body).unwrap();
    fs::write(root.path().join("ref.md"), "[[test]]\n").unwrap();
    // Positive control: this native gate must really use an insensitive volume.
    let original = fs::symlink_metadata(root.path().join("test.md")).unwrap();
    let alias =
        fs::symlink_metadata(root.path().join("Test.md")).expect("case-insensitive APFS fixture");
    assert_eq!((original.dev(), original.ino()), (alias.dev(), alias.ino()));
    let preview = Preview::prepare(root.path(), "test.md", "Test.md").unwrap();
    let applied = preview
        .apply(root.path(), state.path(), &mut BTreeMap::new())
        .unwrap();
    assert!(applied.moved, "{:?}", applied.warning);
    assert!(applied.warning.is_none(), "{:?}", applied.warning);
    let names = stored_names(root.path());
    assert!(names.contains(&OsString::from("Test.md")));
    assert!(!names.contains(&OsString::from("test.md")));
    assert_eq!(
        fs::read(root.path().join("Test.md")).unwrap(),
        body.as_bytes()
    );
    assert_eq!(
        fs::read_to_string(root.path().join("ref.md")).unwrap(),
        "[[test]]\n"
    );
    // A wiki target which still resolves needs no source-text rewrite.
    assert_eq!(
        crate::vault::Vault::scan(root.path())
            .unwrap()
            .resolve_from("test", "ref.md")
            .path(),
        Some("Test.md")
    );
    // A crash between rename and journal acknowledgement must also be undoable.
    let mut operation: serde_json::Value =
        serde_json::from_slice(&fs::read(&applied.journal).unwrap()).unwrap();
    operation["complete"] = false.into();
    fs::write(&applied.journal, serde_json::to_vec(&operation).unwrap()).unwrap();
    Operation::revert(&applied.journal, state.path()).unwrap();
    assert!(stored_names(root.path()).contains(&OsString::from("test.md")));
    assert!(!stored_names(root.path()).contains(&OsString::from("Test.md")));
    assert_eq!(
        fs::read(root.path().join("test.md")).unwrap(),
        body.as_bytes()
    );
    assert_eq!(
        fs::read_to_string(root.path().join("ref.md")).unwrap(),
        "[[test]]\n"
    );
}

#[test]
fn folder_case_rename_keeps_resolvable_links_and_undo_preserves_assets() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("Security")).unwrap();
    fs::write(root.path().join("Security/test.md"), "# Test\r\n").unwrap();
    fs::write(root.path().join("Security/picture.png"), [0, 255, 13]).unwrap();
    fs::write(root.path().join("ref.md"), "[[Security/test]]\n").unwrap();
    assert!(
        root.path().join("security").is_dir(),
        "case-insensitive APFS fixture"
    );
    let preview = Preview::prepare(root.path(), "Security", "security").unwrap();
    let applied = preview
        .apply(root.path(), state.path(), &mut BTreeMap::new())
        .unwrap();
    assert!(applied.moved, "{:?}", applied.warning);
    assert!(applied.warning.is_none(), "{:?}", applied.warning);
    assert!(stored_names(root.path()).contains(&OsString::from("security")));
    assert!(!stored_names(root.path()).contains(&OsString::from("Security")));
    assert_eq!(
        fs::read_to_string(root.path().join("ref.md")).unwrap(),
        "[[Security/test]]\n"
    );
    assert_eq!(
        crate::vault::Vault::scan(root.path())
            .unwrap()
            .resolve_from("Security/test", "ref.md")
            .path(),
        Some("security/test.md")
    );
    assert_eq!(
        fs::read(root.path().join("security/picture.png")).unwrap(),
        [0, 255, 13]
    );
    Operation::revert(&applied.journal, state.path()).unwrap();
    assert!(stored_names(root.path()).contains(&OsString::from("Security")));
    assert!(!stored_names(root.path()).contains(&OsString::from("security")));
    assert_eq!(
        fs::read(root.path().join("Security/picture.png")).unwrap(),
        [0, 255, 13]
    );
    assert_eq!(
        fs::read_to_string(root.path().join("ref.md")).unwrap(),
        "[[Security/test]]\n"
    );
}

#[test]
fn case_rename_reports_a_source_replacement_without_overwriting_bytes() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("test.md"), "original").unwrap();
    let source = fs::symlink_metadata(root.path().join("test.md")).unwrap();
    let alias = fs::symlink_metadata(root.path().join("Test.md"))
        .expect("case-insensitive APFS race fixture");
    assert_eq!((source.dev(), source.ino()), (alias.dev(), alias.ino()));
    let plan = MovePlan::prepare(root.path(), Path::new("test.md"), Path::new("Test.md")).unwrap();
    let moved = plan
        .commit_before_rename(|| {
            fs::rename(root.path().join("test.md"), root.path().join("saved.md")).unwrap();
            fs::write(root.path().join("Test.md"), "external").unwrap();
        })
        .unwrap();
    // The syscall succeeded on the replacement alias. Reporting it as an
    // unmoved error would invite an unsafe retry; retain both writers' bytes
    // and require inspection through the existing post-rename warning.
    assert!(moved
        .warning
        .as_deref()
        .is_some_and(|warning| warning.contains("external writer")));
    assert_eq!(
        fs::read_to_string(root.path().join("Test.md")).unwrap(),
        "external"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("saved.md")).unwrap(),
        "original"
    );
}

#[test]
fn undo_case_rename_accepts_a_canonically_equivalent_journal_name() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(root.path().join("café.md"), "# Body\n").unwrap();
    // Start from the filesystem's stored form, as the sidebar does.
    let from = stored_names(root.path())
        .pop()
        .unwrap()
        .into_string()
        .unwrap();
    let preview = Preview::prepare(root.path(), &from, "Café.md").unwrap();
    let applied = preview
        .apply(root.path(), state.path(), &mut BTreeMap::new())
        .unwrap();
    assert!(applied.moved, "{:?}", applied.warning);
    assert!(applied.warning.is_none(), "{:?}", applied.warning);
    let mut operation: serde_json::Value =
        serde_json::from_slice(&fs::read(&applied.journal).unwrap()).unwrap();
    operation["to"] = "Cafe\u{301}.md".into();
    operation["complete"] = false.into();
    fs::write(&applied.journal, serde_json::to_vec(&operation).unwrap()).unwrap();
    Operation::revert(&applied.journal, state.path()).unwrap();
    assert!(stored_names(root.path()).contains(&OsString::from(&from)));
    assert_eq!(
        fs::read_to_string(root.path().join(&from)).unwrap(),
        "# Body\n"
    );
}
