use super::*;
use ::core::prelude::v1::test;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    thread,
};
use tungstenite::Message;
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
    cx.update(gpui_component::init);
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
    cx.update(gpui_component::init);
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
    cx.update(gpui_component::init);
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
