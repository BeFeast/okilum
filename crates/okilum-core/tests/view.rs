//! Conformance tests for the shared view_api: 0 fixture vault.
use chrono::{DateTime, FixedOffset};
use okilum_core::{
    records::{FieldSchema, Projection, Source},
    view::{self, RunContext},
    Vault,
};
use serde_json::Value;
use std::{fs, path::Path, sync::Arc};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/views");
const EXAMPLES: [&str; 7] = [
    "agent-context",
    "domain",
    "health",
    "okilum",
    "questions",
    "sidebar",
    "whats-new",
];

fn timestamp(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}

#[test]
fn project_record_override_selects_fields_and_rows_from_the_same_sources() {
    use view::builtin::{project_records, ProjectKind, ProjectRecord};

    let root = tempfile::tempdir().unwrap();
    let newer = "Cedar/ops/decisions/2026-11-24-new.md";
    let older = "Cedar/decisions/2026-11-20-old.md";
    let vault = Vault::from_note_paths([newer.into(), older.into(), "Elsewhere.md".into()]);
    let run = |kind| {
        project_records(
            root.path(),
            &vault,
            kind,
            [
                ProjectRecord {
                    source: Source {
                        path: older,
                        bytes: b"---\nowner: alice\n---\n# Older\n",
                        mtime: None,
                    },
                    title: "Older",
                    date: Some("2026-11-20"),
                    status: None,
                },
                ProjectRecord {
                    source: Source {
                        path: newer,
                        bytes: b"# Newer\n",
                        mtime: None,
                    },
                    title: "Newer",
                    date: Some("2026-11-24"),
                    status: None,
                },
            ],
        )
    };
    let baseline = run(ProjectKind::Decisions).unwrap();
    assert_eq!(baseline.show, view::Show::List);
    assert_eq!(baseline.cols, ["title", "date", "path"]);
    assert_eq!(baseline.rows[0].path, newer);
    assert_eq!(baseline.rows[1].path, older);

    fs::create_dir(root.path().join("@views")).unwrap();
    let path = root.path().join("@views/project-decisions.md");
    let definition = "---\ntype: view\n---\n```view\nfrom: notes\nwhere: owner = alice\nshow: table\ncols: [title, owner]\n```\n";
    fs::write(&path, definition).unwrap();
    let overridden = run(ProjectKind::Decisions).unwrap();
    assert_eq!(overridden.show, view::Show::Table);
    assert_eq!(overridden.cols, ["title", "owner"]);
    assert_eq!(overridden.rows.len(), 1);
    assert_eq!(overridden.rows[0].path, older);
    assert_eq!(overridden.rows[0].fields["owner"], "alice");
    assert_eq!(fs::read_to_string(&path).unwrap(), definition);

    fs::write(&path, "# Invalid override\n").unwrap();
    assert!(run(ProjectKind::Decisions)
        .unwrap_err()
        .contains("@views/project-decisions.md:1: Expected view frontmatter"));
    assert_eq!(run(ProjectKind::Reports).unwrap().show, view::Show::List);
    assert_eq!(run(ProjectKind::Handovers).unwrap().show, view::Show::List);
    fs::remove_file(path).unwrap();
    assert_eq!(run(ProjectKind::Decisions).unwrap().cols, baseline.cols);
}

#[test]
fn all_examples_are_deterministic_and_match_golden_json() {
    let root = Path::new(FIXTURE);
    let schema = Arc::new(FieldSchema::load(root).unwrap());
    let (vault, bytes) = Vault::scan_snapshot_with(root, &mut |_, _| Ok(())).unwrap();
    let mut sources: Vec<_> = bytes
        .iter()
        .map(|(path, bytes)| Source {
            path,
            bytes,
            mtime: Some(timestamp(if path.contains("tasker") {
                "2026-10-25T10:00:00+02:00"
            } else {
                "2026-11-22T10:00:00+02:00"
            })),
        })
        .collect();
    sources.sort_by_key(|source| source.path);
    let projection = Projection::build(schema.clone(), sources.iter().copied());
    let len = sources.len();
    sources.rotate_left(len / 3);
    sources.reverse();
    let shuffled = Projection::build(schema.clone(), sources);
    let mut shuffled_vault = vault.clone();
    shuffled_vault.notes.reverse();
    let text = |path: &str| String::from_utf8(bytes.get(path)?.clone()).ok();
    let as_of = timestamp("2026-11-24T01:30:00+02:00");

    for name in EXAMPLES {
        let path = format!("@views/{name}.md");
        let source = fs::read_to_string(root.join(&path)).unwrap();
        let doc = view::parse(&path, &source).unwrap();
        let findings = view::validate(&doc, &schema);
        assert!(
            findings.iter().all(|finding| !finding.is_error()),
            "{name}: {findings:?}"
        );
        let run = |projection: &Projection, vault: &Vault| {
            let mut ctx = RunContext::new(projection, vault, as_of);
            ctx.read_source = Some(&text);
            if name == "domain" {
                ctx.args.insert("domain".into(), "Health".into());
            }
            view::run(&doc, &ctx)
        };
        let result = run(&projection, &vault);
        let actual = view::render_json(&result).unwrap();
        assert!(actual.starts_with("{\n  \"view_api\": 0,"));
        assert_eq!(
            actual,
            view::render_json(&run(&projection, &vault)).unwrap()
        );
        assert_eq!(
            actual,
            view::render_json(&run(&shuffled, &shuffled_vault)).unwrap()
        );

        let json: Value = serde_json::from_str(&actual).unwrap();
        for section in json["sections"].as_array().unwrap() {
            let expected = if section["id"] == "for-the-task" {
                "degraded"
            } else {
                "ok"
            };
            assert_eq!(section["status"], expected, "{name}: {section}");
        }
        let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/view-golden")
            .join(format!("{name}.json"));
        assert_eq!(actual, fs::read_to_string(expected).unwrap(), "{name}");
        let markdown = view::render_markdown(&result);
        assert!(!markdown.is_empty(), "{name}");
        if name == "sidebar" {
            assert!(doc.is_sidebar_tree());
            let section = &result.sections[0];
            let path = section
                .resolve_node_view(&vault, doc.path())
                .unwrap()
                .unwrap();
            assert_eq!(path, "@views/domain.md");
            let template = view::parse(&path, &text(&path).unwrap()).unwrap();
            let health = section
                .nodes
                .as_ref()
                .unwrap()
                .iter()
                .find(|node| node.label == "Health")
                .unwrap();
            let context = &health.children[0];
            let layer = &context.children[0];
            let mut branch = Vec::new();
            for node in [health, context, layer] {
                branch.push((node.field.clone(), node.value.clone()));
                let args = template.node_args(&branch);
                assert_eq!(args.len(), branch.len());
                assert_eq!(args["domain"], "Health");
                if branch.len() >= 2 {
                    assert_eq!(args["context"], "sleep-tracking");
                }
                if branch.len() == 3 {
                    assert_eq!(args["layer"], "state");
                }
            }
            assert!(markdown.contains("- Health (4)\n  - sleep\\-tracking (2)\n    - state (1)\n      - Sleep tracking — "));
        }
    }
}

#[test]
fn invalid_files_have_positioned_errors() {
    let root = Path::new(FIXTURE);
    let schema = FieldSchema::load(root).unwrap();
    let directory = root.join("@Schema/tests/view-invalid");
    let mut count = 0;
    for entry in fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        let source = fs::read_to_string(&path).unwrap();
        let path = path.strip_prefix(root).unwrap().to_str().unwrap();
        let findings = match view::parse(path, &source) {
            Ok(doc) => view::validate(&doc, &schema),
            Err(findings) => findings,
        };
        assert!(findings.iter().any(|finding| finding.is_error()), "{path}");
        for finding in findings {
            let line = source.lines().nth(finding.line - 1).unwrap();
            assert!(
                (1..=line.chars().count() + 1).contains(&finding.col),
                "{path}: {finding:?}"
            );
        }
        count += 1;
    }
    assert_eq!(count, 9);
}

fn projection(root: &Path, sources: &[(&str, &str, Option<&str>)]) -> Projection {
    let schema = Arc::new(FieldSchema::load(root).unwrap());
    Projection::build(
        schema,
        sources.iter().map(|(path, source, mtime)| Source {
            path,
            bytes: source.as_bytes(),
            mtime: mtime.map(timestamp),
        }),
    )
}

fn doc(body: &str) -> view::ViewDoc {
    view::parse("@views/test.md", &format!("---\ntype: view\n---\n{body}")).unwrap()
}

#[test]
fn binds_authored_headings_and_reports_missing_title_parameters() {
    let root = Path::new(FIXTURE);
    let projection = projection(root, &[]);
    let vault = Vault::scan(root).unwrap();
    let ctx = RunContext::new(&projection, &vault, timestamp("2026-11-24T01:30:00+02:00"));
    let doc = view::parse(
        "@views/test.md",
        r#"---
type: view
params: {subject: {type: text, default: "Rest & sleep"}}
---
# $subject

## Notes for $subject and $missing
```view
from: notes
show: list
```
"#,
    )
    .unwrap();
    let result = view::run(&doc, &ctx);
    assert_eq!(result.heading_labels["$subject"], "Rest & sleep");
    assert_eq!(
        result.heading_labels["Notes for $subject and $missing"],
        "Notes for Rest & sleep and $missing"
    );
    assert_eq!(
        result.sections[0].title,
        "Notes for Rest & sleep and $missing"
    );
    assert!(result.findings.iter().any(|finding| {
        finding.line == 7 && finding.message == "Unbound heading parameter '$missing'"
    }));
    let markdown = view::render_markdown(&result);
    assert!(markdown.contains("# Rest \\& sleep"));
    assert!(markdown.contains("## Notes for Rest \\& sleep and $missing"));
}

#[test]
fn calendar_midnight_applies_to_typed_note_and_jsonl_timestamps() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("@views")).unwrap();
    fs::write(
        root.path().join("@views/changes.jsonl"),
        concat!(
            "{\"ts\":\"2026-03-26T21:59:59Z\",\"msg\":\"before\"}\n",
            "{\"ts\":\"2026-03-26T22:00:00Z\",\"msg\":\"midnight\"}\n",
            "{\"ts\":\"2026-03-27T00:15:00+02:00\",\"msg\":\"after\"}\n",
        ),
    )
    .unwrap();
    let projection = projection(
        Path::new(FIXTURE),
        &[
            ("before.md", "# Before", Some("2026-03-26T21:59:59Z")),
            ("midnight.md", "# Midnight", Some("2026-03-26T22:00:00Z")),
            ("after.md", "# After", Some("2026-03-27T00:15:00+02:00")),
        ],
    );
    let vault = Vault::scan(root.path()).unwrap();
    let ctx = RunContext::new(&projection, &vault, timestamp("2026-03-29T12:00:00+03:00"));
    let doc = doc(r#"
```view
from: notes
where: mtime >= today-2d
sort: mtime
show: table
```
```view
from: "./@views/changes.jsonl"
where: ts >= today-2d
sort: ts
show: log
```
"#);
    let result = view::run(&doc, &ctx);
    assert_eq!(
        result.sections[0]
            .rows
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        ["midnight.md", "after.md"]
    );
    assert_eq!(
        result.sections[1]
            .rows
            .iter()
            .map(|row| row.line.unwrap())
            .collect::<Vec<_>>(),
        [2, 3]
    );
}

#[test]
fn yaml_errors_stay_in_their_section_and_values_never_change_the_where_ast() {
    let root = Path::new(FIXTURE);
    let projection = projection(
        root,
        &[
            (
                "knowledge/first.md",
                "---\narea: Health\nproject_ids: [one, two]\nprobe: today\n---\n# First",
                None,
            ),
            (
                "knowledge/second.md",
                "---\narea: Health\nproject_ids: [two]\nprobe: $probe\n---\n# Second",
                None,
            ),
            (
                "knowledge/third.md",
                "---\narea: Health\n---\n# Third",
                None,
            ),
        ],
    );
    let vault = Vault::from_note_paths(projection.notes().map(|note| note.path.clone()));
    let mut ctx = RunContext::new(&projection, &vault, timestamp("2026-11-24T01:30:00+02:00"));
    ctx.args.insert(
        "probe".into(),
        r#"a" or area = Health or title = "x"#.into(),
    );
    let source = r#"---
type: view
params: {probe: text, required: text}
---
# $probe

> ```view
> from: notes
> show: tree
> ```

````markdown
```view
where: invalid YAML : : :
```
````

## Incorrect YAML
```view
from: [notes
```

## Bad literal
```view
from: notes
where: area = does-not-exist
show: table
```

## Missing
```view
from: notes
where: title = $required
show: table
```

## Lists
```view
from: notes
where: area = health and project_ids not in (one, three) and project_ids?
show: table
```

## Unsafe syntax stays a value
```view
from: notes
where: title = $probe
show: table
```

## Quoted literals
```view
from: notes
where: probe = "today" or probe = "$probe"
show: table
```
"#;
    let doc = view::parse("@views/test.md", source).unwrap();
    let result = view::run(&doc, &ctx);
    use view::Status::{Error, Ok};
    assert_eq!(
        result.sections.iter().map(|s| s.status).collect::<Vec<_>>(),
        [Error, Error, Error, Ok, Ok, Ok]
    );
    assert!(result.sections[2].diagnostics[0]
        .message
        .contains("required"));
    assert_eq!(result.sections[3].rows[0].path, "knowledge/second.md");
    assert_eq!(result.sections[3].total, 1);
    assert_eq!(result.sections[4].total, 0);
    assert_eq!(result.sections[5].total, 2);
    let finding = &result.sections[1].diagnostics[0];
    assert!(
        source.lines().nth(finding.line - 1).unwrap()[finding.col - 1..]
            .starts_with("does-not-exist")
    );
    assert!(result.sections[5]
        .diagnostics
        .iter()
        .all(|finding| !finding.is_error()));
    assert!(view::render_markdown(&result).contains(r#"a\" or area \= Health or title \= \"x"#));
}

#[test]
fn jsonl_keys_computations_groups_and_limits_share_the_same_rows() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("test.jsonl"), concat!(
        r#"{"level":"urgent","msg":"abc","message":"xyz","logger":"main","@time":"raw time","@source":"pipeline","payload":{"cost":3,"names":["A","B"]}}"#, "\n",
        r#"{"level":"urgent","msg":"def","message":"xyz","logger":"main","@time":"raw time","@source":"pipeline","payload":{"cost":3,"names":["C"]}}"#, "\n",
        r#"{"level":"urgent","msg":"ghi","message":"xyz","logger":"main","@time":"raw time","@source":"pipeline","payload":{"names":[]}}"#, "\n",
        r#"{"level":"urgent","msg":"jkl","logger":"main","@time":"raw time","@source":"pipeline","payload":{"cost":8}}"#, "\n",
    )).unwrap();
    let projection = projection(Path::new(FIXTURE), &[]);
    let vault = Vault::scan(root.path()).unwrap();
    let ctx = RunContext::new(&projection, &vault, timestamp("2026-11-24T01:30:00+02:00"));
    let doc = doc(r#"
```view
from: test.jsonl
where: level = urgent and logger? and @time = "raw time" and typed(@source, string, pipeline) and message = xyz and msg !~= nonexistent
let: {count_names: "len(payload.names)", normalized: "if(level = urgent, lower(level), 'other')"}
group: normalized
sort: -payload.cost
agg: {n: "count()", sum_cost: "sum(payload.cost)", mean_cost: "avg(payload.cost)"}
limit: 2
cols: [msg, payload.cost]
show: log
```
"#);
    let result = view::run(&doc, &ctx);
    let section = &result.sections[0];
    assert_eq!(
        section.status,
        view::Status::Ok,
        "{:?}",
        section.diagnostics
    );
    assert_eq!(section.total, 3);
    assert_eq!(section.groups[0].key, ["urgent"]);
    assert_eq!(section.groups[0].count, 3);
    assert_eq!(section.groups[0].agg["sum_cost"], 6);
    assert_eq!(section.groups[0].agg["mean_cost"], 3.0);
    assert_eq!(
        section
            .rows
            .iter()
            .map(|r| r.line.unwrap())
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(section.rows[0].fields["payload.cost"], 3);
    assert_eq!(section.rows[0].fields["count_names"], 2);
    assert_eq!(section.rows[1].fields["count_names"], 1);
    assert!(!section.rows[0].fields.contains_key("payload.names"));
    assert!(section.rows[0].rev.starts_with("sha256:"));
}

#[test]
fn budget_carries_across_sections_and_excluded_sources_cannot_feed_a_view() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("@views/out")).unwrap();
    fs::write(root.path().join("@views/out/data.jsonl"), "{\"value\":1}\n").unwrap();
    let large = format!("---\ntitle: {}\n---\n# Note\n", "x".repeat(1500));
    let projection = projection(
        Path::new(FIXTURE),
        &[
            ("knowledge/a.md", &large, None),
            ("knowledge/b.md", "# Small", None),
            ("@views/out/note.md", "# Excluded", None),
            ("@Schema/note.md", "# Excluded", None),
        ],
    );
    let vault = Vault::scan(root.path()).unwrap();
    let ctx = RunContext::new(&projection, &vault, timestamp("2026-11-24T01:30:00+02:00"));
    let doc = view::parse(
        "@views/test.md",
        r#"---
type: view
budget: 1kb
---
```view
from: notes
show: table
```
```view
from: notes
where: path = knowledge/b.md
show: table
```
```view
from: "@views/out/data.jsonl"
show: log
```
"#,
    )
    .unwrap();
    let result = view::run(&doc, &ctx);
    assert_eq!(result.sections[0].total, 2);
    assert_eq!(result.sections[1].total, 1);
    for section in &result.sections[..2] {
        assert!(section.rows.is_empty());
        assert!(section.truncated);
    }
    assert_eq!(result.sections[2].status, view::Status::Ok);
    assert_eq!(result.sections[2].total, 0);
}

#[cfg(unix)]
#[test]
fn jsonl_respects_source_bounds_and_refuses_symlink_components() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("records.jsonl"), "{\"value\":1}\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("records.jsonl"),
        root.path().join("linked.jsonl"),
    )
    .unwrap();
    fs::write(root.path().join("rows.jsonl"), "{}\n".repeat(64 * 1024 + 1)).unwrap();
    fs::File::create(root.path().join("bytes.jsonl"))
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    let projection = projection(Path::new(FIXTURE), &[]);
    let vault = Vault::scan(root.path()).unwrap();
    let ctx = RunContext::new(&projection, &vault, timestamp("2026-11-24T01:30:00+02:00"));
    for (path, error) in [
        ("linked/records.jsonl", "Unreadable"),
        ("linked.jsonl", "Unreadable"),
        ("rows.jsonl", "row JSONL limit"),
        ("bytes.jsonl", "Budget"),
    ] {
        let doc = doc(&format!("```view\nfrom: {path}\nshow: list\n```\n"));
        let result = view::run(&doc, &ctx);
        let section = &result.sections[0];
        assert_eq!(section.status, view::Status::Error);
        assert!(
            section.diagnostics[0].message.contains(error),
            "{:?}",
            section.diagnostics
        );
    }
}

#[test]
fn resolved_links_full_text_and_semantic_paths_intersect() {
    struct Semantic;
    impl view::NearProvider for Semantic {
        fn matches(&self, query: &str) -> Result<Vec<String>, String> {
            assert_eq!(query, "query terms");
            Ok(vec![
                "log/dev/okilum/decisions/2026-11-20-views-in-vault.md".into(),
                "log/dev/okilum/decisions/2026-11-24-near-snapshot-only.md".into(),
            ])
        }
    }
    let root = Path::new(FIXTURE);
    let schema = Arc::new(FieldSchema::load(root).unwrap());
    let (vault, bytes) = Vault::scan_snapshot_with(root, &mut |_, _| Ok(())).unwrap();
    let projection = Projection::build(
        schema,
        bytes.iter().map(|(path, bytes)| Source {
            path,
            bytes,
            mtime: None,
        }),
    );
    let read = |path: &str| String::from_utf8(bytes.get(path)?.clone()).ok();
    let as_of = timestamp("2026-11-24T01:30:00+02:00");
    let mut ctx = RunContext::new(&projection, &vault, as_of);
    ctx.read_source = Some(&read);
    ctx.near = Some(&Semantic);
    let view = doc(r#"
```view
from: notes
links: {from: "[[state/dev/okilum/index]]", depth: 2}
match: "declarative"
near: query terms
where: kind = decisions and decision_status = accepted
actions: [{set: due}]
show: table
```

```view
from: notes
where: path = state/dev/common/conventions.md
cols: [related_to]
show: table
```

```view
from: notes
links: {to: "[[roadmap]]"}
show: table
```
"#);
    let original =
        fs::read(root.join("log/dev/okilum/decisions/2026-11-20-views-in-vault.md")).unwrap();
    let result = view::run(&view, &ctx);
    assert_eq!(
        result.sections[0].total, 1,
        "{:?}",
        result.sections[0].diagnostics
    );
    let row = &result.sections[0].rows[0];
    assert!(row.path.ends_with("2026-11-20-views-in-vault.md"));
    assert_eq!(row.actions, ["open", "set:due"]);
    assert_eq!(fs::read(root.join(&row.path)).unwrap(), original);

    let link = &result.sections[1].rows[0].fields["related_to"][0];
    assert_eq!(link["link"], "roadmap");
    assert!(link["path"].is_null());
    assert_eq!(link["ambiguous"].as_array().unwrap().len(), 2);
    assert_eq!(result.sections[2].status, view::Status::Error);
    assert!(result.sections[2].diagnostics[0]
        .message
        .contains("Ambiguous link 'roadmap'"));
}

#[test]
fn near_reuses_only_a_compatible_vault_snapshot() {
    use serde_json::json;

    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("@views/out");
    fs::create_dir_all(&out).unwrap();
    let path = out.join("test.json");
    let vault = Vault::scan(root.path()).unwrap();
    let projection = projection(Path::new(FIXTURE), &[]);
    let doc = doc("## For the task\n```view\nfrom: notes\nnear: task\nshow: list\n```\n");
    let context = RunContext::new(&projection, &vault, timestamp("2026-11-25T09:00:00Z"))
        .with_vault_snapshots();
    let run = || view::run(&doc, &context);
    let mut snapshot = serde_json::to_value(run()).unwrap();
    let assert_unavailable = || {
        let result = run();
        let section = &result.sections[0];
        assert_eq!(section.status, view::Status::Degraded);
        assert!(section.rows.is_empty());
        assert_eq!(
            section.diagnostics[0].message,
            "near needs the search service"
        );
    };
    assert_unavailable();

    snapshot["snapshot_version"] = json!(1);
    snapshot["as_of"] = json!("2026-11-24T08:00:00Z");
    snapshot["generated_at"] = json!("2026-11-24T08:04:00Z");
    let section = &mut snapshot["sections"][0];
    section["status"] = json!("ok");
    section["diagnostics"] = json!([]);
    section["total"] = json!(1);
    section["rows"] = json!([{
        "path": "knowledge/rust-error-handling.md",
        "rev": "sha256:6343547da4bb0dd4e125bc3b6d4a1f9a627eac868376a394fe9897eb07689607",
        "line": null, "anchor": null, "group": [],
        "fields": {"title": "Rust error handling"},
        "actions": ["open"]
    }]);

    let write = |value: &Value| fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    for (key, wrong) in [
        ("snapshot_version", json!(2)),
        ("view_api", json!(1)),
        ("params", json!({"task": "different"})),
        ("as_of", json!("invalid timestamp")),
    ] {
        let mut wrong_snapshot = snapshot.clone();
        wrong_snapshot[key] = wrong;
        write(&wrong_snapshot);
        assert_unavailable();
    }
    let mut wrong_revision = snapshot.clone();
    wrong_revision["view"]["rev"] = snapshot["sections"][0]["rows"][0]["rev"].clone();
    write(&wrong_revision);
    assert_unavailable();

    write(&snapshot);
    let result = run();
    let section = &result.sections[0];
    assert_eq!(section.status, view::Status::Ok);
    assert_eq!(section.source, "snapshot");
    assert_eq!(section.snapshot_at.as_deref(), Some("2026-11-24T08:00:00Z"));
    assert_eq!(section.snapshot_clock().as_deref(), Some("10:00"));
    assert_eq!(section.rows[0].fields["title"], "Rust error handling");
    assert!(view::render_markdown(&result).contains("snapshot 10:00"));

    // A newer envelope can carry a section evaluated before the DST change.
    snapshot["sections"][0]["source"] = json!("snapshot");
    snapshot["sections"][0]["snapshot_at"] = json!("2026-10-24T07:00:00Z");
    write(&snapshot);
    let result = run();
    let section = &result.sections[0];
    assert_eq!(section.snapshot_at.as_deref(), Some("2026-10-24T07:00:00Z"));
    assert_eq!(section.snapshot_clock().as_deref(), Some("10:00"));

    fs::remove_file(&path).unwrap();
    assert_unavailable();
}

#[test]
fn opaque_and_object_fields_run_views_and_reject_where_eq_and_set_actions() {
    let root = Path::new(FIXTURE);
    let schema = Arc::new(
        FieldSchema::from_documents(|name| {
            let mut text = fs::read_to_string(root.join("@Schema").join(name))?;
            if name == "fields.yaml" {
                text = text.replace(
                    "\nbuiltin_defaults:\n",
                    concat!(
                        "\n  waits:\n",
                        "    type: list<object>\n",
                        "    index: {indexed: false}\n",
                        "    writable_by_actions: true\n",
                        "  custom_blob_field:\n",
                        "    type: custom_blob\n",
                        "    index: {indexed: false}\n",
                        "    writable_by_actions: true\n\n",
                        "builtin_defaults:\n",
                    ),
                );
            }
            Ok(text)
        })
        .unwrap(),
    );

    let note_path = "state/dev/okilum/wait-note.md";
    let note_src = concat!(
        "---\n",
        "type: note\n",
        "area: Dev\n",
        "waits:\n",
        "  - on: \"vendor\"\n",
        "    since: 2026-11-27\n",
        "    next_check: 2026-11-27\n",
        "---\n",
        "# Wait note\n",
    );
    let projection = Projection::build(
        Arc::clone(&schema),
        [Source {
            path: note_path,
            bytes: note_src.as_bytes(),
            mtime: None,
        }],
    );
    let vault = Vault::from_note_paths([note_path.into()]);
    let ctx = RunContext::new(&projection, &vault, timestamp("2026-11-27T10:00:00+02:00"));

    let ok_view = doc("```view\nfrom: notes\nshow: table\n```\n");
    assert!(
        view::validate(&ok_view, &schema)
            .iter()
            .all(|f| !f.is_error()),
        "valid view should have no errors"
    );
    let ok_result = view::run(&ok_view, &ctx);
    assert_eq!(ok_result.sections[0].status, view::Status::Ok);
    assert_eq!(ok_result.sections[0].total, 1);
    assert_eq!(ok_result.sections[0].rows[0].path, note_path);
    assert!(ok_result.sections[0]
        .diagnostics
        .iter()
        .all(|f| !f.is_error()));

    // Existence check on opaque fields should work cleanly without panicking.
    let exists_view = doc("```view\nfrom: notes\nwhere: waits?\nshow: table\n```\n");
    assert!(view::validate(&exists_view, &schema)
        .iter()
        .all(|f| !f.is_error()));
    let exists_result = view::run(&exists_view, &ctx);
    assert_eq!(exists_result.sections[0].status, view::Status::Ok);
    assert_eq!(exists_result.sections[0].total, 0);

    // Exact comparison on opaque fields (literal and parameter) must emit a validation error and never panic.
    let eq_view = doc("```view\nfrom: notes\nwhere: waits = \"vendor\"\nshow: table\n```\n");
    let eq_findings = view::validate(&eq_view, &schema);
    assert!(eq_findings.iter().any(|f| f.is_error()), "{eq_findings:?}");
    let eq_result = view::run(&eq_view, &ctx);
    assert_eq!(eq_result.sections[0].status, view::Status::Error);

    let param_view = view::parse(
        "@views/param-opaque.md",
        "---\ntype: view\nparams: {p: text}\n---\n```view\nfrom: notes\nwhere: waits = $p\nshow: table\n```\n",
    )
    .unwrap();
    let param_findings = view::validate(&param_view, &schema);
    assert!(
        param_findings.iter().any(|f| f.is_error()),
        "{param_findings:?}"
    );
    let mut param_ctx = ctx;
    param_ctx.args.insert("p".into(), "vendor".into());
    let param_result = view::run(&param_view, &param_ctx);
    assert_eq!(param_result.sections[0].status, view::Status::Error);

    // `set` actions targeting opaque fields must be rejected even if writable_by_actions is true.
    for field in ["waits", "custom_blob_field"] {
        let action_view = doc(&format!(
            "```view\nfrom: notes\nactions:\n  - {{set: {field}}}\nshow: table\n```\n"
        ));
        let findings = view::validate(&action_view, &schema);
        assert!(
            findings.iter().any(|f| f.is_error()),
            "expected error finding for set:{field}: {findings:?}"
        );
    }

    // Also verify `is_opaque()` rejects an allowlisted `set` field (`owner`) when declared opaque.
    let original_owner = concat!(
        "  owner:\n",
        "    type: string\n",
        "    format: slug\n",
        "    origin: stored\n",
        "    rule: identifies the person or assistant expected to act\n",
        "    writeback: none\n",
        "    writers: [people, agents]\n",
        "    required: {}\n",
        "    index: {indexed: true, facet: true, sort: true}\n",
        "    writable_by_actions: true",
    );
    let opaque_owner = concat!(
        "  owner:\n",
        "    type: custom_blob\n",
        "    origin: stored\n",
        "    index: {indexed: false}\n",
        "    writable_by_actions: true",
    );
    let opaque_owner_schema = FieldSchema::from_documents(|name| {
        let mut text = fs::read_to_string(root.join("@Schema").join(name))?;
        if name == "fields.yaml" {
            assert!(text.contains(original_owner));
            text = text.replace(original_owner, opaque_owner);
        }
        Ok(text)
    })
    .unwrap();
    let owner_action_view =
        doc("```view\nfrom: notes\nactions:\n  - {set: owner}\nshow: table\n```\n");
    let owner_findings = view::validate(&owner_action_view, &opaque_owner_schema);
    assert!(
        owner_findings.iter().any(|f| f.is_error()),
        "expected error finding when owner is opaque: {owner_findings:?}"
    );
}
