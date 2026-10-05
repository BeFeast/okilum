//! Frozen wire, actual connection loss, and provider dispatch admission.
use super::*;
use crate::discussion_send::{self, SendRequest};
use std::{
    fs,
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        Barrier,
    },
    time::Instant,
};
use tessera_core::source::WriteBoundary;
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
fn request(shared: &Arc<Mutex<Backend>>, goal: &str, message: &str) -> SendRequest {
    let b = shared.lock().unwrap();
    let mut request = SendRequest {
        operation_id: Uuid::new_v4().to_string(),
        goal_id: goal.into(),
        expected_actor_id: b.app.local_actor().into(),
        conversation_id: None,
        message: message.into(),
        source_paths: vec![],
        request_sha256: String::new(),
    };
    request.request_sha256 = discussion_send::digest(
        b.runner.workspace_identity()["brain_id"].as_str().unwrap(),
        goal,
        &request.expected_actor_id,
        None,
        message,
        &[],
    )
    .unwrap();
    request
}
fn wire(shared: &Arc<Mutex<Backend>>, request: &SendRequest, get: bool) -> Value {
    let mut value = if get {
        serde_json::to_value(request.key()).unwrap()
    } else {
        serde_json::to_value(request).unwrap()
    };
    value["schema"] = json!("ai-brain/workspace-v1");
    value["id"] = json!(Uuid::new_v4().to_string());
    value["op"] = json!(if get { "chat_send_get" } else { "chat_send" });
    value["expected_workspace"] = shared.lock().unwrap().runner.workspace_identity();
    value
}
fn invoke(shared: &Arc<Mutex<Backend>>, value: Value) -> Result<Value> {
    let request: Request = serde_json::from_value(value)?;
    dispatch(shared, request.command, request.expected_workspace)
}
fn wait_done(shared: &Arc<Mutex<Backend>>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let value = invoke_get_chat(shared, id);
        if value["status"] == "complete" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "provider job did not finish: {value}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn invoke_get_chat(shared: &Arc<Mutex<Backend>>, id: &str) -> Value {
    dispatch(
        shared,
        Command::ChatGet {
            conversation_id: id.into(),
            context_turn_id: None,
        },
        None,
    )
    .unwrap()
}

#[test]
fn strict_wire_requires_exact_envelope_and_explicit_nullable_target() {
    let (_temp, shared, goal) = fixture();
    let request = request(&shared, &goal, "Question");
    for get in [false, true] {
        let valid = wire(&shared, &request, get);
        assert!(serde_json::from_value::<Request>(valid.clone()).is_ok());
        for (key, value) in [
            ("schema", json!(SCHEMA)),
            ("id", json!(7)),
            ("id", json!("")),
            ("expected_workspace", Value::Null),
            ("extra", json!(true)),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(
                serde_json::from_value::<Request>(invalid).is_err(),
                "accepted {key}"
            );
        }
        let mut missing = valid.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("expected_workspace");
        assert!(serde_json::from_value::<Request>(missing).is_err());
        let mut foreign = valid;
        foreign["expected_workspace"]["brain_id"] = json!(Uuid::new_v4().to_string());
        assert!(invoke(&shared, foreign).is_err());
    }
    let mut missing = wire(&shared, &request, false);
    missing.as_object_mut().unwrap().remove("conversation_id");
    assert!(serde_json::from_value::<Request>(missing).is_err());
    let b = shared.lock().unwrap();
    assert_eq!(
        b.app.capabilities(&b.runner)["discussion_send_recovery"],
        true
    );
    drop(b);
    assert_eq!(
        invoke(&shared, wire(&shared, &request, true)).unwrap()["status"],
        "unknown"
    );
}

#[test]
fn concurrent_replay_lost_ack_and_lookup_dispatch_only_new_operations() {
    let (_temp, shared, goal) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    shared.lock().unwrap().app.chat = Some(chat::ChatConfig {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        api_key: "synthetic".into(),
        model: "fixture".into(),
        idle_timeout: Duration::from_secs(5),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let bodies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let captured = bodies.clone();
    let finished = stop.clone();
    let provider = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !finished.load(Ordering::SeqCst) && Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("provider accept: {e}"),
            };
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
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            captured.lock().unwrap().push(body);
            let sse =
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}", sse.len()).unwrap();
        }
    });
    let request = request(&shared, &goal, "Exact question\r\n😀");
    let barrier = Arc::new(Barrier::new(3));
    let jobs: Vec<_> = (0..2)
        .map(|_| {
            let shared = shared.clone();
            let value = wire(&shared, &request, false);
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                invoke(&shared, value).unwrap()
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(results[0], results[1]);
    let first = &results[0];
    println!(
        "discussion_send_projected_fixture={}",
        json!({"request":wire(&shared, &request, false),"data":first})
    );
    wait_done(
        &shared,
        first["record"]["conversation_id"].as_str().unwrap(),
    );
    assert_eq!(bodies.lock().unwrap().len(), 1);
    assert_eq!(
        crate::retrieval::sha(&bodies.lock().unwrap()[0]),
        first["record"]["provider_request_sha256"]
    );

    // Positive control: a distinct UUID with identical payload is an explicit new send.
    // The real TCP client closes without reading the acknowledgement.
    let mut next = request.clone();
    next.operation_id = Uuid::new_v4().to_string();
    let service = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(service.local_addr().unwrap()).unwrap();
    let worker_backend = shared.clone();
    let server = std::thread::spawn(move || {
        let (stream, _) = service.accept().unwrap();
        let _ = connection(stream, worker_backend);
    });
    writeln!(client, "{}", wire(&shared, &next, false)).unwrap();
    client.shutdown(std::net::Shutdown::Both).unwrap();
    drop(client);
    server.join().unwrap();
    let second = invoke(&shared, wire(&shared, &next, true)).unwrap();
    assert_eq!(second["status"], "projected");
    assert_ne!(
        second["record"]["conversation_id"],
        first["record"]["conversation_id"]
    );
    wait_done(
        &shared,
        second["record"]["conversation_id"].as_str().unwrap(),
    );
    assert_eq!(bodies.lock().unwrap().len(), 2);
    assert_eq!(
        crate::retrieval::sha(&bodies.lock().unwrap()[1]),
        second["record"]["provider_request_sha256"]
    );
    {
        let mut b = shared.lock().unwrap();
        fs::remove_file(
            b.runner
                .root()
                .join(first["record"]["source_path"].as_str().unwrap()),
        )
        .unwrap();
        b.app = Application::unconfigured();
    }
    assert_eq!(
        invoke(&shared, wire(&shared, &request, true)).unwrap(),
        *first
    );
    assert_eq!(
        invoke(&shared, wire(&shared, &request, false)).unwrap(),
        *first
    );
    assert_eq!(invoke(&shared, wire(&shared, &next, true)).unwrap(), second);
    std::thread::sleep(Duration::from_millis(100));
    stop.store(true, Ordering::SeqCst);
    provider.join().unwrap();
    assert_eq!(bodies.lock().unwrap().len(), 2);
}

#[test]
fn typed_rejection_is_only_before_checkpoint_and_recovery_never_dispatches() {
    let (_temp, shared, goal) = fixture();
    let request = request(&shared, &goal, "Question");
    let rejected = invoke(&shared, wire(&shared, &request, false)).unwrap_err();
    let rejected = error_value(&rejected);
    println!(
        "discussion_send_rejected_fixture={}",
        json!({"request":wire(&shared, &request, false),"error":rejected})
    );
    assert_eq!(rejected["code"], "discussion_send_rejected");
    assert_eq!(rejected["recorded"], false);
    assert_eq!(rejected["operation_id"], request.operation_id);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    {
        let mut b = shared.lock().unwrap();
        b.app.chat = Some(chat::ChatConfig {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            model: "fixture".into(),
            api_key: "synthetic".into(),
            idle_timeout: Duration::from_secs(1),
        });
        b.runner.interrupt_next_source_projection();
    }
    let failed = invoke(&shared, wire(&shared, &request, false)).unwrap_err();
    assert_eq!(
        error_value(&failed)["code"],
        "discussion_send_recovery_error"
    );
    assert!(error_value(&failed).get("recorded").is_none());
    let found = invoke(&shared, wire(&shared, &request, true)).unwrap();
    assert_eq!(found["status"], "projected");
    assert_eq!(
        invoke(&shared, wire(&shared, &request, false)).unwrap(),
        found
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    // Prove this very listener would observe a dispatch connection.
    let _control = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    assert!(listener.accept().is_ok());
}
