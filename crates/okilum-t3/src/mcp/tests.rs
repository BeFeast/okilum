use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use super::*;
use crate::testing::{FakeServer, Request, Response};

const SESSION: &str = "session-1";

fn token(access_token: &str) -> StoredToken {
    StoredToken {
        access_token: access_token.into(),
        expires_at: unix_now() + 30 * 24 * 60 * 60,
        scope: "orchestration:read orchestration:operate".into(),
        client_id: "client".into(),
    }
}

fn gateway(server: &FakeServer, token: Option<StoredToken>) -> McpGateway {
    McpGateway::new(Environment::new("test", &server.origin).unwrap(), token).unwrap()
}

/// A T3 `/mcp` endpoint: accepts the bearer `good`, issues one session and
/// answers `tools/call` with `tool(name, arguments)`.
fn fake_t3(tool: impl Fn(&str, &Value) -> Value + Send + Sync + 'static) -> FakeServer {
    FakeServer::start(move |request| {
        if request.header("authorization") != Some("Bearer good") {
            return Response::empty(401)
                .with_header("www-authenticate", "Bearer resource_metadata=\"x\"");
        }
        let message = request.json();
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => Response::json(
                200,
                json!({ "jsonrpc": "2.0", "id": message["id"], "result": {
                    "protocolVersion": PROTOCOL_VERSION, "capabilities": { "tools": {} },
                    "serverInfo": { "name": "T3 Code", "version": "test" } } }),
            )
            .with_header(SESSION_HEADER, SESSION),
            "notifications/initialized" => Response::empty(202),
            "tools/call" => {
                let params = &message["params"];
                let result = tool(params["name"].as_str().unwrap(), &params["arguments"]);
                Response::json(
                    200,
                    json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }),
                )
            }
            other => panic!("unexpected method {other}"),
        }
    })
}

fn structured(value: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": value.to_string() }], "structuredContent": value })
}

fn tool_calls(requests: &[Request]) -> Vec<Value> {
    requests
        .iter()
        .map(Request::json)
        .filter(|message| message["method"] == "tools/call")
        .map(|message| message["params"].clone())
        .collect()
}

fn configuration(model: &str) -> Value {
    json!({
        "threadId": "mcp:manager",
        "modelSelection": { "instanceId": "claudeAgent", "model": model },
        "runtimeMode": "full-access",
        "interactionMode": "default"
    })
}

#[test]
fn initialises_once_and_sends_session_and_protocol_headers() {
    let server = fake_t3(|_, _| structured(json!({ "requestIds": ["r1"] })));
    let gateway = gateway(&server, Some(token("good")));
    assert_eq!(gateway.pending_requests("mcp:manager").unwrap(), vec!["r1"]);
    assert_eq!(gateway.pending_requests("mcp:manager").unwrap(), vec!["r1"]);

    let requests = server.requests();
    let methods: Vec<String> = requests
        .iter()
        .map(|r| r.json()["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/call",
            "tools/call"
        ]
    );
    assert_eq!(requests[0].header(SESSION_HEADER), None);
    assert_eq!(requests[0].header(PROTOCOL_HEADER), None);
    for later in &requests[1..] {
        assert_eq!(later.path(), "/mcp");
        assert_eq!(later.header(SESSION_HEADER), Some(SESSION));
        assert_eq!(later.header(PROTOCOL_HEADER), Some(PROTOCOL_VERSION));
        assert_eq!(
            later.header("accept"),
            Some("application/json, text/event-stream")
        );
    }
    assert_eq!(
        tool_calls(&requests)[0]["arguments"],
        json!({ "threadId": "mcp:manager" })
    );
    assert!(matches!(gateway.status(), GatewayStatus::Connected { .. }));
}

#[test]
fn no_token_or_an_expired_one_fails_without_a_request() {
    let server = fake_t3(|_, _| panic!("no tool call expected"));
    let none = gateway(&server, None);
    assert_eq!(none.capabilities().unwrap_err(), GatewayError::NotConnected);
    assert_eq!(none.status(), GatewayStatus::NotConnected);

    let mut old = token("good");
    old.expires_at = unix_now() - 1;
    let expired = gateway(&server, Some(old));
    assert_eq!(expired.capabilities().unwrap_err(), GatewayError::Expired);
    assert_eq!(expired.status(), GatewayStatus::Expired);
    assert!(server.requests().is_empty());
}

#[test]
fn a_refused_token_reads_as_revoked_until_a_new_token_arrives() {
    let server = fake_t3(|_, _| structured(json!({ "requestIds": [] })));
    let gateway = gateway(&server, Some(token("revoked-on-server")));
    assert_eq!(
        gateway.pending_requests("t").unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(gateway.status(), GatewayStatus::Revoked);
    // Only the initialize was tried: a 401 is not retried.
    assert_eq!(server.requests().len(), 1);

    gateway.set_token(Some(token("good")));
    assert!(matches!(gateway.status(), GatewayStatus::Connected { .. }));
    assert!(gateway.pending_requests("t").unwrap().is_empty());
}

#[test]
fn a_revocation_after_the_session_started_is_seen_on_the_next_call() {
    let revoked = Arc::new(AtomicUsize::new(0));
    let flag = revoked.clone();
    let inner = fake_t3(|_, _| structured(json!({ "requestIds": [] })));
    let origin = inner.origin.clone();
    // A proxy that starts refusing the token once `revoked` is set.
    let server = FakeServer::start(move |request| {
        if flag.load(Ordering::SeqCst) > 0 {
            return Response::empty(401);
        }
        forward(&origin, request)
    });
    let gateway = gateway(&server, Some(token("good")));
    assert!(gateway.pending_requests("t").is_ok());
    revoked.store(1, Ordering::SeqCst);
    assert_eq!(
        gateway.pending_requests("t").unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(gateway.status(), GatewayStatus::Revoked);
}

fn forward(origin: &str, request: &Request) -> Response {
    let client = reqwest::blocking::Client::new();
    let mut outgoing = client
        .post(format!("{origin}{}", request.target))
        .body(request.body.clone());
    for (name, value) in &request.headers {
        if !matches!(name.as_str(), "host" | "content-length") {
            outgoing = outgoing.header(name.as_str(), value.as_str());
        }
    }
    let reply = outgoing.send().unwrap();
    let status = reply.status().as_u16();
    let headers = reply
        .headers()
        .iter()
        .filter(|(name, _)| *name != "content-length" && *name != "connection")
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
        .collect();
    Response {
        status,
        headers,
        body: reply.text().unwrap(),
    }
}

#[test]
fn an_unreachable_server_is_reported_as_such() {
    let origin = {
        let server = fake_t3(|_, _| Value::Null);
        server.origin.clone()
    };
    let gateway = McpGateway::new(
        Environment::new("gone", &origin).unwrap(),
        Some(token("good")),
    )
    .unwrap();
    assert!(matches!(
        gateway.capabilities(),
        Err(GatewayError::Unreachable(_))
    ));
    assert!(matches!(
        gateway.status(),
        GatewayStatus::Unreachable { .. }
    ));
}

#[test]
fn a_tool_failure_carries_t3s_code() {
    let server = fake_t3(|_, _| {
        let failure = json!({ "_tag": "OrchestratorMcpFailure", "code": "thread_not_found",
                              "message": "Thread mcp:x does not exist." });
        json!({ "isError": true, "content": [{ "type": "text", "text": failure.to_string() }] })
    });
    let gateway = gateway(&server, Some(token("good")));
    assert_eq!(
        gateway.thread_configuration("mcp:x").unwrap_err(),
        GatewayError::Tool {
            code: "thread_not_found".into(),
            message: "Thread mcp:x does not exist.".into()
        }
    );
    // The server took the token: a refusal of the operation is not a connection problem.
    assert!(matches!(gateway.status(), GatewayStatus::Connected { .. }));
}

#[test]
fn a_retried_send_carries_the_same_request_id() {
    let server = fake_t3(|_, arguments| {
        structured(
            json!({ "threadId": arguments["threadId"], "messageId": "m1", "runId": "r1",
                           "status": "queued", "delivery": "queued" }),
        )
    });
    let gateway = gateway(&server, Some(token("good")));
    let message = OutgoingMessage::queue("mcp:manager", "What is next?");
    let first = gateway.send_message(&message).unwrap();
    let second = gateway.send_message(&message).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.delivery, "queued");

    let calls = tool_calls(&server.requests());
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    assert_eq!(calls[0]["name"], "t3_thread_send");
    assert_eq!(
        calls[0]["arguments"]["clientRequestId"],
        message.client_request_id.as_str()
    );
    assert_eq!(calls[0]["arguments"]["mode"], "queue");
}

#[test]
fn thread_listing_follows_the_cursor() {
    let server = fake_t3(|name, arguments| {
        assert_eq!(name, "t3_thread_list");
        assert_eq!(arguments["includeSubagents"], true);
        let cursor = arguments["cursor"].as_u64().unwrap();
        let thread = |id: &str| {
            json!({ "threadId": id, "title": id, "createdBy": "user", "creationSource": "mcp",
                    "status": "idle", "latestRunId": null, "providerInstanceId": "codex",
                    "model": "gpt-6-astra", "runtimeMode": "full-access",
                    "interactionMode": "default",
                    "linkedPullRequest": { "url": "https://git.oklabs.uk/BeFeast/okilum/pulls/1" },
                    "settled": false, "settledAt": null, "snoozed": false, "snoozedUntil": null,
                    "parentThreadId": null, "relationshipToParent": null, "itemCount": 12,
                    "createdAt": "2026-10-18T17:00:00.000Z", "updatedAt": "2026-10-18T17:05:00.000Z" })
        };
        let page = match cursor {
            0 => json!({ "projectId": "p", "currentThreadId": null, "threads": [thread("a")],
                         "nextCursor": 1, "total": 2 }),
            1 => json!({ "projectId": "p", "currentThreadId": null, "threads": [thread("b")],
                         "nextCursor": null, "total": 2 }),
            other => panic!("cursor {other}"),
        };
        structured(page)
    });
    let threads = gateway(&server, Some(token("good")))
        .list_threads("p")
        .unwrap();
    let ids: Vec<&str> = threads.iter().map(|t| t.thread_id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(threads[0].item_count, 12);
    assert_eq!(
        threads[0].pull_request_url(),
        Some("https://git.oklabs.uk/BeFeast/okilum/pulls/1")
    );
}

#[test]
fn a_reply_as_an_event_stream_is_read() {
    let server = FakeServer::start(|request| {
        let message = request.json();
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => Response::json(
                200,
                json!({ "jsonrpc": "2.0", "id": message["id"], "result": {} }),
            ),
            "notifications/initialized" => Response::empty(202),
            _ => {
                let reply = json!({ "jsonrpc": "2.0", "id": message["id"],
                                    "result": structured(json!({ "requestIds": ["q"] })) });
                Response {
                    status: 200,
                    headers: vec![("content-type".into(), "text/event-stream".into())],
                    body: format!(
                        "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                        json!({ "jsonrpc": "2.0", "method": "notifications/progress" }),
                        reply
                    ),
                }
            }
        }
    });
    let gateway = gateway(&server, Some(token("good")));
    assert_eq!(gateway.pending_requests("t").unwrap(), vec!["q"]);
}

#[test]
fn a_dropped_session_is_restarted_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let inner = fake_t3(|_, _| structured(json!({ "requestIds": [] })));
    let origin = inner.origin.clone();
    // The first tool call meets a server that forgot the session.
    let server = FakeServer::start(move |request| {
        if request.json()["method"] == "tools/call" && counter.fetch_add(1, Ordering::SeqCst) == 0 {
            return Response::empty(404);
        }
        forward(&origin, request)
    });
    let gateway = gateway(&server, Some(token("good")));
    assert!(gateway.pending_requests("t").unwrap().is_empty());
    let methods: Vec<String> = server
        .requests()
        .iter()
        .map(|r| r.json()["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/call",
            "initialize",
            "notifications/initialized",
            "tools/call"
        ]
    );
}

#[test]
fn configure_model_returns_the_configuration_read_back() {
    let server = fake_t3(|name, arguments| match name {
        "t3_thread_configure" => {
            assert_eq!(arguments["modelSelection"]["model"], "claude-fable-5-1");
            structured(json!({ "sequence": 7 }))
        }
        // The server kept the old model: the UI must show what is true.
        "t3_thread_configuration" => structured(configuration("claude-opus-5-5")),
        other => panic!("unexpected tool {other}"),
    });
    let selection = ModelSelection {
        instance_id: "claudeAgent".into(),
        model: "claude-fable-5-1".into(),
        options: vec![],
    };
    let confirmed = gateway(&server, Some(token("good")))
        .configure_model("mcp:manager", &selection)
        .unwrap();
    assert_eq!(confirmed.model_selection.model, "claude-opus-5-5");
}

fn model_page(project: &str) -> Value {
    json!({"thread":{"threadId":"mcp:manager","projectId":project,"title":"Manager",
        "status":"idle","providerInstanceId":"claudeAgent","model":"old",
        "runtimeMode":"full-access","updatedAt":"now"},"items":[]})
}

fn model_catalog() -> Value {
    json!({"runtimeMode":"full-access","providers":[{"providerInstanceId":"claudeAgent",
        "driverKind":"claude","models":[{"id":"old"},{"id":"new"}]}]})
}

#[test]
fn project_model_control_uses_live_catalog_and_confirms_actual_configuration() {
    let selected = Arc::new(std::sync::Mutex::new(
        json!({"instanceId":"claudeAgent","model":"old",
        "options":[{"id":"effort","value":"high"}]}),
    ));
    let retained = selected.clone();
    let server = fake_t3(move |name, args| {
        structured(match name {
            "t3_thread_read" => model_page("managers"),
            "orchestrator_capabilities" => model_catalog(),
            "t3_thread_configuration" => {
                json!({"threadId":"mcp:manager","modelSelection":*retained.lock().unwrap(),
            "runtimeMode":"full-access","interactionMode":"default"})
            }
            "t3_thread_configure" => {
                *retained.lock().unwrap() = args["modelSelection"].clone();
                json!({})
            }
            other => panic!("unexpected {other}"),
        })
    });
    let gateway = gateway(&server, Some(token("good")));
    let target = crate::models::Target {
        thread_id: "mcp:manager".into(),
        project_id: "managers".into(),
    };
    let snapshot = target.load(&gateway).unwrap();
    assert_eq!(snapshot.selection("claudeAgent", "old").options.len(), 1);
    let selection = snapshot.selection("claudeAgent", "new");
    assert!(
        selection.options.is_empty(),
        "old model options must not leak to a new model"
    );
    let confirmed = target.configure(&gateway, &selection).unwrap();
    assert_eq!(confirmed.configuration.model_selection, selection);
    let calls = tool_calls(&server.requests());
    assert_eq!(calls[calls.len() - 2]["name"], "t3_thread_configure");
    assert_eq!(calls[calls.len() - 1]["name"], "t3_thread_configuration");
    assert_eq!(
        calls
            .iter()
            .filter(|c| c["name"] == "t3_thread_configure")
            .count(),
        1
    );
}

#[test]
fn project_model_control_never_claims_a_configure_ack_or_permission_error_as_confirmation() {
    for deny in [false, true] {
        let server = fake_t3(move |name, _| {
            if name == "t3_thread_configure" && deny {
                return json!({"isError":true,"structuredContent":{"code":"capability_denied","message":"Credential cannot configure this full-access thread"}});
            }
            structured(match name {
                "t3_thread_read" => model_page("managers"),
                "orchestrator_capabilities" => model_catalog(),
                // Even a successful configure ACK does not establish the new model.
                "t3_thread_configuration" => configuration("old"),
                "t3_thread_configure" => json!({"success":true}),
                other => panic!("unexpected {other}"),
            })
        });
        let gateway = gateway(&server, Some(token("good")));
        let target = crate::models::Target {
            thread_id: "mcp:manager".into(),
            project_id: "managers".into(),
        };
        let selection = ModelSelection {
            instance_id: "claudeAgent".into(),
            model: "new".into(),
            options: vec![],
        };
        let error = target
            .configure(&gateway, &selection)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(if deny {
                "capability_denied"
            } else {
                "unconfirmed"
            }),
            "{error}"
        );
        assert_eq!(
            tool_calls(&server.requests())
                .iter()
                .filter(|c| c["name"] == "t3_thread_configure")
                .count(),
            1,
            "an unconfirmed write must not trigger another write"
        );
    }
}

#[test]
fn project_model_control_refuses_moved_targets_and_models_removed_from_live_catalog() {
    for moved in [false, true] {
        let server = fake_t3(move |name, _| {
            structured(match name {
                "t3_thread_read" => model_page(if moved { "elsewhere" } else { "managers" }),
                "orchestrator_capabilities" => model_catalog(),
                "t3_thread_configuration" => configuration("old"),
                other => panic!("no write is permitted here: {other}"),
            })
        });
        let gateway = gateway(&server, Some(token("good")));
        let target = crate::models::Target {
            thread_id: "mcp:manager".into(),
            project_id: "managers".into(),
        };
        let selection = ModelSelection {
            instance_id: "claudeAgent".into(),
            model: "removed".into(),
            options: vec![],
        };
        assert!(target.configure(&gateway, &selection).is_err());
        assert!(tool_calls(&server.requests())
            .iter()
            .all(|c| c["name"] != "t3_thread_configure"));
    }
}

#[test]
fn reads_a_thread_page_and_a_growing_item() {
    let server = fake_t3(|name, arguments| {
        assert_eq!(name, "t3_thread_read");
        let truncated = arguments.get("itemId").is_none();
        structured(json!({
            "thread": { "threadId": "mcp:manager", "projectId": "p", "title": "Manager",
                        "createdBy": "user", "creationSource": "mcp", "status": "running",
                        "latestRunId": "run1", "activeRunId": "run1",
                        "providerInstanceId": "claudeAgent", "model": "claude-opus-5-5",
                        "runtimeMode": "full-access", "interactionMode": "default",
                        "linkedPullRequest": null, "titleRegeneration": null, "branch": null,
                        "worktreePath": null, "parentThreadId": null,
                        "relationshipToParent": null, "runCount": 3, "itemCount": 40,
                        "pendingRequestCount": 1, "archived": false, "settled": false,
                        "settledAt": null, "snoozed": false, "snoozedUntil": null,
                        "createdAt": "2026-10-18T17:00:00.000Z",
                        "updatedAt": "2026-10-18T17:05:00.000Z" },
            "recentRuns": [],
            "items": [{ "position": 39, "visibility": "local", "sourceThreadId": "mcp:manager",
                        "itemId": "item-39", "runId": "run1", "messageId": "m", "createdBy": null,
                        "creationSource": null, "type": "assistant_message", "status": "running",
                        "title": null, "text": if truncated { "Hel" } else { "Hello" },
                        "textTruncated": truncated, "nextTextOffset": if truncated { json!(3) } else { Value::Null },
                        "updatedAt": "2026-10-18T17:05:00.000Z" }],
            "nextPosition": 40,
            "hasMore": false
        }))
    });
    let gateway = gateway(&server, Some(token("good")));
    let page = gateway
        .read_thread(&ThreadQuery::messages("mcp:manager").after(38))
        .unwrap();
    assert_eq!(page.thread.pending_request_count, 1);
    assert_eq!(page.next_position, Some(40));
    let item = &page.items[0];
    assert_eq!(
        (item.kind.as_str(), item.next_text_offset),
        ("assistant_message", Some(3))
    );

    let rest = gateway
        .read_thread(&ThreadQuery::messages("mcp:manager").item("item-39", item.next_text_offset))
        .unwrap();
    assert_eq!(rest.items[0].text.as_deref(), Some("Hello"));
    let calls = tool_calls(&server.requests());
    assert_eq!(calls[1]["arguments"]["textOffset"], 3);
}

#[test]
fn an_sse_reply_returns_before_the_server_closes_the_stream() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let env = Environment::new(
        "test",
        &format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    let (returned, wait) = mpsc::channel();
    let server = std::thread::spawn(move || {
        for step in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            if step == 1 {
                stream
                    .write_all(
                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                continue;
            }
            let result = if step == 0 {
                json!({})
            } else {
                structured(json!({"requestIds": ["q"]}))
            };
            let reply = json!({"jsonrpc":"2.0", "id":body["id"], "result":result});
            if step == 0 {
                let text = reply.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).unwrap();
            } else {
                let event = format!("data: {reply}\r\n\r\n");
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{event}\r\n", event.len()).unwrap();
                stream.flush().unwrap();
                // The stream deliberately never reaches EOF while the client reads.
                // A whole-body decoder hits an incomplete chunked response instead.
                let _ = wait.recv_timeout(Duration::from_secs(3));
            }
        }
    });
    let gateway = McpGateway::new(env, Some(token("good"))).unwrap();
    let result = gateway.pending_requests("t");
    let _ = returned.send(());
    server.join().unwrap();
    assert_eq!(result.unwrap(), vec!["q"]);
}

#[test]
fn an_incomplete_listing_is_an_error_instead_of_a_partial_success() {
    for stuck in [false, true] {
        let server = fake_t3(move |_, args| {
            let cursor = args["cursor"].as_u64().unwrap();
            structured(json!({"projects": [], "nextCursor": if stuck {cursor} else {cursor + 1}}))
        });
        let error = gateway(&server, Some(token("good")))
            .list_projects()
            .unwrap_err();
        assert!(matches!(error, GatewayError::Protocol(_)));
        assert_eq!(
            tool_calls(&server.requests()).len(),
            if stuck { 1 } else { MAX_PAGES }
        );
    }
}

#[test]
fn expiry_takes_precedence_over_an_observed_connection_failure() {
    let server = fake_t3(|_, _| unreachable!());
    let gateway = gateway(&server, Some(token("good")));
    *gateway.observed.lock().unwrap() = Some((
        "good".into(),
        GatewayStatus::Unreachable {
            detail: "offline".into(),
        },
    ));
    gateway.token.lock().unwrap().as_mut().unwrap().expires_at = unix_now() - 1;
    assert_eq!(gateway.status(), GatewayStatus::Expired);
    assert_eq!(gateway.capabilities().unwrap_err(), GatewayError::Expired);
    assert!(server.requests().is_empty());
}

#[test]
fn launch_context_resolves_only_the_bound_manager_and_executor_project_over_mcp() {
    for invalid in ["", "manager", "project"] {
        let server = fake_t3(move |name, _| {
            structured(match name {
                "t3_thread_read" => model_page(if invalid == "manager" {
                    "moved"
                } else {
                    "managers"
                }),
                "t3_project_list" => {
                    json!({"projects": if invalid == "project" { json!([]) } else { json!([
                {"id":"executors","title":"Repo","workspaceRoot":"/server/repo","defaultModelSelection":{"instanceId":"claudeAgent","model":"new"}}]) }})
                }
                "orchestrator_capabilities" => model_catalog(),
                other => panic!("request preparation must not launch or mutate anything: {other}"),
            })
        });
        let gateway = gateway(&server, Some(token("good")));
        let source = include_str!("../../tests/fixtures/area/ops/project.yaml")
            .replace("poc-managers", "managers")
            .replace("mcp:poc-manager", "mcp:manager")
            .replace("poc-executors", "executors");
        let binding = crate::manifest::AreaBinding::parse(&source)
            .unwrap()
            .unwrap();
        let result = crate::launch::LaunchContext::load(&gateway, &binding);
        if invalid.is_empty() {
            let context = result.unwrap();
            assert_eq!(context.project.id, "executors");
            assert_eq!(
                context.project.default_model_selection.unwrap().model,
                "new"
            );
        } else {
            assert!(result.is_err());
        }
        assert!(tool_calls(&server.requests()).iter().all(|c| matches!(
            c["name"].as_str(),
            Some("t3_thread_read" | "t3_project_list" | "orchestrator_capabilities")
        )));
    }
}
