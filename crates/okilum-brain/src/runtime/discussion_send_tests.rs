//! Real Runner, canonical SourceWrite and retained-operation recovery boundaries.
use super::goal_brief::tests::{fixture, save_reply};
use super::*;
use crate::{
    application::Application,
    chat::{ChatConfig, ChatEvent},
    discussion_send::{self, Admission, Lookup, SendRequest},
};
use serde_json::json;

fn app() -> Application {
    let mut app = Application::unconfigured();
    app.chat = Some(ChatConfig {
        base_url: "http://127.0.0.1:1/v1".into(),
        model: "fixture-model".into(),
        api_key: "synthetic".into(),
        idle_timeout: std::time::Duration::from_secs(1),
    });
    app
}
fn request(
    r: &Runner,
    goal: &str,
    message: &str,
    conversation_id: Option<String>,
    mut paths: Vec<String>,
) -> SendRequest {
    paths.sort();
    paths.dedup();
    let mut req = SendRequest {
        operation_id: Uuid::new_v4().to_string(),
        goal_id: goal.into(),
        expected_actor_id: app().local_actor().into(),
        conversation_id,
        message: message.into(),
        source_paths: paths,
        request_sha256: String::new(),
    };
    req.request_sha256 = discussion_send::digest(
        &r.state.brain_id,
        goal,
        app().local_actor(),
        req.conversation_id.as_deref(),
        message,
        &req.source_paths,
    )
    .unwrap();
    req
}
fn fresh(
    r: &mut Runner,
    req: SendRequest,
) -> (
    crate::discussion_context::Prepared,
    discussion_send::OperationResult,
) {
    match discussion_send::send(&app(), r, req).unwrap() {
        Admission::NewlyCommitted(p, r) => (*p, r),
        Admission::ExistingOperation(_) => panic!("expected a new projection"),
    }
}
fn existing(r: &mut Runner, req: SendRequest) -> discussion_send::OperationResult {
    match discussion_send::send(&Application::unconfigured(), r, req).unwrap() {
        Admission::ExistingOperation(r) => r,
        Admission::NewlyCommitted(..) => panic!("recovery cannot dispatch"),
    }
}
fn journal(config: &RunnerConfig, operation: &str) -> PathBuf {
    config
        .operational_dir
        .join("source")
        .join(format!("{operation}.json"))
}
fn error_code(error: anyhow::Error) -> String {
    error
        .downcast_ref::<discussion_send::Error>()
        .expect("typed recovery error")
        .value()["code"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn correlated_replay_retains_original_identity_after_sources_and_canonical_disappear() {
    let (_temp, mut r, config, goal, other) = fixture();
    let manual = save_reply(&mut r, &other, "Original manual reference\r\n");
    let req = request(
        &r,
        &goal,
        "Question 😀\r\n",
        None,
        vec![manual.path.clone().unwrap()],
    );
    let (prepared, first) = fresh(&mut r, req.clone());
    assert_eq!(first.status, "projected");
    assert_eq!(first.operation_id, req.operation_id);
    let original = r.source.recovery_record(&req.operation_id).unwrap();
    assert_eq!(original.request.operation_id, req.operation_id);
    let original_bytes = fs::read(journal(&config, &req.operation_id)).unwrap();
    r.with_goal(&goal, |r| {
        Application::chat_event(
            r,
            &prepared.id,
            ChatEvent::Complete {
                text: "Original reply".into(),
            },
        )
    })
    .unwrap();
    let path = first.record.as_ref().unwrap().source_path.clone();
    // Deliberate later canonical edits and missing source inputs do not control recovery.
    fs::write(config.root.join(&path), "# User replaced metadata\n").unwrap();
    fs::remove_file(config.root.join(manual.path.unwrap())).unwrap();
    let state_before = fs::read(config.operational_dir.join("state.json")).unwrap();
    assert_eq!(
        serde_json::to_value(existing(&mut r, req.clone())).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    fs::remove_file(config.root.join(path)).unwrap();
    assert_eq!(
        serde_json::to_value(discussion_send::lookup(&r, &req.key()).unwrap()).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    assert_eq!(
        fs::read(config.operational_dir.join("state.json")).unwrap(),
        state_before
    );
    assert_eq!(
        fs::read(journal(&config, &req.operation_id)).unwrap(),
        original_bytes
    );
    drop(r);
    let r = Runner::open(config).unwrap();
    assert_eq!(
        discussion_send::lookup(&r, &req.key())
            .unwrap()
            .record
            .unwrap()
            .conversation_id,
        prepared.id
    );
}

#[test]
fn distinct_operation_uuid_is_an_explicit_new_send_and_existing_target_keeps_original_journal() {
    let (_temp, mut r, config, goal, _) = fixture();
    let req = request(&r, &goal, "Same question", None, vec![]);
    let (first, a) = fresh(&mut r, req.clone());
    let bytes = fs::read(journal(&config, &req.operation_id)).unwrap();
    let mut next = req.clone();
    next.operation_id = Uuid::new_v4().to_string();
    let (second, b) = fresh(&mut r, next);
    assert_ne!(first.id, second.id);
    assert_ne!(a.record.unwrap().turn_id, b.record.unwrap().turn_id);
    r.with_goal(&goal, |r| {
        Application::chat_event(
            r,
            &first.id,
            ChatEvent::Complete {
                text: "Reply".into(),
            },
        )
    })
    .unwrap();
    let follow = request(&r, &goal, "Follow up", Some(first.id.clone()), vec![]);
    let (_, follow_result) = fresh(&mut r, follow.clone());
    assert_eq!(
        follow_result.record.as_ref().unwrap().conversation_id,
        first.id
    );
    assert_eq!(
        follow_result.record.unwrap().original_conversation_id,
        Some(first.id)
    );
    assert_eq!(
        fs::read(journal(&config, &req.operation_id)).unwrap(),
        bytes
    );
    assert_eq!(existing(&mut r, req).status, "projected");
    assert_eq!(existing(&mut r, follow).status, "projected");
}

#[test]
fn pending_only_and_receipt_before_ack_never_reenter_preparation() {
    for phase in [1, 2, 3] {
        let (_temp, mut r, config, goal, _) = fixture();
        let req = request(&r, &goal, "Original operation", None, vec![]);
        if phase < 3 {
            r.discussion_fault = Some(phase);
        } else {
            r.interrupt_next_source_projection();
        }
        let failure = discussion_send::send(&app(), &mut r, req.clone())
            .err()
            .expect("injected checkpoint failure");
        assert_eq!(error_code(failure), "discussion_send_recovery_error");
        let retained = discussion_send::lookup(&r, &req.key()).unwrap();
        let expected = match phase {
            1 => "unknown",
            2 => "pending_projection",
            _ => "projected",
        };
        assert_eq!(retained.status, expected);
        let state = fs::read(config.operational_dir.join("state.json")).unwrap();
        if phase > 1 {
            let found = existing(&mut r, req.clone());
            assert_eq!(
                serde_json::to_value(&found).unwrap(),
                serde_json::to_value(&retained).unwrap()
            );
            assert_eq!(r.state.pending_writes.len(), 1);
        }
        assert_eq!(
            fs::read(config.operational_dir.join("state.json")).unwrap(),
            state
        );
        drop(r);
        let mut r = Runner::open(config).unwrap();
        if phase > 1 {
            r.with_goal(&goal, |r| Application::unconfigured().recover(r))
                .unwrap();
            let recovered = discussion_send::lookup(&r, &req.key()).unwrap();
            assert_eq!(recovered.status, "projected");
            assert_eq!(
                recovered.record.unwrap().conversation_id,
                retained.record.unwrap().conversation_id
            );
            assert_eq!(existing(&mut r, req).status, "projected");
        } else {
            assert_eq!(
                discussion_send::lookup(&r, &req.key()).unwrap().status,
                "unknown"
            );
        }
    }
}

#[test]
fn stored_owner_payload_or_foreign_uuid_conflicts_without_new_state() {
    let (_temp, mut r, config, goal, other) = fixture();
    let req = request(&r, &goal, "Original", None, vec![]);
    fresh(&mut r, req.clone());
    let state = fs::read(config.operational_dir.join("state.json")).unwrap();
    for mut changed in [
        request(&r, &goal, "Changed", None, vec![]),
        request(&r, &other, "Original", None, vec![]),
    ] {
        changed.operation_id = req.operation_id.clone();
        assert_eq!(
            error_code(
                discussion_send::send(&app(), &mut r, changed)
                    .err()
                    .unwrap()
            ),
            "discussion_send_conflict"
        );
    }
    let mut key = req.key();
    key.expected_actor_id = "previous-actor".into();
    assert_eq!(
        error_code(discussion_send::lookup(&r, &key).unwrap_err()),
        "discussion_send_conflict"
    );
    let foreign = Uuid::new_v4().to_string();
    r.source
        .write(SourceWrite {
            schema: SCHEMA.into(),
            operation_id: foreign.clone(),
            brain_id: r.state.brain_id.clone(),
            path: "foreign.md".into(),
            expected_revision: None,
            content_base64: STANDARD.encode(b"foreign source edit"),
        })
        .unwrap();
    let mut foreign_key = req.key();
    foreign_key.operation_id = foreign;
    assert_eq!(
        error_code(discussion_send::lookup(&r, &foreign_key).unwrap_err()),
        "discussion_send_recovery_error"
    );
    assert_eq!(
        fs::read(config.operational_dir.join("state.json")).unwrap(),
        state
    );
}

#[test]
fn corrupt_mismatched_and_missing_records_never_become_accepted_or_fresh_sends() {
    let (_temp, mut r, config, goal, _) = fixture();
    let req = request(&r, &goal, "Original", None, vec![]);
    fresh(&mut r, req.clone());
    let path = journal(&config, &req.operation_id);
    let original = fs::read(&path).unwrap();
    let json_record: Value = serde_json::from_slice(&original).unwrap();
    for variant in 0..7 {
        let mut value = json_record.clone();
        match variant {
            0 => value["request"]["operation_id"] = json!(Uuid::new_v4().to_string()),
            1 => value["receipt"]["revision"] = json!(format!("sha256:{}", "0".repeat(64))),
            2 => {
                value["receipt"]["previous_revision"] = json!(format!("sha256:{}", "0".repeat(64)))
            }
            3 => value["request"]["brain_id"] = json!(Uuid::new_v4().to_string()),
            4 => {
                value["conflict"] = json!({"conflict_id":req.operation_id,"path":value["request"]["path"],"expected_revision":null,"current_revision":null,"reason":"unmanaged_writers"})
            }
            5 => value["request"]["content_base64"] = json!("broken!"),
            _ => {
                value["base"] = json!({"schema":SCHEMA,"brain_id":value["request"]["brain_id"],"path":value["request"]["path"],"revision":"bad","content_base64":"","media_type":"text/markdown"})
            }
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(
            error_code(discussion_send::lookup(&r, &req.key()).unwrap_err()),
            "discussion_send_recovery_error"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    fs::write(&path, b"broken").unwrap();
    assert_eq!(
        error_code(discussion_send::lookup(&r, &req.key()).unwrap_err()),
        "discussion_send_recovery_error"
    );
    fs::write(&path, &original).unwrap();
    let mut pending: SourceWrite = serde_json::from_value(json_record["request"].clone()).unwrap();
    pending.expected_revision = Some("different".into());
    r.state.pending_writes.push(pending);
    assert_eq!(
        error_code(discussion_send::lookup(&r, &req.key()).unwrap_err()),
        "discussion_send_recovery_error"
    );
    r.state.pending_writes.clear();
    fs::remove_file(path).unwrap();
    let state = fs::read(config.operational_dir.join("state.json")).unwrap();
    let unknown = discussion_send::lookup(&r, &req.key()).unwrap();
    assert_eq!(unknown.status, "unknown");
    assert!(unknown.record.is_none());
    assert_eq!(
        fs::read(config.operational_dir.join("state.json")).unwrap(),
        state
    );
}

#[test]
fn terminal_rejection_requires_a_digest_valid_new_request_before_checkpoint() {
    let (_temp, mut r, config, goal, _) = fixture();
    let req = request(&r, &goal, "", None, vec![]);
    let error = discussion_send::send(&app(), &mut r, req.clone())
        .err()
        .unwrap();
    let value = error
        .downcast_ref::<discussion_send::Error>()
        .unwrap()
        .value();
    assert_eq!(value["code"], "discussion_send_rejected");
    assert_eq!(value["operation_id"], req.operation_id);
    assert_eq!(value["recorded"], false);
    assert!(!journal(&config, &req.operation_id).exists());
    let mut invalid = req.clone();
    invalid.request_sha256 = "0".repeat(64);
    let error = discussion_send::send(&app(), &mut r, invalid)
        .err()
        .unwrap();
    assert_eq!(error_code(error), "discussion_send_recovery_error");
    let unknown = discussion_send::lookup(&r, &req.key()).unwrap();
    assert_eq!(unknown.status, "unknown");
    let max_actor = Lookup {
        expected_actor_id: "x".repeat(4096),
        ..req.key()
    };
    assert!(
        serde_json::to_vec(&discussion_send::lookup(&r, &max_actor).unwrap())
            .unwrap()
            .len()
            < 65536
    );
}

#[test]
fn pending_revision_corruption_and_valid_projection_conflict_stay_lookup_only() {
    let (_temp, mut r, config, goal, _) = fixture();
    let req = request(&r, &goal, "Original", None, vec![]);
    fresh(&mut r, req.clone());
    let path = journal(&config, &req.operation_id);
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let write: SourceWrite = serde_json::from_value(record["request"].clone()).unwrap();
    fs::remove_file(&path).unwrap();
    for expected in ["bad".to_string(), format!("sha256:{}", "0".repeat(64))] {
        let mut corrupt = write.clone();
        corrupt.expected_revision = Some(expected);
        r.state.pending_writes = vec![corrupt];
        assert_eq!(
            error_code(discussion_send::lookup(&r, &req.key()).unwrap_err()),
            "discussion_send_recovery_error"
        );
    }
    r.state.pending_writes = vec![write.clone(), write.clone()];
    assert_eq!(
        error_code(discussion_send::lookup(&r, &req.key()).unwrap_err()),
        "discussion_send_recovery_error"
    );
    r.state.pending_writes.clear();
    record["receipt"] = Value::Null;
    record["conflict"] = json!({"conflict_id":req.operation_id,"path":write.path,"expected_revision":null,"current_revision":null,"reason":"unmanaged_writers"});
    let bytes = serde_json::to_vec(&record).unwrap();
    fs::write(&path, &bytes).unwrap();
    let found = discussion_send::lookup(&r, &req.key()).unwrap();
    assert_eq!(found.status, "projection_conflict");
    assert!(found.source_receipt.is_none());
    assert_eq!(existing(&mut r, req).status, "projection_conflict");
    assert_eq!(fs::read(path).unwrap(), bytes);
}
