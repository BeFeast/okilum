use okilum_inbox_domain::{execution::*, OwnerId};
use okilum_inboxd::{
    launch::*,
    store::{Error, Store},
};
use uuid::Uuid;
fn fixture(store: &mut Store) -> (OwnerId, TargetSnapshot, Launch) {
    let owner = OwnerId(Uuid::new_v4());
    let project = Uuid::new_v4();
    let brief = Uuid::new_v4();
    store
        .save_execution_project(
            owner,
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
    store
        .save_execution_brief(
            owner,
            &SaveBrief {
                operation_id: Uuid::new_v4(),
                project_id: project,
                brief_id: brief,
                expected_revision: 0,
                title: "One executor".into(),
                text: "Print pilot marker only".into(),
                target_id: "pilot".into(),
            },
        )
        .unwrap();
    let target = TargetSnapshot {
        project_id: project,
        instance_id: "t3-instance".into(),
        source_project_id: "source-pilot".into(),
        target: Target {
            id: "pilot".into(),
            label: "Isolated pilot".into(),
            repository: "fixture".into(),
            base_commit: "a".repeat(40),
            model_selection: serde_json::json!({"instanceId":"codex","model":"test"}),
            runtime_mode: "approval-required".into(),
            interaction_mode: "default".into(),
        },
    };
    let request = Launch {
        operation_id: Uuid::new_v4(),
        brief_id: brief,
        expected_revision: 1,
        target_revision: target.revision(),
    };
    (owner, target, request)
}
#[test]
fn immutable_launch_replay_survives_restart_target_revocation_and_brief_update() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (owner, target, r) = fixture(&mut store);
    let op = store.prepare_execution_launch(owner, &r, &target).unwrap();
    assert_ne!(op.thread_id, op.message_id);
    store
        .save_execution_brief(
            owner,
            &SaveBrief {
                operation_id: Uuid::new_v4(),
                project_id: target.project_id,
                brief_id: r.brief_id,
                expected_revision: 1,
                title: "Changed".into(),
                text: "Changed draft".into(),
                target_id: "pilot".into(),
            },
        )
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    let mut revoked = target.clone();
    revoked.target.model_selection = serde_json::json!({});
    assert_eq!(
        store.prepare_execution_launch(owner, &r, &revoked).unwrap(),
        op
    );
    let mut altered = r.clone();
    altered.expected_revision = 2;
    assert!(matches!(
        store.prepare_execution_launch(owner, &altered, &target),
        Err(Error::OperationConflict)
    ));
    altered.operation_id = Uuid::new_v4();
    altered.expected_revision = 1;
    assert!(matches!(
        store.prepare_execution_launch(owner, &altered, &target),
        Err(Error::ExecutionRevisionConflict)
    ));
    assert!(store
        .execution_launch(OwnerId(Uuid::new_v4()), r.operation_id)
        .unwrap()
        .is_none());
}
#[test]
fn one_concurrent_launch_per_brief_revision() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let (owner, target, r) = fixture(&mut store);
    drop(store);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let jobs: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let target = target.clone();
            let mut r = r.clone();
            r.operation_id = Uuid::new_v4();
            let b = barrier.clone();
            std::thread::spawn(move || {
                let mut s = Store::open(&path).unwrap();
                b.wait();
                s.prepare_execution_launch(owner, &r, &target)
            })
        })
        .collect();
    let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::OperationConflict)))
            .count(),
        1
    );
}
#[test]
fn target_scope_and_fingerprint_are_not_authorized_by_draft() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("db")).unwrap();
    let (owner, t, r) = fixture(&mut s);
    let mut changed = t.clone();
    changed.target.base_commit = "b".repeat(40);
    assert!(s.prepare_execution_launch(owner, &r, &changed).is_err());
    changed = t.clone();
    changed.project_id = Uuid::new_v4();
    let mut request = r.clone();
    request.target_revision = changed.revision();
    assert!(s
        .prepare_execution_launch(owner, &request, &changed)
        .is_err());
    assert!(s
        .prepare_execution_launch(OwnerId(Uuid::new_v4()), &r, &t)
        .is_err());
    assert!(s.prepare_execution_launch(owner, &r, &t).is_ok());
}
#[test]
fn guarded_launch_progress_cannot_requeue_or_change_source_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut s = Store::open(&path).unwrap();
    let (owner, t, r) = fixture(&mut s);
    s.prepare_execution_launch(owner, &r, &t).unwrap();
    let p = Progress {
        expected: State::Queued,
        next: State::Uncertain,
        run_id: None,
        worktree_path: None,
        error_code: None,
    };
    let uncertain = s
        .advance_execution_launch(owner, r.operation_id, &p)
        .unwrap();
    assert_eq!(
        s.advance_execution_launch(owner, r.operation_id, &p)
            .unwrap(),
        uncertain
    );
    let p = Progress {
        expected: State::Uncertain,
        next: State::Queued,
        ..p
    };
    assert!(s
        .advance_execution_launch(owner, r.operation_id, &p)
        .is_err());
    let p = Progress {
        expected: State::Uncertain,
        next: State::Running,
        run_id: Some("run-1".into()),
        worktree_path: Some("/isolated/worktree".into()),
        error_code: None,
    };
    s.advance_execution_launch(owner, r.operation_id, &p)
        .unwrap();
    let mut p = Progress {
        expected: State::Running,
        next: State::Completed,
        ..p
    };
    p.run_id = Some("other-run".into());
    assert!(s
        .advance_execution_launch(owner, r.operation_id, &p)
        .is_err());
    p.run_id = Some("run-1".into());
    let done = s
        .advance_execution_launch(owner, r.operation_id, &p)
        .unwrap();
    assert_eq!(
        s.advance_execution_launch(owner, r.operation_id, &p)
            .unwrap(),
        done
    );
    drop(s);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "DROP TABLE execution_outputs; DROP TABLE execution_results; DROP TABLE execution_launches; DROP TABLE auth_passkeys; DROP TABLE sync_scopes; DROP TABLE sync_requests; DROP TABLE sync_grants; PRAGMA user_version=8;",
    )
    .unwrap();
    drop(db);
    let s = Store::open(&path).unwrap();
    assert!(s
        .latest_execution_brief(owner, r.brief_id)
        .unwrap()
        .is_some());
}
