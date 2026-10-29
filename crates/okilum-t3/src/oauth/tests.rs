use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::testing::{FakeServer, Request, Response};

#[derive(Default)]
struct Seen {
    challenge: Option<String>,
    redirect_uri: Option<String>,
}

/// T3's `/oauth/mcp/*` endpoints. `iss` is what the authorize redirect sends back.
fn fake_t3_oauth(
    iss: impl Fn(&str) -> String + Send + Sync + 'static,
) -> (FakeServer, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let state = seen.clone();
    let server = FakeServer::start(move |request: &Request| {
        let origin = format!("http://{}", request.header("host").unwrap());
        match (request.method.as_str(), request.path()) {
            ("GET", "/.well-known/oauth-authorization-server") => Response::json(
                200,
                json!({
                    "issuer": origin,
                    "authorization_endpoint": format!("{origin}/oauth/mcp/authorize"),
                    "token_endpoint": format!("{origin}/oauth/mcp/token"),
                    "registration_endpoint": format!("{origin}/oauth/mcp/register"),
                    "response_types_supported": ["code"],
                    "grant_types_supported": ["authorization_code"],
                    "code_challenge_methods_supported": ["S256"],
                    "token_endpoint_auth_methods_supported": ["none"],
                    "scopes_supported": ["orchestration:read", "orchestration:operate"],
                    "authorization_response_iss_parameter_supported": true
                }),
            ),
            ("POST", "/oauth/mcp/register") => {
                let body = request.json();
                assert_eq!(body["client_name"], "Okilum on test");
                assert_eq!(body["token_endpoint_auth_method"], "none");
                let redirect = body["redirect_uris"][0].as_str().unwrap();
                assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
                Response::json(
                    201,
                    json!({ "client_id": "signed-client", "client_name": "Okilum on test" }),
                )
            }
            ("GET", "/oauth/mcp/authorize") => {
                let url = Url::parse(&format!("{origin}{}", request.target)).unwrap();
                let param = |name: &str| {
                    url.query_pairs()
                        .find(|(k, _)| k == name)
                        .map(|(_, v)| v.into_owned())
                        .unwrap()
                };
                assert_eq!(param("client_id"), "signed-client");
                assert_eq!(param("code_challenge_method"), "S256");
                assert_eq!(param("scope"), SCOPE);
                assert_eq!(param("resource"), format!("{origin}/mcp"));
                let redirect = param("redirect_uri");
                let mut seen = state.lock().unwrap();
                seen.challenge = Some(param("code_challenge"));
                seen.redirect_uri = Some(redirect.clone());
                let mut back = Url::parse(&redirect).unwrap();
                back.query_pairs_mut()
                    .append_pair("code", "one-use-code")
                    .append_pair("state", &param("state"))
                    .append_pair("iss", &iss(&origin));
                Response::empty(302).with_header("location", back.as_str())
            }
            ("POST", "/oauth/mcp/token") => {
                let form: Vec<(String, String)> =
                    url::form_urlencoded::parse(request.body.as_bytes())
                        .into_owned()
                        .collect();
                let field = |name: &str| form.iter().find(|(k, _)| k == name).unwrap().1.clone();
                let seen = state.lock().unwrap();
                assert_eq!(field("grant_type"), "authorization_code");
                assert_eq!(field("code"), "one-use-code");
                assert_eq!(field("client_id"), "signed-client");
                assert_eq!(Some(field("redirect_uri")), seen.redirect_uri);
                assert_eq!(field("resource"), format!("{origin}/mcp"));
                // The verifier must hash to the challenge the browser carried.
                assert_eq!(
                    Some(Pkce::from_verifier(field("code_verifier")).challenge),
                    seen.challenge
                );
                Response::json(
                    200,
                    json!({ "access_token": "issued-token", "token_type": "Bearer",
                            "expires_in": 2_592_000, "scope": SCOPE }),
                )
            }
            (method, path) => panic!("unexpected {method} {path}"),
        }
    });
    (server, seen)
}

/// Plays the browser: follows the authorize redirect back to the loopback port.
fn browser(url: &Url) -> Result<()> {
    let url = url.clone();
    std::thread::spawn(move || {
        let _ = reqwest::blocking::get(url);
    });
    Ok(())
}

#[test]
fn rfc7636_challenge_vector() {
    let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
    assert_eq!(
        pkce.challenge,
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn random_tokens_differ() {
    let (a, b) = (random_token().unwrap(), random_token().unwrap());
    assert_ne!(a, b);
    assert_eq!(a.len(), 43);
}

#[test]
fn connect_runs_the_whole_flow() {
    let (server, _) = fake_t3_oauth(|origin| origin.to_owned());
    let env = Environment::new("test", &server.origin).unwrap();
    let before = unix_now();
    let token = connect(&env, "Okilum on test", browser, Duration::from_secs(10)).unwrap();
    assert_eq!(token.access_token, "issued-token");
    assert_eq!(token.client_id, "signed-client");
    assert!(token.expires_at >= before + 2_592_000);
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|r| r.path().to_owned())
        .collect();
    assert_eq!(
        paths,
        [
            "/.well-known/oauth-authorization-server",
            "/oauth/mcp/register",
            "/oauth/mcp/authorize",
            "/oauth/mcp/token"
        ]
    );
}

#[test]
fn an_answer_from_another_issuer_is_refused() {
    let (server, _) = fake_t3_oauth(|_| "https://evil.example".to_owned());
    let env = Environment::new("test", &server.origin).unwrap();
    let error = connect(&env, "Okilum on test", browser, Duration::from_secs(10)).unwrap_err();
    assert!(error.to_string().contains("issuer"), "{error}");
    // The code was never exchanged.
    assert!(server
        .requests()
        .iter()
        .all(|r| r.path() != "/oauth/mcp/token"));
}

#[test]
fn metadata_pointing_elsewhere_is_refused() {
    let server = FakeServer::start(|_| {
        Response::json(
            200,
            json!({ "issuer": "https://elsewhere.example",
                    "authorization_endpoint": "https://elsewhere.example/a",
                    "token_endpoint": "https://elsewhere.example/t",
                    "code_challenge_methods_supported": ["S256"] }),
        )
    });
    let env = Environment::new("test", &server.origin).unwrap();
    let error = discover(&http_client().unwrap(), &env).unwrap_err();
    assert!(error.to_string().contains("issuer"), "{error}");
}

#[test]
fn the_callback_checks_state_and_skips_other_paths() {
    let callback = Callback::bind().unwrap();
    let redirect = callback.redirect_uri().to_owned();
    let port_root = redirect.trim_end_matches(CALLBACK_PATH).to_owned();
    std::thread::spawn(move || {
        let favicon = reqwest::blocking::get(format!("{port_root}/favicon.ico")).unwrap();
        assert_eq!(favicon.status(), 404);
        let wrong = reqwest::blocking::get(format!("{redirect}?code=c&state=forged")).unwrap();
        assert_eq!(wrong.status(), 400);
    });
    let error = callback
        .wait("expected", None, Duration::from_secs(10))
        .unwrap_err();
    assert!(error.to_string().contains("different state"), "{error}");
}

#[test]
fn the_callback_gives_up_after_the_timeout() {
    let callback = Callback::bind().unwrap();
    let error = callback
        .wait("s", None, Duration::from_millis(100))
        .unwrap_err();
    assert!(error.to_string().contains("no answer"), "{error}");
}

#[test]
fn a_denied_approval_is_reported() {
    let callback = Callback::bind().unwrap();
    let redirect = callback.redirect_uri().to_owned();
    std::thread::spawn(move || {
        let _ = reqwest::blocking::get(format!(
            "{redirect}?error=access_denied&error_description=Declined&state=s"
        ));
    });
    let error = callback
        .wait("s", None, Duration::from_secs(10))
        .unwrap_err();
    assert!(error.to_string().contains("access_denied"), "{error}");
}

#[test]
fn a_pasted_callback_after_loopback_timeout_keeps_pkce_and_rejects_foreign_answers() {
    let (server, _) = fake_t3_oauth(str::to_owned);
    let env = Environment::new("test", &server.origin).unwrap();
    let pending = Authorization::prepare(&env, "Okilum on test").unwrap();
    // Browser approved, but never delivered its redirect to the listener.
    let response = http_client()
        .unwrap()
        .get(pending.url().clone())
        .send()
        .unwrap();
    let callback = response.headers()["location"].to_str().unwrap().to_owned();
    let (send, receive) = std::sync::mpsc::channel();
    let (events, progress) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        pending.wait(
            receive,
            |p| {
                let _ = events.send(p);
            },
            Duration::from_millis(100),
        )
    });
    loop {
        if matches!(
            progress.recv_timeout(Duration::from_secs(3)).unwrap(),
            Progress::TimedOut
        ) {
            break;
        }
    }
    let url = Url::parse(&callback).unwrap();
    assert!(
        TcpStream::connect(url.socket_addrs(|| None).unwrap()[0]).is_err(),
        "positive control: the loopback port really closed"
    );
    for (name, value) in [("state", "foreign"), ("iss", "https://foreign.example")] {
        let mut forged = url.clone();
        let pairs: Vec<_> = forged
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        forged.set_query(None);
        for (k, v) in pairs {
            forged
                .query_pairs_mut()
                .append_pair(&k, if k == name { value } else { &v });
        }
        send.send(Input::Callback(forged.to_string())).unwrap();
        loop {
            if let Progress::InvalidCallback(error) =
                progress.recv_timeout(Duration::from_secs(3)).unwrap()
            {
                assert!(error.contains(name), "{error}");
                break;
            }
        }
        assert!(!server
            .requests()
            .iter()
            .any(|r| r.path() == "/oauth/mcp/token"));
    }
    send.send(Input::Callback(callback)).unwrap();
    let token = worker.join().unwrap().unwrap();
    assert_eq!(token.access_token, "issued-token");
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.path() == "/oauth/mcp/token")
            .count(),
        1
    );
}

#[test]
fn waiting_longer_reopens_the_original_callback_and_cancel_is_bounded() {
    let (server, _) = fake_t3_oauth(str::to_owned);
    let pending = Authorization::prepare(
        &Environment::new("test", &server.origin).unwrap(),
        "Okilum on test",
    )
    .unwrap();
    let response = http_client()
        .unwrap()
        .get(pending.url().clone())
        .send()
        .unwrap();
    let callback = response.headers()["location"].to_str().unwrap().to_owned();
    let (send, receive) = std::sync::mpsc::channel();
    let (events, progress) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        pending.wait(
            receive,
            |p| {
                let _ = events.send(p);
            },
            Duration::from_millis(200),
        )
    });
    while !matches!(
        progress.recv_timeout(Duration::from_secs(3)).unwrap(),
        Progress::TimedOut
    ) {}
    send.send(Input::WaitLonger).unwrap();
    while !matches!(
        progress.recv_timeout(Duration::from_secs(3)).unwrap(),
        Progress::Waiting(_)
    ) {}
    assert!(reqwest::blocking::get(&callback)
        .unwrap()
        .status()
        .is_success());
    assert_eq!(worker.join().unwrap().unwrap().access_token, "issued-token");
    let pending = Authorization::prepare(
        &Environment::new("test", &server.origin).unwrap(),
        "Okilum on test",
    )
    .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    send.send(Input::Cancel).unwrap();
    assert!(pending
        .wait(receive, |_| {}, DEFAULT_WAIT)
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
}
