use super::*;
use crate::source_projection::{project, Bias, FallbackReason};

fn snapshot(source: &str) -> Snapshot {
    Snapshot::new("synthetic.md", 7, source)
}
fn display(source: &str) -> String {
    let current = snapshot(source);
    let classified = classify(&current);
    project(&current, classified.plan(), &Active::default())
        .unwrap()
        .display()
        .to_owned()
}

#[test]
fn supported_forms_have_exact_authored_display() {
    for (source, expected) in [
        ("", ""),
        ("plain paragraph", "plain paragraph"),
        ("# heading", "heading"),
        ("### **bold** ###", "bold"),
        (
            "**bold** *italic* __under__ _em_ ~~strike~~",
            "bold italic under em strike",
        ),
        ("***nested***", "nested"),
        ("` x ` and `` a ` b ``", " x  and  a ` b "),
        ("[label](https://example.test/a)", "label"),
        ("[[Target]] [[Target|alias]]", "Target alias"),
        ("**b&amp;d** and [a\\]b](target)", "b&amp;d and a\\]b"),
        ("escaped \\*star\\* **bold**", "escaped \\*star\\* bold"),
    ] {
        assert_eq!(display(source), expected, "{source:?}");
    }
}

#[test]
fn semantic_styles_are_raw_canonical_ranges() {
    let current = snapshot("# **é** and [label](dest)");
    let result = classify(&current);
    assert_eq!(
        result.styles_for(&current).unwrap(),
        &[
            StyleSpan {
                range: 2..26,
                style: Style::Heading(1)
            },
            StyleSpan {
                range: 4..6,
                style: Style::Strong
            },
            StyleSpan {
                range: 14..19,
                style: Style::Link
            },
        ]
    );
    let current = snapshot("***nested***");
    let result = classify(&current);
    assert_eq!(
        result.styles_for(&current).unwrap(),
        &[
            StyleSpan {
                range: 1..11,
                style: Style::Emphasis
            },
            StyleSpan {
                range: 3..9,
                style: Style::Strong
            },
        ]
    );
}

#[test]
fn unsupported_and_malformed_blocks_have_positive_neighbor() {
    for unsupported in [
        "```md\n**code**\n```",
        "    **indented**",
        "| a | b |\n|---|---|\n|**x**|y|",
        "![image](url) **bold**",
        "![[embed]] **bold**",
        "title\n=====",
        "<div>**html**</div>",
        "---",
        "**bold** [broken",
        "**bold** `broken",
        "**bold** ~single~",
        "**bold** [ref][id]",
        "**bold** [label](dest \"title\")",
        "**bold** [label](a(b)c)",
        "**bold** [label](a\\)b)",
        "**bold** <https://example.test>",
        "**bold** [[Target|]]",
        "**bold** [[A|B|C]]",
        "**bold** `one\ntwo`",
        "**bold** [[a\\|b]]",
        "**bold** *unfinished",
        "**bold** ==highlight==",
        "**bold** $math$",
    ] {
        // Place valid neighbor first so a thematic break cannot become frontmatter.
        let source = format!("# neighbor\n\n{unsupported}");
        let expected = format!("neighbor\n\n{unsupported}");
        assert_eq!(display(&source), expected, "unsupported={unsupported:?}");
    }
    assert_eq!(
        display("---\na: **raw**\n---\n\n# neighbor"),
        "---\na: **raw**\n---\n\nneighbor"
    );
    assert_eq!(
        display("+++\na: **raw**\n+++\n\n# neighbor"),
        "+++\na: **raw**\n+++\n\nneighbor"
    );
    let unclosed = "---\na: **raw**\n\n# neighbor";
    assert_eq!(display(unclosed), unclosed);
}

#[test]
fn containers_project_inline_and_keep_container_syntax_visible() {
    for (source, expected) in [
        ("> **quote**", "> quote"),
        ("- **list**", "- list"),
        ("10) **ten**", "10) ten"),
        (
            "1. *one*\n   - `two`\n     > **three** [[T|alias]]",
            "1. one\n   - two\n     > three alias",
        ),
        (
            "- [ ] task **b**\n- [x] done ~~s~~",
            "- [ ] task b\n- [x] done s",
        ),
        ("> a **b\n> c** d\nlazy *x*", "> a b\n> c d\nlazy x"),
        ("> # **T**\n> [l](d)", "> T\n> l"),
        ("-\t**tab**", "-\ttab"),
        ("\u{feff}- **a**\r\n> *b*\r\n", "\u{feff}- a\r\n> b\r\n"),
        (
            "- **привет** *שָׁלוֹם* e\u{301} 😀",
            "- привет שָׁלוֹם e\u{301} 😀",
        ),
        // Unsupported blocks inside containers stay exact; siblings project.
        (
            "- **a**\n\n      code **x**\n- *b*",
            "- a\n\n      code **x**\n- b",
        ),
        ("> ```\n> **x**\n> ```\n> *y*", "> ```\n> **x**\n> ```\n> y"),
        (
            "> | a |\n> |---|\n> | **x** |",
            "> | a |\n> |---|\n> | **x** |",
        ),
        (
            "> title **x**\n> ===\n\n- *y*",
            "> title **x**\n> ===\n\n- y",
        ),
        ("- **a** ![i](x)\n- *b*", "- **a** ![i](x)\n- b"),
    ] {
        assert_eq!(display(source), expected, "{source:?}");
    }
    let source = "- [[yes]]\n> [[also]]";
    let current = snapshot(source);
    let targets: Vec<_> = classify(&current)
        .links_for(&current)
        .unwrap()
        .iter()
        .map(|link| link.target.clone())
        .collect();
    assert_eq!(targets, ["yes", "also"]);
}

#[test]
fn resolved_references_and_wrapped_labels_project_and_definitions_stay_quiet() {
    let source = "[full][Id] [Id][] [Id] [raw][nope]\n[wrapped\nlabel](dest)\n\n[Id]: /x \"t\"\n   [two]: y\n";
    let current = snapshot(source);
    let result = classify(&current);
    // The unresolved reference keeps its whole paragraph raw.
    assert_eq!(
        project(&current, result.plan(), &Active::default())
            .unwrap()
            .display(),
        source
    );
    let source =
        "[full][Id] [Id][] [Id]\n[wrapped\r\nlabel](dest)\r\n\r\n[Id]: /x \"t\"\r\n   [two]: y\r\n";
    let current = snapshot(source);
    let result = classify(&current);
    assert_eq!(
        project(&current, result.plan(), &Active::default())
            .unwrap()
            .display(),
        "full Id Id\nwrapped\r\nlabel\r\n\r\n[Id]: /x \"t\"\r\n   [two]: y\r\n"
    );
    let links = result.links_for(&current).unwrap();
    let targets: Vec<_> = links.iter().map(|l| l.target.as_str()).collect();
    assert_eq!(targets, ["/x", "/x", "/x", "dest"]);
    assert_eq!(&source[links[3].label.clone()], "wrapped\r\nlabel");
    let quiet: Vec<_> = result
        .styles_for(&current)
        .unwrap()
        .iter()
        .filter(|s| s.style == Style::Definition)
        .map(|s| &source[s.range.clone()])
        .collect();
    assert_eq!(quiet, ["[Id]: /x \"t\"", "   [two]: y"]);
    // A wrapped label must not style the next quote prefix.
    assert_eq!(display("> [a\n> b](d)"), "> [a\n> b](d)");
    assert_eq!(display("- [a\n  b](d)"), "- a\n  b");
}

#[test]
fn line_endings_unicode_and_raw_spaces_survive() {
    for (source, expected) in [
        (
            "\u{feff}# כותרת 🧠\r\n\r\né **жир**\r\n",
            "\u{feff}כותרת 🧠\r\n\r\né жир\r\n",
        ),
        ("#\tTitle\n\ntext\t**bold**", "Title\n\ntext\tbold"),
        ("**one\r\ntwo**", "one\r\ntwo"),
        ("plain\r\n**two**", "plain\r\ntwo"),
        ("  # heading  ", "  heading  "),
        ("**e\u{301}**", "e\u{301}"),
        ("**👩‍👩‍👧‍👦**", "👩‍👩‍👧‍👦"),
    ] {
        assert_eq!(display(source), expected, "{source:?}");
    }
    for source in ["**one**\r# two", "**a\0b** [x](d)"] {
        let current = snapshot(source);
        let result = classify(&current);
        assert_eq!(result.reasons(), &[SourceReason::ParserCoordinates]);
        assert_eq!(display(source), source);
    }
}

#[test]
fn grapheme_join_is_exact_source_fallback() {
    let source = "🇦**🇧**";
    let current = snapshot(source);
    let result = classify(&current);
    assert_eq!(result.reasons(), &[SourceReason::ProjectionBoundary]);
    assert_eq!(display(source), source);
}

#[test]
fn active_selection_and_composition_reveal_complete_blocks() {
    let source = "# title\n\n**bold** and [[target|label]]";
    let current = snapshot(source);
    let result = classify(&current);
    for active in [
        Active {
            selection: Some(12..12),
            composition: None,
        },
        Active {
            selection: None,
            composition: Some(11..14),
        },
    ] {
        let projection = project(&current, result.plan(), &active).unwrap();
        assert_eq!(
            projection.display(),
            "title\n\n**bold** and [[target|label]]"
        );
    }
    let projection = project(&current, result.plan(), &Active::default()).unwrap();
    assert_eq!(projection.display(), "title\n\nbold and label");
    assert_eq!(
        projection
            .display_to_source(&current, 0, Bias::Left)
            .unwrap(),
        0
    );
    assert_eq!(
        projection
            .display_to_source(&current, 0, Bias::Right)
            .unwrap(),
        2
    );
    assert_eq!(current.copy_source(9..17).unwrap(), "**bold**");
}

#[test]
fn styles_and_projection_reject_every_stale_identity() {
    let source = "**bold**";
    let original = snapshot(source);
    let result = classify(&original);
    for current in [
        Snapshot::new("other.md", 7, source),
        Snapshot::new("synthetic.md", 8, source),
        snapshot("**changed**"),
    ] {
        assert_eq!(result.styles_for(&current), Err(MapError::StaleSnapshot));
        let fallback = project(&current, result.plan(), &Active::default()).unwrap_err();
        assert_eq!(fallback.reason, FallbackReason::StaleSnapshot);
        assert_eq!(fallback.source(), current.source());
    }
}

#[test]
fn input_and_structure_limits_preserve_exact_source() {
    let at_limit = format!("# {}", "a".repeat(MAX_BYTES - 2));
    assert_eq!(display(&at_limit), &at_limit[2..]);
    let over = "a".repeat(MAX_BYTES + 1);
    assert_eq!(
        classify(&snapshot(&over)).reasons(),
        &[SourceReason::InputLimit]
    );
    assert_eq!(display(&over), over);
    let nodes = "**x** ".repeat(1500);
    assert_eq!(
        classify(&snapshot(&nodes)).reasons(),
        &[SourceReason::StructureLimit]
    );
    assert_eq!(display(&nodes), nodes);
    let deep = format!("{}x", "> ".repeat(MAX_DEPTH + 2));
    assert_eq!(
        classify(&snapshot(&deep)).reasons(),
        &[SourceReason::StructureLimit]
    );
    assert_eq!(display(&deep), deep);
}

#[test]
fn generated_syntax_has_independent_source_edit_and_mapping_oracle() {
    for count in 1..65 {
        let labels: Vec<_> = (0..count).map(|i| format!("word{i}")).collect();
        let source = labels
            .iter()
            .map(|label| format!("**{label}**"))
            .collect::<Vec<_>>()
            .join(" ");
        let expected = labels.join(" ");
        let current = snapshot(&source);
        let result = classify(&current);
        let projection = project(&current, result.plan(), &Active::default()).unwrap();
        assert_eq!(projection.display(), expected);
        for offset in 0..=expected.len() {
            for bias in [Bias::Left, Bias::Right] {
                let raw = projection
                    .display_to_source(&current, offset, bias)
                    .unwrap();
                assert_eq!(projection.source_to_display(&current, raw).unwrap(), offset);
            }
        }
        let changed = current
            .replace_source(&current, 2..7, "replacement")
            .unwrap();
        assert_eq!(changed.source(), format!("**replacement{}", &source[7..]));
        assert_eq!(current.copy_source(0..source.len()).unwrap(), source);
    }
}

#[test]
fn caret_in_authored_indentation_reveals_heading() {
    let current = snapshot("  # heading");
    let result = classify(&current);
    let active = Active {
        selection: Some(0..0),
        composition: None,
    };
    assert_eq!(
        project(&current, result.plan(), &active).unwrap().display(),
        current.source()
    );
}

#[test]
fn range_limit_has_a_positive_below_limit_control() {
    let block = format!("{}x{}", "*".repeat(58), "*".repeat(58));
    let below = vec![block.as_str(); 69].join("\n\n");
    assert_eq!(display(&below), vec!["x"; 69].join("\n\n"));
    let over = vec![block.as_str(); 70].join("\n\n");
    assert_eq!(
        classify(&snapshot(&over)).reasons(),
        &[SourceReason::StructureLimit]
    );
    assert_eq!(display(&over), over);
}

#[test]
fn open_link_descriptors_preserve_raw_unicode_ranges_and_snapshot_identity() {
    let raw = "\u{feff}# Header\r\n\r\n[[ notes/План.md |😀 alias]] [local](../notes/space%20name.md)\r\n";
    let snap = snapshot(raw);
    let result = classify(&snap);
    let links = result.links_for(&snap).unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(&raw[links[0].range.clone()], "[[ notes/План.md |😀 alias]]");
    assert_eq!(&raw[links[0].label.clone()], "😀 alias");
    assert_eq!(links[0].target, " notes/План.md ");
    assert!(links[0].wiki);
    assert_eq!(links[1].target, "../notes/space%20name.md");
    assert!(!links[1].wiki);
    assert_eq!(display(raw), "\u{feff}Header\r\n\r\n😀 alias local\r\n");
    assert!(result
        .links_for(&Snapshot::new("other.md", 7, raw))
        .is_err());
    assert!(result
        .links_for(&Snapshot::new("synthetic.md", 8, raw))
        .is_err());
}

#[test]
fn open_link_descriptors_never_escape_rejected_blocks_or_global_caps() {
    for rejected in [
        "> ```\n> [[no]]\n> ```",
        "- [[no]] ![image](x)",
        "```\n[[no]]\n```",
        "[[no]] ![image](x)",
        "[**nested**](x.md)",
        "[ref][missing]\n\n[id]: x.md",
    ] {
        let raw = format!("{rejected}\n\n[[yes]]\n");
        let snap = snapshot(&raw);
        let result = classify(&snap);
        let links = result.links_for(&snap).unwrap();
        assert_eq!(links.len(), 1, "{rejected}");
        assert_eq!(links[0].target, "yes", "positive accepted neighbor");
    }
    for raw in [
        format!("[[x]]{}", "a".repeat(MAX_BYTES)),
        "[[x]] ".repeat(MAX_NODES),
        "[[x]]\rbroken".into(),
    ] {
        let snap = snapshot(&raw);
        assert!(classify(&snap).links_for(&snap).unwrap().is_empty());
    }
}

#[test]
fn heading_navigation_uses_exact_raw_atx_offsets_and_reader_normalization() {
    let raw = "\u{feff}---\r\ntype: Note\r\n---\r\n# Origin\r\n\r\n## **Decision** `C#` 😀 ###\r\nBody\r\n";
    let c = classify(&snapshot(raw));
    assert_eq!(
        c.heading_offset("  decision   C# 😀  "),
        Ok(raw.find("## **Decision").unwrap())
    );
    assert!(c.heading_offset("decision-c#-😀").is_err());
    assert!(c.heading_offset("absent").is_err());
}

#[test]
fn heading_navigation_refuses_duplicates_and_unsupported_targets() {
    let c = classify(&snapshot("# **Same**\n\n# same\n"));
    assert!(c
        .heading_offset("SAME")
        .unwrap_err()
        .to_ascii_lowercase()
        .contains("multiple"));
    let c = classify(&snapshot(
        "# Same\n\n# [**Same**](https://example.com) extra\n",
    ));
    assert!(c.heading_offset("Same extra").is_err());
    let c = classify(&snapshot(
        "# Same extra\n\n# [**Same**](https://example.com) extra\n",
    ));
    assert!(c
        .heading_offset("Same extra")
        .unwrap_err()
        .to_ascii_lowercase()
        .contains("multiple"));
    for raw in [
        "```\n# Hidden\n```\n",
        "> # Hidden\n",
        "Hidden\n======\n",
        "---\nheading: Hidden\n---\n",
        "    # Hidden\n",
    ] {
        assert!(classify(&snapshot(raw)).heading_offset("Hidden").is_err());
    }
    let raw = format!("# Huge\n{}", "x".repeat(MAX_BYTES));
    assert!(classify(&snapshot(&raw))
        .heading_offset("Huge")
        .unwrap_err()
        .contains("64 KiB"));
}

#[test]
fn wikilink_whitespace_preserves_source_and_reveals_exact_syntax() {
    for (raw, expected, target) in [
        ("[[Target|alias ]]", "alias", "Target"),
        ("[[Target| alias ]]", "alias", "Target"),
        ("[[ Target#Heading | alias ]]", "alias", " Target#Heading "),
        ("[[Target#^block| שם ]]", "שם", "Target#^block"),
        ("[[ Target ]]", "Target", " Target "),
        ("[[Target|\talias\t]]", "alias", "Target"),
    ] {
        let snap = snapshot(raw);
        let classified = classify(&snap);
        assert_eq!(display(raw), expected, "{raw:?}");
        let links = classified.links_for(&snap).unwrap();
        assert_eq!(links.len(), 1, "{raw:?}");
        assert_eq!(&raw[links[0].label.clone()], expected);
        assert_eq!(links[0].target, target);
        assert_eq!(snap.copy_source(0..raw.len()).unwrap(), raw);
        let active = Active {
            selection: Some(links[0].label.start..links[0].label.start),
            composition: None,
        };
        assert_eq!(
            project(&snap, classified.plan(), &active)
                .unwrap()
                .display(),
            raw
        );
    }
}
