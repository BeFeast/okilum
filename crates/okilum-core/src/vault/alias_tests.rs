use super::*;
use crate::{properties, quick_open};

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("notes")).unwrap();
    for (path, source) in [
        ("notes/24q4.md", "---\ntype: Quarter\naliases: ['[[Q4 2024]]', Winter, 'שלום']\n---\n# Quarterly plans\n"),
        ("notes/25q1.md", "---\naliases: [Winter, Spring]\n---\n# Next quarter\n"),
        ("notes/Physical.md", "# Physical filename\n"),
        ("start.md", "[[Q4 2024#Plans]] [[Winter]] [[Spring]]\n"),
    ] { std::fs::write(dir.path().join(path), source).unwrap(); }
    dir
}

#[test]
fn aliases_resolve_vault_wide_and_keep_collision_and_path_rules() {
    let root = fixture();
    let mut vault = Vault::scan(root.path()).unwrap();
    assert_eq!(
        vault
            .resolve_from("q4 2024#Plans", "elsewhere/start.md")
            .path(),
        Some("notes/24q4.md")
    );
    assert_eq!(vault.resolve("שלום").path(), Some("notes/24q4.md"));
    assert_eq!(
        vault.resolve("Winter"),
        Resolution::Ambiguous {
            candidates: vec!["notes/24q4.md".into(), "notes/25q1.md".into()]
        }
    );
    assert_eq!(vault.resolve("Spring").path(), Some("notes/25q1.md"));
    assert_eq!(
        vault.resolve_from("./Q4 2024", "notes/start.md"),
        Resolution::Unresolved
    );
    assert_eq!(
        vault.resolve_markdown("Q4 2024.md", "start.md"),
        Resolution::Unresolved
    );
    assert_eq!(
        vault
            .backlinks("notes/24q4.md")
            .iter()
            .filter(|b| b.path == "start.md")
            .count(),
        1
    );
    assert!(vault
        .backlinks("notes/24q4.md")
        .iter()
        .all(|b| b.property.as_deref() != Some("aliases")));
    let original_revision = vault.link_inventory_revision();
    vault
        .aliases
        .get_mut("notes/24q4.md")
        .unwrap()
        .push("Physical".into());
    vault.rebuild_alias_map();
    assert_ne!(
        vault.link_inventory_revision(),
        original_revision,
        "link caches invalidate on alias-only edits"
    );
    assert_eq!(
        vault.resolve("Physical"),
        Resolution::Ambiguous {
            candidates: vec!["notes/24q4.md".into(), "notes/Physical.md".into()]
        }
    );
    assert_eq!(
        vault.resolve("notes/Physical").path(),
        Some("notes/Physical.md")
    );
    std::fs::write(root.path().join("Physical.md"), "# Root exact\n").unwrap();
    let vault = Vault::scan(root.path()).unwrap();
    assert_eq!(
        vault.resolve("Physical").path(),
        Some("Physical.md"),
        "exact canonical path still wins"
    );
}

#[test]
fn aliases_are_text_properties_not_relationships_or_rewrite_targets() {
    let source = "---\naliases: ['[[Q4 2024]]', 'https://example.com', '2026-10-23']\nrelated: '[[Q4 2024]]'\n---\n[[Q4 2024]]\n";
    let props = properties::parse(properties::frontmatter_block(source).unwrap()).unwrap();
    assert_eq!(
        props[0].value,
        properties::PropertyValue::List(vec![
            properties::PropertyValue::Text("Q4 2024".into()),
            properties::PropertyValue::Text("https://example.com".into()),
            properties::PropertyValue::Text("2026-10-23".into()),
        ])
    );
    assert_eq!(
        properties::links(&props),
        [("related".into(), "Q4 2024".into())]
    );
    let targets = crate::link_candidates::syntax::targets(source);
    assert_eq!(
        targets.len(),
        2,
        "relationship and body link are controls; alias definition is not a destination"
    );
    assert!(targets
        .iter()
        .all(|t| t.range.start > source.find("related:").unwrap()));
    for (yaml, expected) in [
        ("aliases: Plain alias", vec!["Plain alias"]),
        (
            "ALIASES: ['[[Legacy]]', Legacy, 'שם אחר', 'two, names']",
            vec!["Legacy", "שם אחר", "two, names"],
        ),
        ("aliases: [[Legacy]]", vec!["Legacy"]),
        (
            "aliases: [false, 12, null, '', '[[Broken', 'Alias#heading']",
            vec![],
        ),
        ("aliases: {not: an alias}", vec![]),
        ("aliases: [bad", vec![]),
    ] {
        assert_eq!(
            properties::aliases(&format!("---\n{yaml}\n---\n")),
            expected,
            "{yaml}"
        );
    }
}

#[test]
fn aliases_match_quick_open_once_per_path_with_unicode_and_ambiguity() {
    let root = fixture();
    let vault = Vault::scan(root.path()).unwrap();
    let titles = HashMap::new();
    let search = |q| quick_open::search_named(&vault.notes, &titles, vault.aliases(), q, &[], 100);
    assert!(
        quick_open::search(&vault.notes, "Winter", &[], 100).is_empty(),
        "names alone cannot match the alias control"
    );
    assert_eq!(
        search("winter")
            .iter()
            .map(|n| n.path.as_str())
            .collect::<Vec<_>>(),
        ["notes/24q4.md", "notes/25q1.md"]
    );
    assert_eq!(search("q42024")[0].path, "notes/24q4.md");
    assert_eq!(search("שלום")[0].path, "notes/24q4.md");
    assert_eq!(search("Spring")[0].path, "notes/25q1.md");
}

#[test]
fn aliases_survive_warm_start_and_reconcile_changed_declarations() {
    use warm::*;
    let root = fixture();
    let cache = tempfile::tempdir().unwrap();
    let (vault, snapshot, _) = reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
    save_complete(&snapshot, &vault, cache.path()).unwrap();
    let startup = StartupSnapshot::load(cache.path(), root.path()).unwrap();
    assert_eq!(
        startup.vault().resolve("Q4 2024").path(),
        Some("notes/24q4.md")
    );
    let loaded = Snapshot::load_checked(cache.path(), root.path()).unwrap();
    let (vault, _, stats) =
        reconcile(root.path(), Some(&loaded), false, &mut |_, _| Ok(())).unwrap();
    assert_eq!(stats.read, 0, "warm alias identity needs no source rereads");
    assert_eq!(vault.resolve("Spring").path(), Some("notes/25q1.md"));
    std::fs::write(
        root.path().join("notes/25q1.md"),
        "---\naliases: ['Q4 2024']\n---\n# Replacement\n",
    )
    .unwrap();
    let (vault, _, _) = reconcile(root.path(), Some(&loaded), false, &mut |_, _| Ok(())).unwrap();
    assert!(matches!(
        vault.resolve("Q4 2024"),
        Resolution::Ambiguous { .. }
    ));
    assert_eq!(vault.resolve("Spring"), Resolution::Unresolved);
    let fresh = Vault::scan(root.path()).unwrap();
    assert_eq!(
        serde_json::to_value(vault.backlink_map).unwrap(),
        serde_json::to_value(fresh.backlink_map).unwrap()
    );
}

#[test]
fn aliases_incremental_edit_remove_and_delta_restart_refresh_unchanged_referrers() {
    use warm::*;
    let root = fixture();
    let cache = tempfile::tempdir().unwrap();
    let (vault, snapshot, _) = reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
    save_complete(&snapshot, &vault, cache.path()).unwrap();
    let mut state = incremental::State::new(vault, snapshot);
    for next in [
        Some("---\naliases: ['Q4 2024', Spring]\n---\nChanged\n"),
        Some("No aliases now\n"),
        None,
    ] {
        let path = "notes/24q4.md";
        if let Some(raw) = next {
            std::fs::write(root.path().join(path), raw).unwrap();
        } else {
            std::fs::remove_file(root.path().join(path)).unwrap();
        }
        let changes = if next.is_some() {
            crate::Changes {
                changed: [path.into()].into(),
                ..Default::default()
            }
        } else {
            crate::Changes {
                removed: [path.into()].into(),
                ..Default::default()
            }
        };
        let mut reads = Vec::new();
        let batch = state
            .apply_with_reader(&changes, &mut |_, _| Ok(()), &mut |p| {
                reads.push(p.to_owned());
                std::fs::read(p)
            })
            .unwrap();
        assert_eq!(reads.len(), usize::from(next.is_some()));
        if next.is_some() {
            assert!(
                batch.affected.contains("start.md"),
                "alias referrers re-resolve without canonical I/O"
            );
        }
        let fresh = Vault::scan(root.path()).unwrap();
        assert_eq!(state.vault.aliases, fresh.aliases);
        assert_eq!(
            serde_json::to_value(&state.vault.backlink_map).unwrap(),
            serde_json::to_value(&fresh.backlink_map).unwrap()
        );
        state.persist_delta(cache.path()).unwrap();
        for restored in [
            Snapshot::load_checked(cache.path(), root.path())
                .unwrap()
                .vault(),
            StartupSnapshot::load(cache.path(), root.path())
                .unwrap()
                .vault(),
        ] {
            assert_eq!(restored.aliases, fresh.aliases);
            for key in ["Q4 2024", "Spring", "Winter"] {
                assert_eq!(restored.resolve(key), fresh.resolve(key));
            }
        }
    }
}

#[test]
fn aliases_in_yaml_forms_are_not_edges_to_existing_namesakes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Actual.md"), "# Exact filename\n").unwrap();
    std::fs::write(root.path().join("Other.md"), "# Other\n").unwrap();
    for yaml in [
        "{aliases: ['[[Actual]]'], related: '[[Other]]'}",
        "'ALIASES': ['[[Actual]]']\nrelated: '[[ Other#Heading|Shown ]]'",
        r#""\u0061liases": ['[[Actual]]']
related: '[[Other]]'"#,
        "aliases:\n  - '[[Actual]]'\nrelated:\n  - '[[Other]]'",
    ] {
        let source = format!("---\n{yaml}\n---\n# Declaration\n");
        std::fs::write(root.path().join("decl.md"), &source).unwrap();
        let vault = Vault::scan(root.path()).unwrap();
        assert!(vault.backlinks("Actual.md").is_empty(), "{yaml}");
        assert_eq!(
            vault.backlinks("Other.md").len(),
            1,
            "ordinary relationship positive control: {yaml}"
        );
        assert_eq!(crate::link_candidates::syntax::targets(&source).len(), 1);
    }
}

#[test]
fn aliases_cold_graph_reads_each_source_once_before_resolving_referrers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.md"), "[[Late alias]]\n").unwrap();
    std::fs::write(
        root.path().join("z.md"),
        "---\naliases: [Late alias]\n---\n",
    )
    .unwrap();
    let mut vault = Vault::scan_metadata(root.path()).unwrap();
    let mut reads = HashMap::new();
    vault
        .build_backlinks_from(&mut |_, _| Ok(()), |path| {
            let count = reads.entry(path.to_owned()).or_insert(0);
            *count += 1;
            assert_eq!(*count, 1, "graph must not reread {path}");
            Some(std::fs::read_to_string(root.path().join(path)).unwrap())
        })
        .unwrap();
    assert_eq!(reads.len(), 2);
    assert_eq!(vault.resolve("Late alias").path(), Some("z.md"));
    assert_eq!(vault.backlinks("z.md").len(), 1);
}
