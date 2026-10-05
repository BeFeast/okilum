use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;
use std::{fs, path::Path};
use tessera_brain::{preview::source_preview, Runner, RunnerConfig};
use tessera_core::{render, source::WriteBoundary, Vault};

fn fixture(root: &Path, runtime: &Path) -> Runner {
    fs::create_dir_all(root.join("records")).unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::create_dir_all(root.join("attachments")).unwrap();
    fs::create_dir_all(runtime).unwrap();
    Runner::open(RunnerConfig {
        brain_id: "01000000-0000-4000-8000-000000000041".into(),
        root: root.into(),
        operational_dir: runtime.into(),
        records_dir: "records".into(),
        boundary: WriteBoundary::Managed,
    })
    .unwrap()
}
fn candidate<'a>(preview: &'a Value, target: &str) -> &'a Value {
    preview["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["target"] == target)
        .unwrap()
}

#[test]
fn draft_preview_matches_reader_links_code_boundaries_callouts_and_transports_images() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    let runner = fixture(&root, &runtime);
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::write(
        root.join("notes/current.md"),
        "---\ncustom: retained\n---\n# Original\n",
    )
    .unwrap();
    fs::write(root.join("notes/target.md"), "# Target\n").unwrap();
    fs::write(root.join("a/duplicate.md"), "# A\n").unwrap();
    fs::write(root.join("b/duplicate.md"), "# B\n").unwrap();
    let image=STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
    fs::write(root.join("attachments/pixel.png"), &image).unwrap();
    let original = runner.read_source("notes/current.md").unwrap();
    let draft="---\ncustom: draft-only\n---\n# Unsaved\n\n[[target|Selected note]] [[duplicate]] [[missing]]\n\n> [!note] Reference\n> [[target|Nested alias]]\n> ![[pixel.png]]\n\n![Picture](../attachments/pixel.png)\n\n`[[target]] ![[pixel.png]]`\n\n```md\n[[target]] ![[pixel.png]]\n```\n";
    let preview =
        source_preview(&runner, "notes/current.md", Some(&STANDARD.encode(draft))).unwrap();
    assert_eq!(preview["revision"], original.revision);
    assert_ne!(preview["preview_revision"], original.revision);
    assert_eq!(
        runner
            .read_source("notes/current.md")
            .unwrap()
            .content_base64,
        original.content_base64
    );
    let md = preview["markdown"].as_str().unwrap();
    assert!(md.contains("# Unsaved"));
    assert!(!md.contains("custom: draft-only"));
    assert!(md.contains("> [!note] Reference"));
    assert!(md.contains("> [Nested alias](tessera://open/notes/target.md)"));
    assert!(md.contains("[Selected note](tessera://open/notes/target.md)"));
    assert!(md.contains("[duplicate](tessera://ambiguous/duplicate)"));
    assert!(md.contains("[missing](tessera://unresolved/missing)"));
    assert!(md.contains("`[[target]] ![[pixel.png]]`"));
    assert!(md.contains("```md\n[[target]] ![[pixel.png]]\n```"));
    assert!(!md.contains("file://"));
    assert_eq!(
        candidate(&preview, "duplicate")["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(candidate(&preview, "missing")["status"], "unresolved");
    let assets = preview["assets"].as_array().unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0]["media_type"], "image/png");
    assert_eq!(
        STANDARD
            .decode(assets[0]["content_base64"].as_str().unwrap())
            .unwrap(),
        image
    );
    assert!(md.contains(assets[0]["url"].as_str().unwrap()));
    // A source-only preview with no images uses exactly the existing reader
    // preprocessing and resolver rather than a weaker fallback renderer.
    let plain = "[[target|Alias]] ==highlight== `==literal==`\n\n> [!warning] Same callout\n> ==Nested highlight==\n\n```md\n==code literal==\n```\n";
    let prepared =
        source_preview(&runner, "notes/current.md", Some(&STANDARD.encode(plain))).unwrap();
    let vault = Vault::scan(&root).unwrap();
    assert_eq!(
        prepared["markdown"],
        render::rewrite_highlights(&render::rewrite_source_links(
            &render::preprocess(plain),
            &vault,
            "notes/current.md"
        ))
    );
}

#[test]
fn preview_never_reads_escaped_or_symlink_sources_or_assets() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    let runner = fixture(&root, &runtime);
    let secret = b"outside-secret-image";
    fs::write(temp.path().join("outside.png"), secret).unwrap();
    fs::write(temp.path().join("outside.md"), "outside-secret-note").unwrap();
    symlink(
        temp.path().join("outside.png"),
        root.join("attachments/symlink.png"),
    )
    .unwrap();
    symlink(temp.path(), root.join("linked-directory")).unwrap();
    symlink(
        temp.path().join("outside.md"),
        root.join("notes/symlink.md"),
    )
    .unwrap();
    let draft=format!("# Safe\n![escape](../../outside.png)\n![symlink](../attachments/symlink.png)\n![directory](../linked-directory/outside.png)\n![absolute]({})\n![file](file://{})\n",temp.path().join("outside.png").display(),temp.path().join("outside.png").display());
    fs::write(root.join("notes/current.md"), &draft).unwrap();
    let preview = source_preview(&runner, "notes/current.md", None).unwrap();
    assert_eq!(preview["assets"], serde_json::json!([]));
    assert!(!preview.to_string().contains(&STANDARD.encode(secret)));
    assert!(!preview["markdown"].as_str().unwrap().contains("file://"));
    assert_eq!(
        preview["markdown"]
            .as_str()
            .unwrap()
            .matches("tessera-asset://unavailable")
            .count(),
        5
    );
    for path in [
        "../outside.md",
        "notes/symlink.md",
        "linked-directory/outside.md",
    ] {
        assert!(source_preview(&runner, path, None).is_err(), "{path}");
    }
    assert!(source_preview(&runner, "notes/current.md", Some("invalid-base64!")).is_err());
    assert!(source_preview(
        &runner,
        "notes/current.md",
        Some(&STANDARD.encode([0xff, 0xfe]))
    )
    .is_err());
}

#[test]
fn preview_enforces_source_and_asset_budgets_without_truncating_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    let runner = fixture(&root, &runtime);
    let source = "# Image budget\n![large](../attachments/large.png)\n";
    fs::write(root.join("notes/current.md"), source).unwrap();
    fs::File::create(root.join("attachments/large.png"))
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();
    let preview = source_preview(&runner, "notes/current.md", None).unwrap();
    assert_eq!(preview["assets"], serde_json::json!([]));
    assert!(preview["markdown"]
        .as_str()
        .unwrap()
        .contains("tessera-asset://unavailable"));
    assert_eq!(
        fs::read_to_string(root.join("notes/current.md")).unwrap(),
        source
    );
    fs::File::create(root.join("notes/large.md"))
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();
    assert!(source_preview(&runner, "notes/large.md", None)
        .unwrap_err()
        .to_string()
        .contains("byte budget"));
    let draft = STANDARD.encode(vec![b'a'; 8 * 1024 * 1024 + 1]);
    assert!(source_preview(&runner, "notes/current.md", Some(&draft))
        .unwrap_err()
        .to_string()
        .contains("byte budget"));
}

#[test]
fn open_link_classifier_targets_join_actual_preview_resolver_rows() {
    use tessera_core::{source_classifier, source_projection::Snapshot};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runner = fixture(&root, &temp.path().join("runtime"));
    for dir in ["left", "right"] {
        fs::create_dir(root.join(dir)).unwrap();
    }
    for path in [
        "notes/target.md",
        "notes/space name.md",
        "left/shared.md",
        "right/shared.md",
    ] {
        fs::write(root.join(path), "# Destination\n").unwrap();
    }
    let raw = "\u{feff}# Source\r\n\r\n[[notes/target.md|😀 alias]] [[ shared |Shared]] [relative](../notes/target.md) [space](space%20name.md)\r\n";
    fs::write(root.join("notes/current.md"), raw).unwrap();
    let source = runner.read_source("notes/current.md").unwrap();
    let preview = source_preview(&runner, "notes/current.md", None).unwrap();
    assert_eq!(preview["path"], "notes/current.md");
    assert_eq!(preview["revision"], source.revision);
    assert_eq!(preview["preview_revision"], source.revision);
    let snap = Snapshot::new("notes/current.md", 1, raw);
    let classified = source_classifier::classify(&snap);
    let links = classified.links_for(&snap).unwrap();
    assert_eq!(links.len(), 4);
    for link in links {
        let target = if link.wiki {
            link.target.trim().to_owned()
        } else {
            link.target.trim_end_matches(".md").replace("%20", " ")
        };
        let row = candidate(&preview, &target);
        assert!(row["url"].is_string());
        if target == "shared" {
            assert_eq!(row["status"], "ambiguous");
            assert_eq!(row["candidates"].as_array().unwrap().len(), 2);
        } else {
            assert_eq!(row["status"], "resolved", "{target}");
            let expected = if target.contains("space") {
                "notes/space name.md"
            } else {
                "notes/target.md"
            };
            assert_eq!(row["candidates"][0]["path"], expected);
        }
    }
    assert_eq!(
        fs::read_to_string(root.join("notes/current.md")).unwrap(),
        raw
    );
}

#[test]
fn shared_document_links_keep_exact_metadata_urls_fragments_and_source_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runner = fixture(&root, &temp.path().join("runtime"));
    let raw = "\u{feff}# Origin\r\n\r\n[Sibling](sibling.md) [Heading][r] [[sibling#Landing|Wiki]] [Self](#Origin)\r\n\r\n[r]: <sibling.md#Landing> \"title\"\r\n";
    fs::write(root.join("notes/current.md"), raw).unwrap();
    fs::write(root.join("notes/sibling.md"), "intro\n\n## Landing\n").unwrap();
    fs::write(root.join("sibling.md"), "# Root sentinel\n\n## Landing\n").unwrap();
    let before = runner.read_source("notes/current.md").unwrap();
    let preview = source_preview(&runner, "notes/current.md", None).unwrap();
    assert_eq!(preview["document_links_version"], 1);
    for (target, path, heading, url) in [
        (
            "sibling.md",
            "notes/sibling.md",
            None,
            "tessera://open/notes/sibling.md",
        ),
        (
            "sibling.md#Landing",
            "notes/sibling.md",
            Some("Landing"),
            "tessera://open/notes/sibling.md#Landing",
        ),
        (
            "sibling#Landing",
            "sibling.md",
            Some("Landing"),
            "tessera://open/sibling.md#Landing",
        ),
        (
            "#Origin",
            "notes/current.md",
            Some("Origin"),
            "tessera://open/notes/current.md#Origin",
        ),
    ] {
        let row = preview["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["authored_target"] == target)
            .unwrap();
        assert_eq!(row["candidates"][0]["path"], path);
        assert_eq!(row["heading"].as_str(), heading);
        assert_eq!(row["url"], url);
        assert!(preview["markdown"].as_str().unwrap().contains(url));
    }
    assert_eq!(
        runner
            .read_source("notes/current.md")
            .unwrap()
            .content_base64,
        before.content_base64
    );
    assert_eq!(
        fs::read(root.join("notes/current.md")).unwrap(),
        raw.as_bytes()
    );
}

#[test]
fn wiki_and_markdown_ambiguity_have_distinct_transport_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runner = fixture(&root, &temp.path().join("runtime"));
    fs::create_dir(root.join("other")).unwrap();
    for path in ["notes/same.md", "notes/Same.md", "other/same.md"] {
        fs::write(root.join(path), "# Candidate").unwrap();
    }
    fs::write(root.join("notes/current.md"), "[[same.md]] [md](same.md)").unwrap();
    let preview = source_preview(&runner, "notes/current.md", None).unwrap();
    let rows = preview["links"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let wiki = rows.iter().find(|r| r["wiki"] == true).unwrap();
    let md = rows.iter().find(|r| r["wiki"] == false).unwrap();
    assert_ne!(wiki["url"], md["url"]);
    assert_eq!(wiki["candidates"].as_array().unwrap().len(), 3);
    assert_eq!(md["candidates"].as_array().unwrap().len(), 2);
    assert!(md["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["path"].as_str().unwrap().starts_with("notes/")));
    for row in rows {
        assert!(preview["markdown"]
            .as_str()
            .unwrap()
            .contains(row["url"].as_str().unwrap()));
    }
}

#[test]
fn additive_contract_preserves_old_joins_and_old_handlers_refuse_heading_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runner = fixture(&root, &temp.path().join("runtime"));
    for dir in ["a", "b"] {
        fs::create_dir(root.join(dir)).unwrap();
    }
    for path in ["notes/space name.md", "a/dup.md", "b/dup.md"] {
        fs::write(root.join(path), "# Landing").unwrap();
    }
    let raw = "[[notes/space name.md]] [md](space%20name.md) [[notes/space name.md#Landing]] [mdh](space%20name.md#Landing) [[dup#Landing]] [amb](dup.md#Landing)";
    fs::write(root.join("notes/current.md"), raw).unwrap();
    let preview = source_preview(&runner, "notes/current.md", None).unwrap();
    let rows = preview["links"].as_array().unwrap();
    for (authored, legacy) in [
        ("notes/space name.md", "notes/space name.md"),
        ("space%20name.md", "space name"),
        ("notes/space name.md#Landing", "notes/space name.md"),
        ("space%20name.md#Landing", "space name"),
        ("dup#Landing", "dup"),
        ("dup.md#Landing", "dup"),
    ] {
        let row = rows
            .iter()
            .find(|r| r["authored_target"] == authored)
            .unwrap();
        assert_eq!(row["target"], legacy);
        // Exact baseline preview_link branch conditions: only resolved and
        // ambiguous navigate/offer a chooser; all other statuses show an error.
        let old_rendered_can_navigate =
            matches!(row["status"].as_str(), Some("resolved" | "ambiguous"));
        assert_eq!(
            old_rendered_can_navigate,
            row["heading"].is_null(),
            "{authored}"
        );
    }
    // Baseline explicit resolved_row refuses duplicate legacy-target matches.
    // Do not collapse wiki/Markdown or heading intents to rescue an old client.
    assert_eq!(rows.iter().filter(|r| r["target"] == "dup").count(), 2);
    // Baseline explicit resolved_row + open_link_at_caret decisions, copied
    // as a compatibility negative control rather than the new implementation.
    let old_explicit_can_navigate = |reply: &Value, target: &str| {
        let mut matches = reply["links"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["target"] == target);
        let Some(row) = matches.next() else {
            return false;
        };
        if matches.next().is_some() {
            return false;
        }
        let count = row["candidates"].as_array().unwrap().len();
        (row["status"] == "resolved" && count == 1) || (row["status"] == "ambiguous" && count > 0)
    };
    assert!(!old_explicit_can_navigate(&preview, "dup"));
    for authored in ["notes/space name.md#Landing", "dup#Landing"] {
        let probe = format!("[[{authored}]]\n");
        let reply =
            source_preview(&runner, "notes/current.md", Some(&STANDARD.encode(probe))).unwrap();
        assert_eq!(reply["links"].as_array().unwrap().len(), 1);
        assert!(!old_explicit_can_navigate(
            &reply,
            authored.split('#').next().unwrap()
        ));
    }
    // The existing insertion probe is a single qualified wikilink and remains
    // usable by old note_link::verify without understanding additive fields.
    let probe = "[[notes/space name.md]]\n";
    let proof = source_preview(&runner, "notes/current.md", Some(&STANDARD.encode(probe))).unwrap();
    let row = &proof["links"][0];
    assert_eq!(proof["links"].as_array().unwrap().len(), 1);
    assert_eq!(row["target"], "notes/space name.md");
    assert_eq!(row["status"], "resolved");
    assert_eq!(row["candidates"][0]["path"], "notes/space name.md");
    assert!(
        old_explicit_can_navigate(&proof, "notes/space name.md"),
        "baseline positive control"
    );
}

#[test]
fn literal_wiki_and_legacy_entity_keys_do_not_follow_comrak_unescaping() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runner = fixture(&root, &temp.path().join("runtime"));
    for path in ["a&amp;b.md", "a&b.md"] {
        fs::write(root.join(path), "# Sentinel").unwrap();
    }
    fs::write(
        root.join("notes/current.md"),
        "[[a&amp;b.md]] [md](a&amp;b.md)",
    )
    .unwrap();
    let preview = source_preview(&runner, "notes/current.md", None).unwrap();
    let rows = preview["links"].as_array().unwrap();
    let wiki = rows.iter().find(|r| r["wiki"] == true).unwrap();
    let md = rows.iter().find(|r| r["wiki"] == false).unwrap();
    assert_eq!(wiki["target"], "a&amp;b.md");
    assert_eq!(wiki["authored_target"], "a&amp;b.md");
    assert_eq!(wiki["candidates"][0]["path"], "a&amp;b.md");
    assert_eq!(md["target"], "a&amp;b", "old-client join key is unchanged");
    assert_eq!(
        md["authored_target"], "a&b.md",
        "CommonMark destination semantics"
    );
    assert_eq!(md["candidates"][0]["path"], "a&b.md");
    let html = render::render_html(
        &Vault::scan(&root).unwrap(),
        "notes/current.md",
        "base16-ocean.dark",
    )
    .unwrap();
    assert!(html.contains("tessera://open/a%26amp%3Bb.md"), "{html}");
}

#[test]
fn prepared_metadata_is_scoped_to_draft_and_target_revisions_and_recovers_on_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runner = fixture(&root, &temp.path().join("runtime"));
    let original = b"# Saved\r\n";
    fs::write(root.join("notes/current.md"), original).unwrap();
    let draft = "# Draft heading\n\n> [!note]\n> [[target#Landing|Keep label]]\n\n[self](#Draft%20heading) [absent](missing.md) [remote](https://example.invalid/x.md)";
    let encoded = STANDARD.encode(draft);
    for (content, status) in [
        (None, "missing_document"),
        (Some("# Other"), "missing_heading"),
        (Some("# Landing"), "resolved"),
        (Some("Landing\n==="), "unsupported"),
        (None, "missing_document"),
    ] {
        if let Some(content) = content {
            fs::write(root.join("notes/target.md"), content).unwrap();
        } else {
            let _ = fs::remove_file(root.join("notes/target.md"));
        }
        let preview = source_preview(&runner, "notes/current.md", Some(&encoded)).unwrap();
        assert_eq!(preview["prepared_links_version"], 1);
        let rows = preview["links"].as_array().unwrap();
        let row = rows
            .iter()
            .find(|r| r["authored_target"] == "target#Landing")
            .unwrap();
        assert_eq!(row["prepared"]["status"], status);
        assert!(preview["markdown"]
            .as_str()
            .unwrap()
            .contains("[Keep label]("));
        let own = rows
            .iter()
            .find(|r| r["authored_target"] == "#Draft%20heading")
            .unwrap();
        assert_eq!(
            own["prepared"]["status"], "resolved",
            "unsaved self-heading uses draft bytes"
        );
        assert_eq!(
            own["prepared"]["target_revision"],
            preview["preview_revision"]
        );
        let absent = rows
            .iter()
            .find(|r| r["authored_target"] == "missing.md")
            .unwrap();
        assert_eq!(absent["prepared"]["status"], "missing_document");
        assert_eq!(fs::read(root.join("notes/current.md")).unwrap(), original);
    }
}

#[test]
fn prepared_heading_respects_actual_managed_navigation_limits() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runner = fixture(&root, &temp.path().join("runtime"));
    fs::write(root.join("notes/current.md"), "[target](target.md#Landing)").unwrap();
    for raw in [
        format!(
            "# Landing\n{}",
            "x".repeat(tessera_core::source_classifier::MAX_BYTES)
        ),
        "# <span>Landing</span>".into(),
    ] {
        fs::write(root.join("notes/target.md"), raw).unwrap();
        let preview = source_preview(&runner, "notes/current.md", None).unwrap();
        assert_eq!(preview["links"][0]["prepared"]["status"], "unsupported");
    }
    fs::write(
        root.join("notes/target.md"),
        "# [Landing](https://example.invalid)",
    )
    .unwrap();
    let preview = source_preview(&runner, "notes/current.md", None).unwrap();
    assert_eq!(
        preview["links"][0]["prepared"]["status"], "resolved",
        "supported link formatting remains supported"
    );
}
