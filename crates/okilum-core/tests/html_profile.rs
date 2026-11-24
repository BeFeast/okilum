use okilum_core::{html_profile, Vault};
use std::{fs, path::Path};

#[test]
fn mobile_note_html_contract() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/html-profile");
    let original = fixtures.join("vault");
    let copy = tempfile::tempdir().unwrap();
    for entry in walkdir::WalkDir::new(&original).min_depth(1) {
        let entry = entry.unwrap();
        let path = copy
            .path()
            .join(entry.path().strip_prefix(&original).unwrap());
        if entry.file_type().is_dir() {
            fs::create_dir(&path).unwrap();
        } else {
            fs::copy(entry.path(), path).unwrap();
        }
    }
    let vault = Vault::scan_metadata(copy.path()).unwrap();

    for note in ["nested/Note.md", "Other Note.md"] {
        let expected =
            fs::read_to_string(fixtures.join("expected").join(note).with_extension("html"))
                .unwrap();
        let html = html_profile::render_note(&vault, note).unwrap();
        assert_eq!(html, expected.replace("\r\n", "\n"), "{note}");
        assert_eq!(html, html_profile::render_note(&vault, note).unwrap());
    }
}

#[test]
fn mobile_profile_requires_a_vault_note() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("Note.md"), "# Note").unwrap();
    let outside = tmp.path().join("Outside.md");
    fs::write(&outside, "# Outside").unwrap();
    let vault = Vault::scan(&root).unwrap();

    for path in ["", "../Outside.md", outside.to_str().unwrap()] {
        assert!(html_profile::render_note(&vault, path).is_err(), "{path}");
    }

    #[cfg(unix)]
    {
        // A previously indexed path can be replaced after the vault scan.
        fs::remove_file(root.join("Note.md")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("Note.md")).unwrap();
        assert!(html_profile::render_note(&vault, "Note.md").is_err());
    }
}

const ACTIVITY: &str = include_str!("fixtures/html-profile/views/activity.json");

#[test]
fn mobile_view_is_independent_of_json_key_order() {
    let snapshot = serde_json::from_str(ACTIVITY).unwrap();
    let mut reordered: serde_json::Value = serde_json::from_str(ACTIVITY).unwrap();
    reordered.sort_all_objects();
    let reordered = serde_json::from_value(reordered).unwrap();
    assert_eq!(
        html_profile::render_view_snapshot(&snapshot),
        html_profile::render_view_snapshot(&reordered),
    );
}

#[test]
fn mobile_view_rejects_unsupported_versions_and_invalid_source_paths() {
    let original: serde_json::Value = serde_json::from_str(ACTIVITY).unwrap();
    for version in ["snapshot_version", "view_api"] {
        let mut value = original.clone();
        value[version] = 2.into();
        let error = serde_json::from_value::<html_profile::SnapshotJson>(value).unwrap_err();
        assert!(error
            .to_string()
            .contains("Unsupported view snapshot version pair"));
    }
    let mut value = original;
    value["sections"][0]["rows"][0]["path"] = "../private.md".into();
    let error = serde_json::from_value::<html_profile::SnapshotJson>(value).unwrap_err();
    assert!(error.to_string().contains("Invalid row source"));
}
