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
        assert!(!view::render_markdown(&result).is_empty(), "{name}");
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
