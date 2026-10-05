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
    let app = router(backend);
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
