//! Session rules on an in-memory store. Real files and the note FileEditor are
//! covered in `tests/excalidraw_write.rs`, where the plugin oracle sees them.
use super::*;
use anyhow::bail;
use serde_json::json;

const BOARD: &str = include_str!("../../../tests/fixtures/excalidraw/board-json.excalidraw.md");

#[derive(Default)]
struct MemoryStore {
    disk: String,
    /// What the store last read or wrote.
    base: String,
    draft: String,
    saves: usize,
    /// The file changes outside while the draft is staged.
    race_on_stage: Option<String>,
    fail_stage: bool,
    fail_save: bool,
}

impl DrawingStore for MemoryStore {
    fn current(&self) -> Result<String> {
        Ok(self.disk.clone())
    }
    fn stage(&mut self, text: String) -> Result<()> {
        if self.fail_stage {
            bail!("Injected recovery failure");
        }
        self.draft = text;
        if let Some(outside) = self.race_on_stage.take() {
            self.disk = outside;
        }
        Ok(())
    }
    fn save(&mut self) -> Result<Save> {
        if self.fail_save {
            bail!("Injected save failure");
        }
        if self.disk != self.base {
            return Ok(Save::Conflict);
        }
        self.saves += 1;
        self.disk = self.draft.clone();
        self.base = self.disk.clone();
        Ok(Save::Saved)
    }
    fn keep_mine(&mut self, reviewed: &str) -> Result<Save> {
        if self.disk != reviewed {
            return Ok(Save::Conflict);
        }
        self.base = reviewed.to_owned();
        self.save()
    }
    fn reload(&mut self) -> Result<String> {
        self.base = self.disk.clone();
        self.draft = self.disk.clone();
        Ok(self.disk.clone())
    }
}

struct Fixture {
    session: DrawingEdit<MemoryStore>,
    _root: tempfile::TempDir,
}

fn session_for(name: &str, file: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let canonical = root.path().canonicalize().unwrap();
    let store = MemoryStore {
        disk: file.to_owned(),
        base: file.to_owned(),
        draft: file.to_owned(),
        ..Default::default()
    };
    let session =
        DrawingEdit::with_store(store, &canonical, &canonical.join(name), file.to_owned()).unwrap();
    Fixture {
        session,
        _root: root,
    }
}

fn plain() -> Fixture {
    let file = json!({"type":"excalidraw","version":2,"elements":[{
        "id":"shape001","type":"rectangle","x":0,"y":0,
        "width":10,"height":10,"version":1,"extension":{"keep":true}
    }]})
    .to_string();
    session_for("shape.excalidraw", &file)
}

/// The editor's view after load: upstream drops fields it does not know.
fn load(session: &mut DrawingEdit<MemoryStore>) -> Value {
    let mut scene = session.editor_scene().unwrap();
    for element in elements_mut(&mut scene) {
        let fields = element.as_object_mut().unwrap();
        fields.remove("rawText");
        fields.remove("extension");
    }
    session.loaded(session.revision(), scene.clone()).unwrap();
    scene
}

fn moved(scene: &Value, index: usize, x: i64) -> Value {
    let mut scene = scene.clone();
    scene["elements"][index]["x"] = json!(x);
    scene["elements"][index]["version"] = json!(99);
    scene
}

#[test]
fn no_op_apply_writes_nothing_and_an_edit_saves_exact_prepared_bytes() {
    let mut fixture = plain();
    let session = &mut fixture.session;
    let original = session.opened().to_owned();
    let baseline = load(session);
    assert_eq!(
        session.apply(0, &baseline).unwrap(),
        Applied::Unchanged { revision: 0 }
    );
    assert_eq!(session.store.saves, 0);
    assert_eq!(session.store.disk, original);
    assert!(!session.can_undo());

    let edited = moved(&baseline, 0, 30);
    let WriteOutcome::Updated(prepared) = session.prepare(&edited).unwrap() else {
        panic!("positive control: a move is an edit");
    };
    assert_eq!(
        session.apply(0, &edited).unwrap(),
        Applied::Saved { revision: 1 }
    );
    assert_eq!(session.store.disk, prepared);
    let saved = Scene::parse(&session.store.disk).unwrap().json;
    assert_eq!(saved["elements"][0]["x"], 30);
    assert_eq!(saved["elements"][0]["extension"], json!({"keep": true}));
    assert_eq!(session.opened(), prepared);
}

#[test]
fn stale_revisions_and_double_loads_are_refused() {
    let mut fixture = plain();
    let session = &mut fixture.session;
    let baseline = load(session);
    assert!(session.loaded(0, baseline.clone()).is_err());
    assert!(session.apply(1, &moved(&baseline, 0, 5)).is_err());
    assert_eq!(session.store.saves, 0);
    assert_eq!(
        session.apply(0, &moved(&baseline, 0, 5)).unwrap(),
        Applied::Saved { revision: 1 }
    );
    assert!(session.apply(0, &moved(&baseline, 0, 6)).is_err());
    assert!(session.undo(0).is_err());
}

#[test]
fn outside_change_is_a_conflict_that_writes_nothing_and_keeps_the_edit() {
    let mut fixture = plain();
    let session = &mut fixture.session;
    let baseline = load(session);
    let outside = session.opened().replace("\"x\":0", "\"x\":7");
    session.store.disk = outside.clone();
    let edited = moved(&baseline, 0, 30);
    assert_eq!(
        session.apply(0, &edited).unwrap(),
        Applied::Conflict {
            disk: outside.clone()
        }
    );
    assert_eq!(session.store.disk, outside, "conflict writes nothing");
    assert_eq!(session.store.saves, 0);
    assert!(session.store.draft.contains("30"), "edit kept in recovery");
    assert_eq!(session.revision(), 0);
    assert_eq!(session.conflict(), Some(outside.as_str()));
    assert!(!session.can_undo());
    // No second Apply or Undo until the user chooses.
    assert!(session.apply(0, &edited).is_err());
    assert!(session.undo(0).is_err());
}

#[test]
fn race_at_publication_is_a_conflict() {
    let mut fixture = plain();
    let session = &mut fixture.session;
    let baseline = load(session);
    session.store.race_on_stage = Some("racing outside version".into());
    assert_eq!(
        session.apply(0, &moved(&baseline, 0, 30)).unwrap(),
        Applied::Conflict {
            disk: "racing outside version".into()
        }
    );
    assert_eq!(session.store.disk, "racing outside version");
    assert_eq!(session.revision(), 0);
}

#[test]
fn keep_mine_writes_the_edit_over_the_reviewed_version_and_undo_restores_it() {
    let mut fixture = plain();
    let session = &mut fixture.session;
    let baseline = load(session);
    let outside = session.opened().replace("\"x\":0", "\"x\":7");
    session.store.disk = outside.clone();
    let edited = moved(&baseline, 0, 30);
    let WriteOutcome::Updated(mine) = session.prepare(&edited).unwrap() else {
        panic!("an edit");
    };
    assert!(matches!(
        session.apply(0, &edited).unwrap(),
        Applied::Conflict { .. }
    ));

    // The file changes again before the choice: a new conflict, no write.
    let again = outside.replace("\"x\":7", "\"x\":8");
    session.store.disk = again.clone();
    assert_eq!(
        session.keep_mine(0, &outside).unwrap(),
        Applied::Conflict {
            disk: again.clone()
        }
    );
    assert_eq!(session.store.disk, again);
    // A repeated Keep mine that reviewed only the first outside version
    // cannot overwrite the second one.
    assert_eq!(
        session.keep_mine(0, &outside).unwrap(),
        Applied::Conflict {
            disk: again.clone()
        }
    );
    assert_eq!(session.store.disk, again);
    assert_eq!(session.store.saves, 0);

    assert_eq!(
        session.keep_mine(0, &again).unwrap(),
        Applied::Saved { revision: 1 }
    );
    assert_eq!(session.store.disk, mine);
    assert_eq!(session.conflict(), None);
    assert!(session.can_undo());
    // The editor continues from its own scene.
    assert_eq!(
        session.apply(1, &edited).unwrap(),
        Applied::Unchanged { revision: 1 }
    );
    assert_eq!(session.undo(1).unwrap(), Undone::Restored { revision: 2 });
    assert_eq!(
        session.store.disk, again,
        "Undo restores what Keep mine replaced"
    );
}

#[test]
fn reload_drops_the_edit_and_needs_a_fresh_load() {
    let mut fixture = plain();
    let session = &mut fixture.session;
    let baseline = load(session);
    let outside = session.opened().replace("\"x\":0", "\"x\":7");
    session.store.disk = outside.clone();
    let edited = moved(&baseline, 0, 30);
    assert!(matches!(
        session.apply(0, &edited).unwrap(),
        Applied::Conflict { .. }
    ));
    assert!(session.reload(1).is_err(), "stale revision");
    assert_eq!(session.reload(0).unwrap(), 1);
    assert_eq!(session.store.disk, outside);
    assert_eq!(session.opened(), outside);
    assert!(!session.store.draft.contains("30"), "the edit is dropped");
    assert_eq!(session.conflict(), None);
    assert_eq!(session.editor_scene().unwrap()["elements"][0]["x"], 7);
    assert!(
        session.apply(1, &edited).is_err(),
        "Reload needs a new load"
    );
    let baseline = load(session);
    assert_eq!(
        session.apply(1, &moved(&baseline, 0, 40)).unwrap(),
        Applied::Saved { revision: 2 }
    );
}

#[test]
fn save_as_copy_keeps_the_edit_beside_an_untouched_original() {
    let mut fixture = session_for("dir/Board.excalidraw.md", BOARD);
    let session = &mut fixture.session;
    std::fs::create_dir_all(session.root.join("dir")).unwrap();
    let baseline = load(session);
    let outside = BOARD.replace("Back of the note", "Changed elsewhere");
    session.store.disk = outside.clone();
    let edited = moved(&baseline, 0, 30);
    let WriteOutcome::Updated(mine) = session.prepare(&edited).unwrap() else {
        panic!("an edit");
    };
    assert!(matches!(
        session.apply(0, &edited).unwrap(),
        Applied::Conflict { .. }
    ));
    let copy = session.save_as_copy(0).unwrap();
    let name = copy.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("Board.okilum-conflict-"), "{name}");
    assert!(name.ends_with(".excalidraw.md"), "{name}");
    assert_eq!(copy.parent(), Some(Path::new("dir")));
    assert!(crate::excalidraw::is_drawing(name));
    assert_eq!(
        std::fs::read_to_string(session.root.join(&copy)).unwrap(),
        mine
    );
    assert_eq!(session.store.disk, outside, "the original is not touched");
    // Repeating the choice does not pile up copies; the conflict stays open.
    assert_eq!(session.save_as_copy(0).unwrap(), copy);
    assert_eq!(
        std::fs::read_dir(session.root.join("dir")).unwrap().count(),
        1
    );
    assert_eq!(session.conflict(), Some(outside.as_str()));
    assert_eq!(session.reload(0).unwrap(), 1);
}

#[test]
fn conflict_copies_stay_drawings() {
    let name = |rel: &str| conflict_copy(Path::new(rel)).to_string_lossy().into_owned();
    let plain = name("a/Plan.excalidraw");
    assert!(plain.starts_with("a/Plan.okilum-conflict-") && plain.ends_with(".excalidraw"));
    let upper = name("Plan.Excalidraw.MD");
    assert!(upper.starts_with("Plan.okilum-conflict-") && upper.ends_with(".Excalidraw.MD"));
    let synced = name("Plan.sync-conflict-1.excalidraw.md");
    assert!(!synced.contains(".sync-conflict-"), "{synced}");
}

#[test]
fn undo_is_single_exact_and_never_over_an_outside_change() {
    let mut fixture = plain();
    let session = &mut fixture.session;
    let original = session.opened().to_owned();
    let baseline = load(session);
    assert!(session.undo(0).is_err(), "nothing to undo yet");
    assert_eq!(
        session.apply(0, &moved(&baseline, 0, 30)).unwrap(),
        Applied::Saved { revision: 1 }
    );
    let saved = session.store.disk.clone();
    session.store.disk = "outside".into();
    assert_eq!(
        session.undo(1).unwrap(),
        Undone::Blocked {
            disk: "outside".into()
        }
    );
    assert_eq!(session.store.disk, "outside");
    assert!(session.can_undo());
    session.store.disk = saved;
    assert_eq!(session.undo(1).unwrap(), Undone::Restored { revision: 2 });
    assert_eq!(session.store.disk, original, "exact bytes");
    assert!(!session.can_undo());
    assert!(session.undo(2).is_err(), "one Undo");
    assert!(
        session.apply(2, &moved(&baseline, 0, 30)).is_err(),
        "Undo needs a new load"
    );
}

#[test]
fn failed_recovery_or_write_changes_neither_file_nor_session() {
    for fail_stage in [true, false] {
        let mut fixture = plain();
        let session = &mut fixture.session;
        let original = session.opened().to_owned();
        let baseline = load(session);
        session.store.fail_stage = fail_stage;
        session.store.fail_save = !fail_stage;
        assert!(session.apply(0, &moved(&baseline, 0, 42)).is_err());
        assert_eq!(session.store.disk, original);
        assert_eq!(session.opened(), original);
        assert_eq!(session.revision(), 0);
        assert!(!session.can_undo());
        assert_eq!(session.conflict(), None);
        if !fail_stage {
            assert!(
                session.store.draft.contains("42"),
                "recovery keeps the edit"
            );
        }
    }
}

#[test]
fn linked_text_is_edited_as_its_source_and_kept_resolved_when_untouched() {
    let mut fixture = session_for("Board.excalidraw.md", BOARD);
    let session = &mut fixture.session;
    let scene = session.editor_scene().unwrap();
    let linked = elements(&scene).find(|e| e["id"] == "Tx2kLmN4").unwrap();
    assert_eq!(linked["text"], "See [[Project Plan|the plan]]");
    assert_eq!(linked["originalText"], "See [[Project Plan|the plan]]");
    let plain_text = elements(&scene).find(|e| e["id"] == "Tx1aBcD3").unwrap();
    assert_eq!(plain_text["text"], "Inbox", "unlinked text unchanged");

    let baseline = load(session);
    assert_eq!(
        session.prepare(&baseline).unwrap(),
        WriteOutcome::Unchanged,
        "showing the source is not an edit"
    );
    let index = elements(&baseline)
        .position(|e| e["id"] == "Tx2kLmN4")
        .unwrap();
    let WriteOutcome::Updated(saved) = session.prepare(&moved(&baseline, index, 40)).unwrap()
    else {
        panic!("a move is an edit");
    };
    let saved = Scene::parse(&saved).unwrap().json;
    let text = elements(&saved).find(|e| e["id"] == "Tx2kLmN4").unwrap();
    assert_eq!(text["x"], 40);
    assert_eq!(text["text"], "See the plan");
    assert_eq!(text["originalText"], "See the plan");
    assert_eq!(text["rawText"], "See [[Project Plan|the plan]]");
}

#[test]
fn images_that_would_save_blank_or_lose_data_are_refused() {
    let file = json!({"type":"excalidraw","version":2,
        "elements":[{"id":"image001","type":"image","x":0,"y":0,"width":10,"height":10,
            "version":1,"fileId":"old"}],
        "files":{"old":{"id":"old","mimeType":"image/png","dataURL":"data:image/png;base64,AA=="}}})
    .to_string();
    let mut fixture = session_for("images.excalidraw", &file);
    let session = &mut fixture.session;
    let baseline = load(session);
    let mut image = baseline["elements"][0].clone();
    image["id"] = json!("image002");
    image["fileId"] = json!("new");
    let mut added = baseline.clone();
    added["elements"].as_array_mut().unwrap().push(image);
    assert!(session.prepare(&added).is_err(), "new image without data");
    added["files"]["new"] = json!({"id":"new","dataURL":"data:image/svg+xml;base64,AA=="});
    assert!(session.prepare(&added).is_err(), "SVG is not saved inline");
    added["files"]["new"] = json!({"id":"new","dataURL":"data:image/png;base64,BB=="});
    assert!(matches!(
        session.prepare(&added).unwrap(),
        WriteOutcome::Updated(_)
    ));
    let mut replaced = baseline.clone();
    replaced["files"]["old"]["dataURL"] = json!("data:image/png;base64,CC==");
    assert!(session.prepare(&replaced).is_err(), "new data, old id");
    let mut loading = baseline;
    loading["elements"][0]
        .as_object_mut()
        .unwrap()
        .remove("fileId");
    assert!(session.prepare(&loading).is_err(), "still loading");
}

#[test]
fn vault_images_are_shown_but_never_written_and_formulas_do_not_block() {
    let compressed = include_str!("../../../tests/fixtures/excalidraw/board.excalidraw.md");
    let mut fixture = session_for("Board.excalidraw.md", compressed);
    std::fs::create_dir_all(fixture.session.root.join("assets")).unwrap();
    std::fs::write(
        fixture.session.root.join("assets/diagram.png"),
        b"\x89PNG fake",
    )
    .unwrap();
    // Reload re-reads the vault images.
    assert_eq!(fixture.session.reload(0).unwrap(), 1);
    let session = &mut fixture.session;
    let image = "5f4c2a0d9b8e7f6a1c3b5d7e9f0a2c4e6b8d0f1a";
    let scene = session.editor_scene().unwrap();
    assert!(scene["files"][image]["dataURL"]
        .as_str()
        .unwrap()
        .starts_with("data:image/png;base64,"));
    let baseline = load(session);
    let edited = moved(&baseline, 0, 77);
    assert_eq!(
        session.apply(1, &edited).unwrap(),
        Applied::Saved { revision: 2 }
    );
    let saved = Scene::parse(&session.store.disk).unwrap().json;
    assert!(
        saved["files"].get(image).is_none(),
        "display data stays out"
    );
    let section = compressed.find("## Drawing").unwrap();
    assert_eq!(
        &session.store.disk[..section],
        &compressed[..section],
        "Markdown, Text Elements and Embedded Files unchanged"
    );
}

#[test]
fn undo_shows_the_vault_images_of_the_restored_file() {
    use base64::Engine as _;
    let compressed = include_str!("../../../tests/fixtures/excalidraw/board.excalidraw.md");
    let mut fixture = session_for("Board.excalidraw.md", compressed);
    let root = fixture.session.root.clone();
    std::fs::create_dir_all(root.join("assets")).unwrap();
    std::fs::write(root.join("assets/diagram.png"), b"\x89PNG image a").unwrap();
    std::fs::write(root.join("assets/other.png"), b"\x89PNG image b").unwrap();
    assert_eq!(fixture.session.reload(0).unwrap(), 1);
    let session = &mut fixture.session;
    let image = "5f4c2a0d9b8e7f6a1c3b5d7e9f0a2c4e6b8d0f1a";
    let data = |bytes: &[u8]| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    };
    let shown = |session: &DrawingEdit<MemoryStore>| {
        session.editor_scene().unwrap()["files"][image]["dataURL"].clone()
    };
    assert_eq!(shown(session), data(b"\x89PNG image a"));

    // Outside, the same file id now embeds another image.
    let outside = compressed.replace("[[assets/diagram.png]]", "[[assets/other.png]]");
    assert_ne!(outside, compressed);
    session.store.disk = outside.clone();
    let baseline = load(session);
    assert!(matches!(
        session.apply(1, &moved(&baseline, 0, 77)).unwrap(),
        Applied::Conflict { .. }
    ));
    assert_eq!(
        session.keep_mine(1, &outside).unwrap(),
        Applied::Saved { revision: 2 }
    );
    assert_eq!(session.undo(2).unwrap(), Undone::Restored { revision: 3 });
    assert_eq!(session.store.disk, outside);
    assert_eq!(shown(session), data(b"\x89PNG image b"));
}
