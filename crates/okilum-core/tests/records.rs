use okilum_core::{
    records::{FieldSchema, FieldType, Projection, Source, Value},
    tasks, Vault,
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use time::macros::date;

#[test]
fn task_inline_fields_take_precedence_in_source_order() {
    let source = "- [ ] Check [DUE:: 2026-11-27] 📅 2026-11-25 (scheduled:: 2026-11-24)\n\
                  - [x] Checked (done:: 2026-11-26) [priority:: 1]\n\
                  - [ ] Skip `[due:: 2026-11-22]` [link (due:: 2026-11-23)](a.md) 📅 2026-11-28\n\
                  - [ ] Invalid [due:: 2026-02-30] 📅 2026-11-25\n";
    let rows = tasks::parse("tasks.md", source);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].due, Some(date!(2026 - 11 - 27)));
    assert_eq!(rows[0].scheduled, Some(date!(2026 - 11 - 24)));
    assert_eq!(rows[1].done, Some(date!(2026 - 11 - 26)));
    assert_eq!(rows[1].priority, 1);
    assert_eq!(rows[2].due, Some(date!(2026 - 11 - 28)));
    assert_eq!(rows[3].due, None);
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/views")
}

#[test]
fn projects_fixture_types_links_and_coverage() {
    let fixture = fixture();
    let schema = Arc::new(FieldSchema::load(&fixture).unwrap());
    let (vault, sources) = Vault::scan_snapshot_with(&fixture, &mut |_, _| Ok(())).unwrap();
    let projection = Projection::build(
        schema,
        sources.iter().map(|(path, bytes)| Source {
            path,
            bytes,
            mtime: None,
        }),
    );
    let hygiene = projection.note("knowledge/sleep-hygiene.md").unwrap();
    assert_eq!(hygiene.fields["area"], Value::String("Health".into()));
    let legacy = projection.note("Projects/Okilum Legacy/Notes.md").unwrap();
    assert_eq!(legacy.fields["area"], Value::String("Dev".into()));
    assert_eq!(legacy.fields["status"], Value::String("active".into()));
    assert_eq!(legacy.fields.get("layer"), None);
    assert!(Projection::missing([legacy, hygiene], "layer").contains(&legacy.path.as_str()));
    assert_eq!(
        projection.legacy_paths().collect::<Vec<_>>(),
        [
            "Areas/HomeLab/Network.md",
            "Projects/Okilum Legacy/Notes.md",
            "Resources/Guides/Git tips.md"
        ]
    );

    let runbook = projection.note("state/homelab/backups/runbook.md").unwrap();
    assert_eq!(runbook.fields["layer"], Value::String("state".into()));
    assert!(runbook
        .findings
        .iter()
        .any(|f| f.code == "fields/conflict" && f.field == "layer"));

    let conventions = projection.note("state/dev/common/conventions.md").unwrap();
    let links = &conventions.fields["related_to"];
    let json = links.to_json(&vault, &conventions.path);
    assert_eq!(
        json,
        json!([{"link":"roadmap","path":null,"ambiguous":[
            "state/dev/okilum/roadmap.md", "state/dev/tasker/roadmap.md"
        ]}])
    );
    assert!(projection
        .note("@Schema/tests/view-invalid/bad-show.md")
        .is_none());

    let questions = projection.note("state/dev/okilum/questions.md").unwrap();
    let task = &questions.tasks[0];
    assert_eq!(task.fields["owner"], Value::String("oleg".into()));
    assert_eq!(task.fields["due"], Value::Date(date!(2026 - 11 - 26)));
    assert_eq!(
        task.fields["text"],
        Value::String("Approve `@views/out/` as the only snapshot folder".into())
    );
    assert_eq!(
        task.fields["thread"],
        Value::String("t3code://threads/env-fixture/thread-views".into())
    );
    assert_eq!(questions.sections[0].line, 6);
}

#[test]
fn incremental_projection_handles_mirrors_invalid_values_and_removals() {
    let schema = Arc::new(FieldSchema::load(&fixture()).unwrap());
    let path = "log/dev/common/reports/2026-11-25-first.md";
    let legacy = "Dev/Areas/a/retired.md";
    let mut projection = Projection::build(
        schema.clone(),
        [Source {
            path: legacy,
            bytes: b"---\narea: Dev\nkind: reports\n---\n",
            mtime: None,
        }],
    );
    assert_eq!(projection.note(legacy).unwrap().fields.get("kind"), None);

    let bytes = b"---\narea: [dev, health]\ncontext: [other]\nproject_ids: [one, two]\nupdated: 2026-11-24T23:30:01Z\nmeasurement: [1, true, hi]\n---\n# Report\n\n- [ ] Try [due:: invalid] (DUE:: 2026-11-25)\n";
    let source = Source {
        path,
        bytes,
        mtime: None,
    };
    let skipped = Source {
        path: "@views/out/a.md",
        ..source
    };
    projection.apply([source, skipped], [legacy]);
    assert!(projection.note(legacy).is_none());
    assert!(projection.note(skipped.path).is_none());
    let note = projection.note(path).unwrap();
    assert_eq!(note.fields.get("area"), None);
    assert_eq!(note.fields["context"], Value::String("common".into()));
    assert_eq!(
        note.fields["project_ids"],
        Value::List(vec![
            Value::String("one".into()),
            Value::String("two".into())
        ])
    );
    assert_eq!(
        note.fields["updated"].as_date(),
        Some(date!(2026 - 11 - 25))
    );
    assert_eq!(
        note.fields["measurement"],
        Value::List(vec![
            Value::Number(1.into()),
            Value::Bool(true),
            Value::String("hi".into())
        ])
    );
    assert!(note
        .findings
        .iter()
        .any(|f| f.code == "fields/conflict" && f.field == "context"));
    assert!(note
        .findings
        .iter()
        .any(|f| f.code == "fields/duplicate" && f.field == "due" && f.line == Some(10)));
    assert!(!note.tasks[0].fields.contains_key("due"));
    assert_eq!(
        note,
        Projection::build(schema, [source]).note(path).unwrap()
    );
}

#[test]
fn schema_rejects_missing_references_and_domain_drift() {
    let reader = |name: &str| -> anyhow::Result<String> {
        Ok(std::fs::read_to_string(
            fixture().join("@Schema").join(name),
        )?)
    };
    assert!(FieldSchema::from_documents(|name| {
        if name == "tags.yaml" {
            anyhow::bail!("not found");
        }
        reader(name)
    })
    .is_err());
    let error = FieldSchema::from_documents(|name| {
        let text = reader(name)?;
        Ok(if name == "layout.yaml" {
            text.replace("area: Health", "area: Different")
        } else {
            text
        })
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("Area and domain lists disagree"));
}

#[test]
fn opaque_and_object_fields_load_and_project_cleanly() {
    let extra_fields = concat!(
        "\n  waits:\n",
        "    type: list<object>\n",
        "    index: {indexed: false}\n",
        "    writable_by_actions: true\n",
        "  payload_obj:\n",
        "    type: object\n",
        "  custom_blob_field:\n",
        "    type: custom_blob\n",
        "    index: {indexed: false}\n",
        "    writable_by_actions: true\n",
        "  custom_blob_list:\n",
        "    type: list<custom_blob>\n",
        "    index: {indexed: false}\n",
        "  req_opaque:\n",
        "    type: list<object>\n",
        "    index: {indexed: false}\n",
        "    legacy_keys: [old_req_opaque]\n",
        "    required: {layers: [state]}\n\n",
    );
    let schema_from_docs = FieldSchema::from_documents(|name| {
        let mut text = std::fs::read_to_string(fixture().join("@Schema").join(name))?;
        if name == "fields.yaml" {
            assert!(text.contains("\nbuiltin_defaults:\n"));
            text = text.replace(
                "\nbuiltin_defaults:\n",
                &format!("{extra_fields}builtin_defaults:\n"),
            );
        }
        Ok(text)
    })
    .unwrap();

    // Also exercise FieldSchema::load against a temporary @Schema copy on disk.
    let temp = tempfile::tempdir().unwrap();
    let schema_dir = temp.path().join("@Schema");
    std::fs::create_dir_all(&schema_dir).unwrap();
    for file in [
        "fields.yaml",
        "layout.yaml",
        "frontmatter-schema.yaml",
        "tags.yaml",
    ] {
        let mut content = std::fs::read_to_string(fixture().join("@Schema").join(file)).unwrap();
        if file == "fields.yaml" {
            content = content.replace(
                "\nbuiltin_defaults:\n",
                &format!("{extra_fields}builtin_defaults:\n"),
            );
        }
        std::fs::write(schema_dir.join(file), content).unwrap();
    }
    let schema = Arc::new(FieldSchema::load(temp.path()).unwrap());

    for loaded in [&schema_from_docs, schema.as_ref()] {
        assert_eq!(
            loaded.fields["waits"].field_type,
            FieldType::List(Box::new(FieldType::Object))
        );
        assert!(loaded.fields["waits"].field_type.is_opaque());
        assert_eq!(loaded.fields["payload_obj"].field_type, FieldType::Object);
        assert!(loaded.fields["payload_obj"].field_type.is_opaque());
        assert_eq!(
            loaded.fields["custom_blob_field"].field_type,
            FieldType::Opaque("custom_blob".into())
        );
        assert!(loaded.fields["custom_blob_field"].field_type.is_opaque());
        assert_eq!(
            loaded.fields["custom_blob_list"].field_type,
            FieldType::List(Box::new(FieldType::Opaque("custom_blob".into())))
        );
        assert!(loaded.fields["custom_blob_list"].field_type.is_opaque());
        assert!(!loaded.fields["title"].field_type.is_opaque());
    }

    let path = "state/dev/okilum/wait-note.md";
    let note_src = concat!(
        "---\n",
        "type: note\n",
        "area: Dev\n",
        "waits:\n",
        "  - on: \"vendor\"\n",
        "    since: 2026-11-27\n",
        "    next_check: 2026-11-27\n",
        "custom_blob_field:\n",
        "  a: 1\n",
        "req_opaque:\n",
        "  - k: v\n",
        "---\n",
        "# Wait note\n",
    );
    let legacy_path = "state/dev/okilum/wait-legacy.md";
    let legacy_src = "---\ntype: note\narea: Dev\nold_req_opaque:\n  - k: v\n---\n# Legacy\n";
    let missing_path = "state/dev/okilum/wait-missing.md";
    let missing_src = "---\ntype: note\narea: Dev\n---\n# Missing\n";
    let empty_path = "state/dev/okilum/wait-empty.md";
    let empty_src = "---\ntype: note\narea: Dev\nreq_opaque: []\n---\n# Empty\n";

    let projection = Projection::build(
        schema,
        [
            Source {
                path,
                bytes: note_src.as_bytes(),
                mtime: None,
            },
            Source {
                path: legacy_path,
                bytes: legacy_src.as_bytes(),
                mtime: None,
            },
            Source {
                path: missing_path,
                bytes: missing_src.as_bytes(),
                mtime: None,
            },
            Source {
                path: empty_path,
                bytes: empty_src.as_bytes(),
                mtime: None,
            },
        ],
    );

    let note = projection.note(path).unwrap();
    assert!(
        !note
            .findings
            .iter()
            .any(|f| matches!(f.code, "fields/invalid" | "fields/missing")
                && matches!(
                    f.field.as_str(),
                    "waits" | "custom_blob_field" | "req_opaque"
                )),
        "unexpected findings: {:?}",
        note.findings
    );
    assert!(!note.fields.contains_key("waits"));
    assert!(!note.fields.contains_key("custom_blob_field"));
    assert!(!note.fields.contains_key("req_opaque"));

    let legacy_note = projection.note(legacy_path).unwrap();
    assert!(
        !legacy_note
            .findings
            .iter()
            .any(|f| f.code == "fields/missing" && f.field == "req_opaque"),
        "legacy opaque key should satisfy required check: {:?}",
        legacy_note.findings
    );

    for absent in [missing_path, empty_path] {
        let record = projection.note(absent).unwrap();
        assert!(
            record
                .findings
                .iter()
                .any(|f| f.code == "fields/missing" && f.field == "req_opaque"),
            "absent required opaque field on {absent} must be flagged: {:?}",
            record.findings
        );
    }
}

#[test]
fn schema_rejects_unknown_indexed_and_empty_field_types() {
    let base_fields = std::fs::read_to_string(fixture().join("@Schema/fields.yaml")).unwrap();
    let load_with_extra = |snippet: &str| {
        FieldSchema::from_documents(|name| {
            if name == "fields.yaml" {
                Ok(base_fields.replace(
                    "\nbuiltin_defaults:\n",
                    &format!("\n{snippet}\n\nbuiltin_defaults:\n"),
                ))
            } else {
                Ok(std::fs::read_to_string(
                    fixture().join("@Schema").join(name),
                )?)
            }
        })
    };

    for (label, snippet) in [
        ("omitted index", "  bad_field:\n    type: custom_blob"),
        (
            "explicit indexed true",
            "  bad_field:\n    type: custom_blob\n    index: {indexed: true}",
        ),
        (
            "empty index mapping",
            "  bad_field:\n    type: custom_blob\n    index: {}",
        ),
        (
            "list of unknown indexed",
            "  bad_field:\n    type: list<custom_blob>",
        ),
        (
            "empty type even unindexed",
            "  bad_field:\n    type: \"\"\n    index: {indexed: false}",
        ),
        (
            "empty list inner even unindexed",
            "  bad_field:\n    type: \"list<>\"\n    index: {indexed: false}",
        ),
    ] {
        let err = load_with_extra(snippet)
            .expect_err(&format!("{label} should fail FieldSchema::from_documents"));
        assert!(
            format!("{err:#}").contains("Unknown field type"),
            "{label}: unexpected error {err:#}"
        );
    }

    // Negative control via FieldSchema::load on disk.
    let temp = tempfile::tempdir().unwrap();
    let schema_dir = temp.path().join("@Schema");
    std::fs::create_dir_all(&schema_dir).unwrap();
    for file in [
        "fields.yaml",
        "layout.yaml",
        "frontmatter-schema.yaml",
        "tags.yaml",
    ] {
        let mut content = std::fs::read_to_string(fixture().join("@Schema").join(file)).unwrap();
        if file == "fields.yaml" {
            content = content.replace(
                "\nbuiltin_defaults:\n",
                "\n  bad_indexed:\n    type: custom_blob\n\nbuiltin_defaults:\n",
            );
        }
        std::fs::write(schema_dir.join(file), content).unwrap();
    }
    let err = FieldSchema::load(temp.path()).unwrap_err();
    assert!(format!("{err:#}").contains("Unknown field type"));
}

#[test]
#[ignore = "requires a local copy of a 6,356-note vault"]
fn projection_timing() {
    use std::{hint::black_box, time::Instant};

    let root = PathBuf::from(std::env::var_os("OKILUM_RECORDS_BENCH_VAULT").unwrap());
    let schema = Arc::new(FieldSchema::load(&root).unwrap());
    let (vault, snapshot, _) =
        okilum_core::vault::warm::reconcile_parallel(&root, None, true, &mut |_, _| Ok(()))
            .unwrap();
    let sources = snapshot.sources().unwrap();
    assert_eq!(sources.len(), 6_356);
    let mut input: Vec<_> = sources
        .iter()
        .map(|(path, bytes)| Source {
            path,
            bytes,
            mtime: std::fs::metadata(root.join(path))
                .and_then(|meta| meta.modified())
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from)
                .map(|t| t.fixed_offset()),
        })
        .collect();
    input.sort_unstable_by_key(|source| source.path);
    let included: Vec<_> = input
        .iter()
        .copied()
        .filter(|source| schema.includes(source.path))
        .collect();
    // Spread changed sources over the vault. A changed payload guarantees that
    // the batch cannot pass by skipping identical revisions.
    let changed: Vec<_> = (0..20)
        .map(|i| {
            let source = included[i * included.len() / 20];
            let mut bytes = source.bytes.to_vec();
            bytes.extend(b"\n# Updated heading\n\n- [ ] Confirm (priority:: 1)\n");
            (source, bytes)
        })
        .collect();

    for _ in 0..5 {
        let start = Instant::now();
        let mut projection = Projection::from_snapshot(schema.clone(), &snapshot);
        let full = start.elapsed();
        let start = Instant::now();
        projection.apply(
            changed
                .iter()
                .map(|(source, bytes)| Source { bytes, ..*source }),
            [],
        );
        let batch = start.elapsed();
        let note = projection.note(changed[0].0.path).unwrap();
        assert!(note.sections.iter().any(|s| s.anchor == "Updated heading"));
        black_box(note.value("backlinks", &vault, date!(2026 - 11 - 24)));
        println!(
            "inputs={} rows={} full_ms={:.3} 20_file_batch_ms={:.3}",
            input.len(),
            projection.notes().count(),
            full.as_secs_f64() * 1000.0,
            batch.as_secs_f64() * 1000.0
        );
    }
}
