//! Round trips through the Excalidraw writer (#478) on Obsidian plugin files.
use okilum_core::excalidraw::{write, Scene, WriteOutcome};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const COMPRESSED: &str = include_str!("fixtures/excalidraw/board.excalidraw.md");
const PLAIN_JSON: &str = include_str!("fixtures/excalidraw/board-json.excalidraw.md");
const MINIMAL: &str = include_str!("fixtures/excalidraw/elements.excalidraw.md");
const PLAIN_FILE: &str = include_str!("fixtures/excalidraw/elements.excalidraw");

fn variants() -> Vec<(&'static str, String)> {
    vec![
        ("compressed", COMPRESSED.to_owned()),
        ("json", PLAIN_JSON.to_owned()),
        ("crlf", COMPRESSED.replace('\n', "\r\n")),
        ("bom", format!("\u{feff}{PLAIN_JSON}")),
    ]
}

fn scene(file: &str) -> Value {
    Scene::parse(file).unwrap().json
}

/// What upstream Excalidraw hands back after loading: `rawText` is deleted.
fn restored(file: &str) -> Value {
    let mut scene = scene(file);
    for element in scene["elements"].as_array_mut().unwrap() {
        element.as_object_mut().unwrap().remove("rawText");
    }
    scene
}

fn element<'a>(scene: &'a mut Value, id: &str) -> &'a mut Value {
    scene["elements"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|element| element["id"] == id)
        .unwrap_or_else(|| panic!("no element {id}"))
}

fn find<'a>(scene: &'a Value, id: &str) -> &'a Value {
    scene["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| element["id"] == id)
        .unwrap_or_else(|| panic!("no element {id}"))
}

/// An edit as upstream makes it: the element's version moves forward.
fn edit(scene: &mut Value, id: &str, change: impl FnOnce(&mut Value)) {
    let element = element(scene, id);
    change(element);
    element["version"] = json!(element["version"].as_i64().unwrap_or(0) + 1);
}

fn set_text(scene: &mut Value, id: &str, text: &str) {
    edit(scene, id, |element| {
        element["text"] = json!(text);
        element["originalText"] = json!(text);
    });
}

fn updated(outcome: WriteOutcome) -> String {
    match outcome {
        WriteOutcome::Updated(text) => text,
        WriteOutcome::Unchanged => panic!("expected a write"),
    }
}

/// The file with the Drawing fence body cut out, and that body.
fn split_drawing(file: &str) -> (String, String) {
    let open = file
        .find("```compressed-json")
        .or_else(|| file.find("```json"))
        .unwrap();
    let start = open + file[open..].find('\n').unwrap() + 1;
    let end = start + file[start..].find("```").unwrap();
    (
        format!("{}<DRAWING>{}", &file[..start], &file[end..]),
        file[start..end].to_owned(),
    )
}

/// The Obsidian plugin's `## Text Elements` reader (`ExcalidrawData.loadData`):
/// block refs match `\s\^(.{8})[\n]+`, and each next text starts 12 bytes
/// (" ^12345678\n\n") after the previous match.
fn plugin_text_elements(file: &str) -> BTreeMap<String, String> {
    let header = "## Text Elements\n";
    let Some(start) = file.find(header).map(|at| at + header.len()) else {
        return BTreeMap::new();
    };
    let end = ["## Element Links", "## Embedded Files", "## Drawing"]
        .iter()
        .filter_map(|marker| file[start..].find(marker))
        .min()
        .map_or(file.len(), |at| start + at);
    let mut position = start;
    let mut entries = BTreeMap::new();
    for entry in Regex::new(r"\s\^(.{8})\n+")
        .unwrap()
        .captures_iter(&file[start..end])
    {
        let at = start + entry.get(0).unwrap().start();
        assert!(
            entries
                .insert(entry[1].to_owned(), file[position..at].to_owned())
                .is_none(),
            "duplicate entry {}",
            &entry[1]
        );
        position = at + 12;
    }
    entries
}

/// `## Element Links` as the plugin reads it: `<8-char id>: [[link]]` lines.
fn plugin_element_links(file: &str) -> BTreeMap<String, String> {
    let Some(start) = file.find("## Element Links\n") else {
        return BTreeMap::new();
    };
    let section = &file[start..];
    let end = ["## Embedded Files", "%%", "## Drawing"]
        .iter()
        .filter_map(|marker| section.find(marker))
        .min()
        .unwrap();
    Regex::new(r"(?m)^(.{8}): (\[\[[^\]]*\]\])$")
        .unwrap()
        .captures_iter(&section[..end])
        .map(|entry| (entry[1].to_owned(), entry[2].to_owned()))
        .collect()
}

/// With `OKILUM_EXCALIDRAW_DUMP=<dir>`, keep every checked file and its scene
/// facts for the plugin's own loader (scripts/excalidraw-compat, CI job
/// excalidraw-compat): block-ref text entries, every element link, all ids.
fn dump_for_plugin_oracle(file: &str, text: &BTreeMap<String, String>, scene: &Value) {
    use std::hash::{Hash, Hasher};
    let Some(dir) = std::env::var_os("OKILUM_EXCALIDRAW_DUMP") else {
        return;
    };
    let elements = scene["elements"].as_array().unwrap();
    let links: BTreeMap<&str, &str> = elements
        .iter()
        .filter(|element| element["isDeleted"] != true)
        .filter_map(|element| Some((element["id"].as_str()?, element["link"].as_str()?)))
        .collect();
    let ids: Vec<&str> = elements
        .iter()
        .filter_map(|element| element["id"].as_str())
        .collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    file.hash(&mut hasher);
    let name = format!("{:016x}", hasher.finish());
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{name}.excalidraw.md")), file).unwrap();
    let expected = json!({ "text": text, "links": links, "ids": ids });
    std::fs::write(
        dir.join(format!("{name}.expected.json")),
        expected.to_string(),
    )
    .unwrap();
}

/// What a plugin load of `file` must see: one Markdown entry per live text
/// element with a block-ref id, equal to its `rawText`, and wiki links listed.
fn assert_plugin_consistent(file: &str) {
    let lf = file.replace("\r\n", "\n");
    let scene = scene(file);
    let live = || {
        scene["elements"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|element| element["isDeleted"] != true)
            .filter(|element| element["id"].as_str().unwrap().len() == 8)
    };
    let expected_text: BTreeMap<String, String> = live()
        .filter(|element| element["type"] == "text")
        .map(|element| {
            (
                element["id"].as_str().unwrap().to_owned(),
                element["rawText"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(plugin_text_elements(&lf), expected_text, "{lf}");
    let expected_links: BTreeMap<String, String> = live()
        .filter_map(|element| {
            let link = element["link"].as_str()?;
            link.starts_with("[[")
                .then(|| (element["id"].as_str().unwrap().to_owned(), link.to_owned()))
        })
        .collect();
    dump_for_plugin_oracle(file, &expected_text, &scene);
    assert_eq!(plugin_element_links(&lf), expected_links, "{lf}");
}

#[test]
fn fixtures_are_what_the_plugin_reads() {
    for (name, file) in variants() {
        assert_plugin_consistent(&file);
        assert_eq!(scene(&file), scene(COMPRESSED), "{name}");
    }
}

#[test]
fn no_op_save_writes_nothing_and_an_edit_writes() {
    let mut cases = variants();
    cases.push(("minimal", MINIMAL.to_owned()));
    cases.push(("plain file", PLAIN_FILE.to_owned()));
    for (name, file) in cases {
        let baseline = restored(&file);
        let outcome = write(&file, Some(&baseline), &baseline).unwrap();
        assert_eq!(outcome, WriteOutcome::Unchanged, "{name}");
        assert_eq!(outcome.contents(&file), file, "{name}");
        // Without a baseline, the dropped `rawText` alone is not a change.
        assert_eq!(
            write(&file, None, &baseline).unwrap(),
            WriteOutcome::Unchanged,
            "{name}"
        );
        // Positive control: the same harness detects a one-pixel move.
        let mut edited = baseline.clone();
        let id = edited["elements"][0]["id"].as_str().unwrap().to_owned();
        edit(&mut edited, &id, |element| {
            element["x"] = json!(element["x"].as_f64().unwrap() + 1.)
        });
        let saved = updated(write(&file, Some(&baseline), &edited).unwrap());
        assert_ne!(saved, file, "{name}");
        assert_eq!(
            find(&scene(&saved), &id)["x"],
            json!(edited["elements"][0]["x"]),
            "{name}"
        );
    }
}

#[test]
fn editing_one_text_changes_only_its_entry_and_the_drawing() {
    for (name, file) in variants() {
        let baseline = restored(&file);
        let mut edited = baseline.clone();
        set_text(&mut edited, "Tx3pQrS5", "Line one\nLine 2, edited");
        let saved = updated(write(&file, Some(&baseline), &edited).unwrap());

        let nl = if name == "crlf" { "\r\n" } else { "\n" };
        let (before, _) = split_drawing(&file);
        let (after, drawing) = split_drawing(&saved);
        let old_entry = format!("Line one{nl}Line two ^Tx3pQrS5");
        let new_entry = format!("Line one{nl}Line 2, edited ^Tx3pQrS5");
        assert_eq!(before.matches(&old_entry).count(), 1, "{name}");
        assert_eq!(after, before.replace(&old_entry, &new_entry), "{name}");
        if name == "crlf" {
            assert!(!saved.replace("\r\n", "").contains('\n'), "bare LF");
        }
        if name == "compressed" || name == "crlf" {
            // The plugin's chunking: 256-character lines separated by a blank line.
            let chunks: Vec<&str> = drawing.split(&format!("{nl}{nl}")).collect();
            assert!(chunks.len() > 1);
            assert!(chunks[..chunks.len() - 1].iter().all(|c| c.len() == 256));
            assert!(!drawing.contains('{'));
        } else {
            assert!(drawing.starts_with("{\n\t\"type\": \"excalidraw\""));
        }

        let original = scene(&file);
        let saved_scene = scene(&saved);
        let text = find(&saved_scene, "Tx3pQrS5");
        assert_eq!(text["rawText"], "Line one\nLine 2, edited");
        assert_eq!(text["originalText"], "Line one\nLine 2, edited");
        assert!(text["version"].as_i64() > find(&original, "Tx3pQrS5")["version"].as_i64());
        // Every other element is the file's own object, `rawText` included.
        let others = |scene: &Value| -> Vec<Value> {
            scene["elements"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|element| element["id"] != "Tx3pQrS5")
                .cloned()
                .collect()
        };
        assert_eq!(others(&saved_scene), others(&original), "{name}");
        assert_eq!(
            find(&saved_scene, "Tx2kLmN4")["rawText"],
            "See [[Project Plan|the plan]]"
        );
        assert_eq!(saved_scene["appState"], original["appState"]);
        assert_eq!(saved_scene["source"], original["source"]);
        assert_plugin_consistent(&saved);
        // Saving the saved file again with its own baseline is a no-op.
        assert_eq!(
            write(&saved, Some(&restored(&saved)), &restored(&saved)).unwrap(),
            WriteOutcome::Unchanged
        );
    }
}

#[test]
fn moving_linked_text_keeps_its_markdown_source() {
    let baseline = restored(COMPRESSED);
    let mut edited = baseline.clone();
    edit(&mut edited, "Tx2kLmN4", |element| element["x"] = json!(40));
    let saved = updated(write(COMPRESSED, Some(&baseline), &edited).unwrap());
    assert_eq!(split_drawing(&saved).0, split_drawing(COMPRESSED).0);
    let text = find(&scene(&saved), "Tx2kLmN4").clone();
    assert_eq!(text["rawText"], "See [[Project Plan|the plan]]");
    assert_eq!(text["x"], 40);
}

#[test]
fn added_elements_get_entries_the_plugin_can_read() {
    for (name, file) in variants() {
        let baseline = restored(&file);
        let mut edited = baseline.clone();
        let mut text = find(&baseline, "Tx3pQrS5").clone();
        text["id"] = json!("NewText1");
        text["text"] = json!("Fresh idea");
        text["originalText"] = json!("Fresh idea");
        text["version"] = json!(1);
        let mut upstream = text.clone();
        upstream["id"] = json!("aB3dE5gH7jK9mN1pQ3sT5");
        upstream["text"] = json!("From the browser");
        upstream["originalText"] = json!("From the browser");
        let mut shape = find(&baseline, "Rr1aBcD3").clone();
        shape["id"] = json!("NewRect1");
        shape["boundElements"] = Value::Null;
        shape["link"] = json!("[[New Note]]");
        let elements = edited["elements"].as_array_mut().unwrap();
        elements.extend([text, upstream, shape]);
        let saved = updated(write(&file, Some(&baseline), &edited).unwrap());

        let lf = saved.replace("\r\n", "\n");
        assert!(
            lf.contains("Line two ^Tx3pQrS5\n\nFresh idea ^NewText1\n\n## Element Links\n"),
            "{name}: {lf}"
        );
        assert!(
            lf.contains("El1wXyZ6: [[Daily Note]]\n\nNewRect1: [[New Note]]\n\n## Embedded Files"),
            "{name}"
        );
        // A 21-character upstream id cannot be a block ref: no entry, and the
        // plugin adopts the element from `rawText` in the JSON.
        assert!(
            !split_drawing(&lf).0.contains("aB3dE5gH7jK9mN1pQ3sT5"),
            "{name}"
        );
        let saved_scene = scene(&saved);
        assert_eq!(
            find(&saved_scene, "aB3dE5gH7jK9mN1pQ3sT5")["rawText"],
            "From the browser"
        );
        assert_eq!(find(&saved_scene, "NewText1")["rawText"], "Fresh idea");
        assert_plugin_consistent(&saved);
    }
}

#[test]
fn deleted_elements_become_tombstones_and_lose_their_entries() {
    for (name, file) in variants() {
        let baseline = restored(&file);
        let mut edited = baseline.clone();
        // One deletion as an upstream tombstone, two dropped from the array.
        edit(&mut edited, "Tx2kLmN4", |element| {
            element["isDeleted"] = json!(true)
        });
        edited["elements"]
            .as_array_mut()
            .unwrap()
            .retain(|element| element["id"] != "Tx1aBcD3" && element["id"] != "El1wXyZ6");
        // The file's own tombstone is also absent from the editor's scene.
        edited["elements"]
            .as_array_mut()
            .unwrap()
            .retain(|element| element["id"] != "TxDeadOn");
        let saved = updated(write(&file, Some(&baseline), &edited).unwrap());

        let lf = saved.replace("\r\n", "\n");
        assert!(
            lf.contains("## Text Elements\nLine one\nLine two ^Tx3pQrS5\n\n## Element Links\n## Embedded Files"),
            "{name}: {lf}"
        );
        let original = scene(&file);
        let saved_scene = scene(&saved);
        for id in ["Tx1aBcD3", "Tx2kLmN4", "El1wXyZ6"] {
            let tombstone = find(&saved_scene, id);
            assert_eq!(tombstone["isDeleted"], true, "{id}");
            assert!(tombstone["version"].as_i64() > find(&original, id)["version"].as_i64());
        }
        assert_eq!(find(&saved_scene, "TxDeadOn"), find(&original, "TxDeadOn"));
        assert_eq!(
            saved_scene["elements"].as_array().unwrap().len(),
            original["elements"].as_array().unwrap().len()
        );
        assert_plugin_consistent(&saved);
        // The reader no longer draws them.
        let drawn: BTreeSet<String> = Scene::parse(&saved)
            .unwrap()
            .elements
            .into_iter()
            .map(|element| element.id)
            .collect();
        let expected = ["Ar1aBcD3", "Im1gHjK7", "Im2LaTeX", "Rr1aBcD3", "Tx3pQrS5"];
        assert_eq!(drawn, expected.map(String::from).into(), "{name}");
    }
}

#[test]
fn link_changes_rewrite_only_that_link() {
    let baseline = restored(COMPRESSED);
    let mut edited = baseline.clone();
    edit(&mut edited, "El1wXyZ6", |element| {
        element["link"] = json!("[[Weekly Note]]")
    });
    let saved = updated(write(COMPRESSED, Some(&baseline), &edited).unwrap());
    assert_eq!(
        split_drawing(&saved).0,
        split_drawing(COMPRESSED)
            .0
            .replace("El1wXyZ6: [[Daily Note]]", "El1wXyZ6: [[Weekly Note]]")
    );
    // A URL is not a Markdown link: the entry goes, or it would win on load.
    let mut edited = baseline.clone();
    edit(&mut edited, "El1wXyZ6", |element| {
        element["link"] = json!("https://example.com/board")
    });
    let saved = updated(write(COMPRESSED, Some(&baseline), &edited).unwrap());
    assert!(!saved.contains("El1wXyZ6:"));
    assert!(saved.contains("## Element Links\n## Embedded Files"));
    assert_eq!(
        find(&scene(&saved), "El1wXyZ6")["link"],
        "https://example.com/board"
    );
    assert_plugin_consistent(&saved);
}

#[test]
fn legacy_link_markers_in_text_elements_follow_the_link() {
    // Older plugin versions wrote a shape's link as a block-ref marker.
    let file = COMPRESSED
        .replace(
            "Line two ^Tx3pQrS5\n\n",
            "Line two ^Tx3pQrS5\n\n[[Daily Note]] ^El1wXyZ6\n\n",
        )
        .replace("El1wXyZ6: [[Daily Note]]\n\n", "");
    let baseline = restored(&file);
    assert_eq!(
        write(&file, Some(&baseline), &baseline).unwrap(),
        WriteOutcome::Unchanged
    );
    let mut edited = baseline.clone();
    edit(&mut edited, "El1wXyZ6", |element| {
        element["link"] = json!("[[Weekly Note]]")
    });
    let saved = updated(write(&file, Some(&baseline), &edited).unwrap());
    assert_eq!(
        split_drawing(&saved).0,
        split_drawing(&file).0.replace(
            "[[Daily Note]] ^El1wXyZ6\n\n## Element Links\n",
            "## Element Links\nEl1wXyZ6: [[Weekly Note]]\n\n"
        )
    );
    assert_plugin_consistent(&saved);
}

#[test]
fn vault_files_stay_out_of_the_json_and_new_files_are_kept() {
    let baseline = restored(COMPRESSED);
    let mut edited = baseline.clone();
    // The bridge inlines vault images for display; a pasted image is new.
    let file = |id: &str| json!({"mimeType": "image/png", "id": id, "dataURL": "data:image/png;base64,iVBORw0KGgo=", "created": 1});
    edited["files"] = json!({
        "5f4c2a0d9b8e7f6a1c3b5d7e9f0a2c4e6b8d0f1a": file("5f4c2a0d9b8e7f6a1c3b5d7e9f0a2c4e6b8d0f1a"),
        "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d": file("9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d"),
    });
    assert_eq!(
        write(COMPRESSED, Some(&baseline), &edited).unwrap(),
        WriteOutcome::Unchanged
    );
    edited["files"]["pasted01"] = file("pasted01");
    let saved = updated(write(COMPRESSED, Some(&baseline), &edited).unwrap());
    assert_eq!(
        scene(&saved)["files"],
        json!({"pasted01": file("pasted01")})
    );
    assert_eq!(split_drawing(&saved).0, split_drawing(COMPRESSED).0);
}

#[test]
fn only_user_app_state_is_written() {
    let baseline = restored(COMPRESSED);
    let mut edited = baseline.clone();
    edited["appState"]["theme"] = json!("dark");
    edited["appState"]["zoom"] = json!({"value": 2});
    assert_eq!(
        write(COMPRESSED, Some(&baseline), &edited).unwrap(),
        WriteOutcome::Unchanged
    );
    edited["appState"]["viewBackgroundColor"] = json!("#fff9db");
    let saved = scene(&updated(
        write(COMPRESSED, Some(&baseline), &edited).unwrap(),
    ));
    let mut expected = scene(COMPRESSED)["appState"].clone();
    expected["viewBackgroundColor"] = json!("#fff9db");
    assert_eq!(saved["appState"], expected);
}

#[test]
fn missing_text_section_is_created_before_embedded_files() {
    let baseline = restored(MINIMAL);
    let mut edited = baseline.clone();
    let mut text = find(&baseline, "element-6").clone();
    assert_eq!(text["type"], "text");
    text["id"] = json!("AddedTx1");
    text["text"] = json!("Added");
    text["originalText"] = json!("Added");
    edited["elements"].as_array_mut().unwrap().push(text);
    let saved = updated(write(MINIMAL, Some(&baseline), &edited).unwrap());
    assert!(saved.starts_with(
        "---\nexcalidraw-plugin: parsed\n---\n## Text Elements\nAdded ^AddedTx1\n\n## Embedded Files\n"
    ));
    assert_plugin_consistent(&saved);
}

#[test]
fn plain_excalidraw_files_keep_bom_and_unknown_keys() {
    let mut original = scene(PLAIN_FILE);
    original["okilumUnknown"] = json!({"kept": true});
    let file = format!(
        "\u{feff}{}\n",
        serde_json::to_string_pretty(&original).unwrap()
    );
    let baseline = scene(&file);
    let mut edited = baseline.clone();
    edited.as_object_mut().unwrap().remove("okilumUnknown");
    edit(&mut edited, "element-0", |element| {
        element["width"] = json!(120)
    });
    let saved = updated(write(&file, Some(&baseline), &edited).unwrap());
    assert!(saved.starts_with("\u{feff}{\n  \"type\": \"excalidraw\""));
    assert!(saved.ends_with("}\n"));
    let saved = scene(&saved);
    assert_eq!(saved["okilumUnknown"], json!({"kept": true}));
    assert_eq!(find(&saved, "element-0")["width"], 120);
    // Plain files carry no plugin `rawText`.
    assert!(find(&saved, "element-6").get("rawText").is_none());
}

/// Upstream Excalidraw rebuilds every element from the fields it knows, so
/// plugin and newer-format fields vanish on load. Editing such an element must
/// not delete them, while a field the editor did load can still be removed.
#[test]
fn edited_elements_keep_fields_the_editor_never_loaded() {
    let extra = json!({"nested": [1, 2, 3], "flag": true});
    let mut cases = variants();
    cases.push(("minimal", MINIMAL.to_owned()));
    for (name, file) in cases {
        // A file whose shape and text carry a field upstream does not know.
        let mut seeded = scene(&file);
        let ids: Vec<String> = ["rectangle", "text"]
            .iter()
            .map(|kind| {
                seeded["elements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|element| element["type"] == *kind && element["isDeleted"] != true)
                    .unwrap_or_else(|| panic!("{name}: no {kind}"))["id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let (shape, text) = (&ids[0], &ids[1]);
        for id in &ids {
            element(&mut seeded, id)["pluginOnly"] = extra.clone();
        }
        element(&mut seeded, shape)["link"] = json!("[[Daily Note]]");
        let file = updated(write(&file, None, &seeded).unwrap());
        assert_eq!(find(&scene(&file), shape)["pluginOnly"], extra, "{name}");

        // The editor drops `rawText` and the unknown field on load.
        let mut baseline = restored(&file);
        for id in &ids {
            element(&mut baseline, id)
                .as_object_mut()
                .unwrap()
                .remove("pluginOnly");
        }
        let mut edited = baseline.clone();
        edit(&mut edited, shape, |element| {
            element["x"] = json!(element["x"].as_f64().unwrap() + 30.);
            // A field the editor loaded is removed on purpose.
            element.as_object_mut().unwrap().remove("link");
        });
        set_text(&mut edited, text, "Edited text");
        let saved = updated(write(&file, Some(&baseline), &edited).unwrap());
        let reread = scene(&saved);
        for id in &ids {
            assert_eq!(find(&reread, id)["pluginOnly"], extra, "{name} {id}");
        }
        assert_eq!(
            find(&reread, shape)["x"],
            find(&edited, shape)["x"],
            "{name}"
        );
        assert!(find(&reread, shape).get("link").is_none(), "{name}");
        assert_eq!(find(&reread, text)["rawText"], "Edited text", "{name}");
        assert_plugin_consistent(&saved);
        // Re-saving the result unchanged writes nothing.
        assert_eq!(
            write(&saved, None, &restored(&saved)).unwrap(),
            WriteOutcome::Unchanged,
            "{name}"
        );
    }
}

/// `rawText` derives from the text, so an edited text never inherits the old
/// one, even in a plain file whose editor dropped it on load.
#[test]
fn edited_plain_text_does_not_inherit_a_stale_raw_text() {
    let mut original = scene(PLAIN_FILE);
    element(&mut original, "element-6")["rawText"] = json!("Old [[link]]");
    let file = serde_json::to_string_pretty(&original).unwrap();
    let mut baseline = scene(&file);
    element(&mut baseline, "element-6")
        .as_object_mut()
        .unwrap()
        .remove("rawText");
    let mut edited = baseline.clone();
    set_text(&mut edited, "element-6", "New text");
    let saved = scene(&updated(write(&file, Some(&baseline), &edited).unwrap()));
    assert_eq!(find(&saved, "element-6")["text"], "New text");
    assert!(find(&saved, "element-6").get("rawText").is_none());
}

#[test]
fn invalid_edits_are_rejected_without_output() {
    let baseline = restored(COMPRESSED);
    let mut duplicate = baseline.clone();
    let first = duplicate["elements"][0].clone();
    duplicate["elements"].as_array_mut().unwrap().push(first);
    let mut broken = baseline.clone();
    edit(&mut broken, "Rr1aBcD3", |element| {
        element["width"] = json!(-5)
    });
    for edited in [
        json!({"type": "other", "elements": []}),
        json!({"type": "excalidraw"}),
        duplicate,
        // Geometry the reader refuses is never written.
        broken,
    ] {
        assert!(write(COMPRESSED, Some(&baseline), &edited).is_err());
    }
    assert!(write("not a drawing", None, &baseline).is_err());
}

/// Seeded random edit sequences: every save stays plugin-consistent, keeps the
/// bytes outside the edited sections, and re-saving it unchanged is a no-op.
#[test]
fn random_edit_sequences_round_trip() {
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let invariant_head = |file: &str| file[..file.find("# Excalidraw Data").unwrap()].to_owned();
    let embedded = |file: &str| {
        let start = file.find("## Embedded Files").unwrap();
        file[start..start + file[start..].find("## Drawing").unwrap()].to_owned()
    };
    for (name, start) in variants() {
        let mut file = start.clone();
        for step in 0..60 {
            let baseline = restored(&file);
            let mut edited = baseline.clone();
            for _ in 0..1 + next(3) {
                let live: Vec<(String, String)> = edited["elements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|element| element["isDeleted"] != true)
                    .map(|element| {
                        (
                            element["id"].as_str().unwrap().to_owned(),
                            element["type"].as_str().unwrap().to_owned(),
                        )
                    })
                    .collect();
                let pick = |n: usize, text: bool| {
                    live.iter()
                        .filter(|(_, kind)| (kind == "text") == text)
                        .nth(n)
                        .map(|(id, _)| id.clone())
                };
                match next(6) {
                    0 => {
                        if let Some((id, _)) = live.get(next(live.len().max(1))) {
                            edit(&mut edited, id, |element| element["y"] = json!(step * 10));
                        }
                    }
                    1 => {
                        if let Some(id) = pick(next(4), true) {
                            let text = format!("Step {step}\nline {}", next(100));
                            set_text(&mut edited, &id, &text);
                        }
                    }
                    2 => {
                        if let Some((id, _)) = live.get(next(live.len().max(1))) {
                            if next(2) == 0 {
                                edit(&mut edited, id, |element| {
                                    element["isDeleted"] = json!(true)
                                });
                            } else {
                                let id = id.clone();
                                edited["elements"]
                                    .as_array_mut()
                                    .unwrap()
                                    .retain(|element| element["id"] != id);
                            }
                        }
                    }
                    3 => {
                        let mut text = find(&restored(COMPRESSED), "Tx3pQrS5").clone();
                        let id = if next(3) == 0 {
                            format!("upstream{step:05}xxxxxxxx")
                        } else {
                            format!("N{step:02}{:05}", next(100_000))
                        };
                        if edited["elements"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|e| e["id"] == id)
                        {
                            continue;
                        }
                        text["id"] = json!(id);
                        text["text"] = json!(format!("Added {step}"));
                        text["originalText"] = json!(format!("Added {step}"));
                        edited["elements"].as_array_mut().unwrap().push(text);
                    }
                    4 => {
                        if let Some(id) = pick(next(4), false) {
                            let link = match next(3) {
                                0 => json!(format!("[[Note {step}]]")),
                                1 => json!("https://example.com"),
                                _ => Value::Null,
                            };
                            edit(&mut edited, &id, |element| element["link"] = link);
                        }
                    }
                    _ => {
                        edited["appState"]["viewBackgroundColor"] =
                            json!(format!("#{:06x}", next(0xffffff)));
                    }
                }
            }
            let outcome = write(&file, Some(&baseline), &edited)
                .unwrap_or_else(|error| panic!("{name} step {step}: {error:#}"));
            let saved = outcome.contents(&file).to_owned();
            assert_eq!(
                invariant_head(&saved),
                invariant_head(&start),
                "{name} {step}"
            );
            assert_eq!(embedded(&saved), embedded(&start), "{name} {step}");
            assert!(saved.ends_with(&start[start.rfind("```").unwrap()..]));
            assert_plugin_consistent(&saved);
            assert_eq!(
                write(&saved, Some(&restored(&saved)), &restored(&saved)).unwrap(),
                WriteOutcome::Unchanged,
                "{name} {step}"
            );
            file = saved;
        }
        // Positive control: the sequence really edited the file.
        assert_ne!(file, start, "{name}");
    }
}

// Apply sessions (#478 slice 3a): the scene an editor hands back, written
// through the note FileEditor into a real file. Their output goes through
// `assert_plugin_consistent`, so the plugin oracle loads it too.
mod apply_session {
    use super::*;
    use okilum_core::excalidraw::edit::{Applied, DrawingEdit, Undone};
    use std::path::{Path, PathBuf};

    struct Vault {
        dir: tempfile::TempDir,
        file: PathBuf,
    }

    impl Vault {
        fn new(name: &str, contents: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join(name);
            std::fs::write(&file, contents).unwrap();
            Self { dir, file }
        }
        fn open(&self) -> anyhow::Result<DrawingEdit> {
            DrawingEdit::open(self.dir.path(), &self.file, &self.dir.path().join(".state"))
        }
        fn read(&self) -> String {
            std::fs::read_to_string(&self.file).unwrap()
        }
        fn root(&self) -> &Path {
            self.dir.path()
        }
    }

    /// Loads the session's scene as upstream Excalidraw would and reports it.
    fn load(session: &mut DrawingEdit) -> Value {
        let mut scene = session.editor_scene().unwrap();
        for element in scene["elements"].as_array_mut().unwrap() {
            element.as_object_mut().unwrap().remove("rawText");
        }
        session.loaded(session.revision(), scene.clone()).unwrap();
        scene
    }

    fn markdown(name: &str) -> bool {
        name != "plain file"
    }

    #[test]
    fn apply_writes_the_writer_bytes_and_undo_restores_the_file_exactly() {
        let mut cases: Vec<(&str, String)> = variants();
        cases.push(("minimal", MINIMAL.to_owned()));
        cases.push(("plain file", PLAIN_FILE.to_owned()));
        for (name, original) in cases {
            let file_name = if markdown(name) {
                "Board.excalidraw.md"
            } else {
                "Board.excalidraw"
            };
            let vault = Vault::new(file_name, &original);
            let mut session = vault.open().unwrap();
            let baseline = load(&mut session);
            assert_eq!(
                session.apply(0, &baseline).unwrap(),
                Applied::Unchanged { revision: 0 },
                "{name}"
            );
            assert_eq!(vault.read(), original, "{name}: no-op writes nothing");

            let mut edited = baseline.clone();
            let id = edited["elements"][0]["id"].as_str().unwrap().to_owned();
            edit(&mut edited, &id, |element| {
                element["x"] = json!(element["x"].as_f64().unwrap() + 25.)
            });
            let prepared = updated(session.prepare(&edited).unwrap());
            assert_eq!(
                session.apply(0, &edited).unwrap(),
                Applied::Saved { revision: 1 },
                "{name}"
            );
            let saved = vault.read();
            assert_eq!(saved, prepared, "{name}: exact prepared bytes");
            assert_eq!(find(&scene(&saved), &id)["x"], find(&edited, &id)["x"]);
            if markdown(name) {
                assert_plugin_consistent(&saved);
            }
            assert_eq!(
                session.undo(1).unwrap(),
                Undone::Restored { revision: 2 },
                "{name}"
            );
            assert_eq!(vault.read(), original, "{name}: Undo is byte-exact");
        }
    }

    #[test]
    fn links_in_text_stay_markdown_through_edits() {
        for (name, original) in variants() {
            let vault = Vault::new("Board.excalidraw.md", &original);
            let mut session = vault.open().unwrap();
            let shown = session.editor_scene().unwrap();
            assert_eq!(
                find(&shown, "Tx2kLmN4")["text"],
                "See [[Project Plan|the plan]]",
                "{name}: the editor gets the link source"
            );
            let baseline = load(&mut session);

            // Moving linked text keeps the file's text and the Markdown.
            let mut moved = baseline.clone();
            edit(&mut moved, "Tx2kLmN4", |element| element["y"] = json!(500));
            assert_eq!(
                session.apply(0, &moved).unwrap(),
                Applied::Saved { revision: 1 }
            );
            let saved = vault.read();
            assert_eq!(
                split_drawing(&saved).0,
                split_drawing(&original).0,
                "{name}"
            );
            let text = find(&scene(&saved), "Tx2kLmN4").clone();
            assert_eq!(text["text"], "See the plan", "{name}");
            assert_eq!(text["rawText"], "See [[Project Plan|the plan]]", "{name}");
            assert_plugin_consistent(&saved);

            // Typing in linked text writes the typed links as Markdown.
            let mut typed = moved.clone();
            set_text(
                &mut typed,
                "Tx2kLmN4",
                "See [[Project Plan|the plan]] and [[Budget]]",
            );
            assert_eq!(
                session.apply(1, &typed).unwrap(),
                Applied::Saved { revision: 2 }
            );
            let saved = vault.read();
            let entries = plugin_text_elements(&saved.replace("\r\n", "\n"));
            assert_eq!(
                entries["Tx2kLmN4"], "See [[Project Plan|the plan]] and [[Budget]]",
                "{name}"
            );
            assert_plugin_consistent(&saved);

            // Deleting the link text on purpose removes the link.
            let mut unlinked = typed.clone();
            set_text(&mut unlinked, "Tx2kLmN4", "See the plan");
            assert_eq!(
                session.apply(2, &unlinked).unwrap(),
                Applied::Saved { revision: 3 }
            );
            let saved = vault.read();
            assert_eq!(
                find(&scene(&saved), "Tx2kLmN4")["rawText"],
                "See the plan",
                "{name}"
            );
            assert_plugin_consistent(&saved);
        }
    }

    #[test]
    fn outside_change_conflict_resolves_like_notes() {
        let vault = Vault::new("Board.excalidraw.md", COMPRESSED);
        let mut session = vault.open().unwrap();
        let baseline = load(&mut session);
        let outside = COMPRESSED.replace("Back of the note", "Edited in Obsidian");
        assert_ne!(outside, COMPRESSED);
        std::fs::write(&vault.file, &outside).unwrap();

        let mut edited = baseline.clone();
        set_text(&mut edited, "Tx1aBcD3", "Inbox (sorted)");
        let mine = updated(session.prepare(&edited).unwrap());
        assert_eq!(
            session.apply(0, &edited).unwrap(),
            Applied::Conflict {
                disk: outside.clone()
            }
        );
        assert_eq!(vault.read(), outside, "a conflict writes nothing");

        // Save as copy: a second drawing with the edit; the original stays.
        let copy = session.save_as_copy(0).unwrap();
        let copy_text = std::fs::read_to_string(vault.root().join(&copy)).unwrap();
        assert_eq!(copy_text, mine);
        assert!(okilum_core::excalidraw::is_drawing(&copy.to_string_lossy()));
        assert_plugin_consistent(&copy_text);
        assert_eq!(vault.read(), outside);

        // Keep mine: the edit replaces the reviewed outside version.
        assert_eq!(
            session.keep_mine(0, &outside).unwrap(),
            Applied::Saved { revision: 1 }
        );
        assert_eq!(vault.read(), mine);
        assert_plugin_consistent(&mine);
        assert_eq!(session.undo(1).unwrap(), Undone::Restored { revision: 2 });
        assert_eq!(
            vault.read(),
            outside,
            "Undo brings the outside version back"
        );
    }

    #[test]
    fn reload_takes_the_outside_version_and_clears_recovery() {
        let vault = Vault::new("Board.excalidraw.md", PLAIN_JSON);
        let mut session = vault.open().unwrap();
        let baseline = load(&mut session);
        let outside = PLAIN_JSON.replace("Back of the note", "Edited in Obsidian");
        std::fs::write(&vault.file, &outside).unwrap();
        let mut edited = baseline;
        edit(&mut edited, "Rr1aBcD3", |element| element["x"] = json!(321));
        assert!(matches!(
            session.apply(0, &edited).unwrap(),
            Applied::Conflict { .. }
        ));
        assert_eq!(session.reload(0).unwrap(), 1);
        assert_eq!(session.opened(), outside);
        assert_eq!(vault.read(), outside);
        drop(session);
        // No recovery draft is left behind, so the drawing opens again.
        let mut session = vault.open().unwrap();
        assert_eq!(session.opened(), outside);
        let baseline = load(&mut session);
        let mut edited = baseline;
        edit(&mut edited, "Rr1aBcD3", |element| element["x"] = json!(321));
        assert_eq!(
            session.apply(0, &edited).unwrap(),
            Applied::Saved { revision: 1 }
        );
        assert!(vault.read().contains("Edited in Obsidian"));
    }

    #[test]
    fn an_unresolved_conflict_keeps_the_edit_in_recovery() {
        let vault = Vault::new("Board.excalidraw.md", PLAIN_JSON);
        let mut session = vault.open().unwrap();
        assert!(vault.open().is_err(), "one editor per drawing");
        let baseline = load(&mut session);
        let outside = PLAIN_JSON.replace("Back of the note", "Edited in Obsidian");
        std::fs::write(&vault.file, &outside).unwrap();
        let mut edited = baseline;
        edit(&mut edited, "Rr1aBcD3", |element| element["x"] = json!(321));
        assert!(matches!(
            session.apply(0, &edited).unwrap(),
            Applied::Conflict { .. }
        ));
        drop(session);
        let refused = vault
            .open()
            .err()
            .expect("recovery draft blocks a new edit");
        assert!(format!("{refused:#}").contains("recovery"), "{refused:#}");
        let draft = okilum_core::file_editor::FileEditor::unsaved_draft_text(
            &vault.file.canonicalize().unwrap(),
            &vault.root().join(".state"),
        )
        .unwrap()
        .expect("the edit is in recovery");
        assert_eq!(find(&scene(&draft), "Rr1aBcD3")["x"], 321);
        assert_eq!(vault.read(), outside);
    }
}
