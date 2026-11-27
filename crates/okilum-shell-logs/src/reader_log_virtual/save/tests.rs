use super::*;
use ::core::prelude::v1::test;
use okilum_core::source_history;
use std::{fs, path::PathBuf};

const ORIGINAL: &str =
    "{\r\n  \"version\":1, \"name\":\"Ch\\u0065ckout\", \"glob\":\"logs/*\"\r\n}\r\n";
struct Fixture {
    _dir: tempfile::TempDir,
    vault: PathBuf,
    state: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let path = vault.join("original.okilog");
        fs::write(&path, ORIGINAL).unwrap();
        let state = dir.path().join("state");
        Self {
            _dir: dir,
            vault,
            state,
            path,
        }
    }
    fn open(&self) -> (Session, Definition) {
        let loaded = model::load(&self.vault, &self.path).unwrap();
        (
            Session::open(&loaded, &self.state).unwrap(),
            loaded.definition,
        )
    }
    fn history(&self) -> source_history::Listing {
        source_history::list(&self.state.join("editor-drafts"), &self.vault).unwrap()
    }
}

#[test]
fn canonical_save_noop_preserves_exact_bytes_stamp_and_history() {
    let f = Fixture::new();
    let (mut session, definition) = f.open();
    let stamp = fs::metadata(&f.path).unwrap().modified().unwrap();
    assert_eq!(
        session.save(&f.vault, &definition).unwrap(),
        Outcome::Unchanged
    );
    assert_eq!(fs::read(&f.path).unwrap(), ORIGINAL.as_bytes());
    assert_eq!(fs::metadata(&f.path).unwrap().modified().unwrap(), stamp);
    assert!(f.history().versions.is_empty());
    assert!(!session.editor.dirty());
    assert!(!f
        .state
        .join("editor-drafts")
        .read_dir()
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|e| e == "json")));
}

#[test]
fn canonical_save_changed_original_reopens_and_records_exact_preimage() {
    let f = Fixture::new();
    fs::write(f.vault.join("other.okilog"), ORIGINAL).unwrap();
    fs::write(f.vault.join("home.md"), "# Untouched\n").unwrap();
    let (mut session, mut definition) = f.open();
    definition.name = "Edited criteria".into();
    definition.query = Some("level>=warn".into());
    assert_eq!(session.save(&f.vault, &definition).unwrap(), Outcome::Saved);
    assert_eq!(
        model::load(&f.vault, &f.path).unwrap().definition.name,
        "Edited criteria"
    );
    assert!(!session.editor.dirty());
    let history = f.history();
    assert!(history.warnings.is_empty(), "{:?}", history.warnings);
    assert!(history
        .versions
        .iter()
        .any(|v| v.note == f.path && v.bytes() == ORIGINAL.as_bytes()));
    assert_eq!(
        fs::read_to_string(f.vault.join("other.okilog")).unwrap(),
        ORIGINAL
    );
    assert_eq!(
        fs::read_to_string(f.vault.join("home.md")).unwrap(),
        "# Untouched\n"
    );
    drop(session);
    let (mut reopened, definition) = f.open();
    let bytes = fs::read(&f.path).unwrap();
    assert_eq!(
        reopened.save(&f.vault, &definition).unwrap(),
        Outcome::Unchanged
    );
    assert_eq!(fs::read(&f.path).unwrap(), bytes);
}

#[test]
fn canonical_save_external_valid_or_malformed_changes_retain_draft_and_disk() {
    for external in [
        r#"{"version":1,"name":"External","glob":"logs/**"}"#,
        "{broken",
    ] {
        let f = Fixture::new();
        let (mut session, mut definition) = f.open();
        definition.name = "My draft".into();
        fs::write(&f.path, external).unwrap();
        assert_eq!(
            session.save(&f.vault, &definition).unwrap(),
            Outcome::Conflict
        );
        assert_eq!(fs::read_to_string(&f.path).unwrap(), external);
        assert_eq!(
            Definition::parse(session.editor.text()).unwrap().name,
            "My draft"
        );
        assert!(session.editor.dirty());
        drop(session);
        let recovered = FileEditor::open(&f.path, &f.state.join("editor-drafts")).unwrap();
        assert_eq!(
            Definition::parse(recovered.text()).unwrap().name,
            "My draft"
        );
        assert!(recovered.dirty());
        assert!(f.history().versions.is_empty());
    }
}

#[test]
fn canonical_save_deleted_original_never_recreates_and_keeps_recovery() {
    let f = Fixture::new();
    let (mut session, mut definition) = f.open();
    definition.name = "Recover me".into();
    fs::remove_file(&f.path).unwrap();
    assert!(session.save(&f.vault, &definition).is_err());
    assert!(!f.path.exists());
    assert!(session.editor.dirty());
    let draft = session.editor.text().to_owned();
    assert_eq!(Definition::parse(&draft).unwrap().name, "Recover me");
    assert!(f
        .state
        .join("editor-drafts")
        .read_dir()
        .unwrap()
        .any(|entry| {
            let path = entry.unwrap().path();
            path.extension().is_some_and(|e| e == "json")
                && fs::read_to_string(path).unwrap().contains("Recover me")
        }));
}

#[test]
fn canonical_save_invalid_criteria_do_not_publish_and_recovered_draft_is_not_overwritten() {
    let f = Fixture::new();
    let (mut session, mut definition) = f.open();
    for glob in ["../*.log", "[", ""] {
        definition.glob = glob.into();
        assert!(session.save(&f.vault, &definition).is_err());
        assert_eq!(fs::read_to_string(&f.path).unwrap(), ORIGINAL);
        assert_eq!(Definition::parse(session.editor.text()).unwrap().glob, glob);
    }
    definition.glob = "logs/*".into();
    definition.query = Some("(".into());
    assert!(session.save(&f.vault, &definition).is_err());
    assert_eq!(fs::read_to_string(&f.path).unwrap(), ORIGINAL);
    let draft = session.editor.text().to_owned();
    drop(session);
    let loaded = model::load(&f.vault, &f.path).unwrap();
    assert!(Session::open(&loaded, &f.state).is_err());
    let recovered = FileEditor::open(&f.path, &f.state.join("editor-drafts")).unwrap();
    assert_eq!(recovered.text(), draft);
    assert!(recovered.dirty());
}

#[test]
fn canonical_save_stale_load_and_noop_conflict_do_not_silently_accept_external_bytes() {
    let f = Fixture::new();
    let loaded = model::load(&f.vault, &f.path).unwrap();
    fs::write(&f.path, ORIGINAL.replace("logs/*", "other/*")).unwrap();
    assert!(Session::open(&loaded, &f.state).is_err());
    fs::write(&f.path, ORIGINAL).unwrap();
    let (mut session, definition) = f.open();
    fs::write(&f.path, "malformed external content").unwrap();
    assert_eq!(
        session.save(&f.vault, &definition).unwrap(),
        Outcome::Conflict
    );
    assert_eq!(
        fs::read_to_string(&f.path).unwrap(),
        "malformed external content"
    );
}

#[test]
fn local_writer_review_virtual_log_refusal_allows_retry_and_reopen() {
    let f = Fixture::new();
    let (mut session, mut definition) = f.open();
    definition.name = "Held criteria".into();
    let a = okilum_write_authority::Authority::for_root(&f.vault).unwrap();
    let hold = a.begin_drain("virtual-log-refusal").unwrap();
    assert!(session.save(&f.vault, &definition).is_err());
    assert!(!session.review_required);
    assert!(!session.editor.dirty());
    assert_eq!(fs::read_to_string(&f.path).unwrap(), ORIGINAL);
    let token = hold.token().clone();
    hold.release(&token).unwrap();
    assert_eq!(session.save(&f.vault, &definition).unwrap(), Outcome::Saved);
    drop(session);
    let (mut session, mut definition) = f.open();
    definition.name = "Retry after reopening".into();
    assert_eq!(session.save(&f.vault, &definition).unwrap(), Outcome::Saved);
}
