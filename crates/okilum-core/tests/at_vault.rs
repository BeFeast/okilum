//! Synthetic mixed/legacy compatibility; all source data is public and invented.
#[path = "support/at_vault.rs"]
mod fixture;
use okilum_core::{quick_open, render, vault::EntryKind, Resolution, Vault};
use std::path::Path;

#[test]
fn windows_issue2821_assets_are_not_notes_but_resolve_and_render() {
    let root = tempfile::tempdir().unwrap();
    fixture::populate(root.path());
    let vault = Vault::load(root.path()).unwrap();
    for asset in [
        "@Assets/Excalidraw/a.excalidraw.md",
        "@Assets/Clippings/Nested/clip.md",
    ] {
        assert!(
            !vault.notes.iter().any(|note| note.path == asset),
            "@Assets Markdown must not enter note lists or counts"
        );
        assert!(vault
            .entries
            .iter()
            .any(|entry| entry.path == asset && entry.kind == EntryKind::Attachment));
        assert_eq!(
            vault.resolve_from(asset, "Area/Links.md"),
            Resolution::Resolved { path: asset.into() }
        );
        assert!(!quick_open::inventory(vault.notes.clone(), &vault.entries)
            .iter()
            .any(|note| note.path == asset));
        assert!(vault.read_note(asset).unwrap().contains("Assetonlyneedle"));
    }
    assert!(vault.notes.iter().any(|note| note.path == "@Private/y.md"));
    assert!(vault
        .notes
        .iter()
        .any(|note| note.path == "_Assets/Daily Notes/day.md"));
    let document = render::reader_document(&vault, "Area/Links.md").unwrap();
    assert!(
        document.rendered.contains("Assetonlyneedle"),
        "Markdown asset embeds must still expand: {}",
        document.rendered
    );
    assert_eq!(
        vault.resolve_asset("a.png", "Area/Links.md"),
        Some(root.path().join("@Assets/Images/a.png"))
    );
}

#[test]
fn windows_issue2821_mixed_inventory_links_search_and_warm_sources() {
    use okilum_core::search::{SearchDocument, Searcher};
    let root = tempfile::tempdir().unwrap();
    fixture::populate(root.path());
    let (vault, snapshot, _) =
        okilum_core::vault::warm::reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
    for (target, expected) in [
        ("Area/@index", "Area/@index.md"),
        ("@index", "Area/@index.md"),
        ("@AgentContract", "@AgentContract.md"),
        ("a.excalidraw.md", "@Assets/Excalidraw/a.excalidraw.md"),
    ] {
        assert_eq!(
            vault.resolve_from(target, "Area/Links.md"),
            Resolution::Resolved {
                path: expected.into()
            },
            "{target}"
        );
    }
    assert!(snapshot
        .source("@Assets/Clippings/Nested/clip.md")
        .unwrap()
        .contains("Assetonlyneedle"));
    assert!(!snapshot
        .source_paths()
        .any(|path| path.starts_with("@Assets/")));
    let documents: Vec<_> = snapshot
        .sources()
        .unwrap()
        .into_iter()
        .map(|(path, bytes)| SearchDocument {
            title: Vault::title_of(&path),
            path,
            text: String::from_utf8(bytes).unwrap(),
        })
        .collect();
    let search =
        Searcher::build_snapshot_in_memory(&vault, &documents, &mut |_, _| Ok(())).unwrap();
    assert_eq!(
        search.search("Uniqueindexneedle", 20).unwrap()[0].path,
        "Area/@index.md"
    );
    assert_eq!(
        search.search("Ordinaryatneedle", 20).unwrap()[0].path,
        "@Private/y.md"
    );
    assert!(search.search("Assetonlyneedle", 20).unwrap().is_empty());
    search
        .update_snapshot_batch(&vault, &documents, &[])
        .unwrap();
    assert!(
        search.search("Assetonlyneedle", 20).unwrap().is_empty(),
        "incremental publication must also exclude Markdown assets"
    );
    let cache = tempfile::tempdir().unwrap();
    snapshot.save(cache.path()).unwrap();
    let restored = okilum_core::vault::warm::Snapshot::load(cache.path(), root.path()).unwrap();
    assert!(!restored
        .vault()
        .notes
        .iter()
        .any(|note| note.path.starts_with("@Assets/")));
    assert!(restored
        .source("@Assets/Excalidraw/a.excalidraw.md")
        .is_some());
}

#[test]
fn windows_issue2821_links_rewrite_on_index_contract_and_asset_rename() {
    use okilum_core::link_rewrite::Preview;
    let root = tempfile::tempdir().unwrap();
    fixture::populate(root.path());
    for (from, to, expected) in [
        ("Area/@index.md", "Area/Guide.md", "Guide"),
        ("@AgentContract.md", "@Rules.md", "@Rules"),
        (
            "@Assets/Images",
            "@Assets/Pictures",
            "@Assets/Pictures/a.png",
        ),
        (
            "@Assets/Excalidraw/a.excalidraw.md",
            "@Assets/Excalidraw/b.excalidraw.md",
            "b.excalidraw",
        ),
    ] {
        let preview = Preview::prepare(root.path(), from, to).unwrap();
        assert!(
            !preview.changes.is_empty(),
            "rewrite control did not fire for {from}"
        );
        let rewritten = preview.rewritten("Area/Links.md").unwrap();
        assert!(rewritten.contains(expected), "{from}: {rewritten}");
        preview.validate(root.path()).unwrap();
    }
}

#[test]
fn windows_issue2821_projects_templates_and_agent_imports() {
    use okilum_core::{agent_imports, agent_rules, note_templates, projects};
    let root = tempfile::tempdir().unwrap();
    fixture::populate(root.path());
    let (_, snapshot, _) =
        okilum_core::vault::warm::reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
    let projects = projects::Index::from_snapshot(&snapshot);
    let paths: Vec<_> = projects
        .rows()
        .iter()
        .map(|project| project.path.as_str())
        .collect();
    assert!(paths.contains(&"Area/@index.md"));
    assert!(paths.contains(&"Legacy/_index.md"));
    assert!(paths.contains(&"Mixed/@index.md"));
    assert!(
        !paths.contains(&"Mixed/_index.md"),
        "mixed project must have one identity"
    );
    assert_eq!(
        projects
            .rows()
            .iter()
            .find(|project| project.path == "Area/@index.md")
            .unwrap()
            .open_tasks,
        1
    );
    assert_eq!(
        note_templates::Catalog::load(root.path()).unwrap().folder,
        Path::new("@Schema/Templates")
    );
    std::fs::remove_dir_all(root.path().join("@Schema/Templates")).unwrap();
    assert_eq!(
        note_templates::Catalog::load(root.path()).unwrap().folder,
        Path::new("_Assets/Templates")
    );
    assert!(agent_imports::is_agent_document("@AgentContract.md"));
    assert!(!agent_imports::is_agent_document("Area/@AgentContract.md"));
    assert!(quick_open::FileFilter::Default.admits("@AgentContract.md"));
    let imports = agent_imports::parse("@@AgentContract.md\n@./@AgentContract.md\n");
    assert_eq!(
        imports
            .iter()
            .map(|import| import.target.as_str())
            .collect::<Vec<_>>(),
        ["@AgentContract.md", "./@AgentContract.md"]
    );
    let rules = agent_rules::discover(root.path(), &root.path().join("Area"));
    assert!(rules
        .layers
        .iter()
        .any(|layer| layer.path == Path::new("@AgentContract.md") && layer.source.is_some()));
    assert!(rules.diagnostics.is_empty(), "{:?}", rules.diagnostics);
}

#[test]
fn windows_issue2821_at_data_syncs_and_deep_links_round_trip() {
    use okilum_core::{deep_link, syncthing_ignore};
    for path in [
        "@Assets/Images/a.png",
        "@Assets/Excalidraw/a.excalidraw.md",
        "@Schema/language.yaml",
        "@index.md",
        "@AgentContract.md",
    ] {
        assert!(!okilum_core::vault::service_path(Path::new(path)));
        for pattern in syncthing_ignore::PATTERNS {
            // These are the only two built-in patterns, both literal dot prefixes.
            assert!(pattern.starts_with('.'));
            assert!(!path
                .split('/')
                .any(|segment| segment.starts_with(pattern.trim_end_matches('*'))));
        }
        let position = deep_link::Position::default();
        let url = deep_link::build("Synthetic", path, &position);
        assert!(url.contains("%40"));
        assert_eq!(
            deep_link::parse(&url).unwrap().address,
            deep_link::Address::Vault {
                vault: "Synthetic".into(),
                path: path.into()
            }
        );
    }
}

#[test]
fn windows_issue2821_incremental_asset_create_update_and_directory_delete() {
    use okilum_core::{
        vault::warm::{self, incremental::State},
        Changes,
    };
    let root = tempfile::tempdir().unwrap();
    fixture::populate(root.path());
    let (vault, snapshot, _) =
        warm::reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
    let mut state = State::new(vault, snapshot);
    let path = "@Assets/Clippings/Nested/new.md";
    std::fs::write(root.path().join(path), "# Created asset").unwrap();
    state
        .apply(
            &Changes {
                directories: ["@Assets/Clippings/Nested".into()].into(),
                ..Default::default()
            },
            &mut |_, _| Ok(()),
        )
        .unwrap();
    assert!(state
        .snapshot
        .source(path)
        .unwrap()
        .contains("Created asset"));
    assert!(!state.vault.notes.iter().any(|note| note.path == path));
    std::fs::write(root.path().join(path), "# Updated asset").unwrap();
    state
        .apply(
            &Changes {
                changed: [path.into()].into(),
                ..Default::default()
            },
            &mut |_, _| Ok(()),
        )
        .unwrap();
    assert!(state
        .snapshot
        .source(path)
        .unwrap()
        .contains("Updated asset"));
    std::fs::remove_dir_all(root.path().join("@Assets/Clippings")).unwrap();
    state
        .apply(
            &Changes {
                directories: ["@Assets/Clippings".into()].into(),
                ..Default::default()
            },
            &mut |_, _| Ok(()),
        )
        .unwrap();
    for path in [path, "@Assets/Clippings/Nested/clip.md"] {
        assert!(
            state.snapshot.source(path).is_none(),
            "deleted assets must not retain ghost sources"
        );
        assert_eq!(
            state.vault.resolve_from(path, "Area/Links.md"),
            Resolution::Unresolved
        );
    }
}
