use super::*;
use ::core::prelude::v1::test;
use okilum_t3::stream::NativeToken;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, PartialEq, Eq)]
struct WarmCatalogBaseline {
    requests: usize,
    catalog: gpui::EntityId,
    epoch: u64,
    providers: usize,
}

fn serve_catalog_snapshot(listener: &TcpListener, counted: &AtomicUsize) {
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
    write!(
        http,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    drop(http);
    let (tcp, _) = listener.accept().unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut socket = tungstenite::accept(tcp).unwrap();
    let request: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(request["tag"], "subscribeServerConfig");
    counted.fetch_add(1, Ordering::SeqCst);
    let models: Vec<_> = (0..292)
        .map(|n| json!({"slug":format!("model-{n}"),"name":format!("Model {n}")}))
        .collect();
    socket
        .send(tungstenite::Message::Text(
            json!({
                "_tag":"Chunk",
                "requestId":"1",
                "values":[{
                    "version":1,
                    "type":"snapshot",
                    "config":{
                        "environment":{"environmentId":"qa-a","label":"QA"},
                        "providers":[{
                            "enabled":true,
                            "instanceId":"account-a",
                            "driver":"codex",
                            "displayName":"Work",
                            "models":models
                        }]
                    }
                }]
            })
            .to_string()
            .into(),
        ))
        .unwrap();
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
}

fn warm_catalog_baseline(
    view: &Entity<ProjectView>,
    visual: &mut VisualTestContext,
    requests: &AtomicUsize,
) -> WarmCatalogBaseline {
    view.read_with(visual, |view, cx| {
        let catalog = view
            .native
            .catalog
            .as_ref()
            .expect("warm picker retains shared catalog");
        let state = catalog.read(cx);
        assert!(
            !state.loading && state.data.is_some() && !view.model_busy,
            "warmup settled before repeated opens"
        );
        WarmCatalogBaseline {
            requests: requests.load(Ordering::SeqCst),
            catalog: catalog.entity_id(),
            epoch: state.epoch,
            providers: state.data.as_ref().unwrap().providers.as_ptr() as usize,
        }
    })
}

fn check_warm_picker_open(
    view: &Entity<ProjectView>,
    visual: &mut VisualTestContext,
    requests: &AtomicUsize,
    baseline: &WarmCatalogBaseline,
) -> Result<(), String> {
    if visual.debug_bounds("project-model-choice-1").is_none() {
        return Err("usable model rows, not just a spinner, are painted".into());
    }
    let actual_requests = requests.load(Ordering::SeqCst);
    if actual_requests != baseline.requests {
        return Err(format!(
            "warm picker open sent extra server request: expected {}, got {actual_requests}",
            baseline.requests
        ));
    }
    let (catalog_id, epoch, loading, model_busy, providers) = view.read_with(visual, |view, cx| {
        let catalog = view
            .native
            .catalog
            .as_ref()
            .expect("warm picker retains shared catalog");
        let state = catalog.read(cx);
        let providers = state
            .data
            .as_ref()
            .map(|snapshot| snapshot.providers.as_ptr() as usize)
            .unwrap_or(0);
        (
            catalog.entity_id(),
            state.epoch,
            state.loading,
            view.model_busy,
            providers,
        )
    });
    if catalog_id != baseline.catalog {
        return Err("warm picker replaced shared catalog entity".into());
    }
    if epoch != baseline.epoch || loading || model_busy || providers != baseline.providers {
        return Err(format!(
            "warm picker refetched or rebuilt catalog: epoch={epoch} (expected {}), loading={loading}, model_busy={model_busy}",
            baseline.epoch
        ));
    }
    Ok(())
}

fn assert_warm_picker_open(
    view: &Entity<ProjectView>,
    visual: &mut VisualTestContext,
    requests: &AtomicUsize,
    baseline: &WarmCatalogBaseline,
) {
    if let Err(error) = check_warm_picker_open(view, visual, requests, baseline) {
        panic!("{error}");
    }
}

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
        serve_catalog_snapshot(&listener, &counted);
        listener.set_nonblocking(true).unwrap();
        assert!(
            listener.accept().is_err(),
            "open/close sent another transport request"
        );
        listener
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
    let baseline = warm_catalog_baseline(&view, visual, &requests);
    assert_eq!(baseline.epoch, 1, "warmup completed in one catalog epoch");
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
        assert_warm_picker_open(&view, visual, &requests, &baseline);
        timings.push(start.elapsed().as_millis());
        view.update(visual, |view, cx| {
            view.model_open = false;
            cx.notify();
        });
        visual.run_until_parked();
    }
    diagnostic_error!("MODEL2207 GPUI first/repeated click→usable rows ms={timings:?}; 292 models; headless regression, not idunn compositor timing");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "ten open/close interactions sent zero RPC"
    );
    view.update(visual, |view, cx| {
        view.native
            .catalog
            .as_ref()
            .unwrap()
            .update(cx, |catalog, _| {
                catalog.worker.take();
                catalog.cancel.take().unwrap().cancel();
            });
    });
    let listener = server.join().unwrap();
    listener.set_nonblocking(false).unwrap();
    let refetch_counted = requests.clone();
    let refetch_server = std::thread::spawn(move || {
        serve_catalog_snapshot(&listener, &refetch_counted);
    });
    let trigger = visual.debug_bounds("project-manager-model").unwrap();
    visual.simulate_click(trigger.center(), Modifiers::default());
    view.update_in(visual, |view, window, cx| {
        view.model_request(None, window, cx);
    });
    visual.run_until_parked();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_warm_picker_open(&view, visual, &requests, &baseline);
    }))
    .expect_err("positive control: forcing a catalog refetch on open must fail the warm-open work assertion");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.contains("warm picker"),
        "unexpected panic message: {message}"
    );
    visual
        .condition(&view, |view, cx| {
            let state = view.native.catalog.as_ref().unwrap().read(cx);
            state.epoch == 2 && !state.loading && requests.load(Ordering::SeqCst) == 2
        })
        .await;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_warm_picker_open(&view, visual, &requests, &baseline);
    }))
    .expect_err(
        "positive control: completed refetch on open still fails request/epoch/rebuild assertion",
    );
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.contains("extra server request"),
        "unexpected panic message: {message}"
    );
    view.update(visual, |view, _| view.native.clear());
    visual.run_until_parked();
    refetch_server.join().unwrap();
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
