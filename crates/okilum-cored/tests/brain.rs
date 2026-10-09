//! A real backend child survives client disconnect, and the next process reads
//! the same durable goal/source state. No installed server or personal vault.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

struct Server {
    child: Child,
    address: String,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn start(root: &Path, runtime: &Path) -> Server {
    start_config(root, runtime, None)
}
fn start_config(root: &Path, runtime: &Path, config: Option<&Path>) -> Server {
    let mut command = Command::new(env!("CARGO_BIN_EXE_okilum-cored"));
    if let Some(path) = config {
        command
            .env("OKILUM_FIXTURE_TOKEN", "fixture-token-only")
            .arg("brain")
            .arg("--config")
            .arg(path);
    } else {
        command.arg("brain");
    }
    let mut child = command
        .args([
            "--brain-id",
            "01000000-0000-4000-8000-000000000001",
            "--listen",
            "127.0.0.1:0",
            "--records-dir",
            "records",
            "--managed-brain",
        ])
        .arg("--vault")
        .arg(root)
        .arg("--operational-dir")
        .arg(runtime)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let data: Value = serde_json::from_str(&ready).expect("backend ready JSON");
    Server {
        child,
        address: data["listen"].as_str().unwrap().to_owned(),
    }
}
fn call(address: &str, body: Value) -> Value {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "{body}").unwrap();
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).unwrap();
    serde_json::from_str(&reply).unwrap()
}
fn request(op: &str) -> Value {
    json!({"schema":"ai-brain/v1","id":1,"op":op})
}

#[test]
fn client_disconnect_and_backend_restart_preserve_goal_and_raw_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let server = start(&root, &runtime);
    let mut create = request("create_goal");
    create["goal"] = json!({"id":"02000000-0000-4000-8000-000000000001","title":"Persistent test goal","status":"draft","criteria":[{"id":"C1","description":"Saved evidence","requires_human":false}],"stage_ids":[],"task_ref":null});
    create["body"] = json!("\n# Test goal\n");
    let created = call(&server.address, create);
    assert_eq!(created["ok"], true, "{created}");
    // call() drops its connection. A new client sees the same running backend.
    let snapshot = call(&server.address, request("snapshot"));
    assert_eq!(snapshot["data"]["goal"]["title"], "Persistent test goal");
    let source = call(&server.address, request("goal_source"));
    assert_eq!(source["ok"], true);
    let mut write = request("source_write");
    write["request"] = json!({"schema":"ai-brain/v1","brain_id":"01000000-0000-4000-8000-000000000001","operation_id":"03000000-0000-4000-8000-000000000001","path":"scratch.md","expected_revision":null,"content_base64":"aGVsbG8="});
    assert_eq!(call(&server.address, write.clone())["ok"], true);
    write["request"]["operation_id"] = json!("03000000-0000-4000-8000-000000000002");
    let conflict = call(&server.address, write);
    assert_eq!(conflict["ok"], false);
    assert_eq!(conflict["error"]["code"], "conflict");
    assert!(conflict["error"]["conflict"]["conflict_id"].is_string());
    drop(server);
    let server = start(&root, &runtime);
    let snapshot = call(&server.address, request("snapshot"));
    assert_eq!(snapshot["data"]["goal"]["title"], "Persistent test goal");
    let mut read = request("source_read");
    read["path"] = json!("scratch.md");
    assert_eq!(
        call(&server.address, read)["data"]["content_base64"],
        "aGVsbG8="
    );
    let mut wrong = request("snapshot");
    wrong["schema"] = json!("ai-brain/v0");
    assert_eq!(call(&server.address, wrong)["ok"], false);
}

fn wait_chat(address: &str, id: &Value, wanted: &str) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut get = request("chat_get");
        get["conversation_id"] = id.clone();
        let reply = call(address, get);
        assert_eq!(reply["ok"], true, "{reply}");
        if reply["data"]["status"] == wanted
            || (wanted == "partial"
                && reply["data"]["partial"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()))
        {
            return reply["data"].clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "chat did not reach {wanted}: {reply}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
#[test]
fn chat_job_survives_socket_disconnect_and_restart_retains_partial_without_retry() {
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::mpsc;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    std::fs::write(
        root.join("source.md"),
        "# Source\nThe selected source bytes.\n",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = temp.path().join("providers.json");
    std::fs::write(&config,json!({"actor":"fixture operator","chat":{"base_url":format!("http://{}/v1",listener.local_addr().unwrap()),"model":"fixture-model","api_key_env":"OKILUM_FIXTURE_TOKEN"},"todoist":null,"t3":null}).to_string()).unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let http = std::thread::spawn(move || {
        let mut captured = Vec::new();
        for index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            let len = headers
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|s| s.parse::<usize>().ok())
                })
                .unwrap();
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            captured.push(serde_json::from_slice::<Value>(&body).unwrap());
            let delta="data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Prepared context\"},\"finish_reason\":null}]}\n\n";
            let terminal="data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",delta.len()+terminal.len(),delta).unwrap();
            stream.flush().unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            if index == 0 {
                stream.write_all(terminal.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        }
        captured
    });
    let server = start_config(&root, &runtime, Some(&config));
    let primary = "02000000-0000-4000-8000-000000000010";
    let mut create_primary = request("create_goal");
    create_primary["goal"] = json!({"id":primary,"title":"Unrelated primary goal","status":"draft","criteria":[{"id":"C1","description":"Other result","requires_human":false}],"stage_ids":[],"task_ref":null});
    create_primary["body"] = json!("# Other goal");
    assert_eq!(call(&server.address, create_primary)["ok"], true);
    let goal = "02000000-0000-4000-8000-000000000011";
    let mut create = request("create_goal");
    create["goal"] = json!({"id":goal,"title":"Prepare context","status":"draft","criteria":[{"id":"C1","description":"Sourced explanation","requires_human":false}],"stage_ids":[],"task_ref":null});
    create["body"] = json!("\n# Prepare context\n");
    assert_eq!(call(&server.address, create)["ok"], true);
    let listed = call(&server.address, request("source_list"));
    assert!(listed["data"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["path"] == "source.md" && s["title"] == "Source"));
    let mut start = request("chat_start");
    start["goal_id"] = json!(goal);
    start["message"] = json!("Read the selected source");
    start["source_paths"] = json!(["source.md"]);
    start["conversation_id"] = Value::Null;
    let reply = call(&server.address, start.clone());
    assert_eq!(reply["ok"], true, "{reply}");
    let id = reply["data"]["conversation_id"].clone();
    // Source selection and streaming do not monopolize the backend mutex.
    let partial = wait_chat(&server.address, &id, "partial");
    assert_eq!(partial["status"], "running");
    let primary_snapshot = call(&server.address, request("snapshot"));
    assert_eq!(primary_snapshot["data"]["goal"]["id"], primary);
    assert!(primary_snapshot["data"]["conversations"]
        .as_array()
        .unwrap()
        .is_empty());
    let mut selected = request("snapshot");
    selected["goal_id"] = json!(goal);
    let snapshot = call(&server.address, selected.clone());
    assert_eq!(snapshot["data"]["conversations"][0]["id"], id);
    assert_eq!(
        snapshot["data"]["selected_source_paths"],
        json!(["source.md"])
    );
    // A navigation read for the other goal cannot steal a late stream callback.
    assert_eq!(
        call(&server.address, request("snapshot"))["data"]["goal"]["id"],
        primary
    );
    release_tx.send(()).unwrap();
    let complete = wait_chat(&server.address, &id, "complete");
    assert_eq!(complete["messages"][1]["text"], "Prepared context");
    start["conversation_id"] = id.clone();
    start["message"] = json!("Continue using the previous answer");
    assert_eq!(call(&server.address, start)["ok"], true);
    wait_chat(&server.address, &id, "partial");
    drop(server);
    let server = start_config(&root, &runtime, Some(&config));
    let interrupted = wait_chat(&server.address, &id, "interrupted");
    assert_eq!(interrupted["goal_id"], goal);
    assert_eq!(
        call(&server.address, selected)["data"]["selected_source_paths"],
        json!(["source.md"])
    );
    assert!(
        call(&server.address, request("snapshot"))["data"]["conversations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(interrupted["partial"], "Prepared context");
    assert_eq!(interrupted["error"], "backend_restarted");
    release_tx.send(()).unwrap();
    let requests = http.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]
        .to_string()
        .contains("The selected source bytes."));
    assert!(requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "assistant" && m["content"] == "Prepared context"));
    let body = std::fs::read_to_string(root.join(interrupted["path"].as_str().unwrap())).unwrap();
    assert!(body.contains("## Assistant (partial)"));
    let journal = std::fs::read_to_string(runtime.join("state.json")).unwrap();
    assert!(!journal.contains("fixture-token-only"));
}

#[test]
fn source_conflict_api_preserves_versions_and_guards_resolution() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    std::fs::write(root.join("note.md"), "Choice: blue\n").unwrap();
    let server = start(&root, &runtime);
    let mut read = request("source_read");
    read["path"] = json!("note.md");
    let base = call(&server.address, read.clone())["data"].clone();
    let write = |id: &str, source: &Value, bytes: &str| {
        let mut r = request("source_write");
        r["request"] = json!({"schema":"ai-brain/v1", "brain_id":base["brain_id"], "operation_id":id, "path":"note.md", "expected_revision":source["revision"], "content_base64":STANDARD.encode(bytes)});
        r["base"] = source.clone();
        r
    };
    assert_eq!(
        call(
            &server.address,
            write(
                "03000000-0000-4000-8000-000000000021",
                &base,
                "Choice: red\n"
            )
        )["ok"],
        true
    );
    let rejected = call(
        &server.address,
        write(
            "03000000-0000-4000-8000-000000000022",
            &base,
            "Choice: green\n",
        ),
    );
    assert_eq!(rejected["error"]["code"], "conflict");
    let mut inspect = request("source_conflict");
    inspect["brain_id"] = base["brain_id"].clone();
    inspect["path"] = json!("note.md");
    inspect["conflict_id"] = rejected["error"]["conflict"]["conflict_id"].clone();
    drop(server);
    let server = start(&root, &runtime);
    let versions = call(&server.address, inspect.clone());
    assert_eq!(versions["ok"], true);
    assert_eq!(versions["data"]["base"], base);
    assert_eq!(
        versions["data"]["proposed"]["content_base64"],
        STANDARD.encode("Choice: green\n")
    );
    let current = versions["data"]["current"].clone();
    assert_eq!(current["content_base64"], STANDARD.encode("Choice: red\n"));
    assert_eq!(
        call(
            &server.address,
            write(
                "03000000-0000-4000-8000-000000000023",
                &current,
                "Choice: green, reviewed against red\n"
            )
        )["ok"],
        true
    );
    assert_eq!(
        call(&server.address, read)["data"]["content_base64"],
        STANDARD.encode("Choice: green, reviewed against red\n")
    );
    inspect["path"] = json!("other.md");
    assert_eq!(
        call(&server.address, inspect)["error"]["code"],
        "invalid_request"
    );
}

#[test]
fn workspace_identity_guard_rejects_wrong_root_before_reads_and_writes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let server = start(&root, &runtime);
    let capabilities = call(&server.address, request("capabilities"));
    assert_eq!(capabilities["data"]["workspace_guard"], true);
    let identity = capabilities["data"]["workspace"].clone();
    let mut read = request("source_list");
    read["schema"] = json!("ai-brain/workspace-v1");
    assert_eq!(call(&server.address, read.clone())["ok"], false);
    read["expected_workspace"] = identity.clone();
    assert_eq!(call(&server.address, read.clone())["ok"], true);
    read["expected_workspace"]["root"] = json!("/wrong/root");
    assert_eq!(call(&server.address, read)["ok"], false);
    let mut write = request("source_write");
    write["request"] = json!({"schema":"ai-brain/v1","brain_id":identity["brain_id"],"operation_id":"03000000-0000-4000-8000-000000000001","path":"guarded.md","expected_revision":null,"content_base64":"aGVsbG8="});
    write["expected_workspace"] = identity.clone();
    write["expected_workspace"]["root"] = json!("/wrong/root");
    assert_eq!(call(&server.address, write.clone())["ok"], false);
    assert!(!root.join("guarded.md").exists());
    write["expected_workspace"] = identity;
    assert_eq!(call(&server.address, write)["ok"], true);
    assert_eq!(std::fs::read(root.join("guarded.md")).unwrap(), b"hello");
}

#[test]
fn two_goals_reconcile_lost_task_ack_without_relinking_the_other_goal() {
    use std::{io::Read, net::TcpListener};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = temp.path().join("config.json");
    std::fs::write(&config,json!({"actor":"fixture operator","chat":null,"t3":null,
        "todoist":{"base_url":format!("http://{}/api/v1",http.local_addr().unwrap()),"instance_id":"fixture","token_env":"OKILUM_FIXTURE_TOKEN"}}).to_string()).unwrap();
    let operations = [
        "03000000-0000-4000-8000-000000000031",
        "03000000-0000-4000-8000-000000000032",
    ];
    let provider = std::thread::spawn(move || {
        for step in 0..5 {
            let (mut stream, _) = http.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut header = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                header.push_str(&line);
            }
            let len = header
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|v| v.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut bytes = vec![0; len];
            reader.read_exact(&mut bytes).unwrap();
            let body = if [0,1,3].contains(&step) {
                let encoded=String::from_utf8(bytes).unwrap();
                let op=operations[usize::from(step==1)];
                assert!(encoded.contains(op) && encoded.contains("item_add"));
                if step==0 {continue;} // accepted remotely; acknowledgement lost
                json!({"sync_status":{op:"ok"},"temp_id_mapping":{op:if step==1 {"second-task"}else{"first-task"}}})
            } else {
                let task=if step==2 {"second-task"}else{"first-task"};
                assert!(header.starts_with(&format!("GET /api/v1/tasks/{task} ")));
                json!({"id":task,"project_id":"inbox","content":task,"description":"","checked":false,"is_deleted":false,"labels":[],"priority":1,"due":null,"updated_at":"2026-09-05T12:00:00Z","completed_at":null})
            }.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let server = start_config(&root, &runtime, Some(&config));
    let goals = [
        "02000000-0000-4000-8000-000000000031",
        "02000000-0000-4000-8000-000000000032",
    ];
    for (index, goal) in goals.iter().enumerate() {
        let mut create = request("create_goal");
        create["goal"] = json!({"id":goal,"title":format!("Thought {index}"),"status":"draft","criteria":[{"id":"C1","description":"Own result","requires_human":false}],"stage_ids":[],"task_ref":null});
        create["body"] = json!("# Thought");
        let reply = call(&server.address, create);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["data"]["goal"]["id"], *goal);
        let mut task = request("task_create");
        task["goal_id"] = json!(goal);
        task["operation_id"] = json!(operations[index]);
        task["content"] = json!(format!("Task {index}"));
        let reply = call(&server.address, task);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(
            reply["data"]["status"],
            if index == 0 {
                "indeterminate"
            } else {
                "accepted"
            }
        );
    }
    drop(server);
    let server = start_config(&root, &runtime, Some(&config));
    let mut reconcile = request("task_reconcile");
    reconcile["operation_id"] = json!(operations[0]);
    let reply = call(&server.address, reconcile);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["data"]["task"]["task_id"], "first-task");
    for (index, goal) in goals.iter().enumerate() {
        let mut snapshot = request("snapshot");
        snapshot["goal_id"] = json!(goal);
        let reply = call(&server.address, snapshot);
        assert_eq!(reply["data"]["goal"]["id"], *goal);
        assert_eq!(
            reply["data"]["task"]["task_id"],
            if index == 0 {
                "first-task"
            } else {
                "second-task"
            }
        );
        assert!(reply["data"]["pending_task_operation_id"].is_null());
    }
    provider.join().unwrap();
}

#[test]
fn saved_connectors_refresh_credentials_and_refuse_a_different_todoist_account() {
    use std::net::TcpListener;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    std::fs::write(root.join("note.md"), "# Still readable\n").unwrap();
    let token = temp.path().join("token");
    // First setup is offline: the credential file is initially absent.
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", http.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        for _ in 0..5 {
            let (mut stream, _) = http.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(
                headers.starts_with("GET /user "),
                "Only the account identity read is allowed"
            );
            let (status, body) = if headers.contains("account-a-token") {
                ("200 OK", r#"{"id":"account-a"}"#)
            } else if headers.contains("account-b-token") {
                ("200 OK", r#"{"id":"account-b"}"#)
            } else {
                ("401 Unauthorized", r#"{}"#)
            };
            write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let server = start(&root, &runtime);
    let config = json!({"actor":"fixture operator","chat":null,"todoist":{"base_url":base,"instance_id":"fixture","token_env":format!("file:{}",token.display())},"t3":null});
    // Legacy settings without Maestro remain accepted; reads return the additive default.
    let mut expected_config = config.clone();
    expected_config["maestro"] = Value::Null;
    let mut save = request("connectors_save");
    save["config"] = config.clone();
    let saved = call(&server.address, save);
    assert_eq!(saved["ok"], true, "{saved}");
    assert_eq!(
        saved["data"]["states"]["todoist"]["status"],
        "authentication_required"
    );
    let bytes = std::fs::read(runtime.join("connector-settings.json")).unwrap();
    assert!(!String::from_utf8(bytes)
        .unwrap()
        .contains("account-a-token"));
    drop(server);
    std::fs::write(&token, "account-a-token\n").unwrap();
    let server = start(&root, &runtime);
    let pinned: Value =
        serde_json::from_slice(&std::fs::read(runtime.join("connector-settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        pinned["todoist_account_id"], "account-a",
        "startup must persist a newly authenticated account"
    );
    drop(server);
    std::fs::write(&token, "account-b-token\n").unwrap();
    let server = start(&root, &runtime);
    assert_eq!(
        call(&server.address, request("connectors_get"))["data"]["states"]["todoist"]["status"],
        "identity_mismatch"
    );
    assert_eq!(
        call(&server.address, request("connectors_get"))["data"]["config"],
        expected_config
    );
    std::fs::write(&token, "expired-token").unwrap();
    let expired = call(&server.address, request("connectors_reconnect"));
    assert_eq!(
        expired["data"]["states"]["todoist"]["status"], "authentication_required",
        "{expired}"
    );
    assert_eq!(
        call(&server.address, request("capabilities"))["data"]["todoist"],
        false
    );
    let mut read = request("source_read");
    read["path"] = json!("note.md");
    assert_eq!(call(&server.address, read)["ok"], true);
    std::fs::write(&token, "account-b-token").unwrap();
    let wrong = call(&server.address, request("connectors_reconnect"));
    assert_eq!(
        wrong["data"]["states"]["todoist"]["status"], "identity_mismatch",
        "{wrong}"
    );
    std::fs::write(&token, "account-a-token").unwrap();
    let renewed = call(&server.address, request("connectors_reconnect"));
    assert_eq!(
        renewed["data"]["states"]["todoist"]["status"], "reachable",
        "{renewed}"
    );
    assert_eq!(
        call(&server.address, request("capabilities"))["data"]["todoist"],
        true
    );
    worker.join().unwrap();
}

#[test]
fn waiting_provider_discovery_does_not_hold_source_access_or_save_configuration() {
    use std::net::TcpListener;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    std::fs::write(root.join("source.md"), "# Remains available").unwrap();
    let token = temp.path().join("token");
    std::fs::write(&token, "fixture-token").unwrap();
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", http.local_addr().unwrap());
    let (entered, receive) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = http.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        assert!(headers.starts_with("GET /v1/models "));
        entered.send(()).unwrap();
        wait.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let body = r#"{"data":[{"id":"fixture-model"}]}"#;
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    let server = start(&root, &runtime);
    let address = server.address.clone();
    let mut check = request("connectors_check");
    check["config"] = json!({"actor":"fixture","chat":{"base_url":base,"model":"unselected","api_key_env":format!("file:{}",token.display())},"todoist":null,"t3":null});
    let checking = std::thread::spawn(move || call(&address, check));
    receive
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let mut read = request("source_read");
    read["path"] = json!("source.md");
    let response = call(&server.address, read);
    assert_eq!(response["ok"], true);
    assert!(!runtime.join("connector-settings.json").exists());
    release.send(()).unwrap();
    let checked = checking.join().unwrap();
    assert_eq!(checked["data"]["states"]["chat"]["status"], "reachable");
    assert_eq!(
        checked["data"]["choices"]["chat_models"][0],
        "fixture-model"
    );
    worker.join().unwrap();
}

#[test]
fn guarded_export_download_extracts_to_client_without_dispatch_or_original_database() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("server-brain");
    let runtime = temp.path().join("server-runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    std::fs::write(
        root.join("note.md"),
        b"# Export test\n![](attachment.bin)\n",
    )
    .unwrap();
    let media: Vec<_> = (0..2_100_000).map(|i| (i % 251) as u8).collect();
    std::fs::write(root.join("attachment.bin"), &media).unwrap();
    let server = start(&root, &runtime);
    let identity = call(&server.address, request("capabilities"))["data"]["workspace"].clone();
    let guarded = |op: &str| json!({"schema":"ai-brain/workspace-v1","id":7,"op":op,"expected_workspace":identity});
    let mut wrong = guarded("export_prepare");
    wrong["expected_workspace"]["root"] = json!("/wrong");
    assert_eq!(call(&server.address, wrong)["ok"], false);
    let mut create = guarded("create_goal");
    create["goal"] = json!({"id":"02000000-0000-4000-8000-000000000085","title":"Saved export goal","status":"draft","criteria":[{"id":"C1","description":"Archive can be inspected","requires_human":false}],"stage_ids":[],"task_ref":null});
    create["body"] = json!("\n# Saved export goal\n");
    assert_eq!(call(&server.address, create)["ok"], true);
    let before = call(&server.address, request("snapshot"));
    let ready = call(&server.address, guarded("export_prepare"));
    assert_eq!(ready["ok"], true, "{ready}");
    let ready = &ready["data"];
    assert!(ready.get("destination").is_none()); // backend path is not a client download
    let client = temp.path().join("client");
    std::fs::create_dir(&client).unwrap();
    let archive = client.join("saved.tar");
    okilum_core::export::save_download(
        &archive,
        ready["bytes"].as_u64().unwrap(),
        ready["revision"].as_str().unwrap(),
        |offset| {
            let mut request = guarded("export_chunk");
            request["export_id"] = ready["export_id"].clone();
            request["offset"] = json!(offset);
            let chunk = call(&server.address, request);
            assert_eq!(chunk["ok"], true);
            assert_eq!(chunk["data"]["offset"], offset);
            Ok(STANDARD
                .decode(chunk["data"]["content_base64"].as_str().unwrap())
                .unwrap())
        },
    )
    .unwrap();
    let mut release = guarded("export_release");
    release["export_id"] = ready["export_id"].clone();
    assert_eq!(call(&server.address, release)["ok"], true);
    let mut chunk = guarded("export_chunk");
    chunk["export_id"] = ready["export_id"].clone();
    chunk["offset"] = json!(0);
    assert_eq!(call(&server.address, chunk)["ok"], false);
    let after = call(&server.address, request("snapshot"));
    assert_eq!(before["data"], after["data"]);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(runtime).unwrap();
    let extracted = client.join("extracted");
    std::fs::create_dir(&extracted).unwrap();
    // Independent system extractor, without Okilum's original configuration.
    assert!(Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&extracted)
        .status()
        .unwrap()
        .success());
    assert_eq!(
        std::fs::read(extracted.join("brain/attachment.bin")).unwrap(),
        media
    );
    assert_eq!(
        std::fs::read(extracted.join("brain/note.md")).unwrap(),
        b"# Export test\n![](attachment.bin)\n"
    );
    assert!(okilum_core::Vault::scan(&extracted.join("brain")).is_ok());
}

#[test]
fn concurrent_initial_reconnects_cannot_replace_a_new_account_pin() {
    use std::{net::TcpListener, sync::mpsc, time::Duration};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let token = temp.path().join("token");
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", http.local_addr().unwrap());
    let server = start(&root, &runtime);
    let config = json!({"actor":"fixture operator","chat":null,"todoist":{"base_url":base,"instance_id":"fixture","token_env":format!("file:{}",token.display())},"t3":null});
    let mut save = request("connectors_save");
    save["config"] = config;
    assert_eq!(call(&server.address, save)["ok"], true); // Missing token: save no pin.
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut streams = Vec::new();
        for expected in ["account-a-token", "account-b-token"] {
            let (stream, _) = http.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(headers.contains(expected));
            streams.push(stream);
            arrived_tx.send(()).unwrap();
        }
        for (mut stream, account) in streams.into_iter().zip(["account-a", "account-b"]) {
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let body = json!({"id":account}).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    std::fs::write(&token, "account-a-token").unwrap();
    let endpoint = server.address.clone();
    let a = std::thread::spawn(move || call(&endpoint, request("connectors_reconnect")));
    arrived_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    std::fs::write(&token, "account-b-token").unwrap();
    let endpoint = server.address.clone();
    let b = std::thread::spawn(move || call(&endpoint, request("connectors_reconnect")));
    arrived_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(a.join().unwrap()["ok"], true);
    release_tx.send(()).unwrap();
    let second = b.join().unwrap();
    assert_eq!(second["ok"], false, "{second}");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(runtime.join("connector-settings.json")).unwrap())
            .unwrap();
    assert_eq!(saved["todoist_account_id"], "account-a");
    worker.join().unwrap();
}

#[test]
fn prepared_revision_api_replays_after_restart_without_provider_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let server = start(&root, &runtime);
    let goal = "02000000-0000-4000-8000-000000000098";
    let stage = "03000000-0000-4000-8000-000000000098";
    let context = "04000000-0000-4000-8000-000000000098";
    let mut create = request("create_goal");
    create["goal"] = json!({"id":goal,"title":"Prepared correction","status":"active","criteria":[{"id":"C1","description":"Keep original context","requires_human":false}],"stage_ids":[],"task_ref":null});
    create["body"] = json!("\n# Goal\n");
    assert_eq!(call(&server.address, create)["ok"], true);
    let goal_revision = call(&server.address, request("goal_source"))["data"]["revision"].clone();
    let mut prepare = request("prepare_stage");
    prepare["stage"] = json!({"id":stage,"goal_id":goal,"engine":"t3","status":"ready","criterion_ids":["C1"],"context_id":context,"result_ids":[]});
    prepare["packet"] = json!({"id":context,"goal_id":goal,"stage_id":stage,"goal_revision":goal_revision,"goal":"Prepared correction","decisions":[],"constraints":[],"sources":[],"previous_result_id":null,"next_step":"Truncated"});
    prepare["operation_id"] = json!("05000000-0000-4000-8000-000000000098");
    prepare["target"] = json!({});
    let prepared = call(&server.address, prepare);
    assert_eq!(prepared["ok"], true, "{prepared}");
    let capabilities = call(&server.address, request("capabilities"));
    assert_eq!(capabilities["data"]["prepared_stage_edit"], true);
    assert_eq!(capabilities["data"]["t3"], false);
    let mut revise = request("stage_revise");
    revise["goal_id"] = json!(goal);
    revise["operation_id"] = json!("06000000-0000-4000-8000-000000000098");
    revise["expected"] = prepared["data"]["prepared_guard"].clone();
    revise["next_step"] = json!("Complete instruction, включая окончание");
    let mut stale = revise.clone();
    stale["expected"]["stage_revision"] = json!("sha256:stale");
    let rejection = call(&server.address, stale);
    assert_eq!(rejection["ok"], false);
    assert_eq!(rejection["error"]["prepared_change_recorded"], false);
    let revised = call(&server.address, revise.clone());
    assert_eq!(revised["ok"], true, "{revised}");
    let receipt = revised["data"]["prepared_change"].clone();
    assert_eq!(
        revised["data"]["dispatch"]["packet"]["next_step"],
        revise["next_step"]
    );
    assert_eq!(revised["data"]["stages"].as_array().unwrap().len(), 2);
    drop(server);
    let server = start(&root, &runtime);
    let replay = call(&server.address, revise.clone());
    assert_eq!(replay["ok"], true, "{replay}");
    assert_eq!(replay["data"]["prepared_change"], receipt);
    assert_eq!(replay["data"]["stages"].as_array().unwrap().len(), 2);
    revise["next_step"] = json!("Different payload under old operation identity");
    let collision = call(&server.address, revise);
    assert_eq!(collision["ok"], false);
    assert!(collision["error"]["prepared_change_recorded"].is_null());
    let mut discard = request("stage_discard");
    discard["goal_id"] = json!(goal);
    discard["operation_id"] = json!("07000000-0000-4000-8000-000000000098");
    discard["expected"] = replay["data"]["prepared_guard"].clone();
    let discarded = call(&server.address, discard.clone());
    assert_eq!(discarded["ok"], true, "{discarded}");
    assert_eq!(discarded["data"]["phase"], "discarded");
    assert_eq!(
        call(&server.address, discard)["data"]["prepared_change"],
        discarded["data"]["prepared_change"]
    );
}

#[test]
fn goal_brief_is_workspace_guarded_owned_and_read_only_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let server = start(&root, &runtime);
    let caps = call(&server.address, request("capabilities"));
    assert_eq!(caps["data"]["goal_context_brief"], true);
    let identity = caps["data"]["workspace"].clone();
    let guarded = |op: &str| json!({"schema":"ai-brain/workspace-v1","id":9,"op":op,"expected_workspace":identity});
    let goals = [
        "02000000-0000-4000-8000-000000000160",
        "02000000-0000-4000-8000-000000000161",
    ];
    for (index, id) in goals.iter().enumerate() {
        let mut create = guarded("create_goal");
        create["goal"] = json!({"id":id,"title":format!("Brief fixture {index}"),"status":"draft","criteria":[{"id":format!("C{index}"),"description":"Observe actual behavior","requires_human":true}],"stage_ids":[],"task_ref":null});
        create["body"] = json!("# Isolated brief fixture\n");
        assert_eq!(call(&server.address, create)["ok"], true);
    }
    let before = std::fs::read(runtime.join("state.json")).unwrap();
    let mut query = guarded("goal_context_brief");
    query["goal_id"] = json!(goals[0]);
    let first = call(&server.address, query.clone());
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["data"]["schema"], "tessera-goal-brief/v1");
    assert_eq!(first["data"]["goal_id"], goals[0]);
    assert_eq!(first["data"]["remaining_criteria"][0]["id"], "C0");
    assert_eq!(first["data"]["inputs"], json!([]));
    let mut other = query.clone();
    other["goal_id"] = json!(goals[1]);
    assert_eq!(
        call(&server.address, other)["data"]["remaining_criteria"][0]["id"],
        "C1"
    );
    let mut wrong = query.clone();
    wrong["expected_workspace"]["root"] = json!("/wrong");
    assert_eq!(call(&server.address, wrong)["ok"], false);
    let mut missing = query.clone();
    missing["goal_id"] = json!("02000000-0000-4000-8000-000000000162");
    assert_eq!(call(&server.address, missing)["ok"], false);
    assert_eq!(std::fs::read(runtime.join("state.json")).unwrap(), before);
    drop(server);
    let server = start(&root, &runtime);
    assert_eq!(call(&server.address, query)["data"], first["data"]);
}
