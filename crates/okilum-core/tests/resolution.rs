//! Link resolution: identity is the path from the vault root, and a link is a
//! suffix of one. These tests exist because the previous index was keyed on the
//! bare stem, which produced two failures that looked like features — a
//! colliding stem silently kept the shortest path, and a path-qualified link was
//! reduced to its last segment before lookup.

use std::fs;
use std::path::{Path, PathBuf};

use okilum_core::{Resolution, Vault};

/// Build a throwaway vault from `(relative path, contents)` pairs.
fn vault_with(name: &str, files: &[(&str, &str)]) -> (PathBuf, Vault) {
    let root = std::env::temp_dir().join(format!("okilum-test-{name}-{}", std::process::id()));
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

// #2905: data files are graph destinations, never note sources.
#[test]
fn data_file_wikilink_has_exactly_one_backlink() {
    let (_root, vault) = vault_with(
        "data-file-wiki-2905",
        &[("note.md", "[[data.json]]\n"), ("data.json", "{}")],
    );
    let links = vault.backlinks("data.json");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].path, "note.md");
    assert!(!links[0].ambiguous);
    assert_eq!(vault.notes.len(), 1);
}

#[test]
fn data_file_markdown_links_keep_relative_root_and_suffix_intent() {
    let (_root, vault) = vault_with(
        "data-file-markdown-2905",
        &[
            ("notes/start.md", "[local](data.json)\n[root](/data.json)\n[explicit](../a/shared.json)\n[ambiguous](shared.json)\n[space](../a/my%20data.json)\n[missing](missing.json)\n"),
            ("data.json", "{}"),
            ("notes/data.json", "{}"),
            ("a/shared.json", "{}"),
            ("b/shared.json", "{}"),
            ("a/my data.json", "{}"),
        ],
    );
    assert_eq!(vault.backlinks("notes/data.json")[0].link, "data.json");
    assert_eq!(vault.backlinks("data.json")[0].link, "/data.json");
    let shared = vault.backlinks("a/shared.json");
    assert_eq!(shared.len(), 2);
    assert!(!shared[0].ambiguous);
    assert!(shared[1].ambiguous);
    assert!(vault.backlinks("b/shared.json")[0].ambiguous);
    assert_eq!(vault.backlinks("a/my data.json").len(), 1);
    assert!(vault.backlinks("missing.json").is_empty());
}

#[test]
fn data_file_wikilinks_keep_relative_qualified_and_ambiguous_intent() {
    let (_root, vault) = vault_with(
        "data-file-resolution-2905",
        &[
            ("notes/start.md", "[[data.json]]\n[[../a/shared.json]]\n[[shared.json]]\n[[a/shared.json]]\n[[missing.json]]\n"),
            ("data.json", "{}"),
            ("a/shared.json", "{}"),
            ("b/shared.json", "{}"),
        ],
    );
    assert_eq!(vault.backlinks("data.json").len(), 1);
    assert_eq!(vault.backlinks("a/shared.json").len(), 3);
    assert!(vault.backlinks("b/shared.json")[0].ambiguous);
    assert!(vault.backlinks("missing.json").is_empty());
}

/// What the Reader opens for `target` from `from`: the graph must agree.
fn opens(vault: &Vault, target: &str, wiki: bool, from: &str) -> Vec<String> {
    okilum_core::document_links::IndexedResolver::new(vault)
        .resolve(target, wiki, from)
        .candidates
}

fn sources(vault: &Vault, target: &str) -> Vec<(String, String)> {
    vault
        .backlinks(target)
        .iter()
        .map(|b| (b.path.clone(), b.link.clone()))
        .collect()
}

// A data file and a note that differ only by `.md` are two identities.
#[test]
fn data_file_and_its_md_namesake_keep_separate_backlinks() {
    let (_root, vault) = vault_with(
        "data-file-md-namesake-2905",
        &[
            (
                "a.md",
                "[[data.json]]\n[[data.json.md]]\n[f](data.json)\n[n](data.json.md)\n",
            ),
            ("b.md", "[[data.json.md]]\n"),
            ("data.json", "{}"),
            ("data.json.md", "# Data\n"),
        ],
    );
    assert_eq!(opens(&vault, "data.json", true, "a.md"), ["data.json"]);
    assert_eq!(
        opens(&vault, "data.json.md", true, "a.md"),
        ["data.json.md"]
    );
    assert_eq!(opens(&vault, "data.json", false, "a.md"), ["data.json"]);
    assert_eq!(
        sources(&vault, "data.json"),
        [
            ("a.md".into(), "data.json".into()),
            ("a.md".into(), "data.json".into())
        ]
    );
    assert_eq!(
        sources(&vault, "data.json.md"),
        [
            ("a.md".into(), "data.json.md".into()),
            ("a.md".into(), "data.json.md".into()),
            ("b.md".into(), "data.json.md".into()),
        ]
    );
    assert!(vault.backlinks("data.json").iter().all(|b| !b.ambiguous));
    assert!(vault.backlinks("data.json.md").iter().all(|b| !b.ambiguous));
}

// `[[data.json]]` names a file, so a nested `sub/data.json` answers it,
// never the note `data.json.md`; the Reader opens the same file.
#[test]
fn data_file_link_reaches_the_nested_file_not_the_md_namesake() {
    let (_root, vault) = vault_with(
        "data-file-nested-namesake-2905",
        &[
            ("note.md", "[[data.json]]\n"),
            ("sub/data.json", "{}"),
            ("data.json.md", "# Data\n"),
        ],
    );
    assert_eq!(
        opens(&vault, "data.json", true, "note.md"),
        ["sub/data.json"]
    );
    assert_eq!(
        sources(&vault, "sub/data.json"),
        [("note.md".into(), "data.json".into())]
    );
    assert!(vault.backlinks("data.json.md").is_empty());
}

// PDFs and images are data files too: wiki and Markdown links reach them
// whatever the link's letter case, and a missing file gets no backlink.
#[test]
fn pdf_and_image_links_resolve_case_insensitively_and_missing_files_stay_empty() {
    let (_root, vault) = vault_with(
        "data-file-pdf-image-case-2905",
        &[
            (
                "note.md",
                "[[report.pdf]]\n\
                 [[Report.pdf#page=3]]\n\
                 [r](report.pdf)\n\
                 [p](REPORT.PDF#page=2)\n\
                 [[photo.png|Photo]]\n\
                 ![[PHOTO.png]]\n\
                 [shot](img/photo.png)\n\
                 [[Data.JSON]]\n\
                 [[report.png]]\n\
                 [[missing.pdf]]\n\
                 [gone](gone.png)\n\
                 ![[absent.jpg]]\n",
            ),
            ("Report.PDF", "%PDF-1.4\n"),
            ("img/Photo.PNG", "png"),
            ("data.json", "{}"),
        ],
    );

    let report = sources(&vault, "Report.PDF");
    assert_eq!(report.len(), 4, "{report:?}");
    assert!(report.iter().all(|(path, _)| path == "note.md"));
    assert_eq!(report[0].1, "report.pdf");
    assert_eq!(report[1].1, "Report.pdf");
    assert!(vault.backlinks("Report.PDF").iter().all(|b| !b.ambiguous));

    let photo = sources(&vault, "img/Photo.PNG");
    assert_eq!(photo.len(), 3, "{photo:?}");
    assert_eq!(photo[0].1, "photo.png");
    assert_eq!(photo[1].1, "PHOTO.png");
    assert!(vault
        .backlinks("img/Photo.PNG")
        .iter()
        .all(|b| !b.ambiguous));

    assert_eq!(
        sources(&vault, "data.json"),
        [("note.md".into(), "Data.JSON".into())]
    );

    // The Reader opens the same files the graph names.
    assert_eq!(opens(&vault, "report.pdf", true, "note.md"), ["Report.PDF"]);
    assert_eq!(
        opens(&vault, "REPORT.PDF", false, "note.md"),
        ["Report.PDF"]
    );
    assert_eq!(
        opens(&vault, "PHOTO.png", true, "note.md"),
        ["img/Photo.PNG"]
    );
    assert_eq!(
        opens(&vault, "img/photo.png", false, "note.md"),
        ["img/Photo.PNG"]
    );

    // A different extension is a different file; missing targets resolve nowhere.
    for missing in ["report.png", "missing.pdf", "gone.png", "absent.jpg"] {
        assert!(vault.backlinks(missing).is_empty(), "{missing}");
        assert!(
            opens(&vault, missing, true, "note.md").is_empty(),
            "{missing}"
        );
    }
    assert_eq!(vault.notes.len(), 1);
}

#[test]
fn data_file_identity_never_shadows_notes() {
    let (_root, vault) = vault_with(
        "data-file-shadow-2905",
        &[
            ("note.md", "[[Makefile]]\n"),
            ("Makefile.md", "# Make\n"),
            ("Makefile", "all:\n"),
        ],
    );
    assert_eq!(vault.resolve("Makefile").path(), Some("Makefile.md"));
    assert_eq!(vault.backlinks("Makefile.md").len(), 1);
    assert!(vault.backlinks("Makefile").is_empty());
}
