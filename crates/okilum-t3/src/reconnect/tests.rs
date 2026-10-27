use super::*;
use crate::testing::{FakeServer, Response};
use crate::ManagerGateway;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

fn token(remaining: u64) -> StoredToken {
    StoredToken {
        access_token: "saved-test-token".into(),
        expires_at: crate::unix_now() + remaining,
        scope: "orchestration:read orchestration:operate".into(),
        client_id: "client".into(),
    }
}
fn server(compatible: bool) -> FakeServer {
    FakeServer::start(move |request| {
        let message = request.json();
        assert_eq!(
            request.header("authorization"),
            Some("Bearer saved-test-token")
        );
        match message["method"].as_str().unwrap() {
            "initialize" => {
                Response::json(200, json!({"jsonrpc":"2.0","id":message["id"],"result":{}}))
            }
            "notifications/initialized" => Response::empty(202),
            "tools/list" => Response::json(
                200,
                json!({"jsonrpc":"2.0","id":message["id"],"result":{"tools":
                if compatible {json!([{"name":"t3_environment_read"},{"name":"t3_thread_read"},{"name":"t3_thread_send"}])} else {json!([])} }}),
            ),
            other => panic!("reconnect must not call feature {other}"),
        }
    })
}
fn environment(server: &FakeServer) -> Environment {
    Environment::new("test", &server.origin).unwrap()
}

#[test]
fn saved_valid_token_reconnects_without_oauth_or_credential_rewrite() {
    let server = server(true);
    let env = environment(&server);
    let token = token(30 * 24 * 60 * 60);
    let expiry = token.expires_at;
    let connection = connect(&env, Some((env.credential_url(), token)), false, || {
        panic!("RECONNECT1773: valid saved token must not start OAuth")
    })
    .unwrap();
    assert!(connection.issued_token.is_none());
    assert_eq!(connection.gateway.token_expires_at(), Some(expiry));
    assert_eq!(
        server
            .requests()
            .iter()
            .map(|r| r.json()["method"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["initialize", "notifications/initialized", "tools/list"]
    );
}

#[test]
fn saved_expiring_token_stays_reusable_until_expiry() {
    let server = server(true);
    let env = environment(&server);
    assert_eq!(crate::EXPIRES_SOON_SECS, 3 * 24 * 60 * 60);
    let connection = connect(
        &env,
        Some((env.credential_url(), token(crate::EXPIRES_SOON_SECS))),
        false,
        || panic!("RECONNECT1773: expiring token still works; Renew is explicit"),
    )
    .unwrap();
    assert!(matches!(
        connection.gateway.status(),
        crate::GatewayStatus::ExpiresSoon { .. }
    ));
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn explicit_invalid_mcp_credential_401_allows_one_oauth_flow() {
    for challenge in [false, true] {
        let server = FakeServer::start(move |_| {
            if challenge {
                Response::json(401, json!({})).with_header(
                    "www-authenticate",
                    "Bearer error=\"invalid_mcp_credential\", resource_metadata=\"test\"",
                )
            } else {
                Response::json(401, json!({"error":"invalid_mcp_credential"}))
            }
        });
        let env = environment(&server);
        let authorizations = AtomicUsize::new(0);
        let connection = connect(
            &env,
            Some((env.credential_url(), token(30 * 86400))),
            false,
            || {
                authorizations.fetch_add(1, Ordering::SeqCst);
                Ok(token(30 * 86400))
            },
        )
        .unwrap();
        assert_eq!(authorizations.load(Ordering::SeqCst), 1);
        assert!(connection.issued_token.is_some());
        assert_eq!(
            server.requests().len(),
            1,
            "one rejected initialize, no blind retry"
        );
    }
}

#[test]
fn network_generic_401_and_compatibility_failures_never_start_oauth() {
    for status in [401, 503] {
        let server = FakeServer::start(move |_| Response::empty(status));
        let env = environment(&server);
        let result = connect(
            &env,
            Some((env.credential_url(), token(30 * 86400))),
            false,
            || panic!("unconfirmed failure must not ask for pairing"),
        );
        assert!(result.is_err());
        assert_eq!(server.requests().len(), 1);
    }
    let incompatible = server(false);
    let env = environment(&incompatible);
    let result = connect(
        &env,
        Some((env.credential_url(), token(30 * 86400))),
        false,
        || panic!("missing tools must not ask for pairing"),
    );
    assert!(result.err().unwrap().to_string().contains("required tool"));
    let env = {
        let closed = server(true);
        environment(&closed)
    };
    assert!(connect(
        &env,
        Some((env.credential_url(), token(30 * 86400))),
        false,
        || panic!("network failure must not ask for pairing")
    )
    .is_err());
}

#[test]
fn changed_origin_never_sends_the_previous_servers_token() {
    let old = server(true);
    let new = server(true);
    let env = environment(&new);
    let authorizations = AtomicUsize::new(0);
    let connection = connect(
        &env,
        Some((environment(&old).credential_url(), token(30 * 86400))),
        false,
        || {
            authorizations.fetch_add(1, Ordering::SeqCst);
            Ok(token(30 * 86400))
        },
    )
    .unwrap();
    assert_eq!(authorizations.load(Ordering::SeqCst), 1);
    assert!(connection.issued_token.is_some());
    assert!(
        old.requests().is_empty() && new.requests().is_empty(),
        "old credential cannot reach the new origin"
    );
    connection.gateway.connect().unwrap();
    assert_eq!(new.requests().len(), 3);
}

#[test]
fn missing_expired_or_explicit_sign_in_uses_a_new_authorization() {
    for mode in ["missing", "expired", "explicit"] {
        let server = server(true);
        let env = environment(&server);
        let mut saved = token(30 * 86400);
        if mode == "expired" {
            saved.expires_at = crate::unix_now() - 1;
        }
        let saved = (mode != "missing").then(|| (env.credential_url(), saved));
        let authorizations = AtomicUsize::new(0);
        let connection = connect(&env, saved, mode == "explicit", || {
            authorizations.fetch_add(1, Ordering::SeqCst);
            Ok(token(30 * 86400))
        })
        .unwrap();
        assert_eq!(authorizations.load(Ordering::SeqCst), 1);
        assert!(connection.issued_token.is_some());
        assert!(
            server.requests().is_empty(),
            "unusable/skipped token is not sent"
        );
    }
}
