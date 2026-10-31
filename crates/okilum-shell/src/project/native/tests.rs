use super::*;
use ::core::prelude::v1::test;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    thread,
};
use tungstenite::Message;

#[gpui::test]
fn scrolling_to_the_oldest_row_loads_a_real_history_page_without_losing_the_anchor(
    cx: &mut TestAppContext,
) {
    use okilum_t3::{projection::Projection, stream::ThreadUpdate};
    use std::sync::atomic::{AtomicUsize, Ordering};
    cx.update(crate::test_init);
    let thread_id = "mcp:fixture-manager";
    let message = |n: u64| json!({"id":format!("history-{n}"),"threadId":thread_id,"ordinal":n,"type":"assistant_message","text":format!("Message {n}\n\nRetained history"),"status":"completed","runId":null,"nodeId":null});
    let row = |item: Value| json!({"sourceThreadId":thread_id,"sourceItemId":item["id"],"visibility":"local","item":item});
    let page: Vec<_> = (0..50).map(|n| row(message(n))).collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        assert!(request.starts_with("GET "), "history never sends a command");
        assert!(request.contains("/history?cursor=older-page"));
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        drop(reader);
        counter.fetch_add(1, Ordering::SeqCst);
        let body = json!({"items":page,"nextCursor":null,"hasMoreHistory":false}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    let environment = Environment::new("history", &origin).unwrap();
    let expiry = chrono::Utc::now().timestamp() as u64 + 3600;
    let token = NativeToken::from_secret(&environment,&serde_json::to_vec(&json!({"origin":origin,"access_token":"fixture-readonly","expires_at":expiry,"scope":"orchestration:read"})).unwrap()).unwrap();
    let client =
        Arc::new(NativeClient::new(environment.clone(), token, Duration::from_secs(2)).unwrap());
    let items: Vec<_> = (50..100).map(message).collect();
    let mut model = Projection::new(thread_id, "fixture-managers");
    model.apply(&[ThreadUpdate::Snapshot {sequence:10,projection:json!({"thread":{"id":thread_id,"projectId":"fixture-managers","title":"History"},"runs":[],"attempts":[],"turnItems":items,"visibleTurnItems":items.iter().cloned().map(row).collect::<Vec<_>>()}),history_cursor:Some("older-page".into()),has_more_history:true,payload_budget_exceeded:false,latest_local_turn_ordinal:Some(99)}]).unwrap();
    let changes = model.take_changes();
    // Native positions are allocated from the unordered changed-row batch,
    // independently of chronological order. Track the actual oldest message.
    let oldest_position = changes
        .rows
        .iter()
        .position(|row| row.key.item == "history-50")
        .unwrap()
        + 1;
    let oldest_selector: &'static str =
        Box::leak(format!("project-message-{oldest_position}").into_boxed_str());
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.environment = Some(environment);
            view.native.client = Some(client);
            view.native.metadata = changes.metadata.clone().unwrap();
            view.native.retained = Some(Arc::new(Mutex::new(Retained {
                model,
                cursor: None,
                valid: true,
                epoch: 0,
            })));
            view.feed
                .update(cx, |feed, cx| feed.apply_native(changes, &[], cx));
            view
        });
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(900.)));
    visual.run_until_parked();
    assert!(
        requests.load(Ordering::SeqCst) == 0,
        "the live edge must not eagerly download old pages"
    );
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    let history = visual.debug_bounds("project-history").unwrap();
    for _ in 0..16 {
        if requests.load(Ordering::SeqCst) > 0 {
            break;
        }
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: history.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(4000.))),
            ..Default::default()
        });
        visual.run_until_parked();
    }
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "reaching old history loads exactly one page"
    );
    assert!(
        visual.debug_bounds(oldest_selector).is_some(),
        "the previously oldest message remains the read anchor after prepending"
    );
    view.read_with(visual, |view, _| {
        assert!(!view.native.metadata.has_more_history);
        assert_eq!(
            view.native
                .retained
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .model
                .keys()
                .len(),
            100
        );
    });
    server.join().unwrap();
}

#[gpui::test]
fn open_in_t3_loads_native_identity_and_only_dispatches_the_exact_desktop_thread(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
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
            json!({"ticket":"fixture-ticket","expiresAt":"2099-01-01T00:00:00Z"}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        drop(stream);
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let request: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["tag"], "server.getConfig");
        socket.send(Message::Text(json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"environment":{"environmentId":"actual-remote-env","label":"Server"}}}}).to_string().into())).unwrap();
    });
    let environment = Environment::new("local-alias-is-not-an-id", &origin).unwrap();
    let expiry = chrono::Utc::now().timestamp() as u64 + 3600;
    let token = NativeToken::from_secret(&environment, &serde_json::to_vec(&json!({"origin":origin,"access_token":"fixture-only","expires_at":expiry,"scope":"orchestration:read"})).unwrap()).unwrap();
    let client =
        Arc::new(NativeClient::new(environment.clone(), token, Duration::from_secs(2)).unwrap());
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.environment = Some(environment);
            view.identity = None;
            view.native.client = Some(client);
            view
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let button = visual
        .debug_bounds("project-open-t3")
        .expect("native-only handoff without MCP identity");
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        visual.opened_url().as_deref(),
        Some("t3code://threads/actual-remote-env/mcp:fixture-manager")
    );
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    view.read_with(visual, |view, _| {
        assert_eq!(
            view.identity.as_ref().unwrap().environment_id,
            "actual-remote-env"
        )
    });
    server.join().unwrap();
}

#[test]
fn retry_backoff_is_bounded_and_classifies_auth_or_protocol_failures_as_terminal() {
    assert_eq!(delay(1), Duration::from_secs(1));
    assert_eq!(delay(4), Duration::from_secs(8));
    assert_eq!(delay(100), Duration::from_secs(15));
    assert!(terminal(&StreamError::PairingRequired));
    assert!(terminal(&StreamError::Protocol("schema")));
    assert!(!terminal(&StreamError::Unreachable));
    assert!(terminal(&StreamError::Http(400)));
    assert!(terminal(&StreamError::Http(302)));
    assert!(!terminal(&StreamError::Http(429)));
    assert!(!terminal(&StreamError::Http(503)));
}
#[gpui::test]
fn live_snapshot_and_flush_reach_the_real_feed_without_an_mcp_history_poll(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            let mut view = super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            view.identity = Some(ServerIdentity {
                environment_id: "fixture".into(),
                label: "Fixture".into(),
            });
            view
        });
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(1000.)));
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    let (thread_id, project_id) = view.read_with(visual, |view, _| {
        (
            view.chat.as_ref().unwrap().thread_id.clone(),
            view.binding
                .as_ref()
                .unwrap()
                .manager
                .project_id
                .as_ref()
                .unwrap()
                .clone(),
        )
    });
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
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
        let body = json!({"ticket":"test-ticket","expiresAt":"2099-01-01T00:00:00Z"}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        drop(stream);
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        let request: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["payload"]["threadId"], thread_id);
        let item = json!({"id":"a","threadId":thread_id,"ordinal":1,"type":"assistant_message","text":"Initial native reply","status":"running","runId":null,"nodeId":null});
        let snapshot = json!({"kind":"snapshot","snapshotSequence":10,"projection":{"thread":{"id":thread_id,"projectId":project_id,"title":"Live manager"},"runs":[],"attempts":[],"turnItems":[item],"visibleTurnItems":[{"sourceThreadId":thread_id,"sourceItemId":"a","visibility":"local","position":0,"item":item}]}});
        let mut phase = 0;
        loop {
            let request: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            match request["_tag"].as_str().unwrap() {
                "Ping" => {
                    ws.send(Message::Text(json!({"_tag":"Pong"}).to_string().into()))
                        .unwrap();
                    if phase == 0 {
                        ws.send(Message::Text(json!({"_tag":"Chunk","requestId":"1","values":[snapshot,{"kind":"synchronized"}]}).to_string().into())).unwrap();
                        phase = 1;
                    }
                }
                "Ack" if phase == 1 => {
                    let mut changed = item.clone();
                    changed["text"] = json!("Streamed native reply");
                    let command = json!({"id":"cmd","threadId":thread_id,"ordinal":2,"type":"command_execution","input":"cargo test","status":"completed","runId":null,"nodeId":null});
                    ws.send(Message::Text(json!({"_tag":"Chunk","requestId":"1","values":[{"kind":"event","sequence":11,"event":{"type":"turn-item.updated","threadId":thread_id,"payload":changed}},{"kind":"event","sequence":12,"event":{"type":"turn-item.updated","threadId":thread_id,"payload":command}}]}).to_string().into())).unwrap();
                    phase = 2;
                }
                "Ack" if phase == 2 => break,
                _ => panic!("unexpected native controller frame"),
            }
        }
    });
    let environment = Environment::new("fixture", &origin).unwrap();
    let token=NativeToken::from_secret(&environment,&serde_json::to_vec(&json!({"origin":origin,"access_token":"fixture-only","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read"})).unwrap()).unwrap();
    let client = NativeClient::new(environment, token, Duration::from_secs(2)).unwrap();
    view.update_in(visual, |view, window, cx| {
        view.active = true;
        view.install_native(
            Arc::new(client),
            chrono::Utc::now().timestamp() as u64 + 3600,
            window,
            cx,
        );
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        let model = view.native.retained.as_ref().unwrap().lock().unwrap();
        assert_eq!(model.cursor.as_ref().unwrap().sequence(), 12);
        assert_eq!(model.model.metadata.title, "Live manager");
    });
    assert!(
        visual.debug_bounds("project-tool-2").is_some(),
        "native command did not reach the real feed"
    );
    view.update(visual, |view, _| view.native.pause());
    server.join().unwrap();
}

#[gpui::test]
fn pairing_token_keeps_the_full_row_width_in_a_narrow_panel(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            view.native.open = true;
            view.connection_details = true;
            view
        });
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(320.), px(1000.)));
    visual.run_until_parked();
    let bounds = visual
        .debug_bounds("project-live-token-row")
        .expect("pairing input row missing");
    assert!(
        bounds.size.width > px(240.) && bounds.right() <= px(320.),
        "pairing input shrank or escaped the panel: {bounds:?}"
    );
    drop(root);
}

#[gpui::test]
fn deleted_live_thread_disables_send_with_a_visible_reason(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let env = Environment::new("fixture", "https://native.invalid").unwrap();
    let token=NativeToken::from_secret(&env,&serde_json::to_vec(&json!({"origin":env.origin(),"access_token":"fixture-only","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read orchestration:operate"})).unwrap()).unwrap();
    let client = Arc::new(NativeClient::new(env, token, Duration::from_secs(2)).unwrap());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::fixture::Fake::default()),
                window,
                cx,
            );
            view.active = false;
            view.identity = Some(ServerIdentity {
                environment_id: "fixture".into(),
                label: "Fixture".into(),
            });
            view.native.client = Some(client);
            view.native.stream_ready = true; // Fixture represents an accepted live snapshot.
            assert!(
                view.can_send(),
                "positive control: valid manager send was already disabled"
            );
            view.native.metadata.deleted = true;
            assert!(!view.can_send());
            view
        });
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(1000.)));
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-live-deleted").is_some(),
        "deleted live thread has no visible reason"
    );
    drop(root);
}
