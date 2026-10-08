use tessera_core::{render::rewrite_source_links, Vault};

#[test]
fn markdown_sibling_and_fragment_are_exact() {
    let root = tempfile::Builder::new()
        .prefix("tessera-links314-")
        .tempdir()
        .unwrap();
    std::fs::create_dir(root.path().join("notes")).unwrap();
    for (path, body) in [
        ("sibling.md", "# Root sentinel"),
        ("notes/sibling.md", "intro\n\n## Landing\n"),
        ("notes/start.md", "# Start"),
    ] {
        std::fs::write(root.path().join(path), body).unwrap();
    }
    let vault = Vault::scan(root.path()).unwrap();
    assert_eq!(vault.notes.len(), 3, "fixture inventory positive control");
    assert_eq!(
        rewrite_source_links("[Sibling](sibling.md)", &vault, "notes/start.md"),
        "[Sibling](tessera://open/notes/sibling.md)"
    );
    assert_eq!(
        rewrite_source_links("[Heading](sibling.md#Landing)", &vault, "notes/start.md"),
        "[Heading](tessera://open/notes/sibling.md#Landing)"
    );
    assert_eq!(
        rewrite_source_links("[[sibling]]", &vault, "notes/start.md"),
        "[sibling](tessera://open/sibling.md)"
    );
}

use tessera_core::document_links::{self as links, Destination};

#[test]
fn portable_corpus_has_exact_paths_fragments_and_scoped_outcomes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/document-links/vault");
    let vault = Vault::scan(&root).unwrap();
    let source = std::fs::read_to_string(root.join("notes/start.md")).unwrap();
    let parsed = links::parse(&source);
    for (target, wiki, path, heading) in [
        ("./sibling.md", false, "notes/sibling.md", None),
        ("sibling.md", false, "notes/sibling.md", None),
        ("../other/target.md", false, "other/target.md", None),
        ("./Space%20Note.md", false, "notes/Space Note.md", None),
        (
            "./%D0%97%D0%B0%D0%BC%D0%B5%D1%82%D0%BA%D0%B0.md",
            false,
            "notes/Заметка.md",
            None,
        ),
        (
            "../other/target.md#Details",
            false,
            "other/target.md",
            Some("Details"),
        ),
        (
            "#Local%20heading",
            false,
            "notes/start.md",
            Some("Local heading"),
        ),
        ("/other/target.md", false, "other/target.md", None),
        ("other/target", true, "other/target.md", None),
        (
            "other/target#Details",
            true,
            "other/target.md",
            Some("Details"),
        ),
    ] {
        assert!(
            parsed.iter().any(|l| l.target == target && l.wiki == wiki),
            "parser positive control: {target}"
        );
        let resolved = links::resolve(target, wiki, &vault, "notes/start.md");
        assert_eq!(resolved.status, "resolved", "{target}");
        assert_eq!(resolved.candidates, [path], "{target}");
        assert_eq!(resolved.heading.as_deref(), heading, "{target}");
        assert_eq!(
            tessera_core::render::split_open_url(
                resolved
                    .url
                    .strip_prefix(tessera_core::render::WIKI_SCHEME)
                    .unwrap()
            ),
            (path.into(), heading.map(str::to_owned))
        );
    }
    for (target, path) in [
        ("../attachments/sample.png", "attachments/sample.png"),
        ("./local.png", "notes/local.png"),
        ("attachments/sample.png", "attachments/sample.png"),
    ] {
        assert!(parsed
            .iter()
            .any(|link| link.target == target && !link.wiki));
        let resolved = links::resolve(target, false, &vault, "notes/start.md");
        assert_eq!(resolved.status, "attachment", "{target}");
        assert_eq!(resolved.candidates, [path], "{target}");
        assert_eq!(resolved.url, format!("tessera://attachment/{path}"));
        assert_eq!(resolved.heading, None);
    }
    assert_eq!(
        links::resolve("../other/target", false, &vault, "notes/start.md").status,
        "unsupported"
    );
    // Block references land on a block (#651), in both link grammars.
    for (target, wiki) in [
        ("../other/target.md#^sample-block", false),
        ("other/target#^sample-block", true),
    ] {
        let resolved = links::resolve(target, wiki, &vault, "notes/start.md");
        assert_eq!(resolved.status, "resolved", "{target}");
        assert_eq!(resolved.heading.as_deref(), Some("^sample-block"));
    }
    assert_eq!(
        links::resolve("other/target#^", true, &vault, "notes/start.md").status,
        "unsupported",
        "an empty block target is not a link to the note"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("notes/start.md")).unwrap(),
        source
    );
}

#[test]
fn comrak_ranges_cover_reference_title_angle_unicode_and_exclude_code() {
    let source = "\u{feff}# Origin\r\n\r\n[ref][r] [title](sibling.md \"kept\") [angle](<Space Note.md>) [Ю](Заметка.md)\r\n\r\n[r]: sibling.md#Landing \"reference title\"\r\n\r\n`[code](hidden.md)`\r\n\r\n```md\r\n[fence](hidden.md)\r\n```\r\n\r\n    [indent](hidden.md)\r\n\r\n[broken](bad path.md) [positive](sibling.md)\r\n";
    let parsed = links::parse(source);
    assert_eq!(parsed.len(), 5, "{parsed:?}");
    assert_eq!(
        parsed.iter().map(|l| l.target.as_str()).collect::<Vec<_>>(),
        [
            "sibling.md#Landing",
            "sibling.md",
            "Space Note.md",
            "Заметка.md",
            "sibling.md"
        ]
    );
    assert_eq!(&source[parsed[0].range.clone()], "[ref][r]");
    assert_eq!(parsed[0].title, "reference title");
    assert_eq!(&source[parsed[2].range.clone()], "[angle](<Space Note.md>)");
    assert!(parsed.iter().all(|l| !l.wiki));
}

/// Comrak misplaces some inline source positions (#650). A rewrite into a
/// wrong range used to splice the resolved URL into the middle of the note.
#[test]
fn misreported_comrak_ranges_are_repaired_or_skipped_never_spliced() {
    let vault = Vault::from_note_paths([]);
    // A title on the next line: Comrak ends the link at its destination.
    let source = "text [a](b.md \"t\"\n) z\n\n[link](   /uri\n  \"title\"  ) end\n";
    let parsed = links::parse(source);
    assert_eq!(
        parsed
            .iter()
            .map(|l| &source[l.range.clone()])
            .collect::<Vec<_>>(),
        ["[a](b.md \"t\"\n)", "[link](   /uri\n  \"title\"  )"]
    );
    let rewritten = rewrite_source_links(source, &vault, "note.md");
    assert!(rewritten.starts_with("text [a](tessera://"), "{rewritten}");
    assert!(
        rewritten.contains("\"t\") z\n\n[link](tessera://"),
        "{rewritten}"
    );
    assert!(rewritten.ends_with("\"title\") end\n"), "{rewritten}");
    // Comrak also stops counting lines after such a link, so a later link in
    // the same paragraph gets a wrong range and stays as written.
    let source = "text [a](b.md \"t\"\n) z\n[c](d.md) end\n";
    let rewritten = rewrite_source_links(source, &vault, "note.md");
    assert!(rewritten.ends_with(") z\n[c](d.md) end\n"), "{rewritten}");
    // A paragraph that opens with a reference definition: Comrak places its
    // inlines as if the definition were absent. Those links stay as written.
    // Their targets still count, for backlinks and link state.
    for (source, targets) in [
        ("[r]: /u\npara [a](x.md) [b](y.md)\n", &["x.md", "y.md"][..]),
        ("[foo]: /url\n===\n[foo]\n", &["/url"]),
    ] {
        let parsed = links::parse(source);
        assert_eq!(
            parsed.iter().map(|l| l.target.as_str()).collect::<Vec<_>>(),
            targets
        );
        assert!(parsed.iter().all(|l| !l.exact_range), "{source:?}");
        assert_eq!(rewrite_source_links(source, &vault, "note.md"), source);
    }
    // Positive control: the same link after a blank line is found and rewritten.
    let source = "[r]: /u\n\npara [a](x.md)\n";
    assert!(links::parse(source)[0].exact_range);
    assert_eq!(&source[links::parse(source)[0].range.clone()], "[a](x.md)");
    assert_ne!(rewrite_source_links(source, &vault, "note.md"), source);
    // Multi-line labels inside containers keep their exact ranges.
    for (source, link) in [
        ("> quote [a\n> b](x.md)\n", "[a\n> b](x.md)"),
        ("- item [a\n  b](x.md)\n", "[a\n  b](x.md)"),
        ("para\n[a](\nx.md)\n", "[a](\nx.md)"),
    ] {
        assert_eq!(&source[links::parse(source)[0].range.clone()], link);
    }
}

#[test]
fn decoding_once_keeps_encoded_filename_delimiters_separate_from_fragment() {
    assert_eq!(
        links::destination(
            "name%23part%2520.md#%D0%A2%D0%B5%D1%81%D1%82%20heading",
            false
        ),
        Destination::Note {
            path: "name#part%20.md".into(),
            heading: Some("Тест heading".into())
        }
    );
    let root = tempfile::Builder::new()
        .prefix("tessera-decode-")
        .tempdir()
        .unwrap();
    std::fs::write(
        root.path().join("name#part%20.md"),
        "intro\n\n# Тест heading\n",
    )
    .unwrap();
    let vault = Vault::scan(root.path()).unwrap();
    let resolved = links::resolve(
        "name%23part%2520.md#%D0%A2%D0%B5%D1%81%D1%82%20heading",
        false,
        &vault,
        "source.md",
    );
    assert_eq!(resolved.candidates, ["name#part%20.md"]);
    assert_eq!(
        tessera_core::render::split_open_url(
            resolved
                .url
                .strip_prefix(tessera_core::render::WIKI_SCHEME)
                .unwrap()
        ),
        ("name#part%20.md".into(), Some("Тест heading".into()))
    );
    assert_eq!(
        links::destination("https://example.com/x.md#x", false),
        Destination::External("https://example.com/x.md#x".into())
    );
    for target in [
        "mailto:a@b",
        "tel:+123",
        "ftp://example.com/a",
        "HTTPS://example.com",
    ] {
        assert_eq!(
            links::destination(target, false),
            Destination::External(target.into())
        );
    }
    for target in ["file:///tmp/n.md", "custom:note.md", "javascript:alert(1)"] {
        assert!(matches!(
            links::destination(target, false),
            Destination::Unsupported(_)
        ));
    }
}

#[test]
fn fallback_requires_absence_and_relative_escape_never_falls_back() {
    let root = tempfile::Builder::new()
        .prefix("tessera-fallback-")
        .tempdir()
        .unwrap();
    for folder in ["notes", "a", "b"] {
        std::fs::create_dir(root.path().join(folder)).unwrap();
    }
    for path in [
        "root.md",
        "a/dup.md",
        "b/dup.md",
        "a/unique.md",
        "blocked.md",
        "bad.md",
    ] {
        std::fs::write(root.path().join(path), "# Destination").unwrap();
    }
    std::fs::create_dir(root.path().join("notes/blocked.md")).unwrap();
    std::fs::write(root.path().join("notes/bad.md"), [0xff]).unwrap();
    let vault = Vault::scan_metadata(root.path()).unwrap();
    let resolve = |target| links::resolve(target, false, &vault, "notes/start.md");
    assert_eq!(resolve("root.md").candidates, ["root.md"]);
    assert_eq!(resolve("unique.md").candidates, ["a/unique.md"]);
    assert_eq!(resolve("dup.md").status, "ambiguous");
    assert_eq!(resolve("dup.md").candidates.len(), 2);
    assert_eq!(resolve("blocked.md").status, "unresolved");
    assert_eq!(
        resolve("bad.md").candidates,
        ["notes/bad.md"],
        "invalid source must not choose root"
    );
    for target in ["./root.md", "../../root.md", "../missing/root.md"] {
        assert_eq!(resolve(target).status, "unresolved", "{target}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("missing", root.path().join("notes/root.md")).unwrap();
        assert_eq!(
            resolve("root.md").status,
            "unresolved",
            "dangling local entry is not absence"
        );
    }
}

#[test]
fn heading_inventory_retains_setext_and_refuses_duplicates_missing_and_nested() {
    let source = "intro\n\n# **Landing** `C#`\n\nbody\n\nSetext\n======\n";
    let heading = links::heading(source, "landing c#").unwrap();
    assert_eq!(
        (heading.block, heading.offset, heading.setext),
        (1, 7, false)
    );
    assert!(links::heading(source, "Setext").unwrap().setext);
    assert!(links::heading(source, "missing").is_err());
    assert!(links::heading("# Same\n\nSame\n====\n", "same").is_err());
    assert!(links::heading("# Same\n\n> # Same\n", "same").is_err());
    assert!(links::heading("`# Same`\n\n```\n# Same\n```\n", "same").is_err());
}

#[cfg(unix)]
#[test]
fn dangling_root_candidate_blocks_suffix_fallback() {
    let temp = tempfile::Builder::new()
        .prefix("tessera-root-guard-")
        .tempdir()
        .unwrap();
    std::fs::create_dir(temp.path().join("notes")).unwrap();
    std::fs::create_dir(temp.path().join("other")).unwrap();
    std::fs::write(
        temp.path().join("other/target.md"),
        "# Positive suffix control",
    )
    .unwrap();
    let vault = Vault::scan_metadata(temp.path()).unwrap();
    assert_eq!(
        links::resolve("target.md", false, &vault, "notes/from.md").candidates,
        ["other/target.md"]
    );
    std::os::unix::fs::symlink("missing", temp.path().join("target.md")).unwrap();
    assert_eq!(
        links::resolve("target.md", false, &vault, "notes/from.md").status,
        "unresolved"
    );
}

#[test]
fn wiki_targets_keep_literal_entities_and_backslash_bytes() {
    let temp = tempfile::Builder::new()
        .prefix("tessera-wiki-literal-")
        .tempdir()
        .unwrap();
    for path in ["a&amp;b.md", "a&b.md", "a\\&b.md"] {
        std::fs::write(temp.path().join(path), "# Literal").unwrap();
    }
    let vault = Vault::scan(temp.path()).unwrap();
    for (source, path) in [
        ("[[a&amp;b.md]]", "a&amp;b.md"),
        ("[[a\\&b.md|alias]]", "a\\&b.md"),
    ] {
        let parsed = links::parse(source);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].target, path);
        assert_eq!(
            links::resolve(&parsed[0].target, true, &vault, "start.md").candidates,
            [path]
        );
    }
}

#[test]
fn indexed_markdown_lookup_keeps_repeated_extensions_distinct() {
    let temp = tempfile::Builder::new()
        .prefix("tessera-repeat-extension-")
        .tempdir()
        .unwrap();
    for dir in ["notes", "other"] {
        std::fs::create_dir(temp.path().join(dir)).unwrap();
    }
    for path in [
        "notes/name.md",
        "notes/name.md.md",
        "other/fallback.md",
        "other/fallback.md.md",
    ] {
        std::fs::write(temp.path().join(path), "# Exact filename").unwrap();
    }
    let vault = Vault::scan_metadata(temp.path()).unwrap();
    for (target, path) in [
        ("name.md", "notes/name.md"),
        ("name.md.md", "notes/name.md.md"),
        ("fallback.md", "other/fallback.md"),
        ("fallback.md.md", "other/fallback.md.md"),
    ] {
        assert_eq!(
            links::resolve(target, false, &vault, "notes/from.md").candidates,
            [path]
        );
    }
}

#[test]
#[cfg(unix)]
fn obsidian_paths_share_click_backlink_and_rename_identity() {
    use tessera_core::link_rewrite::Preview;
    let temp = tempfile::tempdir().unwrap();
    // The owner sample's directory spelling, including literal tilde pairs.
    let root = temp.path().join(
        "Users/test-user/Library/Mobile Documents/iCloud~md~obsidian/Documents/Obsidian Vault",
    );
    std::fs::create_dir_all(root.join("Life")).unwrap();
    std::fs::write(root.join("Life/qa target.md"), "# Target").unwrap();
    let absolute = root
        .join("Life/qa target.md")
        .to_string_lossy()
        .into_owned();
    let source = format!(
        "[Full Path]({absolute})\n[Relative Path, sibling](qa target.md)\n[Encoded](qa%20target.md)\n[Angle](<qa target.md>)\n[Absolute angle](<{absolute}>)\n[Mixed](qa%20target.md#Target)\n"
    );
    std::fs::write(root.join("Life/start.md"), &source).unwrap();
    let vault = Vault::scan(&root).unwrap();
    let parsed = links::parse_in_vault(&source, &vault, "Life/start.md");
    assert_eq!(parsed.len(), 6, "{parsed:?}");
    for link in &parsed {
        let resolved = links::resolve(&link.target, link.wiki, &vault, "Life/start.md");
        assert_eq!(resolved.candidates, ["Life/qa target.md"], "{link:?}");
    }
    assert_eq!(vault.backlinks("Life/qa target.md").len(), 6);
    let output = rewrite_source_links(&source, &vault, "Life/start.md");
    assert_eq!(
        output.matches("tessera://open/Life/qa%20target.md").count(),
        6
    );
    assert!(!output.contains("~md~"));
    let html =
        tessera_core::render::render_html(&vault, "Life/start.md", "base16-ocean.dark").unwrap();
    assert_eq!(
        html.matches("href=\"tessera://open/Life/qa%20target.md")
            .count(),
        6,
        "{html}"
    );
    let preview = Preview::prepare(&root, "Life/qa target.md", "Life/renamed target.md").unwrap();
    assert!(preview.skipped.is_empty(), "{:?}", preview.skipped);
    assert_eq!(preview.changes.len(), 6, "{:?}", preview.changes);
    let changed = preview.rewritten("Life/start.md").unwrap();
    std::fs::rename(
        root.join("Life/qa target.md"),
        root.join("Life/renamed target.md"),
    )
    .unwrap();
    let after = Vault::scan(&root).unwrap();
    for link in links::parse_in_vault(&changed, &after, "Life/start.md") {
        assert_eq!(
            links::resolve(&link.target, link.wiki, &after, "Life/start.md").candidates,
            ["Life/renamed target.md"]
        );
    }
}

#[test]
fn whitespace_fallback_requires_existing_file_and_real_link_context() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("qa target.md"), "# Target").unwrap();
    let vault = Vault::scan(temp.path()).unwrap();
    for source in [
        "plain text (qa target.md)",
        "[Missing](missing file.md)",
        "`[Code](qa target.md)`",
        "```md\n[Code](qa target.md)\n```",
        "    [Code](qa target.md)",
        "<!-- [Comment](qa target.md) -->",
        "\\[Escaped](qa target.md)",
        "![Image](qa target.md)",
        "![image [label](qa target.md)](image.png)",
    ] {
        assert!(
            links::parse_in_vault(source, &vault, "start.md").is_empty(),
            "{source}"
        );
        assert_eq!(rewrite_source_links(source, &vault, "start.md"), source);
    }
    let strict = "[Title](<qa target.md> \"Title with spaces\")";
    let parsed = links::parse_in_vault(strict, &vault, "start.md");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].title, "Title with spaces");
    // Mixed URL encoding is decoded once, not twice.
    std::fs::write(temp.path().join("literal %20 file.md"), "# Literal").unwrap();
    let vault = Vault::scan(temp.path()).unwrap();
    let parsed = links::parse_in_vault("[Mixed](literal %2520 file.md)", &vault, "start.md");
    assert_eq!(parsed.len(), 1);
    assert_eq!(
        links::resolve(&parsed[0].target, false, &vault, "start.md").candidates,
        ["literal %20 file.md"]
    );
}

#[test]
fn outside_absolute_file_is_explicit_and_never_a_vault_note() {
    let vault_dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("outside note.md");
    std::fs::write(&path, "# Outside").unwrap();
    let vault = Vault::scan(vault_dir.path()).unwrap();
    let source = format!("[Outside]({})", path.display());
    let parsed = links::parse_in_vault(&source, &vault, "start.md");
    assert_eq!(parsed.len(), 1);
    let resolved = links::resolve(&parsed[0].target, false, &vault, "start.md");
    assert_eq!(resolved.status, "outside_file");
    assert!(resolved.candidates.is_empty());
    assert!(resolved.url.starts_with("tessera://outside-file/"));
    assert_eq!(
        vault.resolve_markdown(path.to_str().unwrap(), "start.md"),
        tessera_core::Resolution::Unresolved
    );
}

#[test]
fn image_rewriting_shares_link_identity_and_keeps_cached_and_quick_view_images() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("notes")).unwrap();
    std::fs::write(temp.path().join("notes/start.md"), "# Start").unwrap();
    std::fs::write(temp.path().join("notes/image (1).png"), b"sibling").unwrap();
    std::fs::write(temp.path().join("image (1).png"), b"root").unwrap();
    let mut vault = Vault::scan_metadata(temp.path()).unwrap();
    let source = "![`bracket ]` and **bold**][image]\n\n[image]: <image%20(1).png>\n\n`![code](image%20(1).png)`\n";
    let rendered = tessera_core::render::rewrite_source_images(source, &vault, "notes/start.md");
    assert!(
        rendered.contains("![`bracket ]` and **bold**](<file://"),
        "{rendered}"
    );
    assert!(rendered.contains("/notes/image%20(1).png>"));
    assert!(rendered.contains("`![code](image%20(1).png)`"));
    assert_eq!(
        links::resolve("image%20(1).png", false, &vault, "notes/start.md").candidates,
        ["notes/image (1).png"]
    );
    vault.inventory_complete = false;
    assert_eq!(
        tessera_core::render::rewrite_source_images(source, &vault, "notes/start.md"),
        rendered
    );
    vault.single_file = true;
    assert_eq!(
        tessera_core::render::rewrite_source_images(source, &vault, "notes/start.md"),
        rendered
    );
}
