//! The editor bridge on real drawing files and the note FileEditor: routing,
//! every guard, the Apply/Undo/conflict choices, and the real loopback socket.
//! The scene rules themselves are tested in okilum-core.
use super::*;
use ::core::prelude::v1::test;
use serde_json::{json, Value};
use std::io::{Read, Write};

const BOARD: &str =
    include_str!("../../../okilum-core/tests/fixtures/excalidraw/board-json.excalidraw.md");
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const AUTHORITY: &str = "127.0.0.1:4321";
const ORIGIN: &str = "http://127.0.0.1:4321";

struct Vault {
    dir: tempfile::TempDir,
    file: PathBuf,
}

impl Vault {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Board.excalidraw.md");
        std::fs::write(&file, BOARD).unwrap();
        Self { dir, file }
    }
    fn drawing(&self) -> DrawingEdit {
        DrawingEdit::open(self.dir.path(), &self.file, &self.dir.path().join(".state")).unwrap()
    }
    fn session(&self) -> Session<okilum_core::file_editor::FileEditor> {
        Session::new(self.drawing(), AUTHORITY.into(), TOKEN)
    }
    fn read(&self) -> String {
        std::fs::read_to_string(&self.file).unwrap()
    }
}

fn request(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Request {
    let mut map = BTreeMap::from([("host".to_owned(), AUTHORITY.to_owned())]);
    for (key, value) in headers {
        map.insert(key.to_string(), value.to_string());
    }
    Request {
        method: method.into(),
        path: path.into(),
        headers: map,
        body: body.to_vec(),
    }
}

fn get(session: &mut Session<impl DrawingStore>, route: &str) -> Response {
    session.handle(&request("GET", &format!("/{TOKEN}/{route}"), &[], b""))
}

fn post(session: &mut Session<impl DrawingStore>, action: &str, body: Value) -> (u16, Value) {
    let response = session.handle(&request(
        "POST",
        &format!("/{TOKEN}/api/{action}"),
        &[("origin", ORIGIN), ("content-type", "application/json")],
        &serde_json::to_vec(&body).unwrap(),
    ));
    (
        response.status,
        serde_json::from_slice(&response.body).unwrap(),
    )
}

fn json_of(response: &Response) -> Value {
    serde_json::from_slice(&response.body).unwrap()
}

/// What the page does on open: fetch the scene and report it as loaded.
fn load(session: &mut Session<impl DrawingStore>) -> Value {
    let opened = json_of(&get(session, "api/session"));
    assert_eq!(opened["loaded"], false);
    let scene = opened["scene"].clone();
    let (status, reply) = post(
        session,
        "loaded",
        json!({ "revision": opened["revision"], "scene": scene }),
    );
    assert_eq!(status, 200, "{reply}");
    scene
}

/// Move the first element, as a drag in the editor would.
fn moved(scene: &Value) -> Value {
    let mut scene = scene.clone();
    let element = &mut scene["elements"][0];
    element["x"] = json!(element["x"].as_f64().unwrap() + 40.0);
    element["version"] = json!(element["version"].as_i64().unwrap_or(1) + 1);
    element["versionNonce"] = json!(987_654_321);
    scene
}

#[test]
fn the_page_and_its_assets_are_served_only_under_the_capability() {
    let vault = Vault::new();
    let mut session = vault.session();
    let index = get(&mut session, "");
    assert_eq!(index.status, 200);
    assert!(index.gzip && index.mime.starts_with("text/html"));
    let mut html = String::new();
    std::io::Read::read_to_string(
        &mut flate2::read::GzDecoder::new(index.body.as_slice()),
        &mut html,
    )
    .unwrap();
    assert!(html.contains("assets/editor.js"), "{html}");
    for (route, _, _, _) in ASSETS {
        assert_eq!(get(&mut session, route).status, 200, "{route}");
    }
    assert!(ASSETS
        .iter()
        .any(|(path, ..)| path.starts_with("fonts/Excalifont/")));
    // The token is the only way in, and paths are exact names, not files.
    for path in [
        "/".to_owned(),
        "/index.html".to_owned(),
        format!("/{}/", &TOKEN[..63]),
        format!("/{TOKEN}/../{TOKEN}/index.html"),
        format!("/{TOKEN}/assets/../index.html"),
        format!("/{TOKEN}/Board.excalidraw.md"),
    ] {
        assert_eq!(
            session.handle(&request("GET", &path, &[], b"")).status,
            404,
            "{path}"
        );
    }
}

#[test]
fn foreign_hosts_origins_and_bodies_are_refused_and_nothing_is_written() {
    let vault = Vault::new();
    let mut session = vault.session();
    let scene = load(&mut session);
    let body = serde_json::to_vec(&json!({ "revision": 0, "scene": moved(&scene) })).unwrap();
    let apply = format!("/{TOKEN}/api/apply");
    let json = ("content-type", "application/json");
    // DNS rebinding: a page on evil.example resolving to 127.0.0.1.
    let mut rebound = request("POST", &apply, &[("origin", ORIGIN), json], &body);
    rebound
        .headers
        .insert("host".into(), "evil.example:4321".into());
    assert_eq!(session.handle(&rebound).status, 421);
    let mut rebound_get = request("GET", &format!("/{TOKEN}/api/session"), &[], b"");
    rebound_get
        .headers
        .insert("host".into(), "evil.example:4321".into());
    assert_eq!(session.handle(&rebound_get).status, 421);
    // A cross-site page, a missing origin, and a form post.
    let cross = request(
        "POST",
        &apply,
        &[("origin", "https://evil.example"), json],
        &body,
    );
    assert_eq!(session.handle(&cross).status, 403);
    assert_eq!(
        session
            .handle(&request("POST", &apply, &[json], &body))
            .status,
        403
    );
    let form = request(
        "POST",
        &apply,
        &[("origin", ORIGIN), ("content-type", "text/plain")],
        &body,
    );
    assert_eq!(session.handle(&form).status, 415);
    // Unknown fields and actions.
    let (status, _) = post(
        &mut session,
        "apply",
        json!({ "revision": 0, "path": "/etc/passwd" }),
    );
    assert_eq!(status, 400);
    let (status, _) = post(&mut session, "write", json!({ "revision": 0 }));
    assert_eq!(status, 404);
    assert_eq!(vault.read(), BOARD);
    // Control: the same body with the page's origin and host is written.
    let accepted = request("POST", &apply, &[("origin", ORIGIN), json], &body);
    let response = session.handle(&accepted);
    assert_eq!(response.status, 200, "{}", json_of(&response));
    assert_ne!(vault.read(), BOARD);
}

#[test]
fn apply_then_undo_restores_the_exact_bytes_and_reports_both() {
    let vault = Vault::new();
    let mut session = vault.session();
    let scene = load(&mut session);
    let (status, reply) = post(
        &mut session,
        "apply",
        json!({ "revision": 0, "scene": scene }),
    );
    assert_eq!((status, reply["status"].as_str()), (200, Some("unchanged")));
    assert!(session.take_events().is_empty());
    let (status, reply) = post(
        &mut session,
        "apply",
        json!({ "revision": 0, "scene": moved(&scene) }),
    );
    assert_eq!(
        (status, reply["status"].as_str()),
        (200, Some("saved")),
        "{reply}"
    );
    assert_eq!(
        (reply["revision"].as_u64(), reply["canUndo"].as_bool()),
        (Some(1), Some(true))
    );
    assert_eq!(session.take_events(), vec![Event::Saved]);
    let saved = vault.read();
    assert_ne!(saved, BOARD);
    // A call with an old revision is refused, whatever it carries.
    let (status, _) = post(&mut session, "undo", json!({ "revision": 0 }));
    assert_eq!(status, 422);
    assert_eq!(vault.read(), saved);
    let (status, reply) = post(&mut session, "undo", json!({ "revision": 1 }));
    assert_eq!(
        (status, reply["status"].as_str()),
        (200, Some("restored")),
        "{reply}"
    );
    assert!(reply["scene"]["elements"].is_array());
    assert_eq!(session.take_events(), vec![Event::Restored]);
    assert_eq!(vault.read(), BOARD);
    // After Undo the page must load the restored scene before it can Apply.
    let (status, _) = post(
        &mut session,
        "apply",
        json!({ "revision": 2, "scene": moved(&scene) }),
    );
    assert_eq!(status, 422);
    assert_eq!(vault.read(), BOARD);
}

#[test]
fn an_outside_change_is_never_overwritten_without_consent_to_that_version() {
    let vault = Vault::new();
    let mut session = vault.session();
    let scene = load(&mut session);
    let outside = BOARD.replacen("%%\n## Drawing", "Outside note.\n\n%%\n## Drawing", 1);
    assert_ne!(outside, BOARD);
    std::fs::write(&vault.file, &outside).unwrap();
    let (status, reply) = post(
        &mut session,
        "apply",
        json!({ "revision": 0, "scene": moved(&scene) }),
    );
    assert_eq!(
        (status, reply["status"].as_str()),
        (409, Some("conflict")),
        "{reply}"
    );
    let seen = reply["conflict"].as_str().unwrap().to_owned();
    assert_eq!(seen, digest(&outside));
    assert_eq!(vault.read(), outside);
    // The file changes again after the page showed the conflict: consent to
    // the version it saw does not cover this one.
    let again = outside.replacen("Outside note.", "Outside note, edited.", 1);
    std::fs::write(&vault.file, &again).unwrap();
    let (status, reply) = post(
        &mut session,
        "keep-mine",
        json!({ "revision": 0, "reviewed": seen }),
    );
    assert_eq!(
        (status, reply["status"].as_str()),
        (409, Some("conflict")),
        "{reply}"
    );
    assert_eq!(vault.read(), again);
    let (status, reply) = post(
        &mut session,
        "keep-mine",
        json!({ "revision": 0, "reviewed": "0" }),
    );
    assert_eq!(status, 409, "{reply}");
    assert_eq!(vault.read(), again);
    // Save as copy keeps the edit next to the file, which stays as it is.
    let (status, reply) = post(&mut session, "save-copy", json!({ "revision": 0 }));
    assert_eq!(status, 200, "{reply}");
    let copy = vault.dir.path().join(reply["copy"].as_str().unwrap());
    assert!(copy.to_string_lossy().ends_with(".excalidraw.md"));
    assert!(std::fs::read_to_string(&copy)
        .unwrap()
        .contains("## Drawing"));
    assert!(matches!(
        session.take_events().as_slice(),
        [Event::Copied(_)]
    ));
    assert_eq!(vault.read(), again);
    let (status, _) = post(&mut session, "close", json!({ "revision": 0 }));
    assert_eq!(status, 422, "Done must wait for a conflict choice");
    assert!(!session.closed());
    // Keep mine on the version now reported writes the edit.
    let current = reply["conflict"].as_str().unwrap().to_owned();
    let (status, reply) = post(
        &mut session,
        "keep-mine",
        json!({ "revision": 0, "reviewed": current }),
    );
    assert_eq!(
        (status, reply["status"].as_str()),
        (200, Some("saved")),
        "{reply}"
    );
    assert_eq!(reply["conflict"], Value::Null);
    assert_ne!(vault.read(), again);
    assert_eq!(session.take_events(), vec![Event::Saved]);
}

#[test]
fn reload_takes_the_file_and_a_second_tab_sees_the_open_session() {
    let vault = Vault::new();
    let mut session = vault.session();
    load(&mut session);
    let second = json_of(&get(&mut session, "api/session"));
    assert_eq!(second["loaded"], true);
    let (status, reply) = post(&mut session, "reload", json!({ "revision": 0 }));
    assert_eq!((status, reply["loaded"].as_bool()), (200, Some(false)));
    assert_eq!(reply["revision"], 1);
    // The first tab's revision is now stale.
    let (status, _) = post(
        &mut session,
        "apply",
        json!({ "revision": 0, "scene": reply["scene"] }),
    );
    assert_eq!(status, 422);
    assert_eq!(vault.read(), BOARD);
}

#[test]
fn done_ends_the_session_and_leaves_an_unapplied_drawing_alone() {
    let vault = Vault::new();
    let mut session = vault.session();
    load(&mut session);
    let (status, reply) = post(&mut session, "close", json!({ "revision": 0 }));
    assert_eq!((status, reply["status"].as_str()), (200, Some("closed")));
    assert!(session.closed());
    assert!(json_of(&get(&mut session, "api/session"))["error"].is_string());
    assert_eq!(get(&mut session, "api/session").status, 410);
    assert_eq!(vault.read(), BOARD);
}

#[test]
fn the_request_parser_refuses_what_it_does_not_frame() {
    let ok = parse_head(b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 2\r\n\r\n").unwrap();
    assert_eq!((ok.0.method.as_str(), ok.1), ("POST", 2));
    for head in [
        "POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
        "POST /x HTTP/1.1\r\nHost: a\r\nhost: b\r\n\r\n",
        "GET /x HTTP/1.1\r\nContent-Length: 1\r\n\r\n",
        "PUT /x HTTP/1.1\r\n\r\n",
        "GET http://evil/ HTTP/1.1\r\n\r\n",
        "GET //evil/ HTTP/1.1\r\n\r\n",
        "GET /x HTTP/2\r\n\r\n",
        "POST /x HTTP/1.1\r\nContent-Length: -1\r\n\r\n",
        "GET /x HTTP/1.1\r\nBad Header: 1\r\n\r\n",
    ] {
        assert!(parse_head(head.as_bytes()).is_err(), "{head:?}");
    }
    let huge = format!(
        "POST /x HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        BODY_LIMIT + 1
    );
    assert!(parse_head(huge.as_bytes()).is_err());
}

fn exchange(address: &str, raw: &[u8]) -> std::io::Result<(String, Vec<u8>)> {
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(raw)?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response head")
        + 4;
    let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
    Ok((head, bytes[split..].to_vec()))
}

#[test]
fn the_loopback_server_applies_through_a_real_socket_and_stops_on_done() {
    let vault = Vault::new();
    let bridge = Bridge::start(vault.drawing()).unwrap();
    let url = url::Url::parse(&bridge.url).unwrap();
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let authority = format!("127.0.0.1:{}", url.port().unwrap());
    let token = url.path().trim_matches('/').to_owned();
    assert_eq!(token.len(), 64);
    let origin = format!("http://{authority}");

    let (head, _) = exchange(
        &authority,
        format!("GET /{token}/ HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes(),
    )
    .unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("Content-Encoding: gzip"), "{head}");
    assert!(
        head.contains("Content-Security-Policy: default-src 'none'"),
        "{head}"
    );

    let (head, body) = exchange(
        &authority,
        format!("GET /{token}/api/session HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes(),
    )
    .unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let opened: Value = serde_json::from_slice(&body).unwrap();
    let post = |action: &str, body: Value| {
        let body = serde_json::to_vec(&body).unwrap();
        let mut raw = format!(
            "POST /{token}/api/{action} HTTP/1.1\r\nHost: {authority}\r\nOrigin: {origin}\r\n\
Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        raw.extend_from_slice(&body);
        let (head, body) = exchange(&authority, &raw).unwrap();
        (head, serde_json::from_slice::<Value>(&body).unwrap())
    };
    let scene = opened["scene"].clone();
    let (head, _) = post("loaded", json!({ "revision": 0, "scene": scene }));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let (head, reply) = post("apply", json!({ "revision": 0, "scene": moved(&scene) }));
    assert!(head.starts_with("HTTP/1.1 200"), "{head} {reply}");
    assert_ne!(vault.read(), BOARD);
    assert_eq!(bridge.events.try_recv(), Ok(Event::Saved));

    let (head, _) = post("close", json!({ "revision": 1 }));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    // The listener stops and the session's editor lock is released: the
    // drawing can be opened for editing again.
    let started = Instant::now();
    while TcpStream::connect(&authority).is_ok() || !bridge.events.is_closed() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "bridge kept running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!bridge.running());
    let started = Instant::now();
    while let Err(error) = DrawingEdit::open(
        vault.dir.path(),
        &vault.file,
        &vault.dir.path().join(".state"),
    ) {
        assert!(started.elapsed() < Duration::from_secs(10), "{error:#}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn dropping_the_bridge_stops_it_without_touching_the_drawing() {
    let vault = Vault::new();
    let bridge = Bridge::start(vault.drawing()).unwrap();
    let url = url::Url::parse(&bridge.url).unwrap();
    let authority = format!("127.0.0.1:{}", url.port().unwrap());
    let events = bridge.events.clone();
    drop(bridge);
    let started = Instant::now();
    while !events.is_closed() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "bridge kept running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(TcpStream::connect(&authority).is_err());
    assert_eq!(vault.read(), BOARD);
}

#[test]
fn the_embedded_page_is_the_checked_build() {
    // build.mjs --check proves SHA256SUMS is a reproducible rebuild; this
    // proves the binary embeds exactly those bytes, for every served file.
    let sums = include_str!("../../assets/excalidraw-editor/SHA256SUMS");
    let listed: HashMap<&str, &str> = sums
        .lines()
        .filter_map(|line| line.split_once("  "))
        .map(|(sum, path)| (path, sum))
        .collect();
    assert!(ASSETS.len() > 100, "{}", ASSETS.len());
    for (path, _, gzip, bytes) in ASSETS {
        let stored = if *gzip {
            format!("{path}.gz")
        } else {
            path.to_string()
        };
        let sum = sha2::Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                use std::fmt::Write as _;
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        assert_eq!(listed.get(stored.as_str()), Some(&sum.as_str()), "{stored}");
    }
    assert!(ASSETS.iter().any(|(path, ..)| *path == "index.html"));
}
