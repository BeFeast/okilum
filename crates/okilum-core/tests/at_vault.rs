//! Synthetic @ conventions and ordinary underscore paths; all source data is public and invented.
#[path = "support/at_vault.rs"]
mod fixture;
use okilum_core::{quick_open, render, vault::EntryKind, Resolution, Vault};
use std::path::Path;

#[test]
fn windows_issue2821_assets_are_not_notes_but_resolve_and_render() {
    let root = tempfile::tempdir().unwrap();
    fixture::populate(root.path());
    let vault = Vault::scan(root.path()).unwrap();
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
        ("./@index", "Area/@index.md"),
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
    assert_eq!(
        restored
            .vault()
            .resolve_from("a.excalidraw.md", "Area/Links.md"),
        Resolution::Resolved {
            path: "@Assets/Excalidraw/a.excalidraw.md".into()
        }
    );
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
    assert!(
        !paths.contains(&"Legacy/_index.md"),
        "underscore is not a project index"
    );
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
        Path::new("@Schema/Templates")
    );
    assert!(agent_imports::is_agent_document("@AgentContract.md"));
    assert!(!agent_imports::is_agent_document("Area/@AgentContract.md"));
    assert!(quick_open::FileFilter::Default.admits("@AgentContract.md"));
    // #2939: Claude Code ignores `@@name`; only `@./@name` imports the contract.
    let source = "@@AgentContract.md\n@./@AgentContract.md\n```\n@@x\n```\n";
    let imports = agent_imports::parse(source);
    assert_eq!(
        imports
            .iter()
            .map(|import| import.target.as_str())
            .collect::<Vec<_>>(),
        ["./@AgentContract.md"]
    );
    let ignored = agent_imports::ignored_bounded(source, usize::MAX);
    assert_eq!(
        ignored
            .iter()
            .map(|import| import.target.as_str())
            .collect::<Vec<_>>(),
        ["@AgentContract.md"],
        "fenced @@x is neither an import nor a hint"
    );
    assert!(agent_imports::parse("```\n@@x\n```\n").is_empty());
    let rules = agent_rules::discover(root.path(), &root.path().join("Area"));
    assert!(rules
        .layers
        .iter()
        .any(|layer| layer.path == Path::new("@AgentContract.md") && layer.source.is_some()));
    let imported: Vec<_> = rules
        .layers
        .iter()
        .filter_map(|layer| match &layer.provenance {
            agent_rules::Provenance::Import { from, line } => {
                Some((from.as_path(), *line, layer.path.as_path()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        imported,
        [(Path::new("AGENTS.md"), 2, Path::new("@AgentContract.md"))],
        "line 1 (@@AgentContract.md) must not add a layer"
    );
    assert_eq!(
        rules.diagnostics,
        ["AGENTS.md:1: Not an import: Claude Code ignores @@AgentContract.md. Write @./@AgentContract.md."]
    );
}

#[test]
fn windows_issue2821_at_data_syncs_and_deep_links_round_trip() {
    use okilum_core::{deep_link, syncthing_ignore};
    for service in [
        ".okilum/settings.json",
        ".obsidian/app.json",
        ".okilum-index/cache",
    ] {
        assert!(
            okilum_core::vault::service_path(Path::new(service)),
            "service positive control: {service}"
        );
    }
    let matchers: Vec<_> = syncthing_ignore::PATTERNS
        .iter()
        .map(|pattern| globset::Glob::new(pattern).unwrap().compile_matcher())
        .collect();
    assert!(matchers[0].is_match(".okilum-index"));
    assert!(matchers[1].is_match(".okilum-save-positive-control.tmp"));
    for path in [
        "@Assets/Images/a.png",
        "@Assets/Excalidraw/a.excalidraw.md",
        "@Schema/language.yaml",
        "@index.md",
        "@AgentContract.md",
    ] {
        assert!(!okilum_core::vault::service_path(Path::new(path)));
        for matcher in &matchers {
            assert!(
                !path.split('/').any(|segment| matcher.is_match(segment)),
                "built-in ignore matched @ data: {path}"
            );
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
    let canonical_root = root.path().canonicalize().unwrap();
    let (vault, snapshot, _) =
        warm::reconcile(&canonical_root, None, false, &mut |_, _| Ok(())).unwrap();
    let cache = tempfile::tempdir().unwrap();
    snapshot.save(cache.path()).unwrap();
    let snapshot = warm::Snapshot::load(cache.path(), &canonical_root).unwrap();
    let cached_vault = snapshot.vault();
    assert_eq!(
        vault
            .notes
            .iter()
            .map(|note| (&note.path, &note.title))
            .collect::<Vec<_>>(),
        cached_vault
            .notes
            .iter()
            .map(|note| (&note.path, &note.title))
            .collect::<Vec<_>>()
    );
    let mut state = State::new(cached_vault, snapshot);
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
        assert!(
            state
                .vault
                .resolve_asset(path.rsplit('/').next().unwrap(), "Area/Links.md")
                .is_none(),
            "deleted asset name lookup must also be cleared"
        );
        assert_eq!(
            state.vault.resolve_from(path, "Area/Links.md"),
            Resolution::Unresolved
        );
    }
}

#[test]
fn windows_issue2821_only_new_bare_at_index_prefers_source_folder() {
    let root = tempfile::tempdir().unwrap();
    for (path, source) in [
        ("@Index.md", "# Root current"),
        ("_Index.md", "# Root legacy"),
        ("Area/@INDEX.md", "# Local current"),
        ("Area/_index.md", "# Local legacy"),
        ("guide.md", "# Root guide"),
        ("Area/guide.md", "# Local guide"),
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let vault = Vault::scan(root.path()).unwrap();
    for (target, from, expected) in [
        ("@index", "Area/Links.md", "Area/@INDEX.md"),
        ("@index.md#Heading", "Area/Links.md", "Area/@INDEX.md"),
        ("@index", "NoLocal/Links.md", "@Index.md"),
        ("_index", "Area/Links.md", "_Index.md"),
        ("guide", "Area/Links.md", "guide.md"),
        ("./@index", "Area/Links.md", "Area/@INDEX.md"),
        ("/@index", "Area/Links.md", "@Index.md"),
        ("Area/@index", "NoLocal/Links.md", "Area/@INDEX.md"),
    ] {
        assert_eq!(
            vault.resolve_from(target, from),
            Resolution::Resolved {
                path: expected.into()
            },
            "{target} from {from}"
        );
    }
    let ambiguous = Vault::from_note_paths(
        ["@Index.md", "Area/@index.md", "Area/@INDEX.md"]
            .into_iter()
            .map(str::to_owned),
    );
    assert!(
        matches!(ambiguous.resolve_from("@index", "Area/Links.md"), Resolution::Ambiguous { candidates } if candidates.len() == 2)
    );
}

#[test]
fn windows_issue2847_underscore_notes_remain_in_inventory_search_and_snapshots() {
    use okilum_core::{agent_imports, vault::warm};
    let root = tempfile::tempdir().unwrap();
    fixture::populate(root.path());
    let (vault, snapshot, _) =
        warm::reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
    for path in [
        "_Private/x.md",
        "_Assets/Templates/Old.md",
        "_Assets/Daily Notes/day.md",
        "Legacy/_index.md",
        "_AgentContract.md",
        "_index.md",
    ] {
        assert!(
            vault.notes.iter().any(|note| note.path == path),
            "ordinary note inventory: {path}"
        );
        assert!(
            snapshot.source_paths().any(|source| source == path),
            "ordinary indexed source: {path}"
        );
        assert!(snapshot.source(path).is_some());
        assert_eq!(
            vault.resolve_from(path, "Area/Links.md"),
            Resolution::Resolved { path: path.into() }
        );
    }
    assert!(!agent_imports::is_agent_document("_AgentContract.md"));
    assert!(vault
        .entries
        .iter()
        .any(|entry| entry.path == "_export/data.json" && entry.kind == EntryKind::Attachment));
}
