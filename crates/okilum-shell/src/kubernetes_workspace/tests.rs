//! GPUI coverage with a synthetic transport (no cluster, kubeconfig or
//! credentials) and a process-free log worker: no real kubectl ever runs.
use super::model::tests as fixtures;
use super::*;
use crate::reader_log_kubernetes::worker;
use ::core::prelude::v1::test;
use gpui::{TestAppContext, VisualTestContext};
use okilum_core::kubernetes::Kind;
use okilum_core::kubernetes::{Cleanup, Client, End, Limits, Raw, Transport};
use okilum_core::log::kubernetes::Request;
use okilum_core::log::stream::Assembler;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

type Calls = Arc<Mutex<Vec<String>>>;

struct Fake {
    routes: Vec<(&'static str, Raw)>,
    calls: Calls,
}

impl Transport for Fake {
    fn run(
        &self,
        args: &[String],
        _: Limits,
        _: &okilum_core::kubernetes::Cancel,
    ) -> std::io::Result<Raw> {
        let line = args.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        self.routes
            .iter()
            .find(|(key, _)| line.contains(key))
            .map(|(_, raw)| raw.clone())
            .ok_or_else(|| std::io::Error::other(format!("no synthetic route for {line}")))
    }
}

fn ok(value: Value) -> Raw {
    Raw {
        stdout: value.to_string().into_bytes(),
        stderr: Vec::new(),
        stderr_truncated: false,
        end: End::Exited { code: Some(0) },
        cleanup: Cleanup::NotNeeded,
    }
}

fn fail(stderr: &str) -> Raw {
    Raw {
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
        stderr_truncated: false,
        end: End::Exited { code: Some(1) },
        cleanup: Cleanup::NotNeeded,
    }
}

fn list(kind: &str, items: Vec<Value>) -> Value {
    json!({"kind": format!("{kind}List"), "apiVersion": "v1",
           "metadata": {"resourceVersion": "900"}, "items": items})
}

fn pod_json(name: &str, uid: &str) -> Value {
    // A single-object GET carries kind/apiVersion; list items may omit them.
    json!({
        "kind": "Pod",
        "apiVersion": "v1",
        "metadata": fixtures::meta(name, uid, fixtures::owner("ReplicaSet", "checkout-7d9f", "rs-1"),
                                   json!({"app": "checkout"})),
        "spec": {"initContainers": [{"name": "proxy", "image": "registry.invalid/proxy:2",
                                     "restartPolicy": "Always"}],
                 "containers": [{"name": "api", "image": "registry.invalid/api:3"}]},
        "status": {"phase": "Running",
            "initContainerStatuses": [{"name": "proxy", "ready": true, "restartCount": 0,
                "state": {"running": {}}, "lastState": {}}],
            "containerStatuses": [{"name": "api", "ready": true, "restartCount": 3,
                "state": {"running": {}},
                "lastState": {"terminated": {"exitCode": 137, "reason": "OOMKilled"}}}]}
    })
}

fn contexts() -> Value {
    json!({
        "current-context": "prod-east",
        "contexts": [
            {"name": "prod-east", "context": {"cluster": "east", "namespace": "shop"}},
            {"name": "staging", "context": {"cluster": "stage"}}
        ],
        "clusters": [
            {"name": "east", "cluster": {"server": "https://east.invalid"}},
            {"name": "stage", "cluster": {"server": "https://stage.invalid"}}
        ]
    })
}

fn routes(pods: Raw, get_uid: &str) -> Vec<(&'static str, Raw)> {
    vec![
        ("config view", ok(contexts())),
        ("/pods?limit=", pods),
        ("/pods/checkout-a", ok(pod_json("checkout-a", get_uid))),
        (
            "/deployments?",
            ok(list(
                "Deployment",
                vec![json!({
                    "metadata": fixtures::meta("checkout", "dep-1", json!([]), json!({})),
                    "spec": {"replicas": 2, "selector": {"matchLabels": {"app": "checkout"}}},
                    "status": {"observedGeneration": 1, "replicas": 2, "readyReplicas": 2}
                })],
            )),
        ),
        (
            "/replicasets?",
            ok(list(
                "ReplicaSet",
                vec![json!({
                    "metadata": fixtures::meta("checkout-7d9f", "rs-1",
                        fixtures::owner("Deployment", "checkout", "dep-1"), json!({})),
                    "spec": {"replicas": 2}, "status": {"replicas": 2, "readyReplicas": 2}
                })],
            )),
        ),
        ("/services?", ok(list("Service", vec![]))),
        ("/endpointslices?", ok(list("EndpointSlice", vec![]))),
        ("/events?", ok(list("Event", vec![]))),
        (
            "/horizontalpodautoscalers?",
            fail("Error from server (Forbidden): horizontalpodautoscalers is forbidden"),
        ),
    ]
}

fn standard_pods() -> Raw {
    ok(list(
        "Pod",
        vec![
            pod_json("checkout-a", "pod-a"),
            pod_json("checkout-b", "pod-b"),
        ],
    ))
}

struct Started {
    requests: Vec<Request>,
    senders: Vec<async_channel::Sender<worker::Update>>,
    cancelled: Vec<Arc<AtomicBool>>,
}

struct Harness<'a> {
    workspace: Entity<Workspace>,
    visual: &'a mut VisualTestContext,
    calls: Calls,
    started: Rc<RefCell<Started>>,
}

fn open<'a>(cx: &'a mut TestAppContext, routes: Vec<(&'static str, Raw)>) -> Harness<'a> {
    cx.update(|cx| {
        crate::test_init(cx);
        reader_log::bind_keys(cx);
        bind_keys(cx);
    });
    let calls: Calls = Arc::default();
    let reader: Arc<dyn backend::Reader> = Arc::new(Client::new(
        Fake {
            routes,
            calls: calls.clone(),
        },
        Limits::default(),
    ));
    let started = Rc::new(RefCell::new(Started {
        requests: Vec::new(),
        senders: Vec::new(),
        cancelled: Vec::new(),
    }));
    let mut workspace = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            let mut ws = Workspace::new("fixture-vault".into(), reader, window, cx);
            let record = started.clone();
            ws.dock.starter = Rc::new(move |request: Request, _scope: String| {
                let (worker, flag) = worker::Worker::detached();
                let (sender, receiver) = async_channel::unbounded();
                let mut record = record.borrow_mut();
                record.requests.push(request);
                record.senders.push(sender);
                record.cancelled.push(flag);
                (worker, receiver)
            });
            ws
        });
        workspace = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1320.), px(900.)));
    let mut harness = Harness {
        workspace: workspace.unwrap(),
        visual,
        calls,
        started,
    };
    harness.wait("contexts read", |ws| ws.contexts.is_some());
    harness
}

impl Harness<'_> {
    /// Fires the UI side's poll timer and runs everything it scheduled.
    fn settle(&mut self) {
        self.visual.run_until_parked();
        self.visual.executor().advance_clock(backend::POLL * 2);
        self.visual.run_until_parked();
    }
    /// Reads run on OS threads (as in production); poll their completion.
    fn wait(&mut self, what: &str, done: impl Fn(&Workspace) -> bool) {
        for _ in 0..400 {
            self.settle();
            if self.workspace.read_with(&*self.visual, |ws, _| done(ws)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn update<R>(
        &mut self,
        f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
    ) -> R {
        self.workspace.update_in(self.visual, f)
    }
    fn load(&mut self, namespace: &str) {
        let namespace = namespace.to_string();
        self.update(|ws, window, cx| {
            ws.namespace
                .update(cx, |input, cx| input.set_value(namespace, window, cx));
            ws.load(cx);
        });
        self.wait("load", |ws| ws.loading.is_none());
    }
    fn click(&mut self, selector: &'static str) {
        self.visual.run_until_parked();
        let bounds = self
            .visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is rendered"));
        self.visual
            .simulate_click(bounds.center(), Modifiers::default());
        self.settle();
    }
    fn send(&mut self, index: usize, bytes: &[u8], status: worker::Status) {
        let mut assembler = Assembler::new("pod.log".into(), format!("fixture-{index}")).unwrap();
        let snapshot = assembler.append(bytes).unwrap();
        let sender = self.started.borrow().senders[index].clone();
        sender
            .send_blocking(worker::Update {
                snapshot,
                status,
                diagnostic: String::new(),
                pending: 0,
            })
            .unwrap();
        self.settle();
    }
}

#[gpui::test]
fn selection_and_scope_changes_never_start_or_retarget_logs(cx: &mut TestAppContext) {
    let mut h = open(cx, routes(standard_pods(), "pod-a"));
    assert_eq!(h.calls().len(), 1, "opening reads kubeconfig contexts only");
    assert!(h.calls()[0].contains("config view"));
    assert!(
        h.workspace.read_with(h.visual, |ws, _| ws.data.is_none()),
        "nothing reads the cluster before Load"
    );
    h.load("");
    let calls = h.calls();
    assert_eq!(calls.len(), 8, "contexts + one page per kind: {calls:?}");
    assert!(calls[1..]
        .iter()
        .all(|c| c.starts_with("--context=prod-east ") && c.contains("/namespaces/shop/")));
    h.workspace.read_with(h.visual, |ws, _| {
        let data = ws.data.as_ref().unwrap();
        assert_eq!(data.scope.namespace, "shop", "the context's namespace");
        assert_eq!(ws.rows.len(), 2);
        assert_eq!(ws.rows[0].ready.as_deref(), Some("2/2"));
        assert_eq!(ws.rows[0].restarts, Some(3));
        assert_eq!(ws.rows[0].reasons, ["api: last: OOMKilled (exit 137)"]);
        // A forbidden HPA read is a failure of that source only.
        assert!(model::freshness(&data.hpas, std::time::SystemTime::now()).0 == "forbidden");
        assert!(model::latest(&data.pods).is_some());
    });

    // Selecting rows and switching sections reads nothing and starts nothing.
    h.click("k8s-row-0");
    h.click("k8s-section-Containers");
    h.click("k8s-row-1");
    assert_eq!(h.calls().len(), 8, "selection never runs kubectl");
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(ws.dock.session.is_none());
        assert!(matches!(
            ws.selected.as_ref(),
            Some(Key::Container { container, pod_uid, .. }) if container == "api" && pod_uid == "pod-a"
        ));
    });

    // Open logs pins the exact target, verifies the UID, then starts.
    h.click("k8s-open-logs-api");
    h.wait("verified start", |ws| {
        ws.dock
            .session
            .as_ref()
            .is_some_and(|s| s.phase == dock::Phase::Receiving)
    });
    assert!(h
        .calls()
        .last()
        .unwrap()
        .contains("--raw=/api/v1/namespaces/shop/pods/checkout-a"));
    {
        let started = h.started.borrow();
        assert_eq!(started.requests.len(), 1);
        let args = started.requests[0].args().unwrap();
        for expected in [
            "--context=prod-east",
            "--namespace=shop",
            "pod/checkout-a",
            "--container=api",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected} in {args:?}");
        }
        assert!(args.iter().any(|a| a == "--previous=false"));
    }
    h.send(0, b"first line\nsecond line\n", worker::Status::Reading);
    let view = h.workspace.read_with(h.visual, |ws, cx| {
        let view = ws.dock.view.clone().expect("rows arrived");
        assert_eq!(
            view.read(cx).stream_bottom(),
            Some(true),
            "follows the bottom"
        );
        view
    });

    // Another row, another section, another namespace: the session stays.
    h.click("k8s-section-Pods");
    h.click("k8s-row-1");
    h.load("payments");
    h.workspace.read_with(h.visual, |ws, _| {
        let session = ws.dock.session.as_ref().unwrap();
        assert_eq!(session.target.pod, "checkout-a");
        assert_eq!(session.target.pod_uid, "pod-a");
        assert_eq!(session.target.namespace, "shop");
        assert_eq!(session.phase, dock::Phase::Receiving);
        assert!(ws.dock.worker_running());
        assert_eq!(ws.dock.view.as_ref().unwrap().entity_id(), view.entity_id());
        assert_eq!(ws.data.as_ref().unwrap().scope.namespace, "payments");
    });
    assert!(!h.started.borrow().cancelled[0].load(Ordering::Acquire));
    assert_eq!(h.started.borrow().requests.len(), 1);

    // Following the bottom and receiving are independent.
    h.click("k8s-dock-follow-bottom");
    h.workspace.read_with(h.visual, |ws, cx| {
        assert_eq!(view.read(cx).stream_bottom(), Some(false));
        assert!(ws.dock.worker_running(), "receiving continues");
    });
    h.send(0, b"third line\n", worker::Status::Reading);
    assert_eq!(
        view.read_with(h.visual, |v, _| v.stream_bottom()),
        Some(false)
    );

    // A different target needs an explicit retarget.
    h.load("shop");
    h.click("k8s-section-Containers");
    h.click("k8s-row-1");
    h.click("k8s-open-previous-api");
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(ws.dock.pending_retarget.as_ref().unwrap().previous);
        assert!(!ws.dock.session.as_ref().unwrap().target.previous);
    });
    assert_eq!(
        h.started.borrow().requests.len(),
        1,
        "not started before confirming"
    );
    h.click("k8s-dock-retarget-confirm");
    h.wait("retarget start", |ws| {
        ws.dock
            .session
            .as_ref()
            .is_some_and(|s| s.target.previous && s.phase == dock::Phase::Receiving)
    });
    {
        let started = h.started.borrow();
        assert!(
            started.cancelled[0].load(Ordering::Acquire),
            "old session stopped"
        );
        assert!(started.requests[1]
            .args()
            .unwrap()
            .contains(&"--previous=true".to_string()));
    }
    // Old rows stay until the new session sends.
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(!ws.dock.displayed.as_ref().unwrap().previous);
        assert_eq!(ws.dock.view.as_ref().unwrap().entity_id(), view.entity_id());
    });
    h.send(1, b"previous instance\n", worker::Status::Complete);
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(ws.dock.displayed.as_ref().unwrap().previous);
        assert_eq!(ws.dock.session.as_ref().unwrap().phase, dock::Phase::Ended);
        assert!(!ws.dock.worker_running());
    });
}

#[gpui::test]
fn replaced_pod_is_detected_before_and_after_start(cx: &mut TestAppContext) {
    // The single-Pod read before start returns another UID: nothing starts.
    let mut h = open(cx, routes(standard_pods(), "pod-a2"));
    h.load("");
    h.click("k8s-section-Containers");
    h.click("k8s-row-1");
    h.click("k8s-open-logs-api");
    h.wait("verification", |ws| {
        ws.dock
            .session
            .as_ref()
            .is_some_and(|s| !matches!(s.phase, dock::Phase::Verifying))
    });
    h.workspace.read_with(h.visual, |ws, _| {
        assert_eq!(
            ws.dock.session.as_ref().unwrap().phase,
            dock::Phase::Replaced {
                uid: "pod-a2".into()
            }
        );
        assert!(ws
            .uid_note()
            .unwrap()
            .0
            .contains("now refers to Pod UID pod-a2"));
    });
    assert!(
        h.started.borrow().requests.is_empty(),
        "no kubectl logs by name"
    );
}

#[gpui::test]
fn replacement_after_start_is_disclosed_from_the_next_pods_read(cx: &mut TestAppContext) {
    let mut h = open(cx, routes(standard_pods(), "pod-a"));
    h.load("");
    h.click("k8s-section-Containers");
    h.click("k8s-row-1");
    h.click("k8s-open-logs-api");
    h.wait("start", |ws| ws.dock.worker_running());
    h.workspace.read_with(h.visual, |ws, _| {
        let (note, tone) = ws.uid_note().unwrap();
        assert!(note.contains("verified at") && note.contains("by name"));
        assert_eq!(tone, Tone::Neutral);
    });
    h.update(|ws, _, cx| {
        let data = ws.data.as_mut().unwrap();
        fixtures::put(
            &mut data.pods,
            vec![fixtures::recovered_pod("checkout-a", "pod-a9", "rs-1")],
        );
        ws.refresh_rows(cx);
    });
    h.workspace.read_with(h.visual, |ws, _| {
        let (note, tone) = ws.uid_note().unwrap();
        assert!(note.contains("pod-a9") && note.contains("may come from the new Pod"));
        assert_eq!(tone, Tone::Warning);
        assert!(ws.dock.worker_running(), "disclosed, not silently stopped");
    });
}

#[gpui::test]
fn forbidden_pods_are_not_an_empty_namespace(cx: &mut TestAppContext) {
    let mut h = open(
        cx,
        routes(
            fail("Error from server (Forbidden): pods is forbidden: User cannot list pods"),
            "pod-a",
        ),
    );
    h.load("");
    h.visual.run_until_parked();
    assert!(h.visual.debug_bounds("k8s-list-empty").is_some());
    h.workspace.read_with(h.visual, |ws, _| {
        let data = ws.data.as_ref().unwrap();
        assert!(model::empty_reason(&data.pods, "Pods", &data.scope).contains("Forbidden"));
        assert!(
            ws.notice.is_none(),
            "forbidden is per source, not a sign-in problem"
        );
    });
}

#[gpui::test]
fn an_empty_namespace_is_a_successful_read(cx: &mut TestAppContext) {
    let mut h = open(cx, routes(ok(list("Pod", vec![])), "pod-a"));
    h.load("");
    h.workspace.read_with(h.visual, |ws, _| {
        let data = ws.data.as_ref().unwrap();
        assert!(model::empty_reason(&data.pods, "Pods", &data.scope).contains("successful read"));
    });
}

#[gpui::test]
fn interactive_sign_in_is_surfaced_at_once_and_stops_reading(cx: &mut TestAppContext) {
    let mut h = open(
        cx,
        routes(
            fail("error: exec plugin: interactive login required: open https://login.invalid to sign in with MFA"),
            "pod-a",
        ),
    );
    h.load("");
    let calls = h.calls();
    h.workspace.read_with(h.visual, |ws, _| {
        let notice = ws.notice.as_deref().unwrap_or_default();
        assert!(
            notice.contains("sign-in") || notice.contains("Credentials"),
            "{notice}"
        );
    });
    assert_eq!(
        calls.len(),
        2,
        "no further reads after an auth failure: {calls:?}"
    );
}

#[gpui::test]
fn workspace_layout_keeps_regions_apart_in_light_and_dark(cx: &mut TestAppContext) {
    let mut h = open(cx, routes(standard_pods(), "pod-a"));
    h.load("");
    h.click("k8s-row-0");
    // Inspector tabs: Events and the typed Manifest.
    let diag = format!(
        "tab {:?} tabs {:?} inspector {:?} title {:?}",
        h.visual.debug_bounds("k8s-tab-Manifest"),
        h.visual.debug_bounds("k8s-inspector-tabs"),
        h.visual.debug_bounds("k8s-inspector"),
        h.visual.debug_bounds("k8s-inspector-title"),
    );
    h.click("k8s-tab-Manifest");
    h.workspace.read_with(h.visual, |ws, _| {
        assert_eq!(ws.tab, Tab::Manifest, "tab click: {diag}")
    });
    assert!(h.visual.debug_bounds("k8s-manifest").is_some());
    h.click("k8s-tab-Events");
    assert!(h.visual.debug_bounds("k8s-manifest").is_none());
    h.click("k8s-tab-Overview");
    for mode in [
        gpui_component::ThemeMode::Light,
        gpui_component::ThemeMode::Dark,
    ] {
        h.visual.update(|_, cx| {
            gpui_component::Theme::change(mode, None, cx);
            brand::apply_theme(cx);
        });
        h.visual.run_until_parked();
        let mut get = |id: &'static str| {
            h.visual
                .debug_bounds(id)
                .unwrap_or_else(|| panic!("{id} rendered in {mode:?}"))
        };
        let scope = get("k8s-scope");
        let list = get("k8s-list");
        let inspector = get("k8s-inspector");
        let dock = get("k8s-dock");
        let status = get("k8s-status");
        assert!(scope.bottom() <= list.top());
        assert!(
            list.right() <= inspector.left() + px(1.),
            "list | inspector"
        );
        assert!(list.bottom() <= dock.top() + px(1.), "dock below the split");
        assert!(dock.bottom() <= status.top() + px(1.));
        assert!(dock.size.height >= px(200.));
        assert!(list.size.width >= px(300.) && inspector.size.width >= px(400.));
        assert!(h.visual.debug_bounds("k8s-inspector-title").is_some());
        assert!(h.visual.debug_bounds("k8s-dock-empty").is_some());
    }
}

#[gpui::test]
fn list_keys_navigate_only_while_the_list_has_focus(cx: &mut TestAppContext) {
    let mut h = open(cx, routes(standard_pods(), "pod-a"));
    h.load("");
    h.click("k8s-row-0");
    h.visual.simulate_keystrokes("j");
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(matches!(&ws.selected, Some(Key::Object { name, .. }) if name == "checkout-b"));
    });
    h.visual.simulate_keystrokes("k");
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(matches!(&ws.selected, Some(Key::Object { name, .. }) if name == "checkout-a"));
    });
    // `/` moves focus to the filter; typed `j` is text there, not navigation.
    h.visual.simulate_keystrokes("/");
    h.visual.simulate_keystrokes("j");
    h.workspace.read_with(h.visual, |ws, cx| {
        assert_eq!(ws.filter.read(cx).value().as_ref(), "j");
        assert!(matches!(&ws.selected, Some(Key::Object { name, .. }) if name == "checkout-a"));
    });
    assert_eq!(h.calls().len(), 8, "keys never read the cluster");
}

// ---------- Triage and Service path (#3011) ----------

/// Earlier routes win: the Fake takes the first route the argv contains.
fn with(
    mut routes: Vec<(&'static str, Raw)>,
    key: &'static str,
    raw: Raw,
) -> Vec<(&'static str, Raw)> {
    routes.insert(0, (key, raw));
    routes
}

fn service_list() -> Raw {
    ok(list(
        "Service",
        vec![json!({
            "metadata": fixtures::meta("checkout", "svc-1", json!([]), json!({})),
            "spec": {"type": "ClusterIP", "clusterIP": "10.96.0.10", "selector": {"app": "checkout"},
                     "ports": [{"name": "http", "port": 80, "protocol": "TCP", "targetPort": 8080}]}
        })],
    ))
}

fn slice_list(endpoints: &[(&str, bool)]) -> Raw {
    let endpoints: Vec<Value> = endpoints
        .iter()
        .enumerate()
        .map(|(i, (uid, ready))| {
            json!({"addresses": [format!("10.0.0.{}", i + 10)],
                   "conditions": {"ready": ready, "serving": ready, "terminating": false},
                   "targetRef": {"kind": "Pod", "name": format!("pod-{uid}"), "namespace": "shop", "uid": uid}})
        })
        .collect();
    ok(list(
        "EndpointSlice",
        vec![json!({
            "metadata": fixtures::meta("checkout-abc", "slice-1", json!([]),
                                       json!({"kubernetes.io/service-name": "checkout"})),
            "addressType": "IPv4",
            "endpoints": endpoints,
            "ports": [{"name": "http", "port": 8080, "protocol": "TCP"}]
        })],
    ))
}

fn path_routes(slices: Raw) -> Vec<(&'static str, Raw)> {
    with(
        with(
            routes(standard_pods(), "pod-a"),
            "/services?",
            service_list(),
        ),
        "/endpointslices?",
        slices,
    )
}

#[gpui::test]
fn triage_keeps_recovered_rows_stable_and_never_resolves_them_silently(cx: &mut TestAppContext) {
    let mut h = open(
        cx,
        with(
            routes(standard_pods(), "pod-a"),
            "/pods/checkout-b",
            ok(pod_json("checkout-b", "pod-b")),
        ),
    );
    h.load("");
    let reads = h.calls().len();
    h.click("k8s-lens-triage");
    h.workspace.read_with(h.visual, |ws, _| {
        assert_eq!(ws.lens, Lens::Triage);
        let titles: Vec<_> = ws
            .rows
            .iter()
            .map(|r| (r.title.as_str(), r.status.as_str()))
            .collect();
        assert_eq!(
            titles,
            [
                ("api", "Recovered · prior failure kept"),
                ("api", "Recovered · prior failure kept")
            ],
            "Ready Pods keep their retained OOMKilled; healthy sidecars are not rows"
        );
        assert!(ws.rows[0].reasons[0].contains("Last termination: OOMKilled (exit 137)"));
        assert!(ws.rows[0].subtitle.contains("3 restarts (cumulative)"));
    });
    assert!(h.visual.debug_bounds("k8s-triage").is_some());
    assert!(h.visual.debug_bounds("k8s-triage-rules").is_some());
    h.click("k8s-row-1");
    let chosen = h
        .workspace
        .read_with(h.visual, |ws, _| ws.selected.clone().unwrap());
    assert!(
        matches!(&chosen, Key::Container { pod_uid, container, .. } if pod_uid == "pod-b" && container == "api")
    );
    // The shared inspector's explicit action pins the previous instance.
    h.click("k8s-open-previous-api");
    h.wait("previous logs start", |ws| ws.dock.worker_running());
    {
        let started = h.started.borrow();
        let args = started.requests[0].args().unwrap();
        assert!(args.contains(&"pod/checkout-b".to_string()));
        assert!(args.contains(&"--previous=true".to_string()));
    }
    // Refresh: reordered, plus a new failing Pod listed first. The selected
    // observation and the session stay; only the order of others changes.
    h.update(|ws, _, cx| {
        let data = ws.data.as_mut().unwrap();
        fixtures::put(
            &mut data.pods,
            vec![
                fixtures::crashing_pod("checkout-z", "pod-z", "rs-1"),
                fixtures::recovered_pod("checkout-b", "pod-b", "rs-1"),
                fixtures::recovered_pod("checkout-a", "pod-a", "rs-1"),
            ],
        );
        ws.refresh_rows(cx);
    });
    h.settle();
    h.workspace.read_with(h.visual, |ws, _| {
        assert_eq!(ws.rows[0].subtitle.split(" · ").next(), Some("checkout-z"));
        assert_eq!(ws.selected.as_ref(), Some(&chosen));
        assert!(ws.rows.iter().any(|r| r.key == chosen));
        assert!(ws.dock.worker_running(), "refresh never stops the session");
    });
    assert!(h.visual.debug_bounds("k8s-triage-selection-note").is_none());
    // The selected container stops matching any rule (lastState no longer
    // reported). It stays selected, is not called resolved, and says so.
    h.update(|ws, _, cx| {
        let data = ws.data.as_mut().unwrap();
        fixtures::put(
            &mut data.pods,
            vec![lenses::tests::healthy_pod("checkout-b", "pod-b", "rs-1")],
        );
        ws.refresh_rows(cx);
    });
    h.settle();
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(ws.rows.is_empty());
        assert_eq!(ws.selected.as_ref(), Some(&chosen));
    });
    assert!(h.visual.debug_bounds("k8s-triage-selection-note").is_some());
    // Lens switches and selections read nothing; only the UID check did.
    assert_eq!(h.calls().len(), reads + 1);
}

#[gpui::test]
fn triage_denied_pods_is_unknown_not_zero(cx: &mut TestAppContext) {
    let mut h = open(
        cx,
        routes(
            fail("Error from server (Forbidden): pods is forbidden"),
            "pod-a",
        ),
    );
    h.load("");
    h.click("k8s-lens-triage");
    assert!(h.visual.debug_bounds("k8s-triage-unknown").is_some());
    h.workspace
        .read_with(h.visual, |ws, _| assert!(ws.rows.is_empty()));
}

#[gpui::test]
fn service_path_follows_routing_and_selects_in_the_shared_inspector(cx: &mut TestAppContext) {
    let mut h = open(
        cx,
        path_routes(slice_list(&[
            ("pod-a", true),
            ("pod-b", false),
            ("pod-gone", false),
        ])),
    );
    h.load("");
    let reads = h.calls().len();
    h.click("k8s-lens-service-path");
    assert!(
        h.visual.debug_bounds("k8s-path-empty").is_some(),
        "no Service chosen yet"
    );
    h.click("k8s-row-0");
    for id in [
        "k8s-path-service",
        "k8s-path-slice-0",
        "k8s-path-endpoint-0-0",
        "k8s-path-endpoint-0-1",
        "k8s-path-endpoint-0-2",
        "k8s-path-endpoint-0-0-api",
        "k8s-path-endpoint-0-0-proxy",
        "k8s-path-facts",
    ] {
        assert!(h.visual.debug_bounds(id).is_some(), "{id} rendered");
    }
    let service = h
        .workspace
        .read_with(h.visual, |ws, _| ws.path_service.clone().unwrap());
    // A target Pod opens in the same inspector; the path stays on its Service.
    let geometry = format!(
        "node {:?} band {:?} list {:?} dock {:?} chip {:?}",
        h.visual.debug_bounds("k8s-path-endpoint-0-1"),
        h.visual.debug_bounds("k8s-path"),
        h.visual.debug_bounds("k8s-list"),
        h.visual.debug_bounds("k8s-dock"),
        h.visual.debug_bounds("k8s-path-endpoint-0-1-api"),
    );
    h.click("k8s-path-endpoint-0-1");
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(
            matches!(&ws.selected, Some(Key::Object { kind: Kind::Pod, uid, .. }) if uid == "pod-b"),
            "selected {:?}; {geometry}",
            ws.selected
        );
        assert_eq!(ws.path_service.as_ref(), Some(&service));
        assert_eq!(ws.lens, Lens::ServicePath);
    });
    assert!(h.visual.debug_bounds("k8s-path-service").is_some());
    // A container node selects that container; logs start only explicitly.
    h.click("k8s-path-endpoint-0-0-api");
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(matches!(&ws.selected, Some(Key::Container { pod_uid, container, .. }) if pod_uid == "pod-a" && container == "api"));
        assert!(ws.dock.session.is_none(), "selecting never pins logs");
    });
    assert_eq!(h.calls().len(), reads, "path clicks read nothing");
    h.click("k8s-open-logs-api");
    h.wait("explicit pin", |ws| ws.dock.worker_running());
    let args = h.started.borrow().requests[0].args().unwrap();
    assert!(
        args.contains(&"pod/checkout-a".to_string())
            && args.contains(&"--container=api".to_string())
    );
    // Another endpoint's container: a retarget asks first, the session stays.
    h.click("k8s-path-endpoint-0-1-api");
    h.click("k8s-open-logs-api");
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(ws.dock.pending_retarget.is_some());
        assert_eq!(ws.dock.session.as_ref().unwrap().target.pod, "checkout-a");
    });
    // Switching lens keeps the selection and the session.
    h.click("k8s-lens-workspace");
    h.workspace.read_with(h.visual, |ws, _| {
        assert!(matches!(&ws.selected, Some(Key::Container { pod_uid, .. }) if pod_uid == "pod-b"));
        assert!(ws.dock.worker_running());
    });
}

#[gpui::test]
fn service_path_unknown_zero_and_missing_targets_read_differently(cx: &mut TestAppContext) {
    let mut h = open(
        cx,
        path_routes(fail(
            "Error from server (Forbidden): endpointslices.discovery.k8s.io is forbidden",
        )),
    );
    h.load("");
    h.click("k8s-lens-service-path");
    h.click("k8s-row-0");
    assert!(h.visual.debug_bounds("k8s-path-slices-unknown").is_some());
    assert!(h.visual.debug_bounds("k8s-path-no-slices").is_none());
    h.workspace.read_with(h.visual, |ws, _| {
        assert_eq!(ws.rows[0].status, "Endpoints unknown (forbidden)");
    });
    // A readable but empty slice list is zero endpoints, said as such.
    h.update(|ws, _, cx| {
        fixtures::put(&mut ws.data.as_mut().unwrap().slices, vec![]);
        ws.refresh_rows(cx);
    });
    h.settle();
    assert!(h.visual.debug_bounds("k8s-path-no-slices").is_some());
    assert!(h.visual.debug_bounds("k8s-path-slices-unknown").is_none());
}

#[gpui::test]
fn lenses_lay_out_in_light_and_dark_and_tabs_click_after_an_app_theme_switch(
    cx: &mut TestAppContext,
) {
    let mut h = open(
        cx,
        path_routes(slice_list(&[("pod-a", true), ("pod-b", false)])),
    );
    h.load("");
    h.click("k8s-row-0");
    for mode in [
        gpui_component::ThemeMode::Dark,
        gpui_component::ThemeMode::Light,
    ] {
        // The app's own path (sync_appearance): change with the window, so
        // it refreshes, then apply the brand tokens.
        h.visual.update(|window, cx| {
            gpui_component::Theme::change(mode, Some(window), cx);
            brand::apply_theme(cx);
        });
        h.settle();
        for (lens, band) in [
            ("k8s-lens-triage", "k8s-triage"),
            ("k8s-lens-service-path", "k8s-path"),
        ] {
            h.click(lens);
            if lens == "k8s-lens-service-path" {
                h.click("k8s-row-0");
            }
            let get = |h: &mut Harness, id: &'static str| {
                h.visual
                    .debug_bounds(id)
                    .unwrap_or_else(|| panic!("{id} rendered in {mode:?}"))
            };
            let filter = get(&mut h, "k8s-lenses");
            let band = get(&mut h, band);
            let list = get(&mut h, "k8s-list");
            let inspector = get(&mut h, "k8s-inspector");
            let dock = get(&mut h, "k8s-dock");
            let status = get(&mut h, "k8s-status");
            assert!(filter.bottom() <= band.top() + px(1.), "{lens}");
            assert!(
                band.bottom() <= list.top() + px(1.),
                "{lens}: band above the split"
            );
            assert!(list.right() <= inspector.left() + px(1.));
            assert!(list.bottom() <= dock.top() + px(1.));
            assert!(
                status.bottom() <= px(900.) + px(1.),
                "{lens}: status line stays in the window"
            );
            assert!(
                list.size.height >= px(190.),
                "{lens}: list keeps its height"
            );
            // An inspector tab still takes clicks after the theme switch.
            h.click("k8s-tab-Manifest");
            h.workspace.read_with(h.visual, |ws, _| {
                assert_eq!(ws.tab, Tab::Manifest, "{lens} {mode:?}")
            });
            h.click("k8s-tab-Overview");
            h.workspace.read_with(h.visual, |ws, _| {
                assert_eq!(ws.tab, Tab::Overview, "{lens} {mode:?}")
            });
        }
        h.click("k8s-lens-workspace");
    }
}
