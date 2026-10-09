use super::*;
use crate::inbox::{CaptureRequest, SourceIdentity};
use std::sync::{Arc, Mutex};
struct Fixture {
    dir: tempfile::TempDir,
    brain: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("brain/records")).unwrap();
        fs::create_dir(dir.path().join("state")).unwrap();
        Self {
            dir,
            brain: Uuid::new_v4().to_string(),
        }
    }
    fn open(&self) -> Runner {
        Runner::open(RunnerConfig {
            brain_id: self.brain.clone(),
            root: self.dir.path().join("brain"),
            operational_dir: self.dir.path().join("state"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        })
        .unwrap()
    }
}
fn raw(r: &mut Runner) -> Result<Vec<Value>> {
    crate::application::Application::unconfigured().attention_items(r)
}
fn list(r: &mut Runner) -> List {
    let raw = raw(r).unwrap();
    r.attention_list(raw, "operator", None, None, None).unwrap()
}
fn add_goal(r: &mut Runner, kind: &str) -> String {
    let id = Uuid::new_v4().to_string();
    r.create_goal(
        Goal {
            id: id.clone(),
            title: "Attention fixture".into(),
            status: "draft".into(),
            criteria: vec![Criterion {
                id: "C1".into(),
                description: "Real observed result".into(),
                requires_human: true,
            }],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        },
        "# Original goal\n".into(),
    )
    .unwrap();
    r.with_goal(&id, |r| {
        r.attention(kind, "Please inspect this item");
        r.persist()
    })
    .unwrap();
    id
}
fn request(item: &Item) -> Mutation {
    let operation_id = Uuid::new_v4().to_string();
    Mutation {
        operation_id: operation_id.clone(),
        target: api::Target {
            goal_id: item.goal_id.clone(),
            attention_id: item.attention_id.clone(),
            expected_revision: item.revision.clone(),
            stage_id: item.stage_id.clone(),
        },
        source: SourceIdentity {
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
fn reply(r: &mut Runner, request: Mutation, text: &str) -> Outcome {
    r.attention_mutate(request, "save_decision", Some(text), "operator", raw)
        .unwrap()
}
fn ack(r: &mut Runner, request: Mutation) -> Outcome {
    r.attention_mutate(request, "ack_seen", None, "operator", raw)
        .unwrap()
}
fn code(e: &anyhow::Error) -> &str {
    e.downcast_ref::<api::AttentionError>().unwrap().code
}
fn change_goal(r: &mut Runner, goal: &str) {
    let (mut value, _) = r.record::<Goal>("goal", goal).unwrap();
    value.title.push_str(" revised");
    r.queue("goal", goal, &value, None).unwrap();
    r.persist().unwrap();
    r.flush_writes().unwrap();
}
#[test]
fn revisions_survive_repeated_reads_and_restart_but_change_with_material_goal_source() {
    let f = Fixture::new();
    let mut r = f.open();
    let goal = add_goal(&mut r, "decision");
    let before = serde_json::to_value(&r.state.current).unwrap();
    let first = list(&mut r);
    let again = list(&mut r);
    assert_eq!(first.items[0].revision, again.items[0].revision);
    assert_eq!(first.delivery_cursor, again.delivery_cursor);
    assert_eq!(before, serde_json::to_value(&r.state.current).unwrap());
    drop(r);
    let mut r = f.open();
    assert_eq!(list(&mut r).items[0].revision, first.items[0].revision);
    change_goal(&mut r, &goal);
    let new = list(&mut r);
    assert_ne!(new.items[0].revision, first.items[0].revision);
    assert_eq!(new.delivery_cursor, first.delivery_cursor + 1);
    let raw = raw(&mut r).unwrap();
    let historical = r
        .attention_get(
            raw,
            "operator",
            None,
            &goal,
            &first.items[0].attention_id,
            Some(&first.items[0].revision),
        )
        .unwrap();
    assert_eq!(historical["item"]["current"], false);
    assert_eq!(historical["item"]["allowed_actions"], serde_json::json!([]));
    assert_eq!(historical["item"]["message"], first.items[0].message);
}
#[test]
fn save_decision_preserves_exact_text_and_goal_workflow_and_does_not_acknowledge() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "blocker");
    let item = list(&mut r).items.remove(0);
    let before = serde_json::to_value(&r.state.current).unwrap();
    let text = "  Keep as a proposal\r\n---\r\nwithout terminal newline  ";
    let outcome = reply(&mut r, request(&item), text);
    let source = r.read_source(outcome.path.as_ref().unwrap()).unwrap();
    let (metadata, body) = parse_document(&source).unwrap();
    assert_eq!(body.as_bytes(), text.as_bytes());
    let value = serde_yaml::Value::Mapping(metadata);
    assert_eq!(value["verification"], "unverified");
    assert_eq!(value["goal_id"].as_str(), Some(item.goal_id.as_str()));
    assert!(value["stage_id"].is_null());
    assert_eq!(
        value["attention_revision"].as_str(),
        Some(item.revision.as_str())
    );
    assert_eq!(before, serde_json::to_value(&r.state.current).unwrap());
    assert!(!list(&mut r).items[0].seen);
    assert!(r.state.human_acceptances.is_empty());
    assert!(r.state.dispatch.is_none());
    let export = f.dir.path().join("decision.tar");
    assert!(r
        .export_exact(&export)
        .unwrap()
        .manifest
        .files
        .iter()
        .any(|file| Some(&file.path) == outcome.path.as_ref()));
}
#[test]
fn wrong_goal_stage_revision_actor_and_final_reply_never_create_decisions_or_seen_state() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    let item = list(&mut r).items.remove(0);
    for case in 0..4 {
        let mut req = request(&item);
        match case {
            0 => req.target.goal_id = Uuid::new_v4().to_string(),
            1 => req.target.stage_id = Some(Uuid::new_v4().to_string()),
            2 => req.target.expected_revision = format!("sha256:{}", "a".repeat(64)),
            _ => req.source.actor_id = "other actor".into(),
        }
        let before = fs::read(r.state_dir.join("state.json")).unwrap();
        let error = r
            .attention_mutate(req, "save_decision", Some("must not save"), "operator", raw)
            .unwrap_err();
        assert_eq!(
            code(&error),
            if case == 3 {
                "attention_invalid_request"
            } else {
                "attention_stale"
            }
        );
        assert_eq!(fs::read(r.state_dir.join("state.json")).unwrap(), before);
    }
    r.state.attention[0].kind = "final".into();
    r.persist().unwrap();
    let item = list(&mut r).items.remove(0);
    assert_eq!(
        code(
            &r.attention_mutate(
                request(&item),
                "save_decision",
                Some("no final acceptance"),
                "operator",
                raw
            )
            .unwrap_err()
        ),
        "attention_unsupported"
    );
    assert!(r.state.attention_journal.operations.is_empty());
    assert!(r.state.attention_journal.seen.is_empty());
    assert!(ack(&mut r, request(&item)).item.unwrap().seen);
}
#[test]
fn seen_state_is_scoped_and_restarts_without_hiding_blocker_or_accepting_goal() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "blocker");
    let item = list(&mut r).items.remove(0);
    let req = request(&item);
    let before = serde_json::to_value(&r.state.current).unwrap();
    let first = ack(&mut r, req.clone());
    let timestamp = first.acknowledged_at.clone();
    assert!(list(&mut r).items[0].seen);
    assert_eq!(list(&mut r).items[0].kind, "blocker");
    assert!(
        !r.attention_present(item.clone(), "another actor", "native", true)
            .unwrap()
            .seen
    );
    assert!(
        !r.attention_present(item.clone(), "operator", "telegram", true)
            .unwrap()
            .seen
    );
    assert_eq!(before, serde_json::to_value(&r.state.current).unwrap());
    drop(r);
    let mut r = f.open();
    assert_eq!(ack(&mut r, req).acknowledged_at, timestamp);
    assert_eq!(ack(&mut r, request(&item)).acknowledged_at, timestamp);
    change_goal(&mut r, &item.goal_id);
    assert!(!list(&mut r).items[0].seen);
}
#[test]
fn reply_crashes_and_replay_after_supersession_recover_original_receipt_without_new_authority() {
    for after_source in [false, true] {
        let f = Fixture::new();
        let mut r = f.open();
        add_goal(&mut r, "decision");
        let item = list(&mut r).items.remove(0);
        let req = request(&item);
        if after_source {
            r.interrupt_after_write = Some(1);
        } else {
            r.interrupt_after_inbox_intent = true;
        }
        assert!(r
            .attention_mutate(
                req.clone(),
                "save_decision",
                Some("exact saved reply"),
                "operator",
                raw
            )
            .is_err());
        let pending = r.state.attention_journal.operations[&req.operation_id]
            .outcome
            .clone();
        assert_eq!(pending.receipt.status, "pending");
        assert_eq!(
            r.root.join(pending.path.as_ref().unwrap()).exists(),
            after_source
        );
        drop(r);
        let mut r = f.open();
        change_goal(&mut r, &item.goal_id);
        let done = reply(&mut r, req.clone(), "exact saved reply");
        assert!(done.receipt.replayed);
        assert_eq!(done.decision_id, pending.decision_id);
        assert_eq!(done.revision, pending.revision);
        let done = r
            .attention_mutate(
                req,
                "save_decision",
                Some("exact saved reply"),
                "operator",
                |_| anyhow::bail!("current source deliberately unavailable"),
            )
            .unwrap();
        assert!(done.receipt.replayed);
        assert!(done.item.is_none());
        assert_eq!(done.decision_id, pending.decision_id);
    }
}
#[test]
fn concurrent_duplicates_aliases_and_cross_action_or_inbox_collisions_are_durable() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    let item = list(&mut r).items.remove(0);
    let req = request(&item);
    let shared = Arc::new(Mutex::new(r));
    let threads: Vec<_> = (0..6)
        .map(|_| {
            let shared = shared.clone();
            let req = req.clone();
            std::thread::spawn(move || reply(&mut shared.lock().unwrap(), req, "One reply"))
        })
        .collect();
    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(outcomes
        .iter()
        .all(|outcome| outcome.decision_id == outcomes[0].decision_id));
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| !outcome.receipt.replayed)
            .count(),
        1
    );
    let mut r = Arc::try_unwrap(shared).ok().unwrap().into_inner().unwrap();
    let mut alias = req.clone();
    alias.operation_id = Uuid::new_v4().to_string();
    assert!(reply(&mut r, alias.clone(), "One reply").receipt.replayed);
    drop(r);
    let mut r = f.open();
    assert!(reply(&mut r, alias.clone(), "One reply").receipt.replayed);
    assert_eq!(
        code(
            &r.attention_mutate(alias.clone(), "ack_seen", None, "operator", raw)
                .unwrap_err()
        ),
        "attention_identity_conflict"
    );
    assert_eq!(
        code(
            &r.attention_mutate(
                alias,
                "save_decision",
                Some("Changed reply"),
                "operator",
                raw
            )
            .unwrap_err()
        ),
        "attention_identity_conflict"
    );
    let capture = CaptureRequest {
        operation_id: req.operation_id.clone(),
        text: "wrong action".into(),
        source: req.source.clone(),
    };
    assert_eq!(
        r.inbox_capture(capture, "operator")
            .unwrap_err()
            .downcast_ref::<crate::inbox::InboxError>()
            .unwrap()
            .code,
        "inbox_identity_conflict"
    );
    let other = request(&item);
    r.inbox_capture(
        CaptureRequest {
            operation_id: other.operation_id.clone(),
            text: "Real thought".into(),
            source: other.source.clone(),
        },
        "operator",
    )
    .unwrap();
    assert_eq!(
        code(
            &r.attention_mutate(other, "ack_seen", None, "operator", raw)
                .unwrap_err()
        ),
        "attention_identity_conflict"
    );
}
#[test]
fn null_owner_is_not_inferred_from_current_stage_and_wrong_record_owner_is_rejected() {
    let f = Fixture::new();
    let mut r = f.open();
    let goal = add_goal(&mut r, "decision");
    let original = list(&mut r).items.remove(0);
    let stage_id = Uuid::new_v4().to_string();
    let stage = Stage {
        id: stage_id.clone(),
        goal_id: goal.clone(),
        engine: "t3".into(),
        status: "draft".into(),
        criterion_ids: vec!["C1".into()],
        context_id: Uuid::new_v4().to_string(),
        result_ids: vec![],
        extra: BTreeMap::new(),
    };
    r.queue("stage", &stage_id, &stage, Some("Stage source"))
        .unwrap();
    r.state.stage_id = Some(stage_id.clone());
    r.persist().unwrap();
    r.flush_writes().unwrap();
    let current = list(&mut r).items.remove(0);
    assert!(current.stage_id.is_none());
    assert!(current.result_id.is_none());
    assert_eq!(current.attention_id, original.attention_id);
    let mut requested = request(&current);
    requested.target.stage_id = Some(stage_id.clone());
    assert_eq!(
        code(
            &r.attention_mutate(
                requested,
                "save_decision",
                Some("wrong inferred owner"),
                "operator",
                raw
            )
            .unwrap_err()
        ),
        "attention_stale"
    );
    r.state
        .attention_stage_ids
        .insert(current.attention_id.clone(), stage_id.clone());
    r.persist().unwrap();
    let owned = list(&mut r).items.remove(0);
    assert_eq!(owned.stage_id, Some(stage_id.clone()));
    let mut forged = stage;
    forged.goal_id = Uuid::new_v4().to_string();
    r.queue("stage", &stage_id, &forged, None).unwrap();
    r.persist().unwrap();
    r.flush_writes().unwrap();
    assert!(raw(&mut r)
        .and_then(|items| r.attention_list(items, "operator", None, None, None))
        .is_err());
}
#[test]
fn pagination_and_retained_observation_history_reject_stale_generation_after_seen_change() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    r.attention("blocker", "Another independent blocker");
    r.persist().unwrap();
    let items = raw(&mut r).unwrap();
    let first = r
        .attention_list(items, "operator", None, Some(1), None)
        .unwrap();
    assert!(!first.complete);
    let items = raw(&mut r).unwrap();
    let second = r
        .attention_list(
            items,
            "operator",
            None,
            Some(1),
            first.next_cursor.as_deref(),
        )
        .unwrap();
    assert!(second.complete);
    assert_ne!(first.items[0].attention_id, second.items[0].attention_id);
    ack(&mut r, request(&first.items[0]));
    let items = raw(&mut r).unwrap();
    assert_eq!(
        code(
            &r.attention_list(
                items,
                "operator",
                None,
                Some(1),
                first.next_cursor.as_deref()
            )
            .unwrap_err()
        ),
        "attention_cursor_stale"
    );
    let cursor = r.state.attention_journal.sequence;
    assert_eq!(cursor, first.delivery_cursor);
    let deleted = first.items[0].clone();
    r.state
        .attention
        .retain(|item| item.id != deleted.attention_id);
    r.persist().unwrap();
    let items = raw(&mut r).unwrap();
    let retained = r
        .attention_get(
            items,
            "operator",
            None,
            &deleted.goal_id,
            &deleted.attention_id,
            Some(&deleted.revision),
        )
        .unwrap();
    assert_eq!(retained["item"]["current"], false);
    assert_eq!(retained["item"]["seen"], true);
}
#[test]
fn acknowledgement_lost_response_survives_restart_without_markdown_or_workflow_changes() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "final");
    let item = list(&mut r).items.remove(0);
    let req = request(&item);
    let source_before = fs::read(r.root.join(r.path("goal", &item.goal_id))).unwrap();
    let goal_before = serde_json::to_value(&r.state.current).unwrap();
    r.interrupt_after_inbox_intent = true;
    assert!(r
        .attention_mutate(req.clone(), "ack_seen", None, "operator", raw)
        .is_err());
    let committed = r.state.attention_journal.operations[&req.operation_id]
        .outcome
        .clone();
    assert_eq!(committed.receipt.status, "committed");
    drop(r);
    let mut r = f.open();
    let recovered = ack(&mut r, req);
    assert!(recovered.receipt.replayed);
    assert_eq!(recovered.acknowledged_at, committed.acknowledged_at);
    assert!(r.state.pending_writes.is_empty());
    assert_eq!(goal_before, serde_json::to_value(&r.state.current).unwrap());
    assert_eq!(
        fs::read(r.root.join(r.path("goal", &item.goal_id))).unwrap(),
        source_before
    );
}
#[test]
fn pending_decision_create_collision_preserves_both_intent_and_existing_source() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    let item = list(&mut r).items.remove(0);
    let req = request(&item);
    r.interrupt_after_inbox_intent = true;
    assert!(r
        .attention_mutate(
            req.clone(),
            "save_decision",
            Some("pending reply"),
            "operator",
            raw
        )
        .is_err());
    let pending = r.state.attention_journal.operations[&req.operation_id]
        .outcome
        .clone();
    let path = r.root.join(pending.path.as_ref().unwrap());
    fs::write(&path, b"existing unrelated bytes").unwrap();
    assert!(r
        .attention_mutate(req, "save_decision", Some("pending reply"), "operator", raw)
        .unwrap_err()
        .downcast_ref::<okilum_core::source::SourceError>()
        .is_some());
    assert_eq!(fs::read(path).unwrap(), b"existing unrelated bytes");
    assert_eq!(r.state.pending_writes.len(), 1);
    assert_eq!(r.state.attention_journal.operations.len(), 1);
}
#[test]
fn canonical_decision_without_delivery_journal_disables_mutations_but_retains_reading() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    let item = list(&mut r).items.remove(0);
    let outcome = reply(&mut r, request(&item), "Portable attributed decision");
    let path = r.state_dir.join("state.json");
    drop(r);
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("attention_journal");
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let mut r = f.open();
    assert!(!r.attention_writable());
    assert!(!r.inbox_writable());
    assert_eq!(raw(&mut r).unwrap()[0]["attention_id"], item.attention_id);
    let items = raw(&mut r).unwrap();
    assert_eq!(
        code(
            &r.attention_list(items, "operator", None, None, None)
                .unwrap_err()
        ),
        "attention_unsupported"
    );
    assert!(r.read_source(outcome.path.as_ref().unwrap()).is_ok());
    assert_eq!(
        code(
            &r.attention_mutate(request(&item), "ack_seen", None, "operator", raw)
                .unwrap_err()
        ),
        "attention_unsupported"
    );
}
#[test]
fn required_nullable_stage_and_text_limits_reject_before_intent_and_future_transport_is_explicit() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    let item = list(&mut r).items.remove(0);
    let req = request(&item);
    let mut wire = serde_json::to_value(&req).unwrap();
    assert!(serde_json::from_value::<Mutation>(wire.clone())
        .unwrap()
        .target
        .stage_id
        .is_none());
    wire.as_object_mut().unwrap().remove("stage_id");
    assert!(serde_json::from_value::<Mutation>(wire).is_err());
    let before = fs::read(r.state_dir.join("state.json")).unwrap();
    for text in ["  ".to_owned(), "я".repeat(32_769)] {
        assert_eq!(
            code(
                &r.attention_mutate(req.clone(), "save_decision", Some(&text), "operator", raw)
                    .unwrap_err()
            ),
            "attention_invalid_request"
        );
    }
    assert_eq!(fs::read(r.state_dir.join("state.json")).unwrap(), before);
    let items = raw(&mut r).unwrap();
    assert_eq!(
        code(
            &r.attention_list(items, "operator", Some("telegram"), None, None)
                .unwrap_err()
        ),
        "attention_unsupported"
    );
}

#[test]
fn first_upgrade_enrolls_after_checkpoint_and_missing_ack_only_journal_cannot_look_like_fresh_history(
) {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "final");
    let marker = r.state_dir.join("attention-enrollment.json");
    let state_path = r.state_dir.join("state.json");
    // Construct the actual pre-attention baseline: no field and no marker.
    drop(r);
    let mut old: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    old.as_object_mut().unwrap().remove("attention_journal");
    fs::write(&state_path, serde_json::to_vec(&old).unwrap()).unwrap();
    fs::remove_file(&marker).unwrap();
    let mut r = f.open();
    assert!(r.attention_readable());
    assert!(r.attention_writable());
    assert!(marker.exists());
    let durable: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert!(durable["attention_journal"].is_object());
    let item = list(&mut r).items.remove(0);
    ack(&mut r, request(&item));
    assert!(!r
        .source_list()
        .unwrap()
        .iter()
        .any(|value| value["path"].as_str().unwrap().contains("decision-")));
    drop(r);
    let mut lost: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    lost.as_object_mut().unwrap().remove("attention_journal");
    fs::write(&state_path, serde_json::to_vec(&lost).unwrap()).unwrap();
    let mut r = f.open();
    assert!(!r.attention_readable());
    assert!(!r.attention_writable());
    assert!(!r.inbox_writable());
    assert!(!raw(&mut r).unwrap().is_empty());
    let projection = raw(&mut r).unwrap();
    assert_eq!(
        code(
            &r.attention_list(projection, "operator", None, None, None)
                .unwrap_err()
        ),
        "attention_unsupported"
    );
    drop(r);
    let r = f.open();
    assert!(!r.attention_readable());
}
#[test]
fn interrupted_enrollment_after_checkpoint_recovers_without_resetting_existing_delivery_state() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    let item = list(&mut r).items.remove(0);
    ack(&mut r, request(&item));
    let sequence = r.state.attention_journal.sequence;
    let marker = r.state_dir.join("attention-enrollment.json");
    drop(r);
    fs::remove_file(&marker).unwrap();
    let mut r = f.open();
    assert!(marker.exists());
    assert!(r.attention_readable());
    assert!(list(&mut r).items[0].seen);
    assert_eq!(r.state.attention_journal.sequence, sequence);
}

#[test]
fn malformed_marker_or_unreadable_journal_never_becomes_fresh_upgrade() {
    for corrupt_marker in [true, false] {
        let f = Fixture::new();
        let r = f.open();
        let marker = r.state_dir.join("attention-enrollment.json");
        let state_path = r.state_dir.join("state.json");
        let config = RunnerConfig {
            brain_id: f.brain.clone(),
            root: r.root.clone(),
            operational_dir: r.state_dir.clone(),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let journal = fs::read(&state_path).unwrap();
        let marker_bytes = fs::read(&marker).unwrap();
        drop(r);
        if corrupt_marker {
            fs::write(&marker, b"{truncated").unwrap();
        } else {
            fs::remove_file(&state_path).unwrap();
            fs::create_dir(&state_path).unwrap();
        }
        assert!(Runner::open(config).is_err());
        if corrupt_marker {
            assert_eq!(fs::read(&state_path).unwrap(), journal);
            assert_eq!(fs::read(&marker).unwrap(), b"{truncated");
        } else {
            assert!(state_path.is_dir());
            assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
        }
    }
}
#[test]
fn known_committed_receipts_remain_replayable_while_unknown_history_blocks_new_actions() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    let item = list(&mut r).items.remove(0);
    let req = request(&item);
    let original = reply(&mut r, req.clone(), "Known committed reply");
    let capture_id = Uuid::new_v4().to_string();
    let mut source = req.source.clone();
    source.message_id = capture_id.clone();
    source.update_id = capture_id.clone();
    let capture = CaptureRequest {
        operation_id: capture_id,
        text: "Known capture".into(),
        source,
    };
    let original_capture = r.inbox_capture(capture.clone(), "operator").unwrap();
    r.state.attention_journal.recovery_required = true;
    r.persist().unwrap();
    let before = fs::read(r.state_dir.join("state.json")).unwrap();
    let replay = reply(&mut r, req.clone(), "Known committed reply");
    assert!(replay.receipt.replayed);
    assert_eq!(replay.decision_id, original.decision_id);
    assert_eq!(
        r.inbox_capture(capture, "operator").unwrap().capture_id,
        original_capture.capture_id
    );
    assert_eq!(fs::read(r.state_dir.join("state.json")).unwrap(), before);
    assert_eq!(
        code(
            &r.attention_mutate(request(&item), "ack_seen", None, "operator", raw)
                .unwrap_err()
        ),
        "attention_unsupported"
    );
    let mut new_alias = req;
    new_alias.operation_id = Uuid::new_v4().to_string();
    assert_eq!(
        code(
            &r.attention_mutate(
                new_alias,
                "save_decision",
                Some("Known committed reply"),
                "operator",
                raw
            )
            .unwrap_err()
        ),
        "attention_unsupported"
    );
}

#[test]
fn proposal_feed_triggers_decision_once_but_never_seen_receipts() {
    let f = Fixture::new();
    let mut r = f.open();
    add_goal(&mut r, "decision");
    r.enroll_proposal_feed(1).unwrap();
    let item = list(&mut r).items[0].clone();
    ack(&mut r, request(&item));
    assert_eq!(
        serde_json::to_value(&r.state).unwrap()["proposal_feed"]["published"],
        0
    );
    let item = list(&mut r).items[0].clone();
    let req = request(&item);
    let outcome = reply(&mut r, req.clone(), "Keep the original decision source");
    assert_eq!(
        serde_json::to_value(&r.state).unwrap()["proposal_feed"]["acknowledged"],
        1
    );
    assert_eq!(
        reply(&mut r, req, "Keep the original decision source").decision_id,
        outcome.decision_id
    );
    assert_eq!(
        serde_json::to_value(&r.state).unwrap()["proposal_feed"]["published"],
        1
    );
}
