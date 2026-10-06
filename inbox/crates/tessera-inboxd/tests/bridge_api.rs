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
            thread_title: None,
            id: Uuid::new_v4(),
            project_id: self.project,
            source: QuestionSource {
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
