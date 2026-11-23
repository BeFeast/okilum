//! The views fixture vault (#3262, spec task T1) must stay loadable: every note
//! has parseable frontmatter, every top-level `view` block is a YAML mapping
//! whose keys the view schema knows, every JSONL line is a JSON object, and the
//! contract copies under `@Schema/` parse. The view conformance tests exercise
//! execution against the same vault.
use okilum_core::{properties, tasks};
use serde_json::Value as Json;
use serde_yaml::Value as Yaml;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/views")
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn rel(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

/// Top-level ```view fences of a note body (not inside lists, quotes or other
/// fences), as `(line, yaml)`.
fn view_blocks(body: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    // (start line, is a view fence, collected text)
    let mut open: Option<(usize, bool, String)> = None;
    for (n, line) in body.lines().enumerate() {
        match &mut open {
            Some((start, is_view, text)) => {
                if line.trim_end() == "```" {
                    if *is_view {
                        blocks.push((*start, std::mem::take(text)));
                    }
                    open = None;
                } else {
                    text.push_str(line);
                    text.push('\n');
                }
            }
            None if line.starts_with("```") => {
                open = Some((n + 1, line.trim_end() == "```view", String::new()));
            }
            None => {}
        }
    }
    assert!(open.is_none(), "unclosed fence");
    blocks
}

fn frontmatter(path: &Path, source: &str) -> Vec<properties::Property> {
    let yaml = properties::frontmatter_block(source)
        .unwrap_or_else(|| panic!("{}: no frontmatter", rel(path)));
    let props = properties::parse(yaml).unwrap_or_else(|e| panic!("{}: {e}", rel(path)));
    assert!(!props.is_empty(), "{}: empty frontmatter", rel(path));
    props
}

fn prop<'a>(props: &'a [properties::Property], key: &str) -> Option<&'a properties::PropertyValue> {
    props.iter().find(|p| p.key == key).map(|p| &p.value)
}

fn text(value: Option<&properties::PropertyValue>) -> Option<&str> {
    match value? {
        properties::PropertyValue::Text(s) => Some(s),
        _ => None,
    }
}

#[test]
fn every_fixture_file_loads() {
    let schema: Json = serde_json::from_str(
        &std::fs::read_to_string(root().join("@Schema/view.schema.json")).unwrap(),
    )
    .expect("view.schema.json parses");
    let block_keys: BTreeSet<String> = schema["$defs"]["block"]["properties"]
        .as_object()
        .expect("block properties")
        .keys()
        .cloned()
        .collect();
    let fields: Yaml =
        serde_yaml::from_str(&std::fs::read_to_string(root().join("@Schema/fields.yaml")).unwrap())
            .expect("fields.yaml parses");
    let note_fields = fields
        .get("fields")
        .and_then(Yaml::as_mapping)
        .expect("note field definitions");
    assert!(note_fields.len() > 10);
    let writable_fields: BTreeSet<_> = note_fields
        .iter()
        .filter_map(|(name, field)| {
            let flag = field.get("writable_by_actions")?;
            let name = name.as_str().expect("note field name");
            flag.as_bool()
                .unwrap_or_else(|| panic!("{name}: writable_by_actions must be a boolean"))
                .then_some(name)
        })
        .collect();
    assert_eq!(writable_fields, BTreeSet::from(["due", "owner", "status"]));

    let mut all = Vec::new();
    files(&root(), &mut all);
    all.sort();
    let (mut lifecycle, mut para, mut views, mut invalid, mut jsonl_rows) = (0, 0, 0, 0, 0);
    let mut health = 0;
    for path in &all {
        let name = rel(path);
        let source = std::fs::read_to_string(path).unwrap();
        if name.ends_with(".jsonl") {
            for (n, line) in source.lines().enumerate() {
                let row: Json =
                    serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}:{}: {e}", n + 1));
                assert!(row.is_object(), "{name}:{}: not an object", n + 1);
                jsonl_rows += 1;
            }
            continue;
        }
        if name.starts_with("@Schema/") && !name.ends_with(".md") {
            continue; // the contract copies, parsed above
        }
        if name == "README.md" {
            continue;
        }
        assert!(name.ends_with(".md"), "unexpected fixture file {name}");
        let props = frontmatter(path, &source);
        let body = okilum_core::render::without_frontmatter(&source);
        let blocks = view_blocks(body);
        for (line, yaml) in &blocks {
            let block: Yaml = serde_yaml::from_str(yaml)
                .unwrap_or_else(|e| panic!("{name}: view block at body line {line}: {e}"));
            let map = block.as_mapping().unwrap_or_else(|| {
                panic!("{name}: view block at body line {line} is not a mapping")
            });
            if name.starts_with("@views/") {
                for key in map.keys() {
                    let key = key.as_str().unwrap();
                    assert!(
                        block_keys.contains(key),
                        "{name}: block key {key} not in view.schema.json"
                    );
                }
            }
        }
        if name.starts_with("@views/") || name.starts_with("@Schema/tests/view-invalid/") {
            assert_eq!(text(prop(&props, "type")), Some("view"), "{name}: type");
            if name.starts_with("@views/") {
                views += 1;
                let template = prop(&props, "extends").is_some();
                assert!(template || !blocks.is_empty(), "{name}: no view blocks");
            } else {
                invalid += 1;
            }
            continue;
        }
        let top = name.split('/').next().unwrap();
        match top {
            "state" | "log" | "knowledge" | "sources" | "inbox" => lifecycle += 1,
            "Projects" | "Areas" | "Resources" => para += 1,
            other => panic!("{name}: unknown top folder {other}"),
        }
        if text(prop(&props, "area")).is_some_and(|a| a.eq_ignore_ascii_case("health")) {
            health += 1;
        }
        // Task notes parse into tasks (the questions and okilum views read them).
        if source.contains("- [") {
            assert!(!tasks::parse(&name, &source).is_empty(), "{name}: tasks");
        }
    }
    assert!(lifecycle >= 25, "lifecycle notes: {lifecycle}");
    assert_eq!(para, 3, "old-layout (PARA) notes");
    assert_eq!(views, 7, "example views in @views/");
    assert_eq!(invalid, 9, "invalid views");
    assert_eq!(health, 4, "Health notes (the Areas tab count in T8)");
    assert!(jsonl_rows >= 5, "changes.jsonl rows: {jsonl_rows}");
}

#[test]
fn fixture_keeps_its_deliberate_edge_cases() {
    let read = |p: &str| std::fs::read_to_string(root().join(p)).unwrap();
    // Alias case: lower-case `area: health` normalises to Health.
    assert!(read("knowledge/sleep-hygiene.md").contains("\narea: health\n"));
    // Ambiguous link: two notes named roadmap.md, linked as [[roadmap]].
    assert!(root().join("state/dev/okilum/roadmap.md").is_file());
    assert!(root().join("state/dev/tasker/roadmap.md").is_file());
    assert!(read("state/dev/common/conventions.md").contains("[[roadmap]]"));
    // Path/frontmatter conflict: stored layer disagrees with the state/ path.
    assert!(read("state/homelab/backups/runbook.md").contains("\nlayer: knowledge\n"));
    // The task T7 checks in the okilum view.
    assert!(read("state/dev/okilum/index.md").contains("- [ ] Write view schema"));
}
