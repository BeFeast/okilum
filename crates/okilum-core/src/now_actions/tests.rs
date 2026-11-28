use super::*;
use chrono::TimeZone;

const CASE: &str = "\u{feff}---\r\n# keep\r\ntype: case\r\ncase_status: 'needs-decision'\r\nlabels: [lab]\r\n---\r\n# Case\r\n\r\n## История\r\n\r\n- Earlier\r\n## Next\nKeep me";
const INTENT: &str = "# Lab\r\n\r\n## Намерения\r\n\r\n- Earlier\r\n\r\n## Next\r\nKeep me";
const CASE_PATH: &str = "state/lab/case.md";
const AREA_PATH: &str = "state/lab/@index.md";

fn clock() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 27, 23, 4, 0).unwrap()
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    fs::create_dir_all(root.join("state/lab")).unwrap();
    fs::create_dir(root.join("@Schema")).unwrap();
    fs::write(root.join(CASE_PATH), CASE).unwrap();
    fs::write(root.join(AREA_PATH), INTENT).unwrap();
    let drafts = temp.path().join("editor-drafts");
    (temp, root, drafts)
}

#[test]
fn case_updates_plain_scalars_and_prepends_history_preserving_other_bytes() {
    let before = CASE;
    let after = Action::DecideCase
        .plan(before, "Proceed", "Coordinator", clock())
        .unwrap();
    assert_eq!(
        after,
        before
            .replace("'needs-decision'", "'active'")
            .replace("---\r\n# Case", "updated: 2026-11-28\r\n---\r\n# Case")
            .replace(
                "- Earlier",
                "- 28.11 01:04 — Coordinator: Proceed\r\n- Earlier"
            )
    );
}

#[test]
fn intention_inserts_before_next_peer_or_parent_and_handles_eof() {
    let after = Action::AddIntention
        .plan(INTENT, "Review drawings", "", clock())
        .unwrap();
    assert_eq!(
        after,
        INTENT.replace(
            "\r\n\r\n## Next",
            "\r\n- Review drawings (2026-11-28)\r\n\r\n## Next"
        )
    );
    for (before, after) in [
        ("## Intentions", "## Intentions\n\n- New (2026-11-28)"),
        (
            "## Intentions\n\n",
            "## Intentions\n\n- New (2026-11-28)\n\n",
        ),
        (
            "## Intentions\r\n\r\nSome prose",
            "## Intentions\r\n\r\nSome prose\r\n\r\n- New (2026-11-28)",
        ),
        (
            "## Intentions\n### Later\n\n# Next\n",
            "## Intentions\n### Later\n\n- New (2026-11-28)\n\n# Next\n",
        ),
        (
            "## Intentions\n\nNext\n====\n",
            "## Intentions\n\n- New (2026-11-28)\n\nNext\n====\n",
        ),
    ] {
        assert_eq!(
            Action::AddIntention
                .plan(before, "New", "", clock())
                .unwrap(),
            after
        );
    }
}

#[test]
fn ignores_fake_headings_and_refuses_missing_ambiguous_or_unsafe_section_endings() {
    let decoys = [
        "~~~md\n## Intentions\n~~~\n",
        "<!--\n\n## Intentions\n-->\n",
        "Intro <!--\n\n## Intentions\n-->\n",
        "%%\n\n## Intentions\n%%\n",
    ];
    for decoy in decoys {
        assert!(
            Action::AddIntention
                .plan(decoy, "New", "", clock())
                .is_err(),
            "{decoy}"
        );
        let source = format!("{decoy}\n## Intentions\n\n- Earlier\n");
        assert!(
            Action::AddIntention
                .plan(&source, "New", "", clock())
                .is_ok(),
            "{source}"
        );
    }
    for source in [
        "## Intentions\n\n## Намерения\n",
        "## Intentions\n\n   ## *Intentions*\n",
        "## Intentions\n\n> ## Intentions\n",
        "## Intentions\n\n```md\nUnclosed\n",
        "## Intentions\n\n<!--\nUnclosed\n",
    ] {
        assert!(
            Action::AddIntention
                .plan(source, "New", "", clock())
                .is_err(),
            "{source}"
        );
    }
    for decoy in [
        "```\n## History\n```\n",
        "<!--\n## История\n-->\n",
        "%%\n## История\n%%\n",
    ] {
        let source = format!("---\ntype: case\n---\n\n{decoy}");
        assert!(
            Action::DecideCase
                .plan(&source, "New", "owner", clock())
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn refuses_editing_malformed_or_non_case_frontmatter() {
    for yaml in [
        "type: issue",
        "type: case\ntype: case",
        "type: case\ncase_status: old\n'case_status': older",
        "{type: case}",
        "type: case\ncase_status: {state: pending}",
        "type: case\nmeta: {title: Other}",
        "type: case\nmeta: [ {title: Other} ]",
        "type: case\nmeta: [title: Other]",
        "type: &anchor case\nother: *anchor",
        "type: case\nmeta: !tag x",
        "type: 'case",
        "type: case\ncase_status: >\n  old",
    ] {
        let source = format!("---\n{yaml}\n---\n\n## History\n");
        assert!(
            Action::DecideCase
                .plan(&source, "New", "owner", clock())
                .is_err(),
            "{yaml}"
        );
    }
}

#[test]
fn writer_uses_people_owner_and_undo_restores_exact_source() {
    let (_temp, root, drafts) = fixture();
    for (people, owner) in [
        (
            Some("roles:\n  owner:\n    display_name: Coordinator\n"),
            "Coordinator",
        ),
        (Some("roles:\n  owner:\n    display_name: ' '\n"), "owner"),
        (None, "owner"),
    ] {
        let people_path = root.join(now_feed::PEOPLE_PATH);
        if let Some(people) = people {
            fs::write(&people_path, people).unwrap();
        } else {
            fs::remove_file(&people_path).unwrap();
        }
        let receipt = apply(
            &root,
            &drafts,
            &Target::capture(CASE_PATH, CASE),
            Action::DecideCase,
            "Proceed",
            clock(),
        )
        .unwrap();
        assert!(fs::read_to_string(root.join(CASE_PATH))
            .unwrap()
            .contains(&format!("28.11 01:04 — {owner}: Proceed")));
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(fs::read(root.join(CASE_PATH)).unwrap(), CASE.as_bytes());
        assert!(receipt.undo(&root, &drafts).is_err());
    }
    let receipt = apply(
        &root,
        &drafts,
        &Target::capture(AREA_PATH, INTENT),
        Action::AddIntention,
        "Draw",
        clock(),
    )
    .unwrap();
    assert!(fs::read_to_string(root.join(AREA_PATH))
        .unwrap()
        .contains("- Draw (2026-11-28)"));
    receipt.undo(&root, &drafts).unwrap();
    assert_eq!(fs::read(root.join(AREA_PATH)).unwrap(), INTENT.as_bytes());
}

#[test]
fn refuses_stale_lone_cr_and_invalid_lines_without_creating_a_draft() {
    let (_temp, root, drafts) = fixture();
    let path = root.join(AREA_PATH);
    let target = Target::capture(AREA_PATH, INTENT);
    let attempt = |target, text| apply(&root, &drafts, target, Action::AddIntention, text, clock());
    for text in [
        "",
        "   ",
        "first\nsecond",
        "first\tsecond",
        "\r",
        "first\u{2028}second",
    ] {
        assert!(attempt(&target, text).is_err(), "{text:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), INTENT);
    }
    let external = format!("{INTENT}\rExternal");
    fs::write(&path, &external).unwrap();
    assert!(attempt(&target, "New")
        .unwrap_err()
        .to_string()
        .contains("changed"));
    let fresh = Target::capture(AREA_PATH, &external);
    assert!(attempt(&fresh, "New")
        .unwrap_err()
        .to_string()
        .contains("carriage"));
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    assert!(!FileEditor::has_unsaved_draft(&path, &drafts).unwrap());
}

#[test]
fn refuses_archive_nested_vault_and_traversal_for_both_actions() {
    let (_temp, root, drafts) = fixture();
    for (path, original, action) in [
        (CASE_PATH, CASE, Action::DecideCase),
        (AREA_PATH, INTENT, Action::AddIntention),
    ] {
        let attempt = |path| {
            apply(
                &root,
                &drafts,
                &Target::capture(path, original),
                action,
                "New",
                clock(),
            )
        };
        fs::write(root.join(".brain-read-only"), "").unwrap();
        assert!(attempt(path)
            .unwrap_err()
            .to_string()
            .contains("read-only archive"));
        fs::remove_file(root.join(".brain-read-only")).unwrap();
        for marker in [".okilum/vault.json", ".obsidian"] {
            let path_to_marker = root.join("state/lab").join(marker);
            fs::create_dir_all(path_to_marker.parent().unwrap()).unwrap();
            fs::write(&path_to_marker, "{}").unwrap();
            assert!(attempt(path)
                .unwrap_err()
                .to_string()
                .contains("nested vault boundary"));
            fs::remove_file(path_to_marker).unwrap();
        }
        assert!(attempt("state/lab/../lab/@index.md").is_err());
        assert!(attempt("../elsewhere.md").is_err());
        assert_eq!(fs::read_to_string(root.join(path)).unwrap(), original);
    }
}

#[test]
fn refuses_active_editor_dirty_recovery_and_undo_after_an_external_write() {
    let (_temp, root, drafts) = fixture();
    let path = root.join(AREA_PATH);
    let target = Target::capture(AREA_PATH, INTENT);
    let attempt = || {
        apply(
            &root,
            &drafts,
            &target,
            Action::AddIntention,
            "New",
            clock(),
        )
    };
    let mut editor = FileEditor::open(&path, &drafts).unwrap();
    assert!(attempt().is_err());
    editor.set_text("Unsaved".into()).unwrap();
    drop(editor);
    assert!(attempt().unwrap_err().to_string().contains("unsaved"));
    assert_eq!(fs::read_to_string(&path).unwrap(), INTENT);
    let mut editor = FileEditor::open(&path, &drafts).unwrap();
    editor.reload().unwrap();
    drop(editor);
    let receipt = attempt().unwrap();
    fs::write(&path, "External").unwrap();
    assert!(receipt.undo(&root, &drafts).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "External");
}

#[cfg(unix)]
#[test]
fn refuses_symlink_leaf_and_parent_for_apply_and_undo() {
    use std::os::unix::fs::symlink;
    let (temp, root, drafts) = fixture();
    let attempt = |path| {
        apply(
            &root,
            &drafts,
            &Target::capture(path, CASE),
            Action::DecideCase,
            "New",
            clock(),
        )
    };
    let receipt = attempt(CASE_PATH).unwrap();
    let root_link = temp.path().join("vault-link");
    symlink(&root, &root_link).unwrap();
    assert!(receipt.undo(&root_link, &drafts).is_err());
    let file = root.join(CASE_PATH);
    let outside = temp.path().join("outside.md");
    fs::rename(&file, &outside).unwrap();
    symlink(&outside, &file).unwrap();
    assert!(attempt(CASE_PATH).is_err());
    assert!(receipt.undo(&root, &drafts).is_err());
    symlink(temp.path(), root.join("alias")).unwrap();
    assert!(attempt("alias/outside.md").is_err());
    assert!(fs::read_to_string(outside).unwrap().contains("owner: New"));
}

#[test]
fn discovery_captures_only_eligible_case_targets_and_immediate_area_overviews() {
    let (_temp, root, _) = fixture();
    fs::write(root.join("state/lab/act.md"), CASE).unwrap();
    fs::write(root.join("state/lab/legacy.md"), CASE).unwrap();
    fs::create_dir_all(root.join("state/lab/project")).unwrap();
    fs::write(root.join("state/lab/project/@index.md"), INTENT).unwrap();
    let item = |path: &str, response: Option<&str>| {
        serde_json::json!({
            "id": path, "group": "answer", "kind": "case", "title": "Decide",
            "area": "lab", "source_path": CASE_PATH, "overdue": false,
            "response": response, "answer_target": {"kind": "case", "path": path}
        })
    };
    let feed: now_feed::FeedGroup = serde_json::from_value(serde_json::json!({
        "id": "answer", "items": [
            item(CASE_PATH, Some("answer")),
            item("state/lab/act.md", Some("act")),
            item("state/lab/legacy.md", None),
            item("../outside.md", Some("answer"))
        ]
    }))
    .unwrap();
    let loaded = now_feed::LoadOutcome::Ready(now_feed::NowFeed {
        groups: vec![feed],
        version: 2,
        generated_at: String::new(),
        warnings: Vec::new(),
    });
    let targets = load_targets(&root, &loaded);
    assert_eq!(
        targets.cases.keys().map(String::as_str).collect::<Vec<_>>(),
        [CASE_PATH, "state/lab/legacy.md"]
    );
    assert_eq!(
        targets.areas.keys().map(String::as_str).collect::<Vec<_>>(),
        ["lab"]
    );
    assert_eq!(targets.areas["lab"].path(), AREA_PATH);
}

#[test]
fn declared_nested_boundary_blocks_undo_after_successful_write() {
    let (_temp, root, drafts) = fixture();
    let target = Target::capture(AREA_PATH, INTENT);
    let receipt = apply(
        &root,
        &drafts,
        &target,
        Action::AddIntention,
        "New",
        clock(),
    )
    .unwrap();
    fs::create_dir(root.join(".okilum")).unwrap();
    fs::write(
        vault_descriptor::path(&root),
        serde_json::json!({
            "layout": {
                "schema_version": 1,
                "managed_by": "gardener",
                "contract": "@Schema/layout.yaml",
                "roots": {"inbox":"inbox", "sources":"sources", "knowledge":"knowledge",
                          "state":"state", "log":"log", "views":"@views"},
                "domains": [],
                "generated_at": "2026-11-27T00:00:00Z",
                "nested_roots": [
                    {"path":"state/lab", "kind":"unit", "files":2, "tree_sha256":"0".repeat(64)}
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    assert!(receipt
        .undo(&root, &drafts)
        .unwrap_err()
        .to_string()
        .contains("nested vault boundary"));
    assert!(fs::read_to_string(root.join(AREA_PATH))
        .unwrap()
        .contains("New (2026-11-28)"));
}
