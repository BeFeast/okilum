//! Time-to-first-message measurement for the native Manager panel (#2518).
//!
//! One `manager_ttfm` line lands in `reader-diagnostic.log` when the first
//! message row is painted after a panel open. Every phase keeps only its first
//! monotonic timestamp within that open; a panel whose thread snapshot never
//! arrives never paints a row and logs nothing. Recording costs one `Option`
//! check per phase, no extra RPCs and no per-frame logging.
use super::*;

pub(super) struct Ttfm {
    opened: std::time::Instant,
    connect_started: Option<std::time::Instant>,
    auth_done: Option<std::time::Instant>,
    websocket_open: Option<std::time::Instant>,
    shell_reply: Option<std::time::Instant>,
    thread_snapshot: Option<std::time::Instant>,
    reused: bool,
    logged: bool,
}

fn elapsed_ms(from: std::time::Instant, until: std::time::Instant) -> u64 {
    until.saturating_duration_since(from).as_millis() as u64
}

impl Ttfm {
    /// A panel open restarts the measurement. `reused` marks an open that
    /// reuses the already-paired native client instead of reading or pairing
    /// credentials again.
    pub(super) fn open(reused: bool) -> Self {
        Self {
            opened: std::time::Instant::now(),
            connect_started: None,
            auth_done: None,
            websocket_open: None,
            shell_reply: None,
            thread_snapshot: None,
            reused,
            logged: false,
        }
    }

    /// Descriptor/connect start: a fresh native subscription attempt begins.
    pub(super) fn connect_started(&mut self) {
        self.connect_started
            .get_or_insert_with(std::time::Instant::now);
    }

    /// First socket to complete its ticket request and WebSocket upgrade.
    pub(super) fn socket_connected(&mut self, phases: okilum_t3::stream::ConnectPhases) {
        self.auth_done.get_or_insert(phases.auth_done);
        self.websocket_open.get_or_insert(phases.websocket_open);
    }

    /// First `subscribeShell` reply for the thread directory.
    pub(super) fn shell_replied(&mut self) {
        self.shell_reply.get_or_insert_with(std::time::Instant::now);
    }

    /// First `subscribeThread` snapshot for the manager thread.
    pub(super) fn thread_synchronized(&mut self) {
        self.thread_snapshot
            .get_or_insert_with(std::time::Instant::now);
    }

    /// The first painted message row completes the measurement: exactly one
    /// line per panel open, carrying the phases that re-occurred during it.
    /// Durations are measured from connect start, except
    /// `open_to_first_message_ms` and `connect_start_ms`, which are measured
    /// from the panel open itself.
    pub(super) fn first_message_rendered(&mut self, rendered: std::time::Instant, cx: &App) {
        if self.logged {
            return;
        }
        self.logged = true;
        let mut details = serde_json::Map::new();
        details.insert(
            "open_to_first_message_ms".into(),
            elapsed_ms(self.opened, rendered).into(),
        );
        if let Some(connect) = self.connect_started {
            details.insert(
                "connect_start_ms".into(),
                elapsed_ms(self.opened, connect).into(),
            );
            if let Some(auth) = self.auth_done {
                details.insert("auth_ms".into(), elapsed_ms(connect, auth).into());
            }
            if let Some(socket) = self.websocket_open {
                details.insert("connect_ms".into(), elapsed_ms(connect, socket).into());
            }
            if let Some(shell) = self.shell_reply {
                details.insert("shell_ms".into(), elapsed_ms(connect, shell).into());
            }
            if let Some(thread) = self.thread_snapshot {
                details.insert("thread_ms".into(), elapsed_ms(connect, thread).into());
            }
            details.insert("render_ms".into(), elapsed_ms(connect, rendered).into());
        }
        details.insert("reused_connection".into(), self.reused.into());
        if let Some(trace) = reader_diagnostics::trace(cx) {
            trace.event("manager_ttfm", serde_json::Value::Object(details));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_t3::stream::{NativeClient, NativeToken};
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream},
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        thread,
    };
    use tungstenite::Message;

    const LOG_FILE: &str = "reader-diagnostic.log";

    struct FakeT3 {
        origin: String,
        thread_subscriptions: Arc<AtomicUsize>,
        stopped: Arc<AtomicBool>,
    }

    impl Drop for FakeT3 {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::SeqCst);
        }
    }

    fn send(socket: &mut tungstenite::WebSocket<TcpStream>, value: Value) {
        socket
            .send(Message::Text(value.to_string().into()))
            .unwrap();
    }

    fn keep_alive(mut socket: tungstenite::WebSocket<TcpStream>) {
        loop {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    let Ok(value) = serde_json::from_str::<Value>(&text) else {
                        break;
                    };
                    if value["_tag"] == "Ping" {
                        send(&mut socket, json!({"_tag":"Pong"}));
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }

    fn http_ticket(mut stream: TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        drop(reader);
        let body =
            json!({"ticket":"ttfm-fixture-ticket","expiresAt":"2099-01-01T00:00:00Z"}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    }

    fn thread_snapshot_chunk() -> Value {
        let thread_id = "mcp:fixture-manager";
        let item = json!({"id":"ttfm-reply","threadId":thread_id,"ordinal":1,"type":"assistant_message","text":"First visible manager reply","status":"completed","runId":null,"nodeId":null});
        json!({
            "_tag":"Chunk","requestId":"1","values":[
                {"kind":"snapshot","snapshotSequence":10,"projection":{
                    "thread":{"id":thread_id,"projectId":"fixture-managers","title":"TTFM manager"},
                    "runs":[],"attempts":[],"turnItems":[item],
                    "visibleTurnItems":[{"sourceThreadId":thread_id,"sourceItemId":"ttfm-reply","visibility":"local","position":0,"item":item}],
                }},
                {"kind":"synchronized"},
            ]
        })
    }

    fn serve_connection(
        mut stream: TcpStream,
        shell_synchronized: &AtomicBool,
        thread_subscriptions: &AtomicUsize,
        hold_thread_snapshot: bool,
    ) {
        let mut peeked = [0; 4];
        let Ok(bytes) = stream.peek(&mut peeked) else {
            return;
        };
        if &peeked[..bytes] == b"POST" {
            http_ticket(stream);
            return;
        }
        let Ok(mut socket) = tungstenite::accept(stream) else {
            return;
        };
        let request: Value = match socket.read() {
            Ok(Message::Text(text)) => serde_json::from_str(&text).unwrap_or_default(),
            _ => return,
        };
        match request["tag"].as_str().unwrap_or_default() {
            "orchestration.subscribeShell" => {
                send(
                    &mut socket,
                    json!({
                        "_tag":"Chunk","requestId":"1","values":[
                            {"kind":"snapshot","snapshot":{"snapshotSequence":20,"threads":[
                                {"id":"mcp:fixture-manager","projectId":"fixture-managers","title":"Manager","status":"idle"},
                            ]}},
                            {"kind":"synchronized"},
                        ]
                    }),
                );
                // Hold the shell snapshot's Ack as the barrier: the manager
                // snapshot never leaves the server before the shell snapshot
                // was received, so shell_ms cannot be skipped by the first
                // rendered frame. The live reader pings before its first read;
                // answer those while waiting for the Ack.
                loop {
                    match socket.read() {
                        Ok(Message::Text(text)) => {
                            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                                return;
                            };
                            match value["_tag"].as_str() {
                                Some("Ack") => break,
                                Some("Ping") => send(&mut socket, json!({"_tag":"Pong"})),
                                _ => {}
                            }
                        }
                        _ => return,
                    }
                }
                shell_synchronized.store(true, Ordering::SeqCst);
                keep_alive(socket);
            }
            "orchestration.subscribeThread" => {
                thread_subscriptions.fetch_add(1, Ordering::SeqCst);
                while !shell_synchronized.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(5));
                }
                if !hold_thread_snapshot {
                    send(&mut socket, thread_snapshot_chunk());
                }
                keep_alive(socket);
            }
            "subscribeServerConfig" => {
                send(
                    &mut socket,
                    json!({
                        "_tag":"Chunk","requestId":"1","values":[
                            {"version":1,"type":"snapshot","config":{
                                "environment":{"environmentId":"ttfm","label":"TTFM fixture"},
                                "providers":[],
                            }},
                        ]
                    }),
                );
                keep_alive(socket);
            }
            "server.getConfig" => {
                send(
                    &mut socket,
                    json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{
                        "environment":{"environmentId":"ttfm","label":"TTFM fixture"},
                        "providers":[],
                    }}}),
                );
            }
            other => panic!("unexpected T3 request tag: {other}"),
        }
    }

    fn fake_t3(hold_thread_snapshot: bool) -> FakeT3 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let shell_synchronized = Arc::new(AtomicBool::new(false));
        let thread_subscriptions = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        {
            let shell_synchronized = shell_synchronized.clone();
            let thread_subscriptions = thread_subscriptions.clone();
            let stopped = stopped.clone();
            thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                while !stopped.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let shell_synchronized = shell_synchronized.clone();
                            let thread_subscriptions = thread_subscriptions.clone();
                            thread::spawn(move || {
                                serve_connection(
                                    stream,
                                    &shell_synchronized,
                                    &thread_subscriptions,
                                    hold_thread_snapshot,
                                );
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        FakeT3 {
            origin,
            thread_subscriptions,
            stopped,
        }
    }

    fn ttfm_lines(diagnostics: &Path) -> Vec<Value> {
        std::fs::read_to_string(diagnostics.join(LOG_FILE))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["phase"] == "manager_ttfm")
            .collect()
    }

    fn wait_for_ttfm_lines(diagnostics: &Path, count: usize) -> Vec<Value> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let lines = ttfm_lines(diagnostics);
            if lines.len() >= count {
                return lines;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {} manager_ttfm lines reached the diagnostic log",
                lines.len()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_until(ready: impl Fn() -> bool, context: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {context}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn field_ms(details: &Value, name: &str) -> u64 {
        let Some(ms) = details[name].as_u64() else {
            panic!("missing manager_ttfm field {name} in {details}");
        };
        ms
    }

    fn assert_monotonic(details: &Value) {
        let connect_start = field_ms(details, "connect_start_ms");
        let auth = field_ms(details, "auth_ms");
        let connect = field_ms(details, "connect_ms");
        let shell = field_ms(details, "shell_ms");
        let thread = field_ms(details, "thread_ms");
        let render = field_ms(details, "render_ms");
        let total = field_ms(details, "open_to_first_message_ms");
        for name in [
            "connect_start_ms",
            "auth_ms",
            "connect_ms",
            "shell_ms",
            "thread_ms",
            "render_ms",
            "open_to_first_message_ms",
        ] {
            let ms = field_ms(details, name);
            assert!(ms <= total, "{name}={ms} escapes the panel open at {total}");
        }
        assert!(
            auth <= connect,
            "auth_ms={auth} exceeds connect_ms={connect}"
        );
        assert!(
            connect <= shell,
            "connect_ms={connect} exceeds shell_ms={shell}"
        );
        assert!(
            connect <= thread,
            "connect_ms={connect} exceeds thread_ms={thread}"
        );
        assert!(
            thread <= render,
            "thread_ms={thread} exceeds render_ms={render}"
        );
        assert!(
            connect_start + render <= total + 2,
            "connect_start_ms={connect_start} plus render_ms={render} exceeds open_to_first_message_ms={total}"
        );
    }

    #[gpui::test]
    async fn manager_ttfm_logs_each_panel_open_once_with_monotonic_phases(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let fake = fake_t3(false);
        let temp = tempfile::tempdir().unwrap();
        let diagnostics = temp.path().join("reader-diagnostics");
        std::fs::create_dir_all(&diagnostics).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            cx.set_global(reader_diagnostics::LaunchTrace(
                reader_diagnostics::Trace::new(Some(diagnostics.clone()), None),
            ));
        });
        let environment = Environment::new("ttfm", &fake.origin).unwrap();
        let expiry = chrono::Utc::now().timestamp() as u64 + 3600;
        let token = NativeToken::from_secret(
            &environment,
            &serde_json::to_vec(&json!({
                "origin":fake.origin,"access_token":"ttfm-fixture-only",
                "expires_at":expiry,"scope":"orchestration:read"
            }))
            .unwrap(),
        )
        .unwrap();
        let client = NativeClient::new(environment.clone(), token, Duration::from_secs(2)).unwrap();
        let gateway = Arc::new(super::super::fixture::Fake::default());
        let (root, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                let mut view =
                    super::super::fixture::view(temp.path().to_owned(), gateway, window, cx);
                view.threads.enabled = true;
                view.active = false;
                view
            });
            Root::new(entity, window, cx)
        });
        visual.simulate_resize(size(px(760.), px(1000.)));
        visual.run_until_parked();
        let view = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ProjectView>().unwrap()
        });
        view.update_in(visual, |view, window, cx| {
            view.active = true;
            view.environment = Some(environment);
            view.install_native(Arc::new(client), expiry, window, cx);
        });
        visual
            .condition(&view, |view, _| view.native.title() == "TTFM manager")
            .await;
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
        let lines = wait_for_ttfm_lines(&diagnostics, 1);
        assert_eq!(lines.len(), 1, "one cold panel open logs exactly one line");
        let cold = &lines[0]["details"];
        assert_eq!(cold["reused_connection"], false);
        assert_monotonic(cold);
        for _ in 0..3 {
            visual.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
        thread::sleep(Duration::from_millis(200));
        assert_eq!(
            ttfm_lines(&diagnostics).len(),
            1,
            "further frames of the same open never log again"
        );
        view.update_in(visual, |view, window, cx| {
            view.set_active(false, window, cx);
            view.set_active(true, window, cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
        let lines = wait_for_ttfm_lines(&diagnostics, 2);
        let warm = &lines[1]["details"];
        assert_eq!(
            warm["reused_connection"], true,
            "reopening with a warm paired client is marked reused"
        );
        assert!(
            warm["open_to_first_message_ms"].as_u64().is_some(),
            "the warm reopen logs its own time to first message: {warm}"
        );
        thread::sleep(Duration::from_millis(200));
        visual.run_until_parked();
        assert_eq!(
            ttfm_lines(&diagnostics).len(),
            2,
            "two panel opens log exactly two manager_ttfm lines"
        );
        drop(view);
        drop(root);
        drop(fake);
    }

    #[gpui::test]
    async fn manager_ttfm_without_a_thread_snapshot_logs_no_line(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let fake = fake_t3(true);
        let temp = tempfile::tempdir().unwrap();
        let diagnostics = temp.path().join("reader-diagnostics");
        std::fs::create_dir_all(&diagnostics).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            cx.set_global(reader_diagnostics::LaunchTrace(
                reader_diagnostics::Trace::new(Some(diagnostics.clone()), None),
            ));
        });
        // Positive control: this exact trace writes other phases to the log.
        cx.update(|cx| {
            if let Some(trace) = reader_diagnostics::trace(cx) {
                trace.event("ttfm_probe", json!({"control": "log path works"}));
            }
        });
        let environment = Environment::new("ttfm", &fake.origin).unwrap();
        let expiry = chrono::Utc::now().timestamp() as u64 + 3600;
        let token = NativeToken::from_secret(
            &environment,
            &serde_json::to_vec(&json!({
                "origin":fake.origin,"access_token":"ttfm-fixture-only",
                "expires_at":expiry,"scope":"orchestration:read"
            }))
            .unwrap(),
        )
        .unwrap();
        let client = NativeClient::new(environment.clone(), token, Duration::from_secs(2)).unwrap();
        let gateway = Arc::new(super::super::fixture::Fake::default());
        let (root, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                let mut view =
                    super::super::fixture::view(temp.path().to_owned(), gateway, window, cx);
                view.threads.enabled = true;
                view.active = false;
                view
            });
            Root::new(entity, window, cx)
        });
        visual.simulate_resize(size(px(760.), px(1000.)));
        visual.run_until_parked();
        let view = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ProjectView>().unwrap()
        });
        view.update_in(visual, |view, window, cx| {
            view.active = true;
            view.environment = Some(environment);
            view.install_native(Arc::new(client), expiry, window, cx);
        });
        visual
            .condition(&view, |view, _| {
                view.threads
                    .model
                    .as_ref()
                    .is_some_and(|directory| directory.lock().unwrap().synchronized)
            })
            .await;
        let subscriptions = fake.thread_subscriptions.clone();
        wait_until(
            || subscriptions.load(Ordering::SeqCst) > 0,
            "the manager thread subscription to start",
        );
        let probe_path = diagnostics.join(LOG_FILE);
        wait_until(
            || {
                std::fs::read_to_string(&probe_path)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .any(|event| event["phase"] == "ttfm_probe")
            },
            "the control probe line in the shared diagnostic log",
        );
        // The subscription is live (server saw it, shell synchronized) but the
        // thread snapshot never arrives: several keepalive windows must not
        // produce a manager_ttfm line.
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(100));
            visual.run_until_parked();
            assert!(
                ttfm_lines(&diagnostics).is_empty(),
                "no thread snapshot ever arrived, yet a manager_ttfm line was logged"
            );
        }
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
        thread::sleep(Duration::from_millis(200));
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.native.title(),
                "",
                "positive control: no thread metadata was ever applied"
            );
        });
        assert!(
            ttfm_lines(&diagnostics).is_empty(),
            "a panel whose thread snapshot never arrives logs no open_to_first_message_ms"
        );
        drop(view);
        drop(root);
        drop(fake);
    }
}
