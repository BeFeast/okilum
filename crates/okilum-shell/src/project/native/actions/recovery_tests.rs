use super::super::recovery::{ConnectionAttempt, ConnectionPhase, ConnectionSource};
use super::super::recovery_state::ConnectionContext;
use super::*;
use ::core::prelude::v1::test;
use okilum_t3::projection::Projection;
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};
use tungstenite::{Message, WebSocket};

const THREAD: &str = "mcp:fixture-manager";
const PROJECT: &str = "fixture-managers";

fn accept(listener: &TcpListener) -> TcpStream {
    let (stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}
fn headers(stream: &mut TcpStream) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
    }
}
fn response(stream: &mut TcpStream, status: u16, body: &str) {
    write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}
fn ticket(listener: &TcpListener) {
    let mut stream = accept(listener);
    headers(&mut stream);
    response(
        &mut stream,
        200,
        &json!({"ticket":"fixture-ticket","expiresAt":"2099-01-01T00:00:00Z"}).to_string(),
    );
}
fn read(ws: &mut WebSocket<TcpStream>) -> Value {
    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap()
}
fn send(ws: &mut WebSocket<TcpStream>, value: Value) {
    ws.send(Message::Text(value.to_string().into())).unwrap();
}
// The authenticated connection warms its own config stream independently of
// thread replay. Route real connections by RPC tag instead of assuming ticket
// and upgrade pairs from concurrent clients arrive in a fixed order.
struct RecoverySockets {
    threads: mpsc::Receiver<(WebSocket<TcpStream>, Value)>,
    shells: mpsc::Receiver<(WebSocket<TcpStream>, Value)>,
    address: SocketAddr,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl RecoverySockets {
    fn new(listener: TcpListener) -> Self {
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = stopped.clone();
        let (threads, accepted) = mpsc::channel();
        let (shells, accepted_shells) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            for incoming in listener.incoming() {
                let mut stream = incoming.unwrap();
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let threads = threads.clone();
                let shells = shells.clone();
                let stopping = stopping.clone();
                workers.push(thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut first = [0; 1];
                    assert_eq!(stream.peek(&mut first).unwrap(), 1);
                    if first[0] == b'P' {
                        headers(&mut stream);
                        response(&mut stream, 200, &json!({"ticket":"fixture-ticket","expiresAt":"2099-01-01T00:00:00Z"}).to_string());
                        return;
                    }
                    let mut ws = tungstenite::accept(stream).unwrap();
                    let request = read(&mut ws);
                    match request["tag"].as_str() {
                        Some("orchestration.subscribeThread") => {
                            threads.send((ws, request)).unwrap();
                        }
                        Some("orchestration.subscribeShell") => {
                            shells.send((ws, request)).unwrap();
                        }
                        Some("subscribeServerConfig") => {
                            assert_eq!(request["payload"], json!({}));
                            ws.get_ref().set_read_timeout(Some(Duration::from_millis(100))).unwrap();
                            let snapshot = json!({"_tag":"Chunk","requestId":"1","values":[{
                                "version":1,"type":"snapshot","config":{
                                    "environment":{"environmentId":"recovery-fixture","label":"Recovery fixture"},
                                    "providers":[{"instanceId":"fixture-codex","driver":"codex","enabled":true,
                                        "models":[{"slug":"fixture-model","name":"Fixture model"}]}]
                                }
                            }]});
                            if ws.send(Message::Text(snapshot.to_string().into())).is_err() {
                                return;
                            }
                            while !stopping.load(Ordering::SeqCst) {
                                match ws.read() {
                                    Ok(Message::Text(text)) => {
                                        let value: Value = serde_json::from_str(&text).unwrap();
                                        match value["_tag"].as_str() {
                                            Some("Ping") => {
                                                if ws.send(Message::Text(json!({"_tag":"Pong"}).to_string().into())).is_err() {
                                                    break;
                                                }
                                            }
                                            Some("Ack") => assert_eq!(value["requestId"], "1"),
                                            Some("Interrupt") => break,
                                            other => panic!("unexpected config stream frame: {other:?}"),
                                        }
                                    }
                                    Ok(Message::Ping(_)) => {
                                        if ws.flush().is_err() { break; }
                                    }
                                    Ok(Message::Pong(_)) => {}
                                    Ok(Message::Close(_)) => break,
                                    Err(tungstenite::Error::Io(error))
                                        if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                                    Err(_) => break,
                                    Ok(_) => panic!("unexpected config WebSocket frame"),
                                }
                            }
                        }
                        other => panic!("unexpected recovery fixture RPC: {other:?}"),
                    }
                }));
            }
            crate::project::fixture::join_workers(workers);
        });
        Self {
            threads: accepted,
            shells: accepted_shells,
            address,
            stopped,
            worker: Some(worker),
        }
    }
}
impl Drop for RecoverySockets {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Wake the blocking accept, then join every owned connection worker.
        let _ = TcpStream::connect(self.address);
        crate::project::fixture::join_workers(self.worker.take());
    }
}
fn socket(server: &RecoverySockets, after: Option<u64>) -> WebSocket<TcpStream> {
    let (mut ws, request) = server.threads.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request["tag"], "orchestration.subscribeThread");
    assert_eq!(request["payload"]["threadId"], THREAD);
    assert_eq!(request["payload"]["afterSequence"].as_u64(), after);
    assert_eq!(request["payload"]["requestCompletionMarker"], true);
    ping(&mut ws);
    ws
}

fn ping(ws: &mut WebSocket<TcpStream>) {
    assert_eq!(read(ws)["_tag"], "Ping");
    send(ws, json!({"_tag":"Pong"}));
}
fn chunk(ws: &mut WebSocket<TcpStream>, values: Value) {
    send(ws, json!({"_tag":"Chunk","requestId":"1","values":values}));
    assert_eq!(read(ws)["_tag"], "Ack");
    ping(ws);
}
fn question() -> Value {
    json!({"id":"question-row","threadId":THREAD,"ordinal":1,"type":"user_input_request","requestId":"question","status":"pending","questions":[{"id":"q","question":"Keep the exact draft","multiSelect":true,"options":[{"label":"Exact","value":" exact "}]}]})
}
fn snapshot() -> Value {
    let item = question();
    json!({"kind":"snapshot","snapshotSequence":10,"projection":{"thread":{"id":THREAD,"projectId":PROJECT,"title":"Recovery fixture"},"runs":[],"attempts":[],"runtimeRequests":[{"id":"question","threadId":THREAD,"kind":"user_input","status":"pending","responseCapability":{"type":"message"}}],"providerThreads":[],"providerSessions":[],"turnItems":[item],"visibleTurnItems":[{"sourceThreadId":THREAD,"sourceItemId":"question-row","visibility":"local","item":item}]}})
}
fn fixture<'a>(
    cx: &'a mut TestAppContext,
    origin: &str,
) -> (
    Entity<ProjectView>,
    &'a mut VisualTestContext,
    tempfile::TempDir,
) {
    // Real socket threads require GPUI's documented mixed real-I/O mode.
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_features::enable_test_modules(cx);
        cx.set_reduce_motion(true);
    });
    let temp = tempfile::tempdir().unwrap();
    let environment = Environment::new("recovery", origin).unwrap();
    let expiry = chrono::Utc::now().timestamp() as u64 + 3600;
    let token = NativeToken::from_secret(&environment, &serde_json::to_vec(&json!({"origin":origin,"access_token":"recovery-fixture","expires_at":expiry,"scope":"orchestration:read orchestration:operate"})).unwrap()).unwrap();
    let client =
        Arc::new(NativeClient::new(environment.clone(), token, Duration::from_secs(5)).unwrap());
    let mut project = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            view.threads.enabled = true;
            view.environment = Some(environment);
            view.native.client = Some(client);
            view.native.expires_at = Some(expiry);
            view.native.retained = Some(Arc::new(Mutex::new(super::super::Retained {
                model: Projection::new(THREAD, PROJECT),
                cursor: None,
                valid: true,
                epoch: 0,
            })));
            view.sync_native_action_surface(cx);
            view
        });
        project = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(1000.)));
    visual.run_until_parked();
    (project.unwrap(), visual, temp)
}
fn start(view: &Entity<ProjectView>, visual: &mut VisualTestContext) {
    view.update_in(visual, |view, window, cx| {
        view.active = true;
        view.native_start(window, cx);
    });
}
fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .expect("positive control: actual recovery control rendered");
    visual.simulate_click(bounds.center(), Modifiers::default());
}
fn key(visual: &mut VisualTestContext, name: &str) {
    let keystroke = gpui::Keystroke::parse(name).unwrap();
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(gpui::KeyUpEvent { keystroke });
    visual.run_until_parked();
}
fn repaint(view: &Entity<ProjectView>, visual: &mut VisualTestContext) {
    view.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();
}

#[gpui::test]
async fn real_close_and_cursor_replay_keep_controls_disabled_until_sync_and_retain_the_same_draft(
    cx: &mut TestAppContext,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (command, commands) = mpsc::channel();
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let server = thread::spawn(move || {
        let sockets = RecoverySockets::new(listener);
        let mut first = socket(&sockets, None);
        count.fetch_add(1, Ordering::SeqCst);
        chunk(&mut first, json!([snapshot(),{"kind":"synchronized"}]));
        assert_eq!(
            commands.recv_timeout(Duration::from_secs(5)).unwrap(),
            "close"
        );
        first.close(None).unwrap();
        drop(first);
        let mut second = socket(&sockets, Some(10));
        count.fetch_add(1, Ordering::SeqCst);
        let mut item = question();
        item["prompt"] = json!("Replayed before synchronization");
        chunk(
            &mut second,
            json!([{"kind":"event","sequence":11,"event":{"type":"turn-item.updated","threadId":THREAD,"payload":item}}]),
        );
        assert_eq!(
            commands.recv_timeout(Duration::from_secs(5)).unwrap(),
            "sync"
        );
        send(
            &mut second,
            json!({"_tag":"Chunk","requestId":"1","values":[{"kind":"synchronized"}]}),
        );
        assert_eq!(read(&mut second)["_tag"], "Ack");
        assert_eq!(
            commands.recv_timeout(Duration::from_secs(5)).unwrap(),
            "stop"
        );
    });
    let (view, visual, _temp) = fixture(cx, &origin);
    start(&view, visual);
    visual
        .condition(&view, |view, _| view.native.stream_ready)
        .await;
    repaint(&view, visual);
    assert!(visual.debug_bounds("project-request-question").is_some());
    let (input, client, retained, lease) = view.read_with(visual, |view, cx| {
        (
            view.native.actions.read(cx).inputs[&("question".into(), "q".into())].clone(),
            view.native.client.clone().unwrap(),
            view.native.retained.clone().unwrap(),
            view.native.request_lease.clone(),
        )
    });
    click(visual, "answer-custom-question-q");
    visual.simulate_input("  exact retained draft  ");
    visual.run_until_parked();
    command.send("close").unwrap();
    visual
        .condition(&view, |view, _| {
            view.native.phase == ConnectionPhase::Reconnecting
        })
        .await;
    repaint(&view, visual);
    assert!(visual.debug_bounds("manager-primary").is_some());
    view.read_with(visual, |view, cx| {
        assert!(!view.native.stream_ready);
        assert!(!view.native.actions.read(cx).enabled);
        assert!(!view.native.actions.read(cx).state.ready);
    });
    click(visual, "respond-question");
    visual.run_until_parked();
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "disabled buttons cannot dispatch or duplicate the reconnect worker"
    );
    visual.executor().advance_clock(Duration::from_secs(1));
    visual
        .condition(&view, |view, _| {
            view.native
                .retained
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.sequence() == 11)
        })
        .await;
    view.read_with(visual, |view, cx| {
        assert!(!view.native.stream_ready);
        assert!(!view.native.actions.read(cx).state.ready);
        assert!(view.native.actions.read(cx).records.is_empty());
    });
    command.send("sync").unwrap();
    visual
        .condition(&view, |view, _| view.native.stream_ready)
        .await;
    view.read_with(visual, |view, cx| {
        assert!(Arc::ptr_eq(&client, view.native.client.as_ref().unwrap()));
        assert!(Arc::ptr_eq(
            &retained,
            view.native.retained.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(&lease, &view.native.request_lease));
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.inputs[&("question".into(), "q".into())], input);
        assert!(actions.state.ready);
        assert!(actions.records.is_empty());
        assert_eq!(
            input.read(cx).value().to_string(),
            "  exact retained draft  "
        );
    });
    command.send("stop").unwrap();
    view.update(visual, |view, _| view.native.pause());
    server.join().unwrap();
}

#[gpui::test]
async fn terminal_ticket_and_protocol_failures_offer_real_inline_recovery_without_retry_loops(
    cx: &mut TestAppContext,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let server = thread::spawn(move || {
        let mut first = accept(&listener);
        headers(&mut first);
        count.fetch_add(1, Ordering::SeqCst);
        response(&mut first, 401, "{}");
        drop(first);
        ticket(&listener);
        let mut upgrade = accept(&listener);
        headers(&mut upgrade);
        count.fetch_add(1, Ordering::SeqCst);
        response(&mut upgrade, 426, "{}");
    });
    let (view, visual, _temp) = fixture(cx, &origin);
    start(&view, visual);
    visual
        .condition(&view, |view, _| {
            view.native.phase == ConnectionPhase::Terminal
        })
        .await;
    repaint(&view, visual);
    assert!(
        visual.debug_bounds("manager-primary").is_some(),
        "401 has one explicit credential recovery action"
    );
    assert!(
        visual.debug_bounds("manager-details").is_none(),
        "recovery does not require Details"
    );
    let lease = view.read_with(visual, |view, _| view.native.request_lease.clone());
    view.update_in(visual, |view, window, cx| view.native_retry_now(window, cx));
    visual
        .condition(&view, |view, _| {
            view.native
                .failure
                .as_ref()
                .is_some_and(|failure| failure.cause == StreamError::IncompatibleProtocol)
        })
        .await;
    repaint(&view, visual);
    assert!(visual.debug_bounds("manager-primary").is_some());
    assert!(visual.debug_bounds("manager-details").is_none());
    view.read_with(visual, |view, _| {
        assert_eq!(view.native.phase, ConnectionPhase::Terminal);
        assert!(!view.native.stream_ready);
        assert!(Arc::ptr_eq(&lease, &view.native.request_lease));
    });
    visual.executor().advance_clock(Duration::from_secs(60));
    visual.run_until_parked();
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "terminal auth/protocol failures never loop"
    );
    view.update_in(visual, |view, window, cx| {
        view.native_failed(StreamError::Expired, ConnectionContext::Connect, window, cx);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("manager-primary").is_some(),
        "typed Expired has its safe Reconnect action"
    );
    view.read_with(visual, |view, _| {
        assert_eq!(
            view.native.failure.as_ref().unwrap().cause,
            StreamError::Expired
        );
        assert_eq!(view.native.phase, ConnectionPhase::Terminal);
    });
    view.update(visual, |view, _| view.native.pause());
    server.join().unwrap();
}

#[gpui::test]
async fn history_failure_stays_connected_and_exact_dismissals_survive_navigation_without_losing_retry(
    cx: &mut TestAppContext,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let gets = Arc::new(AtomicUsize::new(0));
    let count = gets.clone();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        headers(&mut stream);
        count.fetch_add(1, Ordering::SeqCst);
        response(&mut stream, 503, "{}");
    });
    let (view, visual, _temp) = fixture(cx, &origin);
    let exact_error = format!(
        "first line\nsecond line\nthird line\n{}",
        "full retained details\n".repeat(35)
    );
    let record = Record {
        command: okilum_t3::actions::Command::new(
            THREAD,
            Intent::message("retained message".into(), DispatchMode::StartImmediately),
        ),
        sequence: None,
    };
    view.update_in(visual, |view, window, cx| {
        view.active = false;
        view.native.stream_ready = true;
        view.native.phase = ConnectionPhase::Connected;
        let mut model = view.native.retained.as_ref().unwrap().lock().unwrap();
        model
            .model
            .apply(&[ThreadUpdate::Snapshot {
                sequence: 10,
                projection: snapshot()["projection"].clone(),
                history_cursor: Some("opaque-history".into()),
                has_more_history: true,
                latest_local_turn_ordinal: Some(1),
            }])
            .unwrap();
        let changes = model.model.take_changes();
        view.native.metadata = changes.metadata.clone().unwrap();
        let state = model.model.action_state();
        drop(model);
        view.native.actions.update(cx, |actions, cx| {
            actions.set_state(state, window, cx);
            actions.error = Some(exact_error.clone());
            actions.records = vec![record.clone()];
            cx.notify();
        });
        view.feed
            .update(cx, |feed, cx| feed.apply_native(changes, &[], cx));
        cx.notify();
    });
    repaint(&view, visual);
    // Reach the real trigger by physical Tab from the composer; never infer
    // keyboard focus from a mouse click, which Button deliberately avoids.
    let panel = visual.debug_bounds("project-native-actions").unwrap();
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: panel.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-3000.))),
        ..Default::default()
    });
    visual.run_until_parked();
    let preview = visual
        .debug_bounds("project-action-error-preview")
        .expect("positive control: actual long error preview");
    let max_preview_height = visual.update(|window, _| window.line_height() * 3.);
    assert!(
        preview.size.height <= max_preview_height,
        "error preview is bounded to three defined line boxes: bounds={preview:?}; max={max_preview_height:?}"
    );
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| input.focus(window, cx))
    });
    for _ in 0..40 {
        key(visual, "tab");
        if visual.update(|window, _| {
            window
                .context_stack()
                .iter()
                .any(|context| context.contains("ProjectErrorDetailsTrigger"))
        }) {
            break;
        }
    }
    assert!(
        visual.update(|window, _| window
            .context_stack()
            .iter()
            .any(|context| context.contains("ProjectErrorDetailsTrigger"))),
        "positive control: Tab reached the actual Details trigger context"
    );
    key(visual, "space");
    assert!(
        visual
            .debug_bounds("project-action-error-details-body")
            .is_some(),
        "physical Space opens the focused Details trigger"
    );
    let text_before = visual
        .debug_bounds("project-action-error-details-text")
        .unwrap()
        .origin
        .y;
    key(visual, "pagedown");
    let text_scrolled = visual
        .debug_bounds("project-action-error-details-text")
        .unwrap()
        .origin
        .y;
    assert!(
        text_scrolled < text_before,
        "positive control: keyboard PageDown scrolls the full multiline details"
    );
    let focused = visual.update(|window, cx| window.focused(cx));
    assert!(focused.is_some());
    repaint(&view, visual);
    assert_eq!(
        focused,
        visual.update(|window, cx| window.focused(cx)),
        "parent repaint keeps the exact details FocusHandle"
    );
    key(visual, "down");
    key(visual, "up");
    assert_eq!(
        visual
            .debug_bounds("project-action-error-details-text")
            .unwrap()
            .origin
            .y,
        text_scrolled,
        "normal parent repaint preserves the same focused scroll state"
    );
    key(visual, "escape");
    key(visual, "tab");
    assert!(
        visual.update(|window, _| window
            .context_stack()
            .iter()
            .any(|context| context.contains("ProjectErrorDismiss"))),
        "positive control: keyboard focus reached Dismiss"
    );
    key(visual, "space");
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.error.as_ref(), Some(&exact_error));
        assert_eq!(
            actions.records[0].command.command_id,
            record.command.command_id
        );
        assert_eq!(
            actions.records[0].command.wire().unwrap(),
            record.command.wire().unwrap()
        );
        assert!(actions.retry_allowed(&record));
    });
    assert!(visual.debug_bounds("project-action-error").is_none());
    view.update(visual, |view, cx| {
        view.chat = Some(Conversation::new("clean-b"));
        view.sync_native_action_surface(cx);
        view.native.actions.update(cx, |actions, cx| {
            actions.error = Some(exact_error.clone());
            cx.notify();
        });
        cx.notify();
    });
    repaint(&view, visual);
    assert!(
        visual.debug_bounds("project-action-error").is_some(),
        "the same exact error in another thread is visible"
    );
    view.update_in(visual, |view, window, cx| {
        view.chat = Some(Conversation::new(THREAD));
        view.sync_native_action_surface(cx);
        let state = view
            .native
            .retained
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .model
            .action_state();
        view.native.actions.update(cx, |actions, cx| {
            actions.set_state(state, window, cx);
            actions.error = Some(exact_error.clone());
            actions.records = vec![record.clone()];
            cx.notify();
        });
        cx.notify();
    });
    repaint(&view, visual);
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.native.actions.read(cx).error.as_ref(),
            Some(&exact_error)
        )
    });
    assert!(
        visual.debug_bounds("project-action-error").is_none(),
        "session dismissal survives navigation and reconstructed canonical error"
    );
    view.update(visual, |view, cx| {
        view.native.actions.update(cx, |actions, cx| {
            actions.error = Some(format!("{exact_error}new hidden detail"));
            cx.notify();
        });
        cx.notify();
    });
    repaint(&view, visual);
    assert!(
        visual.debug_bounds("project-action-error").is_some(),
        "new full text with the same three-line preview is visible"
    );
    view.update(visual, |view, _| view.active = true);
    click(visual, "project-live-history");
    visual
        .condition(&view, |view, _| view.native.history_error.is_some())
        .await;
    repaint(&view, visual);
    assert!(visual.debug_bounds("project-history-error").is_some());
    view.read_with(visual, |view, cx| {
        assert_eq!(view.native.status_label(cx), "Waiting");
        assert!(view.native.client_can_operate());
        assert!(view.native.actions.read(cx).state.ready);
        assert!(view.native.failure.is_none());
    });
    click(visual, "project-history-error-dismiss");
    visual.run_until_parked();
    repaint(&view, visual);
    assert!(
        visual.debug_bounds("project-live-history").is_some(),
        "explicit history retry remains available after dismissal"
    );
    assert_eq!(
        gets.load(Ordering::SeqCst),
        1,
        "dismiss/repaint never retries a failed page"
    );
    view.update(visual, |view, _| view.native.pause());
    server.join().unwrap();
}

#[gpui::test]
fn stale_connection_epochs_and_cancelled_credential_attempts_cannot_mutate_new_authority(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp) = fixture(cx, "http://127.0.0.1:9");
    view.update(visual, |view, cx| {
        view.native.stream_ready = true;
        view.native
            .actions
            .update(cx, |actions, _| actions.state.ready = true);
        assert!(
            view.native.client_can_operate() && view.native.stream_error_is_clear(),
            "positive control: the old synchronized client can operate"
        );
        let old_attempt =
            ConnectionAttempt::begin(view, view.environment.as_ref().unwrap().origin());
        view.native.busy = true;
        assert!(
            !view.native.client_can_operate() && !view.native.stream_error_is_clear(),
            "credential setup blocks even queued operations from an old ready state"
        );
        assert!(
            old_attempt.current(view),
            "positive control: current setup attempt is accepted"
        );
        let client = view.native.client.clone().unwrap();
        let retained = view.native.retained.clone().unwrap();
        let epoch = retained.lock().unwrap().epoch;
        let old_source =
            ConnectionSource::new(view, client.clone(), retained.clone(), THREAD.into(), epoch);
        assert!(
            old_source.current(view),
            "positive control: the original transport source is current"
        );
        view.native.pause();
        assert!(
            !view.native.busy,
            "cancelling an in-flight credential read cannot wedge reactivation"
        );
        let fresh_attempt = ConnectionAttempt::begin(view, client.origin());
        view.native.busy = true;
        assert!(!old_attempt.current(view));
        assert!(fresh_attempt.current(view));
        assert!(!old_source.current(view));
        let new_epoch = retained.lock().unwrap().epoch;
        let fresh_source = ConnectionSource::new(view, client, retained, THREAD.into(), new_epoch);
        assert!(
            fresh_source.current(view),
            "positive control: fresh same-thread source passes after epoch replacement"
        );
        // This is the identical source/epoch predicate used by every real
        // subscribe success/error and receive-completion callback.
        if old_attempt.current(view) {
            view.native.busy = false;
        }
        assert!(
            view.native.busy,
            "old credential completion cannot unlock a new attempt"
        );
        view.chat = Some(Conversation::new("other-thread"));
        assert!(!fresh_source.current(view));
        view.chat = Some(Conversation::new(THREAD));
        view.native.pause();
    });
}

#[path = "ttfm_tests.rs"]
mod ttfm_tests;
