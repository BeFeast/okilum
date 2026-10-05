//! Source-level link rewriting, as the GPUI shell uses it — and the regression
//! for #15: a note must rewrite the same way whether it is addressed by a
//! vault-relative path or by an absolute path to a file outside the vault.

use std::fs;
use std::path::{Path, PathBuf};

use tessera_core::render::{rewrite_source_links, AMBIGUOUS_SCHEME, WIKI_SCHEME};
use tessera_core::Vault;

fn vault_with(name: &str, files: &[(&str, &str)]) -> (PathBuf, Vault) {
    let root = std::env::temp_dir().join(format!("tessera-srclinks-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for (rel, body) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }
    let vault = Vault::scan(Path::new(&root)).unwrap();
    (root, vault)
}

const DEMO: &str = "\
Resolved: [[notes/alpha]].
Ambiguous: [[alpha]].
Unresolved: [[no-such-note]].
Aliased: [[notes/alpha|the alias]].
Md link: [text](notes/alpha.md).
";

#[test]
fn every_link_state_rewrites_as_specified() {
    let (_root, v) = vault_with(
        "states",
        &[("notes/alpha.md", "# A\n"), ("dup/alpha.md", "# B\n")],
    );
    let out = rewrite_source_links(DEMO, &v, "demo.md");

    assert!(
        out.contains(&format!("[notes/alpha]({WIKI_SCHEME}notes/alpha.md)")),
        "{out}"
    );
    assert!(
        out.contains(&format!("[alpha]({AMBIGUOUS_SCHEME}alpha)")),
        "{out}"
    );
    assert!(
        out.contains("[no-such-note](tessera://unresolved/no-such-note)"),
        "{out}"
    );
    assert!(
        out.contains(&format!("[the alias]({WIKI_SCHEME}notes/alpha.md)")),
        "{out}"
    );
    assert!(
        out.contains(&format!("[text]({WIKI_SCHEME}notes/alpha.md)")),
        "{out}"
    );
    assert!(
        !out.contains("[["),
        "no literal wikilink may survive: {out}"
    );
}

#[test]
fn a_file_outside_the_vault_rewrites_the_same_way_as_one_inside_it() {
    // #15. The shell used to take a separate branch for an absolute --note that
    // skipped link rewriting entirely, so the same bytes rendered as live links
    // when addressed vault-relatively and as literal `[[...]]` when addressed
    // absolutely. The only thing an absolute path may change is where the
    // bytes come from.
    let (_root, v) = vault_with(
        "outside",
        &[("notes/alpha.md", "# A\n"), ("dup/alpha.md", "# B\n")],
    );
    let inside = rewrite_source_links(DEMO, &v, "demo.md");
    let outside = rewrite_source_links(DEMO, &v, "");
    assert_eq!(inside, outside);
}

#[test]
fn a_relative_link_from_outside_the_vault_is_unresolved_not_guessed() {
    // With no source location, `../x` means nothing. The honest answer is a
    // struck-through link, not a suffix match against whatever ends in `x`.
    let (_root, v) = vault_with(
        "relative-outside",
        &[
            ("proj-a/ops/signoff.md", "# A\n"),
            ("proj-b/ops/signoff.md", "# B\n"),
        ],
    );
    let out = rewrite_source_links("See [[../ops/signoff]].", &v, "");
    assert!(
        out.contains("[../ops/signoff](tessera://unresolved/../ops/signoff)"),
        "{out}"
    );
    // And from inside, the same link resolves against its own directory.
    let out = rewrite_source_links("See [[../ops/signoff]].", &v, "proj-a/planning/plan.md");
    assert!(
        out.contains(&format!("({WIKI_SCHEME}proj-a/ops/signoff.md)")),
        "{out}"
    );
}

#[test]
fn heading_anchors_do_not_break_resolution() {
    let (_root, v) = vault_with("anchors", &[("notes/alpha.md", "# A\n")]);
    let out = rewrite_source_links("[[notes/alpha#Some Heading|see]]", &v, "demo.md");
    assert!(
        out.contains(&format!("[see]({WIKI_SCHEME}notes/alpha.md#")),
        "{out}"
    );
}

#[test]
fn note_source_renders_a_file_the_same_way_whichever_path_names_it() {
    // The actual #15 path: note_source() decides where the bytes come from AND
    // must not let that decision change the rewriting. Testing rewrite_source_links
    // alone would have missed the defect entirely, because the bug was that the
    // shell never called it on the absolute branch.
    use tessera_core::render::note_source;

    let (root, v) = vault_with(
        "note-source",
        &[
            ("demo.md", DEMO),
            ("notes/alpha.md", "# A\n"),
            ("dup/alpha.md", "# B\n"),
        ],
    );

    let by_rel = note_source(&v, "demo.md").unwrap();
    let by_abs = note_source(&v, root.join("demo.md").to_str().unwrap()).unwrap();

    assert_eq!(by_rel, by_abs, "the address must not change the rendering");
    assert!(
        by_rel.contains(WIKI_SCHEME),
        "links must be rewritten: {by_rel}"
    );
    assert!(
        !by_rel.contains("[["),
        "no literal wikilink may survive: {by_rel}"
    );
}

#[test]
fn frontmatter_is_stripped_before_rendering() {
    use tessera_core::render::note_source;
    let (_root, v) = vault_with(
        "frontmatter",
        &[(
            "demo.md",
            "---\ntitle: Demo\ntags: [x]\n---\n\n# Body\n\nSee [[demo]].\n",
        )],
    );
    let out = note_source(&v, "demo.md").unwrap();
    assert!(
        !out.contains("tags:"),
        "frontmatter must not reach the renderer: {out}"
    );
    assert!(!out.contains("title: Demo"), "{out}");
    // The blank line that followed the closing `---` survives, so compare after
    // trimming: the point is that nothing of the frontmatter is left, not that
    // the body starts at byte zero.
    assert!(out.trim_start().starts_with("# Body"), "{out}");
    assert!(
        out.contains(&format!("[demo]({WIKI_SCHEME}demo.md)")),
        "links still rewritten: {out}"
    );
}

// ---------------------------------------------------------------------------
// #20: a wikilink inside code is text about a link, not a link.
// ---------------------------------------------------------------------------

/// The repro from the issue. `b.md` exists, `x` does not.
const CODE_REPRO: &str = "\
# A

Inline `[[x]]` and [[x]] and [[b]].

```yaml
related_to: \"[[x]]\"
```

    indented [[x]]
";

#[test]
fn wikilinks_inside_code_round_trip_unchanged() {
    let (_root, v) = vault_with("code-repro", &[("b.md", "# B\n")]);
    let out = rewrite_source_links(CODE_REPRO, &v, "demo.md");
    let expected = format!(
        "\
# A

Inline `[[x]]` and [x](tessera://unresolved/x) and [b]({WIKI_SCHEME}b.md).

```yaml
related_to: \"[[x]]\"
```

    indented [[x]]
"
    );
    assert_eq!(out, expected);
}

#[test]
fn a_tilde_fence_and_an_info_string_are_still_fences() {
    let (_root, v) = vault_with("tilde", &[("b.md", "# B\n")]);
    let src = "~~~markdown\n[[x]] [[b]]\n~~~\n[[b]]\n";
    let out = rewrite_source_links(src, &v, "demo.md");
    assert_eq!(
        out,
        format!("~~~markdown\n[[x]] [[b]]\n~~~\n[b]({WIKI_SCHEME}b.md)\n")
    );
}

#[test]
fn a_double_backtick_span_holding_a_single_backtick_is_one_span() {
    let (_root, v) = vault_with("dbl-tick", &[("b.md", "# B\n")]);
    let src = "See `` [[x]] ` [[x]] `` then [[b]].\n";
    let out = rewrite_source_links(src, &v, "demo.md");
    assert_eq!(
        out,
        format!("See `` [[x]] ` [[x]] `` then [b]({WIKI_SCHEME}b.md).\n")
    );
}

#[test]
fn an_unclosed_fence_makes_everything_after_it_code() {
    let (_root, v) = vault_with("unclosed", &[("b.md", "# B\n")]);
    let src = "[[b]]\n```\n[[x]]\n\n[[b]]\n";
    let out = rewrite_source_links(src, &v, "demo.md");
    assert_eq!(
        out,
        format!("[b]({WIKI_SCHEME}b.md)\n```\n[[x]]\n\n[[b]]\n")
    );
}

#[test]
fn a_wikilink_right_after_a_closed_inline_span_is_rewritten() {
    let (_root, v) = vault_with("after-span", &[("b.md", "# B\n")]);
    let src = "`[[x]]`[[b]] and `a`[[x]]\n";
    let out = rewrite_source_links(src, &v, "demo.md");
    assert_eq!(
        out,
        format!("`[[x]]`[b]({WIKI_SCHEME}b.md) and `a`[x](tessera://unresolved/x)\n")
    );
}

#[test]
fn images_and_embeds_inside_code_are_not_rewritten_either() {
    // The same boundary applies to the two other source rewriters, or a code
    // example of an image would still sprout a file:// URL.
    use tessera_core::render::note_source;
    let (_root, v) = vault_with(
        "code-images",
        &[
            (
                "demo.md",
                "`![[pic.png]]` `![a](pic.png)`\n\n    ![[pic.png]]\n\n![[pic.png]]\n",
            ),
            ("pic.png", "not really a png"),
        ],
    );
    let out = note_source(&v, "demo.md").unwrap();
    assert!(
        out.starts_with("`![[pic.png]]` `![a](pic.png)`\n\n    ![[pic.png]]\n\n![](file://"),
        "{out}"
    );
}

// --- heading targets (#49) ---

#[test]
fn heading_fragments_survive_the_rewrite() {
    use tessera_core::render::split_open_url;
    let (_root, v) = vault_with("frag", &[("notes/alpha.md", "# A\n")]);
    let out = rewrite_source_links("[[notes/alpha#Some Heading|see]]", &v, "demo.md");
    assert_eq!(
        out,
        format!("[see]({WIKI_SCHEME}notes/alpha.md#Some%20Heading)")
    );
    let (rel, heading) = split_open_url("notes/alpha.md#Some%20Heading");
    assert_eq!(
        (rel.as_str(), heading.as_deref()),
        ("notes/alpha.md", Some("Some Heading"))
    );
    assert_eq!(
        split_open_url("notes/alpha.md"),
        ("notes/alpha.md".to_string(), None)
    );
}

#[test]
fn a_bare_heading_link_names_the_note_itself() {
    let (_root, v) = vault_with("selfref", &[("demo.md", "# A\n")]);
    let out = rewrite_source_links("[[#Setup]]", &v, "demo.md");
    assert_eq!(out, format!("[#Setup]({WIKI_SCHEME}demo.md#Setup)"));
    // A file outside the vault has no path; the shell reads "" as "this note".
    let out = rewrite_source_links("[[#Setup|go]]", &v, "");
    assert_eq!(out, format!("[go]({WIKI_SCHEME}#Setup)"));
}

#[test]
fn block_references_are_explicitly_unsupported() {
    let (_root, v) = vault_with("blockref", &[("notes/alpha.md", "# A\n")]);
    let out = rewrite_source_links("[[notes/alpha#^abc123]]", &v, "demo.md");
    assert_eq!(
        out,
        "[notes/alpha#^abc123](tessera://unsupported/Block%20references%20are%20not%20supported.%20Target%3A%20notes/alpha%23%5Eabc123)"
    );
}

#[test]
fn heading_matching_is_obsidian_not_github() {
    use tessera_core::render::heading_matches;
    assert!(heading_matches("Some Heading", "some heading"));
    assert!(heading_matches("  Some   Heading ", "Some Heading"));
    assert!(heading_matches("Setup & Run", "setup & run"));
    assert!(!heading_matches("Some Heading", "some-heading"));
    assert!(!heading_matches("Some Heading", ""));
    assert!(!heading_matches("Other", "Some Heading"));
}

#[test]
fn heading_block_index_counts_top_level_blocks_and_ignores_code() {
    use tessera_core::render::heading_block_index;
    let src = "\
# Title

intro `# not a heading`

```
# Setup
```

- a list
- with items

## Setup

body

## Teardown
";
    assert_eq!(heading_block_index(src, "Title"), Some(0));
    assert_eq!(heading_block_index(src, "setup"), Some(4));
    assert_eq!(heading_block_index(src, "TEARDOWN "), Some(6));
    assert_eq!(heading_block_index(src, "not a heading"), None);
    assert_eq!(heading_block_index(src, "missing"), None);
}

#[test]
fn table_wiki_aliases_and_embeds_preserve_cells_and_exact_targets() {
    use comrak::{nodes::NodeValue, parse_document, Arena};
    let source = "| Note | Other |\n| --- | --- |\n| [[notes/alpha|Русский alias]] | one |\n| [[notes/alpha\\|Escaped alias]] | two |\n| ![[notes/alpha|200]] | three |\n| ![[picture.png|200]] | four |\n| ![[picture.png\\|200]] | five |\n| literal\\|pipe | six |\n";
    let (root, vault) = vault_with(
        "table-pipes",
        &[("notes/alpha.md", "# Alpha"), ("table.md", source)],
    );
    fs::write(root.join("picture.png"), b"asset").unwrap();
    let rendered = tessera_core::render::reader_document(&vault, "table.md")
        .unwrap()
        .rendered;
    let arena = Arena::new();
    let doc = parse_document(&arena, &rendered, &tessera_core::render::comrak_options());
    let table = doc
        .children()
        .find(|n| matches!(n.data.borrow().value, NodeValue::Table(_)))
        .expect("table survives rewriting");
    assert_eq!(table.children().count(), 7);
    for row in table.children() {
        assert_eq!(row.children().count(), 2, "{rendered}");
    }
    let links: Vec<_> = doc
        .descendants()
        .filter_map(|n| match &n.data.borrow().value {
            NodeValue::Link(link) => Some(link.url.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        links,
        vec![format!("{WIKI_SCHEME}notes/alpha.md"); 3],
        "{rendered}"
    );
    assert_eq!(
        doc.descendants()
            .filter(|n| matches!(n.data.borrow().value, NodeValue::Image(_)))
            .count(),
        2
    );
    assert!(rendered.contains("[Русский alias]"));
    assert!(rendered.contains("[Escaped alias]"));
    assert!(rendered.contains("[200]"));
    assert_eq!(fs::read_to_string(root.join("table.md")).unwrap(), source);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn table_wiki_link_ranges_remain_canonical_with_unicode_and_escaped_delimiters() {
    let source = "| Note | Next |\n| --- | --- |\n| [[путь|alias]] [[другой\\|подпись]] | [[last|end]] |\n\n`[[code|literal]]`\n\n```md\n| [[fenced|literal]] |\n```\n";
    let parsed = tessera_core::document_links::parse(source);
    assert_eq!(parsed.len(), 3);
    for (link, (authored, target, label)) in parsed.iter().zip([
        ("[[путь|alias]]", "путь", "alias"),
        ("[[другой\\|подпись]]", "другой", "подпись"),
        ("[[last|end]]", "last", "end"),
    ]) {
        assert_eq!(&source[link.range.clone()], authored);
        assert_eq!(link.target, target);
        assert_eq!(link.label, label);
    }
}

#[test]
fn wiki_separator_mask_never_leaks_into_normal_markdown_urls_or_titles() {
    let source = r#"[example](https://example.test/[[x|y]] "a [[title|alias]]")"#;
    let links = tessera_core::document_links::parse(source);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target, "https://example.test/[[x|y]]");
    assert_eq!(links[0].title, "a [[title|alias]]");
}

#[test]
fn three_column_alias_fixture_keeps_gfm_row_shape() {
    use comrak::{nodes::NodeValue, parse_document, Arena};
    let source = include_str!("../../../fixtures/reader/table-alias-columns.md");
    let (root, vault) = vault_with(
        "table-three-columns",
        &[
            ("table.md", source),
            ("notes/alpha.md", "Alpha"),
            ("notes/beta.md", "Beta"),
            ("notes/gamma.md", "Gamma"),
        ],
    );
    let rendered = tessera_core::render::reader_document(&vault, "table.md")
        .unwrap()
        .rendered;
    let arena = Arena::new();
    let document = parse_document(&arena, &rendered, &tessera_core::render::comrak_options());
    let table = document
        .children()
        .find(|n| matches!(n.data.borrow().value, NodeValue::Table(_)))
        .unwrap();
    assert_eq!(table.children().count(), 5);
    assert!(table.children().all(|row| row.children().count() == 3));
    assert!(!rendered.contains("[["), "all aliases resolved: {rendered}");
    assert!(rendered.contains("[First alias]") && rendered.contains("[Second alias]"));
    assert_eq!(fs::read_to_string(root.join("table.md")).unwrap(), source);
    fs::remove_dir_all(root).unwrap();
}
