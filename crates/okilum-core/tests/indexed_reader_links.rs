use okilum_core::{
    document_links::{
        self,
        prepared::{LinkPreparation, LinkStatus, TargetSnapshot},
        IndexedResolver,
    },
    open_diagnostics, render, Vault,
};

#[test]
fn fifty_outside_paths_render_without_filesystem_operations_on_any_platform() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("note.md"), "# Start\n").unwrap();
    // A real external file is a positive control: the legacy resolver observes
    // filesystem work here, while rendering must not even check existence.
    let existing = outside.path().join("real.json");
    std::fs::write(&existing, "{}").unwrap();
    let vault = Vault::scan_metadata(root.path()).unwrap();
    let mut source = String::new();
    for i in 0..50 {
        let path = if i == 0 {
            existing.clone()
        } else {
            outside.path().join(format!(
                "foreign-{i}.{}",
                if i % 2 == 0 { "md" } else { "json" }
            ))
        };
        source += &format!(
            "[file {i}](<{}>)\n\n",
            document_links::encode(&path.to_string_lossy())
        );
    }
    let (document, report) = open_diagnostics::capture(|| {
        render::reader_document_from_source(&vault, "note.md", &source)
    });
    assert_eq!(
        report.phases.get("filesystem").map_or(0, |p| p.calls),
        0,
        "{}",
        serde_json::to_string(&report).unwrap()
    );
    assert_eq!(document.links.len(), 50);
    assert!(report.phases["resolve_links"].calls >= 50);
    assert!(report.phases["parse"].calls > 0);
    assert_eq!(
        document.rendered.matches("okilum://outside-file/").count(),
        50
    );
    let target = document_links::encode(&existing.to_string_lossy());
    let (live, control) =
        open_diagnostics::capture(|| document_links::resolve(&target, false, &vault, "note.md"));
    assert_eq!(live.status, "outside_file");
    assert!(
        control.phases["filesystem"].calls > 0,
        "counter positive control"
    );
    // Lazy/background verification still distinguishes present from absent.
    let mut prep = LinkPreparation::new(&vault, "note.md", |_| -> Result<TargetSnapshot, String> {
        panic!("outside files are not note reads")
    })
    .with_local_files();
    assert_eq!(prep.link(&target, false).1.status, LinkStatus::Resolved);
    let missing = document_links::encode(&outside.path().join("foreign-1.json").to_string_lossy());
    assert_eq!(prep.link(&missing, false).1.status, LinkStatus::MissingFile);
}

#[test]
fn index_only_links_preserve_local_root_suffix_ambiguity_and_occupancy() {
    let root = tempfile::tempdir().unwrap();
    for (path, text) in [
        ("notes/start.md", "# Start"),
        ("notes/target.md", "# Local"),
        ("target.md", "# Root"),
        ("dup/a.md", "# A"),
        ("else/a.md", "# Other"),
        ("notes/local.png", "image"),
        ("assets/shared.json", "{}"),
        ("other/shared.json", "{}"),
        ("blocked.md", "# Root fallback"),
        ("notes/blocked.md/child.md", "# Occupied directory"),
    ] {
        let p = root.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let vault = Vault::scan_metadata(root.path()).unwrap();
    let resolver = IndexedResolver::new(&vault);
    let (_, report) = open_diagnostics::capture(|| {
        assert_eq!(
            resolver
                .resolve("target.md", false, "notes/start.md")
                .candidates,
            ["notes/target.md"]
        );
        assert_eq!(
            resolver
                .resolve("/target.md", false, "notes/start.md")
                .candidates,
            ["target.md"]
        );
        assert_eq!(
            resolver
                .resolve("../target.md", false, "notes/start.md")
                .candidates,
            ["target.md"]
        );
        assert_eq!(
            resolver.resolve("a.md", false, "notes/start.md").status,
            "ambiguous"
        );
        assert_eq!(
            resolver
                .resolve("local.png", false, "notes/start.md")
                .candidates,
            ["notes/local.png"]
        );
        assert_eq!(
            resolver
                .resolve("shared.json", false, "notes/start.md")
                .status,
            "ambiguous"
        );
        assert_eq!(
            resolver
                .resolve("blocked.md", false, "notes/start.md")
                .status,
            "unresolved"
        );
        let absolute =
            document_links::encode(&root.path().join("notes/target.md").to_string_lossy());
        assert_eq!(
            resolver
                .resolve(&absolute, false, "notes/start.md")
                .candidates,
            ["notes/target.md"]
        );
        assert_eq!(
            resolver
                .resolve("https://example.invalid/a.json", false, "notes/start.md")
                .status,
            "external"
        );
    });
    assert_eq!(report.phases.get("filesystem").map_or(0, |p| p.calls), 0);
}

#[cfg(unix)]
#[test]
fn indexed_links_do_not_redirect_a_dangling_local_alias_to_a_root_namesake() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("notes")).unwrap();
    std::fs::write(root.path().join("notes/start.md"), "# Start").unwrap();
    std::fs::write(root.path().join("target.md"), "# Wrong winner").unwrap();
    std::fs::write(root.path().join("asset.json"), "{}").unwrap();
    std::os::unix::fs::symlink("absent", root.path().join("notes/target.md")).unwrap();
    std::os::unix::fs::symlink("absent", root.path().join("notes/asset.json")).unwrap();
    let vault = Vault::scan_metadata(root.path()).unwrap();
    let resolver = IndexedResolver::new(&vault);
    let (_, report) = open_diagnostics::capture(|| {
        for target in ["target.md", "asset.json"] {
            assert_eq!(
                resolver.resolve(target, false, "notes/start.md").status,
                "unresolved"
            );
        }
    });
    assert_eq!(report.phases.get("filesystem").map_or(0, |p| p.calls), 0);
}

#[test]
fn a_relative_scope_root_does_not_reinterpret_a_relative_link_as_an_os_path() {
    let mut vault = Vault::from_note_paths(["notes/target.md".to_owned()]);
    vault.root = "notes".into();
    let resolver = IndexedResolver::new(&vault);
    let (link, report) =
        open_diagnostics::capture(|| resolver.resolve("notes/target.md", false, "start.md"));
    assert_eq!(link.candidates, ["notes/target.md"]);
    assert_eq!(report.phases.get("filesystem").map_or(0, |p| p.calls), 0);
}

#[test]
fn unverified_outside_paths_do_not_activate_invalid_markdown_space_destinations() {
    let vault = Vault::from_note_paths([]);
    for source in [
        "[link](/my uri)\n",
        "[link](/url \"title \"and\" title\")\n",
    ] {
        let (rendered, report) =
            open_diagnostics::capture(|| render::rewrite_source_links(source, &vault, "note.md"));
        assert_eq!(rendered, source);
        assert_eq!(report.phases.get("filesystem").map_or(0, |p| p.calls), 0);
    }
}

#[test]
fn provisional_cached_image_identity_stays_visible_without_filesystem_calls() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("assets")).unwrap();
    std::fs::write(root.path().join("note.md"), "# Start").unwrap();
    std::fs::write(root.path().join("assets/picture.png"), "image").unwrap();
    let mut vault = Vault::scan_metadata(root.path()).unwrap();
    let expected = vault.resolve_asset("picture.png", "note.md").unwrap();
    vault.inventory_complete = false;
    let (document, report) = open_diagnostics::capture(|| {
        render::reader_document_from_source(&vault, "note.md", "![picture](picture.png)")
    });
    assert!(document
        .rendered
        .contains(url::Url::from_file_path(expected).unwrap().as_str()));
    assert_eq!(report.phases.get("filesystem").map_or(0, |p| p.calls), 0);
}

#[test]
fn single_file_sibling_verification_keeps_the_original_rendered_url_actionable() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("note.md"), "# Start").unwrap();
    std::fs::write(root.path().join("sibling.md"), "# Sibling").unwrap();
    let mut vault = Vault::from_note_paths(["note.md".to_owned()]);
    vault.root = root.path().to_owned();
    vault.single_file = true;
    // Complete within the currently browsed quick scope; sibling is not in
    // that snapshot. Live verification may discover it, rendering may not.
    vault.inventory_complete = true;
    let (document, report) = open_diagnostics::capture(|| {
        render::reader_document_from_source(&vault, "note.md", "[sibling](sibling.md)")
    });
    assert_eq!(report.phases.get("filesystem").map_or(0, |p| p.calls), 0);
    assert_eq!(document.links.len(), 1);
    assert!(document.rendered.contains("okilum://unresolved/sibling.md"));
    let mut preparation = LinkPreparation::new(&vault, "note.md", |path| {
        assert_eq!(path, "sibling.md");
        Ok(TargetSnapshot {
            revision: "same-sibling".into(),
            headings: document_links::HeadingInventory::new("# Sibling"),
            supports_setext: true,
            managed: None,
        })
    })
    .with_local_files();
    let states = preparation.identities(&document.links);
    let state = &states[&document.links[0].url];
    assert_eq!(state.status, LinkStatus::Resolved);
    assert_eq!(
        state.action_url.as_deref(),
        Some("okilum://open/sibling.md")
    );
}

#[test]
fn nested_boundary_stops_source_reads_links_projection_and_incremental_updates(
) -> anyhow::Result<()> {
    use okilum_core::records::{FieldSchema, Projection};
    use okilum_core::vault::{warm, Resolution};
    use okilum_core::vault_descriptor::{self, NestedBoundary};
    use okilum_core::view::{self, RunContext};
    use std::{fs, path::Path, sync::Arc};

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    for folder in [
        "Declared",
        "Marked/.obsidian",
        "Archive/.okilum",
        "Unit",
        "Service/.okilum",
        ".okilum",
        "@Schema",
    ] {
        fs::create_dir_all(root.join(folder))?;
    }
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lifecycle-vault");
    let mut descriptor = vault_descriptor::read(&fixture).unwrap();
    let hash = "0".repeat(64);
    descriptor["layout"]["nested_roots"] = serde_json::json!([
        {"path":"Declared","kind":"vault","name":"Declared name","tree_sha256":hash,"files":1},
        {"path":"Unit","kind":"unit","tree_sha256":hash,"files":1},
        {"path":"Declared/Inner","kind":"vault","name":"Inner name","tree_sha256":hash,"files":1}
    ]);
    fs::write(
        vault_descriptor::path(&root),
        serde_json::to_vec(&descriptor)?,
    )?;
    fs::write(
        vault_descriptor::path(&root.join("Archive")),
        r#"{"name":"Archived name"}"#,
    )?;
    for schema in ["fields.yaml", "frontmatter-schema.yaml", "tags.yaml"] {
        let path = Path::new("@Schema").join(schema);
        fs::copy(fixture.join(&path), root.join(&path))?;
    }
    for folder in ["Declared", "Marked", "Archive", "Unit", "Service"] {
        fs::write(root.join(folder).join("Note.md"), format!("# {folder}"))?;
    }
    fs::write(
        root.join("Control.md"),
        "[[Declared/Note]]\n[Note](Unit/Note.md)",
    )?;
    fs::write(root.join("Unit/Attachment.txt"), "Opaque bytes")?;
    fs::write(root.join("Unit/Events.jsonl"), "{\"event\":\"nested\"}\n")?;
    fs::write(root.join("Events.jsonl"), "{\"event\":\"outer\"}\n")?;
    let (vault, snapshot, _) =
        warm::reconcile_with_reader(&root, None, false, &mut |_, _| Ok(()), &mut |path| {
            assert!(
                path == root.join("Control.md") || path == root.join("Service/Note.md"),
                "unexpected read of {}",
                path.display()
            );
            fs::read(path)
        })?;
    assert_eq!(vault.resolve("Declared/Note"), Resolution::Unresolved);
    assert_eq!(vault.resolve("Note").path(), Some("Service/Note.md"));
    assert_eq!(
        vault.resolve_markdown("Unit/Note.md", "Control.md"),
        Resolution::Unresolved
    );
    assert_eq!(
        document_links::resolve("Unit/Attachment.txt", false, &vault, "Control.md").status,
        "unresolved"
    );
    assert_eq!(vault.read_note("Marked/Note.md")?, "# Marked");
    assert_eq!(
        vault.nested_boundary("Declared/Inner/Note.md"),
        Some((
            "Declared/Inner",
            &NestedBoundary::Vault("Inner name".into())
        ))
    );
    assert_eq!(
        vault.nested_boundaries().get("Archive"),
        Some(&NestedBoundary::Vault("Archived name".into()))
    );
    assert_eq!(
        vault.nested_boundaries().get("Unit"),
        Some(&NestedBoundary::Unit)
    );
    let schema = FieldSchema::load_with_folders(&root, vault.folders())?;
    let projection = Projection::from_snapshot(Arc::new(schema), &snapshot);
    assert_eq!(
        projection
            .notes()
            .map(|note| note.path.as_str())
            .collect::<Vec<_>>(),
        ["Control.md", "Service/Note.md"]
    );
    let view = view::parse(
        "@views/Test.md",
        "---\ntype: view\n---\n```view\nfrom: Unit/Events.jsonl\nshow: log\n```\n```view\nfrom: Events.jsonl\nshow: log\n```\n",
    )
    .expect("valid nested and outer JSONL queries");
    let now = chrono::DateTime::parse_from_rfc3339("2026-11-24T12:00:00Z")?;
    let result = view::run(&view, &RunContext::new(&projection, &vault, now));
    assert_eq!(result.sections[0].total, 0);
    assert_eq!(result.sections[1].total, 1);
    assert_eq!(
        snapshot.vault().nested_boundaries(),
        vault.nested_boundaries()
    );

    let mut worker = warm::incremental::State::new(vault, snapshot);
    let mut changes = okilum_core::Changes::default();
    changes.changed.insert("Unit/New.md".to_owned());
    changes.directories.insert("Unit".to_owned());
    fs::write(root.join("Unit/New.md"), "not an outer note")?;
    let batch = worker.apply_with_reader(&changes, &mut |_, _| Ok(()), &mut |_| {
        panic!("nested source was read")
    })?;
    assert!(batch.changed.is_empty());
    assert!(worker.vault.resolve("Unit/New").path().is_none());
    Ok(())
}
