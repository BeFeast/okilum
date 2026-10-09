#![cfg(all(unix, feature = "brain"))]
use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_core::source::*;
use std::fs;
use std::os::unix::fs::symlink;

struct Fixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    state: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        let state = temp.path().join("runtime");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&state).unwrap();
        Self {
            _temp: temp,
            root,
            state,
        }
    }
    fn store(&self, boundary: WriteBoundary) -> SourceStore {
        SourceStore::open(
            "01000000-0000-4000-8000-000000000001",
            &self.root,
            &self.state,
            boundary,
        )
        .unwrap()
    }
}
fn request(id: u32, path: &str, revision: Option<String>, bytes: &[u8]) -> SourceWrite {
    SourceWrite {
        schema: SCHEMA.into(),
        operation_id: format!("06000000-0000-4000-8000-{id:012}"),
        brain_id: "01000000-0000-4000-8000-000000000001".into(),
        path: path.into(),
        expected_revision: revision,
        content_base64: STANDARD.encode(bytes),
    }
}

#[test]
fn exact_bytes_survive_create_read_update_and_restart() {
    let f = Fixture::new();
    let store = f.store(WriteBoundary::Managed);
    let original = b"\xef\xbb\xbf---\r\ntype: Note\r\nunknown: retained\r\n---\r\n# Exact\r\n[[note|alias]]\r\n\xff\0";
    let created = store.write(request(1, "note.md", None, original)).unwrap();
    assert_eq!(fs::read(f.root.join("note.md")).unwrap(), original);
    let snapshot = store.read("note.md").unwrap();
    assert_eq!(STANDARD.decode(snapshot.content_base64).unwrap(), original);
    assert_eq!(snapshot.revision, created.revision);
    let updated = b"---\r\ntype: Note\r\n---\r\n[[other]]\r\n";
    store
        .write(request(2, "note.md", Some(created.revision), updated))
        .unwrap();
    drop(store);
    let store = f.store(WriteBoundary::Managed);
    assert_eq!(
        STANDARD
            .decode(store.read("note.md").unwrap().content_base64)
            .unwrap(),
        updated
    );
    assert_eq!(
        STANDARD
            .decode(
                store
                    .recovery_record(&request(2, "", None, b"").operation_id)
                    .unwrap()
                    .preimage_base64
                    .unwrap()
            )
            .unwrap(),
        original
    );
}

#[test]
fn known_sha256_and_missing_are_not_lossy_reader_results() {
    let f = Fixture::new();
    let s = f.store(WriteBoundary::Managed);
    fs::write(f.root.join("hello.md"), b"hello").unwrap();
    assert_eq!(
        s.read("hello.md").unwrap().revision,
        "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(s.read("missing.md").unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn stale_revision_and_create_collision_preserve_current_and_proposal() {
    let f = Fixture::new();
    let s = f.store(WriteBoundary::Managed);
    let receipt = s.write(request(1, "note.md", None, b"base")).unwrap();
    fs::write(f.root.join("note.md"), b"external edit").unwrap();
    let proposed = request(2, "note.md", Some(receipt.revision), b"my edit");
    let e = s.write(proposed.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    let id = e.conflict.unwrap().conflict_id;
    let record = s.recovery_record(&id).unwrap();
    assert_eq!(
        STANDARD.decode(record.preimage_base64.unwrap()).unwrap(),
        b"external edit"
    );
    assert_eq!(record.request, proposed);
    assert_eq!(fs::read(f.root.join("note.md")).unwrap(), b"external edit");
    assert_eq!(
        s.write(request(3, "note.md", None, b"replace"))
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // Positive control: explicit resolution against the actual current revision works.
    let revision = s.read("note.md").unwrap().revision;
    s.write(request(4, "note.md", Some(revision), b"resolved"))
        .unwrap();
    assert_eq!(fs::read(f.root.join("note.md")).unwrap(), b"resolved");
}

#[test]
fn unmanaged_writes_are_durable_proposals_even_with_current_revision() {
    let f = Fixture::new();
    fs::write(f.root.join("note.md"), b"original").unwrap();
    let s = f.store(WriteBoundary::Unmanaged);
    let req = request(
        1,
        "note.md",
        Some(s.read("note.md").unwrap().revision),
        b"proposal",
    );
    let e = s.write(req.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert_eq!(e.conflict.as_ref().unwrap().reason, "unmanaged_writers");
    assert_eq!(s.recovery_record(&req.operation_id).unwrap().request, req);
    assert_eq!(fs::read(f.root.join("note.md")).unwrap(), b"original");
    assert_eq!(
        s.write(request(2, "new.md", None, b"new"))
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert!(!f.root.join("new.md").exists());
}

#[test]
fn receipts_are_idempotent_across_restart_and_id_reuse_is_rejected() {
    let f = Fixture::new();
    let s = f.store(WriteBoundary::Managed);
    let req = request(1, "note.md", None, b"one");
    let receipt = s.write(req.clone()).unwrap();
    fs::write(f.root.join("note.md"), b"later external edit").unwrap();
    drop(s);
    let s = f.store(WriteBoundary::Managed);
    assert_eq!(s.write(req.clone()).unwrap(), receipt);
    assert_eq!(
        fs::read(f.root.join("note.md")).unwrap(),
        b"later external edit"
    );
    let mut reused = req;
    reused.content_base64 = STANDARD.encode(b"different");
    assert_eq!(s.write(reused).unwrap_err().code, ErrorCode::InvalidRequest);
    let current = s.read("note.md").unwrap();
    let unchanged = s
        .write(request(
            2,
            "note.md",
            Some(current.revision),
            b"later external edit",
        ))
        .unwrap();
    assert_eq!(unchanged.outcome, WriteOutcome::Unchanged);
}

#[test]
fn rejects_escape_symlinks_and_nonregular_sources_with_positive_control() {
    let f = Fixture::new();
    let s = f.store(WriteBoundary::Managed);
    fs::create_dir(f.root.join("notes")).unwrap();
    fs::write(f.root.join("notes/valid.md"), b"inside").unwrap();
    fs::write(f._temp.path().join("outside.md"), b"outside").unwrap();
    symlink(f._temp.path().join("outside.md"), f.root.join("escape.md")).unwrap();
    symlink(f.root.join("notes"), f.root.join("linked")).unwrap();
    for path in [
        "../outside.md",
        "/etc/passwd",
        "notes/../escape.md",
        "notes//valid.md",
        "./notes/valid.md",
        "notes\\valid.md",
        "escape.md",
        "linked/valid.md",
        "notes",
    ] {
        assert_eq!(
            s.read(path).unwrap_err().code,
            ErrorCode::InvalidPath,
            "{path}"
        );
        assert_eq!(
            s.write(request(1, path, None, b"attack")).unwrap_err().code,
            ErrorCode::InvalidPath,
            "{path}"
        );
    }
    assert_eq!(
        STANDARD
            .decode(s.read("notes/valid.md").unwrap().content_base64)
            .unwrap(),
        b"inside"
    );
    assert_eq!(
        fs::read(f._temp.path().join("outside.md")).unwrap(),
        b"outside"
    );
}

#[test]
fn invalid_requests_and_journal_binding_do_not_modify_source() {
    let f = Fixture::new();
    let s = f.store(WriteBoundary::Managed);
    fs::write(f.root.join("note.md"), b"original").unwrap();
    let mut bad = request(1, "note.md", None, b"bad");
    bad.content_base64 = "not base64!".into();
    assert_eq!(s.write(bad).unwrap_err().code, ErrorCode::InvalidRequest);
    let mut bad = request(1, "note.md", None, b"bad");
    bad.operation_id = "../escape".into();
    assert_eq!(s.write(bad).unwrap_err().code, ErrorCode::InvalidRequest);
    assert_eq!(fs::read(f.root.join("note.md")).unwrap(), b"original");
    assert!(SourceStore::open(
        "02000000-0000-4000-8000-000000000001",
        &f.root,
        &f.state,
        WriteBoundary::Managed
    )
    .is_err());
    let index = f.root.join(".okilum-index");
    fs::create_dir(&index).unwrap();
    assert!(SourceStore::open(
        "01000000-0000-4000-8000-000000000001",
        &f.root,
        &index,
        WriteBoundary::Managed
    )
    .is_err());
}

#[test]
fn competing_processes_observe_one_creation_and_one_conflict() {
    let f = Fixture::new();
    let _s = f.store(WriteBoundary::Managed);
    let exe = std::env::current_exe().unwrap();
    let spawn = |id: &str| {
        std::process::Command::new(&exe)
            .args(["--exact", "process_writer", "--ignored", "--nocapture"])
            .env("SOURCE_TEST_ROOT", &f.root)
            .env("SOURCE_TEST_STATE", &f.state)
            .env("SOURCE_TEST_ID", id)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = spawn("1");
    let b = spawn("2");
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    assert!(a.status.success());
    assert!(b.status.success());
    let results = format!(
        "{}{}",
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&b.stdout)
    );
    assert_eq!(results.matches("CREATED").count(), 1, "{results}");
    assert_eq!(results.matches("CONFLICT").count(), 1, "{results}");
}

#[test]
#[ignore = "subprocess fixture invoked by competing_processes"]
fn process_writer() {
    let root = std::env::var_os("SOURCE_TEST_ROOT").unwrap();
    let state = std::env::var_os("SOURCE_TEST_STATE").unwrap();
    let id = std::env::var("SOURCE_TEST_ID").unwrap().parse().unwrap();
    let s = SourceStore::open(
        "01000000-0000-4000-8000-000000000001",
        std::path::Path::new(&root),
        std::path::Path::new(&state),
        WriteBoundary::Managed,
    )
    .unwrap();
    match s.write(request(id, "race.md", None, id.to_string().as_bytes())) {
        Ok(_) => println!("CREATED"),
        Err(e) if e.code == ErrorCode::Conflict => println!("CONFLICT"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn completed_retry_survives_deleted_or_symlinked_parent_and_preserves_mode() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let s = f.store(WriteBoundary::Managed);
    fs::create_dir(f.root.join("notes")).unwrap();
    fs::write(f.root.join("notes/note.md"), b"original").unwrap();
    fs::set_permissions(
        f.root.join("notes/note.md"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    let req = request(
        1,
        "notes/note.md",
        Some(s.read("notes/note.md").unwrap().revision),
        b"replacement",
    );
    let receipt = s.write(req.clone()).unwrap();
    assert_eq!(
        fs::metadata(f.root.join("notes/note.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    fs::remove_dir_all(f.root.join("notes")).unwrap();
    assert_eq!(s.write(req.clone()).unwrap(), receipt);
    symlink(f._temp.path(), f.root.join("notes")).unwrap();
    assert_eq!(s.write(req).unwrap(), receipt);
}

#[test]
fn source_matching_old_staging_name_is_never_unlinked() {
    let f = Fixture::new();
    let s = f.store(WriteBoundary::Managed);
    let path = ".okilum-source-06000000-0000-4000-8000-000000000001.tmp";
    fs::write(f.root.join(path), b"original").unwrap();
    s.write(request(
        1,
        path,
        Some(s.read(path).unwrap().revision),
        b"replacement",
    ))
    .unwrap();
    assert_eq!(fs::read(f.root.join(path)).unwrap(), b"replacement");
}

#[test]
fn committed_source_updates_watcher_and_index_without_read_feedback() {
    use okilum_core::{Searcher, Vault, VaultWatcher};
    use std::time::Duration;
    let f = Fixture::new();
    let s = f.store(WriteBoundary::Managed);
    let first = s
        .write(request(1, "note.md", None, b"# Initial\n"))
        .unwrap();
    let index = f.root.join(".okilum-index");
    let vault = Vault::scan(&f.root).unwrap();
    Searcher::build(&vault, &index).unwrap();
    let mut watcher = VaultWatcher::new(&f.root).unwrap();
    s.write(request(
        2,
        "note.md",
        Some(first.revision),
        b"# Replacement\nuniqueword\n",
    ))
    .unwrap();
    let changes = watcher
        .wait(Duration::from_secs(3))
        .expect("source save must reach watcher");
    assert_eq!(changes.changed.iter().collect::<Vec<_>>(), vec!["note.md"]);
    assert!(changes.removed.is_empty());
    let vault = Vault::scan(&f.root).unwrap();
    Searcher::build(&vault, &index).unwrap();
    assert_eq!(
        Searcher::open_or_build(&vault, &index)
            .unwrap()
            .search("uniqueword", 10)
            .unwrap()
            .len(),
        1
    );
    s.read("note.md").unwrap();
    assert!(
        watcher.wait(Duration::from_millis(800)).is_none(),
        "reads and derived-index work must stay quiet"
    );
    fs::remove_dir_all(index).unwrap();
    assert!(s
        .recovery_record(&request(2, "", None, b"").operation_id)
        .unwrap()
        .receipt
        .is_some());
}

#[test]
fn conflict_versions_survive_restart_and_resolution_is_revision_guarded() {
    let f = Fixture::new();
    let store = f.store(WriteBoundary::Managed);
    let original = b"\xef\xbb\xbf---\r\ntype: Note\r\n---\r\nPacking: batteries\r\nSchedule: morning\r\n[[note|alias]]";
    store.write(request(80, "note.md", None, original)).unwrap();
    let base = store.read("note.md").unwrap();
    let current = String::from_utf8(original.to_vec())
        .unwrap()
        .replace("morning", "afternoon");
    store
        .write(request(
            81,
            "note.md",
            Some(base.revision.clone()),
            current.as_bytes(),
        ))
        .unwrap();
    let proposal = String::from_utf8(original.to_vec())
        .unwrap()
        .replace("batteries", "batteries and tape");
    let write = request(
        82,
        "note.md",
        Some(base.revision.clone()),
        proposal.as_bytes(),
    );
    let id = write.operation_id.clone();
    assert_eq!(
        store
            .write_with_base(write.clone(), Some(base.clone()))
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    drop(store);
    let store = f.store(WriteBoundary::Managed);
    let view = store.conflict(&base.brain_id, "note.md", &id).unwrap();
    assert_eq!(view.base, Some(base.clone()));
    assert_eq!(
        STANDARD.decode(view.proposed.content_base64).unwrap(),
        proposal.as_bytes()
    );
    let displayed = view.current.unwrap();
    assert_eq!(
        STANDARD.decode(&displayed.content_base64).unwrap(),
        current.as_bytes()
    );
    assert_eq!(
        store
            .write_with_base(write, Some(base.clone()))
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // Another cooperating writer wins after the comparison was displayed.
    store
        .write(request(
            83,
            "note.md",
            Some(displayed.revision.clone()),
            b"newer concurrent content",
        ))
        .unwrap();
    let resolution = current.replace("batteries", "batteries and tape");
    let resolved = request(
        84,
        "note.md",
        Some(displayed.revision.clone()),
        resolution.as_bytes(),
    );
    assert_eq!(
        store
            .write_with_base(resolved.clone(), Some(displayed))
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        fs::read(f.root.join("note.md")).unwrap(),
        b"newer concurrent content"
    );
    let latest = store.read("note.md").unwrap();
    let final_write = request(
        85,
        "note.md",
        Some(latest.revision.clone()),
        resolution.as_bytes(),
    );
    let receipt = store
        .write_with_base(final_write.clone(), Some(latest.clone()))
        .unwrap();
    // Lost acknowledgement: replay returns the same receipt after a newer write.
    store
        .write(request(
            86,
            "note.md",
            Some(receipt.revision.clone()),
            b"after resolution",
        ))
        .unwrap();
    assert_eq!(
        store.write_with_base(final_write, Some(latest)).unwrap(),
        receipt
    );
    assert_eq!(
        fs::read(f.root.join("note.md")).unwrap(),
        b"after resolution"
    );
    assert!(store.conflict(&base.brain_id, "note.md", &id).is_ok());
    assert!(store.conflict("wrong-brain", "note.md", &id).is_err());
    assert!(store.conflict(&base.brain_id, "other.md", &id).is_err());
}

#[test]
fn forged_base_and_substituted_conflict_identity_are_rejected() {
    let f = Fixture::new();
    let store = f.store(WriteBoundary::Managed);
    store
        .write(request(90, "note.md", None, b"Choice: blue\n"))
        .unwrap();
    let base = store.read("note.md").unwrap();
    let mut forged = base.clone();
    forged.content_base64 = STANDARD.encode(b"invented base");
    assert_eq!(
        store
            .write_with_base(
                request(
                    91,
                    "note.md",
                    Some(base.revision.clone()),
                    b"Choice: green\n"
                ),
                Some(forged)
            )
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    store
        .write(request(
            92,
            "note.md",
            Some(base.revision.clone()),
            b"Choice: red\n",
        ))
        .unwrap();
    let write = request(
        93,
        "note.md",
        Some(base.revision.clone()),
        b"Choice: green\n",
    );
    let id = write.operation_id.clone();
    assert!(store.write_with_base(write, Some(base.clone())).is_err());
    let view = store.conflict(&base.brain_id, "note.md", &id).unwrap();
    assert_eq!(
        STANDARD
            .decode(view.current.unwrap().content_base64)
            .unwrap(),
        b"Choice: red\n"
    );
    assert_eq!(
        STANDARD.decode(view.proposed.content_base64).unwrap(),
        b"Choice: green\n"
    );
    // A journal file substituted under a different UUID must not expose the
    // original operation's contents under that wrong identity.
    let substituted = request(94, "note.md", None, b"").operation_id;
    fs::copy(
        f.state.join(format!("{id}.json")),
        f.state.join(format!("{substituted}.json")),
    )
    .unwrap();
    assert_eq!(
        store
            .conflict(&base.brain_id, "note.md", &substituted)
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let mut record = store.recovery_record(&id).unwrap();
    record.conflict.as_mut().unwrap().conflict_id = substituted;
    fs::write(
        f.state.join(format!("{id}.json")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(
        store
            .conflict(&base.brain_id, "note.md", &id)
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
}
