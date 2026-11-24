#![cfg(any(unix, windows))]
use okilum_core::note_move::{DirectoryMovePlan, DirectorySnapshot, MovePlan};
use std::{fs, path::Path};

#[test]
fn vd4_review_note_move_refuses_both_archive_endpoints_after_preview() {
    for archive in ["Source", "Target"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("Source")).unwrap();
        fs::create_dir(root.join("Target")).unwrap();
        fs::write(root.join("Source/Note.md"), b"keep").unwrap();
        let from = Path::new("Source/Note.md");
        let to = Path::new("Target/Note.md");
        let plan = MovePlan::prepare(root, from, to).unwrap();
        fs::write(root.join(archive).join(".brain-read-only"), "").unwrap();
        let error = plan.commit().err().expect("archive move refused");
        assert!(error.to_string().contains("read-only archive"), "{error:#}");
        assert_eq!(fs::read(root.join(from)).unwrap(), b"keep");
        assert!(!root.join(to).exists());
    }
}

#[test]
fn vd4_review_folder_move_refuses_subtrees_containing_a_nested_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let from = Path::new("Parent");
    let to = Path::new("Moved");
    fs::create_dir_all(root.join("Parent/Archive")).unwrap();
    fs::write(root.join("Parent/Archive/.brain-read-only"), "").unwrap();
    fs::write(root.join("Parent/Archive/Note.md"), "keep").unwrap();
    let snapshot = DirectorySnapshot::read(root, from).unwrap();
    let result =
        DirectoryMovePlan::prepare(root, from, to, &snapshot).and_then(|plan| plan.commit());
    let error = result.err().expect("subtree move refused");
    assert!(
        error.to_string().contains("contains a separate vault"),
        "{error:#}"
    );
    assert!(root.join("Parent/Archive/Note.md").is_file());
    assert!(!root.join(to).exists());
}

#[test]
fn vd4_review_blank_nested_vault_names_fall_back_to_the_folder() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lifecycle-vault");
    let mut descriptor = okilum_core::vault_descriptor::read(&fixture).unwrap();
    let hash = "0".repeat(64);
    descriptor["layout"]["nested_roots"] = serde_json::json!([
        {"path":"Declared","kind":"vault","name":"","tree_sha256":hash,"files":1},
        {"path":"Spaced","kind":"vault","name":" \t ","tree_sha256":hash,"files":1},
        {"path":"Named","kind":"vault","name":"Team notes","tree_sha256":hash,"files":1}
    ]);
    fs::create_dir(root.join(".okilum")).unwrap();
    fs::write(
        okilum_core::vault_descriptor::path(root),
        serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    for folder in ["Declared", "Spaced", "Named", "Own/.okilum"] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    fs::write(
        okilum_core::vault_descriptor::path(&root.join("Own")),
        r#"{"name":"   "}"#,
    )
    .unwrap();
    for (folder, name) in [
        ("Declared", "Declared"),
        ("Spaced", "Spaced"),
        ("Own", "Own"),
        // Positive control: a real declared name is still shown.
        ("Named", "Team notes"),
    ] {
        let error = okilum_core::vault_mutation::check_relocation(root, Path::new(folder))
            .err()
            .unwrap_or_else(|| panic!("{folder} must be refused"));
        assert_eq!(
            error.to_string(),
            format!("The selected folder is a separate vault \"{name}\""),
            "{folder}"
        );
    }
}
