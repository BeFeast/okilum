use super::*;
use crate::testing::{FakeServer, Request, Response};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

const BEARER: &str = "fixture-native-secret";
const TICKET: &str = "fixture-ticket-secret";
const PAIRING: &str = "fixture-pairing-secret";

fn token(environment: &Environment) -> NativeToken {
    NativeToken::from_secret(
        environment,
        &serde_json::to_vec(&json!({
            "origin":environment.origin(),"access_token":BEARER,
            "expires_at":crate::unix_now()+3600,"scope":"orchestration:read"
        }))
        .unwrap(),
    )
    .unwrap()
}

fn client(environment: &Environment) -> NativeClient {
    NativeClient::new(
        environment.clone(),
        token(environment),
        Duration::from_secs(2),
    )
    .unwrap()
}

struct Fixture {
    env: Environment,
    server: JoinHandle<()>,
}

impl Fixture {
    fn start(run: impl FnOnce(TcpListener) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let env = Environment::new(
            "fixture",
            &format!("http://{}", listener.local_addr().unwrap()),
        )
        .unwrap();
        let server = thread::spawn(move || run(listener));
        Self { env, server }
    }
    fn finish(self) {
        self.server.join().unwrap();
    }
}

fn accept(listener: &TcpListener) -> TcpStream {
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "fixture never received a connection"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fixture accept: {error}"),
        }
    }
}

fn ticket(listener: &TcpListener) {
    let mut stream = accept(listener);
    let mut reader = BufReader::new(&mut stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        headers.push_str(&line);
    }
    assert!(headers.starts_with("POST /api/auth/websocket-ticket HTTP/1.1\r\n"));
    assert!(headers
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {BEARER}")));
    drop(reader);
    let body = json!({"ticket":TICKET,"expiresAt":"2099-01-01T00:00:00Z"}).to_string();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}

struct VerifyUpgrade;

impl tungstenite::handshake::server::Callback for VerifyUpgrade {
    fn on_request(
        self,
        request: &tungstenite::handshake::server::Request,
        response: tungstenite::handshake::server::Response,
    ) -> Result<
        tungstenite::handshake::server::Response,
        tungstenite::handshake::server::ErrorResponse,
    > {
        let url = url::Url::parse(&format!("http://fixture{}", request.uri())).unwrap();
        assert_eq!(url.path(), "/ws");
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            query.get("orchestrationProtocol").map(|s| s.as_ref()),
            Some("2")
        );
        assert_eq!(query.get("wsTicket").map(|s| s.as_ref()), Some(TICKET));
        assert!(!request.uri().to_string().contains(BEARER));
        assert!(request.headers().get("authorization").is_none());
        Ok(response)
    }
}

fn socket(listener: &TcpListener) -> WebSocket<TcpStream> {
    tungstenite::accept_hdr(accept(listener), VerifyUpgrade).unwrap()
}

fn send(socket: &mut WebSocket<TcpStream>, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .unwrap();
}

fn read(socket: &mut WebSocket<TcpStream>) -> Value {
    serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap()
}

fn request(socket: &mut WebSocket<TcpStream>, after: Option<u64>) {
    let request = read(socket);
    assert_eq!(request["_tag"], "Request");
    assert_eq!(request["tag"], "orchestration.subscribeThread");
    assert_eq!(request["headers"], json!([]));
    assert_eq!(request["payload"]["threadId"], "thread-a");
    assert_eq!(request["payload"]["acceptBoundedSnapshot"], true);
    assert_eq!(request["payload"]["requestCompletionMarker"], true);
    assert_eq!(request["payload"]["afterSequence"].as_u64(), after);
    assert!(request["payload"].get("acceptCompactTurnItems").is_none());
    assert_eq!(read(socket), json!({"_tag":"Ping"}));
    send(socket, json!({"_tag":"Pong"}));
}

fn snapshot(sequence: u64) -> Value {
    json!({"kind":"snapshot","snapshotSequence":sequence,
        "projection":{"thread":{"id":"thread-a"},"turnItems":[]},
        "historyCursor":"opaque+/cursor","hasMoreHistory":true})
}

fn event(sequence: u64, kind: &str) -> Value {
    json!({"kind":"event","sequence":sequence,"event":{"type":kind,"payload":{"future":"retained"}}})
}

fn chunk(socket: &mut WebSocket<TcpStream>, values: Vec<Value>) {
    send(
        socket,
        json!({"_tag":"Chunk","requestId":"1","values":values}),
    );
}

fn ack(socket: &mut WebSocket<TcpStream>) {
    assert_eq!(read(socket), json!({"_tag":"Ack","requestId":"1"}));
}

#[test]
fn pairing_narrows_scopes_and_has_separate_origin_bound_keychain_payload() {
    let server = FakeServer::start(|request: &Request| {
        assert_eq!(request.path(), "/oauth/token");
        let form: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(request.body.as_bytes()).collect();
        assert_eq!(form["subject_token"], PAIRING);
        assert_eq!(
            form["grant_type"],
            "urn:ietf:params:oauth:grant-type:token-exchange"
        );
        assert_eq!(
            form["subject_token_type"],
            "urn:t3:params:oauth:token-type:environment-bootstrap"
        );
        assert_eq!(form["scope"], "orchestration:read orchestration:operate");
        Response::json(
            200,
            json!({"access_token":BEARER,"token_type":"Bearer","scope":"orchestration:read","expires_in":3600}),
        )
    });
    let env = Environment::new("fixture", &server.origin).unwrap();
    let token = NativeToken::pair(&env, PAIRING, Duration::from_secs(2)).unwrap();
    let saved = token.to_secret();
    assert!(!String::from_utf8_lossy(&saved).contains(PAIRING));
    assert!(!format!("{token:?}").contains(BEARER));
    assert_ne!(NativeToken::credential_key(&env), env.credential_url());
    let restored = NativeToken::from_secret(&env, &saved).unwrap();
    assert_eq!(restored.to_secret(), saved);
    let other = Environment::new("other", "https://other.invalid").unwrap();
    assert!(matches!(
        NativeToken::from_secret(&other, &saved),
        Err(StreamError::WrongOrigin)
    ));
    assert!(NativeToken::from_secret(
        &env,
        &crate::StoredToken {
            access_token: BEARER.into(),
            expires_at: crate::unix_now() + 3600,
            scope: "orchestration:read".into(),
            client_id: "mcp".into()
        }
        .to_secret()
    )
    .is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn refused_ticket_is_explicit_pairing_required_without_oauth_or_secret_echo() {
    let server = FakeServer::start(|request| {
        assert_eq!(request.path(), "/api/auth/websocket-ticket");
        Response::json(401, json!({"error":BEARER,"description":TICKET}))
    });
    let env = Environment::new("fixture", &server.origin).unwrap();
    let error = client(&env).subscribe("thread-a", None).err().unwrap();
    assert_eq!(error, StreamError::PairingRequired);
    for secret in [BEARER, TICKET, PAIRING] {
        assert!(!format!("{error:?}: {error}").contains(secret));
    }
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn subscription_streams_acknowledged_chunks_and_resumes_or_replaces_a_snapshot() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut ws = socket(&listener);
        request(&mut ws, None);
        chunk(&mut ws, vec![snapshot(10), json!({"kind":"synchronized"})]);
        ack(&mut ws);
        assert_eq!(read(&mut ws), json!({"_tag":"Ping"}));
        send(&mut ws, json!({"_tag":"Pong"}));
        // A native WebSocket ping must also get its own pong.
        ws.send(Message::Ping(vec![1, 2, 3].into())).unwrap();
        chunk(
            &mut ws,
            vec![event(10, "duplicate"), event(12, "future.event")],
        );
        assert_eq!(ws.read().unwrap(), Message::Pong(vec![1, 2, 3].into()));
        ack(&mut ws);
        drop(ws); // Simulate a broken connection after the accepted event.

        ticket(&listener);
        let mut ws = socket(&listener);
        request(&mut ws, Some(12));
        // T3's bounded replay may replace with a new snapshot, including after
        // a database reset; do not require globally contiguous sequence ids.
        chunk(&mut ws, vec![snapshot(4), event(7, "turn-item.updated")]);
        ack(&mut ws);
        assert_eq!(
            read(&mut ws),
            json!({"_tag":"Interrupt","requestId":"1","interruptors":[]})
        );
    });
    let client = client(&fixture.env);
    let mut stream = client.subscribe("thread-a", None).unwrap();
    let mut batches = Vec::new();
    stream
        .receive(|updates| {
            batches.push(updates.to_vec());
            Ok(())
        })
        .unwrap();
    let ThreadUpdate::Snapshot {
        history_cursor,
        has_more_history,
        ..
    } = &batches[0][0]
    else {
        panic!("missing snapshot")
    };
    assert_eq!(history_cursor.as_deref(), Some("opaque+/cursor"));
    assert!(*has_more_history);
    stream
        .receive(|updates| {
            batches.push(updates.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(batches[1].len(), 1);
    assert!(
        matches!(&batches[1][0], ThreadUpdate::Event { sequence:12, event } if event["type"]=="future.event")
    );
    let cursor = stream.cursor().unwrap();
    assert_eq!(cursor.sequence(), 12);
    drop(stream);
    let mut resumed = client.subscribe("thread-a", Some(cursor)).unwrap();
    resumed
        .receive(|updates| {
            batches.push(updates.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(resumed.cursor().unwrap().sequence(), 7);
    assert_eq!(
        batches.len(),
        3,
        "positive control: every expected batch reached the consumer"
    );
    resumed.close();
    fixture.finish();
}

#[test]
fn rejected_batch_never_acknowledges_or_advances_the_resume_cursor() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut ws = socket(&listener);
        request(&mut ws, None);
        chunk(&mut ws, vec![snapshot(10)]);
        // No ACK is sent, and receive rejects further use of this socket.
        assert!(ws.read().is_err());
    });
    let mut stream = client(&fixture.env).subscribe("thread-a", None).unwrap();
    let mut invoked = false;
    assert_eq!(
        stream.receive(|_| {
            invoked = true;
            Err(StreamError::Protocol("projection rejected"))
        }),
        Err(StreamError::Protocol("projection rejected"))
    );
    assert!(
        invoked,
        "positive control: callback actually rejected a valid batch"
    );
    assert!(stream.cursor().is_none());
    assert_eq!(
        stream.receive(|_| panic!("closed stream reused")),
        Err(StreamError::Closed)
    );
    fixture.finish();
}

#[test]
fn malformed_chunk_is_validated_before_any_projection_change() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut ws = socket(&listener);
        request(&mut ws, None);
        chunk(&mut ws, vec![snapshot(10)]);
        ack(&mut ws);
        assert_eq!(read(&mut ws), json!({"_tag":"Ping"}));
        chunk(
            &mut ws,
            vec![
                event(11, "future.event"),
                json!({"kind":"event","sequence":-1,"event":{"type":"bad"}}),
            ],
        );
        assert!(ws.read().is_err());
    });
    let mut stream = client(&fixture.env).subscribe("thread-a", None).unwrap();
    stream.receive(|_| Ok(())).unwrap();
    assert!(matches!(
        stream.receive(|_| panic!("partly decoded batch was exposed")),
        Err(StreamError::Protocol(_))
    ));
    assert_eq!(stream.cursor().unwrap().sequence(), 10);
    fixture.finish();
}

#[test]
fn dropping_a_subscription_does_not_write_or_wait_for_the_peer() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut ws = socket(&listener);
        request(&mut ws, None);
        chunk(&mut ws, vec![snapshot(10)]);
        ack(&mut ws);
        // The successful receive/ACK is a positive control for a live socket.
        // Explicit graceful close is checked separately by the streaming test.
        assert!(ws.read().is_err(), "implicit drop wrote a protocol frame");
    });
    let mut stream = client(&fixture.env).subscribe("thread-a", None).unwrap();
    stream.receive(|_| Ok(())).unwrap();
    let started = Instant::now();
    drop(stream);
    assert!(started.elapsed() < Duration::from_secs(1));
    fixture.finish();
}

#[test]
fn cancellation_wakes_a_blocked_receive_without_interrupting_the_agent() {
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let fixture = Fixture::start(move |listener| {
        ticket(&listener);
        let mut ws = socket(&listener);
        request(&mut ws, None);
        ready_tx.send(()).unwrap();
        // Cancellation closes the stream; it never sends a run.interrupt RPC.
        assert!(ws.read().is_err());
    });
    let mut stream = client(&fixture.env).subscribe("thread-a", None).unwrap();
    let cancel = stream.cancellation().unwrap();
    let receiver = thread::spawn(move || stream.receive(|_| panic!("cancel test has no chunk")));
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    cancel.cancel();
    assert_eq!(receiver.join().unwrap(), Err(StreamError::Closed));
    assert!(started.elapsed() < Duration::from_secs(1));
    fixture.finish();
}

#[test]
fn incompatible_upgrade_and_cross_thread_resume_are_explicit() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut stream = accept(&listener);
        let mut request = [0; 1024];
        assert!(stream.read(&mut request).unwrap() > 0);
        stream
            .write_all(
                b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
    });
    let client = client(&fixture.env);
    assert!(matches!(
        client.subscribe("thread-a", None),
        Err(StreamError::IncompatibleProtocol)
    ));
    let cursor = ResumeCursor {
        origin: fixture.env.origin(),
        thread_id: "thread-b".into(),
        sequence: 10,
    };
    assert!(matches!(
        client.subscribe("thread-a", Some(cursor)),
        Err(StreamError::Protocol(_))
    ));
    fixture.finish();
}

#[test]
fn auth_redirects_and_unexpected_grants_are_not_followed_or_accepted() {
    let calls = Arc::new(Mutex::new(0));
    let observed = calls.clone();
    let server = FakeServer::start(move |_| {
        let mut calls = observed.lock().unwrap();
        *calls += 1;
        if *calls == 1 {
            Response::empty(302).with_header("location", "http://127.0.0.1:1/leak")
        } else {
            Response::json(
                200,
                json!({"access_token":BEARER,"token_type":"Bearer","scope":"orchestration:read terminal:operate","expires_in":3600}),
            )
        }
    });
    let env = Environment::new("fixture", &server.origin).unwrap();
    assert!(matches!(
        NativeToken::pair(&env, PAIRING, Duration::from_secs(2)),
        Err(StreamError::Http(302))
    ));
    assert!(matches!(
        NativeToken::pair(&env, PAIRING, Duration::from_secs(2)),
        Err(StreamError::Protocol("unexpected native session scope"))
    ));
    assert_eq!(*calls.lock().unwrap(), 2);
}
