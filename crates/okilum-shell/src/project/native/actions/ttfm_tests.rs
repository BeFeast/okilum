//! Use the existing real-socket recovery fixture; no parallel fake transport.
use super::*;
use crate::reader_diagnostics;
use std::{path::Path, time::Instant};

fn events(path: &Path, phase: &str) -> Vec<Value> {
    std::fs::read_to_string(path.join("reader-diagnostic.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["phase"] == phase)
        .collect()
}
fn wait_events(path: &Path, phase: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = events(path, phase);
        if events.len() >= count {
            return events;
        }
        assert!(Instant::now() < deadline, "missing {phase}: {events:?}");
        thread::sleep(Duration::from_millis(5));
    }
}
fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    visual.run_until_parked();
}

#[gpui::test]
async fn manager_ttfm_records_real_paint_once_per_open_and_waits_for_snapshot(
    cx: &mut TestAppContext,
) {
    scenario(cx, true).await;
}

#[gpui::test]
async fn manager_ttfm_missing_snapshot_never_logs_a_success(cx: &mut TestAppContext) {
    scenario(cx, false).await;
}

async fn scenario(cx: &mut TestAppContext, deliver_snapshot: bool) {
    let diagnostics = tempfile::tempdir().unwrap();
    let trace = reader_diagnostics::Trace::new(Some(diagnostics.path().to_owned()), None);
    cx.update(|cx| cx.set_global(reader_diagnostics::LaunchTrace(trace.clone())));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (release, released) = mpsc::channel();
    let (stop, stopped) = mpsc::channel();
    let (subscribed, subscription) = futures::channel::oneshot::channel();
    let (warm_subscribed, warm_subscription) = futures::channel::oneshot::channel();
    let server = thread::spawn(move || {
        let sockets = RecoverySockets::new(listener);
        let (mut shell, request) = sockets.shells.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(request["tag"], "orchestration.subscribeShell");
        ping(&mut shell);
        chunk(
            &mut shell,
            json!([
                {"kind":"snapshot","snapshot":{"snapshotSequence":20,"threads":[
                    {"id":THREAD,"projectId":PROJECT,"title":"Manager","status":"idle"}
                ]}}, {"kind":"synchronized"}
            ]),
        );
        let mut manager = socket(&sockets, None);
        subscribed.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(10)).unwrap();
        if !deliver_snapshot {
            return;
        }
        let item = json!({"id":"ttfm-message","threadId":THREAD,"ordinal":1,
            "type":"assistant_message","text":"TTFM fixture message","status":"completed"});
        let mut value = snapshot();
        value["projection"]["thread"]["title"] = json!("TTFM fixture");
        value["projection"]["turnItems"] = json!([item]);
        value["projection"]["visibleTurnItems"] = json!([
            {"sourceThreadId":THREAD,"sourceItemId":"ttfm-message","visibility":"local","item":item}
        ]);
        chunk(&mut manager, json!([value, {"kind":"synchronized"}]));
        // Accept both warm subscriptions before test cleanup can cancel them.
        // Withhold snapshots: the second line must come from retained paint.
        let _warm_manager = socket(&sockets, Some(10));
        let (mut warm_shell, _) = sockets
            .shells
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        ping(&mut warm_shell);
        warm_subscribed.send(()).unwrap();
        stopped.recv_timeout(Duration::from_secs(10)).unwrap();
    });
    let (view, visual, _temp) = fixture(cx, &origin);
    view.update_in(visual, |view, window, cx| {
        view.active = true;
        let client = view.native.client.clone().unwrap();
        let expiry = view.native.expires_at.unwrap();
        view.install_native(client, expiry, window, cx);
    });
    subscription.await.unwrap();
    visual
        .condition(&view, |view, _| {
            view.threads
                .model
                .as_ref()
                .is_some_and(|model| model.lock().unwrap().synchronized)
        })
        .await;
    for _ in 0..3 {
        draw(visual);
    }
    // Drain the same diagnostic writer: absence cannot mean an unfired probe.
    trace.event("ttfm_control", json!({"subscribed_without_snapshot":true}));
    wait_events(diagnostics.path(), "ttfm_control", 1);
    assert!(events(diagnostics.path(), "manager_ttfm").is_empty());
    assert!(view.read_with(visual, |view, _| view.ttfm.needs_paint()));

    if !deliver_snapshot {
        // Finish an entire open without ever sending a Manager snapshot.
        view.update_in(visual, |view, window, cx| {
            view.set_active(false, window, cx)
        });
        release.send(()).unwrap();
        server.join().unwrap();
        trace.event("ttfm_control", json!({"closed_without_snapshot":true}));
        wait_events(diagnostics.path(), "ttfm_control", 2);
        assert!(events(diagnostics.path(), "manager_ttfm").is_empty());
        return;
    }
    release.send(()).unwrap();
    visual
        .condition(&view, |view, _| view.native.title() == "TTFM fixture")
        .await;
    draw(visual);
    assert!(
        visual.debug_bounds("project-message-1").is_some(),
        "real message row painted"
    );
    let cold = wait_events(diagnostics.path(), "manager_ttfm", 1);
    assert_eq!(cold.len(), 1);
    let details = &cold[0]["details"];
    let ms = |name: &str| {
        details[name]
            .as_f64()
            .unwrap_or_else(|| panic!("missing {name}: {details}"))
    };
    assert_eq!(details["reused_connection"], false);
    assert!(ms("connect_start_ms") <= ms("auth_ms"));
    assert!(ms("auth_ms") <= ms("connect_ms"));
    assert!(ms("connect_ms") <= ms("thread_ms"));
    assert!(ms("thread_ms") <= ms("render_ms"));
    assert!(ms("connect_start_ms") <= ms("shell_ms"));
    assert!(ms("shell_ms") <= ms("render_ms")); // fixture orders shell before thread
    assert_eq!(ms("render_ms"), ms("open_to_first_message_ms"));
    assert_eq!(
        details.as_object().unwrap().len(),
        8,
        "timings and reuse flag only"
    );
    assert!(!cold[0].to_string().contains("TTFM fixture message"));
    for _ in 0..3 {
        draw(visual);
    }

    let reopened = Instant::now();
    view.update_in(visual, |view, window, cx| {
        view.set_active(false, window, cx);
        view.set_active_since(true, reopened, window, cx);
        // Repeated activation without closing must not create another open.
        view.set_active(true, window, cx);
    });
    draw(visual);
    warm_subscription.await.unwrap();
    let warm = wait_events(diagnostics.path(), "manager_ttfm", 2);
    assert_eq!(warm.len(), 2);
    assert_eq!(warm[1]["details"]["reused_connection"], true);
    assert!(
        warm[1]["details"]["thread_ms"].is_null(),
        "no old snapshot timestamp on reopen"
    );
    assert!(
        warm[1]["details"]["shell_ms"].is_null(),
        "withheld warm shell snapshot"
    );
    assert!(
        warm[1]["details"]["open_to_first_message_ms"]
            .as_f64()
            .unwrap()
            <= reopened.elapsed().as_secs_f64() * 1000.
    );
    assert_eq!(warm[1]["details"].as_object().unwrap().len(), 8);
    for _ in 0..3 {
        draw(visual);
    }
    trace.event("ttfm_control", json!({"frames_after_reopen":true}));
    wait_events(diagnostics.path(), "ttfm_control", 2);
    assert_eq!(events(diagnostics.path(), "manager_ttfm").len(), 2);
    view.update_in(visual, |view, window, cx| {
        view.set_active(false, window, cx)
    });
    stop.send(()).unwrap();
    server.join().unwrap();
}
