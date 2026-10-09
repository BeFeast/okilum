use okilum_inbox_domain::execution::*;
use okilum_inboxd::{
    auth::Auth,
    launch::*,
    results::*,
    store::{Error, Store},
};
use uuid::Uuid;
fn ready(store: &mut Store) -> (okilum_inbox_domain::OwnerId, Report) {
    let owner = okilum_inbox_domain::OwnerId(Uuid::new_v4());
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
                    status: "Testing".into(),
                    next_step: "Check result".into(),
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
                title: "Build".into(),
                text: "Fixture only".into(),
                target_id: "pilot".into(),
            },
        )
        .unwrap();
    let target = TargetSnapshot {
        project_id: project,
        instance_id: "source".into(),
        source_project_id: "pilot".into(),
        target: Target {
            id: "pilot".into(),
            label: "Pilot".into(),
            repository: "fixture".into(),
            base_commit: "a".repeat(40),
            model_selection: serde_json::json!({"instanceId":"codex","model":"test"}),
            runtime_mode: "approval-required".into(),
            interaction_mode: "default".into(),
        },
    };
    let launch = Launch {
        operation_id: Uuid::new_v4(),
        brief_id: brief,
        expected_revision: 1,
        target_revision: target.revision(),
    };
    store
        .prepare_execution_launch(owner, &launch, &target)
        .unwrap();
    for (expected, next, run) in [
        (State::Queued, State::Uncertain, None),
        (State::Uncertain, State::Completed, Some("run".to_string())),
    ] {
        store
            .advance_execution_launch(
                owner,
                launch.operation_id,
                &Progress {
                    expected,
                    next,
                    run_id: run,
                    worktree_path: None,
                    error_code: None,
                },
            )
            .unwrap();
    }
    (
        owner,
        Report {
            operation_id: Uuid::new_v4(),
            project_id: project,
            launch_id: launch.operation_id,
            run_id: "run".into(),
            commit: "b".repeat(40),
            platform: "web".into(),
            channel: "QA".into(),
            version: "1".into(),
            publication: Publication::Published,
            url: "https://inbox-qa.example.test/".into(),
            what_to_check: "Open fixture result".into(),
        },
    )
}
#[test]
fn exact_publication_and_output_replay_survive_restart_and_failed_build_retains_history() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let mut s = Store::open(&p).unwrap();
    let (owner, r) = ready(&mut s);
    let saved = s.report_execution_result(owner, &r, 100).unwrap();
    let output = Output {
        run_id: "run".into(),
        message_id: "final".into(),
        text: "PILOT_READY".into(),
    };
    s.record_execution_output(owner, r.launch_id, &output)
        .unwrap();
    drop(s);
    let mut s = Store::open(&p).unwrap();
    assert_eq!(s.report_execution_result(owner, &r, 200).unwrap(), saved);
    assert_eq!(
        s.record_execution_output(owner, r.launch_id, &output)
            .unwrap(),
        output
    );
    let mut failed = r.clone();
    failed.operation_id = Uuid::new_v4();
    failed.publication = Publication::Failed;
    failed.version = "2".into();
    s.report_execution_result(owner, &failed, 300).unwrap();
    let rows = s.execution_results(owner, r.project_id, 0, 100).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1.report.publication, Publication::Published);
    assert_eq!(rows[1].1.report.publication, Publication::Failed);
    let mut changed = r.clone();
    changed.what_to_check = "Another report".into();
    assert!(matches!(
        s.report_execution_result(owner, &changed, 400),
        Err(Error::OperationConflict)
    ));
    let changed = Output {
        text: "OTHER".into(),
        ..output
    };
    assert!(matches!(
        s.record_execution_output(owner, r.launch_id, &changed),
        Err(Error::OperationConflict)
    ));
}
#[test]
fn foreign_owner_run_project_and_unsafe_links_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(&d.path().join("db")).unwrap();
    let (owner, r) = ready(&mut s);
    let other = okilum_inbox_domain::OwnerId(Uuid::new_v4());
    assert!(s.report_execution_result(other, &r, 100).is_err());
    assert!(s
        .execution_results(other, r.project_id, 0, 100)
        .unwrap()
        .is_empty());
    assert!(s.execution_output(other, r.launch_id).unwrap().is_none());
    for (field, value) in [
        ("run_id", "wrong"),
        ("commit", "main"),
        ("url", "javascript:alert(1)"),
        ("url", "https://user:pass@example.test"),
    ] {
        let mut v = serde_json::to_value(&r).unwrap();
        v[field] = value.into();
        let bad = serde_json::from_value(v).unwrap();
        assert!(s.report_execution_result(owner, &bad, 100).is_err());
    }
    let mut bad = r.clone();
    bad.project_id = Uuid::new_v4();
    assert!(s.report_execution_result(owner, &bad, 100).is_err());
    assert!(s
        .record_execution_output(
            owner,
            r.launch_id,
            &Output {
                run_id: "wrong".into(),
                message_id: "m".into(),
                text: "x".into()
            }
        )
        .is_err());
}
#[test]
fn schema_nine_upgrade_preserves_launches() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let mut s = Store::open(&path).unwrap();
    let (owner, r) = ready(&mut s);
    drop(s);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "DROP TABLE execution_outputs; DROP TABLE execution_results; DROP TABLE auth_passkeys; DROP TABLE sync_scopes; DROP TABLE sync_requests; DROP TABLE sync_grants; PRAGMA user_version=9;",
    )
    .unwrap();
    drop(db);
    let mut s = Store::open(&path).unwrap();
    assert!(s.execution_launch(owner, r.launch_id).unwrap().is_some());
    s.report_execution_result(owner, &r, 100).unwrap();
}
#[tokio::test]
async fn output_api_requires_session() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let d = tempfile::tempdir().unwrap();
    let auth = Auth::new(
        Store::open(&d.path().join("db")).unwrap(),
        "https://inbox-qa.example.test",
    )
    .unwrap();
    let app = okilum_inboxd::http::router(auth);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/launches/00000000-0000-0000-0000-000000000001/output")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
