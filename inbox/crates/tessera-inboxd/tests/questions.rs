use tessera_inbox_domain::{execution::*, OwnerId};
use tessera_inboxd::store::{Error, Store};
use uuid::Uuid;
fn fixture(store: &mut Store) -> (OwnerId, Question, Reply) {
    let who = OwnerId(Uuid::new_v4());
    let project = Uuid::new_v4();
    store
        .save_execution_project(
            who,
            &SaveProject {
                operation_id: Uuid::new_v4(),
                project_id: project,
                expected_revision: 0,
                draft: ProjectDraft {
                    title: "Pilot".into(),
                    status: String::new(),
                    next_step: String::new(),
                },
            },
        )
        .unwrap();
    let q = Question {
        id: Uuid::new_v4(),
        project_id: project,
        source: QuestionSource {
            kind: SourceKind::T3,
            instance_id: "instance".into(),
            project_id: "project".into(),
            thread_id: "thread".into(),
            question_id: "native-request".into(),
            generation: "attempt".into(),
        },
        source_revision: "opaque-1".into(),
        state: QuestionState::Pending,
        can_reply: true,
        fields: vec![QuestionField {
            id: "colour".into(),
            prompt: "Colour?".into(),
            options: vec![QuestionOption {
                id: "blue".into(),
                label: "Blue".into(),
            }],
            allow_text: false,
            multiple: false,
        }],
    };
    store.observe_execution_question(who, &q, 1).unwrap();
    let r = Reply {
        operation_id: Uuid::new_v4(),
        question_id: q.id,
        expected_revision: q.source_revision.clone(),
        answers: vec![AnswerField {
            id: "colour".into(),
            text: String::new(),
            option_ids: vec!["blue".into()],
        }],
    };
    (who, q, r)
}
#[test]
fn reply_snapshot_replay_reservation_and_uncertain_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (who, mut q, r) = fixture(&mut store);
    let op = store.prepare_execution_reply(who, &r).unwrap();
    assert_eq!(op.state, DeliveryState::Queued);
    let visible = store.execution_question(who, q.id).unwrap().unwrap();
    assert!(!visible.question.can_reply);
    assert_eq!(visible.pending_operation_id, Some(r.operation_id));
    let mut other = r.clone();
    other.operation_id = Uuid::new_v4();
    assert!(matches!(
        store.prepare_execution_reply(who, &other),
        Err(Error::ExecutionRevisionConflict)
    ));
    assert!(store
        .execution_reply(OwnerId(Uuid::new_v4()), r.operation_id)
        .unwrap()
        .is_none());
    store
        .advance_execution_reply(
            who,
            r.operation_id,
            DeliveryState::Queued,
            DeliveryState::Uncertain,
            None,
            None,
        )
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.prepare_execution_reply(who, &r).unwrap().state,
        DeliveryState::Uncertain
    );
    q.source_revision = "opaque-2".into();
    q.state = QuestionState::Withdrawn;
    q.can_reply = false;
    store.observe_execution_question(who, &q, 2).unwrap();
    assert_eq!(
        store.prepare_execution_reply(who, &r).unwrap().question,
        op.question
    );
    let mut changed = r.clone();
    changed.answers[0].option_ids.clear();
    assert!(matches!(
        store.prepare_execution_reply(who, &changed),
        Err(Error::OperationConflict)
    ));
    assert!(store
        .advance_execution_reply(
            who,
            r.operation_id,
            DeliveryState::Uncertain,
            DeliveryState::Queued,
            None,
            None
        )
        .is_err());
    store
        .advance_execution_reply(
            who,
            r.operation_id,
            DeliveryState::Uncertain,
            DeliveryState::Accepted,
            None,
            None,
        )
        .unwrap();
    let done = store
        .advance_execution_reply(
            who,
            r.operation_id,
            DeliveryState::Accepted,
            DeliveryState::Delivered,
            Some("native-ack"),
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .advance_execution_reply(
                who,
                r.operation_id,
                DeliveryState::Accepted,
                DeliveryState::Delivered,
                Some("native-ack"),
                None
            )
            .unwrap(),
        done
    );
    assert!(store
        .advance_execution_reply(
            who,
            r.operation_id,
            DeliveryState::Delivered,
            DeliveryState::Rejected,
            None,
            Some("no")
        )
        .is_err());
}
#[test]
fn observations_reject_rebinding_cursor_regression_and_duplicate_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("db")).unwrap();
    let (who, q, _) = fixture(&mut store);
    store.observe_execution_question(who, &q, 1).unwrap();
    let mut same_revision = q.clone();
    same_revision.fields[0].prompt = "Different content".into();
    assert!(store
        .observe_execution_question(who, &same_revision, 2)
        .is_err());
    let mut newer = q.clone();
    newer.source_revision = "r2".into();
    newer.fields[0].prompt = "Updated?".into();
    assert!(store.observe_execution_question(who, &newer, 1).is_err());
    store.observe_execution_question(who, &newer, 2).unwrap();
    assert!(store.observe_execution_question(who, &q, 1).is_err());
    let mut duplicate = newer.clone();
    duplicate.id = Uuid::new_v4();
    assert!(store
        .observe_execution_question(who, &duplicate, 3)
        .is_err());
    newer.source.generation = "replacement-worker".into();
    assert!(store.observe_execution_question(who, &newer, 3).is_err());
    assert!(store
        .execution_question(OwnerId(Uuid::new_v4()), q.id)
        .unwrap()
        .is_none());
}
#[test]
fn simultaneous_replies_reserve_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (who, _, r) = fixture(&mut store);
    drop(store);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let jobs: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let mut r = r.clone();
            r.operation_id = Uuid::new_v4();
            let b = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                b.wait();
                store.prepare_execution_reply(who, &r)
            })
        })
        .collect();
    let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::ExecutionRevisionConflict)))
            .count(),
        1
    );
}

#[test]
fn only_definite_rejection_releases_reservation_and_v6_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (who, q, r) = fixture(&mut store);
    store.prepare_execution_reply(who, &r).unwrap();
    store
        .advance_execution_reply(
            who,
            r.operation_id,
            DeliveryState::Queued,
            DeliveryState::Rejected,
            None,
            Some("source_stale"),
        )
        .unwrap();
    let mut next = r.clone();
    next.operation_id = Uuid::new_v4();
    assert_eq!(
        store.prepare_execution_reply(who, &next).unwrap().state,
        DeliveryState::Queued
    );
    assert_eq!(
        store.prepare_execution_reply(who, &r).unwrap().state,
        DeliveryState::Rejected
    );
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "DROP TABLE execution_replies; DROP TABLE execution_questions; PRAGMA user_version=6;",
    )
    .unwrap();
    drop(db);
    let store = Store::open(&path).unwrap();
    assert!(store
        .execution_project(who, q.project_id)
        .unwrap()
        .is_some());
    assert!(store.execution_question(who, q.id).unwrap().is_none());
}
