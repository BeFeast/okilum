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

#[test]
fn history_uses_protocol_header_encoded_thread_path_and_opaque_cursor() {
    let server = FakeServer::start(|request| {
        assert_eq!(
            request.path(),
            "/api/orchestration/threads/thread%2Fa/history"
        );
        assert_eq!(request.header("x-t3-orchestration-protocol"), Some("2"));
        let u = url::Url::parse(&format!("http://fixture{}", request.target)).unwrap();
        assert_eq!(
            u.query_pairs().find(|(key, _)| key == "cursor").unwrap().1,
            "opaque+/??"
        );
        Response::json(
            200,
            json!({"snapshotSequence":12,"items":[],"nextCursor":null,"hasMoreHistory":false,"future":true}),
        )
    });
    let env = Environment::new("fixture", &server.origin).unwrap();
    let page = client(&env).history("thread/a", "opaque+/??").unwrap();
    assert!(page.items.is_empty());
    assert!(!page.has_more_history);
}

#[test]
fn tier_one_commands_use_exact_effect_rpc_shapes_without_interrupting_a_subscription() {
    use crate::actions::{Command, DispatchMode, Intent};
    let intents = vec![
        Intent::message("start".into(), DispatchMode::StartImmediately),
        Intent::message("queue".into(), DispatchMode::QueueAfterActive),
        Intent::message(
            "steer".into(),
            DispatchMode::SteerActive {
                target_run_id: "run".into(),
            },
        ),
        Intent::Interrupt {
            run_id: "run".into(),
        },
        Intent::Respond {
            attachments_by_question_id: Default::default(),
            request_id: "approval".into(),
            decision: Some("decline".into()),
            answers: None,
        },
        Intent::Respond {
            attachments_by_question_id: Default::default(),
            request_id: "question".into(),
            decision: None,
            answers: Some(std::collections::BTreeMap::from([(
                "q".into(),
                json!(["A", "B"]),
            )])),
        },
        Intent::Dismiss {
            request_id: "question".into(),
        },
    ];
    let commands: Vec<_> = intents
        .into_iter()
        .map(|i| Command::new("thread-a", i))
        .collect();
    let expected = commands.clone();
    let fixture = Fixture::start(move |listener| {
        for (index, command) in expected.iter().enumerate() {
            ticket(&listener);
            let mut ws = socket(&listener);
            let request = read(&mut ws);
            assert_eq!(
                request,
                json!({"_tag":"Request","id":"1","tag":"orchestration.dispatchCommand","headers":[],"payload":command.wire().unwrap()})
            );
            assert!(request["payload"].get("runtimeMode").is_none());
            let payload = &request["payload"];
            match index {
                0..=2 => {
                    assert_eq!(payload["type"], "message.dispatch");
                    assert_eq!(payload["attachments"], json!([]));
                    assert_eq!(payload["createdBy"], "user");
                    assert_eq!(payload["creationSource"], "web");
                    assert_eq!(
                        payload["dispatchMode"]["type"],
                        ["start_immediately", "queue_after_active", "steer_active"][index]
                    );
                    if index == 2 {
                        assert_eq!(payload["dispatchMode"]["targetRunId"], "run");
                    }
                }
                3 => {
                    assert_eq!(payload["type"], "run.interrupt");
                    assert_eq!(payload["runId"], "run");
                    assert_eq!(payload["holdQueue"], true);
                }
                4 => {
                    assert_eq!(payload["type"], "runtime-request.respond");
                    assert_eq!(payload["decision"], "decline");
                    assert_eq!(payload["requestId"], "approval");
                }
                5 => {
                    assert_eq!(payload["type"], "runtime-request.respond");
                    assert_eq!(payload["answers"], json!({"q":["A","B"]}));
                }
                6 => {
                    assert_eq!(payload["type"], "thread.user-input.dismiss");
                    assert_eq!(payload["requestId"], "question");
                }
                _ => unreachable!(),
            }
            send(&mut ws, json!({"_tag":"Ping"}));
            assert_eq!(read(&mut ws), json!({"_tag":"Pong"}));
            send(
                &mut ws,
                json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"sequence":index+1}}}),
            );
        }
    });
    let mut secret = serde_json::from_slice::<Value>(&token(&fixture.env).to_secret()).unwrap();
    secret["scope"] = json!("orchestration:read orchestration:operate");
    let token =
        NativeToken::from_secret(&fixture.env, &serde_json::to_vec(&secret).unwrap()).unwrap();
    let client = NativeClient::new(fixture.env.clone(), token, Duration::from_secs(2)).unwrap();
    for (i, command) in commands.iter().enumerate() {
        assert_eq!(client.dispatch(command).unwrap(), i as u64 + 1);
    }
    fixture.finish();
}
#[test]
fn native_read_only_credentials_never_obtain_a_ticket_for_commands() {
    let fixture = Fixture::start(|listener| {
        thread::sleep(Duration::from_millis(100));
        assert!(listener.accept().is_err());
    });
    let command = crate::actions::Command::new(
        "thread-a",
        crate::actions::Intent::Interrupt {
            run_id: "run".into(),
        },
    );
    assert!(client(&fixture.env).dispatch(&command).is_err());
    fixture.finish();
}

#[test]
fn lost_command_response_retains_the_same_wire_and_reopened_journal_never_resends_an_ack() {
    use crate::actions::{DispatchMode, Intent, Journal};
    let dir = std::env::temp_dir().join(format!("okilum-native-wire-{}", uuid::Uuid::new_v4()));
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut first = socket(&listener);
        let original = read(&mut first);
        assert_eq!(original["tag"], "orchestration.dispatchCommand");
        first.close(None).unwrap();
        drop(first);
        ticket(&listener);
        let mut second = socket(&listener);
        assert_eq!(
            read(&mut second),
            original,
            "explicit retry changed the command or message identity"
        );
        send(
            &mut second,
            json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"sequence":91}}}),
        );
    });
    let mut secret = serde_json::from_slice::<Value>(&token(&fixture.env).to_secret()).unwrap();
    secret["scope"] = json!("orchestration:read orchestration:operate");
    let client = NativeClient::new(
        fixture.env.clone(),
        NativeToken::from_secret(&fixture.env, &serde_json::to_vec(&secret).unwrap()).unwrap(),
        Duration::from_secs(2),
    )
    .unwrap();
    let journal = Journal::new(&dir, &fixture.env, "thread-a");
    let record = journal
        .prepare(Intent::message(
            "hello".into(),
            DispatchMode::QueueAfterActive,
        ))
        .unwrap();
    assert!(journal.send(&record, |c| client.dispatch(c)).is_err());
    let reopened = Journal::new(&dir, &fixture.env, "thread-a");
    let retained = reopened.records().unwrap().pop().unwrap();
    assert_eq!(retained, record);
    assert_eq!(
        reopened
            .send(&retained, |c| client.dispatch(c))
            .unwrap()
            .sequence,
        Some(91)
    );
    assert_eq!(
        reopened
            .send(&retained, |_| panic!("ack was sent again"))
            .unwrap()
            .sequence,
        Some(91)
    );
    fixture.finish();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn disabled_native_client_cannot_fetch_history_or_request_a_socket_ticket() {
    let server = FakeServer::start(|_| Response::empty(503));
    let env = Environment::new("fixture", &server.origin).unwrap();
    let access = crate::NetworkAccess::new(true);
    let native = client(&env).with_access(access.clone());
    assert!(native.history("thread", "cursor").is_err());
    assert!(
        !server.requests().is_empty(),
        "positive control: the enabled history request reached the server"
    );
    let before = server.requests().len();
    access.revoke();
    assert!(matches!(
        native.history("thread", "cursor"),
        Err(StreamError::Closed)
    ));
    assert!(matches!(
        native.subscribe("thread", None),
        Err(StreamError::Closed)
    ));
    assert!(matches!(native.subscribe_shell(), Err(StreamError::Closed)));
    assert!(matches!(
        native.project_threads("bound"),
        Err(StreamError::Closed)
    ));
    assert!(matches!(native.server_identity(), Err(StreamError::Closed)));
    assert!(matches!(
        native.environment_profile(),
        Err(StreamError::Closed)
    ));
    assert!(matches!(
        native.manager_models(&crate::models::Target {
            thread_id: "thread".into(),
            project_id: "bound".into()
        }),
        Err(StreamError::Closed)
    ));
    assert_eq!(server.requests().len(), before);
}

#[test]
fn native_desktop_identity_comes_from_server_config_and_never_from_the_alias() {
    for environment in [
        json!({"environmentId":"actual-server","label":"Remote"}),
        json!({"label":"Missing ID"}),
    ] {
        let valid = environment.get("environmentId").is_some();
        let fixture = Fixture::start(move |listener| {
            ticket(&listener);
            let mut socket = socket(&listener);
            let request = read(&mut socket);
            assert_eq!(request["tag"], "server.getConfig");
            assert_eq!(request["payload"], json!({}));
            send(
                &mut socket,
                json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"environment":environment}}}),
            );
        });
        let result = client(&fixture.env).server_identity();
        if valid {
            assert_eq!(result.unwrap().environment_id, "actual-server");
        } else {
            assert!(matches!(result, Err(StreamError::Protocol(_))));
        }
        fixture.finish();
    }
}

fn model_thread(listener: &TcpListener, thread: Value) {
    ticket(listener);
    let mut socket = socket(listener);
    let request = read(&mut socket);
    assert_eq!(
        request["tag"], "orchestration.subscribeThread",
        "model reads must never request the global shell directory"
    );
    assert_eq!(
        request["payload"],
        json!({"threadId":"thread-a","requestCompletionMarker":true,"acceptBoundedSnapshot":true})
    );
    send(&mut socket, json!({"_tag":"Ping"}));
    assert_eq!(read(&mut socket), json!({"_tag":"Pong"}));
    chunk(
        &mut socket,
        vec![json!({"kind":"snapshot","snapshotSequence":8,"projection":{"thread":thread}})],
    );
    ack(&mut socket);
}

fn attachment_rpc(listener: &TcpListener, tag: &str, payload: Value, result: Value) {
    ticket(listener);
    let mut socket = socket(listener);
    let request = read(&mut socket);
    assert_eq!(request["tag"], tag);
    assert_eq!(request["payload"], payload);
    send(
        &mut socket,
        json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":result}}),
    );
}

fn attachment_client(environment: &Environment) -> NativeClient {
    let mut secret = serde_json::from_slice::<Value>(&token(environment).to_secret()).unwrap();
    secret["scope"] = json!("orchestration:read orchestration:operate");
    NativeClient::new(
        environment.clone(),
        NativeToken::from_secret(environment, &serde_json::to_vec(&secret).unwrap()).unwrap(),
        Duration::from_secs(2),
    )
    .unwrap()
}

fn attachment_metadata(kind: AttachmentKind, size_bytes: u64) -> AttachmentMetadata {
    AttachmentMetadata {
        kind,
        name: "capture.png".into(),
        mime_type: if kind == AttachmentKind::Image {
            "image/png"
        } else {
            "application/octet-stream"
        }
        .into(),
        size_bytes,
    }
}

#[test]
fn attachment_upload_download_carry_real_bytes_without_forwarding_the_bearer() {
    let bytes = vec![0_u8, 255, 1, 2, 0, 3];
    let expected = bytes.clone();
    let metadata = attachment_metadata(AttachmentKind::Image, bytes.len() as u64);
    let requested = serde_json::to_value(&metadata).unwrap();
    let fixture = Fixture::start(move |listener| {
        attachment_rpc(
            &listener,
            "attachments.createUploadUrl",
            requested,
            json!({"attachmentId":"qa-image","relativeUrl":"/api/upload?signature=fixture-signed","expiresAt":9999999999999_u64}),
        );
        let mut upload = accept(&listener);
        let mut reader = BufReader::new(&mut upload);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        assert!(headers.starts_with("POST /api/upload?signature=fixture-signed HTTP/1.1\r\n"));
        assert!(
            !headers.to_ascii_lowercase().contains("authorization:"),
            "signed HTTP must never receive the native bearer"
        );
        assert!(headers
            .to_ascii_lowercase()
            .contains("content-type: image/png"));
        let mut actual = vec![0; expected.len()];
        reader.read_exact(&mut actual).unwrap();
        assert_eq!(
            actual, expected,
            "positive control: actual binary bytes crossed HTTP"
        );
        drop(reader);
        write!(
            upload,
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        drop(upload);
        attachment_rpc(
            &listener,
            "assets.createUrl",
            json!({"resource":{"_tag":"attachment","attachmentId":"qa-image","fileName":"capture.png","mimeType":"image/png"}}),
            json!({"relativeUrl":"/api/asset?signature=fixture-download","expiresAt":9999999999999_u64}),
        );
        let mut download = accept(&listener);
        let mut reader = BufReader::new(&mut download);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        assert!(headers.starts_with("GET /api/asset?signature=fixture-download HTTP/1.1\r\n"));
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        drop(reader);
        write!(download,"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",expected.len()).unwrap();
        download.write_all(&expected).unwrap();
        drop(download);
        attachment_rpc(
            &listener,
            "attachments.delete",
            json!({"attachmentId":"qa-image"}),
            json!({}),
        );
    });
    let native = attachment_client(&fixture.env);
    let uploaded = native.upload_attachment(&metadata, bytes.clone()).unwrap();
    assert_eq!(uploaded.id, "qa-image");
    assert_eq!(uploaded.metadata, metadata);
    let retained = serde_json::to_string(&uploaded).unwrap();
    assert!(!retained.contains("signature") && !retained.contains(BEARER));
    assert_eq!(native.download_attachment(&uploaded).unwrap(), bytes);
    native.delete_attachment(&uploaded.id).unwrap();
    fixture.finish();
}

#[test]
fn attachment_transfer_rejects_foreign_or_expired_urls_without_exposing_them() {
    for relative in [
        "https://foreign.invalid/secret",
        "//foreign.invalid/secret",
        "/api/upload#secret",
    ] {
        let metadata = attachment_metadata(AttachmentKind::File, 1);
        let payload = serde_json::to_value(&metadata).unwrap();
        let fixture = Fixture::start(move |listener| {
            attachment_rpc(
                &listener,
                "attachments.createUploadUrl",
                payload,
                json!({"attachmentId":"qa-file","relativeUrl":relative,"expiresAt":9999999999999_u64}),
            );
        });
        let error = attachment_client(&fixture.env)
            .upload_attachment(&metadata, vec![42])
            .unwrap_err();
        assert!(
            !error.to_string().contains("secret") && !error.to_string().contains("foreign.invalid")
        );
        fixture.finish();
    }
    let metadata = attachment_metadata(AttachmentKind::File, 1);
    let payload = serde_json::to_value(&metadata).unwrap();
    let fixture = Fixture::start(move |listener| {
        attachment_rpc(
            &listener,
            "attachments.createUploadUrl",
            payload,
            json!({"attachmentId":"qa-file","relativeUrl":"/api/upload?signature=secret","expiresAt":1}),
        )
    });
    assert_eq!(
        attachment_client(&fixture.env).upload_attachment(&metadata, vec![42]),
        Err(StreamError::Protocol("attachment transfer URL expired"))
    );
    fixture.finish();
}

#[test]
fn attachment_upload_error_or_redirect_never_becomes_a_confirmed_reference() {
    for status in [0, 500, 307] {
        let metadata = attachment_metadata(AttachmentKind::File, 1);
        let payload = serde_json::to_value(&metadata).unwrap();
        let fixture = Fixture::start(move |listener| {
            attachment_rpc(
                &listener,
                "attachments.createUploadUrl",
                payload,
                json!({"attachmentId":"qa-file","relativeUrl":"/api/upload?signature=fixture","expiresAt":9999999999999_u64}),
            );
            let mut stream = accept(&listener);
            let mut reader = BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            let mut bytes = [0; 1];
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(bytes, [42]);
            drop(reader);
            if status != 0 {
                write!(stream,"HTTP/1.1 {status} Error\r\nLocation: https://foreign.invalid/never-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            drop(stream);
            attachment_rpc(
                &listener,
                "attachments.delete",
                json!({"attachmentId":"qa-file"}),
                json!({}),
            );
        });
        assert_eq!(
            attachment_client(&fixture.env).upload_attachment(&metadata, vec![42]),
            Err(if status == 0 {
                StreamError::Unreachable
            } else {
                StreamError::Http(status)
            })
        );
        fixture.finish();
    }
}

#[test]
fn attachment_download_rejects_truncated_bytes() {
    let fixture = Fixture::start(|listener| {
        attachment_rpc(
            &listener,
            "assets.createUrl",
            json!({"resource":{"_tag":"attachment","attachmentId":"qa-file","fileName":"capture.png","mimeType":"application/octet-stream"}}),
            json!({"relativeUrl":"/api/asset","expiresAt":9999999999999_u64}),
        );
        let mut stream = accept(&listener);
        let mut reader = BufReader::new(&mut stream);
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
        }
        drop(reader);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });
    let attachment = ChatAttachment {
        id: "qa-file".into(),
        metadata: attachment_metadata(AttachmentKind::File, 1),
        source: None,
    };
    assert_eq!(
        client(&fixture.env).download_attachment(&attachment),
        Err(StreamError::Protocol(
            "attachment download size does not match"
        ))
    );
    fixture.finish();
}

#[test]
fn attachment_validation_preserves_the_contract_limits_and_rejects_changed_bytes_before_rpc() {
    assert!(attachment_metadata(AttachmentKind::Image, MAX_IMAGE_BYTES)
        .validate()
        .is_ok());
    assert!(
        attachment_metadata(AttachmentKind::Image, MAX_IMAGE_BYTES + 1)
            .validate()
            .is_err()
    );
    assert!(attachment_metadata(AttachmentKind::File, MAX_FILE_BYTES)
        .validate()
        .is_ok());
    assert!(
        attachment_metadata(AttachmentKind::File, MAX_FILE_BYTES + 1)
            .validate()
            .is_err()
    );
    let image = ChatAttachment {
        id: "qa-image".into(),
        metadata: attachment_metadata(AttachmentKind::Image, MAX_IMAGE_BYTES),
        source: None,
    };
    assert!(validate_attachments(&vec![image.clone(); 8]).is_ok());
    assert!(validate_attachments(&vec![image; 9]).is_err());
    let server = FakeServer::start(|_| Response::empty(503));
    let environment = Environment::new("fixture", &server.origin).unwrap();
    let native = attachment_client(&environment);
    let metadata = attachment_metadata(AttachmentKind::File, 1);
    assert!(native.upload_attachment(&metadata, vec![1]).is_err());
    assert!(
        !server.requests().is_empty(),
        "positive control: valid upload reaches the ticket endpoint"
    );
    let before = server.requests().len();
    assert_eq!(
        native.upload_attachment(&metadata, vec![1, 2]),
        Err(StreamError::Protocol(
            "attachment bytes changed before upload"
        ))
    );
    assert_eq!(
        server.requests().len(),
        before,
        "a changed local draft must never request an upload URL"
    );
}

fn model_catalog(listener: &TcpListener) {
    ticket(listener);
    let mut socket = socket(listener);
    assert_eq!(read(&mut socket)["tag"], "server.getConfig");
    send(
        &mut socket,
        json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"providers":[
            {"instanceId":"claudeAgent","driver":"claudeAgent","displayName":"Claude","enabled":true,"models":(0..42).map(|n|json!({"slug":format!("model-{n}"),"name":format!("Model {n}"),"shortName":format!("M{n}"),"subProvider":"Native vendor","capabilities":{"optionDescriptors":[
                {"type":"select","id":"effort","label":"Reasoning","options":[{"id":"high","label":"High","isDefault":true},{"id":"ultracode","label":"Ultracode"},{"id":"prompt-only","label":"Prompt mode"}],"promptInjectedValues":["prompt-only"]},
                {"type":"boolean","id":"fastMode","label":"Fast","currentValue":false}
            ]}})).collect::<Vec<_>>()},
            {"instanceId":"disabled","driver":"codex","enabled":false,"models":[{"slug":"unavailable","name":"Disabled"}]}
        ]}}}),
    );
}

fn model_thread_value(project: &str, selection: Value) -> Value {
    // The production thread projection has no synthetic shell `status` field.
    json!({"id":"thread-a","projectId":project,"title":"Target thread","modelSelection":selection,"runtimeMode":"full-access","interactionMode":"default","deletedAt":null})
}

#[test]
fn manager_models_read_only_the_bounded_target_and_preserve_real_options() {
    let selection = json!({"instanceId":"claudeAgent","model":"model-17","options":[{"id":"effort","value":"ultracode"},{"id":"fastMode","value":false}]});
    for project in ["bound", "foreign"] {
        let thread = model_thread_value(project, selection.clone());
        let fixture = Fixture::start(move |listener| {
            model_thread(&listener, thread);
            if project == "bound" {
                model_catalog(&listener);
            }
        });
        let result = client(&fixture.env).manager_models(&crate::models::Target {
            thread_id: "thread-a".into(),
            project_id: "bound".into(),
        });
        if project == "bound" {
            let snapshot = result.unwrap();
            assert_eq!(snapshot.catalog.providers.len(), 1);
            assert_eq!(snapshot.catalog.providers[0].models.len(), 42);
            let model = snapshot
                .model_info(&snapshot.configuration.model_selection)
                .unwrap();
            assert_eq!(model.display_name(), "M17");
            assert_eq!(model.sub_provider.as_deref(), Some("Native vendor"));
            let controls = snapshot.option_controls(&snapshot.configuration.model_selection);
            assert_eq!(controls.len(), 2);
            assert!(controls[0].choices[2].prompt_injected);
            assert_eq!(
                serde_json::to_value(&snapshot.configuration.model_selection).unwrap(),
                selection
            );
            assert_eq!(
                snapshot.selection("claudeAgent", "model-17"),
                snapshot.configuration.model_selection
            );
        } else {
            assert!(matches!(result, Err(StreamError::Protocol(_))));
        }
        fixture.finish();
    }
}

#[test]
fn manager_model_change_requires_a_fresh_target_readback() {
    let selection = json!({"instanceId":"claudeAgent","model":"model-17","options":[{"id":"effort","value":"high"}]});
    for confirmed in [true, false] {
        let requested = selection.clone();
        let fixture = Fixture::start(move |listener| {
            let before = json!({"instanceId":"claudeAgent","model":"model-0","options":[]});
            model_thread(&listener, model_thread_value("bound", before.clone()));
            model_catalog(&listener);
            ticket(&listener);
            let mut command = socket(&listener);
            let request = read(&mut command);
            assert_eq!(request["tag"], "orchestration.dispatchCommand");
            assert_eq!(request["payload"]["type"], "thread.model-selection.set");
            assert_eq!(request["payload"]["threadId"], "thread-a");
            assert_eq!(request["payload"]["modelSelection"], requested);
            send(
                &mut command,
                json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{}}}),
            );
            drop(command);
            model_thread(
                &listener,
                model_thread_value("bound", if confirmed { requested } else { before }),
            );
            model_catalog(&listener);
        });
        let mut secret = serde_json::from_slice::<Value>(&token(&fixture.env).to_secret()).unwrap();
        secret["scope"] = json!("orchestration:read orchestration:operate");
        let native = NativeClient::new(
            fixture.env.clone(),
            NativeToken::from_secret(&fixture.env, &serde_json::to_vec(&secret).unwrap()).unwrap(),
            Duration::from_secs(2),
        )
        .unwrap();
        let result = native.configure_manager_model(
            &crate::models::Target {
                thread_id: "thread-a".into(),
                project_id: "bound".into(),
            },
            &serde_json::from_value(selection.clone()).unwrap(),
        );
        if confirmed {
            let snapshot = result.unwrap();
            assert_eq!(
                snapshot
                    .model_info(&snapshot.configuration.model_selection)
                    .unwrap()
                    .display_name(),
                "M17"
            );
            assert_eq!(
                snapshot
                    .option_controls(&snapshot.configuration.model_selection)
                    .len(),
                2,
                "confirmed mutation snapshots retain typed descriptors too"
            );
            assert_eq!(
                serde_json::to_value(snapshot.configuration.model_selection).unwrap(),
                selection
            );
        } else {
            assert!(matches!(
                result,
                Err(StreamError::Protocol("model change not confirmed"))
            ));
        }
        fixture.finish();
    }
}

#[test]
fn shell_directory_uses_real_ws_rpc_and_acknowledges_only_applied_chunks() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut socket = socket(&listener);
        let request = read(&mut socket);
        assert_eq!(request["tag"], "orchestration.subscribeShell");
        assert_eq!(request["payload"], json!({"requestCompletionMarker":true}));
        assert_eq!(read(&mut socket), json!({"_tag":"Ping"}));
        send(&mut socket, json!({"_tag":"Pong"}));
        chunk(
            &mut socket,
            vec![
                json!({"kind":"snapshot","snapshot":{"snapshotSequence":3,"threads":[{"id":"manager","projectId":"bound","title":"Manager","status":"idle"}]}}),
            ],
        );
        ack(&mut socket);
        assert_eq!(read(&mut socket), json!({"_tag":"Ping"}));
        send(&mut socket, json!({"_tag":"Pong"}));
        chunk(
            &mut socket,
            vec![
                json!({"kind":"thread.updated","location":"active","sequence":4,"thread":{"id":"manager","projectId":"bound","title":"Manager","status":"running","activeRunId":"run-a"}}),
            ],
        );
        assert!(
            socket.read().is_err(),
            "rejected chunk is closed without ACK"
        );
    });
    let mut stream = client(&fixture.env).subscribe_shell().unwrap();
    let mut directory = shell::Directory::new(["bound".into()]);
    stream
        .receive(|updates| {
            assert!(directory.apply(updates));
            Ok(())
        })
        .unwrap();
    assert_eq!(directory.threads["manager"].status, "idle");
    assert!(stream.receive(|_| Err(StreamError::Rejected)).is_err());
    assert_eq!(directory.threads["manager"].status, "idle");
    fixture.finish();
}

#[test]
fn shell_directory_accepts_a_bounded_snapshot_larger_than_the_default_frame_limit() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut socket = socket(&listener);
        assert_eq!(read(&mut socket)["tag"], "orchestration.subscribeShell");
        assert_eq!(read(&mut socket), json!({"_tag":"Ping"}));
        send(&mut socket, json!({"_tag":"Pong"}));
        // Unrelated project metadata dominates the real directory. It must be
        // admitted by transport, then excluded by the scoped retained model.
        chunk(
            &mut socket,
            vec![json!({"kind":"snapshot","snapshot":{
                "snapshotSequence":3,"projects":[{"description":"x".repeat(17 * 1024 * 1024)}],
                "threads":[{"id":"manager","projectId":"bound","title":"Manager","status":"idle"},
                    {"id":"foreign","projectId":"other","title":"Foreign","status":"idle"}]
            }})],
        );
        ack(&mut socket);
    });
    // Generating/serializing this intentionally large fixture in an unoptimized
    // test build can exceed the small fixtures' two-second heartbeat interval.
    let mut stream = NativeClient::new(
        fixture.env.clone(),
        token(&fixture.env),
        Duration::from_secs(10),
    )
    .unwrap()
    .subscribe_shell()
    .unwrap();
    let mut directory = shell::Directory::new(["bound".into()]);
    stream
        .receive(|updates| {
            directory.apply(updates);
            Ok(())
        })
        .unwrap();
    assert_eq!(directory.threads.len(), 1);
    assert!(directory.threads.contains_key("manager"));
    drop(stream);
    fixture.finish();
}

#[test]
fn modern_snapshot_without_budget_flag_keeps_opaque_history_metadata() {
    let mut wire = snapshot(10);
    assert!(wire.get("payloadBudgetExceeded").is_none());
    wire["latestLocalTurnOrdinal"] = json!(42);
    let chunk = json!({"values":[wire]});
    let (updates, cursor) = protocol::decode_chunk(&chunk, "thread-a", None).unwrap();
    assert_eq!(cursor, Some(10));
    assert!(matches!(
        &updates[..],
        [ThreadUpdate::Snapshot {
            has_more_history: true,
            history_cursor: Some(history),
            latest_local_turn_ordinal: Some(42),
            ..
        }] if history == "opaque+/cursor"
    ));

    // Removing an obsolete field must not relax the current history contract.
    for (field, invalid) in [
        ("hasMoreHistory", json!("false")),
        ("historyCursor", json!(17)),
    ] {
        let mut malformed = chunk.clone();
        malformed["values"][0][field] = invalid;
        assert!(protocol::decode_chunk(&malformed, "thread-a", None).is_err());
    }
}

#[test]
fn native_environment_profile_reads_explicit_capabilities_without_changing_identity_semantics() {
    let fixture = Fixture::start(|listener| {
        attachment_rpc(
            &listener,
            "server.getConfig",
            json!({}),
            json!({"environment":{
                "environmentId":"actual-server", "label":" Remote label ",
                "capabilities":{"questionAttachments":true,"attachmentUploads":true,
                    "fileAttachments":{"maxUploadBytes":MAX_FILE_BYTES}}
            }}),
        );
    });
    let profile = client(&fixture.env).environment_profile().unwrap();
    assert_eq!(profile.identity.environment_id, "actual-server");
    assert_eq!(profile.identity.label, " Remote label ");
    assert!(profile.capabilities.question_attachments_available());
    assert_eq!(
        profile.capabilities.effective_file_max_upload_bytes(),
        Some(MAX_FILE_BYTES)
    );
    fixture.finish();
}

#[test]
fn question_dispatch_without_explicit_true_capabilities_never_requests_a_ticket_or_config() {
    use crate::actions::{Command, Intent, QuestionAttachments};
    use std::collections::BTreeMap;
    let server = FakeServer::start(|_| Response::empty(503));
    let environment = Environment::new("fixture", &server.origin).unwrap();
    let access = crate::NetworkAccess::new(true);
    let native = attachment_client(&environment).with_access(access.clone());
    let map = QuestionAttachments::from([(
        "q".into(),
        vec![ChatAttachment {
            id: "issued-image".into(),
            metadata: attachment_metadata(AttachmentKind::Image, 19),
            source: None,
        }],
    )]);
    let command = Command::new(
        "thread-a",
        Intent::Respond {
            request_id: "question".into(),
            decision: None,
            answers: Some(BTreeMap::from([("q".into(), json!(""))])),
            attachments_by_question_id: map,
        },
    );
    let positive = NativeEnvironmentCapabilities {
        question_attachments: Some(true),
        attachment_uploads: Some(true),
        file_attachment_max_upload_bytes: Some(MAX_FILE_BYTES),
    };
    assert!(matches!(
        native.dispatch(&command),
        Err(StreamError::Protocol(_))
    ));
    for capabilities in [
        NativeEnvironmentCapabilities::default(),
        NativeEnvironmentCapabilities {
            question_attachments: Some(false),
            ..positive.clone()
        },
        NativeEnvironmentCapabilities {
            attachment_uploads: Some(false),
            ..positive.clone()
        },
    ] {
        assert!(matches!(
            native.dispatch_with_question_attachments(&command, &capabilities),
            Err(StreamError::Protocol(_))
        ));
    }
    assert!(server.requests().is_empty());
    assert!(matches!(
        native.dispatch_with_question_attachments(&command, &positive),
        Err(StreamError::Http(503))
    ));
    assert_eq!(
        server.requests().len(),
        1,
        "positive control: explicit support reaches the same native ticket transport"
    );
    access.revoke();
    assert_eq!(
        native.dispatch_with_question_attachments(&command, &positive),
        Err(StreamError::Closed)
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn uploaded_question_bytes_and_native_response_map_reach_only_the_named_question() {
    use crate::actions::{Command, Intent, QuestionAttachments};
    use std::collections::BTreeMap;
    let bytes = vec![0_u8, 255, 1, 2, 0, 3];
    let expected = bytes.clone();
    let metadata = attachment_metadata(AttachmentKind::Image, bytes.len() as u64);
    let requested = serde_json::to_value(&metadata).unwrap();
    let fixture = Fixture::start(move |listener| {
        attachment_rpc(
            &listener,
            "attachments.createUploadUrl",
            requested,
            json!({"attachmentId":"issued-q2","relativeUrl":"/api/upload?signature=fixture-signed","expiresAt":9999999999999_u64}),
        );
        let mut upload = accept(&listener);
        let mut reader = BufReader::new(&mut upload);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        assert!(headers.starts_with("POST /api/upload?signature=fixture-signed HTTP/1.1\r\n"));
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        let mut actual = vec![0; expected.len()];
        reader.read_exact(&mut actual).unwrap();
        assert_eq!(
            actual, expected,
            "positive control: the actual question file bytes crossed HTTP"
        );
        drop(reader);
        write!(
            upload,
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        drop(upload);
        for (index, mode) in ["live", "message"].iter().enumerate() {
            ticket(&listener);
            let mut ws = socket(&listener);
            let request = read(&mut ws);
            assert_eq!(
                request["tag"], "orchestration.dispatchCommand",
                "no hidden capability RPC during dispatch"
            );
            let payload = &request["payload"];
            assert_eq!(payload["type"], "runtime-request.respond");
            assert_eq!(payload["threadId"], "thread-a");
            assert_eq!(payload["requestId"], format!("{mode}-question"));
            assert_eq!(
                payload["answers"]["q1"],
                if *mode == "message" {
                    json!("[\" exact \",\"β\"]")
                } else {
                    json!([" exact ", "β"])
                }
            );
            assert_eq!(payload["answers"]["q2"], "");
            assert_eq!(
                payload["attachmentsByQuestionId"]["q2"][0]["id"],
                "issued-q2"
            );
            assert_eq!(
                payload["attachmentsByQuestionId"]["q2"][0]["sizeBytes"],
                expected.len()
            );
            assert!(payload["attachmentsByQuestionId"].get("q1").is_none());
            assert!(payload.get("messageId").is_none());
            assert!(payload.get("attachments").is_none());
            send(
                &mut ws,
                json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"sequence":7+index}}}),
            );
        }
    });
    let native = attachment_client(&fixture.env);
    let confirmed = native.upload_attachment(&metadata, bytes).unwrap();
    assert_eq!(confirmed.id, "issued-q2");
    let capabilities = NativeEnvironmentCapabilities {
        question_attachments: Some(true),
        attachment_uploads: Some(true),
        file_attachment_max_upload_bytes: Some(MAX_FILE_BYTES),
    };
    for (index, mode) in ["live", "message"].iter().enumerate() {
        let command = Command::new(
            "thread-a",
            Intent::Respond {
                request_id: format!("{mode}-question"),
                decision: None,
                answers: Some(BTreeMap::from([
                    (
                        "q1".into(),
                        if *mode == "message" {
                            json!("[\" exact \",\"β\"]")
                        } else {
                            json!([" exact ", "β"])
                        },
                    ),
                    ("q2".into(), json!("")),
                ])),
                attachments_by_question_id: QuestionAttachments::from([(
                    "q2".into(),
                    vec![confirmed.clone()],
                )]),
            },
        );
        assert_eq!(
            native
                .dispatch_with_question_attachments(&command, &capabilities)
                .unwrap(),
            7 + index as u64
        );
    }
    fixture.finish();
}

#[test]
fn catalog_subscription_uses_existing_config_stream_and_never_reads_thread_history() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut socket = socket(&listener);
        let request = read(&mut socket);
        assert_eq!(request["tag"], "subscribeServerConfig");
        assert_eq!(request["payload"], json!({}));
        assert_eq!(read(&mut socket), json!({"_tag":"Ping"}));
        send(&mut socket, json!({"_tag":"Pong"}));
        chunk(
            &mut socket,
            vec![json!({"version":1,"type":"snapshot","config":{
                "environment":{"environmentId":"env-a","label":"A"},
                "providers":[{"instanceId":"seat-a","driver":"codex","enabled":true,
                    "models":[{"slug":"model-a","name":"A"}]}]
            }})],
        );
        ack(&mut socket);
        assert_eq!(read(&mut socket), json!({"_tag":"Ping"}));
        send(&mut socket, json!({"_tag":"Pong"}));
        chunk(
            &mut socket,
            vec![json!({"version":1,"type":"settingsUpdated","payload":{"settings":{}}})],
        );
        ack(&mut socket);
    });
    let mut stream = client(&fixture.env).subscribe_model_catalog().unwrap();
    stream
        .receive(|updates| {
            let [model_catalog::Update::Snapshot(snapshot)] = updates else {
                panic!("initial config is ready without fetching a thread snapshot");
            };
            assert_eq!(snapshot.environment.environment_id, "env-a");
            assert_eq!(snapshot.providers[0].provider_instance_id, "seat-a");
            assert_eq!(snapshot.providers[0].models[0].id, "model-a");
            Ok(())
        })
        .unwrap();
    stream
        .receive(|updates| {
            assert_eq!(updates, &[model_catalog::Update::Refresh]);
            Ok(())
        })
        .unwrap();
    fixture.finish();
}

#[test]
fn catalog_rejects_malformed_batch_before_apply_or_ack_and_consumer_can_reject() {
    for malformed in [false, true] {
        let fixture = Fixture::start(move |listener| {
            ticket(&listener);
            let mut socket = socket(&listener);
            assert_eq!(read(&mut socket)["tag"], "subscribeServerConfig");
            assert_eq!(read(&mut socket), json!({"_tag":"Ping"}));
            send(&mut socket, json!({"_tag":"Pong"}));
            let mut values = vec![json!({"version":1,"type":"snapshot","config":{
                "environment":{"environmentId":"env-a","label":"A"},"providers":[]
            }})];
            if malformed {
                values.push(json!({"version":1,"type":"settingsUpdated","payload":{}}));
            }
            chunk(&mut socket, values);
            assert!(
                socket.read().is_err(),
                "a rejected batch must close without ACK"
            );
        });
        let mut stream = client(&fixture.env).subscribe_model_catalog().unwrap();
        let mut calls = 0;
        assert!(stream
            .receive(|_| {
                calls += 1;
                Err(StreamError::Protocol("fixture refused application"))
            })
            .is_err());
        assert_eq!(calls, usize::from(!malformed));
        fixture.finish();
    }
}

#[test]
fn disabled_catalog_access_performs_no_http_and_valid_access_has_positive_control() {
    let server = FakeServer::start(|_| Response::json(200, json!({"ticket":TICKET})));
    let environment = Environment::new("fixture", &server.origin).unwrap();
    let access = crate::NetworkAccess::new(true);
    let native = client(&environment).with_access(access.clone());
    assert!(native.subscribe_model_catalog().is_err());
    assert!(
        !server.requests().is_empty(),
        "positive control: an enabled connection requests a ticket"
    );
    let before = server.requests().len();
    access.revoke();
    assert!(matches!(
        native.subscribe_model_catalog(),
        Err(StreamError::Closed)
    ));
    assert_eq!(native.model_catalog_snapshot(), Err(StreamError::Closed));
    assert_eq!(server.requests().len(), before);
}

#[test]
fn catalog_scope_is_shared_only_for_the_same_origin_and_native_session() {
    let environment = Environment::new("first alias", "http://127.0.0.1:9").unwrap();
    let shared_token = token(&environment);
    let native = NativeClient::new(
        environment.clone(),
        shared_token.clone(),
        Duration::from_secs(2),
    )
    .unwrap();
    let alias = Environment::new("another alias", "http://127.0.0.1:9").unwrap();
    let alias_client = NativeClient::new(alias, shared_token, Duration::from_secs(2)).unwrap();
    assert_eq!(native.catalog_scope(), alias_client.catalog_scope());
    let mut secret: Value = serde_json::from_slice(&token(&environment).to_secret()).unwrap();
    secret["access_token"] = "another-session-secret".into();
    let rotated = NativeClient::new(
        environment.clone(),
        NativeToken::from_secret(&environment, &serde_json::to_vec(&secret).unwrap()).unwrap(),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_ne!(native.catalog_scope(), rotated.catalog_scope());
    let other = Environment::new("first alias", "http://127.0.0.1:10").unwrap();
    assert_ne!(native.catalog_scope(), client(&other).catalog_scope());
}

#[test]
fn manager_response_timeout_is_transport_failure_without_an_implicit_retry() {
    let fixture = Fixture::start(|listener| {
        ticket(&listener);
        let mut socket = socket(&listener);
        assert_eq!(read(&mut socket)["tag"], "server.getConfig");
        thread::sleep(Duration::from_millis(250));
    });
    let native = NativeClient::new(
        fixture.env.clone(),
        token(&fixture.env),
        Duration::from_millis(80),
    )
    .unwrap();
    let started = Instant::now();
    assert_eq!(
        native.manager_rpc("server.getConfig", json!({}), false),
        Err(StreamError::Unreachable)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    fixture.finish();
}

#[test]
fn model_confirmation_accepts_reordered_options_but_rejects_different_values_or_accounts() {
    let requested = json!({"instanceId":"claudeAgent","model":"model-17","options":[
        {"id":"effort","value":"high"},{"id":"fastMode","value":true}
    ]});
    for difference in ["order", "value", "account"] {
        let selection = requested.clone();
        let fixture = Fixture::start(move |listener| {
            model_thread(
                &listener,
                model_thread_value(
                    "bound",
                    json!({"instanceId":"claudeAgent","model":"model-0","options":[]}),
                ),
            );
            model_catalog(&listener);
            ticket(&listener);
            let mut command = socket(&listener);
            let request = read(&mut command);
            assert_eq!(request["tag"], "orchestration.dispatchCommand");
            assert_eq!(request["payload"]["type"], "thread.model-selection.set");
            assert_eq!(request["payload"]["modelSelection"], selection);
            send(
                &mut command,
                json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{}}}),
            );
            drop(command);
            let mut observed = selection;
            observed["options"].as_array_mut().unwrap().reverse();
            match difference {
                "order" => {}
                "value" => observed["options"][0]["value"] = json!(false),
                "account" => observed["instanceId"] = json!("another-account"),
                _ => unreachable!(),
            }
            model_thread(&listener, model_thread_value("bound", observed));
            model_catalog(&listener);
        });
        let mut secret = serde_json::from_slice::<Value>(&token(&fixture.env).to_secret()).unwrap();
        secret["scope"] = json!("orchestration:read orchestration:operate");
        let native = NativeClient::new(
            fixture.env.clone(),
            NativeToken::from_secret(&fixture.env, &serde_json::to_vec(&secret).unwrap()).unwrap(),
            Duration::from_secs(2),
        )
        .unwrap();
        let result = native.configure_manager_model(
            &crate::models::Target {
                thread_id: "thread-a".into(),
                project_id: "bound".into(),
            },
            &serde_json::from_value(requested.clone()).unwrap(),
        );
        if difference == "order" {
            let confirmed = result.unwrap();
            assert_eq!(
                confirmed.configuration.model_selection.options[0].id, "fastMode",
                "the confirmed snapshot retains the server's canonical order"
            );
            assert_eq!(
                confirmed.configuration.model_selection.options[1].id,
                "effort"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(StreamError::Protocol("model change not confirmed"))
                ),
                "a different option value or provider instance is not confirmation"
            );
        }
        fixture.finish();
    }
}

#[test]
fn pairing_reports_build_target_os() {
    let server = FakeServer::start(|request| {
        let form: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(request.body.as_bytes()).collect();
        #[cfg(target_os = "linux")]
        assert_eq!(form["client_os"], "linux");
        #[cfg(target_os = "macos")]
        assert_eq!(form["client_os"], "macos");
        #[cfg(target_os = "windows")]
        assert_eq!(form["client_os"], "windows");
        assert_eq!(form["client_device_type"], "desktop");
        Response::json(
            200,
            json!({"access_token":BEARER,"token_type":"Bearer","scope":"orchestration:read","expires_in":3600}),
        )
    });
    let env = Environment::new("fixture", &server.origin).unwrap();
    NativeToken::pair(&env, PAIRING, Duration::from_secs(2)).unwrap();
    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "the exchange must actually run"
    );
}

#[test]
fn expired_native_metadata_is_display_only_and_still_origin_bound_2653() {
    let env = Environment::new("test", "https://t3.example").unwrap();
    let secret = serde_json::to_vec(
        &json!({"origin":env.origin(), "access_token":"synthetic-only",
        "expires_at":1, "scope":"orchestration:read orchestration:operate"}),
    )
    .unwrap();
    assert_eq!(NativeToken::saved_expiry(&env, &secret), Some(1));
    assert!(
        matches!(
            NativeToken::from_secret(&env, &secret),
            Err(StreamError::Expired)
        ),
        "displaying expiry cannot authorize an expired credential"
    );
    let foreign = Environment::new("other", "https://foreign.example").unwrap();
    assert_eq!(NativeToken::saved_expiry(&foreign, &secret), None);
    assert_eq!(NativeToken::saved_expiry(&env, b"not a credential"), None);
}
