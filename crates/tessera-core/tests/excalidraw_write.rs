//! Round trips through the Excalidraw writer (#478) on Obsidian plugin files.
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use tessera_core::excalidraw::{write, Scene, WriteOutcome};

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
    original["tesseraUnknown"] = json!({"kept": true});
    let file = format!(
        "\u{feff}{}\n",
        serde_json::to_string_pretty(&original).unwrap()
    );
    let baseline = scene(&file);
    let mut edited = baseline.clone();
    edited.as_object_mut().unwrap().remove("tesseraUnknown");
    edit(&mut edited, "element-0", |element| {
        element["width"] = json!(120)
    });
    let saved = updated(write(&file, Some(&baseline), &edited).unwrap());
    assert!(saved.starts_with("\u{feff}{\n  \"type\": \"excalidraw\""));
    assert!(saved.ends_with("}\n"));
    let saved = scene(&saved);
    assert_eq!(saved["tesseraUnknown"], json!({"kept": true}));
    assert_eq!(find(&saved, "element-0")["width"], 120);
    // Plain files carry no plugin `rawText`.
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
