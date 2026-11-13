//! Cross-surface checks using the same public synthetic fixture as core.
use super::*;
use crate::note_paste::{
    attachments::{self, AttachmentKind},
    files::{self, FileAction},
};
use ::core::prelude::v1::test;
#[path = "../../okilum-core/tests/support/at_vault.rs"]
mod fixture;

#[test]
fn mixed_at_tree_visibility_indexes_reveal_and_attachment_names() {
    use reader_tree::{FolderIndex, Tree};
    let root = tempfile::tempdir().unwrap();
    let case_sensitive = fixture::populate(root.path());
    let vault = Vault::scan(root.path()).unwrap();
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
        "@Schema",
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
    assert!(tree.hidden_by_preference("@Schema/Templates/Note.md"));
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
        "@Schema",
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
        ["_index.md", "_Index.md", "@INDEX.md"],
        ["_index.md", "@index.md", "@Index.md"],
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
    let linked = files::plan(
        std::slice::from_ref(&original),
        root.path(),
        "a.md",
        "stamp",
        None,
    );
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

#[gpui::test]
fn at_root_header_and_mixed_folder_activation_use_actual_index(cx: &mut gpui::TestAppContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    fixture::populate(&root);
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Area/Links.md".into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    reader.update(visual, |view, cx| {
        view.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        view.panels.open(reader_layout::Panel::Notes);
        view.sync_tree();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("tree-vault-index").is_some(),
        "root index header must be rendered"
    );
    let header = visual.debug_bounds("tree-vault-index").unwrap();
    visual.simulate_click(header.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(view.current_rel, "@Index.md");
        assert_eq!(view.vault_index_path().as_deref(), Some("@Index.md"));
        assert_eq!(view.backlink_title("Area/@index.md").0, "Area");
    });
    reader.update_in(visual, |view, window, cx| {
        view.sync_tree();
        let row = view
            .tree
            .rows
            .iter()
            .find(|row| row.path == "Mixed")
            .unwrap()
            .clone();
        view.activate_tree_row(&row, window, cx);
    });
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(view.current_rel, "Mixed/@index.md");
        assert!(
            view.link_notice.is_none(),
            "one @ index must suppress legacy ambiguity"
        );
    });
    reader.update(visual, |view, _| {
        let asset = "@Assets/Clippings/Nested/clip.md".to_owned();
        let note = "@Private/y.md".to_owned();
        view.sidebar.recent = vec![(asset.clone(), 1), (note.clone(), 1)];
        view.sidebar.pinned = vec![asset.clone(), note.clone()];
        view.tree.set_show_hidden(true);
        for menu in [
            reader_title_nav::Menu::Recent,
            reader_title_nav::Menu::Pinned,
        ] {
            let listed = view.nav_entries(menu, 2);
            assert!(
                listed.iter().all(|entry| entry.path != asset),
                "history cannot turn a Markdown asset into a note"
            );
            assert!(
                listed.iter().any(|entry| entry.path == note),
                "ordinary @ notes remain available with Show hidden on"
            );
        }
    });
}
