use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tessera_inboxd::{
    auth::{Auth, Error},
    http::router,
    store::Store,
};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;
use webauthn_authenticator_rs::{softpasskey::SoftPasskey, WebauthnAuthenticator};

const ORIGIN: &str = "https://inbox-qa.example.test";
fn auth(path: &std::path::Path) -> Auth {
    Auth::new(Store::open(path).unwrap(), ORIGIN).unwrap()
}
fn seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    cookie: Option<&str>,
    origin: Option<&str>,
) -> (StatusCode, Value, Vec<String>) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|h| h.to_str().unwrap().to_owned())
        .collect();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookies,
    )
}
fn cookie(cookies: &[String], name: &str) -> String {
    cookies
        .iter()
        .find(|c| c.starts_with(name))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn real_webauthn_session_capture_restart_login_and_lost_response_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.db");
    let mut backend = auth(&path);
    let bootstrap = backend.bootstrap(seconds()).unwrap();
    let owner = backend.owner;
    use tessera_inbox_domain::execution::*;
    let source_project = Uuid::new_v4();
    backend
        .store
        .save_execution_project(
            owner,
            &SaveProject {
                operation_id: Uuid::new_v4(),
                project_id: source_project,
                expected_revision: 0,
                draft: ProjectDraft {
                    title: "Questions pilot".into(),
                    status: String::new(),
                    next_step: String::new(),
                },
            },
        )
        .unwrap();
    let source_question = Question {
        approval: None,
        thread_title: None,
        id: Uuid::new_v4(),
        project_id: source_project,
        source: QuestionSource {
            worker_id: None,
            record_kind: SourceRecordKind::Question,
            kind: SourceKind::T3,
            instance_id: "test".into(),
            project_id: "pilot".into(),
            thread_id: "thread".into(),
            question_id: "native".into(),
            generation: "attempt".into(),
        },
        source_revision: "r1".into(),
        state: QuestionState::Pending,
        can_reply: true,
        fields: vec![QuestionField {
            id: "answer".into(),
            prompt: "Reply?".into(),
            options: vec![],
            allow_text: true,
            multiple: false,
        }],
    };
    backend
        .store
        .observe_execution_question(owner, &source_question, 1)
        .unwrap();
    let target = tessera_inboxd::launch::Target {
        id: "launch-pilot".into(),
        label: "Pilot".into(),
        repository: "fixture".into(),
        base_commit: "a".repeat(40),
        model_selection: json!({"instanceId":"codex","model":"test"}),
        runtime_mode: "approval-required".into(),
        interaction_mode: "default".into(),
    };
    let credential_path = dir.path().join("bridge");
    std::fs::write(&credential_path,serde_json::to_vec(&json!({"token":"a".repeat(64),"scope":{"owner_id":owner.0,"project_id":source_project,"instance_id":"test","source_project_id":"pilot","ingest":false,"replies":false,"launches":true},"launch_targets":[target]})).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&credential_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let bridge = tessera_inboxd::bridge::Bridge::from_credential(&credential_path, owner).unwrap();
    let forgejo_path = dir.path().join("forgejo-cache");
    std::fs::write(&forgejo_path,serde_json::to_vec(&json!({"version":1,"owner_id":owner.0,"projects":{source_project.to_string():[{"repo_id":10,"launch_target_ids":["launch-pilot"]}]},"discovered_at":1,"error":null,"repos":[{"id":10,"synced_at":1,"error":null,"issues":[],"pulls":[],"releases":[]},{"id":11,"synced_at":1,"error":null,"issues":[],"pulls":[],"releases":[]}]})).unwrap()).unwrap();
    let app = tessera_inboxd::http::router_with_forgejo(
        backend,
        None,
        None,
        Some(bridge),
        Some(tessera_inboxd::forgejo::Cache(forgejo_path)),
    );

    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let (status, options, cookies) = call(
        &app,
        "POST",
        "/api/v1/auth/register/start",
        json!({"token":bootstrap}),
        None,
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(cookies[0].contains("Secure; HttpOnly; SameSite=Strict"));
    let flow = cookie(&cookies, "__Host-inbox-flow=");
    let credential = key
        .do_registration(
            Url::parse(ORIGIN).unwrap(),
            serde_json::from_value(options).unwrap(),
        )
        .unwrap();
    let body = serde_json::to_value(credential).unwrap();
    let (status, identity, cookies) = call(
        &app,
        "POST",
        "/api/v1/auth/register/finish",
        body.clone(),
        Some(&flow),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["owner_id"], owner.0.to_string());
    let session = cookie(&cookies, "__Host-inbox-session=");
    assert_eq!(
        call(&app, "GET", "/api/v1/forgejo", Value::Null, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, forgejo, _) = call(
        &app,
        "GET",
        "/api/v1/forgejo",
        Value::Null,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(forgejo["repos"].as_array().unwrap().len(), 2);
    let (_, project_repos, _) = call(
        &app,
        "GET",
        &format!("/api/v1/forgejo?project={source_project}"),
        Value::Null,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(project_repos["repos"].as_array().unwrap().len(), 1);
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/v1/forgejo?project={}", Uuid::new_v4()),
            Value::Null,
            Some(&session),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let targets_path = format!("/api/v1/projects/{source_project}/launch-targets");
    assert_eq!(
        call(&app, "GET", &targets_path, Value::Null, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, targets, _) = call(
        &app,
        "GET",
        &targets_path,
        Value::Null,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(targets["targets"].as_array().unwrap().len(), 1);
    let launch_brief = Uuid::new_v4();
    assert_eq!(call(&app,"POST","/api/v1/briefs",json!({"operation_id":Uuid::new_v4(),"project_id":source_project,"brief_id":launch_brief,"expected_revision":0,"title":"Launch pilot","text":"Print marker","target_id":"launch-pilot"}),Some(&session),Some(ORIGIN)).await.0,StatusCode::OK);
    let launch = json!({"operation_id":Uuid::new_v4(),"brief_id":launch_brief,"expected_revision":1,"target_revision":targets["targets"][0]["revision"]});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/launches",
            launch.clone(),
            Some(&session),
            Some("https://foreign.test")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, launched, _) = call(
        &app,
        "POST",
        "/api/v1/launches",
        launch.clone(),
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(launched["state"], "queued");
    let (_, linked, _) = call(
        &app,
        "GET",
        &format!("/api/v1/forgejo?project={source_project}"),
        Value::Null,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(
        linked["execution_links"][0]["thread_id"],
        launched["thread_id"]
    );
    assert_eq!(linked["execution_links"][0]["repo_id"], 10);

    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/launches",
            launch.clone(),
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .1,
        launched
    );

    {
        use tessera_inboxd::launch::{Progress, State};
        let mut store = Store::open(&path).unwrap();
        let id = Uuid::parse_str(launch["operation_id"].as_str().unwrap()).unwrap();
        store
            .advance_execution_launch(
                owner,
                id,
                &Progress {
                    expected: State::Queued,
                    next: State::Uncertain,
                    run_id: None,
                    worktree_path: None,
                    error_code: None,
                },
            )
            .unwrap();
        store
            .advance_execution_launch(
                owner,
                id,
                &Progress {
                    expected: State::Uncertain,
                    next: State::Completed,
                    run_id: Some("test-run".into()),
                    worktree_path: None,
                    error_code: None,
                },
            )
            .unwrap();
    }
    let report = json!({"operation_id":Uuid::new_v4(),"project_id":source_project,"launch_id":launch["operation_id"],"run_id":"test-run","commit":"b".repeat(40),"platform":"web","channel":"QA","version":"1","publication":"published","url":"https://inbox-qa.example.test/","what_to_check":"Check fixture"});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/results",
            report.clone(),
            Some(&session),
            Some("https://foreign.example")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/results",
            report.clone(),
            None,
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (reported, result, _) = call(
        &app,
        "POST",
        "/api/v1/results",
        report.clone(),
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(reported, StatusCode::OK);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/results",
            report,
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .1,
        result
    );
    let (_, results, _) = call(
        &app,
        "GET",
        &format!("/api/v1/projects/{source_project}/results"),
        Value::Null,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(results["results"].as_array().unwrap().len(), 1);
    let mut duplicate = launch.clone();
    duplicate["operation_id"] = json!(Uuid::new_v4());
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/launches",
            duplicate,
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let path_launch = format!(
        "/api/v1/launches/{}",
        launch["operation_id"].as_str().unwrap()
    );
    assert_eq!(
        call(&app, "GET", &path_launch, Value::Null, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let reply_path = format!("/api/v1/questions/{}/reply", source_question.id);
    let reply = json!({"operation_id":Uuid::new_v4(),"question_id":source_question.id,
        "expected_revision":"r1","answers":[{"id":"answer","text":"Exact answer","option_ids":[]}]});
    assert_eq!(
        call(&app, "POST", &reply_path, reply.clone(), None, Some(ORIGIN))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &reply_path,
            reply.clone(),
            Some(&session),
            Some("https://wrong.example")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, operation, _) = call(
        &app,
        "POST",
        &reply_path,
        reply.clone(),
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(operation["state"], "queued");
    assert_eq!(
        call(
            &app,
            "POST",
            &reply_path,
            reply.clone(),
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .1,
        operation
    );
    let mut duplicate = reply;
    duplicate["operation_id"] = json!(Uuid::new_v4());
    let (status, conflict, _) = call(
        &app,
        "POST",
        &reply_path,
        duplicate,
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["current"]["can_reply"], false);

    // Draft APIs share the actual passkey session and origin boundary. Saving a
    // brief does not grant authority to its opaque target or launch anything.
    let project_id = Uuid::new_v4();
    let project = json!({"operation_id":Uuid::new_v4(), "project_id":project_id,
        "expected_revision":0, "draft":{"title":"Pilot","status":"Planning","next_step":"Review brief"}});
    for (cookie, origin, expected) in [
        (None, Some(ORIGIN), StatusCode::UNAUTHORIZED),
        (
            Some(session.as_str()),
            Some("https://wrong.example"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/projects",
                project.clone(),
                cookie,
                origin
            )
            .await
            .0,
            expected
        );
    }
    let (status, first, _) = call(
        &app,
        "POST",
        "/api/v1/projects",
        project.clone(),
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["revision"], 1);
    let mut edit = project.clone();
    edit["operation_id"] = json!(Uuid::new_v4());
    let (status, conflict, _) = call(
        &app,
        "POST",
        "/api/v1/projects",
        edit.clone(),
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["current"], first);
    edit["expected_revision"] = json!(1);
    edit["draft"]["status"] = json!("Doing");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/projects",
            edit,
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/projects",
            project,
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .1,
        first
    );
    let brief_id = Uuid::new_v4();
    let brief = json!({"operation_id":Uuid::new_v4(),"project_id":project_id,"brief_id":brief_id,
        "expected_revision":0,"title":"Pilot brief","text":"Exact draft","target_id":"unconfigured-pilot"});
    let (status, stored, _) = call(
        &app,
        "POST",
        "/api/v1/briefs",
        brief,
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored["revision"], 1);
    let brief_path = format!("/api/v1/briefs/{brief_id}/revisions/1");
    assert_eq!(
        call(&app, "GET", &brief_path, Value::Null, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", &brief_path, Value::Null, Some(&session), None)
            .await
            .1,
        stored
    );

    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/register/finish",
            body,
            Some(&flow),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let capture = json!({"operation_id":Uuid::new_v4(),"item_id":Uuid::new_v4(),"text":"  Мобильная мысль\r\n"});
    let (status, original, _) = call(
        &app,
        "POST",
        "/api/v1/items",
        capture.clone(),
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(original["item"]["original_text"], capture["text"]);
    assert_eq!(
        call(&app, "GET", "/api/v1/items", json!({}), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    drop(app);
    let mut reopened = auth(&path);
    assert!(matches!(
        reopened.bootstrap(seconds()),
        Err(Error::Enrolled)
    ));
    let app = router(reopened);
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/session",
            json!({}),
            Some(&session),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, options, cookies) = call(
        &app,
        "POST",
        "/api/v1/auth/login/start",
        json!({}),
        None,
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let flow = cookie(&cookies, "__Host-inbox-flow=");
    let assertion = key
        .do_authentication(
            Url::parse(ORIGIN).unwrap(),
            serde_json::from_value(options).unwrap(),
        )
        .unwrap();
    let body = serde_json::to_value(assertion).unwrap();
    let (status, _, cookies) = call(
        &app,
        "POST",
        "/api/v1/auth/login/finish",
        body.clone(),
        Some(&flow),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = cookie(&cookies, "__Host-inbox-session=");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/login/finish",
            body,
            Some(&flow),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, replayed, _) = call(
        &app,
        "POST",
        "/api/v1/items",
        capture.clone(),
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed, original);
    let (_, page, _) = call(
        &app,
        "GET",
        "/api/v1/items",
        json!({}),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(page["changes"].as_array().unwrap().len(), 1);
    let mut changed = capture;
    changed["text"] = json!("different");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/items",
            changed,
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/logout",
            json!({}),
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/session",
            json!({}),
            Some(&session),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn csrf_origin_missing_session_wrong_flow_and_malformed_payload_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = auth(&dir.path().join("inbox.db"));
    let bootstrap = backend.bootstrap(seconds()).unwrap();
    let app = router(backend);
    for origin in [None, Some("https://evil.example"), Some("null")] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/auth/register/start",
                json!({"token":bootstrap}),
                None,
                origin
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let (_, options, cookies) = call(
        &app,
        "POST",
        "/api/v1/auth/register/start",
        json!({"token":bootstrap}),
        None,
        Some(ORIGIN),
    )
    .await;
    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = key
        .do_registration(
            Url::parse(ORIGIN).unwrap(),
            serde_json::from_value(options).unwrap(),
        )
        .unwrap();
    let body = serde_json::to_value(credential).unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/register/finish",
            body.clone(),
            None,
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let flow = cookie(&cookies, "__Host-inbox-flow=");
    let mut tampered = body.clone();
    tampered["response"]["clientDataJSON"] = json!("bm90LWpzb24");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/register/finish",
            tampered,
            Some(&flow),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/register/finish",
            body,
            Some(&flow),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let request = json!({"operation_id":Uuid::new_v4(),"item_id":Uuid::new_v4(),"text":"test","owner_id":Uuid::new_v4()});
    assert_eq!(
        call(&app, "POST", "/api/v1/items", request, None, Some(ORIGIN))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[test]
fn expired_rotated_and_consumed_bootstrap_never_enroll() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.db");
    let mut backend = auth(&path);
    let old = backend.bootstrap(1000).unwrap();
    let (flow, options) = backend.register_start(&old, 1001).unwrap();
    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = key
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let new = auth(&path).bootstrap(1002).unwrap(); // CLI rotation while server is up.
    assert!(backend.register_finish(&flow, &credential, 1003).is_err());
    assert!(backend.register_start(&old, 1003).is_err());
    assert!(backend.register_start(&new, 1602).is_err());
    let (flow, options) = backend.register_start(&new, 1003).unwrap();
    let credential = key
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    assert!(backend.register_finish(&flow, &credential, 1303).is_err());
    let (flow, options) = backend.register_start(&new, 1004).unwrap();
    let credential = key
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let session = backend.register_finish(&flow, &credential, 1005).unwrap();
    assert!(backend.authenticate(&session, 1006).is_ok());
    assert!(backend
        .authenticate(&session, 1005 + tessera_inboxd::auth::SESSION_SECONDS)
        .is_err());
    assert!(backend.register_start(&new, 1006).is_err());
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.windows(new.len()).any(|v| v == new.as_bytes()));
}

#[test]
fn mismatched_origin_and_unverified_authenticator_fail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.db");
    let mut backend = auth(&path);
    assert!(Auth::new(Store::open(&path).unwrap(), "https://other.example.test").is_err());
    assert!(Auth::new(Store::open(&path).unwrap(), "http://inbox-qa.example.test").is_err());
    let token = backend.bootstrap(1000).unwrap();
    let (flow, options) = backend.register_start(&token, 1001).unwrap();
    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(false));
    // Simulate a client downgrading only its copy of the options. The server's
    // saved ceremony still requires UV. Produce a real signed non-UV credential
    // unconditionally so success cannot mean the negative path never ran.
    let mut downgraded = serde_json::to_value(options).unwrap();
    assert_eq!(
        downgraded["publicKey"]["authenticatorSelection"]["userVerification"],
        "required"
    );
    downgraded["publicKey"]["authenticatorSelection"]["userVerification"] = json!("discouraged");
    let credential = key
        .do_registration(
            Url::parse(ORIGIN).unwrap(),
            serde_json::from_value(downgraded).unwrap(),
        )
        .expect("positive control: authenticator produces a signed non-UV credential");
    assert!(matches!(
        backend.register_finish(&flow, &credential, 1002),
        Err(Error::Rejected)
    ));
}

#[test]
fn auth_start_rate_is_bounded_and_resets() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = auth(&dir.path().join("inbox.db"));
    for _ in 0..30 {
        assert!(matches!(
            backend.register_start("bad", 1000),
            Err(Error::Unauthorized)
        ));
    }
    assert!(matches!(
        backend.register_start("bad", 1001),
        Err(Error::Limited)
    ));
    assert!(matches!(
        backend.register_start("bad", 1060),
        Err(Error::Unauthorized)
    ));
}

#[tokio::test]
async fn web_shell_is_explicit_and_keeps_api_private() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(auth(&dir.path().join("inbox.db")));
    for (path, mime) in [
        ("/", "text/html"),
        ("/app.js", "text/javascript"),
        ("/ui.js", "text/javascript"),
        ("/noto-sans-400.ttf", "font/ttf"),
        ("/noto-sans-600.ttf", "font/ttf"),
        ("/notosans-OFL.txt", "text/plain"),
        ("/sw.js", "text/javascript"),
        ("/manifest.webmanifest", "application/manifest+json"),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with(mime));
        let csp = response.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert!(csp.contains("script-src 'self'"));
        assert!(!csp.contains("unsafe-inline"));
        assert!(!csp.contains("unsafe-eval"));
        assert!(response.headers().get("set-cookie").is_none());
    }
    for path in [
        "/package.json",
        "/tests/outbox.test.js",
        "/../Cargo.toml",
        "/inbox.db",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        call(&app, "GET", "/api/v1/items", Value::Null, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn discussion_finishes_without_client_and_replay_calls_provider_once() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let provider_app = Router::new().route("/chat/completions", axum::routing::post(move || {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"choices":[{"finish_reason":"stop","message":{"content":"A useful draft"}}]}))
        }
    }));
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/chat/completions", socket.local_addr().unwrap());
    let provider_task =
        tokio::spawn(async move { axum::serve(socket, provider_app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    std::fs::write(&key_path, "fixture-key").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let provider =
        tessera_inboxd::provider::Provider::from_credential(&endpoint, "fixture", &key_path)
            .unwrap();
    let mut backend = auth(&dir.path().join("inbox.db"));
    let bootstrap = backend.bootstrap(seconds()).unwrap();
    let fixture = dir.path().join("fixture");
    std::fs::create_dir_all(fixture.join("Projects")).unwrap();
    let vault = tessera_inboxd::vault::Vault::open(&fixture, vec!["Projects".into()]).unwrap();
    let app = tessera_inboxd::http::router_with_services(
        backend,
        Some(Arc::new(provider)),
        Some(Arc::new(vault)),
    );
    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let (_, options, cookies) = call(
        &app,
        "POST",
        "/api/v1/auth/register/start",
        json!({"token":bootstrap}),
        None,
        Some(ORIGIN),
    )
    .await;
    let credential = key
        .do_registration(
            Url::parse(ORIGIN).unwrap(),
            serde_json::from_value(options).unwrap(),
        )
        .unwrap();
    let (status, _, cookies) = call(
        &app,
        "POST",
        "/api/v1/auth/register/finish",
        serde_json::to_value(credential).unwrap(),
        Some(&cookie(&cookies, "__Host-inbox-flow=")),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = cookie(&cookies, "__Host-inbox-session=");
    let item = Uuid::new_v4();
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/items",
            json!({"operation_id":Uuid::new_v4(),"item_id":item,"text":"Original"}),
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::OK
    );
    let path = format!("/api/v1/items/{item}/discussion");
    let body = json!({"operation_id":Uuid::new_v4(),"text":"Clarify"});
    assert_eq!(
        call(&app, "POST", &path, body.clone(), None, Some(ORIGIN))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            body.clone(),
            Some(&session),
            Some("https://evil.test")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            body.clone(),
            Some(&session),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    // No active request is held open while generation runs on the backend.
    let turns = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (_, turns, _) = call(&app, "GET", &path, Value::Null, Some(&session), None).await;
            if turns[0]["state"] == "succeeded" {
                break turns;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(turns[0]["answer"], "A useful draft");
    let (_, replay, _) = call(&app, "POST", &path, body, Some(&session), Some(ORIGIN)).await;
    assert_eq!(replay["state"], "succeeded");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let publish_path = format!("/api/v1/items/{item}/publications");
    let request = json!({"operation_id":Uuid::new_v4(),"folder":"Projects","filename":"Result.md","content":"# Exact preview\nA useful draft"});
    assert_eq!(
        call(
            &app,
            "POST",
            &publish_path,
            request.clone(),
            None,
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &publish_path,
            request.clone(),
            Some(&session),
            Some("https://evil.test")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for _ in 0..2 {
        let (status, result, _) = call(
            &app,
            "POST",
            &publish_path,
            request.clone(),
            Some(&session),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["state"], "published");
    }
    assert_eq!(
        std::fs::read_dir(fixture.join("Projects")).unwrap().count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(fixture.join("Projects/Result.md")).unwrap(),
        request["content"].as_str().unwrap()
    );
    let (_, original, _) = call(
        &app,
        "GET",
        &format!("/api/v1/items/{item}"),
        Value::Null,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(original["original_text"], "Original");
    provider_task.abort();
}

#[tokio::test]
async fn passkey_routes_enforce_origin_session_and_recent_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = auth(&dir.path().join("db"));
    let now = seconds();
    let bootstrap = backend.bootstrap(now - 301).unwrap();
    let (flow, options) = backend.register_start(&bootstrap, now - 301).unwrap();
    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = key
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let session = format!(
        "__Host-inbox-session={}",
        backend
            .register_finish(&flow, &credential, now - 301)
            .unwrap()
    );
    let app = router(backend);
    assert_eq!(
        call(&app, "GET", "/api/v1/passkeys", json!({}), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, keys, _) = call(
        &app,
        "GET",
        "/api/v1/passkeys",
        json!({}),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keys["keys"].as_array().unwrap().len(), 1);
    for (path, body) in [
        ("/api/v1/passkeys/add/start", json!({"name":"Phone"})),
        (
            "/api/v1/passkeys/revoke",
            json!({"id":keys["keys"][0]["id"]}),
        ),
        ("/api/v1/devices/invitations", json!({})),
        (
            "/api/v1/devices/approve",
            json!({"id":"unknown","code":"unknown"}),
        ),
    ] {
        assert_eq!(
            call(
                &app,
                "POST",
                path,
                body.clone(),
                Some(&session),
                Some("https://foreign.example")
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", path, body.clone(), None, Some(ORIGIN))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body, cookies) =
            call(&app, "POST", path, body, Some(&session), Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "recent_authentication_required");
        assert!(cookies.is_empty());
    }
    for path in ["/api/v1/devices/register/start", "/api/v1/devices/status"] {
        assert_eq!(
            call(
                &app,
                "POST",
                path,
                json!({"token":"invalid"}),
                None,
                Some(ORIGIN)
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "POST", path, json!({"token":"invalid"}), None, None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn revoking_the_calling_passkey_reports_and_clears_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = auth(&dir.path().join("db"));
    let now = seconds();
    let bootstrap = backend.bootstrap(now).unwrap();
    let (flow, options) = backend.register_start(&bootstrap, now).unwrap();
    let mut first = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = first
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let session = backend.register_finish(&flow, &credential, now).unwrap();
    let original = backend.passkeys(&session, now).unwrap()[0].id.clone();
    let (flow, options) = backend.add_start(&session, "Second", now).unwrap();
    let mut second = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = second
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    backend
        .add_finish(&session, &flow, &credential, now)
        .unwrap();
    let app = router(backend);
    let session = format!("__Host-inbox-session={session}");
    let (status, result, cookies) = call(
        &app,
        "POST",
        "/api/v1/passkeys/revoke",
        json!({"id":original}),
        Some(&session),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["session_revoked"], true);
    assert!(cookies
        .iter()
        .any(|c| c.starts_with("__Host-inbox-session=") && c.contains("Max-Age=0")));
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/session",
            json!({}),
            Some(&session),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}
