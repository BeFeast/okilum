use super::*;
const BRAIN: &str = "01000000-0000-4000-8000-000000000086";
const OTHER: &str = "02000000-0000-4000-8000-000000000086";
const RECORD: &str = "11111111-1111-4111-8111-111111111111";
const GOAL: &str = "22222222-2222-4222-8222-222222222222";
fn trigger() -> CommittedTrigger {
    CommittedTrigger {
        identity: Identity {
            brain_id: BRAIN.into(),
            kind: TriggerKind::Inbox,
            record_id: RECORD.into(),
            source_revision: "sha256:abc:def".into(),
            policy_version: 1,
        },
        goal_id: None,
        source_path: format!("records/inbox-{RECORD}.md"),
        received_at: "2026-09-07T05:20:00Z".into(),
    }
}
fn input() -> FrozenInput {
    FrozenInput {
        generation: None,
        provider_identity: "saved-chat:primary".into(),
        model: "fixture-model".into(),
        input_sha256: "a".repeat(64),
    }
}
fn bytes(root: &Path) -> Vec<u8> {
    fs::read(root.join("proposal-intents-v1/journal.json")).unwrap()
}
fn accepted(store: &mut Store, sequence: u64, trigger: CommittedTrigger) -> String {
    match store.enqueue(sequence, trigger).unwrap() {
        Enqueue::Accepted { proposal_id } => proposal_id,
        result => panic!("unexpected {result:?}"),
    }
}
#[test]
fn cross_language_fixed_vectors_count_utf8_bytes_and_keep_delimiters() {
    // Independently generated with Python UTF-8 and hashlib, not serde or Rust.
    let mut identity = trigger().identity;
    assert_eq!(
        identity.id().unwrap(),
        "d8a330a71cb6e1212c271bbc9b54d41bad906e258aa018eaa7eed93ca5566ff6"
    );
    identity.source_revision = "é/雪:12:🧠".into();
    assert_eq!(identity.source_revision.len(), 14);
    assert_eq!(
        identity.id().unwrap(),
        "126a816e24a0ffd4ccbc4a51aa80daec52aec4af05ded347703a795c55d9a5de"
    );
    let baseline = identity.id().unwrap();
    identity.policy_version = 12;
    assert_ne!(identity.id().unwrap(), baseline);
    identity.policy_version = 0;
    assert!(identity.id().is_err());
    identity.policy_version = 1;
    identity.record_id = Uuid::parse_str(RECORD).unwrap().simple().to_string();
    assert!(identity.id().is_err());
}
#[test]
fn activation_is_explicit_owner_bound_and_cannot_be_rebased() {
    let root = tempfile::tempdir().unwrap();
    assert!(Store::open(root.path(), BRAIN).is_err());
    assert!(!root.path().join("proposal-intents-v1").exists());
    let store = Store::initialize(root.path(), BRAIN, 10).unwrap();
    assert_eq!(store.cursor().unwrap(), 10);
    let saved = bytes(root.path());
    assert!(
        Store::open(root.path(), BRAIN).is_err(),
        "second writer must not acquire lock"
    );
    drop(store);
    assert!(Store::initialize(root.path(), BRAIN, 20).is_err());
    assert!(Store::open(root.path(), OTHER).is_err());
    assert_eq!(bytes(root.path()), saved);
    assert_eq!(
        Store::open(root.path(), BRAIN).unwrap().cursor().unwrap(),
        10
    );
}

#[test]
fn retained_proposal_reopens_with_canonical_json_object_order() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use tessera_core::source::SourceSnapshot;

    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    store.journal.drafts_enabled = true;
    let source = |path: String, text: &str| SourceSnapshot {
        schema: tessera_core::source::SCHEMA.into(),
        brain_id: BRAIN.into(),
        path,
        revision: format!("sha256:{:x}", Sha256::digest(text.as_bytes())),
        content_base64: STANDARD.encode(text),
        media_type: "text/markdown".into(),
    };
    let mut trigger = trigger();
    trigger.goal_id = Some(GOAL.into());
    trigger.identity.kind = TriggerKind::Result;
    let trigger_source = source(trigger.source_path.clone(), "Retained input\n");
    trigger.identity.source_revision = trigger_source.revision.clone();
    let goal_source = source(format!("records/goal-{GOAL}.md"), "Retained goal\n");
    // These literal keys deliberately differ from sorted order, including nesting.
    // A same-build round trip alone would hide a lost preserve_order feature.
    let mut brief: serde_json::Value =
        serde_json::from_str(r#"{"schema":"fixture/v1","nested":{"z":1,"a":2}}"#).unwrap();
    brief["goal_id"] = GOAL.into();
    brief["goal_revision"] = goal_source.revision.clone().into();
    let captured = crate::proposal::CapturedInput {
        trigger_source,
        citations: vec![],
        goal_revision: Some(goal_source.revision.clone()),
        omissions: vec![],
    };
    let payload = serde_json::json!({
        "trigger": trigger, "trigger_text": "Retained input\n",
        "citations": [], "omissions": [], "goal_text": "Retained goal\n",
        "goal_brief": brief,
    });
    let request_body = serde_json::json!({
        "model": "fixture-model", "stream": true,
        "messages": [{"role": "system", "content": "Fixture"},
            {"role": "user", "content": payload.to_string()}],
    })
    .to_string();
    let mut frozen = input();
    frozen.input_sha256 = format!("{:x}", Sha256::digest(request_body.as_bytes()));
    frozen.generation = Some(crate::proposal::GenerationInput {
        schema: "tessera-proposal-prompt/v1".into(),
        settings: None,
        goal_brief: Some(brief),
        goal_source: Some(goal_source),
        request_body,
    });
    let id = accepted(&mut store, 1, trigger);
    store
        .begin_draft(
            BRAIN,
            Some(GOAL),
            &id,
            DraftStart {
                input: frozen,
                captured,
                actor: "fixture".into(),
                at: "2026-09-07T05:20:00Z".into(),
                path: format!("records/proposal-{id}.md"),
            },
        )
        .unwrap();
    // Persist a terminal attempt so startup validates without recovery writes.
    store.recover().unwrap();
    let draft = store.journal.intents[&id].draft.as_ref().unwrap();
    let retained = STANDARD
        .decode(&draft.projections.last().unwrap().write.content_base64)
        .unwrap();
    assert!(std::str::from_utf8(&retained)
        .unwrap()
        .contains("        schema: fixture/v1\n        nested:\n          z: 1\n          a: 2\n"));
    let journal = bytes(root.path());
    drop(store);
    let reopened = Store::open(root.path(), BRAIN).unwrap();
    assert_eq!(
        reopened.journal.intents[&id]
            .draft
            .as_ref()
            .unwrap()
            .record
            .bytes()
            .unwrap(),
        retained
    );
    assert_eq!(bytes(root.path()), journal);
}
#[test]
fn cursor_requires_contiguous_committed_events_and_exact_replay_metadata() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 10).unwrap();
    let saved = bytes(root.path());
    assert!(store.enqueue(10, trigger()).is_err());
    assert!(store.enqueue(12, trigger()).is_err());
    assert_eq!(bytes(root.path()), saved);
    let id = accepted(&mut store, 11, trigger());
    let attempt = store.get(BRAIN, None, &id).unwrap().attempt.clone();
    let saved = bytes(root.path());
    assert_eq!(
        store.enqueue(11, trigger()).unwrap(),
        Enqueue::Replay {
            proposal_id: id.clone()
        }
    );
    let mut changed = trigger();
    changed.source_path = "other/source.md".into();
    assert!(store.enqueue(11, changed).is_err());
    let mut changed = trigger();
    changed.received_at = "2026-09-07T05:21:00Z".into();
    assert!(store.enqueue(11, changed).is_err());
    let mut changed = trigger();
    changed.identity.source_revision = "changed".into();
    assert!(store.enqueue(11, changed.clone()).is_err());
    assert!(
        store.enqueue(12, changed).is_err(),
        "source edit cannot masquerade as new committed record"
    );
    let mut changed = trigger();
    changed.identity.policy_version = 2;
    assert!(
        store.enqueue(12, changed).is_err(),
        "policy change cannot replay old record"
    );
    assert_eq!(bytes(root.path()), saved);
    assert_eq!(
        store.enqueue(12, trigger()).unwrap(),
        Enqueue::Replay {
            proposal_id: id.clone()
        }
    );
    drop(store);
    let store = Store::open(root.path(), BRAIN).unwrap();
    assert_eq!(store.cursor().unwrap(), 12);
    assert_eq!(store.get(BRAIN, None, &id).unwrap().attempt, attempt);
    assert_eq!(store.journal.intents.len(), 1);
}
#[test]
fn queue_backpressure_survives_restart_and_does_not_lose_the_next_trigger() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let first = accepted(&mut store, 1, trigger());
    for seq in 2..=32 {
        let mut t = trigger();
        t.identity.record_id = Uuid::new_v4().to_string();
        accepted(&mut store, seq, t);
    }
    let mut next = trigger();
    next.identity.record_id = Uuid::new_v4().to_string();
    let saved = bytes(root.path());
    assert_eq!(
        store.enqueue(33, next.clone()).unwrap(),
        Enqueue::Backlog { cursor: 32 }
    );
    assert_eq!(bytes(root.path()), saved);
    assert_eq!(
        store.enqueue(1, trigger()).unwrap(),
        Enqueue::Replay {
            proposal_id: first.clone()
        }
    );
    drop(store);
    let mut store = Store::open(root.path(), BRAIN).unwrap();
    assert_eq!(
        store.enqueue(33, next.clone()).unwrap(),
        Enqueue::Backlog { cursor: 32 }
    );
    assert!(
        store
            .mark_running(BRAIN, None, &first, input())
            .unwrap()
            .newly_running
    );
    let new_id = accepted(&mut store, 33, next.clone());
    assert_eq!(store.cursor().unwrap(), 33);
    drop(store);
    let store = Store::open(root.path(), BRAIN).unwrap();
    assert_eq!(store.get(BRAIN, None, &new_id).unwrap().trigger, next);
    assert_eq!(store.journal.intents.len(), 33);
}
#[test]
fn three_trigger_kinds_keep_brain_goal_ownership_and_original_source() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    for (sequence, kind) in [
        TriggerKind::Inbox,
        TriggerKind::Decision,
        TriggerKind::Result,
    ]
    .into_iter()
    .enumerate()
    {
        let mut t = trigger();
        t.identity.kind = kind;
        if kind != TriggerKind::Inbox {
            t.goal_id = Some(GOAL.into());
        }
        let id = accepted(&mut store, sequence as u64 + 1, t.clone());
        assert_eq!(
            store.get(BRAIN, t.goal_id.as_deref(), &id).unwrap().trigger,
            t
        );
        assert!(store.get(OTHER, t.goal_id.as_deref(), &id).is_err());
        assert!(store.get(BRAIN, Some(OTHER), &id).is_err());
        assert!(store
            .mark_running(OTHER, t.goal_id.as_deref(), &id, input())
            .is_err());
    }
}
#[test]
fn invalid_trigger_metadata_is_rejected_before_cursor_or_disk_changes() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let before = bytes(root.path());
    let mut invalid = Vec::new();
    let mut t = trigger();
    t.identity.brain_id = OTHER.into();
    invalid.push(t);
    let mut t = trigger();
    t.goal_id = Some(GOAL.into());
    invalid.push(t);
    let mut t = trigger();
    t.identity.kind = TriggerKind::Result;
    invalid.push(t);
    let mut t = trigger();
    t.source_path = "../other.md".into();
    invalid.push(t);
    let mut t = trigger();
    t.source_path = "/absolute.md".into();
    invalid.push(t);
    let mut t = trigger();
    t.source_path = "records\\other.md".into();
    invalid.push(t);
    let mut t = trigger();
    t.received_at = "2026-09-07T08:20:00+03:00".into();
    invalid.push(t);
    let mut t = trigger();
    t.received_at = "not-time".into();
    invalid.push(t);
    for t in invalid {
        assert!(store.enqueue(1, t).is_err());
        assert_eq!(store.cursor().unwrap(), 0);
        assert_eq!(bytes(root.path()), before);
    }
    accepted(&mut store, 1, trigger()); // Positive control: the stream can progress.
}
#[test]
fn dispatch_is_recorded_once_and_restart_never_requeues_or_grants_replay() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let id = accepted(&mut store, 1, trigger());
    let started = store.mark_running(BRAIN, None, &id, input()).unwrap();
    assert!(started.newly_running);
    assert_eq!(
        store.mark_running(BRAIN, None, &id, input()).unwrap(),
        Start {
            attempt: started.attempt.clone(),
            newly_running: false
        }
    );
    let mut changed = input();
    changed.model = "different-model".into();
    assert!(store.mark_running(BRAIN, None, &id, changed).is_err());
    let mut other = trigger();
    other.identity.record_id = Uuid::new_v4().to_string();
    let other_id = accepted(&mut store, 2, other);
    assert!(store.mark_running(BRAIN, None, &other_id, input()).is_err());
    drop(store);
    let mut store = Store::open(root.path(), BRAIN).unwrap();
    let recovered = store.mark_running(BRAIN, None, &id, input()).unwrap();
    assert!(!recovered.newly_running);
    assert_eq!(recovered.attempt.state, AttemptState::Interrupted);
    assert_eq!(recovered.attempt.id, started.attempt.id);
    assert_eq!(recovered.attempt.input, Some(input()));
    let bytes_after_recovery = bytes(root.path());
    drop(store);
    let mut store = Store::open(root.path(), BRAIN).unwrap();
    assert_eq!(
        bytes(root.path()),
        bytes_after_recovery,
        "second recovery must not rewrite or retry"
    );
    assert_eq!(
        store.mark_running(BRAIN, None, &id, input()).unwrap(),
        recovered
    );
    assert!(
        store
            .mark_running(BRAIN, None, &other_id, input())
            .unwrap()
            .newly_running
    );
}
#[test]
fn failed_atomic_commit_requires_reopen_and_recovers_all_or_none_of_event_and_cursor() {
    for fault in [Fault::BeforeReplace, Fault::AfterReplace] {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::initialize(root.path(), BRAIN, 20).unwrap();
        store.fault = Some(fault);
        assert!(store.enqueue(21, trigger()).is_err());
        assert!(store.cursor().is_err());
        assert!(
            store.enqueue(21, trigger()).is_err(),
            "uncertain writer must not acknowledge in-memory replay"
        );
        drop(store);
        let mut store = Store::open(root.path(), BRAIN).unwrap();
        let (cursor, count) = if fault == Fault::BeforeReplace {
            (20, 0)
        } else {
            (21, 1)
        };
        assert_eq!(store.cursor().unwrap(), cursor);
        assert_eq!(store.journal.intents.len(), count);
        let response = store.enqueue(21, trigger()).unwrap();
        assert_eq!(
            matches!(response, Enqueue::Replay { .. }),
            fault == Fault::AfterReplace
        );
        assert_eq!(store.cursor().unwrap(), 21);
        assert_eq!(store.journal.intents.len(), 1);
    }
}
#[test]
fn lost_dispatch_ack_recovery_keeps_attempt_id_and_never_issues_second_permit() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let id = accepted(&mut store, 1, trigger());
    let attempt_id = store.get(BRAIN, None, &id).unwrap().attempt.id.clone();
    store.fault = Some(Fault::AfterReplace);
    assert!(store.mark_running(BRAIN, None, &id, input()).is_err());
    drop(store);
    let mut store = Store::open(root.path(), BRAIN).unwrap();
    let recovered = store.mark_running(BRAIN, None, &id, input()).unwrap();
    assert!(!recovered.newly_running);
    assert_eq!(recovered.attempt.id, attempt_id);
    assert_eq!(recovered.attempt.state, AttemptState::Interrupted);
}
#[test]
fn unsupported_or_inconsistent_journal_refuses_before_recovery_writes() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let id = accepted(&mut store, 1, trigger());
    store.mark_running(BRAIN, None, &id, input()).unwrap();
    drop(store);
    let path = root.path().join("proposal-intents-v1/journal.json");
    let valid: serde_json::Value = serde_json::from_slice(&bytes(root.path())).unwrap();
    for field in ["schema", "brain_id", "cursor", "unknown"] {
        let mut corrupt = valid.clone();
        corrupt[field] = match field {
            "cursor" => serde_json::json!(5),
            _ => serde_json::json!("changed"),
        };
        fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        let before = bytes(root.path());
        assert!(Store::open(root.path(), BRAIN).is_err());
        assert_eq!(bytes(root.path()), before);
    }
    fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
    assert_eq!(
        Store::open(root.path(), BRAIN)
            .unwrap()
            .get(BRAIN, None, &id)
            .unwrap()
            .attempt
            .state,
        AttemptState::Interrupted
    );
}
#[test]
fn deleting_derived_index_does_not_remove_trigger_or_attempt_receipts() {
    let root = tempfile::tempdir().unwrap();
    let index = root.path().join("deletable-index");
    fs::create_dir(&index).unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let id = accepted(&mut store, 1, trigger());
    let intent = store.get(BRAIN, None, &id).unwrap().clone();
    drop(store);
    fs::remove_dir_all(index).unwrap();
    let store = Store::open(root.path(), BRAIN).unwrap();
    assert_eq!(*store.get(BRAIN, None, &id).unwrap(), intent);
}

#[test]
fn recovery_failure_never_acknowledges_or_resends_an_uncertain_attempt() {
    for fault in [Fault::BeforeReplace, Fault::AfterReplace] {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
        let id = accepted(&mut store, 1, trigger());
        let started = store.mark_running(BRAIN, None, &id, input()).unwrap();
        store.fault = Some(fault);
        assert!(store.recover().is_err());
        assert!(store.get(BRAIN, None, &id).is_err());
        assert!(store.mark_running(BRAIN, None, &id, input()).is_err());
        drop(store);
        let mut store = Store::open(root.path(), BRAIN).unwrap();
        let recovered = store.mark_running(BRAIN, None, &id, input()).unwrap();
        assert!(!recovered.newly_running);
        assert_eq!(recovered.attempt.id, started.attempt.id);
        assert_eq!(recovered.attempt.state, AttemptState::Interrupted);
    }
}
#[test]
fn framing_separates_valid_tuples_that_collide_without_field_boundaries() {
    let mut a = trigger().identity;
    a.source_revision = "revision:1".into();
    a.policy_version = 2;
    let mut b = trigger().identity;
    b.source_revision = "revision:".into();
    b.policy_version = 12;
    assert_eq!(
        format!("{}{}", a.source_revision, a.policy_version),
        format!("{}{}", b.source_revision, b.policy_version)
    );
    assert_ne!(a.id().unwrap(), b.id().unwrap());
}

#[test]
fn corrupted_duplicate_record_or_attempt_refuses_before_running_recovery() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let first = accepted(&mut store, 1, trigger());
    let mut other = trigger();
    other.identity.record_id = Uuid::new_v4().to_string();
    let second = accepted(&mut store, 2, other);
    store.mark_running(BRAIN, None, &first, input()).unwrap();
    let valid = store.journal.clone();
    drop(store);
    let path = root.path().join("proposal-intents-v1/journal.json");
    for duplicate_record in [true, false] {
        let mut corrupt = valid.clone();
        if duplicate_record {
            let mut intent = corrupt.intents.remove(&second).unwrap();
            intent.trigger.identity.record_id = RECORD.into();
            intent.trigger.identity.source_revision = "different-revision".into();
            let new_id = intent.trigger.identity.id().unwrap();
            corrupt.events.insert(2, new_id.clone());
            corrupt.intents.insert(new_id, intent);
        } else {
            let id = corrupt.intents.get(&first).unwrap().attempt.id.clone();
            corrupt.intents.get_mut(&second).unwrap().attempt.id = id;
        }
        fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        let before = bytes(root.path());
        assert!(Store::open(root.path(), BRAIN).is_err());
        assert_eq!(bytes(root.path()), before);
    }
    fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
    assert_eq!(
        Store::open(root.path(), BRAIN)
            .unwrap()
            .get(BRAIN, None, &first)
            .unwrap()
            .attempt
            .state,
        AttemptState::Interrupted
    );
}

#[test]
fn capacity_reserves_running_recovery_at_the_exact_journal_byte_limit() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let id = accepted(&mut store, 1, trigger());
    let started = store.mark_running(BRAIN, None, &id, input()).unwrap();
    let before = bytes(root.path());
    let mut full = store.journal.clone();
    let target = MAX_STATE_BYTES as usize - 2;
    let mut length = serde_json::to_vec(&full).unwrap().len();
    // Duplicate deliveries consume receipt capacity without adding a queue item.
    // Account for JSON entry delimiters and changes in decimal cursor width.
    loop {
        let next = full.cursor + 1;
        let growth = next.to_string().len() + id.len() + 6 + next.to_string().len()
            - full.cursor.to_string().len();
        if length + growth > target {
            break;
        }
        full.events.insert(next, id.clone());
        full.cursor = next;
        length += growth;
    }
    full.intents
        .get_mut(&id)
        .unwrap()
        .attempt
        .input
        .as_mut()
        .unwrap()
        .provider_identity
        .push_str(&"p".repeat(target - length));
    assert_eq!(serde_json::to_vec(&full).unwrap().len(), target);
    full.validate(BRAIN).unwrap();
    // Before the fix this exact state is acknowledged, then cannot reopen:
    // running→interrupted adds four bytes and exceeds the16MiB maximum by two.
    if store.commit(full.clone()).is_ok() {
        drop(store);
        assert!(Store::open(root.path(), BRAIN).is_err());
        panic!("acknowledged a running journal that cannot persist restart recovery");
    }
    assert_eq!(bytes(root.path()), before);
    assert_eq!(
        store.get(BRAIN, None, &id).unwrap().attempt.state,
        AttemptState::Running
    );
    // Exact accepted boundary: running occupies MAX-4; recovery occupies MAX.
    let frozen = full
        .intents
        .get_mut(&id)
        .unwrap()
        .attempt
        .input
        .as_mut()
        .unwrap();
    frozen
        .provider_identity
        .truncate(frozen.provider_identity.len() - 2);
    let frozen = frozen.clone();
    assert_eq!(
        serde_json::to_vec(&full).unwrap().len(),
        MAX_STATE_BYTES as usize - 4
    );
    store.commit(full).unwrap();
    drop(store);
    let mut store = Store::open(root.path(), BRAIN).unwrap();
    assert_eq!(bytes(root.path()).len(), MAX_STATE_BYTES as usize);
    let recovered = store.mark_running(BRAIN, None, &id, frozen).unwrap();
    assert!(!recovered.newly_running);
    assert_eq!(recovered.attempt.id, started.attempt.id);
    assert_eq!(recovered.attempt.state, AttemptState::Interrupted);
    let durable = bytes(root.path());
    drop(store);
    let store = Store::open(root.path(), BRAIN).unwrap();
    assert_eq!(bytes(root.path()), durable);
    assert_eq!(
        store.get(BRAIN, None, &id).unwrap().attempt,
        recovered.attempt
    );
}

#[test]
fn bound_headers_refuse_legacy_epoch_activation_and_policy_changes_before_recovery() {
    let root = tempfile::tempdir().unwrap();
    let mut old = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let id = accepted(&mut old, 1, trigger());
    old.mark_running(BRAIN, None, &id, input()).unwrap();
    drop(old);
    let binding = tessera_core::source::RequiredProposalFeed {
        capability: "tessera-proposal-feed/v1".into(),
        epoch: Uuid::new_v4().to_string(),
        activation_id: Uuid::new_v4().to_string(),
        policy_version: 1,
        active: true,
    };
    let before = bytes(root.path());
    assert!(Store::open_bound(root.path(), BRAIN, Some(&binding)).is_err());
    assert_eq!(bytes(root.path()), before);
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize_bound(root.path(), BRAIN, 0, Some(binding.clone())).unwrap();
    let id = match store.enqueue_event(1, trigger(), "a".repeat(64)).unwrap() {
        Enqueue::Accepted { proposal_id } => proposal_id,
        _ => panic!(),
    };
    store.mark_running(BRAIN, None, &id, input()).unwrap();
    drop(store);
    let before = bytes(root.path());
    assert!(Store::open(root.path(), BRAIN).is_err());
    for field in ["epoch", "activation", "policy"] {
        let mut wrong = binding.clone();
        match field {
            "epoch" => wrong.epoch = Uuid::new_v4().to_string(),
            "activation" => wrong.activation_id = Uuid::new_v4().to_string(),
            _ => wrong.policy_version = 2,
        };
        assert!(Store::open_bound(root.path(), BRAIN, Some(&wrong)).is_err());
        assert_eq!(bytes(root.path()), before);
    }
    assert_eq!(
        Store::inspect_bound(root.path(), BRAIN, &binding).unwrap(),
        1
    );
    assert_eq!(bytes(root.path()), before);
    let mut store = Store::open_bound(root.path(), BRAIN, Some(&binding)).unwrap();
    assert!(store.enqueue_event(1, trigger(), "b".repeat(64)).is_err());
    assert!(matches!(
        store.enqueue_event(1, trigger(), "a".repeat(64)).unwrap(),
        Enqueue::Replay { .. }
    ));
}

#[test]
fn generation_reserves_result_publication_before_marking_running() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::initialize(root.path(), BRAIN, 0).unwrap();
    let id = accepted(&mut store, 1, trigger());
    let mut full = store.journal.clone();
    let target = MAX_STATE_BYTES as usize - 2 * 1024 * 1024;
    let mut length = serde_json::to_vec(&full).unwrap().len();
    loop {
        let next = full.cursor + 1;
        let growth = next.to_string().len() + id.len() + 6 + next.to_string().len()
            - full.cursor.to_string().len();
        if length + growth > target {
            break;
        }
        full.events.insert(next, id.clone());
        full.cursor = next;
        length += growth;
    }
    store.commit(full).unwrap();
    let before = bytes(root.path());
    let request_body=serde_json::json!({"model":"fixture-model","stream":true,"messages":[{"role":"user","content":"retained"}]}).to_string();
    let mut frozen = input();
    frozen.input_sha256 = format!("{:x}", Sha256::digest(request_body.as_bytes()));
    frozen.generation = Some(crate::proposal::GenerationInput {
        schema: "tessera-proposal-prompt/v1".into(),
        settings: None,
        goal_brief: None,
        goal_source: None,
        request_body,
    });
    frozen.validate().unwrap();
    let positive_root = tempfile::tempdir().unwrap();
    let mut positive = Store::initialize(positive_root.path(), BRAIN, 0).unwrap();
    let positive_id = accepted(&mut positive, 1, trigger());
    assert!(
        positive
            .mark_running(BRAIN, None, &positive_id, frozen.clone())
            .unwrap()
            .newly_running
    );
    let error = store.mark_running(BRAIN, None, &id, frozen).unwrap_err();
    assert!(
        error.to_string().contains("including recovery reserve"),
        "{error}"
    );
    assert_eq!(bytes(root.path()), before);
    assert_eq!(
        store.get(BRAIN, None, &id).unwrap().attempt.state,
        AttemptState::Queued
    );
}
