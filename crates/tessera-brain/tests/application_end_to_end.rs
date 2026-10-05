//! Independent application acceptance using real transports and synthetic providers.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};
use tessera_brain::{application::*, chat::ChatClient, *};
use tessera_core::source::{SourceWrite, WriteBoundary};
use uuid::Uuid;

const WHEN: &str = "2026-09-05T12:00:00Z";
const SOURCE: &str = "# Supplies\nSTOCK-CABINET-09: need 4 AA, stock 6; need 2 markers, stock 1; need 1 tape, stock 0. Marker costs 1 credit; tape costs 3. Budget 5.\n";
const QUESTION: &str = "Make a packing and shopping checklist; preserve the budget.";
const DISCUSSION: &str = "Use existing AA stock; buy one marker and one tape.";
const RESULT: &str = "Pack 4 AA, 2 markers, 1 tape. Buy 0 AA, 1 marker for 1 credit, 1 tape for 3 credits. Total 4 credits, within budget 5. Source: STOCK-CABINET-09.";

fn listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}
fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("missing fixture connection: {error}"),
        }
    }
}
fn request(stream: &mut TcpStream) -> (String, String) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header = String::from_utf8(header).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (header, String::from_utf8(body).unwrap())
}
fn reply(stream: &mut TcpStream, content_type: &str, body: &str) {
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}
struct Providers {
    todoist: String,
    chat: String,
    t3: String,
    task_worker: thread::JoinHandle<()>,
    chat_worker: thread::JoinHandle<()>,
    t3_worker: thread::JoinHandle<Value>,
}
impl Providers {
    fn new(operation: String) -> Self {
        let tasks = listener();
        let todoist = format!("http://{}/api/v1/", tasks.local_addr().unwrap());
        let task_worker = thread::spawn(move || {
            let mut original = None;
            for step in 0..4 {
                let mut stream = accept(&tasks);
                let (headers, body) = request(&mut stream);
                assert!(headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-token"));
                if step < 2 {
                    assert!(headers.starts_with("POST /api/v1/sync "));
                    let fields: BTreeMap<_, _> =
                        reqwest::Url::parse(&format!("http://fixture/?{body}"))
                            .unwrap()
                            .query_pairs()
                            .map(|(k, v)| (k.into_owned(), v.into_owned()))
                            .collect();
                    let commands: Value = serde_json::from_str(&fields["commands"]).unwrap();
                    assert_eq!(commands[0]["uuid"], operation);
                    assert_eq!(commands[0]["type"], "item_add");
                    if step == 0 {
                        original = Some(commands);
                        // Provider accepted, but the client cannot know the outcome.
                        continue;
                    }
                    assert_eq!(
                        Some(commands),
                        original,
                        "recovery must replay the exact logical mutation"
                    );
                    reply(&mut stream, "application/json", &json!({"sync_status":{operation.clone():"ok"},"temp_id_mapping":{operation.clone():"fixture-task"}}).to_string());
                } else {
                    assert!(headers.starts_with("GET /api/v1/tasks/fixture-task "));
                    reply(&mut stream, "application/json", &json!({"id":"fixture-task","project_id":"inbox","content":"Prepare workshop supplies","description":"","checked":false,"is_deleted":false,"labels":[],"priority":1,"due":null,"updated_at":WHEN,"completed_at":null}).to_string());
                }
            }
        });
        let chats = listener();
        let chat = format!("http://{}/v1/", chats.local_addr().unwrap());
        let chat_worker = thread::spawn(move || {
            let mut stream = accept(&chats);
            let (headers, body) = request(&mut stream);
            assert!(headers.starts_with("POST /v1/chat/completions "));
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["model"], "fixture-model");
            let messages = body["messages"].as_array().unwrap();
            assert!(messages.iter().any(|m| m["content"]
                .as_str()
                .is_some_and(|s| s.contains("STOCK-CABINET-09") && s.contains("stock 6"))));
            assert!(messages.iter().any(|m| m["content"] == QUESTION));
            let events = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"index":0,"delta":{"content":DISCUSSION},"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
            );
            reply(&mut stream, "text/event-stream", &events);
        });
        let t3_listener = listener();
        let t3 = format!("http://{}", t3_listener.local_addr().unwrap());
        let t3_worker = thread::spawn(move || {
            let mut command = Value::Null;
            for step in 0..4 {
                let mut http = accept(&t3_listener);
                let (headers, _) = request(&mut http);
                assert!(headers.starts_with("POST /api/auth/websocket-ticket "));
                assert!(headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-token"));
                reply(
                    &mut http,
                    "application/json",
                    r#"{"ticket":"fixture-ticket"}"#,
                );
                drop(http);
                let mut socket = tungstenite::accept(accept(&t3_listener)).unwrap();
                let call: Value =
                    serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
                let id = &call["id"];
                let response = match step {
                    0 => {
                        assert_eq!(call["tag"], "orchestration.dispatchCommand");
                        command = call["payload"].clone();
                        assert_eq!(command["type"], "thread.turn.start");
                        json!({"_tag":"Exit","requestId":id,"exit":{"_tag":"Success","value":{"sequence":10}}})
                    }
                    1 | 2 => {
                        assert_eq!(call["tag"], "orchestration.subscribeThread");
                        assert_eq!(call["payload"]["threadId"], command["threadId"]);
                        let completed = step == 2;
                        let mut snapshot = json!({"snapshotSequence":if completed {20}else{11},"thread":{
                            "id":command["threadId"],"projectId":"fixture-project","updatedAt":WHEN,
                            "latestTurn":{"turnId":"fixture-turn","state":if completed {"completed"}else{"running"},"requestedAt":command["createdAt"],"startedAt":WHEN,"completedAt":if completed {json!(WHEN)}else{Value::Null},"assistantMessageId":"assistant-1"},
                            "messages":[{"id":command["message"]["messageId"],"role":"user","text":command["message"]["text"],"turnId":null,"streaming":false,"createdAt":command["createdAt"],"updatedAt":WHEN},{"id":"assistant-1","role":"assistant","text":RESULT,"turnId":"fixture-turn","streaming":false,"createdAt":WHEN,"updatedAt":WHEN}],
                            "activities":[],"proposedPlans":[],"session":null,
                            "checkpoints":if completed {json!([{"turnId":"fixture-turn","checkpointTurnCount":1,"checkpointRef":"refs/t3/checkpoints/fixture/1","status":"ready","files":[],"assistantMessageId":"assistant-1","completedAt":WHEN}])}else{json!([])}
                        }});
                        if !completed {
                            snapshot["thread"]["latestTurn"] = Value::Null;
                        }
                        json!({"_tag":"Chunk","requestId":id,"values":[{"kind":"snapshot","snapshot":snapshot},{"kind":"synchronized"}]})
                    }
                    _ => {
                        assert_eq!(call["tag"], "orchestration.getTurnDiff");
                        json!({"_tag":"Exit","requestId":id,"exit":{"_tag":"Success","value":{"threadId":command["threadId"],"fromTurnCount":0,"toTurnCount":1,"diff":""}}})
                    }
                };
                socket
                    .send(tungstenite::Message::Text(response.to_string().into()))
                    .unwrap();
                if step == 1 || step == 2 {
                    let ack: Value =
                        serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
                    assert_eq!(ack["_tag"], "Ack");
                    let interrupt: Value =
                        serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
                    assert_eq!(interrupt["_tag"], "Interrupt");
                }
            }
            command
        });
        Self {
            todoist,
            chat,
            t3,
            task_worker,
            chat_worker,
            t3_worker,
        }
    }
    fn config(&self, env: &str) -> ApplicationConfig {
        serde_json::from_value(json!({"actor":"fixture reviewer","chat":{"base_url":self.chat,"model":"fixture-model","api_key_env":env},"todoist":{"base_url":self.todoist,"instance_id":"fixture-account","token_env":env},"t3":{"base_url":self.t3,"token_env":env,"environment_id":"fixture-env","project_id":"fixture-project","model_instance_id":"fixture-provider","model":"fixture-model","runtime_mode":"approval-required","interaction_mode":"default"}})).unwrap()
    }
}

fn exercise(human: bool) {
    let temporary = tempfile::tempdir().unwrap();
    let brain = temporary.path().join("brain");
    let operational = temporary.path().join("operational");
    fs::create_dir_all(brain.join("sources")).unwrap();
    fs::create_dir(brain.join("records")).unwrap();
    fs::create_dir(&operational).unwrap();
    fs::write(brain.join("sources/supplies.md"), SOURCE).unwrap();
    let brain_id = Uuid::new_v4().to_string();
    let goal_id = Uuid::new_v4().to_string();
    let operation_id = Uuid::new_v4().to_string();
    let providers = Providers::new(operation_id.clone());
    let env = format!("TESSERA_E2E_{}", Uuid::new_v4().simple());
    std::env::set_var(&env, "fixture-token");
    let open = || {
        Runner::open(RunnerConfig {
            brain_id: brain_id.clone(),
            root: brain.clone(),
            operational_dir: operational.clone(),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        })
        .unwrap()
    };
    let mut runner = open();
    let (app, adapters) =
        Application::configure(providers.config(&env), &operational, &mut runner).unwrap();
    runner
        .create_goal(
            Goal {
                id: goal_id.clone(),
                title: "Prepare workshop supplies".into(),
                status: "draft".into(),
                criteria: vec![Criterion {
                    id: "C1".into(),
                    description: "The saved checklist matches the stock and budget".into(),
                    requires_human: human,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "\n# Workshop supplies\n".into(),
        )
        .unwrap();
    let task = app
        .task_create(
            &mut runner,
            goal_id.clone(),
            operation_id.clone(),
            "Prepare workshop supplies".into(),
        )
        .unwrap();
    assert_eq!(task["status"], "indeterminate");
    assert_eq!(
        app.snapshot(&runner).unwrap()["pending_task_operation_id"],
        operation_id
    );
    assert!(app
        .task_create(
            &mut runner,
            goal_id.clone(),
            operation_id.clone(),
            "Changed request".into()
        )
        .is_err());
    drop((app, adapters, runner));
    let mut runner = open();
    let (app, mut adapters) =
        Application::configure(providers.config(&env), &operational, &mut runner).unwrap();
    assert_eq!(
        app.snapshot(&runner).unwrap()["pending_task_operation_id"],
        operation_id
    );
    app.task_reconcile(&mut runner, operation_id.clone())
        .unwrap();
    assert!(app.snapshot(&runner).unwrap()["pending_task_operation_id"].is_null());
    assert_eq!(
        app.snapshot(&runner).unwrap()["task"]["task_id"],
        "fixture-task"
    );
    // Terminal replay may refresh, but must not submit another provider command.
    app.task_reconcile(&mut runner, operation_id).unwrap();
    let (conversation, config, request) = app
        .chat_start(
            &mut runner,
            goal_id.clone(),
            QUESTION.into(),
            vec!["sources/supplies.md".into()],
            None,
        )
        .unwrap();
    let client = ChatClient::new(config).unwrap();
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(client.run(request, receiver, |event| {
            Application::chat_event(&mut runner, &conversation, event).unwrap()
        }));
    assert_eq!(
        app.chat_get(&runner, &conversation).unwrap()["status"],
        "complete"
    );
    app.stage_prepare(
        &mut runner,
        goal_id.clone(),
        Some(conversation.clone()),
        vec!["sources/supplies.md".into()],
        vec!["C1".into()],
        "Produce the packing and shopping checklist".into(),
    )
    .unwrap();
    assert!(app.snapshot(&runner).unwrap()["thread_url"].is_null());
    let frozen = runner.snapshot().unwrap().dispatch.unwrap();
    let context = runner
        .read_source(&format!("records/context-{}.md", frozen.context_id))
        .unwrap();
    let context = String::from_utf8(STANDARD.decode(context.content_base64).unwrap()).unwrap();
    let body = context.splitn(3, "---").nth(2).unwrap();
    assert!(body.contains(SOURCE));
    assert!(body.contains(QUESTION));
    assert!(body.contains(DISCUSSION));
    assert!(!body.contains("content_base64"));
    assert_eq!(frozen.packet.extra["source_excerpts"][0]["text"], SOURCE);
    assert_eq!(
        frozen.packet.extra["conversation"]["messages"][0]["text"],
        QUESTION
    );
    assert_eq!(
        frozen.packet.extra["conversation"]["messages"][1]["text"],
        DISCUSSION
    );
    assert_eq!(
        STANDARD
            .decode(
                frozen.packet.extra["conversation"]["source_snapshots"][0]["content_base64"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
        SOURCE.as_bytes()
    );
    let started = runner
        .start(adapters.get_mut("t3").unwrap().as_mut())
        .unwrap();
    assert_eq!(started.phase.as_deref(), Some("indeterminate"));
    assert!(started.binding.is_none());
    assert!(app.snapshot(&runner).unwrap()["attention"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["message"] == "t3_command_accepted_waiting_for_correlated_turn"));
    let thread_id = tessera_brain::t3::T3Adapter::thread_id(&frozen).unwrap();
    assert_eq!(
        app.snapshot(&runner).unwrap()["thread_url"],
        format!("{}/fixture-env/{thread_id}", providers.t3)
    );
    let source = runner.read_source("sources/supplies.md").unwrap();
    runner
        .write_source(SourceWrite {
            schema: SCHEMA.into(),
            brain_id: brain_id.clone(),
            operation_id: Uuid::new_v4().to_string(),
            path: source.path,
            expected_revision: Some(source.revision),
            content_base64: STANDARD.encode("# Changed after dispatch\nWRONG-NEW-STOCK\n"),
        })
        .unwrap();
    drop((app, adapters, runner));
    let mut runner = open();
    let (app, mut adapters) =
        Application::configure(providers.config(&env), &operational, &mut runner).unwrap();
    assert_eq!(
        runner.snapshot().unwrap().phase.as_deref(),
        Some("indeterminate")
    );
    assert_eq!(
        runner.snapshot().unwrap().dispatch.unwrap().packet,
        frozen.packet
    );
    assert_eq!(
        app.chat_get(&runner, &conversation).unwrap()["messages"][1]["text"],
        DISCUSSION
    );
    let recovered = runner
        .reconcile(adapters.get_mut("t3").unwrap().as_mut())
        .unwrap();
    assert_ne!(recovered.goal.unwrap().status, "completed");
    assert!(recovered
        .attention_history
        .iter()
        .any(|a| a.message == "t3_command_accepted_waiting_for_correlated_turn"));
    assert!(!app.snapshot(&runner).unwrap()["attention"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["message"] == "t3_command_accepted_waiting_for_correlated_turn"));
    let result_id = recovered.stage.unwrap().result_ids[0].clone();
    let result = runner.result(&result_id).unwrap();
    assert_eq!(result.outcome.summary, RESULT);
    assert_eq!(result.outcome.verification, "unverified");
    assert!(result
        .outcome
        .evidence
        .iter()
        .all(|e| e.status == "unverified"));
    let path = app.snapshot(&runner).unwrap()["source_paths"]["result"]
        .as_str()
        .unwrap()
        .to_owned();
    let saved = runner.read_source(&path).unwrap();
    let markdown = String::from_utf8(STANDARD.decode(&saved.content_base64).unwrap()).unwrap();
    let (_, body) = markdown
        .strip_prefix("---\n")
        .unwrap()
        .split_once("\n---\n")
        .unwrap();
    // Review the actual readable artifact, not an engine's completion flag.
    assert!(body.contains(RESULT));
    assert!(body.contains("assistant-1"));
    assert!(body.contains("refs/t3/checkpoints/fixture/1"));
    let evaluation = CriterionEvaluation {
        criterion_id: "C1".into(),
        goal_revision: frozen.packet.goal_revision.clone(),
        status: "passed".into(),
        evidence_ids: vec!["t3-message:assistant-1".into()],
        evaluated_by: "fixture oracle reviewer".into(),
        evaluated_at: WHEN.into(),
    };
    if human {
        assert!(runner
            .evaluate_result(result_id.clone(), evaluation)
            .is_err());
        runner
            .accept_human(
                "C1".into(),
                "fixture human reviewer".into(),
                WHEN.into(),
                SourceRef {
                    uri: format!("brain://{brain_id}/{path}"),
                    revision: Some(saved.revision),
                    locator: None,
                },
            )
            .unwrap();
    } else {
        let mut missing = evaluation.clone();
        missing.evidence_ids = vec!["not-a-saved-evidence".into()];
        assert!(runner.evaluate_result(result_id.clone(), missing).is_err());
        assert_ne!(runner.snapshot().unwrap().goal.unwrap().status, "completed");
        runner
            .evaluate_result(result_id.clone(), evaluation)
            .unwrap();
    }
    assert_eq!(runner.snapshot().unwrap().goal.unwrap().status, "completed");
    assert_eq!(
        app.snapshot(&runner).unwrap()["task"]["status"],
        "open",
        "goal completion must not fabricate Todoist completion"
    );
    assert!(runner
        .snapshot()
        .unwrap()
        .attention
        .iter()
        .any(|a| a.kind == "final"));
    providers.task_worker.join().unwrap();
    providers.chat_worker.join().unwrap();
    let command = providers.t3_worker.join().unwrap();
    let prompt = command["message"]["text"].as_str().unwrap();
    assert!(prompt.contains("STOCK-CABINET-09") && prompt.contains("stock 6"));
    assert!(prompt.contains(QUESTION) && prompt.contains(DISCUSSION));
    assert!(!prompt.contains("WRONG-NEW-STOCK"));
    drop((app, adapters, runner));
    let runner = open();
    assert_eq!(runner.snapshot().unwrap().goal.unwrap().status, "completed");
    assert_eq!(
        runner.snapshot().unwrap().stage.unwrap().result_ids,
        vec![result_id]
    );
    std::env::remove_var(env);
}

#[test]
fn real_transports_recover_task_and_stage_then_require_explicit_human_acceptance() {
    exercise(true);
}

#[test]
fn real_transports_keep_nonhuman_result_unverified_until_saved_evidence_review() {
    exercise(false);
}
