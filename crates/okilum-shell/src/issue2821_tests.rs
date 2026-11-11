//! Cross-surface checks using the same public synthetic fixture as core.
use super::*;
use crate::note_paste::{
    attachments::{self, AttachmentKind},
    files::{self, FileAction},
};
#[path = "../../okilum-core/tests/support/at_vault.rs"]
mod fixture;

#[test]
fn mixed_at_tree_visibility_indexes_reveal_and_attachment_names() {
    use reader_tree::{FolderIndex, Tree};
    let root = tempfile::tempdir().unwrap();
    let case_sensitive = fixture::populate(root.path());
    let vault = Vault::load(root.path()).unwrap();
    let mut tree = Tree::default();
    tree.refresh(root.path(), &vault.entries);
    for folder in ["Area", "Legacy", "Mixed", "Visible"] {
        tree.set_expanded(folder, true);
    }
    assert_eq!(
        reader_tree::folder_index("", &vault.entries),
        FolderIndex::One("@Index.md".into())
    );
    for (folder, index) in [
        ("Area", "Area/@index.md"),
        ("Legacy", "Legacy/_index.md"),
        ("Mixed", "Mixed/@index.md"),
    ] {
        assert_eq!(tree.index_of(folder), FolderIndex::One(index.into()));
        let first = tree
            .rows
            .iter()
            .find(|row| row.path.starts_with(&format!("{folder}/")))
            .unwrap();
        assert_eq!(first.path, index);
        assert!(first.index);
    }
    for path in [
        "@Assets",
        "@Private",
        "_Private",
        "@AgentContract.md",
        "Mixed/_index.md",
    ] {
        assert!(
            !tree.rows.iter().any(|row| row.path == path),
            "hidden with preference off: {path}"
        );
        assert!(tree.hidden_by_preference(path));
    }
    // The configured templates branch is the documented exception.
    assert!(tree.rows.iter().any(|row| row.path == "@Schema"));
    assert!(!tree.hidden_by_preference("@Schema/Templates/Note.md"));
    assert!(tree
        .rows
        .iter()
        .any(|row| row.path == "Visible/@data.csv" && !row.hidden));
    assert!(!tree.hidden_by_preference("Visible/@data.csv"));
    tree.reveal("@Private/y.md").unwrap();
    assert!(tree
        .rows
        .iter()
        .any(|row| row.path == "@Private/y.md" && row.hidden));
    tree.reveal("Area/Links.md").unwrap();
    assert!(!tree.rows.iter().any(|row| row.path == "@Private"));
    tree.set_show_hidden(true);
    for path in [
        "@Assets",
        "@Private",
        "_Private",
        "@AgentContract.md",
        "Mixed/_index.md",
    ] {
        assert!(
            tree.rows.iter().any(|row| row.path == path && row.hidden),
            "muted hidden row: {path}"
        );
        assert!(!tree.hidden_by_preference(path));
    }
    if case_sensitive {
        assert_eq!(tree.index_of("Twice"), FolderIndex::Several);
    }
    // Case-variant precedence also runs on case-insensitive hosts without creating collisions.
    for names in [
        vec!["_index.md", "_Index.md", "@INDEX.md"],
        vec!["_index.md", "@index.md", "@Index.md"],
    ] {
        let entries: Vec<_> = names
            .iter()
            .map(|path| okilum_core::vault::VaultEntry {
                path: (*path).into(),
                kind: okilum_core::vault::EntryKind::Markdown,
            })
            .collect();
        let expected = if names.len() == 3 && names[1] == "_Index.md" {
            FolderIndex::One("@INDEX.md".into())
        } else {
            FolderIndex::Several
        };
        assert_eq!(reader_tree::folder_index("", &entries), expected);
    }
}

#[test]
fn typed_attachment_priority_mixed_copy_and_existing_at_asset_link() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let image = outside.path().join("picture.png");
    let pdf = outside.path().join("report.pdf");
    let extensionless = outside.path().join("clipboard-image");
    std::fs::write(&extensionless, b"\x89PNG\r\n\x1a\nsynthetic").unwrap();
    assert_eq!(
        AttachmentKind::of_path(&extensionless),
        AttachmentKind::Image
    );
    std::fs::write(&image, b"synthetic image bytes").unwrap();
    std::fs::write(&pdf, b"%PDF-synthetic").unwrap();
    for (kind, expected) in [
        (AttachmentKind::Image, "@Assets/Images"),
        (AttachmentKind::File, "@Assets/Files"),
    ] {
        assert_eq!(
            attachments::attachment_folder(root.path(), "Notes/a.md", kind),
            root.path().join(expected)
        );
        assert!(
            !root.path().join(expected).exists(),
            "resolution does not create folders"
        );
    }
    std::fs::create_dir(root.path().join("attachments")).unwrap();
    assert_eq!(
        attachments::attachment_folder(root.path(), "a.md", AttachmentKind::Image),
        root.path().join("attachments")
    );
    std::fs::create_dir_all(root.path().join("_Assets/Attachments")).unwrap();
    assert_eq!(
        attachments::attachment_folder(root.path(), "a.md", AttachmentKind::File),
        root.path().join("_Assets/Attachments")
    );
    std::fs::create_dir(root.path().join("@Assets")).unwrap();
    let plan = files::plan(
        &[image.clone(), pdf.clone()],
        root.path(),
        "a.md",
        "stamp",
        None,
    );
    assert_eq!(plan.items[0].folder, root.path().join("@Assets/Images"));
    assert_eq!(plan.items[1].folder, root.path().join("@Assets/Files"));
    for item in &plan.items {
        let FileAction::Copy { file_name } = &item.action else {
            panic!("external file must copy")
        };
        let name = files::copy_into(&item.source, &item.folder, file_name, &mut |_| {}).unwrap();
        assert_eq!(
            std::fs::read(item.folder.join(name)).unwrap(),
            std::fs::read(&item.source).unwrap()
        );
    }
    let original = root.path().join("@Assets/Files/report.pdf");
    let linked = files::plan(&[original.clone()], root.path(), "a.md", "stamp", None);
    assert!(
        matches!(&linked.items[0].action, FileAction::Link { text } if text.contains("@Assets/Files/report.pdf"))
    );
    assert_eq!(linked.copy_bytes, 0);
    assert_eq!(std::fs::read(original).unwrap(), b"%PDF-synthetic");
    std::fs::create_dir(root.path().join(".obsidian")).unwrap();
    std::fs::write(
        root.path().join(".obsidian/app.json"),
        r#"{"attachmentFolderPath":"./Custom"}"#,
    )
    .unwrap();
    for kind in [AttachmentKind::Image, AttachmentKind::File] {
        assert_eq!(
            attachments::attachment_folder(root.path(), "Notes/a.md", kind),
            root.path().join("Notes/Custom")
        );
    }
}

#[test]
fn template_and_language_resolution_preserve_precedence() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    assert_eq!(
        reader_templates::load(root.path(), state.path()).unwrap(),
        "@Schema/Templates"
    );
    std::fs::create_dir_all(root.path().join("_Assets/Templates")).unwrap();
    assert_eq!(
        reader_templates::load(root.path(), state.path()).unwrap(),
        "_Assets/Templates"
    );
    std::fs::create_dir_all(root.path().join("@Schema/Templates")).unwrap();
    assert_eq!(
        reader_templates::load(root.path(), state.path()).unwrap(),
        "@Schema/Templates"
    );
    std::fs::create_dir(root.path().join(".obsidian")).unwrap();
    std::fs::write(
        root.path().join(".obsidian/templates.json"),
        r#"{"folder":"Obsidian"}"#,
    )
    .unwrap();
    assert_eq!(
        reader_templates::load(root.path(), state.path()).unwrap(),
        "Obsidian"
    );
    std::fs::create_dir(root.path().join("Chosen")).unwrap();
    reader_templates::save(root.path(), state.path(), &root.path().join("Chosen")).unwrap();
    assert_eq!(
        reader_templates::load(root.path(), state.path()).unwrap(),
        "Chosen"
    );
    assert_eq!(
        reader_agent_language::policy_path(root.path()),
        "_Schema/language.yaml"
    );
    std::fs::write(root.path().join("@Schema/language.yaml"), "rules: []").unwrap();
    assert_eq!(
        reader_agent_language::policy_path(root.path()),
        "@Schema/language.yaml"
    );
}
