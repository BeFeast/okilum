use okilum_core::{
    document_links::{
        self,
        prepared::{LinkPreparation, LinkStatus, TargetSnapshot},
        HeadingFailure, HeadingInventory,
    },
    Vault,
};
use std::{cell::Cell, collections::BTreeMap};

fn target(source: &str) -> TargetSnapshot {
    TargetSnapshot {
        revision: format!("test:{source}"),
        headings: HeadingInventory::new(source),
        supports_setext: true,
        managed: None,
    }
}

#[test]
fn typed_preparation_uses_exact_destinations_headings_and_one_read_per_target() {
    let dir = tempfile::Builder::new()
        .prefix("okilum-prepared-")
        .tempdir()
        .unwrap();
    for (path, source) in [
        ("start.md", "# Start"),
        ("same.md", "# Root"),
        (
            "notes/same.md",
            "# Sibling\n\nSetext\n===\n\n# Dup\n\n> # Dup\n\n> # Nested",
        ),
        ("a/shared.md", "a"),
        ("b/shared.md", "b"),
        ("notes/Юникод space.md", "# C# 😀"),
    ] {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let vault = Vault::scan_metadata(dir.path()).unwrap();
    assert!(!vault.notes.is_empty(), "inventory positive control");
    let reads = Cell::new(0);
    let mut prep = LinkPreparation::new(&vault, "notes/start.md", |path| {
        reads.set(reads.get() + 1);
        Ok(target(
            &std::fs::read_to_string(dir.path().join(path)).unwrap(),
        ))
    });
    let (md, found) = prep.link("same.md#Sibling", false);
    assert_eq!(md.candidates, ["notes/same.md"]);
    assert_eq!(found.status, LinkStatus::Resolved);
    assert_eq!(
        prep.link("same.md#Setext", false).1.status,
        LinkStatus::Resolved
    );
    assert_eq!(
        prep.link("same.md#No", false).1.status,
        LinkStatus::MissingHeading
    );
    assert_eq!(
        prep.link("same.md#Dup", false).1.status,
        LinkStatus::Ambiguous
    );
    assert_eq!(
        prep.link("same.md#Nested", false).1.status,
        LinkStatus::Unsupported
    );
    assert_eq!(
        reads.get(),
        1,
        "different fragments reuse one exact source revision"
    );
    let (wiki, found) = prep.link("same.md#Root", true);
    assert_eq!(wiki.candidates, ["same.md"]);
    assert_eq!(found.status, LinkStatus::Resolved);
    assert_eq!(
        prep.link("Юникод%20space.md#C%23%20😀", false).1.status,
        LinkStatus::Resolved
    );
    assert_eq!(prep.link("shared", true).1.status, LinkStatus::Ambiguous);
    assert_eq!(
        prep.link("missing.md", false).1.status,
        LinkStatus::MissingDocument
    );
    assert_eq!(
        prep.link("missing", true).1.status,
        LinkStatus::MissingDocument
    );
    let missing_block = prep.link("same.md#^block", false).1;
    assert_eq!(missing_block.status, LinkStatus::MissingHeading);
    assert_eq!(missing_block.reason, "Block not found: ^block");
    assert_eq!(
        prep.link("https://example.invalid/missing.md", false)
            .1
            .status,
        LinkStatus::External
    );
    assert_eq!(
        prep.link("../../escape.md", false).1.status,
        LinkStatus::Unknown
    );
    assert_eq!(
        reads.get(),
        3,
        "missing/ambiguous/external links do not fetch source"
    );
    let source = "`[[example]]`\n\n```md\n[x](code.md)\n```\n\n[[missing|Keep label]] [**Readable**](missing.md)";
    assert_eq!(prep.source(source).len(), 2);
    let rendered = okilum_core::render::rewrite_source_links(source, &vault, "notes/start.md");
    assert!(rendered.contains("[Keep label](okilum://unresolved/missing)"));
    assert!(rendered.contains("[**Readable**](okilum://unresolved/missing.md)"));
}

#[test]
fn lifecycle_reuses_rendered_identity_but_refreshes_state_and_action() {
    let dir = tempfile::Builder::new()
        .prefix("okilum-prepared-")
        .tempdir()
        .unwrap();
    std::fs::write(dir.path().join("start.md"), "[target](target.md#Landing)").unwrap();
    let original = Vault::scan_metadata(dir.path()).unwrap();
    let source = "[target](target.md#Landing)";
    let identities = okilum_core::render::reader_document(&original, "start.md")
        .unwrap()
        .links;
    let key = document_links::resolve("target.md#Landing", false, &original, "start.md").url;
    let statuses = [
        (None, LinkStatus::MissingDocument),
        (Some("# Elsewhere"), LinkStatus::MissingHeading),
        (Some("# Landing"), LinkStatus::Resolved),
        (None, LinkStatus::MissingDocument),
    ];
    for (content, expected) in statuses {
        if let Some(content) = content {
            std::fs::write(dir.path().join("target.md"), content).unwrap();
        } else {
            let _ = std::fs::remove_file(dir.path().join("target.md"));
        }
        let vault = Vault::scan_metadata(dir.path()).unwrap();
        let mut prep = LinkPreparation::new(&vault, "start.md", |p| {
            Ok(target(
                &std::fs::read_to_string(dir.path().join(p)).unwrap(),
            ))
        });
        let states = prep.identities(&identities);
        assert_eq!(states[&key].status, expected);
        if expected == LinkStatus::Resolved {
            assert_eq!(
                states[&key].action_url.as_deref(),
                Some("okilum://open/target.md#Landing")
            );
        }
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("start.md")).unwrap(),
        source
    );
}

#[test]
fn incomplete_unreadable_and_managed_surface_limits_are_not_missing() {
    let dir = tempfile::Builder::new()
        .prefix("okilum-prepared-")
        .tempdir()
        .unwrap();
    std::fs::write(dir.path().join("target.md"), "Title\n===").unwrap();
    let mut vault = Vault::scan_metadata(dir.path()).unwrap();
    vault.inventory_complete = false;
    let mut prep = LinkPreparation::new(&vault, "start.md", |_| Err("Backend offline".into()));
    assert_eq!(prep.link("missing.md", false).1.status, LinkStatus::Unknown);
    assert_eq!(
        prep.link("target.md#Title", false).1.status,
        LinkStatus::Unknown
    );
    let mut prep = LinkPreparation::new(&vault, "start.md", |_| {
        let mut t = target("Title\n===");
        t.supports_setext = false;
        Ok(t)
    });
    assert_eq!(
        prep.link("target.md#Title", false).1.status,
        LinkStatus::Unsupported
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("absent", dir.path().join("dangling.md")).unwrap();
        vault.inventory_complete = true;
        let mut prep = LinkPreparation::new(&vault, "start.md", |_| unreachable!());
        assert_eq!(
            prep.link("dangling.md", false).1.status,
            LinkStatus::Unknown
        );
    }
}

#[test]
fn many_links_large_target_reuse_and_inventory_locators() {
    let dir = tempfile::Builder::new()
        .prefix("okilum-prepared-")
        .tempdir()
        .unwrap();
    std::fs::write(dir.path().join("target.md"), "# Placeholder").unwrap();
    let vault = Vault::scan_metadata(dir.path()).unwrap();
    let large = format!("{}\n# Landing\n", "Large paragraph.\n\n".repeat(20_000));
    let source = "[yes](target.md#Landing) [no](target.md#Missing)\n".repeat(2000);
    assert!(!vault.notes.is_empty(), "inventory positive control");
    let reads = Cell::new(0);
    let mut prep = LinkPreparation::new(&vault, "start.md", |_| {
        reads.set(reads.get() + 1);
        Ok(target(&large))
    });
    let now = std::time::Instant::now();
    let states: BTreeMap<_, _> = prep.source(&source);
    eprintln!(
        "4000 links / {} target bytes: {:?}, reads={}",
        large.len(),
        now.elapsed(),
        reads.get()
    );
    assert_eq!(reads.get(), 1);
    assert_eq!(states.len(), 2);
    assert_eq!(
        states["okilum://open/target.md#Landing"].status,
        LinkStatus::Resolved
    );
    assert_eq!(
        states["okilum://open/target.md#Missing"].status,
        LinkStatus::MissingHeading
    );
    let inventory =
        HeadingInventory::new("## **Unicode 😀**\n\nSetext\n===\n\n> # Nested\n\n# Dup\n\n> # Dup");
    assert_eq!(inventory.entries[0].level, 2);
    assert_eq!(inventory.locate("Unicode 😀").unwrap().block, 0);
    assert!(inventory.locate("Setext").unwrap().setext);
    assert_eq!(inventory.locate("Nested"), Err(HeadingFailure::Unsupported));
    assert_eq!(inventory.locate("Dup"), Err(HeadingFailure::Ambiguous));
}

#[test]
fn refreshed_sibling_candidate_uses_original_rendered_url_and_embeds_keep_own_source() {
    let dir = tempfile::Builder::new()
        .prefix("okilum-prepared-identities-")
        .tempdir()
        .unwrap();
    for (path, body) in [
        (
            "notes/start.md",
            "[chosen](target.md)\n\n![[embedded/child]]\n",
        ),
        ("other/target.md", "# Old"),
        ("embedded/child.md", "[own](./own.md)"),
        ("embedded/own.md", "# Own"),
    ] {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let vault = Vault::scan_metadata(dir.path()).unwrap();
    let document = okilum_core::render::reader_document(&vault, "notes/start.md").unwrap();
    assert!(document.rendered.contains("okilum://open/other/target.md"));
    assert!(document
        .links
        .iter()
        .any(|l| l.from == "embedded/child.md" && l.url == "okilum://open/embedded/own.md"));
    std::fs::write(dir.path().join("notes/target.md"), "# New sibling").unwrap();
    let refreshed = Vault::scan_metadata(dir.path()).unwrap();
    let mut prep = LinkPreparation::new(&refreshed, "notes/start.md", |p| {
        Ok(target(
            &std::fs::read_to_string(dir.path().join(p)).unwrap(),
        ))
    });
    let states = prep.identities(&document.links);
    assert_eq!(
        states["okilum://open/other/target.md"]
            .action_url
            .as_deref(),
        Some("okilum://open/notes/target.md")
    );
    assert_eq!(
        states["okilum://open/embedded/own.md"].status,
        LinkStatus::Resolved
    );
}

#[test]
fn heading_snapshot_preserves_embed_boundaries_without_reading_bodies() {
    let dir = tempfile::Builder::new()
        .prefix("okilum-embed-heading-")
        .tempdir()
        .unwrap();
    for (path, source) in [
        ("child.md", "# Embedded heading must stay fenced"),
        ("a/shared.md", "a"),
        ("b/shared.md", "b"),
        ("target.md", ""),
    ] {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let vault = Vault::scan_metadata(dir.path()).unwrap();
    assert!(matches!(
        vault.resolve("shared"),
        okilum_core::Resolution::Ambiguous { .. }
    ));
    assert!(matches!(
        vault.resolve("child"),
        okilum_core::Resolution::Resolved { .. }
    ));
    for (prefix, landing) in [
        ("![[child]]", true),
        ("![[absent]]", true),
        ("![[target]]", true),
        ("![[shared]]", false),
        ("inline ![[child]]", false),
        ("![[image.png]]", true),
    ] {
        let raw = format!("{prefix}\nLanding\n=======\n");
        std::fs::write(dir.path().join("target.md"), &raw).unwrap();
        let structural = okilum_core::render::reader_heading_source(&vault, "target.md", &raw);
        let rendered = okilum_core::render::reader_document(&vault, "target.md").unwrap();
        assert_eq!(
            document_links::heading(&structural, "Landing").is_ok(),
            landing,
            "{prefix}: {structural}"
        );
        assert_eq!(
            document_links::heading(&rendered.rendered, "Landing").is_ok(),
            landing,
            "{prefix}: {}",
            rendered.rendered
        );
        assert!(document_links::heading(&structural, "Embedded heading must stay fenced").is_err());
    }
    let raw = "![[child]]\nLanding\n=======\n";
    let before = okilum_core::render::reader_heading_source(&vault, "target.md", raw);
    std::fs::write(dir.path().join("child.md"), [0xff, 0xfe]).unwrap();
    assert_eq!(
        before,
        okilum_core::render::reader_heading_source(&vault, "target.md", raw),
        "embedded source bytes do not affect structural preparation"
    );
}

#[test]
fn block_references_land_on_the_marked_block() {
    let vault = Vault::from_note_paths(["note.md".into()]);
    let raw = "# Title\n\nFirst paragraph. ^first\n\n| a |\n|---|\n\n^table\n\n- item ^dup\n- other ^DUP\n\n`code ^not`\n";
    let rendered = okilum_core::render::reader_heading_source(&vault, "note.md", raw);
    let inventory = HeadingInventory::new(&rendered);
    // heading 0, paragraph 1, table 2, table marker 3, list 4, code 5
    assert_eq!(inventory.locate("^first").map(|t| t.block), Ok(1));
    assert_eq!(
        inventory.locate("^table").map(|t| t.block),
        Ok(2),
        "an ID alone on its line names the block before it"
    );
    assert_eq!(
        inventory.locate("^dup"),
        Err(HeadingFailure::AmbiguousBlock)
    );
    assert_eq!(inventory.locate("^not"), Err(HeadingFailure::MissingBlock));
    assert_eq!(
        inventory.locate("Title").map(|t| t.block),
        Ok(0),
        "positive control: headings still resolve"
    );
    assert_eq!(
        document_links::heading(&rendered, "^first").map(|t| t.block),
        Ok(1)
    );
}
