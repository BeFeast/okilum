use sha2::{Digest, Sha256};
use tessera_inboxd::{
    auth::Auth,
    http,
    store::Store,
    sync::{Config, Error, Exchange, Start, Vault},
};
use url::Url;
use uuid::Uuid;
use webauthn_authenticator_rs::{softpasskey::SoftPasskey, WebauthnAuthenticator};
const ORIGIN: &str = "https://sync-qa.example.test";
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn device(c: char) -> String {
    std::iter::repeat_n(c.to_string().repeat(7), 8)
        .collect::<Vec<_>>()
        .join("-")
}
fn config(owner: Uuid) -> Config {
    Config {
        owner_id: owner,
        vaults: vec![Vault {
            id: Uuid::new_v4(),
            name: "Fixture".into(),
            folder_id: "fixture".into(),
            hub_device_id: device('B'),
            hub_address: "tcp://127.0.0.1:22440".into(),
            ignores: vec!["/.tessera-index".into()],
            adapter_socket: "/unavailable/tessera-fixture.sock".into(),
        }],
    }
}
fn request() -> (Start, Exchange) {
    let id = Uuid::new_v4();
    let verifier = "a".repeat(64);
    let grant_secret = "b".repeat(64);
    (
        Start {
            id,
            device_id: device('A'),
            name: "Test desktop".into(),
            verifier_challenge: hash(&verifier),
            grant_challenge: hash(&grant_secret),
        },
        Exchange {
            id,
            verifier,
            grant_secret,
        },
    )
}
fn enroll(auth: &mut Auth, now: i64) -> (WebauthnAuthenticator<SoftPasskey>, String) {
    let mut key = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let bootstrap = auth.bootstrap(now).unwrap();
    let (flow, options) = auth.register_start(&bootstrap, now).unwrap();
    let credential = key
        .do_registration(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let token = auth.register_finish(&flow, &credential, now).unwrap();
    (key, token)
}
fn login(auth: &mut Auth, key: &mut WebauthnAuthenticator<SoftPasskey>, now: i64) -> String {
    let (flow, options) = auth.login_start(now).unwrap();
    let credential = key
        .do_authentication(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    auth.login_finish(&flow, &credential, now).unwrap()
}
#[test]
fn durable_pairing_scope_replay_expiry_and_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("inbox.db");
    let mut auth = Auth::new(Store::open(&db).unwrap(), ORIGIN).unwrap();
    let (mut key, session) = enroll(&mut auth, 1000);
    let owner = auth.owner.0;
    let c = config(owner);
    c.bind(&mut auth.store).unwrap();
    let (r, x) = request();
    let p = auth.store.sync_start(owner, &r, 1001).unwrap();
    assert_eq!(auth.store.sync_start(owner, &r, 1002).unwrap().code, p.code);
    let mut changed = r.clone();
    changed.name = "Different".into();
    assert!(matches!(
        auth.store.sync_start(owner, &changed, 1002),
        Err(Error::Conflict)
    ));
    assert!(auth.store.sync_start(Uuid::new_v4(), &r, 1002).is_err());
    assert!(auth.store.sync_pairing(Uuid::new_v4(), r.id, 1002).is_err());
    assert!(auth.store.sync_exchange(owner, &x, 1002).unwrap().is_none());
    assert!(auth
        .sync_approve(&c, "wrong-session", r.id, c.vaults[0].id, &p.code, 1003)
        .is_err());
    assert!(auth
        .sync_approve(&c, &session, r.id, Uuid::new_v4(), &p.code, 1003)
        .is_err());
    assert!(auth
        .sync_approve(&c, &session, r.id, c.vaults[0].id, "WRONG", 1003)
        .is_err());
    assert!(auth
        .sync_approve(&c, &session, r.id, c.vaults[0].id, &p.code, 1301)
        .is_err());
    drop(auth);
    let mut auth = Auth::new(Store::open(&db).unwrap(), ORIGIN).unwrap();
    assert_eq!(auth.owner.0, owner);
    assert!(auth.authenticate(&session, 1004).is_err());
    let session = login(&mut auth, &mut key, 1004);
    auth.sync_approve(&c, &session, r.id, c.vaults[0].id, &p.code, 1005)
        .unwrap();
    auth.sync_approve(&c, &session, r.id, c.vaults[0].id, &p.code, 1005)
        .unwrap();
    let bad = Exchange {
        id: r.id,
        verifier: "c".repeat(64),
        grant_secret: x.grant_secret.clone(),
    };
    assert!(auth.store.sync_exchange(owner, &bad, 1006).is_err());
    let bad = Exchange {
        id: r.id,
        verifier: x.verifier.clone(),
        grant_secret: "c".repeat(64),
    };
    assert!(auth.store.sync_exchange(owner, &bad, 1006).is_err());
    let registered = auth.store.sync_exchange(owner, &x, 1006).unwrap().unwrap();
    assert_eq!(registered.state, "provisioning");
    assert_eq!(
        auth.store
            .sync_exchange(owner, &x, 1007)
            .unwrap()
            .unwrap()
            .id,
        registered.id
    );
    assert!(auth
        .store
        .sync_grant(Uuid::new_v4(), &x.grant_secret)
        .is_err());
    assert!(auth.authenticate(&x.grant_secret, 1007).is_err());
    auth.store
        .sync_observed(owner, r.id, "provisioning", None)
        .unwrap();
    assert_eq!(
        auth.store.sync_grant(owner, &x.grant_secret).unwrap().state,
        "provisioning"
    );
    auth.store
        .sync_observed(owner, r.id, "provisioning", Some("hub_ready"))
        .unwrap();
    assert!(
        tessera_inboxd::sync::desktop_status(&auth.store, &c, &x.grant_secret)
            .unwrap()
            .folder_id
            .is_some()
    );
    auth.store
        .sync_observed(owner, r.id, "hub_ready", None)
        .unwrap();
    assert!(
        tessera_inboxd::sync::desktop_status(&auth.store, &c, &x.grant_secret)
            .unwrap()
            .folder_id
            .is_none()
    );
    auth.store.sync_remove(owner, r.id).unwrap();
    // A late Add completion cannot overwrite a newer removal intent.
    auth.store
        .sync_observed(owner, r.id, "provisioning", Some("hub_ready"))
        .unwrap();
    assert_eq!(
        auth.store.sync_grant(owner, &x.grant_secret).unwrap().state,
        "removal_pending"
    );
    drop(auth);
    let mut auth = Auth::new(Store::open(&db).unwrap(), ORIGIN).unwrap();
    assert_eq!(
        auth.store.sync_grant(owner, &x.grant_secret).unwrap().state,
        "removal_pending"
    );
    auth.store
        .sync_observed(owner, r.id, "removal_pending", Some("revoked"))
        .unwrap();
    assert_eq!(auth.store.sync_remove(owner, r.id).unwrap(), "revoked");
    auth.store
        .sync_observed(owner, r.id, "revoked", None)
        .unwrap();
    assert_eq!(
        auth.store.sync_grant(owner, &x.grant_secret).unwrap().state,
        "revoked"
    );
    assert_eq!(
        auth.store
            .sync_exchange(owner, &x, 1008)
            .unwrap()
            .unwrap()
            .state,
        "revoked"
    );
    assert!(auth.store.sync_exchange(owner, &x, 1601).is_err());
    assert!(
        tessera_inboxd::sync::desktop_status(&auth.store, &c, &x.grant_secret)
            .unwrap()
            .folder_id
            .is_none(),
        "revoked grant retains only its removal receipt"
    );
    // A new owner-approved registration may reuse the device after confirmed removal.
    let (mut fresh, mut exchange) = request();
    exchange.grant_secret = "d".repeat(64);
    fresh.grant_challenge = hash(&exchange.grant_secret);
    let pending = auth.store.sync_start(owner, &fresh, 1010).unwrap();
    let session = login(&mut auth, &mut key, 1011);
    auth.sync_approve(&c, &session, fresh.id, c.vaults[0].id, &pending.code, 1012)
        .unwrap();
    let new = auth
        .store
        .sync_exchange(owner, &exchange, 1013)
        .unwrap()
        .unwrap();
    assert_ne!(new.id, r.id);
    assert_eq!(new.state, "provisioning");
    // A delayed observation or repeated removal for the old credential cannot
    // re-open its tombstone or alter the new registration.
    auth.store
        .sync_observed(owner, r.id, "revoked", Some("removal_pending"))
        .unwrap();
    assert_eq!(auth.store.sync_remove(owner, r.id).unwrap(), "revoked");
    assert_eq!(
        auth.store.sync_grant(owner, &x.grant_secret).unwrap().state,
        "revoked"
    );
    assert_eq!(
        auth.store
            .sync_grant(owner, &exchange.grant_secret)
            .unwrap()
            .state,
        "provisioning"
    );
    let mut changed = c.clone();
    changed.vaults[0].folder_id = "other".into();
    assert!(changed.bind(&mut auth.store).is_err());
    let bytes = std::fs::read(&db).unwrap();
    assert!(!bytes
        .windows(x.grant_secret.len())
        .any(|w| w == x.grant_secret.as_bytes()));
}

#[tokio::test]
async fn http_separates_browser_approval_from_native_grants() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;
    async fn send(
        app: &axum::Router,
        path: &str,
        origin: bool,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        if origin {
            b = b.header("origin", ORIGIN);
        }
        if let Some(cookie) = cookie {
            b = b.header("cookie", format!("__Host-inbox-session={cookie}"));
        }
        let response = app
            .clone()
            .oneshot(b.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    let dir = tempfile::tempdir().unwrap();
    let mut auth = Auth::new(Store::open(&dir.path().join("db")).unwrap(), ORIGIN).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (_, session) = enroll(&mut auth, now);
    let c = config(auth.owner.0);
    let vault = c.vaults[0].id;
    let app = http::router_with_sync(auth, None, None, None, None, Some(std::sync::Arc::new(c)));
    let (r, x) = request();
    let b = json!({"id":r.id,"device_id":r.device_id,"name":r.name,"verifier_challenge":r.verifier_challenge,"grant_challenge":r.grant_challenge});
    assert_eq!(
        send(&app, "/api/v1/sync/desktop/start", true, None, b.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &app,
            "/api/v1/sync/desktop/start",
            false,
            Some(&session),
            b.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, p) = send(&app, "/api/v1/sync/desktop/start", false, None, b.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(p["approval_url"].as_str().unwrap().contains("#sync="));
    let code = p["request"]["code"].as_str().unwrap();
    let decision = json!({"id":r.id,"vault_id":vault,"code":code});
    assert_eq!(
        send(
            &app,
            "/api/v1/sync/approve",
            false,
            Some(&session),
            decision.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, "/api/v1/sync/approve", true, None, decision.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, "/api/v1/sync/approve", true, Some(&session), decision)
            .await
            .0,
        StatusCode::OK
    );
    let (status, result) = send(
        &app,
        "/api/v1/sync/desktop/exchange",
        false,
        None,
        json!({"id":x.id,"verifier":x.verifier,"grant_secret":x.grant_secret}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["registration"]["state"], "provisioning");
    assert!(!result.to_string().contains(&x.grant_secret));
    assert_eq!(
        send(
            &app,
            "/api/v1/sync/remove",
            true,
            Some(&session),
            json!({"id":r.id})
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut stale = Auth::new(Store::open(&dir.path().join("stale.db")).unwrap(), ORIGIN).unwrap();
    let (_, old_session) = enroll(&mut stale, now - 301);
    let old_config = config(stale.owner.0);
    let stale_app = http::router_with_sync(
        stale,
        None,
        None,
        None,
        None,
        Some(std::sync::Arc::new(old_config)),
    );
    assert_eq!(
        send(
            &stale_app,
            "/api/v1/sync/remove",
            true,
            Some(&old_session),
            json!({"id":r.id})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/sync/desktop/remove")
                .header("authorization", format!("Bearer {}", x.grant_secret))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn unauthenticated_request_storage_is_bounded_and_expiry_releases_pending_slots() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let owner = Uuid::new_v4();
    for i in 0..128 {
        let (r, _) = request();
        let now = 1000 + (i / 30) * 61;
        store.sync_start(owner, &r, now).unwrap();
        if i == 29 {
            let (r, _) = request();
            assert!(matches!(
                store.sync_start(owner, &r, now),
                Err(Error::Limited)
            ));
        }
    }
    let (r, _) = request();
    assert!(matches!(
        store.sync_start(owner, &r, 1244),
        Err(Error::Limited)
    ));
    assert!(
        store.sync_start(owner, &r, 2000).is_ok(),
        "expired pending slots are reusable"
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DELETE FROM sync_requests;
      WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000)
      INSERT INTO sync_requests SELECT printf('00000000-0000-0000-0000-%012d',x),'owner','device','test','hash','grant',0,1,'cancelled',NULL,NULL FROM n;").unwrap();
    let (r, _) = request();
    assert!(matches!(
        store.sync_start(owner, &r, 2000),
        Err(Error::Capacity)
    ));
}

#[test]
fn invalid_sync_configuration_preserves_existing_passkey_login() {
    use std::{
        net::TcpListener,
        process::{Command, Stdio},
        time::Duration,
    };
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut auth = Auth::new(Store::open(&dir.path().join("inbox.db")).unwrap(), ORIGIN).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (mut key, _) = enroll(&mut auth, now);
    let owner = auth.owner.0;
    drop(auth);
    let guard = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = guard.local_addr().unwrap();
    drop(guard);
    let _child = Child(
        Command::new(env!("CARGO_BIN_EXE_tessera-inboxd"))
            .args(["serve", "--data-dir"])
            .arg(dir.path())
            .args([
                "--origin",
                ORIGIN,
                "--listen",
                &address.to_string(),
                "--sync-config",
            ])
            .arg(dir.path().join("missing-sync-config.json"))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let base = format!("http://{address}");
    let mut ready = false;
    for _ in 0..100 {
        if client
            .get(format!("{base}/health"))
            .send()
            .is_ok_and(|r| r.status().is_success())
        {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        ready,
        "Inbox remains available with invalid opt-in Sync configuration"
    );
    let response = client
        .post(format!("{base}/api/v1/auth/login/start"))
        .header("origin", ORIGIN)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    let flow = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let assertion = key
        .do_authentication(Url::parse(ORIGIN).unwrap(), response.json().unwrap())
        .unwrap();
    let response: serde_json::Value = client
        .post(format!("{base}/api/v1/auth/login/finish"))
        .header("origin", ORIGIN)
        .header("cookie", flow)
        .json(&assertion)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(response["owner_id"], owner.to_string());
    assert!(!client
        .post(format!("{base}/api/v1/sync/desktop/start"))
        .json(&serde_json::json!({}))
        .send()
        .unwrap()
        .status()
        .is_success());
}
