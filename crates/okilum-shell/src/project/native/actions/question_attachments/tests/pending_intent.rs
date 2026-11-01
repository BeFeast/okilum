use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{mpsc, Mutex},
    thread,
    time::Instant,
};

const ASSET: &str = "pending-11111111-1111-4111-8111-111111111111-png";
struct Server {
    environment: Environment,
    config_release: mpsc::Sender<()>,
    post_release: mpsc::Sender<()>,
    events: mpsc::Receiver<&'static str>,
    commands: Arc<Mutex<Vec<Value>>>,
    bytes: Arc<Mutex<Vec<u8>>>,
    tags: Arc<Mutex<Vec<String>>>,
    worker: thread::JoinHandle<()>,
}
fn accept(listener: &TcpListener) -> TcpStream {
    let start = Instant::now();
    loop {
        if let Ok((stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            return stream;
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "positive RPC detector: fixture connection never arrived"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn http(stream: &mut TcpStream) -> (String, String, Vec<u8>) {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut headers = String::new();
    let mut size = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                size = value.trim().parse::<usize>().unwrap();
            }
        }
        headers.push_str(&line);
    }
    assert!(size < 1024 * 1024, "fixture body is bounded");
    let mut body = vec![0; size];
    reader.read_exact(&mut body).unwrap();
    (first, headers, body)
}
fn response(stream: &mut TcpStream, status: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}
fn rpc(listener: &TcpListener) -> (Value, tungstenite::WebSocket<TcpStream>) {
    let mut ticket = accept(listener);
    let (first, headers, _) = http(&mut ticket);
    assert!(first.starts_with("POST /api/auth/websocket-ticket "));
    assert!(headers
        .to_ascii_lowercase()
        .contains("authorization: bearer wizard-fixture"));
    response(
        &mut ticket,
        "200 OK",
        "{\"ticket\":\"pending-intent-ticket\"}",
    );
    drop(ticket);
    let mut socket = tungstenite::accept(accept(listener)).unwrap();
    let value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
    (value, socket)
}
fn exit(socket: &mut tungstenite::WebSocket<TcpStream>, value: Value) {
    socket
        .send(tungstenite::Message::Text(
            json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":value}})
                .to_string()
                .into(),
        ))
        .unwrap();
}
impl Server {
    fn new(config_results: Vec<bool>, upload: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let environment = Environment::new(
            "pending-intent",
            &format!("http://{}", listener.local_addr().unwrap()),
        )
        .unwrap();
        let (release_config, configs) = mpsc::channel();
        let (release_post, posts) = mpsc::channel();
        let (event, events) = mpsc::channel();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let tags = Arc::new(Mutex::new(Vec::new()));
        let captured = commands.clone();
        let captured_bytes = bytes.clone();
        let captured_tags = tags.clone();
        let worker = thread::spawn(move || {
            for success in config_results {
                let (request, mut socket) = rpc(&listener);
                assert_eq!(request["tag"], "server.getConfig");
                assert_eq!(request["payload"], json!({}));
                captured_tags
                    .lock()
                    .unwrap()
                    .push("server.getConfig".into());
                event.send("config").unwrap();
                configs.recv_timeout(Duration::from_secs(5)).unwrap();
                if success {
                    exit(
                        &mut socket,
                        json!({"environment": {
                            "environmentId":"confirmed-environment", "label":"Pending intent fixture",
                            "capabilities":{"questionAttachments":true,"attachmentUploads":true,"fileAttachments":{"maxUploadBytes":52428800}}
                        }}),
                    );
                } else {
                    socket
                        .send(tungstenite::Message::Text(
                            json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Failure"}})
                                .to_string()
                                .into(),
                        ))
                        .unwrap();
                }
            }
            if upload {
                let (request, mut socket) = rpc(&listener);
                assert_eq!(request["tag"], "attachments.createUploadUrl");
                assert_eq!(request["payload"]["type"], "image");
                assert_eq!(request["payload"]["mimeType"], "image/png");
                let size = request["payload"]["sizeBytes"].as_u64().unwrap();
                captured_tags
                    .lock()
                    .unwrap()
                    .push("attachments.createUploadUrl".into());
                exit(
                    &mut socket,
                    json!({"attachmentId":ASSET,"relativeUrl":"/upload?fixture-only","expiresAt":(chrono::Utc::now().timestamp()+60)*1000}),
                );
                drop(socket);
                let mut stream = accept(&listener);
                let (first, headers, body) = http(&mut stream);
                assert!(first.starts_with("POST /upload?fixture-only "));
                assert!(
                    !headers.to_ascii_lowercase().contains("authorization:"),
                    "bearer stays off the HTTP asset transfer"
                );
                assert_eq!(body.len() as u64, size);
                *captured_bytes.lock().unwrap() = body;
                event.send("post").unwrap();
                posts.recv_timeout(Duration::from_secs(5)).unwrap();
                response(&mut stream, "204 No Content", "");
            }
            let (request, mut socket) = rpc(&listener);
            assert_eq!(request["tag"], "orchestration.dispatchCommand");
            captured_tags
                .lock()
                .unwrap()
                .push("orchestration.dispatchCommand".into());
            captured.lock().unwrap().push(request["payload"].clone());
            exit(&mut socket, json!({"sequence":1}));
        });
        Self {
            environment,
            config_release: release_config,
            post_release: release_post,
            events,
            commands,
            bytes,
            tags,
            worker,
        }
    }
    fn event(&self, expected: &str) {
        assert_eq!(
            self.events.recv_timeout(Duration::from_secs(5)).unwrap(),
            expected,
            "positive fixture I/O phase control"
        );
    }
}
fn png(color: u8) -> Vec<u8> {
    let pixels = image::RgbaImage::from_pixel(3, 2, image::Rgba([color, 60, 190, 255]));
    let mut output = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut output, image::ImageFormat::Png)
        .unwrap();
    output.into_inner()
}
fn clipboard(visual: &mut VisualTestContext, bytes: Vec<u8>) {
    visual.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image {
            format: gpui::ImageFormat::Png,
            bytes,
            id: 913,
        }))
    });
}
fn prepare_last(view: &Entity<ProjectView>, visual: &mut VisualTestContext) {
    harness::show(view, visual, request(true));
    harness::click(visual, "answer-custom-wizard-one");
    visual.simulate_input("Ready before files");
    visual.run_until_parked();
    harness::click(visual, "question-next-wizard");
}
fn pending_id(view: &Entity<ProjectView>, visual: &VisualTestContext) -> String {
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(
            actions.question_files.pending.len(),
            1,
            "captured image intent has one stable owner"
        );
        actions.question_files.pending[0].id.clone()
    })
}
fn assert_blocked(
    view: &Entity<ProjectView>,
    visual: &mut VisualTestContext,
    commands: &Mutex<Vec<Value>>,
) {
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert!(
            actions.state.allows(&actions.intent("wizard", cx)),
            "positive control: text/choice alone is already a valid answer"
        );
        assert!(!actions.question_complete("wizard", "two", cx));
        assert!(actions.question_files.references("wizard", None).is_err());
        assert!(actions.records.is_empty());
    });
    harness::click(visual, "respond-wizard");
    harness::tick(visual, 500);
    assert!(commands.lock().unwrap().is_empty());
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.question_progress["wizard"].active(), Some("two"));
        assert_eq!(
            actions.deferred_dispatches.get(),
            0,
            "readiness blocks actual button/timer dispatch, not just receipt creation"
        );
    });
}
fn pending_selector(prefix: &str, id: &str) -> &'static str {
    Box::leak(format!("{prefix}-{id}").into_boxed_str())
}

#[gpui::test]
async fn fresh_capability_lookup_cancels_choice_timer_and_retains_png_until_real_post_then_one_response(
    cx: &mut TestAppContext,
) {
    let server = Server::new(vec![true], true);
    let (view, visual, _temp, _, _, _) = harness::fixture(cx, Some(server.environment.clone()));
    prepare_last(&view, visual);
    harness::click(visual, "answer-wizard-two-0");
    view.read_with(visual, |view, cx| {
        assert!(view
            .native
            .actions
            .read(cx)
            .question_timers
            .contains_key("wizard"))
    });
    let original = png(42);
    clipboard(visual, original.clone());
    harness::click(visual, "question-paste-wizard-two");
    server.event("config");
    let id = pending_id(&view, visual);
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert!(actions.question_files.loading.is_some());
        assert!(
            actions.question_files.entries.is_empty(),
            "this is the formerly unguarded pre-entry capability phase"
        );
        assert!(!actions.question_timers.contains_key("wizard"));
    });
    assert!(visual
        .debug_bounds(pending_selector("question-file-intent-remove", &id))
        .is_some());
    assert_blocked(&view, visual, &server.commands);
    server.config_release.send(()).unwrap();
    visual
        .condition(&view, |view, cx| {
            view.native
                .actions
                .read(cx)
                .question_files
                .entries
                .values()
                .flatten()
                .any(|entry| entry.prepared.is_some())
        })
        .await;
    server.event("post");
    assert_eq!(
        *server.bytes.lock().unwrap(),
        original,
        "actual bounded HTTP POST carries the captured PNG bytes"
    );
    view.read_with(visual, |view, cx| {
        assert!(
            view.native
                .actions
                .read(cx)
                .question_files
                .pending
                .is_empty(),
            "intent-to-entry handoff is atomic"
        )
    });
    assert_blocked(&view, visual, &server.commands);
    server.post_release.send(()).unwrap();
    visual
        .condition(&view, |view, cx| {
            view.native
                .actions
                .read(cx)
                .question_complete("wizard", "two", cx)
        })
        .await;
    harness::tick(visual, 500);
    assert!(
        server.commands.lock().unwrap().is_empty(),
        "neither discovery nor upload completion rearms auto advance"
    );
    harness::click(visual, "respond-wizard");
    server.worker.join().unwrap();
    let commands = server.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["type"], "runtime-request.respond");
    assert_eq!(
        commands[0]["answers"],
        json!({"one":"Ready before files","two":" exact "})
    );
    assert!(commands[0]["attachmentsByQuestionId"].get("one").is_none());
    assert_eq!(
        commands[0]["attachmentsByQuestionId"]["two"][0]["id"],
        ASSET
    );
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.records.len(), 1);
        assert_eq!(
            actions.records[0].command.command_id,
            commands[0]["commandId"].as_str().unwrap()
        );
        assert_eq!(actions.records[0].sequence, Some(1));
    });
}

#[gpui::test]
async fn failed_capability_lookup_keeps_captured_png_retry_remove_and_blocks_text_until_remove(
    cx: &mut TestAppContext,
) {
    let server = Server::new(vec![false], false);
    let (view, visual, _temp, _, _, _) = harness::fixture(cx, Some(server.environment.clone()));
    prepare_last(&view, visual);
    harness::click(visual, "answer-custom-wizard-two");
    visual.simulate_input("valid existing text");
    visual.run_until_parked();
    clipboard(visual, png(80));
    visual.simulate_keystrokes("secondary-v");
    visual.run_until_parked();
    server.event("config");
    let id = pending_id(&view, visual);
    assert_blocked(&view, visual, &server.commands);
    server.config_release.send(()).unwrap();
    visual
        .condition(&view, |view, cx| {
            let actions = view.native.actions.read(cx);
            actions.question_files.loading.is_none()
                && actions
                    .question_files
                    .pending
                    .iter()
                    .any(|pending| pending.error.is_some())
        })
        .await;
    assert_eq!(pending_id(&view, visual), id);
    assert!(visual
        .debug_bounds(pending_selector("question-file-intent-retry", &id))
        .is_some());
    assert!(visual
        .debug_bounds(pending_selector("question-file-intent-remove", &id))
        .is_some());
    assert_blocked(&view, visual, &server.commands);
    harness::click(visual, pending_selector("question-file-intent-remove", &id));
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert!(actions.question_files.pending.is_empty());
        assert!(actions.question_complete("wizard", "two", cx));
    });
    harness::click(visual, "respond-wizard");
    server.worker.join().unwrap();
    let commands = server.commands.lock().unwrap();
    assert_eq!(
        commands.len(),
        1,
        "positive detector: explicit removal restores actual text response"
    );
    assert_eq!(
        commands[0]["answers"],
        json!({"one":"Ready before files","two":"valid existing text"})
    );
    assert!(commands[0].get("attachmentsByQuestionId").is_none());
    assert_eq!(
        *server.tags.lock().unwrap(),
        vec!["server.getConfig", "orchestration.dispatchCommand"]
    );
    assert!(
        server.bytes.lock().unwrap().is_empty(),
        "failed discovery never starts an upload"
    );
}

#[gpui::test]
async fn retry_after_capability_failure_uploads_original_captured_image_not_the_new_clipboard(
    cx: &mut TestAppContext,
) {
    let server = Server::new(vec![false, true], true);
    let (view, visual, _temp, _, _, _) = harness::fixture(cx, Some(server.environment.clone()));
    prepare_last(&view, visual);
    harness::click(visual, "answer-custom-wizard-two");
    visual.simulate_input("valid answer survives retry");
    visual.run_until_parked();
    let original = png(120);
    clipboard(visual, original.clone());
    harness::click(visual, "question-paste-wizard-two");
    server.event("config");
    let id = pending_id(&view, visual);
    server.config_release.send(()).unwrap();
    visual
        .condition(&view, |view, cx| {
            view.native
                .actions
                .read(cx)
                .question_files
                .pending
                .iter()
                .any(|pending| pending.error.is_some())
        })
        .await;
    clipboard(visual, png(210));
    harness::click(visual, pending_selector("question-file-intent-retry", &id));
    server.event("config");
    assert_eq!(
        pending_id(&view, visual),
        id,
        "retry keeps the captured intent identity"
    );
    assert_blocked(&view, visual, &server.commands);
    server.config_release.send(()).unwrap();
    visual
        .condition(&view, |view, cx| {
            view.native
                .actions
                .read(cx)
                .question_files
                .entries
                .values()
                .flatten()
                .any(|entry| entry.prepared.is_some())
        })
        .await;
    server.event("post");
    assert_eq!(
        *server.bytes.lock().unwrap(),
        original,
        "retry retains the captured bytes despite a replacement clipboard image"
    );
    server.post_release.send(()).unwrap();
    visual
        .condition(&view, |view, cx| {
            view.native
                .actions
                .read(cx)
                .question_complete("wizard", "two", cx)
        })
        .await;
    harness::click(visual, "respond-wizard");
    server.worker.join().unwrap();
    let commands = server.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["answers"]["two"], "valid answer survives retry");
    assert_eq!(
        commands[0]["attachmentsByQuestionId"]["two"][0]["id"],
        ASSET
    );
    assert_eq!(
        *server.tags.lock().unwrap(),
        vec![
            "server.getConfig",
            "server.getConfig",
            "attachments.createUploadUrl",
            "orchestration.dispatchCommand"
        ]
    );
}

#[gpui::test]
fn unsupported_profile_keeps_pasted_source_until_explicit_remove_then_allows_text_response(
    cx: &mut TestAppContext,
) {
    let (environment, commands, server) = harness::rpc(true);
    let (view, visual, _temp, _, _, _) = harness::fixture(cx, Some(environment));
    prepare_last(&view, visual);
    harness::click(visual, "answer-custom-wizard-two");
    visual.simulate_input("text without an unsupported file");
    visual.run_until_parked();
    view.update(visual, |view, cx| {
        view.native.actions.update(cx, |actions, cx| {
            let mut unsupported = profile();
            unsupported.capabilities.question_attachments = Some(false);
            actions.question_files.profile = Some(unsupported);
            cx.notify();
        })
    });
    clipboard(visual, png(180));
    visual.simulate_keystrokes("secondary-v");
    visual.run_until_parked();
    let id = pending_id(&view, visual);
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert!(
            actions.question_files.loading.is_none(),
            "known unsupported capability never probes or uploads automatically"
        );
        assert!(actions.question_files.pending[0].error.is_some());
    });
    assert_blocked(&view, visual, &commands);
    harness::click(visual, pending_selector("question-file-intent-remove", &id));
    harness::click(visual, "respond-wizard");
    server.join().unwrap();
    let sent = commands.lock().unwrap();
    assert_eq!(
        sent.len(),
        1,
        "ordinary text remains usable after explicit file removal"
    );
    assert_eq!(
        sent[0]["answers"]["two"],
        "text without an unsupported file"
    );
    assert!(sent[0].get("attachmentsByQuestionId").is_none());
}
