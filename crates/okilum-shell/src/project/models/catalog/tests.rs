use super::*;
use ::core::prelude::v1::test;
use okilum_t3::stream::NativeToken;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::atomic::{AtomicUsize, Ordering},
};

#[gpui::test]
async fn authenticated_warmup_is_shared_and_repeated_picker_opens_read_only_memory(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let server = std::thread::spawn(move || {
        let (mut http, _) = listener.accept().unwrap();
        http.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(&mut http);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("POST /api/auth/websocket-ticket "));
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        drop(reader);
        let body = r#"{"ticket":"catalog-test-ticket"}"#;
        write!(http,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        drop(http);
        let (tcp, _) = listener.accept().unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut socket = tungstenite::accept(tcp).unwrap();
        let request: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["tag"], "subscribeServerConfig");
        counted.fetch_add(1, Ordering::SeqCst);
        let models: Vec<_> = (0..292)
            .map(|n| json!({"slug":format!("model-{n}"),"name":format!("Model {n}")}))
            .collect();
        socket.send(tungstenite::Message::Text(json!({"_tag":"Chunk","requestId":"1","values":[{
            "version":1,"type":"snapshot","config":{"environment":{"environmentId":"qa-a","label":"QA"},
            "providers":[{"enabled":true,"instanceId":"account-a","driver":"codex","displayName":"Work","models":models}]}
        }]}).to_string().into())).unwrap();
        loop {
            let Ok(message) = socket.read() else {
                break;
            };
            let Some(text) = message.to_text().ok() else {
                continue;
            };
            let value: Value = serde_json::from_str(text).unwrap();
            match value["_tag"].as_str() {
                Some("Ping") => {
                    if socket
                        .send(tungstenite::Message::Text(r#"{"_tag":"Pong"}"#.into()))
                        .is_err()
                    {
                        break;
                    }
                }
                Some("Ack") => {}
                _ => panic!("catalog warmup must never dispatch a thread mutation"),
            }
        }
        listener.set_nonblocking(true).unwrap();
        assert!(
            listener.accept().is_err(),
            "open/close sent another transport request"
        );
    });
    let temp = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = super::super::super::fixture::view(temp.path().into(), Arc::new(super::super::super::fixture::Fake::default()), window, cx);
            view.active = false;
            let environment = Environment::new("catalog", &origin).unwrap();
            let token = NativeToken::from_secret(&environment, &serde_json::to_vec(&json!({"origin":origin,"access_token":"test-catalog-token","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read orchestration:operate"})).unwrap()).unwrap();
            let client = Arc::new(NativeClient::new(environment.clone(),token,Duration::from_secs(2)).unwrap());
            view.environment = Some(environment);
            view.native.set_detail_test_client(client.clone());
            view.native.retain_metadata(okilum_t3::projection::Metadata {
                thread_id: "mcp:fixture-manager".into(), model: "model-0".into(),
                model_selection: Some(ModelSelection { instance_id:"account-a".into(),model:"model-0".into(),options:vec![] }),
                runtime_mode: Some("full-access".into()), interaction_mode:Some("default".into()),..Default::default()
            });
            view.ensure_native_catalog(cx);
            let again = Catalog::acquire(client, cx);
            assert_eq!(view.native.catalog.as_ref().unwrap().entity_id(), again.entity_id(),"all Manager views share one authenticated warmup");
            again.update(cx, |catalog,cx| { let epoch = catalog.epoch; for _ in 0..8 { catalog.refresh(cx); } assert_eq!(catalog.epoch,epoch,"inflight warmup coalesces signals"); });
            view
        });
        Root::new(view,window,cx)
    });
    visual.simulate_resize(size(px(760.), px(1000.)));
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual
        .condition(&view, |view, _| view.models.is_some())
        .await;
    view.update(visual, |view, cx| {
        view.active = true;
        cx.notify();
    });
    visual.run_until_parked();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "positive control: authenticated warmup reached the server"
    );
    let mut timings = Vec::new();
    for _ in 0..10 {
        let trigger = visual.debug_bounds("project-manager-model").unwrap();
        let start = std::time::Instant::now();
        visual.simulate_click(trigger.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-model-choice-1").is_some(),
            "usable model rows, not just a spinner, are painted"
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(300),
            "warm rendered picker readiness: {elapsed:?}"
        );
        timings.push(elapsed.as_millis());
        view.update(visual, |view, cx| {
            view.model_open = false;
            cx.notify();
        });
        visual.run_until_parked();
    }
    eprintln!("MODEL2207 GPUI first/repeated click→usable rows ms={timings:?}; 292 models; headless regression, not idunn compositor timing");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "ten open/close interactions sent zero RPC"
    );
    view.update(visual, |view, _| view.native.clear());
    visual.run_until_parked();
    server.join().unwrap();
}

#[gpui::test]
fn catalog_rejects_old_connection_and_replaces_environment_and_provider_instances(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let catalog = cx.new(|_| {
        let env = Environment::new("test", "http://127.0.0.1:1").unwrap();
        let token = NativeToken::from_secret(&env, &serde_json::to_vec(&json!({"origin":env.origin(),"access_token":"scope-test","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read"})).unwrap()).unwrap();
        Catalog { data: None, loading: false, error: None, client:Arc::new(NativeClient::new(env,token,Duration::from_millis(10)).unwrap()), epoch: 2, cancel: None, worker: None }
    });
    let snapshot = |environment: &str, instance: &str| model_catalog::Snapshot {
        environment: ServerIdentity {
            environment_id: environment.into(),
            label: environment.into(),
        },
        providers: vec![okilum_t3::ProviderCapability {
            provider_instance_id: instance.into(),
            driver_kind: "codex".into(),
            display_name: None,
            models: vec![okilum_t3::ModelInfo {
                id: "same-slug".into(),
                label: None,
                ..Default::default()
            }],
            runtime_modes: vec![],
        }],
    };
    catalog.update(cx, |catalog, cx| {
        assert_eq!(
            catalog.apply_batch(
                1,
                vec![model_catalog::Update::Snapshot(snapshot(
                    "foreign",
                    "foreign-account"
                ))],
                cx
            ),
            None
        );
        assert!(
            catalog.data.is_none(),
            "late warmup from an old connection must not populate this scope"
        );
        assert_eq!(
            catalog.apply_batch(
                2,
                vec![model_catalog::Update::Snapshot(snapshot(
                    "environment-a",
                    "work-account"
                ))],
                cx
            ),
            Some(false)
        );
        assert_eq!(
            catalog.data.as_ref().unwrap().providers[0].provider_instance_id,
            "work-account"
        );
        assert_eq!(
            catalog.apply_batch(
                2,
                vec![
                    model_catalog::Update::Refresh,
                    model_catalog::Update::Refresh,
                    model_catalog::Update::Refresh
                ],
                cx
            ),
            Some(true),
            "a config event batch starts only one re-snapshot"
        );
        assert!(catalog.loading);
        assert!(
            catalog.data.is_some(),
            "known catalog stays visible while refreshing"
        );
        catalog.epoch = 3;
        catalog.apply_batch(
            3,
            vec![model_catalog::Update::Snapshot(snapshot(
                "environment-b",
                "personal-account",
            ))],
            cx,
        );
        assert!(!catalog.loading);
        catalog.apply_batch(
            2,
            vec![model_catalog::Update::Snapshot(snapshot(
                "environment-a",
                "work-account",
            ))],
            cx,
        );
        let known = catalog.data.as_ref().unwrap();
        assert_eq!(known.environment.environment_id, "environment-b");
        assert_eq!(known.providers.len(), 1);
        assert_eq!(
            known.providers[0].provider_instance_id, "personal-account",
            "same-driver accounts and foreign catalogs are never merged"
        );
    });
}
