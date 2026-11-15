use super::view_tests::settle;
use super::*;
use ::core::prelude::v1::test;
use gpui_component::WindowExt as _;
use okilum_core::json_schema::Verdict;

fn project(root: &Path) {
    for directory in [".okilum", "jobs", "schemas"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    for (path, content) in [
        (
            ".okilum/viewers.json",
            include_str!("../../../../docs/fixtures/json-schema-project/.okilum/viewers.json"),
        ),
        (
            "jobs/demo.samplejob",
            include_str!("../../../../docs/fixtures/json-schema-project/jobs/demo.samplejob"),
        ),
        (
            "jobs/general.samplejob",
            include_str!("../../../../docs/fixtures/json-schema-project/jobs/general.samplejob"),
        ),
        (
            "schemas/job.schema.json",
            include_str!("../../../../docs/fixtures/json-schema-project/schemas/job.schema.json"),
        ),
        (
            "schemas/general.schema.json",
            include_str!(
                "../../../../docs/fixtures/json-schema-project/schemas/general.schema.json"
            ),
        ),
        (
            "schemas/types.schema.json",
            include_str!("../../../../docs/fixtures/json-schema-project/schemas/types.schema.json"),
        ),
    ] {
        std::fs::write(root.join(path), content).unwrap();
    }
}
fn open(reader: &Entity<Reader>, rel: &str, visual: &mut VisualTestContext) -> Entity<CodePreview> {
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file(rel, window, cx)
    });
    settle(visual);
    reader.read_with(visual, |reader, _| {
        reader
            .file_preview
            .as_ref()
            .unwrap()
            .code
            .clone()
            .expect("explicit project JSON association routed the custom extension")
    })
}

#[gpui::test]
fn schema2994_custom_extension_uses_existing_views_metadata_source_edit_and_undo(
    cx: &mut TestAppContext,
) {
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "source.rs");
    let root = temp.path().join("vault");
    project(&root);
    let source = std::fs::read(root.join("jobs/demo.samplejob")).unwrap();
    let code = open(&reader, "jobs/demo.samplejob", visual);
    code.update_in(visual, |code, window, cx| {
        assert_eq!(code.source_language(), "json");
        assert!(code.json_routed);
        let report = code
            .schema_report
            .as_ref()
            .expect("real schema worker completed");
        assert_eq!(report.verdict, Verdict::Invalid);
        assert_eq!(report.violations.len(), 4);
        assert!(report
            .schema
            .as_ref()
            .unwrap()
            .ends_with("schemas/job.schema.json"));
        assert_eq!(report.fields["/name"].title.as_deref(), Some("Job name"));
        assert!(report.fields["/owner"].missing);
        assert!(code
            .schema_rows
            .iter()
            .any(|row| row.contains("/steps/0/state") && row.contains("Processing state")));
        assert!(code
            .schema_rows
            .iter()
            .any(|row| row.contains("/custom") && row.contains("No schema field declaration")));
        for view in [View::Raw, View::Formatted, View::Tree] {
            assert!(code.availability(view).available());
        }
        code.set_view(View::Tree, window, cx);
        code.structure.all(false, code.document.as_ref().unwrap());
        code.show_schema(window, cx);
    });
    settle(visual);
    visual.update(|window, cx| window.close_dialog(cx));
    code.update_in(visual, |code, window, cx| {
        code.set_view(View::Raw, window, cx)
    });
    settle(visual);
    code.read_with(visual, |code, cx| {
        assert_eq!(code.input().unwrap().read(cx).value().as_bytes(), source);
        assert!(!code.input().unwrap().read(cx).is_editable());
    });
    reader.update_in(visual, |reader, window, cx| {
        assert!(reader.file_is_source());
        assert!(reader.code_file_editable(cx));
        reader.toggle_source(window, cx);
        let input = reader
            .source_input()
            .expect("explicit Edit opens custom JSON source");
        input.update(cx, |input, cx| {
            input.set_selected_range(0..0, cx);
            gpui::EntityInputHandler::replace_text_in_range(input, Some(0..0), " ", window, cx);
        });
        assert_ne!(
            input.read(cx).value().as_bytes(),
            source,
            "real edit positive control"
        );
        input.update(cx, |input, cx| input.focus(window, cx));
    });
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    settle(visual);
    reader.read_with(visual, |reader, cx| {
        assert_eq!(
            reader.source_input().unwrap().read(cx).value().as_bytes(),
            source
        )
    });
    assert_eq!(
        std::fs::read(root.join("jobs/demo.samplejob")).unwrap(),
        source,
        "schema never writes source"
    );
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    settle(visual);
}

#[gpui::test]
fn schema2994_custom_array_keeps_table_and_schema_action_and_broken_schema_falls_back(
    cx: &mut TestAppContext,
) {
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "source.rs");
    let root = temp.path().join("vault");
    project(&root);
    let source = r#"[{"state":"queued","attempts":2},{"state":"bad","attempts":1}]"#;
    std::fs::write(root.join("jobs/demo.samplejob"), source).unwrap();
    std::fs::write(root.join("schemas/job.schema.json"), r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"array","items":{"$ref":"types.schema.json#/$defs/step"}}"##).unwrap();
    let code = open(&reader, "jobs/demo.samplejob", visual);
    code.update_in(visual, |code, window, cx| {
        assert_eq!(code.view, View::Table);
        assert!(code.table.as_ref().unwrap().read(cx).ready());
        assert_eq!(
            code.schema_report.as_ref().unwrap().verdict,
            Verdict::Invalid
        );
        let entity = cx.entity();
        assert!(code
            .chrome(entity, cx)
            .actions
            .iter()
            .any(|action| action.id == "code-schema"));
        for view in View::ALL {
            assert!(code.availability(view).available());
        }
        code.set_view(View::Tree, window, cx);
        code.set_view(View::Formatted, window, cx);
    });
    settle(visual);
    std::fs::write(root.join("schemas/job.schema.json"), "{broken").unwrap();
    code.update_in(visual, |code, window, cx| {
        code.load_schema(true, window, cx)
    });
    settle(visual);
    code.update_in(visual, |code, window, cx| {
        assert_eq!(
            code.schema_report.as_ref().unwrap().verdict,
            Verdict::Unavailable
        );
        for view in View::ALL {
            assert!(code.availability(view).available());
        }
        code.set_view(View::Raw, window, cx);
    });
    settle(visual);
    code.read_with(visual, |code, cx| {
        assert_eq!(code.input().unwrap().read(cx).value().as_ref(), source)
    });
    assert_eq!(
        std::fs::read_to_string(root.join("jobs/demo.samplejob")).unwrap(),
        source
    );
}

#[gpui::test]
fn schema2994_stale_refresh_cannot_replace_newer_snapshot_or_selection(cx: &mut TestAppContext) {
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "source.rs");
    let root = temp.path().join("vault");
    project(&root);
    let code = open(&reader, "jobs/demo.samplejob", visual);
    assert!(
        code.read_with(visual, |code, _| code.schema_report.is_some()),
        "positive worker control"
    );
    let (release, gate) = futures::channel::oneshot::channel();
    code.update_in(visual, |code, window, cx| {
        code.schema_gate = Some(gate);
        code.load_schema(true, window, cx);
    });
    settle(visual);
    assert!(code.read_with(visual, |code, _| code.schema_loading));
    code.update_in(visual, |code, window, cx| {
        code.load_schema(true, window, cx)
    });
    settle(visual);
    let current = code.read_with(visual, |code, _| {
        code.schema_report.clone().expect("newer worker fired")
    });
    release.send(()).unwrap();
    settle(visual);
    code.read_with(visual, |code, _| {
        assert!(
            Arc::ptr_eq(code.schema_report.as_ref().unwrap(), &current),
            "late worker cannot overwrite newer result"
        )
    });
    // Old preview may remain alive in a panel/observer; its result still belongs
    // only to that entity, never the next selected file.
    let (release, gate) = futures::channel::oneshot::channel();
    code.update_in(visual, |code, window, cx| {
        code.schema_gate = Some(gate);
        code.load_schema(true, window, cx);
    });
    let next = open(&reader, "jobs/general.samplejob", visual);
    assert_eq!(
        next.read_with(visual, |code, _| code
            .schema_report
            .as_ref()
            .unwrap()
            .verdict),
        Verdict::Valid
    );
    release.send(()).unwrap();
    settle(visual);
    reader.read_with(visual, |reader, _| {
        assert_eq!(
            reader.file_preview.as_ref().unwrap().code.as_ref(),
            Some(&next)
        )
    });
    assert_eq!(
        next.read_with(visual, |code, _| code
            .schema_report
            .as_ref()
            .unwrap()
            .verdict),
        Verdict::Valid
    );
}

#[test]
fn schema2994_json_pointer_projection_preserves_escaped_keys_and_array_indices() {
    let doc = Document::new(r#"{"a/b~c":[{"x\"y":true}]}"#.into(), "json");
    assert_eq!(
        doc.json_pointers(),
        ["", "/a~1b~0c", "/a~1b~0c/0", "/a~1b~0c/0/x\"y"]
    );
}
