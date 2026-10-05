use super::*;
use std::sync::{Arc, Mutex};
struct Fixture {
    dir: tempfile::TempDir,
    brain_id: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("brain/records")).unwrap();
        fs::create_dir(dir.path().join("state")).unwrap();
        Self {
            dir,
            brain_id: Uuid::new_v4().to_string(),
        }
    }
    fn open(&self) -> Runner {
        self.open_boundary(WriteBoundary::Managed)
    }
    fn open_boundary(&self, boundary: WriteBoundary) -> Runner {
        Runner::open(RunnerConfig {
            brain_id: self.brain_id.clone(),
            root: self.dir.path().join("brain"),
            records_dir: "records".into(),
            operational_dir: self.dir.path().join("state"),
            boundary,
        })
        .unwrap()
    }
}
fn request(text: &str) -> CaptureRequest {
    let operation_id = Uuid::new_v4().to_string();
    CaptureRequest {
        operation_id: operation_id.clone(),
        text: text.into(),
        source: api::SourceIdentity {
            channel: "native".into(),
            instance_id: Uuid::new_v4().to_string(),
            account_id: "local".into(),
            actor_id: "operator".into(),
            chat_id: None,
            topic_id: None,
            message_id: operation_id.clone(),
            update_id: operation_id,
            uri: None,
        },
    }
}
fn capture(r: &mut Runner, req: CaptureRequest) -> Capture {
    r.inbox_capture(req, "operator").unwrap()
}
fn code(e: &anyhow::Error) -> &str {
    e.downcast_ref::<api::InboxError>().unwrap().code
}
#[test]
fn capture_is_exact_goal_independent_and_replay_survives_restart_alias_and_deleted_index() {
    let f = Fixture::new();
    let mut r = f.open();
    let before = serde_json::to_value(&r.state.current).unwrap();
    let req = request("  original CRLF\r\n\r\n---\r\nno final newline  ");
    let first = capture(&mut r, req.clone());
    let get = r.inbox_get(&first.capture_id).unwrap();
    assert_eq!(get.text.as_bytes(), req.text.as_bytes());
    let text = String::from_utf8(STANDARD.decode(&get.source.content_base64).unwrap()).unwrap();
    assert_eq!(api::parse(&text).unwrap().1.as_bytes(), req.text.as_bytes());
    assert_eq!(before, serde_json::to_value(&r.state.current).unwrap());
    assert!(r.goal_ids().is_empty());
    assert!(r.state.pending_writes.is_empty());
    let mut alias = req.clone();
    alias.operation_id = Uuid::new_v4().to_string();
    let repeated = capture(&mut r, alias.clone());
    assert!(repeated.receipt.replayed);
    assert_eq!(first.capture_id, repeated.capture_id);
    fs::create_dir_all(r.state_dir.join("derived")).unwrap();
    fs::remove_dir_all(r.state_dir.join("derived")).unwrap();
    drop(r);
    let mut r = f.open();
    let repeated = capture(&mut r, alias.clone());
    assert_eq!(first.revision, repeated.revision);
    assert_eq!(first.received_at, repeated.received_at);
    assert_eq!(repeated.receipt.operation_id, first.receipt.operation_id);
    alias.text.push('x');
    assert_eq!(
        code(&r.inbox_capture(alias, "operator").unwrap_err()),
        "inbox_identity_conflict"
    );
    let second = capture(&mut r, request("a genuinely new thought"));
    assert_ne!(first.capture_id, second.capture_id);
    assert_eq!(r.inbox_list(None, None).unwrap().items.len(), 2);
}
#[test]
fn crashes_before_source_and_after_source_converge_on_original_intent() {
    for after_source in [false, true] {
        let f = Fixture::new();
        let mut r = f.open();
        let req = request("crash fixture");
        if after_source {
            r.interrupt_after_write = Some(1);
        } else {
            r.interrupt_after_inbox_intent = true;
        }
        assert!(r.inbox_capture(req.clone(), "operator").is_err());
        let pending = r.state.inbox_operations[&req.operation_id].capture.clone();
        assert_eq!(pending.receipt.status, "pending");
        assert_eq!(r.state.pending_writes.len(), 1);
        assert_eq!(r.root.join(&pending.path).exists(), after_source);
        drop(r);
        let mut r = f.open();
        let done = capture(&mut r, req);
        assert!(done.receipt.replayed);
        assert_eq!(done.receipt.status, "committed");
        assert_eq!(done.capture_id, pending.capture_id);
        assert_eq!(done.revision, pending.revision);
        assert_eq!(r.inbox_list(None, None).unwrap().items.len(), 1);
        assert!(r.state.pending_writes.is_empty());
    }
}
#[test]
fn simultaneous_duplicates_and_lost_reply_create_one_record() {
    let f = Fixture::new();
    let req = request("one update");
    let shared = Arc::new(Mutex::new(f.open()));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let shared = shared.clone();
            let req = req.clone();
            std::thread::spawn(move || capture(&mut shared.lock().unwrap(), req))
        })
        .collect();
    let receipts: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(receipts
        .iter()
        .all(|r| r.capture_id == receipts[0].capture_id));
    assert_eq!(receipts.iter().filter(|r| !r.receipt.replayed).count(), 1);
    drop(shared);
    let mut r = f.open();
    assert!(capture(&mut r, req).receipt.replayed);
    assert_eq!(r.inbox_list(None, None).unwrap().items.len(), 1);
}
#[test]
fn create_only_collision_is_retained_and_never_overwrites_existing_bytes() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = request("queued text");
    r.interrupt_after_inbox_intent = true;
    assert!(r.inbox_capture(req.clone(), "operator").is_err());
    let pending = r.state.inbox_operations[&req.operation_id].capture.clone();
    fs::write(r.root.join(&pending.path), b"other writer's bytes").unwrap();
    let error = r.inbox_capture(req, "operator").unwrap_err();
    assert!(error
        .downcast_ref::<tessera_core::source::SourceError>()
        .is_some());
    assert_eq!(
        fs::read(r.root.join(&pending.path)).unwrap(),
        b"other writer's bytes"
    );
    assert_eq!(r.state.inbox_operations.len(), 1);
    assert_eq!(r.state.pending_writes.len(), 1);
    assert_eq!(
        r.state
            .inbox_operations
            .values()
            .next()
            .unwrap()
            .capture
            .receipt
            .status,
        "pending"
    );
}
#[test]
fn committed_replay_does_not_rewrite_edited_or_deleted_sources() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = request("initial");
    let first = capture(&mut r, req.clone());
    let path = r.root.join(&first.path);
    let original = fs::read_to_string(&path).unwrap();
    fs::write(&path, original.replace("initial", "edited")).unwrap();
    assert_ne!(
        r.inbox_get(&first.capture_id).unwrap().item.revision,
        first.revision
    );
    assert_eq!(capture(&mut r, req.clone()).revision, first.revision);
    fs::remove_file(&path).unwrap();
    assert_eq!(capture(&mut r, req).revision, first.revision);
    assert!(!path.exists());
    assert_eq!(
        code(&r.inbox_get(&first.capture_id).unwrap_err()),
        "inbox_not_found"
    );
}
#[test]
fn validation_and_identity_mismatches_do_not_mutate_journal() {
    let f = Fixture::new();
    let mut r = f.open();
    let base = request("valid");
    let original = fs::read(r.state_dir.join("state.json")).unwrap();
    let mut variants = Vec::new();
    for text in [" ".to_owned(), "x".repeat(65_537)] {
        let mut q = base.clone();
        q.text = text;
        variants.push(q);
    }
    let mut q = base.clone();
    q.source.actor_id = "other".into();
    variants.push(q);
    let mut q = base.clone();
    q.source.channel = "telegram".into();
    variants.push(q);
    let mut q = base.clone();
    q.source.instance_id = "bad\nidentity".into();
    variants.push(q);
    let mut q = base.clone();
    q.source.topic_id = Some("remote".into());
    variants.push(q);
    for q in variants {
        assert_eq!(
            code(&r.inbox_capture(q, "operator").unwrap_err()),
            "inbox_invalid_request"
        );
        assert_eq!(fs::read(r.state_dir.join("state.json")).unwrap(), original);
    }
    capture(&mut r, base.clone());
    let after = fs::read(r.state_dir.join("state.json")).unwrap();
    let mut changed = base.clone();
    changed.source.uri = Some("changed".into());
    assert_eq!(
        code(&r.inbox_capture(changed, "operator").unwrap_err()),
        "inbox_identity_conflict"
    );
    let mut changed = base;
    changed.operation_id = Uuid::new_v4().to_string();
    changed.text = "edited update".into();
    assert_eq!(
        code(&r.inbox_capture(changed, "operator").unwrap_err()),
        "inbox_identity_conflict"
    );
    assert_eq!(fs::read(r.state_dir.join("state.json")).unwrap(), after);
}
#[test]
fn pagination_detects_inventory_change_and_invalid_records_fail_closed() {
    let f = Fixture::new();
    let mut r = f.open();
    for i in 0..3 {
        capture(&mut r, request(&format!("thought {i}")));
    }
    let first = r.inbox_list(Some(1), None).unwrap();
    assert!(!first.complete);
    let second = r.inbox_list(Some(2), first.next_cursor.as_deref()).unwrap();
    assert!(second.complete);
    assert!(second
        .items
        .iter()
        .all(|i| i.capture_id > first.items[0].capture_id));
    capture(&mut r, request("new"));
    assert_eq!(
        code(
            &r.inbox_list(Some(2), first.next_cursor.as_deref())
                .unwrap_err()
        ),
        "inbox_cursor_stale"
    );
    let path = r.root.join(&first.items[0].path);
    let source = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        source.replace(&f.brain_id, &Uuid::new_v4().to_string()),
    )
    .unwrap();
    assert_eq!(
        code(&r.inbox_list(None, None).unwrap_err()),
        "inbox_record_invalid"
    );
}
#[test]
fn canonical_only_restore_is_readable_but_cannot_replay_or_capture() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = request("portable knowledge");
    let first = capture(&mut r, req.clone());
    drop(r);
    fs::remove_file(f.dir.path().join("state/state.json")).unwrap();
    let mut r = f.open();
    assert_eq!(
        r.inbox_get(&first.capture_id).unwrap().item.capture_id,
        first.capture_id
    );
    assert_eq!(r.inbox_list(None, None).unwrap().items.len(), 1);
    assert_eq!(
        code(&r.inbox_capture(req, "operator").unwrap_err()),
        "inbox_unsupported"
    );
    drop(r);
    let mut r = f.open();
    assert_eq!(
        code(&r.inbox_capture(request("new"), "operator").unwrap_err()),
        "inbox_unsupported"
    );
}
#[test]
fn unmanaged_capture_is_explicitly_unsupported() {
    let f = Fixture::new();
    let mut r = f.open_boundary(WriteBoundary::Unmanaged);
    assert_eq!(
        code(
            &r.inbox_capture(request("proposal"), "operator")
                .unwrap_err()
        ),
        "inbox_unsupported"
    );
    assert!(r.inbox_list(None, None).unwrap().items.is_empty());
}
#[test]
fn export_drains_pending_inbox_and_contains_exact_source_once_without_operational_receipts() {
    use std::io::Read;
    let f = Fixture::new();
    let mut r = f.open();
    let req = request(" exact body\r\nwithout final newline");
    r.interrupt_after_inbox_intent = true;
    assert!(r.inbox_capture(req.clone(), "operator").is_err());
    let pending = r.state.inbox_operations[&req.operation_id].capture.clone();
    let archive = f.dir.path().join("knowledge.tar");
    let receipt = r.export_exact(&archive).unwrap();
    assert_eq!(
        receipt
            .manifest
            .files
            .iter()
            .filter(|file| file.path == pending.path)
            .count(),
        1
    );
    assert!(receipt
        .manifest
        .files
        .iter()
        .all(|file| !file.path.contains("state.json")));
    let mut tar = tar::Archive::new(File::open(archive).unwrap());
    let mut found = 0;
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_str().unwrap() == format!("brain/{}", pending.path) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, fs::read(r.root.join(&pending.path)).unwrap());
            assert_eq!(
                api::parse(std::str::from_utf8(&bytes).unwrap()).unwrap().1,
                req.text
            );
            found += 1;
        }
    }
    assert_eq!(found, 1);
    assert!(r.state.pending_writes.is_empty());
    assert_eq!(capture(&mut r, req).receipt.status, "committed");
}
#[test]
fn canonical_record_validation_rejects_forged_owner_but_preserves_crlf_and_literal_delimiters() {
    let f = Fixture::new();
    let mut r = f.open();
    let mut req = request("body\r\n");
    req.source.uri = Some("original-reference".into());
    let capture = capture(&mut r, req);
    let source = r.inbox_get(&capture.capture_id).unwrap().source;
    let text = String::from_utf8(STANDARD.decode(&source.content_base64).unwrap()).unwrap();
    let with_literal = text.replacen(
        "schema:",
        "summary: |\n    nested\n    ---\n    retained\nschema:",
        1,
    );
    let with_crlf = format!("\u{feff}{}", with_literal.replace('\n', "\r\n"));
    assert!(api::validate_record(&with_crlf, &capture.path, "records", &f.brain_id).is_ok());
    for altered in [
        text.replace("record_type: inbox", "record_type: decision"),
        text.replace("status: captured", "status: completed"),
        text.replace(&f.brain_id, &Uuid::new_v4().to_string()),
    ] {
        assert!(api::validate_record(&altered, &capture.path, "records", &f.brain_id).is_err());
    }
}

#[test]
fn losing_only_delivery_receipts_in_existing_state_disables_capture_without_hiding_knowledge() {
    let f = Fixture::new();
    let mut r = f.open();
    let first = capture(&mut r, request("canonical knowledge"));
    let path = r.state_dir.join("state.json");
    drop(r);
    let mut journal: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    journal.as_object_mut().unwrap().remove("inbox_operations");
    fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let mut r = f.open();
    assert!(!r.inbox_writable());
    assert_eq!(r.inbox_list(None, None).unwrap().items.len(), 1);
    assert_eq!(
        r.inbox_get(&first.capture_id).unwrap().item.capture_id,
        first.capture_id
    );
    let export = f.dir.path().join("restored.tar");
    assert_eq!(r.export_exact(&export).unwrap().manifest.files.len(), 1);
    assert_eq!(
        code(
            &r.inbox_capture(request("duplicate upstream delivery"), "operator")
                .unwrap_err()
        ),
        "inbox_unsupported"
    );
}

#[test]
fn pre_inbox_upgrade_enables_first_capture_and_preserves_existing_goal() {
    let f = Fixture::new();
    let mut r = f.open();
    let goal = Uuid::new_v4().to_string();
    r.create_goal(
        Goal {
            id: goal.clone(),
            title: "Existing project goal".into(),
            status: "draft".into(),
            criteria: vec![Criterion {
                id: "C1".into(),
                description: "Existing criterion".into(),
                requires_human: false,
            }],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        },
        "Original goal body".into(),
    )
    .unwrap();
    r.state.application = serde_json::json!({"selection":{"source_paths":["original.md"]},"provider_marker":"retain existing provider state"});
    r.persist().unwrap();
    let before = serde_json::to_value(&r.state.current).unwrap();
    let primary = r.state.primary_goal_id.clone();
    let path = r.state_dir.join("state.json");
    drop(r);
    let mut old: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for field in [
        "inbox_operations",
        "inbox_aliases",
        "inbox_recovery_required",
    ] {
        old.as_object_mut().unwrap().remove(field);
    }
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let mut r = f.open();
    assert!(r.inbox_writable());
    let migrated: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(migrated["inbox_operations"], serde_json::json!({}));
    assert_eq!(r.state.primary_goal_id, primary);
    assert_eq!(serde_json::to_value(&r.state.current).unwrap(), before);
    let req = request("First thought after normal upgrade");
    let first = capture(&mut r, req.clone());
    assert_eq!(serde_json::to_value(&r.state.current).unwrap(), before);
    drop(r);
    let mut r = f.open();
    assert_eq!(capture(&mut r, req).capture_id, first.capture_id);
    assert_eq!(r.goal_ids(), vec![goal]);
    assert_eq!(serde_json::to_value(&r.state.current).unwrap(), before);
}

#[test]
fn malformed_canonical_utf8_returns_structured_record_error_for_get_and_list() {
    let f = Fixture::new();
    let mut r = f.open();
    let first = capture(&mut r, request("valid capture"));
    fs::write(r.root.join(&first.path), [0xff, 0xfe]).unwrap();
    assert_eq!(
        code(&r.inbox_get(&first.capture_id).unwrap_err()),
        "inbox_record_invalid"
    );
    assert_eq!(
        code(&r.inbox_list(None, None).unwrap_err()),
        "inbox_record_invalid"
    );
}
