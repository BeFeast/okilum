use super::*;
use crate::source_projection::{project, Bias};

fn complete(session: &mut Session, snapshot: Snapshot) -> Classification {
    let mut progress = session.begin(snapshot).unwrap();
    progress.advance(session, 0..0, usize::MAX);
    assert!(progress.is_complete());
    progress.classification()
}

#[test]
fn structural_pass_leaves_inline_constructs_unparsed() {
    let root = markdown::to_mdast(
        "# **heading**\n\n*em* `code` [link](dest) ![image](img) <b> &amp;\n",
        &structural_options(),
    )
    .unwrap();
    let mut nodes = vec![&root];
    while let Some(node) = nodes.pop() {
        assert!(!matches!(
            node,
            Node::Emphasis(_)
                | Node::Strong(_)
                | Node::InlineCode(_)
                | Node::Link(_)
                | Node::Image(_)
                | Node::Html(_)
        ));
        if let Some(children) = node.children() {
            nodes.extend(children);
        }
    }
    assert!(root.to_string().contains("**heading**"));
}

#[test]
fn viewport_is_visited_before_a_large_offscreen_prefix() {
    let source = "# one\n\n**two**\n\n# three\n";
    let snapshot = Snapshot::new("note.md", 1, source);
    let mut session = Session::default();
    let mut progress = session.begin(snapshot.clone()).unwrap();
    let at = source.find("# three").unwrap();
    assert_eq!(progress.advance(&mut session, at..source.len(), 1), 1);
    let partial = progress.classification();
    assert!(partial.reasons().contains(&SourceReason::PendingBlocks));
    assert!(!progress.is_complete());
    let display = project(&snapshot, partial.plan(), &Active::default()).unwrap();
    assert_eq!(display.display(), "# one\n\n**two**\n\nthree\n");
    assert_eq!(session.stats().misses, 1);
    progress.advance(&mut session, 0..0, usize::MAX);
    let full = progress.classification();
    assert_eq!(
        project(&snapshot, full.plan(), &Active::default())
            .unwrap()
            .display(),
        "one\n\ntwo\n\nthree\n"
    );
}

#[test]
fn differential_whole_parse_preserves_containers_fences_and_unicode_offsets() {
    for source in [
        "\u{feff}# **é**\r\n\r\n> *quote*\r\n> lazy\r\n\r\n- [ ] task\r\n  - **nested**\r\n",
        "# neighbor\n\n```rust\n**raw**\n\n# inside\n```\n\n**outside**\n",
        "# neighbor\n\n> a **b\n> c** d\nlazy *x*\n\n- first\n\n  continuation **ok**\n",
        "# neighbor\n\n<div>\n\n# raw html boundary\n\n**outside**\n",
        "---\na: **raw**\n---\n\n# **heading**\n\n***\n\nnext *line*\n",
        "+++\na: **raw**\n+++\n\n# **heading**\n\n---\n\nnext *line*\n",
        "# neighbor\n\n| a | b |\n|---|---|\n| **x** | y |\n\n![image](local.png)\n",
        "# neighbor\n\nSetext *title*\n===\n\n**e\u{301}** [[目标|שלום]]\n",
        "# neighbor\n\n    **indented**\n\n~~~\nunclosed **fence**\n\n# raw\n",
        "---\nunclosed: **raw**\n\n# raw\n",
    ] {
        let snapshot = Snapshot::new("note.md", 7, source);
        let old = super::super::super::classify(&snapshot);
        let new = complete(&mut Session::default(), snapshot.clone());
        let a = project(&snapshot, old.plan(), &Active::default()).unwrap();
        let b = project(&snapshot, new.plan(), &Active::default()).unwrap();
        assert_eq!(a.display(), b.display(), "{source:?}");
        assert_eq!(
            old.styles_for(&snapshot).unwrap(),
            new.styles_for(&snapshot).unwrap(),
            "{source:?}"
        );
        assert_eq!(
            old.decorations_for(&snapshot).unwrap(),
            new.decorations_for(&snapshot).unwrap(),
            "{source:?}"
        );
        for (at, _) in source.char_indices() {
            if let Ok(offset) = a.source_to_display(&snapshot, at) {
                assert_eq!(b.source_to_display(&snapshot, at).unwrap(), offset);
                assert_eq!(
                    a.display_to_source(&snapshot, offset, Bias::Left),
                    b.display_to_source(&snapshot, offset, Bias::Left)
                );
            }
        }
    }
}

#[test]
fn document_reference_definitions_resolve_across_blocks_and_change_cache_keys() {
    let mut session = Session::default();
    for destination in ["one.md", "two.md"] {
        let source = format!("# neighbor\n\n[label][STRASSE]\n\n[straße]: {destination}\n");
        let snapshot = Snapshot::new("note.md", 1, source);
        let result = complete(&mut session, snapshot.clone());
        assert_eq!(result.links_for(&snapshot).unwrap()[0].target, destination);
        assert_eq!(
            project(&snapshot, result.plan(), &Active::default())
                .unwrap()
                .display(),
            format!("neighbor\n\nlabel\n\n[straße]: {destination}\n")
        );
    }
}

#[test]
fn duplicate_definition_in_container_cannot_override_the_first_document_definition() {
    let source = "[id]: first.md\n\n- [id]: second.md\n\n  [label][id]\n\n[label][id]\n";
    let snapshot = Snapshot::new("note.md", 1, source);
    let result = complete(&mut Session::default(), snapshot.clone());
    assert!(result
        .links_for(&snapshot)
        .unwrap()
        .iter()
        .all(|l| l.target == "first.md"));
    assert!(project(&snapshot, result.plan(), &Active::default())
        .unwrap()
        .display()
        .contains("  [label][id]"));
}

#[test]
fn one_small_edit_reuses_unmodified_blocks_but_large_changes_clear_the_cache() {
    let original = (0..80)
        .map(|n| format!("# heading {n}\n\nparagraph {n} **strong**\n\n"))
        .collect::<String>();
    let mut session = Session::default();
    complete(&mut session, Snapshot::new("note.md", 1, original.clone()));
    let stats = session.stats();
    let edited = original.replacen("paragraph 3 ", "paragraph changed 3 ", 1);
    let mut progress = session.begin(Snapshot::new("note.md", 2, edited)).unwrap();
    assert!(!progress.full_reparse());
    progress.advance(&mut session, 0..0, usize::MAX);
    assert_eq!(session.stats().misses - stats.misses, 1);
    assert_eq!(
        session.stats().hits - stats.hits,
        progress.index().blocks().len() - 1
    );
    let replaced = original.replace("paragraph", "entirely rewritten paragraph");
    let progress = session
        .begin(Snapshot::new("note.md", 3, replaced))
        .unwrap();
    assert!(progress.full_reparse());
    let progress = session
        .begin(Snapshot::new("other.md", 1, original))
        .unwrap();
    assert!(progress.full_reparse());
}

#[test]
fn structural_index_covers_a_mebibyte_without_visiting_offscreen_inlines() {
    let mut source = String::new();
    while source.len() < 1024 * 1024 {
        source.push_str("# heading\n\n**strong** ordinary text with [a](target.md). ");
        source.push_str(&"ordinary text ".repeat(30));
        source.push_str("\n\n");
    }
    let mut session = Session::default();
    let mut progress = session.begin(Snapshot::new("large.md", 1, source)).unwrap();
    assert!(progress.index().snapshot().source().len() >= 1024 * 1024);
    assert_eq!(session.stats().misses, 0);
    assert_eq!(progress.advance(&mut session, 0..100, 4), 4);
    assert_eq!(session.stats().misses + session.stats().hits, 4);
    assert!(!progress.is_complete());
}

#[test]
fn frontmatter_context_is_part_of_the_cache_key() {
    let mut session = Session::default();
    let first = Snapshot::new("note.md", 1, "---\n\n");
    complete(&mut session, first);
    let second = Snapshot::new("note.md", 2, "# heading\n\n---\n\n");
    let result = complete(&mut session, second.clone());
    assert!(result
        .decorations_for(&second)
        .unwrap()
        .iter()
        .any(|m| m.kind == super::super::super::decorations::Kind::ThematicBreak));
}
