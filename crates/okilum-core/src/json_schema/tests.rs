use super::*;
use serde_json::json;

fn write(root: &Path, rel: &str, value: Value) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value.to_string()).unwrap();
}
fn schema(value: Value) -> Value {
    let mut value = value;
    value["$schema"] = DRAFT.into();
    value
}
fn fixture(value: Value) -> (tempfile::TempDir, Association) {
    let directory = tempfile::tempdir().unwrap();
    write(directory.path(), "schema.json", schema(value));
    let association = Association {
        project: directory.path().into(),
        schema: Some(directory.path().join("schema.json")),
        ..Default::default()
    };
    (directory, association)
}

#[test]
fn independent_associations_last_match_and_nearest_project_win() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        ".okilum/viewers.json",
        json!({"version":1,"json":[{"pattern":"**/*.samplejob"}],"schemas":[{"pattern":"**/*.samplejob","schema":"general.json"},{"pattern":"jobs/special.samplejob","schema":"specific.json"},{"pattern":"other.json","schema":"schema-only.json"}]}),
    );
    write(root, "jobs/special.samplejob", json!({}));
    write(root, "other.json", json!({}));
    let result = associate(root, &root.join("jobs/special.samplejob")).unwrap();
    assert!(result.json);
    assert_eq!(
        result.schema,
        Some(root.canonicalize().unwrap().join("specific.json"))
    );
    let result = associate(root, &root.join("other.json")).unwrap();
    assert!(
        !result.json,
        "schema association does not select the viewer"
    );
    assert!(result.schema.is_some());
    write(
        root,
        "jobs/.okilum/viewers.json",
        json!({"version":1,"json":[],"schemas":[]}),
    );
    let result = associate(root, &root.join("jobs/special.samplejob")).unwrap();
    assert!(!result.json);
    assert!(result.schema.is_none(), "nearest config replaces parent");
}

#[test]
fn nested_arrays_references_missing_fields_enums_unknown_keys_and_source_are_preserved() {
    let (directory, association) = fixture(
        json!({"type":"object","required":["name","absent"],"properties":{"name":{"type":"string","title":"Job name","description":"A declared job identifier"},"absent":{"type":"boolean"},"rows":{"type":"array","items":{"$ref":"types.json#/$defs/row"}}}}),
    );
    write(
        directory.path(),
        "types.json",
        schema(
            json!({"$defs":{"row":{"type":"object","required":["status"],"properties":{"status":{"type":"string","enum":["ready","done"],"description":"Declared processing state"},"x/y~z":{"type":"integer"}}}}}),
        ),
    );
    let text = r#"{"name":"Synthetic","rows":[{"status":"other","x/y~z":2,"unknown":{"nested":true}},{}]}"#;
    let bytes = text.as_bytes().to_vec();
    let report = inspect(&association, text).unwrap();
    assert_eq!(report.verdict, Verdict::Invalid);
    assert_eq!(report.violations.len(), 3);
    assert!(report
        .violations
        .iter()
        .any(|v| v.path == "/rows/0/status" && v.message.contains("enum")));
    assert!(report
        .violations
        .iter()
        .any(|v| v.path == "/rows/1/status" && v.message.contains("Required")));
    assert!(report.fields["/name"].required);
    assert_eq!(report.fields["/name"].title.as_deref(), Some("Job name"));
    assert!(report.fields["/absent"].missing);
    assert!(!report.fields["/rows"].required);
    assert_eq!(
        report.fields["/rows/0/status"].description.as_deref(),
        Some("Declared processing state")
    );
    assert!(report.fields.contains_key("/rows/0/x~1y~0z"));
    assert!(report
        .explanation("/rows/0/unknown")
        .contains("No schema field declaration"));
    assert_eq!(text.as_bytes(), bytes);
    assert_eq!(
        inspect(
            &association,
            r#"{"name":"Synthetic","absent":true,"rows":[{"status":"ready"}]}"#
        )
        .unwrap()
        .verdict,
        Verdict::Valid
    );
}

#[test]
fn unsupported_drafts_keywords_and_broken_or_recursive_refs_never_claim_valid() {
    for value in [
        json!({"$schema":"http://json-schema.org/draft-07/schema#"}),
        schema(json!({"pattern":"x"})),
        schema(json!({"properties":{"unused":{"format":"date"}}})),
        schema(json!({"$ref":"missing.json"})),
        schema(json!({"$ref":"https://example.invalid/schema.json"})),
        schema(json!({"$ref":"#/$defs/missing"})),
        schema(json!({"$ref":"#"})),
        schema(json!({"$defs":{"loop":{"$ref":"#/$defs/loop"}}})),
        schema(json!({"type":17})),
        schema(json!({"additionalProperties":{"type":"string"}})),
    ] {
        let (directory, association) = fixture(json!({}));
        write(directory.path(), "schema.json", value.clone());
        let report = inspect(&association, "{}").unwrap();
        assert_eq!(report.verdict, Verdict::Unavailable, "{value}");
        assert!(!report.summary.contains("validation: valid"));
    }
    let (directory, association) = fixture(json!({"$ref":"other.json"}));
    write(
        directory.path(),
        "other.json",
        schema(json!({"$ref":"schema.json"})),
    );
    assert_eq!(
        inspect(&association, "{}").unwrap().verdict,
        Verdict::Unavailable
    );
    std::fs::write(directory.path().join("schema.json"), "{broken").unwrap();
    assert_eq!(
        inspect(&association, "{}").unwrap().verdict,
        Verdict::Unavailable
    );
}

#[test]
fn no_schema_syntax_failure_bounds_and_annotations_are_honest() {
    assert!(inspect(&Association::default(), "{}").is_none());
    let (_directory, association) = fixture(
        json!({"type":"object","properties":{"count":{"type":"integer","default":8}},"additionalProperties":false}),
    );
    let report = inspect(&association, "{}").unwrap();
    assert_eq!(report.verdict, Verdict::Valid);
    assert!(report.fields["/count"].missing, "default is never applied");
    assert_eq!(
        inspect(&association, "{broken").unwrap().verdict,
        Verdict::SyntaxInvalid
    );
    assert_eq!(
        inspect(&association, &" ".repeat(DOCUMENT_BYTES + 1))
            .unwrap()
            .verdict,
        Verdict::Incomplete
    );
    let (_directory, association) = fixture(json!({"type":"array","items":{"type":"integer"}}));
    let many = format!("[{}]", vec!["0"; MAX_FIELDS + 1].join(","));
    assert_eq!(
        inspect(&association, &many).unwrap().verdict,
        Verdict::Incomplete
    );
    let (_directory, association) = fixture(json!({"type":"array","items":false}));
    let many = format!("[{}]", vec!["0"; MAX_ISSUES + 1].join(","));
    assert_eq!(
        inspect(&association, &many).unwrap().verdict,
        Verdict::Incomplete
    );
}

#[test]
fn subset_applies_only_to_relevant_instance_types_and_ref_siblings_are_constraints() {
    let (_directory, association) = fixture(
        json!({"$defs":{"n":{"type":"number","minimum":2}},"$ref":"#/$defs/n","maximum":4}),
    );
    assert_eq!(inspect(&association, "3").unwrap().verdict, Verdict::Valid);
    for text in ["1", "5", "\"3\""] {
        assert_eq!(
            inspect(&association, text).unwrap().verdict,
            Verdict::Invalid
        );
    }
    let (_directory, association) =
        fixture(json!({"minLength":2,"maxLength":3,"minItems":1,"maxItems":2}));
    assert_eq!(inspect(&association, "2").unwrap().verdict, Verdict::Valid);
    assert_eq!(
        inspect(&association, "\"é雪\"").unwrap().verdict,
        Verdict::Valid
    );
    for text in ["\"é\"", "[]", "[1,2,3]"] {
        assert_eq!(
            inspect(&association, text).unwrap().verdict,
            Verdict::Invalid
        );
    }
}

#[test]
fn configs_are_bounded_and_bad_patterns_do_not_silently_select_a_winner() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "example.samplejob", json!({}));
    assert!(
        !associate(root, &root.join("example.samplejob"))
            .unwrap()
            .json
    );
    for value in [
        json!({"version":2}),
        json!({"version":1,"json":[{"pattern":"../*"}]}),
        json!({"version":1,"schemas":[{"pattern":"*","schema":"https://example.invalid/s"}]}),
        json!({"version":1,"json":[{"pattern":"["}]}),
    ] {
        write(root, ".okilum/viewers.json", value);
        assert!(associate(root, &root.join("example.samplejob")).is_err());
    }
    std::fs::write(
        root.join(".okilum/viewers.json"),
        " ".repeat(CONFIG_BYTES + 1),
    )
    .unwrap();
    assert!(associate(root, &root.join("example.samplejob"))
        .unwrap_err()
        .contains("exceeds"));
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_refused_for_config_schema_and_references() {
    use std::os::unix::fs::symlink;
    let (directory, association) = fixture(json!({"$ref":"outside.json"}));
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "schema.json", schema(json!({})));
    symlink(
        outside.path().join("schema.json"),
        directory.path().join("outside.json"),
    )
    .unwrap();
    assert_eq!(
        inspect(&association, "{}").unwrap().verdict,
        Verdict::Unavailable
    );
    let mut escaped = association.clone();
    escaped.schema = Some(directory.path().join("outside.json"));
    assert_eq!(
        inspect(&escaped, "{}").unwrap().verdict,
        Verdict::Unavailable
    );
    write(directory.path(), "example.samplejob", json!({}));
    std::fs::create_dir(directory.path().join(".okilum")).unwrap();
    symlink(
        outside.path().join("schema.json"),
        directory.path().join(".okilum/viewers.json"),
    )
    .unwrap();
    assert!(associate(
        directory.path(),
        &directory.path().join("example.samplejob")
    )
    .is_err());
}

#[test]
fn numeric_equality_ambiguity_and_table_locations_remain_exact() {
    let (_directory, association) = fixture(json!({"enum":[1,{"n":2}],"default":1}));
    assert_eq!(
        inspect(&association, "1.0").unwrap().verdict,
        Verdict::Valid
    );
    assert_eq!(
        inspect(&association, "{\"n\":2.0}").unwrap().verdict,
        Verdict::Valid
    );
    assert_eq!(
        inspect(&association, "9007199254740993").unwrap().verdict,
        Verdict::Incomplete
    );
    assert_eq!(
        inspect(&association, "{\"n\":1,\"n\":2}").unwrap().verdict,
        Verdict::Incomplete
    );
    let (_directory, association) = fixture(
        json!({"type":"array","items":{"properties":{"Value":{"description":"An actual object property"},"state":{"description":"Declared state"}}}}),
    );
    let report = inspect(
        &association,
        r#"[{"state":"done"},{"Value":"object"},17,["array"]]"#,
    )
    .unwrap();
    assert!(report
        .table_explanation(0, "state", false)
        .contains("JSON Pointer: /0/state"));
    assert!(report
        .table_explanation(1, "Value", false)
        .contains("An actual object property"));
    assert!(report
        .table_explanation(2, "Value", false)
        .contains("JSON Pointer: /2\n"));
    assert!(report
        .table_explanation(3, "Column 1", false)
        .contains("JSON Pointer: /3/0"));
    assert!(report
        .table_explanation(0, "state", true)
        .contains("example source row 0"));
}

#[test]
fn referenced_required_is_not_lost_by_sibling_property_metadata() {
    let (_directory, association) = fixture(
        json!({"$defs":{"required":{"required":["name"]}},"$ref":"#/$defs/required","properties":{"name":{"description":"Declared in sibling"}}}),
    );
    let report = inspect(&association, r#"{"name":"value"}"#).unwrap();
    assert!(report.fields["/name"].required);
    assert!(report.fields["/name"].property);
}

#[test]
fn infile_schema_local_relative_path_and_project_config_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "schemas/local.schema.json",
        schema(json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "$schema": { "type": "string" },
                "id": { "type": "string", "title": "Record ID", "description": "Declared in local schema" }
            }
        })),
    );
    write(
        root,
        "schemas/override.schema.json",
        schema(json!({
            "type": "object",
            "properties": {
                "$schema": { "type": "string" },
                "id": { "type": "integer", "title": "Numeric ID" }
            }
        })),
    );
    let doc_text = r#"{"$schema":"../schemas/local.schema.json","id":"abc"}"#;
    write(
        root,
        "items/one.json",
        serde_json::from_str(doc_text).unwrap(),
    );

    // Without .okilum/viewers.json, in-file relative "$schema" resolves safely inside root.
    let association = associate(root, &root.join("items/one.json")).unwrap();
    assert!(association.schema.is_none());
    let report = inspect(&association, doc_text).unwrap();
    assert_eq!(report.verdict, Verdict::Valid);
    assert_eq!(report.fields["/id"].title.as_deref(), Some("Record ID"));
    assert_eq!(
        report.fields["/id"].description.as_deref(),
        Some("Declared in local schema")
    );

    // Explicit .okilum/viewers.json schema rule takes precedence over in-file "$schema".
    write(
        root,
        ".okilum/viewers.json",
        json!({
            "version": 1,
            "schemas": [
                { "pattern": "items/*.json", "schema": "schemas/override.schema.json" }
            ]
        }),
    );
    let association = associate(root, &root.join("items/one.json")).unwrap();
    let report = inspect(&association, doc_text).unwrap();
    assert_eq!(report.verdict, Verdict::Invalid);
    assert_eq!(report.fields["/id"].title.as_deref(), Some("Numeric ID"));

    // Escaping, remote, absolute, and missing in-file "$schema" fall back honestly.
    std::fs::remove_file(root.join(".okilum/viewers.json")).unwrap();
    let association = associate(root, &root.join("items/one.json")).unwrap();
    for bad in [
        r#"{"$schema":"../../outside.schema.json"}"#,
        r#"{"$schema":"/etc/passwd"}"#,
        r#"{"$schema":"https://example.invalid/schema.json"}"#,
        r#"{"$schema":"./missing.schema.json"}"#,
    ] {
        let report = inspect(&association, bad).unwrap();
        assert_eq!(report.verdict, Verdict::Unavailable, "{bad}");
    }
    // Meta-schema URI on a schema file itself is not treated as an unavailable instance schema.
    assert!(inspect(
        &association,
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#
    )
    .is_none());
}

#[test]
fn definitions_and_relative_parent_refs_resolve_safely() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "shared/types.schema.json",
        schema(json!({
            "definitions": {
                "status": {
                    "type": "string",
                    "title": "Job status",
                    "description": "Shared status definition",
                    "enum": ["open", "closed"]
                }
            }
        })),
    );
    write(
        root,
        "nested/main.schema.json",
        schema(json!({
            "type": "object",
            "required": ["status"],
            "properties": {
                "status": { "$ref": "../shared/types.schema.json#/definitions/status" }
            }
        })),
    );
    let association = Association {
        project: root.to_path_buf(),
        schema: Some(root.join("nested/main.schema.json")),
        ..Default::default()
    };
    let report = inspect(&association, r#"{"status":"open"}"#).unwrap();
    assert_eq!(report.verdict, Verdict::Valid);
    assert_eq!(
        report.fields["/status"].title.as_deref(),
        Some("Job status")
    );
    assert_eq!(
        report.fields["/status"].description.as_deref(),
        Some("Shared status definition")
    );
    assert!(report.fields["/status"].required);

    // Parent ref escaping the project root is refused.
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "escape.schema.json", schema(json!({})));
    write(
        root,
        "nested/escape.schema.json",
        schema(json!({
            "$ref": "../../escape.schema.json"
        })),
    );
    let escaped = Association {
        project: root.to_path_buf(),
        schema: Some(root.join("nested/escape.schema.json")),
        ..Default::default()
    };
    assert_eq!(
        inspect(&escaped, "{}").unwrap().verdict,
        Verdict::Unavailable
    );
}

#[test]
fn composition_all_of_any_of_one_of_basic_handling() {
    // allOf merges constraints and field metadata from every branch.
    let (_dir, all_of) = fixture(json!({
        "allOf": [
            {
                "type": "object",
                "required": ["a"],
                "properties": { "a": { "type": "string", "description": "First field" } }
            },
            {
                "type": "object",
                "required": ["b"],
                "properties": { "b": { "type": "integer", "description": "Second field" } }
            }
        ]
    }));
    let valid = inspect(&all_of, r#"{"a":"hi","b":3}"#).unwrap();
    assert_eq!(valid.verdict, Verdict::Valid);
    assert_eq!(
        valid.fields["/a"].description.as_deref(),
        Some("First field")
    );
    assert_eq!(
        valid.fields["/b"].description.as_deref(),
        Some("Second field")
    );
    let invalid = inspect(&all_of, r#"{"a":"hi"}"#).unwrap();
    assert_eq!(invalid.verdict, Verdict::Invalid);
    assert!(invalid.violations.iter().any(|v| v.path == "/b"));

    // anyOf passes when at least one branch matches and fails when none match.
    let (_dir, any_of) = fixture(json!({
        "anyOf": [
            { "type": "string", "description": "Text form" },
            { "type": "integer", "description": "Numeric form" }
        ]
    }));
    let ok = inspect(&any_of, "42").unwrap();
    assert_eq!(ok.verdict, Verdict::Valid);
    assert_eq!(ok.fields[""].description.as_deref(), Some("Numeric form"));
    let bad = inspect(&any_of, "true").unwrap();
    assert_eq!(bad.verdict, Verdict::Invalid);
    assert!(bad.violations[0].message.contains("anyOf"));

    // oneOf passes only when exactly one branch matches.
    let (_dir, one_of) = fixture(json!({
        "oneOf": [
            { "type": "integer", "minimum": 0, "description": "Non-negative" },
            { "type": "integer", "maximum": 10, "description": "At most ten" }
        ]
    }));
    assert_eq!(inspect(&one_of, "-2").unwrap().verdict, Verdict::Valid);
    assert_eq!(inspect(&one_of, "20").unwrap().verdict, Verdict::Valid);
    let both = inspect(&one_of, "5").unwrap();
    assert_eq!(both.verdict, Verdict::Invalid);
    assert!(both.violations[0].message.contains("more than one"));
    let neither = inspect(&one_of, "\"text\"").unwrap();
    assert_eq!(neither.verdict, Verdict::Invalid);
    assert!(neither.violations[0].message.contains("oneOf"));
}
