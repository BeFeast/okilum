use super::*;
use std::fs;
use tessera_core::source::WriteBoundary;
use uuid::Uuid;
const TOKEN: &str = "synthetic-test-credential-only";
struct Fixture {
    temp: tempfile::TempDir,
    backend: Arc<Mutex<Backend>>,
    listener: crate::connector::Listener,
    workspace: Value,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        let state = temp.path().join("state");
        fs::create_dir_all(root.join("records")).unwrap();
        fs::create_dir(&state).unwrap();
        let brain = Uuid::new_v4().to_string();
        let config = || RunnerConfig {
            brain_id: brain.clone(),
            root: root.clone(),
            operational_dir: state.clone(),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let mut runner = Runner::open(config()).unwrap();
        let goal = Uuid::new_v4().to_string();
        runner
            .create_goal(
                Goal {
                    id: goal.clone(),
                    title: "Mobile decision fixture".into(),
                    status: "draft".into(),
                    criteria: vec![Criterion {
                        id: "C1".into(),
                        description: "Human must verify".into(),
                        requires_human: true,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: BTreeMap::new(),
                },
                "Original goal\r\n".into(),
            )
            .unwrap();
        drop(runner);
        let path = state.join("state.json");
        let mut journal: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        journal["attention"] =
            json!([{"id":"fixture-decision","kind":"decision","message":"Please make a decision"}]);
        fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
        let runner = Runner::open(config()).unwrap();
        let workspace = runner.workspace_identity();
        let backend = Arc::new(Mutex::new(Backend {
            runner,
            adapters: Adapters::new(),
            app: Application::unconfigured(),
            exports: crate::export::ExportDownloads::default(),
            todoist_picker: Default::default(),
            index: None,
            index_error: None,
            context_jobs: crate::context_jobs::Jobs::open(&state).unwrap(),
        }));
        fs::write(state.join("token"), format!("{TOKEN}\n")).unwrap();
        let settings = json!({"schema":"ai-brain/connector-config-v1","listen":"127.0.0.1:0","connector_id":"test-connector","token_file":state.join("token"),"workspace":workspace,"instance_id":"test-instance","account_id":"test-bot","actor_id":"operator","sender_id":"123","routes":[{"chat_id":"123","topic_id":null},{"chat_id":"-456","topic_id":"7"}]});
        let path = state.join("connector.json");
        fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
        let listener = crate::connector::Listener::bind(&path, &workspace).unwrap();
        Self {
            temp,
            backend,
            listener,
            workspace,
        }
    }
    fn reopen(self) -> Self {
        let Self {
            temp,
            backend,
            listener,
            workspace,
        } = self;
        let backend = Arc::try_unwrap(backend).ok().unwrap().into_inner().unwrap();
        drop(backend);
        let state = temp.path().join("state");
        let runner = Runner::open(RunnerConfig {
            brain_id: workspace["brain_id"].as_str().unwrap().into(),
            root: temp.path().join("brain"),
            operational_dir: state.clone(),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        })
        .unwrap();
        let backend = Arc::new(Mutex::new(Backend {
            runner,
            adapters: Adapters::new(),
            app: Application::unconfigured(),
            exports: crate::export::ExportDownloads::default(),
            todoist_picker: Default::default(),
            index: None,
            index_error: None,
            context_jobs: crate::context_jobs::Jobs::open(&state).unwrap(),
        }));
        Self {
            temp,
            backend,
            listener,
            workspace,
        }
    }
    fn request(&self, command: Value) -> Value {
        json!({"schema":connector::SCHEMA,"id":Uuid::new_v4().to_string(),"expected_workspace":self.workspace,"connector":{"id":"test-connector","token":TOKEN,"policy_fingerprint":self.listener.config.policy_fingerprint()},"telegram":{"sender_id":"123","chat_id":"123","topic_id":null,"message_id":"456","update_id":Uuid::new_v4().to_string()},"command":command})
    }
    fn call(&self, request: &Value) -> Value {
        handle(&request.to_string(), &self.backend, &self.listener.config)
    }
    fn command(&self, command: Value) -> Value {
        self.call(&self.request(command))
    }
    fn state(&self) -> Vec<u8> {
        fs::read(self.temp.path().join("state/state.json")).unwrap()
    }
    fn files(&self) -> Vec<(String, Vec<u8>)> {
        let mut entries = fs::read_dir(self.temp.path().join("brain/records"))
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().to_str().unwrap().to_owned(),
                    fs::read(e.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }
    fn item(&self) -> Value {
        let response = self.command(json!({"op":"attention_list"}));
        assert_eq!(response["ok"], true, "{response}");
        response["data"]["items"][0].clone()
    }
    fn mutation(&self, item: &Value, op: &str) -> Value {
        let mut command = json!({"op":op,"operation_id":Uuid::new_v4().to_string(),"goal_id":item["goal_id"],"attention_id":item["attention_id"],"expected_revision":item["revision"],"stage_id":item["stage_id"]});
        if op == "attention_reply" {
            command["text"] = json!("Exact reply\r\n  no final newline");
        }
        self.request(command)
    }
}
#[test]
fn authenticated_socket_capture_works_without_providers_and_preserves_source_and_replay() {
    let f = Fixture::new();
    let backend = f.backend.clone();
    let policy_fingerprint = f.listener.config.policy_fingerprint().to_owned();
    let socket = f.listener.socket;
    let config = Arc::new(f.listener.config);
    let address = socket.local_addr().unwrap();
    let request = json!({"schema":connector::SCHEMA,"id":"correlated-request","expected_workspace":f.workspace,"connector":{"id":"test-connector","token":TOKEN,"policy_fingerprint":policy_fingerprint},"telegram":{"sender_id":"123","chat_id":"-456","topic_id":"7","message_id":"456","update_id":"789"},"command":{"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"  exact capture\r\nwithout newline"}});
    let worker = std::thread::spawn(move || {
        let (stream, _) = socket.accept().unwrap();
        connection(stream, backend, config).unwrap();
    });
    let mut stream = TcpStream::connect(address).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut responses = Vec::new();
    for _ in 0..2 {
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        responses.push(serde_json::from_str::<Value>(&line).unwrap());
    }
    assert_eq!(responses[0]["ok"], true, "{}", responses[0]);
    assert_eq!(responses[0]["id"], "correlated-request");
    assert_eq!(responses[1]["data"]["receipt"]["replayed"], true);
    assert_eq!(
        responses[0]["data"]["capture_id"],
        responses[1]["data"]["capture_id"]
    );
    let source = fs::read_to_string(
        f.temp
            .path()
            .join("brain")
            .join(responses[0]["data"]["path"].as_str().unwrap()),
    )
    .unwrap();
    assert!(source.ends_with("  exact capture\r\nwithout newline"));
    assert!(source.contains("telegram"));
    assert!(source.contains("test-bot"));
    assert!(!source.contains(TOKEN));
    drop(reader);
    drop(stream);
    worker.join().unwrap();
}
#[test]
fn authenticated_reply_and_seen_ack_use_exact_target_and_leave_native_seen_and_workflow_unchanged()
{
    let f = Fixture::new();
    let item = f.item();
    assert_eq!(item["stage_id"], Value::Null);
    assert_eq!(item["channel"], "telegram");
    let before = f.backend.lock().unwrap().runner.snapshot().unwrap();
    let request = f.mutation(&item, "attention_reply");
    let response = f.call(&request);
    assert_eq!(response["ok"], true, "{response}");
    let path = response["data"]["path"].as_str().unwrap();
    let source = fs::read_to_string(f.temp.path().join("brain").join(path)).unwrap();
    assert!(source.ends_with("Exact reply\r\n  no final newline"));
    assert!(source.contains("unverified"));
    assert!(source.contains("telegram"));
    assert!(!source.contains(TOKEN));
    let replay = f.call(&request);
    assert_eq!(replay["data"]["receipt"]["replayed"], true);
    assert_eq!(
        replay["data"]["decision_id"],
        response["data"]["decision_id"]
    );
    let ack = f.call(&f.mutation(&item, "attention_ack"));
    assert_eq!(ack["ok"], true, "{ack}");
    assert_eq!(f.item()["seen"], true);
    let native = dispatch(
        &f.backend,
        Command::AttentionList {
            channel: None,
            limit: None,
            cursor: None,
        },
        Some(f.workspace.clone()),
    )
    .unwrap();
    assert_eq!(native["items"][0]["seen"], false);
    let after = f.backend.lock().unwrap().runner.snapshot().unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}
#[test]
fn wrong_or_missing_authority_and_native_fallback_attempt_never_mutate_state() {
    let f = Fixture::new();
    let baseline = f.state();
    let files = f.files();
    let valid=f.request(json!({"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"must not exist"}));
    for case in [
        "missing_auth",
        "missing_token",
        "missing_policy",
        "wrong_policy",
        "wrong_token",
        "wrong_connector",
        "wrong_sender",
        "wrong_chat",
        "wrong_topic",
        "wrong_brain",
        "wrong_schema",
        "native_shape",
    ] {
        let mut request = valid.clone();
        match case {
            "missing_auth" => {
                request.as_object_mut().unwrap().remove("connector");
            }
            "missing_token" => {
                request["connector"]
                    .as_object_mut()
                    .unwrap()
                    .remove("token");
            }
            "missing_policy" => {
                request["connector"]
                    .as_object_mut()
                    .unwrap()
                    .remove("policy_fingerprint");
            }
            "wrong_policy" => request["connector"]["policy_fingerprint"] = json!("outdated-policy"),
            "wrong_token" => request["connector"]["token"] = json!("incorrect"),
            "wrong_connector" => request["connector"]["id"] = json!("other"),
            "wrong_sender" => request["telegram"]["sender_id"] = json!("999"),
            "wrong_chat" => request["telegram"]["chat_id"] = json!("999"),
            "wrong_topic" => request["telegram"]["topic_id"] = json!("7"),
            "wrong_brain" => {
                request["expected_workspace"]["brain_id"] = json!(Uuid::new_v4().to_string())
            }
            "wrong_schema" => request["schema"] = json!("ai-brain/workspace-v1"),
            "native_shape" => {
                request = json!({"schema":"ai-brain/workspace-v1","id":"native-attempt","expected_workspace":f.workspace,"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"native fallback attempt"});
            }
            _ => unreachable!(),
        }
        let response = f.call(&request);
        assert_eq!(response["ok"], false, "{case}: {response}");
        assert_eq!(f.state(), baseline, "{case}");
        assert_eq!(f.files(), files, "{case}");
        assert!(!response.to_string().contains(TOKEN));
    }
    assert_eq!(f.call(&valid)["ok"], true, "permitted positive control");
}
#[test]
fn forbidden_commands_and_source_channel_overrides_are_rejected_before_observation_or_write() {
    let f = Fixture::new();
    let state = f.state();
    let files = f.files();
    for op in [
        "start",
        "stage_prepare",
        "stage_revise",
        "cancel",
        "retry",
        "source_write",
        "criterion_evaluate",
        "task_create",
        "task_complete",
        "connectors_save",
        "export_prepare",
        "workspace_attention",
        "snapshot",
    ] {
        let response = f.command(json!({"op":op}));
        assert_eq!(
            response["error"]["code"], "connector_operation_forbidden",
            "{response}"
        );
        assert_eq!(f.state(), state);
        assert_eq!(f.files(), files);
    }
    for (op, field) in [
        ("attention_list", "channel"),
        ("inbox_capture", "source"),
        ("capabilities", "actor"),
    ] {
        let mut command = json!({"op":op});
        command[field] = json!("native");
        let response = f.command(command);
        assert_eq!(
            response["error"]["code"], "connector_invalid_request",
            "{response}"
        );
        assert_eq!(f.state(), state);
    }
}
#[test]
fn stale_and_wrong_goal_or_stage_replies_retain_original_request_and_do_not_create_decisions() {
    let f = Fixture::new();
    let item = f.item();
    let state = f.state();
    let files = f.files();
    for case in ["missing_stage", "wrong_stage", "wrong_goal", "old_revision"] {
        let mut request = f.mutation(&item, "attention_reply");
        match case {
            "missing_stage" => {
                request["command"]
                    .as_object_mut()
                    .unwrap()
                    .remove("stage_id");
            }
            "wrong_stage" => request["command"]["stage_id"] = json!(Uuid::new_v4().to_string()),
            "wrong_goal" => request["command"]["goal_id"] = json!(Uuid::new_v4().to_string()),
            "old_revision" => {
                request["command"]["expected_revision"] =
                    json!(format!("sha256:{}", "0".repeat(64)))
            }
            _ => unreachable!(),
        }
        let retained = request.clone();
        let response = f.call(&request);
        assert_eq!(response["ok"], false, "{case}: {response}");
        assert_eq!(request, retained);
        assert_eq!(f.files(), files);
        assert_eq!(f.state(), state);
        if case != "missing_stage" {
            assert_eq!(response["error"]["code"], "attention_stale");
            assert!(response["error"].get("current").is_some());
        }
    }
}
#[test]
fn capability_projection_is_scoped_and_missing_mutation_message_identity_is_rejected() {
    let f = Fixture::new();
    let mut request = f.request(json!({"op":"capabilities"}));
    request["telegram"]
        .as_object_mut()
        .unwrap()
        .remove("message_id");
    request["telegram"]
        .as_object_mut()
        .unwrap()
        .remove("update_id");
    let response = f.call(&request);
    assert_eq!(response["ok"], true);
    assert_eq!(response["data"]["workspace"], f.workspace);
    assert_eq!(response["data"]["channel"], "telegram");
    assert!(response["data"].get("source_write").is_none());
    assert!(response["data"].get("connections").is_none());
    assert!(response["data"].get("t3").is_none());
    let before = f.state();
    request["command"] = json!({"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"missing identity"});
    assert_eq!(
        f.call(&request)["error"]["code"],
        "connector_invalid_request"
    );
    assert_eq!(f.state(), before);
}
#[test]
fn invalid_config_never_binds_nonloopback_wrong_workspace_or_in_brain_credentials() {
    let f = Fixture::new();
    let path = f.temp.path().join("state/connector.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for case in [
        "public_listener",
        "other_workspace",
        "no_routes",
        "token_in_brain",
        "missing_token",
        "missing_nullable_topic",
    ] {
        let mut value = original.clone();
        match case {
            "public_listener" => value["listen"] = json!("0.0.0.0:0"),
            "other_workspace" => value["workspace"]["brain_id"] = json!(Uuid::new_v4().to_string()),
            "no_routes" => value["routes"] = json!([]),
            "token_in_brain" => {
                let token = f.temp.path().join("brain/credential");
                fs::write(&token, TOKEN).unwrap();
                value["token_file"] = json!(token);
            }
            "missing_token" => value["token_file"] = json!(f.temp.path().join("absent")),
            "missing_nullable_topic" => {
                value["routes"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("topic_id");
            }
            _ => unreachable!(),
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            crate::connector::Listener::bind(&path, &f.workspace).is_err(),
            "{case}"
        );
    }
}

#[test]
fn lost_capture_and_reply_responses_replay_after_backend_restart_without_new_canonical_records() {
    let f = Fixture::new();
    let capture=f.request(json!({"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"Lost response thought"}));
    let first = f.call(&capture);
    assert_eq!(first["ok"], true);
    let f = f.reopen();
    let again = f.call(&capture);
    assert_eq!(again["data"]["capture_id"], first["data"]["capture_id"]);
    assert_eq!(again["data"]["receipt"]["replayed"], true);
    let item = f.item();
    let reply = f.mutation(&item, "attention_reply");
    let first = f.call(&reply);
    assert_eq!(first["ok"], true);
    let files = f.files();
    let f = f.reopen();
    let again = f.call(&reply);
    assert_eq!(again["data"]["decision_id"], first["data"]["decision_id"]);
    assert_eq!(again["data"]["receipt"]["replayed"], true);
    assert_eq!(f.files(), files);
    let mut changed = reply.clone();
    changed["command"]["text"] = json!("Different body under original update");
    assert_eq!(
        f.call(&changed)["error"]["code"],
        "attention_identity_conflict"
    );
    assert_eq!(f.files(), files);
}
#[test]
fn native_entrypoint_does_not_acquire_connector_authority_after_listener_is_configured() {
    let f = Fixture::new();
    let request=f.request(json!({"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"Scoped source"}));
    let typed: Request = serde_json::from_value(request.clone()).unwrap();
    let context = f.listener.config.authorize(&typed, &f.workspace).unwrap();
    let state = f.state();
    let files = f.files();
    let native = dispatch(
        &f.backend,
        Command::InboxCapture {
            request: crate::inbox::CaptureRequest {
                operation_id: Uuid::new_v4().to_string(),
                text: "Native cannot borrow connector".into(),
                source: context.source().unwrap(),
            },
        },
        Some(f.workspace.clone()),
    );
    assert!(native.is_err());
    assert_eq!(f.state(), state);
    assert_eq!(f.files(), files);
    let native_read = dispatch(
        &f.backend,
        Command::AttentionList {
            channel: Some("telegram".into()),
            limit: None,
            cursor: None,
        },
        Some(f.workspace.clone()),
    );
    assert!(native_read.is_err());
    assert_eq!(f.state(), state);
    assert_eq!(f.call(&request)["ok"], true);
}

#[test]
fn server_only_policy_change_rejects_unsent_and_replayed_requests_before_observation_or_mutation() {
    let f = Fixture::new();
    let sent=f.request(json!({"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"Committed under original authority"}));
    assert_eq!(f.call(&sent)["ok"], true);
    let pending=f.request(json!({"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"Not yet sent"}));
    let observation = f.request(json!({"op":"attention_list"}));
    let state = f.state();
    let files = f.files();
    let path = f.temp.path().join("state/connector.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for field in ["account_id", "instance_id", "actor_id", "routes"] {
        let mut changed = original.clone();
        if field == "routes" {
            changed[field]
                .as_array_mut()
                .unwrap()
                .push(json!({"chat_id":"999","topic_id":null}));
        } else {
            changed[field] = json!("different-authority");
        }
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        let listener = crate::connector::Listener::bind(&path, &f.workspace).unwrap();
        for request in [&sent, &pending, &observation] {
            let response = handle(&request.to_string(), &f.backend, &listener.config);
            assert_eq!(
                response["error"]["code"], "connector_policy_mismatch",
                "{field}: {response}"
            );
            assert_eq!(f.state(), state);
            assert_eq!(f.files(), files);
        }
        let mut current = observation.clone();
        current["command"] = json!({"op":"capabilities"});
        current["connector"]["policy_fingerprint"] = json!(listener.config.policy_fingerprint());
        assert_eq!(
            handle(&current.to_string(), &f.backend, &listener.config)["ok"],
            true,
            "current policy positive control"
        );
        assert_eq!(f.state(), state);
        assert_eq!(f.files(), files);
    }
}

#[test]
fn credential_rotation_preserves_policy_and_original_receipt_identity() {
    let f = Fixture::new();
    let mut request=f.request(json!({"op":"inbox_capture","operation_id":Uuid::new_v4().to_string(),"text":"Retain across credential rotation"}));
    let first = f.call(&request);
    assert_eq!(first["ok"], true);
    let files = f.files();
    let state = f.state();
    fs::write(
        f.temp.path().join("state/token"),
        "rotated-synthetic-credential",
    )
    .unwrap();
    let listener =
        crate::connector::Listener::bind(&f.temp.path().join("state/connector.json"), &f.workspace)
            .unwrap();
    assert_eq!(
        listener.config.policy_fingerprint(),
        f.listener.config.policy_fingerprint()
    );
    assert_eq!(
        handle(&request.to_string(), &f.backend, &listener.config)["error"]["code"],
        "connector_unauthorized"
    );
    assert_eq!(f.state(), state);
    assert_eq!(f.files(), files);
    request["connector"]["token"] = json!("rotated-synthetic-credential");
    let replay = handle(&request.to_string(), &f.backend, &listener.config);
    assert_eq!(replay["data"]["capture_id"], first["data"]["capture_id"]);
    assert_eq!(replay["data"]["receipt"]["replayed"], true);
    assert_eq!(f.files(), files);
}
