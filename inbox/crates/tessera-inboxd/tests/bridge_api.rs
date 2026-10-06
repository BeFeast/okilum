use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::Path;
use tessera_inbox_domain::{execution::*, OwnerId};
use tessera_inboxd::{auth::Auth, bridge::Bridge, http::router_with_bridge, store::Store};
use tower::ServiceExt;
use uuid::Uuid;

const ORIGIN: &str = "https://inbox-qa.example.test";
const KEY: &str = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
struct Fixture {
    dir: tempfile::TempDir,
    owner: OwnerId,
    project: Uuid,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut auth = Auth::new(Store::open(&dir.path().join("db")).unwrap(), ORIGIN).unwrap();
        let project = Uuid::new_v4();
        auth.store
            .save_execution_project(
                auth.owner,
                &SaveProject {
                    operation_id: Uuid::new_v4(),
                    project_id: project,
                    expected_revision: 0,
                    draft: ProjectDraft {
                        title: "Isolated pilot".into(),
                        status: String::new(),
                        next_step: String::new(),
                    },
                },
            )
            .unwrap();
        let f = Self {
            dir,
            owner: auth.owner,
            project,
        };
        f.credential(true, true);
        f
    }
    fn credential(&self, ingest: bool, replies: bool) {
        let path = self.dir.path().join("credential");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"token":KEY,"scope":{
                "owner_id":self.owner.0,"project_id":self.project,"instance_id":"pilot-instance",
                "source_project_id":"pilot-project","ingest":ingest,"replies":replies
            }}))
            .unwrap(),
        )
        .unwrap();
        private(&path);
    }
    fn app(&self, enabled: bool) -> Router {
        let auth = Auth::new(Store::open(&self.dir.path().join("db")).unwrap(), ORIGIN).unwrap();
        let bridge = enabled.then(|| {
            Bridge::from_credential(&self.dir.path().join("credential"), auth.owner).unwrap()
        });
        router_with_bridge(auth, None, None, bridge)
    }
    fn question(&self) -> Question {
        Question {
            approval: None,
            thread_title: None,
            id: Uuid::new_v4(),
            project_id: self.project,
            source: QuestionSource {
                worker_id: None,
                record_kind: SourceRecordKind::Question,
                kind: SourceKind::T3,
                instance_id: "pilot-instance".into(),
                project_id: "pilot-project".into(),
                thread_id: "native-thread".into(),
                question_id: Uuid::new_v4().to_string(),
                generation: "run-1".into(),
            },
            source_revision: "revision-1".into(),
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
        }
    }
    fn enqueue(&self, q: &Question, who: OwnerId) -> Reply {
        let mut store = Store::open(&self.dir.path().join("db")).unwrap();
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
        store.prepare_execution_reply(who, &r).unwrap();
        r
    }
}
fn private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    token: Option<&str>,
    extra: Option<(&str, &str)>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some((k, v)) = extra {
        req = req.header(k, v);
    }
    let response = app
        .clone()
        .oneshot(
            req.body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn observe(app: &Router, q: &Question, seq: u64) -> StatusCode {
    call(
        app,
        "POST",
        "/api/bridge/v1/questions",
        json!({"question":q,"sequence":seq}),
        Some(KEY),
        None,
    )
    .await
    .0
}
#[tokio::test]
async fn machine_auth_does_not_replace_browser_auth_or_accept_ambient_credentials() {
    let f = Fixture::new();
    let app = f.app(true);
    let q = f.question();
    let body = json!({"question":q,"sequence":1});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/bridge/v1/questions",
            body.clone(),
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/bridge/v1/questions",
            body.clone(),
            Some(&"0".repeat(64)),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    for extra in [
        ("origin", ORIGIN),
        ("origin", "https://evil.test"),
        ("cookie", "__Host-inbox-session=not-a-session"),
    ] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/bridge/v1/questions",
                body.clone(),
                Some(KEY),
                Some(extra)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(observe(&app, &q, 1).await, StatusCode::NO_CONTENT); // positive control
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/projects",
            Value::Null,
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", "/api/v1/projects", json!({}), Some(KEY), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let disabled = f.app(false);
    assert_eq!(
        call(
            &disabled,
            "GET",
            "/api/bridge/v1/replies",
            Value::Null,
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn source_scope_and_actions_cannot_be_chosen_by_payload() {
    let f = Fixture::new();
    let app = f.app(true);
    let q = f.question();
    let mut variants = Vec::new();
    let mut x = q.clone();
    x.project_id = Uuid::new_v4();
    variants.push(x);
    let mut x = q.clone();
    x.source.instance_id = "other-instance".into();
    variants.push(x);
    let mut x = q.clone();
    x.source.project_id = "other-project".into();
    variants.push(x);
    let mut x = q.clone();
    x.source.kind = SourceKind::Maestro;
    variants.push(x);
    for x in variants {
        assert_eq!(observe(&app, &x, 1).await, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/bridge/v1/questions",
            json!({"question":q,"sequence":1,"owner_id":Uuid::new_v4()}),
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(observe(&app, &q, 1).await, StatusCode::NO_CONTENT);
    f.credential(true, false);
    let ingest = f.app(true);
    assert_eq!(
        call(
            &ingest,
            "GET",
            "/api/bridge/v1/replies",
            Value::Null,
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(observe(&ingest, &q, 2).await, StatusCode::NO_CONTENT);
    f.credential(false, true);
    let replies = f.app(true);
    assert_eq!(observe(&replies, &q, 3).await, StatusCode::FORBIDDEN);
    assert_eq!(
        call(
            &replies,
            "GET",
            "/api/bridge/v1/replies",
            Value::Null,
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn lost_transition_response_restart_and_terminal_replay_keep_exact_intent() {
    let f = Fixture::new();
    let app = f.app(true);
    let q = f.question();
    assert_eq!(observe(&app, &q, 1).await, StatusCode::NO_CONTENT);
    let r = f.enqueue(&q, f.owner);
    let path = format!("/api/bridge/v1/replies/{}", r.operation_id);
    // A pristine insert must not be mistaken for a successful transition replay.
    for expected in ["queued", "uncertain", "delivered"] {
        assert_eq!(
            call(
                &app,
                "POST",
                &path,
                json!({"expected":expected,"next":"queued"}),
                Some(KEY),
                None
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        call(&app, "GET", &path, Value::Null, Some(KEY), None)
            .await
            .1["state"],
        "queued"
    );
    let body = json!({"expected":"queued","next":"uncertain"});
    assert_eq!(
        call(&app, "POST", &path, body.clone(), Some(KEY), None)
            .await
            .0,
        StatusCode::OK
    );
    drop(app);
    let app = f.app(true);
    let (status, op) = call(&app, "GET", &path, Value::Null, Some(KEY), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(op["state"], "uncertain");
    assert_eq!(op["question"], serde_json::to_value(&q).unwrap());
    assert_eq!(call(&app, "POST", &path, body, Some(KEY), None).await.1, op);
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            json!({"expected":"uncertain","next":"queued"}),
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let done = json!({"expected":"uncertain","next":"delivered","delivery_id":"native-reply"});
    let delivered = call(&app, "POST", &path, done.clone(), Some(KEY), None).await;
    assert_eq!(delivered.0, StatusCode::OK);
    assert_eq!(
        call(&app, "POST", &path, done, Some(KEY), None).await,
        delivered
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            json!({"expected":"delivered","next":"rejected"}),
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, page) = call(
        &app,
        "GET",
        "/api/bridge/v1/replies",
        Value::Null,
        Some(KEY),
        None,
    )
    .await;
    assert_eq!(page["operations"][0]["state"], "delivered"); // no disappearance on restore
}
#[tokio::test]
async fn pages_and_direct_lookups_are_scoped_and_do_not_skip_matches() {
    let f = Fixture::new();
    let app = f.app(true);
    let q = f.question();
    assert_eq!(observe(&app, &q, 1).await, StatusCode::NO_CONTENT);
    let first = f.enqueue(&q, f.owner);
    let mut outside = f.question();
    outside.source.instance_id = "unrelated".into();
    let mut store = Store::open(&f.dir.path().join("db")).unwrap();
    store
        .observe_execution_question(f.owner, &outside, 1)
        .unwrap();
    let hidden = f.enqueue(&outside, f.owner);
    let other = OwnerId(Uuid::new_v4());
    store
        .save_execution_project(
            other,
            &SaveProject {
                operation_id: Uuid::new_v4(),
                project_id: f.project,
                expected_revision: 0,
                draft: ProjectDraft {
                    title: "Foreign".into(),
                    status: String::new(),
                    next_step: String::new(),
                },
            },
        )
        .unwrap();
    let foreign = f.question();
    store
        .observe_execution_question(other, &foreign, 1)
        .unwrap();
    let foreign = f.enqueue(&foreign, other);
    let q = f.question();
    assert_eq!(observe(&app, &q, 1).await, StatusCode::NO_CONTENT);
    let second = f.enqueue(&q, f.owner);
    for id in [hidden.operation_id, foreign.operation_id, Uuid::new_v4()] {
        let path = format!("/api/bridge/v1/replies/{id}");
        assert_eq!(
            call(&app, "GET", &path, Value::Null, Some(KEY), None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(
                &app,
                "POST",
                &path,
                json!({"expected":"queued","next":"uncertain"}),
                Some(KEY),
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    let (_, page) = call(
        &app,
        "GET",
        "/api/bridge/v1/replies?limit=1",
        Value::Null,
        Some(KEY),
        None,
    )
    .await;
    assert_eq!(page["operations"].as_array().unwrap().len(), 1);
    assert_eq!(
        page["operations"][0]["request"]["operation_id"],
        first.operation_id.to_string()
    );
    assert_eq!(page["has_more"], true);
    let cursor = page["next_cursor"].as_u64().unwrap();
    let (_, page) = call(
        &app,
        "GET",
        &format!("/api/bridge/v1/replies?limit=1&after={cursor}"),
        Value::Null,
        Some(KEY),
        None,
    )
    .await;
    assert_eq!(
        page["operations"][0]["request"]["operation_id"],
        second.operation_id.to_string()
    );
    assert_eq!(page["has_more"], false);
    for query in ["limit=0", "limit=101", "after=18446744073709551615"] {
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/api/bridge/v1/replies?{query}"),
                Value::Null,
                Some(KEY),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
}
#[test]
fn credential_is_private_bounded_and_bound_to_existing_owner() {
    let f = Fixture::new();
    let path = f.dir.path().join("credential");
    assert!(Bridge::from_credential(&path, f.owner).is_ok());
    assert!(Bridge::from_credential(&path, OwnerId(Uuid::new_v4())).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Bridge::from_credential(&path, f.owner).is_err());
        private(&path);
        let link = f.dir.path().join("symlink");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(Bridge::from_credential(&link, f.owner).is_err());
    }
    f.credential(false, false);
    assert!(Bridge::from_credential(&path, f.owner).is_err());
    std::fs::write(&path, b"not json").unwrap();
    assert!(Bridge::from_credential(&path, f.owner).is_err());
}

#[tokio::test]
async fn launch_transport_requires_separate_scope_and_preserves_source_identity() {
    use tessera_inboxd::launch::{Launch, Target, TargetSnapshot};
    let f = Fixture::new();
    let target = TargetSnapshot {
        project_id: f.project,
        instance_id: "pilot-instance".into(),
        source_project_id: "pilot-project".into(),
        target: Target {
            id: "pilot".into(),
            label: "Pilot".into(),
            repository: "fixture".into(),
            base_commit: "a".repeat(40),
            model_selection: json!({"instanceId":"codex","model":"test"}),
            runtime_mode: "approval-required".into(),
            interaction_mode: "default".into(),
        },
    };
    let mut store = Store::open(&f.dir.path().join("db")).unwrap();
    let brief = Uuid::new_v4();
    store
        .save_execution_brief(
            f.owner,
            &SaveBrief {
                operation_id: Uuid::new_v4(),
                project_id: f.project,
                brief_id: brief,
                expected_revision: 0,
                title: "Pilot".into(),
                text: "Exact brief".into(),
                target_id: "pilot".into(),
            },
        )
        .unwrap();
    let request = Launch {
        operation_id: Uuid::new_v4(),
        brief_id: brief,
        expected_revision: 1,
        target_revision: target.revision(),
    };
    store
        .prepare_execution_launch(f.owner, &request, &target)
        .unwrap();
    let path = format!("/api/bridge/v1/launches/{}", request.operation_id);
    let app = f.app(true);
    assert_eq!(
        call(&app, "GET", &path, Value::Null, Some(KEY), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let cred = f.dir.path().join("credential");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&cred).unwrap()).unwrap();
    config["scope"]["launches"] = json!(true);
    config["launch_targets"] = json!([target.target]);
    std::fs::write(&cred, serde_json::to_vec(&config).unwrap()).unwrap();
    let app = f.app(true);
    assert_eq!(
        call(&app, "GET", &path, Value::Null, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", &path, Value::Null, Some(KEY), None)
            .await
            .0,
        StatusCode::OK
    );
    let p = json!({"expected":"queued","next":"uncertain","run_id":null,"worktree_path":null,"error_code":null});
    assert_eq!(
        call(&app, "POST", &path, p.clone(), Some(KEY), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "POST", &path, p, Some(KEY), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            json!({"expected":"uncertain","next":"queued"}),
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let output_path = format!("{path}/output");
    let output = json!({"run_id":"completed-run","message_id":"final","text":"PILOT_READY"});
    assert_eq!(
        call(&app, "POST", &output_path, output.clone(), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", &output_path, output.clone(), Some(KEY), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            json!({"expected":"uncertain","next":"completed","run_id":"completed-run"}),
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    for _ in 0..2 {
        assert_eq!(
            call(&app, "POST", &output_path, output.clone(), Some(KEY), None)
                .await
                .0,
            StatusCode::OK
        );
    }
    let mut changed = output.clone();
    changed["text"] = json!("different");
    assert_eq!(
        call(&app, "POST", &output_path, changed, Some(KEY), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    config["scope"]["instance_id"] = json!("replacement-source");
    std::fs::write(&cred, serde_json::to_vec(&config).unwrap()).unwrap();
    let app = f.app(true);
    assert_eq!(
        call(&app, "GET", &path, Value::Null, Some(KEY), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "POST", &output_path, output, Some(KEY), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, list) = call(
        &app,
        "GET",
        "/api/bridge/v1/launches",
        Value::Null,
        Some(KEY),
        None,
    )
    .await;
    assert!(list["operations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn independent_maestro_token_cannot_read_or_mutate_t3_operations() {
    let f = Fixture::new();
    let path = f.dir.path().join("credential");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let maestro_key = "b".repeat(64);
    let mut peer = config.clone();
    peer["token"] = json!(maestro_key);
    peer["scope"]["source_kind"] = json!("maestro");
    peer["scope"]["approval_actions"] = json!(["merge_pr"]);
    config["maestro"] = peer;
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let app = f.app(true);
    let t3 = f.question();
    let mut m = f.question();
    m.source.kind = SourceKind::Maestro;
    m.source.worker_id = Some("worker-1".into());
    for (q, key) in [(&t3, KEY), (&m, maestro_key.as_str())] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/bridge/v1/questions",
                json!({"question":q,"sequence":1}),
                Some(key),
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
    }
    for (q, key) in [(&t3, maestro_key.as_str()), (&m, KEY)] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/bridge/v1/questions",
                json!({"question":q,"sequence":2}),
                Some(key),
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let t3_reply = f.enqueue(&t3, f.owner);
    let maestro_reply = f.enqueue(&m, f.owner);
    for (key, own, foreign) in [
        (KEY, t3_reply.operation_id, maestro_reply.operation_id),
        (
            maestro_key.as_str(),
            maestro_reply.operation_id,
            t3_reply.operation_id,
        ),
    ] {
        let (status, rows) = call(
            &app,
            "GET",
            "/api/bridge/v1/replies",
            Value::Null,
            Some(key),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(rows["operations"].as_array().unwrap().len(), 1);
        assert_eq!(
            rows["operations"][0]["request"]["operation_id"],
            own.to_string()
        );
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/api/bridge/v1/replies/{foreign}"),
                Value::Null,
                Some(key),
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(call(&app,"POST",&format!("/api/bridge/v1/replies/{foreign}"),json!({"expected":"queued","next":"uncertain","delivery_id":null,"error_code":null}),Some(key),None).await.0,StatusCode::NOT_FOUND);
    }
    config["maestro"]["token"] = json!(KEY);
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(Bridge::from_credential(&path, f.owner).is_err());
}

#[tokio::test]
async fn approvals_require_action_scope_and_freeze_exact_revision_and_target() {
    let f = Fixture::new();
    let path = f.dir.path().join("credential");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["scope"]["source_kind"] = json!("maestro");
    config["scope"]["approval_actions"] = json!(["merge_pr"]);
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let app = f.app(true);
    let mut q = f.question();
    q.source.kind = SourceKind::Maestro;
    q.source.record_kind = SourceRecordKind::Approval;
    q.approval = Some(Approval {
        repo: None,
        action: "merge_pr".into(),
        target: json!({"repository":"fixture","number":1}),
        summary: "Merge fixture".into(),
        risk: "high".into(),
        payload_hash: "payload".into(),
        target_state_hash: Some("head-1".into()),
    });
    q.fields = vec![QuestionField {
        id: "decision".into(),
        prompt: "Merge fixture?".into(),
        options: vec![
            QuestionOption {
                id: "approve".into(),
                label: "Approve".into(),
            },
            QuestionOption {
                id: "reject".into(),
                label: "Reject".into(),
            },
        ],
        allow_text: false,
        multiple: false,
    }];
    let mut outside = q.clone();
    outside.approval.as_mut().unwrap().action = "deploy".into();
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/bridge/v1/questions",
            json!({"question":outside,"sequence":1}),
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/bridge/v1/questions",
            json!({"question":q,"sequence":1}),
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let mut changed = q.clone();
    changed.approval.as_mut().unwrap().target = json!({"repository":"fixture","number":2});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/bridge/v1/questions",
            json!({"question":changed,"sequence":2}),
            Some(KEY),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut store = Store::open(&f.dir.path().join("db")).unwrap();
    let r = Reply {
        operation_id: Uuid::new_v4(),
        question_id: q.id,
        expected_revision: q.source_revision.clone(),
        answers: vec![AnswerField {
            id: "decision".into(),
            text: String::new(),
            option_ids: vec!["approve".into()],
        }],
    };
    let original = store.prepare_execution_reply(f.owner, &r).unwrap();
    changed.source_revision = "new-revision".into();
    store
        .observe_execution_question(f.owner, &changed, 2)
        .unwrap();
    assert_eq!(
        store.prepare_execution_reply(f.owner, &r).unwrap(),
        original
    );
    assert_eq!(original.question.approval, q.approval);
    let mut stale = r;
    stale.operation_id = Uuid::new_v4();
    assert!(store.prepare_execution_reply(f.owner, &stale).is_err());
}

// Exercise the shipped Python adapter against a real HTTP Inbox and durable Store.
// The source is the pinned contract fixture; no Maestro process or fleet is involved.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maestro_adapter_round_trip_recovers_lost_response_without_resend() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let f = Fixture::new();
    let path = f.dir.path().join("credential");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["scope"]["source_kind"] = json!("maestro");
    config["scope"]["instance_id"] = json!("instance");
    config["scope"]["source_project_id"] = json!("pilot");
    config["scope"]["approval_actions"] = json!(["merge_pr"]);
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = f.app(true);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let script = r#"
import json,os,signal,sys,tempfile
from pathlib import Path
from maestro import Journal,Runner,Unavailable,project
from test_maestro import SourceFixture,config
from t3_questions import Inbox
signal.alarm(30)
c=config();c['project_id']=sys.argv[2];c['inbox_url']=sys.argv[1]
with tempfile.TemporaryDirectory() as d:
 os.chmod(d,0o700);p=Path(d)/'journal.db';j=Journal(p,c)
 source=SourceFixture(c);inbox=Inbox(c['inbox_url'],sys.argv[3]);runner=Runner(c,j,source,inbox)
 runner.step()
 print(json.dumps([project(source.q,c),project(source.a,c,True)]),flush=True)
 assert sys.stdin.readline().strip()=='send'
 source.lose=True
 for expected in (1,2):
  try:runner.step()
  except Unavailable:pass
  assert len(source.sent)==expected
 j.db.close();j=Journal(p,c);runner=Runner(c,j,source,inbox);runner.step()
 ops=list(inbox.operations());assert sorted(o['state'] for o in ops)==['accepted','delivered']
 question_op=next(o for o in ops if not o['question'].get('approval'))
 source.ack(question_op['request']['operation_id']);runner.step();runner.step()
 assert all(o['state']=='delivered' for o in inbox.operations())
 assert len(source.sent)==2
 j.db.close()
 print('question ack + approval decision recovered; exactly two POSTs',flush=True)
"#;
    let mut child = Command::new("python3")
        .args([
            "-c",
            script,
            &format!("http://{address}"),
            &f.project.to_string(),
            KEY,
        ])
        .env(
            "PYTHONPATH",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let questions: Vec<Question> = serde_json::from_str(&line).unwrap();
    assert_eq!(questions.len(), 2);
    let mut store = Store::open(&f.dir.path().join("db")).unwrap();
    for q in &questions {
        store
            .prepare_execution_reply(
                f.owner,
                &Reply {
                    operation_id: Uuid::new_v4(),
                    question_id: q.id,
                    expected_revision: q.source_revision.clone(),
                    answers: vec![AnswerField {
                        id: q.fields[0].id.clone(),
                        text: String::new(),
                        option_ids: vec![if q.approval.is_some() {
                            "approve"
                        } else {
                            "blue"
                        }
                        .into()],
                    }],
                },
            )
            .unwrap();
    }
    child.stdin.take().unwrap().write_all(b"send\n").unwrap();
    let result = child.wait_with_output().unwrap();
    server.abort();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
