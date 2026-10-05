//! Production CLI startup must never manufacture a historical T3 origin.
use anyhow::Result;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};
use tessera_brain::{
    application::{Application, ApplicationConfig, T3Settings},
    *,
};
use tessera_core::source::WriteBoundary;

const WHEN: &str = "2026-10-03T00:00:00Z";
fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}
struct Seed;
impl Adapter for Seed {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: "t3".into(),
            cancel: false,
        }
    }
    fn start(&mut self, e: &StartEnvelope) -> Result<StartReply> {
        Ok(StartReply::Accepted {
            binding: EngineRef {
                engine: "t3".into(),
                instance_id: "synthetic-old-environment".into(),
                thread_id: Some(t3::T3Adapter::thread_id(e)?),
                turn_id: Some("synthetic-old-turn".into()),
                task_id: None,
            },
        })
    }
    fn observe(&mut self, _: &EngineRef, _: &BTreeMap<String, String>) -> Result<Vec<EngineEvent>> {
        anyhow::bail!("seed adapter has no transport")
    }
    fn reconcile(&mut self, _: &StartEnvelope, _: Option<&EngineRef>) -> Result<ReconcileReply> {
        anyhow::bail!("seed adapter has no transport")
    }
}
fn seed(root: &Path, endpoint: &str, known: bool, saved: bool) -> Result<ApplicationConfig> {
    fs::create_dir_all(root.join("brain/records"))?;
    fs::create_dir(root.join("runtime"))?;
    fs::create_dir_all(root.join("config/tessera"))?;
    fs::write(root.join("token"), "synthetic-only-319")?;
    let config = || RunnerConfig {
        brain_id: id(1),
        root: root.join("brain"),
        operational_dir: root.join("runtime"),
        records_dir: "records".into(),
        boundary: WriteBoundary::Managed,
    };
    let mut runner = Runner::open(config())?;
    let settings = T3Settings {
        base_url: endpoint.into(),
        token_env: format!("file:{}", root.join("token").display()),
        environment_id: "synthetic-old-environment".into(),
        project_id: "synthetic-project".into(),
        model_instance_id: "synthetic-provider".into(),
        model: "synthetic-model".into(),
        runtime_mode: "approval-required".into(),
        interaction_mode: "default".into(),
    };
    let app_config = ApplicationConfig {
        actor: "Synthetic acceptance".into(),
        chat: None,
        todoist: None,
        maestro: None,
        t3: Some(settings.clone()),
    };
    let (_app, _adapters) =
        Application::configure(app_config.clone(), &root.join("runtime"), &mut runner)?;
    runner.create_goal(
        Goal {
            id: id(2),
            title: "Preserved synthetic history".into(),
            status: "draft".into(),
            criteria: vec![Criterion {
                id: "C1".into(),
                description: "Review preserved result".into(),
                requires_human: false,
            }],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        },
        "# Preserved synthetic history".into(),
    )?;
    let revision = runner.goal_source()?.revision;
    runner.prepare_stage(
        Stage {
            id: id(10),
            goal_id: id(2),
            engine: "t3".into(),
            status: "ready".into(),
            criterion_ids: vec!["C1".into()],
            context_id: id(11),
            result_ids: vec![],
            extra: BTreeMap::new(),
        },
        ContextPacket {
            id: id(11),
            goal_id: id(2),
            stage_id: id(10),
            goal_revision: revision,
            goal: "Preserved synthetic history".into(),
            decisions: vec![],
            constraints: vec![],
            sources: vec![],
            previous_result_id: None,
            next_step: "Review".into(),
            extra: BTreeMap::new(),
        },
        id(12),
        BTreeMap::from([
            ("environment_id".into(), json!(settings.environment_id)),
            ("project_id".into(), json!(settings.project_id)),
            ("created_at".into(), json!(WHEN)),
        ]),
    )?;
    let envelope = runner.snapshot()?.dispatch.unwrap();
    let binding = runner.start(&mut Seed)?.binding.unwrap();
    let event = EngineEvent {
        operation_id: envelope.operation_id.clone(),
        engine_ref: binding,
        event_id: "synthetic-terminal".into(),
        stream_id: "synthetic-stream".into(),
        sequence: Some(1),
        cursor: Some("synthetic-cursor-1".into()),
        observed_at: WHEN.into(),
        payload: EventPayload::Outcome(Outcome {
            outcome: "succeeded".into(),
            summary: "Synthetic settled history, pending human review".into(),
            sources: vec![],
            evidence: vec![Evidence {
                id: "e1".into(),
                kind: "engine_response".into(),
                source: SourceRef {
                    uri: "fixture:synthetic-evidence".into(),
                    revision: None,
                    locator: None,
                },
                description: "Synthetic preserved evidence".into(),
                observed_at: WHEN.into(),
                status: "unverified".into(),
            }],
            verification: "unverified".into(),
            criterion_evaluations: vec![],
        }),
    };
    fs::write(
        root.join("runtime/t3-receipts")
            .join(format!("{}.json", envelope.operation_id)),
        serde_json::to_vec(
            &json!({"fingerprint":format!("{:x}",Sha256::digest(serde_json::to_vec(&envelope)?)),"event":event}),
        )?,
    )?;
    runner.ingest(event)?;
    // Model a legacy missing origin only within this new synthetic directory.
    // No invented URL is assigned to its old envelope, binding, or receipt.
    drop(runner);
    let state_path = root.join("runtime/state.json");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    if !known {
        state["application"]["provider_identity"] = serde_json::Value::Null;
    }
    fs::write(&state_path, serde_json::to_vec_pretty(&state)?)?;
    fs::write(
        root.join("app-config.json"),
        serde_json::to_vec(&app_config)?,
    )?;
    if saved {
        settings::save(&root.join("runtime"), &app_config, None)?;
    }
    Ok(app_config)
}

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

struct Fake {
    address: String,
    calls: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fake {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (observed, stopping) = (calls.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut prefix = [0; 4];
                if stream.peek(&mut prefix).is_err() {
                    continue;
                }
                if prefix == *b"POST" {
                    let mut input = BufReader::new(&mut stream);
                    let mut first = String::new();
                    input.read_line(&mut first).unwrap();
                    let mut headers = String::new();
                    loop {
                        let mut line = String::new();
                        input.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        headers.push_str(&line);
                    }
                    assert!(first.starts_with("POST /api/auth/websocket-ticket "));
                    assert!(headers
                        .to_lowercase()
                        .contains("authorization: bearer synthetic-only-319"));
                    let body = r#"{"ticket":"fixture-ticket"}"#;
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
                    continue;
                }
                let mut ws = tungstenite::accept(stream).unwrap();
                while let Ok(message) = ws.read() {
                    if !message.is_text() {
                        break;
                    }
                    let call: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                    if call["_tag"] != "Request" {
                        continue;
                    }
                    let tag = call["tag"].as_str().unwrap_or("missing");
                    observed.lock().unwrap().push(tag.to_owned());
                    let response = match tag {
                        "server.getConfig" => {
                            json!({"_tag":"Exit","requestId":call["id"],"exit":{"_tag":"Success","value":{"environment":{"environmentId":"synthetic-new-environment"},"providers":[{"instanceId":"synthetic-provider","models":[{"slug":"synthetic-model","name":"Synthetic"}]}]}}})
                        }
                        "orchestration.subscribeShell" => {
                            json!({"_tag":"Chunk","requestId":call["id"],"values":[{"kind":"snapshot","snapshot":{"projects":[{"id":"synthetic-project","title":"Synthetic"}]}},{"kind":"synchronized"}]})
                        }
                        _ => {
                            json!({"_tag":"Exit","requestId":call["id"],"exit":{"_tag":"Failure","cause":"fixture_forbids_work"}})
                        }
                    };
                    ws.send(tungstenite::Message::Text(response.to_string().into()))
                        .unwrap();
                }
            }
        });
        Self {
            address,
            calls,
            stop,
            worker: Some(worker),
        }
    }
    fn url(&self) -> String {
        format!("http://{}", self.address)
    }
    fn assert_discovery_only(&self) {
        let calls = self.calls.lock().unwrap();
        assert!(
            calls.iter().any(|x| x == "server.getConfig"),
            "discovery did not fire"
        );
        assert!(calls.iter().any(|x| x == "orchestration.subscribeShell"));
        assert!(
            calls.iter().all(|x| matches!(
                x.as_str(),
                "server.getConfig" | "orchestration.subscribeShell"
            )),
            "unexpected provider work: {calls:?}"
        );
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Server {
    child: Child,
    address: String,
    workspace: Value,
}
impl Server {
    fn start(root: &Path, saved: bool) -> Self {
        let mut cmd = Command::new(
            std::env::var_os("TESSERA_CORED_TEST_BINARY")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_tessera-cored").into()),
        );
        cmd.args([
            "brain",
            "--brain-id",
            &id(1),
            "--listen",
            "127.0.0.1:0",
            "--records-dir",
            "records",
            "--managed-brain",
        ])
        .arg("--vault")
        .arg(root.join("brain"))
        .arg("--operational-dir")
        .arg(root.join("runtime"));
        if !saved {
            cmd.arg("--config").arg(root.join("app-config.json"));
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let ready: Value = serde_json::from_str(&line).expect("real cored ready");
        let mut server = Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            workspace: Value::Null,
        };
        server.workspace = server
            .call(json!({"schema":"ai-brain/v1","id":"cap","op":"capabilities"}))["data"]
            ["workspace"]
            .clone();
        server
    }
    fn call(&self, request: Value) -> Value {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], request["id"]);
        assert_eq!(response["schema"], request["schema"]);
        response
    }
    fn guarded(&self, mut body: Value) -> Value {
        body["schema"] = json!("ai-brain/workspace-v1");
        body["id"] = json!("guarded-test");
        body["expected_workspace"] = self.workspace.clone();
        self.call(body)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn journal(root: &Path) -> Value {
    let value: Value =
        serde_json::from_slice(&fs::read(root.join("runtime/state.json")).unwrap()).unwrap();
    if value["schema"] == "tessera-runtime/routes-v2" {
        value["state"].clone()
    } else {
        value
    }
}
fn checkpoint(root: &Path, name: &str) -> Value {
    let state = journal(root);
    fs::create_dir_all(root.join("checkpoints")).unwrap();
    fs::write(
        root.join("checkpoints").join(format!("{name}.json")),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
    state
}
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_string_lossy().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
fn history(root: &Path, baseline: &Value, immutable: &BTreeMap<String, Vec<u8>>) {
    let state = journal(root);
    for key in [
        "dispatch",
        "events",
        "previous_stages",
        "goal_id",
        "stage_id",
        "other_goals",
    ] {
        assert_eq!(state[key], baseline[key], "history changed: {key}");
    }
    assert_eq!(
        state["application"]["provider_identity"], baseline["application"]["provider_identity"],
        "historical identity invented"
    );
    for (path, bytes) in immutable {
        assert_eq!(
            &fs::read(root.join(path)).unwrap(),
            bytes,
            "immutable file changed: {path}"
        );
    }
}
fn lifecycle(known: bool, saved: bool) {
    let fake = Fake::new();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let config = seed(root, &fake.url(), known, saved).unwrap();
    let baseline = checkpoint(root, "before-start");
    let immutable: BTreeMap<_, _> = files(root)
        .into_iter()
        .filter(|(p, _)| p.starts_with("brain/") || p.starts_with("runtime/t3-receipts/"))
        .collect();
    let server = Server::start(root, saved);
    checkpoint(root, "after-start");
    history(root, &baseline, &immutable);
    let snapshot = server.guarded(json!({"op":"snapshot"}));
    assert_eq!(snapshot["ok"], true, "{snapshot}");
    assert_eq!(
        snapshot["data"]["thread_url"].is_string(),
        known,
        "{snapshot}"
    );
    if !known {
        assert!(
            !snapshot.to_string().contains(&format!("{}/", fake.url())),
            "invented historical URL: {snapshot}"
        );
    }
    if !known {
        for op in ["poll", "reconcile"] {
            let refused = server.guarded(json!({"op":op}));
            assert_eq!(refused["ok"], false, "{refused}");
            assert!(
                refused
                    .to_string()
                    .contains("historical_origin_unavailable"),
                "{refused}"
            );
        }
        history(root, &baseline, &immutable);
    }
    let reconnected = server.guarded(json!({"op":"connectors_reconnect"}));
    assert_eq!(reconnected["ok"], true, "{reconnected}");
    checkpoint(root, "after-reconnect");
    history(root, &baseline, &immutable);
    let mut candidate = serde_json::to_value(config.t3.unwrap()).unwrap();
    candidate["environment_id"] = json!("synthetic-new-environment");
    let prepared = server.guarded(json!({"op":"t3_target_prepare","candidate":candidate}));
    assert_eq!(prepared["ok"], true, "{prepared}");
    checkpoint(root, "after-prepare");
    let review = prepared["data"].clone();
    assert_eq!(review["ready"], true, "{review}");
    history(root, &baseline, &immutable);
    let kind = if known {
        "known_route"
    } else {
        "historical_terminal_unroutable"
    };
    assert_eq!(
        review["associations"][0]["association"]["kind"], kind,
        "{review}"
    );
    let request = json!({"operation_id":id(99),"review":review});
    let adopted = server.guarded(json!({"op":"t3_target_adopt","request":request}));
    assert_eq!(adopted["data"]["status"], "committed", "{adopted}");
    checkpoint(root, "after-adopt");
    history(root, &baseline, &immutable);
    let routes = journal(root)["t3_routes"].clone();
    let selected = routes["active"].clone();
    assert_eq!(adopted["data"]["receipt"]["generation_id"], selected);
    assert_eq!(
        routes["generations"][selected.as_str().unwrap()]["settings"],
        candidate
    );
    assert_eq!(routes["transitions"][id(99)]["request"], request);
    let mut durable_receipt = adopted["data"]["receipt"].clone();
    durable_receipt.as_object_mut().unwrap().remove("replayed");
    assert_eq!(routes["transitions"][id(99)]["receipt"], durable_receipt);
    assert_eq!(
        journal(root)["t3_routes"]["transitions"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    drop(server);
    let server = Server::start(root, saved);
    checkpoint(root, "after-restart");
    history(root, &baseline, &immutable);
    assert_eq!(journal(root)["t3_routes"]["active"], selected);
    let current = server.guarded(json!({"op":"t3_target_get"}));
    assert_eq!(current["data"]["active_generation"], selected);
    assert_eq!(current["data"]["active"], candidate);
    assert_eq!(
        current["data"]["historical_terminal_unroutable_count"],
        if known { 0 } else { 1 }
    );
    let snapshot = server.guarded(json!({"op":"snapshot"}));
    assert_eq!(
        snapshot["data"]["thread_url"].is_string(),
        known,
        "{snapshot}"
    );
    if !known {
        assert!(
            !snapshot.to_string().contains(&fake.url()),
            "invented historical URL: {snapshot}"
        );
    }
    if !known {
        for op in ["poll", "reconcile"] {
            let refused = server.guarded(json!({"op":op}));
            assert_eq!(refused["ok"], false, "{refused}");
            assert!(
                refused
                    .to_string()
                    .contains("historical_operation_nonreplayable"),
                "{refused}"
            );
        }
    }
    let replay = server.guarded(json!({"op":"t3_target_adopt","request":request}));
    assert_eq!(replay["data"]["status"], "committed", "{replay}");
    assert_eq!(
        journal(root)["t3_routes"]["transitions"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    history(root, &baseline, &immutable);
    fake.assert_discovery_only();
}
#[test]
fn null_origin_survives_actual_config_cli() {
    lifecycle(false, false);
}
#[test]
fn null_origin_survives_actual_saved_settings_cli() {
    lifecycle(false, true);
}
#[test]
fn known_origin_survives_actual_config_cli() {
    lifecycle(true, false);
}
#[test]
fn known_origin_survives_actual_saved_settings_cli() {
    lifecycle(true, true);
}
#[test]
fn forbidden_provider_call_detector_has_real_socket_positive_control() {
    let fake = Fake::new();
    let (mut socket, _) = tungstenite::connect(format!("ws://{}/ws", fake.address)).unwrap();
    socket.send(tungstenite::Message::Text(json!({"_tag":"Request","id":"control","tag":"orchestration.dispatchCommand","payload":{"type":"thread.turn.start"}}).to_string().into())).unwrap();
    let response: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(response["exit"]["_tag"], "Failure");
    assert_eq!(
        *fake.calls.lock().unwrap(),
        vec!["orchestration.dispatchCommand"]
    );
    drop(socket);
}
