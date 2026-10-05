//! Link resolution: identity is the path from the vault root, and a link is a
//! suffix of one. These tests exist because the previous index was keyed on the
//! bare stem, which produced two failures that looked like features — a
//! colliding stem silently kept the shortest path, and a path-qualified link was
//! reduced to its last segment before lookup.

use std::fs;
use std::path::{Path, PathBuf};

use tessera_core::{Resolution, Vault};

/// Build a throwaway vault from `(relative path, contents)` pairs.
fn vault_with(name: &str, files: &[(&str, &str)]) -> (PathBuf, Vault) {
    let root = std::env::temp_dir().join(format!("tessera-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for (rel, body) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }
    let vault = Vault::scan(Path::new(&root)).unwrap();
    (root, vault)
}

fn resolved(r: &Resolution) -> &str {
    match r {
        Resolution::Resolved { path } => path,
        other => panic!("expected Resolved, got {other:?}"),
    }
}

#[test]
fn a_unique_stem_resolves() {
    let (_root, v) = vault_with("unique", &[("notes/alpha.md", "# Alpha\n")]);
    assert_eq!(resolved(&v.resolve("alpha")), "notes/alpha.md");
    assert_eq!(
        resolved(&v.resolve("Alpha")),
        "notes/alpha.md",
        "case-insensitive"
    );
    assert_eq!(
        resolved(&v.resolve("alpha.md")),
        "notes/alpha.md",
        "extension optional"
    );
    assert_eq!(
        resolved(&v.resolve("notes/alpha")),
        "notes/alpha.md",
        "qualified"
    );
}

#[test]
fn a_colliding_stem_is_ambiguous_not_a_silent_winner() {
    let (_root, v) = vault_with(
        "collide",
        &[("dup/alpha.md", "# One\n"), ("notes/alpha.md", "# Two\n")],
    );
    // The old index kept "shortest path wins", which answered dup/alpha.md and
    // said nothing about the other candidate.
    match v.resolve("alpha") {
        Resolution::Ambiguous { candidates } => {
            assert_eq!(candidates, vec!["dup/alpha.md", "notes/alpha.md"]);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    // Qualifying the link disambiguates it, which is the whole point of having
    // an ambiguous state to escape from.
    assert_eq!(resolved(&v.resolve("notes/alpha")), "notes/alpha.md");
    assert_eq!(resolved(&v.resolve("dup/alpha")), "dup/alpha.md");
}

#[test]
fn a_qualified_link_never_falls_back_to_a_bare_stem() {
    // The regression this is named for: resolve() used to take the last segment
    // of a qualified target and look that up, so `[[c/note]]` could resolve to a
    // note with no `c` anywhere in its path.
    let (_root, v) = vault_with("qualified", &[("a/b/note.md", "# Note\n")]);
    assert_eq!(resolved(&v.resolve("note")), "a/b/note.md");
    assert_eq!(resolved(&v.resolve("b/note")), "a/b/note.md");
    assert_eq!(resolved(&v.resolve("a/b/note")), "a/b/note.md");
    assert_eq!(
        v.resolve("c/note"),
        Resolution::Unresolved,
        "no note lives under c/, so this names nothing"
    );
}

#[test]
fn an_exact_full_path_beats_a_suffix_tie() {
    // Paths are unique, so a full path from the root is a precise identifier
    // even when another note ends with the same segments.
    let (_root, v) = vault_with(
        "exact",
        &[("alpha.md", "# Root\n"), ("notes/alpha.md", "# Nested\n")],
    );
    assert_eq!(resolved(&v.resolve("alpha")), "alpha.md");
    assert_eq!(resolved(&v.resolve("notes/alpha")), "notes/alpha.md");
}

#[test]
fn a_missing_target_is_unresolved() {
    let (_root, v) = vault_with("missing", &[("alpha.md", "# Alpha\n")]);
    assert_eq!(v.resolve("nope"), Resolution::Unresolved);
    assert_eq!(v.resolve(""), Resolution::Unresolved);
    assert_eq!(v.resolve("   "), Resolution::Unresolved);
    assert_eq!(v.resolve("#heading-only"), Resolution::Unresolved);
}

#[test]
fn headings_and_blocks_are_stripped_before_resolving() {
    let (_root, v) = vault_with("anchors", &[("notes/alpha.md", "# Alpha\n")]);
    assert_eq!(resolved(&v.resolve("alpha#Some Heading")), "notes/alpha.md");
    assert_eq!(resolved(&v.resolve("alpha^block-id")), "notes/alpha.md");
}

#[test]
fn candidate_order_is_stable() {
    let (_root, v) = vault_with(
        "stable",
        &[
            ("z/alpha.md", "# Z\n"),
            ("a/alpha.md", "# A\n"),
            ("m/alpha.md", "# M\n"),
        ],
    );
    for _ in 0..5 {
        match v.resolve("alpha") {
            Resolution::Ambiguous { candidates } => {
                assert_eq!(candidates, vec!["a/alpha.md", "m/alpha.md", "z/alpha.md"]);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }
}

#[test]
fn an_ambiguous_link_gives_every_candidate_a_marked_backlink() {
    // Neither dropping the link nor awarding it to one candidate is honest.
    let (_root, v) = vault_with(
        "backlinks-ambiguous",
        &[
            ("index.md", "Points at [[alpha]].\n"),
            ("dup/alpha.md", "# One\n"),
            ("notes/alpha.md", "# Two\n"),
        ],
    );
    for target in ["dup/alpha.md", "notes/alpha.md"] {
        let bl = v.backlinks(target);
        assert_eq!(bl.len(), 1, "{target} should have the backlink");
        assert_eq!(bl[0].path, "index.md");
        assert!(
            bl[0].ambiguous,
            "{target} backlink must be marked ambiguous"
        );
    }
}

#[test]
fn an_unambiguous_backlink_is_not_marked() {
    let (_root, v) = vault_with(
        "backlinks-clean",
        &[
            ("index.md", "Points at [[alpha]].\n"),
            ("notes/alpha.md", "# Alpha\n"),
        ],
    );
    let bl = v.backlinks("notes/alpha.md");
    assert_eq!(bl.len(), 1);
    assert!(!bl[0].ambiguous);
}

#[test]
fn a_qualified_link_gives_exactly_one_backlink() {
    // The "duplicate stems steal backlinks" defect in its original shape: the
    // qualified link belongs to notes/alpha.md alone.
    let (_root, v) = vault_with(
        "backlinks-qualified",
        &[
            ("index.md", "Points at [[notes/alpha]].\n"),
            ("dup/alpha.md", "# One\n"),
            ("notes/alpha.md", "# Two\n"),
        ],
    );
    assert_eq!(v.backlinks("notes/alpha.md").len(), 1);
    assert!(v.backlinks("dup/alpha.md").is_empty());
}

#[test]
fn a_precise_link_on_the_same_line_clears_the_ambiguity_warning() {
    // Backlinks are deduplicated per (source note, line). When one line carries
    // both an ambiguous and a precise link to the same note, the note really is
    // linked precisely, and warning about it would be a false alarm.
    let (_root, v) = vault_with(
        "backlinks-mixed",
        &[
            ("index.md", "Links: [[alpha]] and [[notes/alpha]].\n"),
            ("dup/alpha.md", "# One\n"),
            ("notes/alpha.md", "# Two\n"),
        ],
    );

    let precise = v.backlinks("notes/alpha.md");
    assert_eq!(precise.len(), 1);
    assert!(
        !precise[0].ambiguous,
        "the same line names this note exactly, so it is not a guess"
    );

    // The other candidate is reachable only through the ambiguous link, so its
    // backlink stays marked.
    let other = v.backlinks("dup/alpha.md");
    assert_eq!(other.len(), 1);
    assert!(other[0].ambiguous);
}

#[test]
fn a_relative_link_resolves_against_the_linking_note() {
    // `[[../ops/signoff]]` names a sibling directory of the note that wrote it
    // and says nothing on its own. Resolving it by suffix would answer with
    // whichever unrelated project also has an ops/signoff — and the vault this
    // was built for has four.
    let (_root, v) = vault_with(
        "relative",
        &[
            ("proj-a/planning/plan.md", "See [[../ops/signoff]].\n"),
            ("proj-a/ops/signoff.md", "# A\n"),
            ("proj-b/ops/signoff.md", "# B\n"),
        ],
    );

    assert_eq!(
        resolved(&v.resolve_from("../ops/signoff", "proj-a/planning/plan.md")),
        "proj-a/ops/signoff.md"
    );
    assert_eq!(
        resolved(&v.resolve_from("./signoff", "proj-a/ops/other.md")),
        "proj-a/ops/signoff.md"
    );
    // Without a source note there is nothing to be relative to.
    assert_eq!(v.resolve("../ops/signoff"), Resolution::Unresolved);
    // And the same target written from elsewhere names that other project.
    assert_eq!(
        resolved(&v.resolve_from("../ops/signoff", "proj-b/planning/plan.md")),
        "proj-b/ops/signoff.md"
    );
    // The backlink follows the same rule rather than landing on both.
    assert_eq!(v.backlinks("proj-a/ops/signoff.md").len(), 1);
    assert!(v.backlinks("proj-b/ops/signoff.md").is_empty());
}

#[test]
fn a_relative_link_climbing_past_the_root_resolves_to_nothing() {
    let (_root, v) = vault_with("escape", &[("a/note.md", "# N\n"), ("target.md", "# T\n")]);
    assert_eq!(
        v.resolve_from("../../../../target", "a/note.md"),
        Resolution::Unresolved
    );
    // One level up from a/note.md is the root, which is legitimate.
    assert_eq!(
        resolved(&v.resolve_from("../target", "a/note.md")),
        "target.md"
    );
}

#[test]
fn a_relative_link_to_a_missing_note_is_unresolved_not_a_lookalike() {
    // The old resolver reduced this to its last segment and answered with an
    // unrelated note that happened to share it.
    let (_root, v) = vault_with(
        "relative-missing",
        &[
            ("proj-a/planning/plan.md", "# P\n"),
            ("elsewhere/deep/signoff.md", "# Not this one\n"),
        ],
    );
    assert_eq!(
        v.resolve_from("../ops/signoff", "proj-a/planning/plan.md"),
        Resolution::Unresolved
    );
}

#[test]
fn a_link_that_only_appears_inside_code_is_not_a_backlink() {
    // #20. A `[[x]]` in a fence, an inline span or an indented block is an
    // example of a link, so it must not show up in the graph. The bare one on
    // the last line is the only real link.
    let (_root, v) = vault_with(
        "backlinks-code",
        &[
            (
                "fenced.md",
                "```\n[[alpha]]\n```\n\nInline `[[alpha]]` only.\n\n    [[alpha]]\n",
            ),
            ("real.md", "`[[beta]]` then [[alpha]].\n"),
            ("notes/alpha.md", "# Alpha\n"),
            ("notes/beta.md", "# Beta\n"),
        ],
    );
    assert!(
        v.backlinks("notes/beta.md").is_empty(),
        "beta is only ever named inside code"
    );
    let bl = v.backlinks("notes/alpha.md");
    assert_eq!(bl.len(), 1, "{bl:?}");
    assert_eq!(bl[0].path, "real.md");
    // Context is the whole source line, code span included.
    assert_eq!(bl[0].context, "`[[beta]]` then [[alpha]].");
    assert!(v.outbound_links("fenced.md").is_empty());
    assert_eq!(v.outbound_links("real.md"), vec!["alpha"]);
}

#[test]
fn backlinks_are_uncapped_and_grouped_by_source_note() {
    // 60 links from one hub plus one from each of two other notes. The old
    // 50-per-target cap would report 50 and drop the rest silently (#25).
    let hub: String = (0..60)
        .map(|i| format!("- line {i} [[target]]\n"))
        .collect();
    let (_root, v) = vault_with(
        "uncapped",
        &[
            ("target.md", "# T\n"),
            ("z/hub.md", &hub),
            ("a/first.md", "see [[target]]\n"),
            ("m/mid.md", "> **also** [[target|T]]\n"),
        ],
    );
    let bl = v.backlinks("target.md");
    assert_eq!(bl.len(), 62);
    let paths: Vec<&str> = bl.iter().map(|b| b.path.as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "grouped by source path");
    let hub_lines: Vec<&str> = bl
        .iter()
        .filter(|b| b.path == "z/hub.md")
        .map(|b| b.context.as_str())
        .collect();
    assert_eq!(hub_lines[0], "- line 0 [[target]]");
    assert_eq!(
        hub_lines[59], "- line 59 [[target]]",
        "source-line order inside a note"
    );
    let mid = bl.iter().find(|b| b.path == "m/mid.md").unwrap();
    assert_eq!(mid.context_plain(), "also T");
}
