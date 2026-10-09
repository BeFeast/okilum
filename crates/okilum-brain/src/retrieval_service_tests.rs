use super::*;
use crate::{
    context::ReviewedPacket,
    retrieval::{Citation, SearchScope, SourceMetadata},
};
use okilum_core::source::WriteBoundary;
use std::{fs, io::Read, sync::mpsc, time::Instant};
use uuid::Uuid;

fn fixture() -> (tempfile::TempDir, Arc<Mutex<Backend>>, String) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let operational = temp.path().join("runtime");
    fs::create_dir_all(root.join("records")).unwrap();
    fs::create_dir(&operational).unwrap();
    let mut runner = Runner::open(RunnerConfig {
        brain_id: Uuid::new_v4().to_string(),
        root,
        operational_dir: operational.clone(),
        records_dir: "records".into(),
        boundary: WriteBoundary::Managed,
    })
    .unwrap();
    let goal = Uuid::new_v4().to_string();
    runner
        .create_goal(
            Goal {
                id: goal.clone(),
                title: "Fixture export".into(),
                status: "draft".into(),
                criteria: vec![Criterion {
                    id: "C1".into(),
                    description: "Preserve evidence".into(),
                    requires_human: false,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "\n# Fixture\n".into(),
        )
        .unwrap();
    let jobs = crate::context_jobs::Jobs::open(&operational).unwrap();
    (
        temp,
        Arc::new(Mutex::new(Backend {
            runner,
            adapters: Adapters::new(),
            app: Application::unconfigured(),
            exports: crate::export::ExportDownloads::default(),
            todoist_picker: Default::default(),
            index: None,
            index_error: None,
            context_jobs: jobs,
        })),
        goal,
    )
}
fn packet(shared: &Arc<Mutex<Backend>>, goal: &str) -> ReviewedPacket {
    let mut b = shared.lock().unwrap();
    let Backend { runner, app, .. } = &mut *b;
    fs::write(
        runner.root().join("decision.md"),
        "# Decision\nShip when reviewed.\n",
    )
    .unwrap();
    let source = runner.read_source("decision.md").unwrap();
    let citation = Citation {
        citation_id: crate::retrieval::citation_id("decision.md", &source.revision, 1, 2),
        path: "decision.md".into(),
        revision: source.revision,
        start_line: 1,
        end_line: 2,
        locator: "L1-L2".into(),
        excerpt: "# Decision\nShip when reviewed.\n".into(),
        metadata: SourceMetadata::default(),
    };
    let scope = SearchScope {
        goal_id: goal.into(),
        mode: "project".into(),
        ..Default::default()
    };
    let value = app
        .context_prepare(
            runner,
            goal.into(),
            "Explain shipping".into(),
            scope,
            vec![citation],
            vec![],
        )
        .unwrap();
    let packet: ReviewedPacket = serde_json::from_value(value["packet"].clone()).unwrap();
    let value = app
        .context_revise(
            runner,
            goal.into(),
            packet.id,
            packet.revision,
            "Explain with the selected evidence.\n".into(),
        )
        .unwrap();
    serde_json::from_value(value["packet"].clone()).unwrap()
}
fn read_request(stream: &mut TcpStream) -> (String, Value) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
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
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    (
        header,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn respond(stream: &mut TcpStream, content_type: &str, body: &str) {
    let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
}
fn assert_responsive(shared: &Arc<Mutex<Backend>>, goal: &str) {
    let before = Instant::now();
    let value = dispatch(
        shared,
        Command::Snapshot {
            goal_id: Some(goal.into()),
        },
        None,
    )
    .unwrap();
    assert_eq!(value["goal"]["id"], goal);
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "snapshot blocked behind provider work"
    );
}
#[test]
fn stalled_export_keeps_backend_responsive_and_cancellation_wins_late_response() {
    let (_temp, shared, goal) = fixture();
    let packet = packet(&shared, &goal);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    shared.lock().unwrap().app.chat = Some(chat::ChatConfig {
        base_url: base,
        api_key: "synthetic-fixture-token".into(),
        model: "fixture".into(),
        idle_timeout: Duration::from_secs(5),
    });
    let (entered, received) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let provider = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (_, request) = read_request(&mut stream);
        assert!(!request.to_string().contains("content_base64"));
        entered.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        respond(&mut stream, "text/event-stream", "data: [DONE]\n\n");
    });
    let job = dispatch(
        &shared,
        Command::ContextExportStart {
            goal_id: goal.clone(),
            packet_id: packet.id.clone(),
            packet_revision: packet.revision,
        },
        None,
    )
    .unwrap();
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_responsive(&shared, &goal);
    let restored = dispatch(
        &shared,
        Command::ContextGet {
            goal_id: goal.clone(),
            packet_id: None,
        },
        None,
    )
    .unwrap();
    assert_eq!(restored["export_job"]["job_id"], job["job_id"]);
    let job_id = job["job_id"].as_str().unwrap().to_string();
    let cancelled = dispatch(
        &shared,
        Command::ContextExportCancel {
            goal_id: goal.clone(),
            job_id: job_id.clone(),
        },
        None,
    )
    .unwrap();
    assert_eq!(cancelled["status"], "interrupted");
    release.send(()).unwrap();
    provider.join().unwrap();
    assert!(dispatch(
        &shared,
        Command::ContextExportPrepare {
            goal_id: goal.clone(),
            job_id: job_id.clone()
        },
        None
    )
    .is_err());
    assert_eq!(
        dispatch(
            &shared,
            Command::ContextExportGet {
                goal_id: goal,
                job_id
            },
            None
        )
        .unwrap()["status"],
        "interrupted"
    );
}
#[test]
fn stalled_semantic_query_keeps_backend_responsive_and_provider_failure_is_explicit() {
    let (temp, shared, goal) = fixture();
    fs::write(
        temp.path().join("brain/reference.md"),
        "# Shipping\nReview before shipping.\n",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let digest = "a".repeat(64);
    fs::write(
        temp.path().join("runtime/retrieval-settings.json"),
        serde_json::to_vec(
            &json!({"base_url":base,"model":"fixture-embedding","digest":digest,"dimensions":2}),
        )
        .unwrap(),
    )
    .unwrap();
    let (entered, received) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let provider = std::thread::spawn(move || {
        let mut embeddings = 0;
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            let (header, request) = read_request(&mut stream);
            if header.starts_with("GET /api/tags ") {
                respond(
                    &mut stream,
                    "application/json",
                    &json!({"models":[{"name":"fixture-embedding","digest":digest}]}).to_string(),
                );
            } else {
                embeddings += 1;
                if embeddings == 2 {
                    entered.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                    respond(&mut stream, "application/json", "{\"embeddings\":[]}");
                    break;
                }
                let count = request["input"].as_array().unwrap().len();
                respond(
                    &mut stream,
                    "application/json",
                    &json!({"embeddings":vec![vec![1.0,0.0];count]}).to_string(),
                );
            }
        }
    });
    let index = {
        let b = shared.lock().unwrap();
        crate::retrieval::BrainIndex::start(
            b.runner.workspace_identity()["brain_id"]
                .as_str()
                .unwrap()
                .into(),
            b.runner.root().into(),
            "records".into(),
            b.runner.operational_root().into(),
            true,
        )
        .unwrap()
    };
    shared.lock().unwrap().index = Some(index.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    while index.status().status != "ready" || index.status().semantic_status != "ready" {
        assert!(
            Instant::now() < deadline,
            "index did not become ready: {:?}",
            index.status()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let other = shared.clone();
    let query_goal = goal.clone();
    let search = std::thread::spawn(move || {
        dispatch(
            &other,
            Command::BrainSearch {
                request: crate::retrieval::SearchRequest {
                    query: "shipping".into(),
                    scope: SearchScope {
                        goal_id: query_goal,
                        mode: "project".into(),
                        ..Default::default()
                    },
                    mode: "hybrid".into(),
                    limit: 5,
                    max_excerpt_bytes: 4096,
                },
            },
            None,
        )
    });
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_responsive(&shared, &goal);
    release.send(()).unwrap();
    provider.join().unwrap();
    let response = search.join().unwrap().unwrap();
    assert_eq!(response["mode_used"], "lexical");
    assert_eq!(response["mode_requested"], "hybrid");
    assert!(!response["warnings"].as_array().unwrap().is_empty());
    assert!(!response["hits"].as_array().unwrap().is_empty());
}

#[test]
fn inbox_wire_operations_work_without_providers_and_leave_selected_goal_unchanged() {
    let (_temp, backend, goal) = fixture();
    let workspace = backend.lock().unwrap().runner.workspace_identity();
    let before = dispatch(
        &backend,
        Command::Snapshot {
            goal_id: Some(goal.clone()),
        },
        Some(workspace.clone()),
    )
    .unwrap();
    let caps = dispatch(&backend, Command::Capabilities, Some(workspace.clone())).unwrap();
    assert_eq!(caps["inbox_read"], true);
    assert_eq!(caps["inbox_capture"], true);
    assert_eq!(caps["chat"], false);
    let operation = Uuid::new_v4().to_string();
    let wire = json!({"schema":crate::SCHEMA,"id":"wire-capture","expected_workspace":workspace,"op":"inbox_capture","operation_id":operation,"text":"Captured without LLM\r\n","source":{"channel":"native","instance_id":Uuid::new_v4().to_string(),"account_id":"local","actor_id":caps["actor"],"chat_id":null,"topic_id":null,"message_id":operation,"update_id":operation,"uri":null}});
    let request: Request = serde_json::from_value(wire.clone()).unwrap();
    let capture = dispatch(&backend, request.command, request.expected_workspace).unwrap();
    assert_eq!(capture["receipt"]["status"], "committed");
    let list = dispatch(
        &backend,
        Command::InboxList {
            limit: Some(1),
            cursor: None,
        },
        Some(workspace.clone()),
    )
    .unwrap();
    assert_eq!(list["items"][0]["capture_id"], capture["capture_id"]);
    let get = dispatch(
        &backend,
        Command::InboxGet {
            capture_id: capture["capture_id"].as_str().unwrap().into(),
        },
        Some(workspace.clone()),
    )
    .unwrap();
    assert_eq!(get["source"]["revision"], capture["revision"]);
    let after = dispatch(
        &backend,
        Command::Snapshot {
            goal_id: Some(goal),
        },
        Some(workspace),
    )
    .unwrap();
    assert_eq!(before, after);
    let request: Request = serde_json::from_value(wire).unwrap();
    assert!(dispatch(&backend, request.command, Some(json!({"brain_id":"wrong"}))).is_err());
}

#[test]
fn bound_attention_wire_contract_saves_decision_and_seen_without_provider_or_human_acceptance() {
    let (_temp, backend, goal) = fixture();
    {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let mut b = backend.lock().unwrap();
        let source = b.runner.goal_source().unwrap();
        let text = String::from_utf8(STANDARD.decode(&source.content_base64).unwrap()).unwrap();
        // An unsupported recorded completion produces a real goal-wide decision.
        b.runner
            .write_source(okilum_core::source::SourceWrite {
                schema: SCHEMA.into(),
                operation_id: Uuid::new_v4().to_string(),
                brain_id: source.brain_id,
                path: source.path,
                expected_revision: Some(source.revision),
                content_base64: STANDARD
                    .encode(text.replace("status: active", "status: completed")),
            })
            .unwrap();
    }
    let workspace = backend.lock().unwrap().runner.workspace_identity();
    let capabilities = dispatch(&backend, Command::Capabilities, Some(workspace.clone())).unwrap();
    assert_eq!(capabilities["attention_read"], true);
    assert_eq!(capabilities["attention_reply"], true);
    assert_eq!(capabilities["chat"], false);
    let listed = dispatch(
        &backend,
        Command::AttentionList {
            channel: None,
            limit: Some(50),
            cursor: None,
        },
        Some(workspace.clone()),
    )
    .unwrap();
    assert_eq!(listed["complete"], true);
    assert!(listed["delivery_cursor"].is_u64());
    let goal_before = dispatch(
        &backend,
        Command::Snapshot {
            goal_id: Some(goal.clone()),
        },
        Some(workspace.clone()),
    )
    .unwrap();
    let operation = Uuid::new_v4().to_string();
    let wire = serde_json::json!({"schema":SCHEMA,"id":"wrong-item","expected_workspace":workspace,"op":"attention_reply","operation_id":operation,"goal_id":goal,"attention_id":"unknown-current-item","expected_revision":format!("sha256:{}","a".repeat(64)),"stage_id":null,"text":"must remain unsent","source":{"channel":"native","instance_id":Uuid::new_v4().to_string(),"account_id":"local","actor_id":capabilities["actor"],"chat_id":null,"topic_id":null,"message_id":operation,"update_id":operation,"uri":null}});
    let request: Request = serde_json::from_value(wire.clone()).unwrap();
    let error = dispatch(&backend, request.command, request.expected_workspace).unwrap_err();
    let error = error_value(&error);
    assert_eq!(error["code"], "attention_stale");
    assert!(error["current"].is_null());
    let goal_after = dispatch(
        &backend,
        Command::Snapshot {
            goal_id: Some(goal),
        },
        Some(workspace),
    )
    .unwrap();
    assert_eq!(goal_before, goal_after);
    let item = &listed["items"][0];
    assert_eq!(item["kind"], "decision");
    let mut valid = wire.clone();
    valid["attention_id"] = item["attention_id"].clone();
    valid["expected_revision"] = item["revision"].clone();
    valid["text"] = json!("Keep this as an unverified decision");
    let request: Request = serde_json::from_value(valid.clone()).unwrap();
    let saved = dispatch(&backend, request.command, request.expected_workspace).unwrap();
    assert_eq!(saved["receipt"]["status"], "committed");
    assert!(saved["decision_id"].is_string());
    assert_eq!(saved["item"]["seen"], false);
    let ack_id = Uuid::new_v4().to_string();
    valid["op"] = json!("attention_ack");
    valid["operation_id"] = json!(ack_id);
    valid["source"]["message_id"] = json!(ack_id);
    valid["source"]["update_id"] = json!(ack_id);
    valid.as_object_mut().unwrap().remove("text");
    let request: Request = serde_json::from_value(valid).unwrap();
    let seen = dispatch(&backend, request.command, request.expected_workspace).unwrap();
    assert_eq!(seen["item"]["seen"], true);
    let final_snapshot = backend.lock().unwrap().runner.snapshot().unwrap();
    assert_eq!(final_snapshot.goal.unwrap().status, "blocked");
    assert!(final_snapshot.dispatch.is_none());
    let mut missing = wire;
    missing.as_object_mut().unwrap().remove("stage_id");
    assert!(serde_json::from_value::<Request>(missing).is_err());
}

#[test]
fn inbox_plan_wire_rejects_client_origin_or_goal_overrides_and_requires_workspace() {
    let (_temp, backend, _goal) = fixture();
    let operation = Uuid::new_v4().to_string();
    let source = json!({"channel":"native","instance_id":Uuid::new_v4().to_string(),"account_id":"local","actor_id":"local operator","chat_id":null,"topic_id":null,"message_id":operation,"update_id":operation,"uri":null});
    let workspace = backend.lock().unwrap().runner.workspace_identity();
    let mut request = json!({"schema":SCHEMA,"id":"plan-wire","expected_workspace":workspace,"op":"inbox_plan","operation_id":operation,"capture_id":Uuid::new_v4().to_string(),"expected_capture_revision":format!("sha256:{}","0".repeat(64)),"title":"Goal","criteria":[{"id":"C1","description":"Observable result","requires_human":true}],"source":source});
    let parsed = serde_json::from_value::<Request>(request.clone()).unwrap();
    assert!(dispatch(&backend, parsed.command, None).is_err());
    for (field, value) in [
        ("goal_id", json!(Uuid::new_v4().to_string())),
        ("origin", json!({"text":"forged"})),
        ("path", json!("unrelated.md")),
    ] {
        request[field] = value;
        assert!(
            serde_json::from_value::<Request>(request.clone()).is_err(),
            "{field} override accepted"
        );
        request.as_object_mut().unwrap().remove(field);
    }
}

#[test]
fn incoming_reference_wire_workspace_goal_capability_and_read_only_dispatch() {
    let (_temp, shared, goal) = fixture();
    let (workspace, revision, index) = {
        let backend = shared.lock().unwrap();
        fs::write(backend.runner.root().join("target.md"), "# Target\n").unwrap();
        fs::write(
            backend.runner.root().join("reference.md"),
            "# Source\n[[target]]\n",
        )
        .unwrap();
        let workspace = backend.runner.workspace_identity();
        let revision = backend.runner.read_source("target.md").unwrap().revision;
        let index = crate::retrieval::BrainIndex::start(
            workspace["brain_id"].as_str().unwrap().into(),
            backend.runner.root().into(),
            "records".into(),
            backend.runner.operational_root().into(),
            true,
        )
        .unwrap();
        (workspace, revision, index)
    };
    shared.lock().unwrap().index = Some(index.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    while index.status().status != "ready" {
        assert!(
            Instant::now() < deadline,
            "index did not become ready: {:?}",
            index.status()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let wire = json!({"schema":"ai-brain/workspace-v1","id":"incoming-1","expected_workspace":workspace,"op":"source_backlinks","path":"target.md","expected_revision":revision,"scope":{"goal_id":goal,"mode":"project"},"limit":10,"cursor":null});
    for key in ["expected_workspace", "id"] {
        let mut bad = wire.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<Request>(bad).is_err());
    }
    let mut bad = wire.clone();
    bad["schema"] = json!("ai-brain/v1");
    assert!(serde_json::from_value::<Request>(bad).is_err());
    let mut bad = wire.clone();
    bad["unused"] = json!(true);
    assert!(serde_json::from_value::<Request>(bad).is_err());
    let request: Request = serde_json::from_value(wire.clone()).unwrap();
    let before = {
        let b = shared.lock().unwrap();
        (
            fs::read(b.runner.operational_root().join("state.json")).unwrap(),
            fs::read(b.runner.root().join("reference.md")).unwrap(),
        )
    };
    let response = dispatch(&shared, request.command, request.expected_workspace).unwrap();
    assert_eq!(response["rows"][0]["path"], "reference.md");
    assert_eq!(response["rows"][0]["start_line"], 2);
    assert_eq!(response["target"]["revision"], revision);
    assert_eq!(
        dispatch(&shared, Command::Capabilities, Some(workspace.clone())).unwrap()
            ["source_backlinks"],
        true
    );
    let mut bad = wire.clone();
    bad["scope"]["goal_id"] = json!(Uuid::new_v4().to_string());
    let request: Request = serde_json::from_value(bad).unwrap();
    assert!(
        dispatch(&shared, request.command, request.expected_workspace)
            .unwrap_err()
            .to_string()
            .contains("unknown target goal")
    );
    let mut bad = wire;
    bad["expected_workspace"]["brain_id"] = json!(Uuid::new_v4().to_string());
    let request: Request = serde_json::from_value(bad).unwrap();
    assert!(dispatch(&shared, request.command, request.expected_workspace).is_err());
    let b = shared.lock().unwrap();
    assert_eq!(
        fs::read(b.runner.operational_root().join("state.json")).unwrap(),
        before.0
    );
    assert_eq!(
        fs::read(b.runner.root().join("reference.md")).unwrap(),
        before.1
    );
}
