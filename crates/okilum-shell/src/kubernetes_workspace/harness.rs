//! Native visual harness (#3012), only with the non-publishing
//! `kubernetes-ui-harness` feature. `OKILUM_KUBERNETES_HARNESS=<lens>:<mode>`
//! (lens `workspace`, `triage` or `service-path`; mode `light` or `dark`)
//! opens the real Workspace window with synthetic in-process data instead of
//! kubectl, drives it to a representative state through the ordinary
//! methods (Load, lens, selection, Open logs), then writes the file named by
//! `OKILUM_KUBERNETES_HARNESS_READY` so a capture script can screenshot it.
//! With `OKILUM_KUBERNETES_HARNESS_EXPORT=<dir>` it also writes an evidence
//! bundle there through the real export path. No process is started and no
//! cluster, kubeconfig or credential is read.
use super::backend::Reader;
use super::lenses::Lens;
use super::model::{Key, LogTarget};
use super::*;
use crate::reader_log_kubernetes::worker;
use okilum_core::kubernetes::{
    Cancel, Completeness, Connection, Deployment, EndpointSlice, Event, HorizontalPodAutoscaler,
    KubeContext, List, Namespace, Observation, Pod, ReplicaSet, Resource, Service, Source,
};
use serde_json::{json, Value};
use std::time::SystemTime;

const CONTEXT_NAME: &str = "demo-east";
const NAMESPACE: &str = "commerce";

fn meta(name: &str, uid: &str, owners: Value, labels: Value) -> Value {
    json!({"name": name, "namespace": NAMESPACE, "uid": uid, "resourceVersion": "4711",
           "creationTimestamp": "2026-11-16T09:00:00Z",
           "labels": labels, "ownerReferences": owners})
}

fn owner(kind: &str, name: &str, uid: &str) -> Value {
    json!([{"apiVersion": "apps/v1", "kind": kind, "name": name, "uid": uid, "controller": true}])
}

fn pod(name: &str, uid: &str, api: Value, last: Value, ready: bool, restarts: u32) -> Value {
    json!({
        "kind": "Pod", "apiVersion": "v1",
        "metadata": meta(name, uid, owner("ReplicaSet", "checkout-api-7d8c9", "rs-demo"),
                         json!({"app": "checkout-api"})),
        "spec": {"nodeName": "worker-a",
            "initContainers": [{"name": "telemetry", "image": "example.invalid/telemetry:1.2",
                                "restartPolicy": "Always"}],
            "containers": [{"name": "api", "image": "example.invalid/checkout:2.8.1",
                "resources": {"requests": {"cpu": "250m", "memory": "256Mi"}, "limits": {"memory": "512Mi"}},
                "readinessProbe": {"httpGet": {"path": "/ready", "port": 8080}, "periodSeconds": 5}}]},
        "status": {"phase": "Running", "podIP": "10.42.1.18", "qosClass": "Burstable",
            "initContainerStatuses": [{"name": "telemetry", "ready": true, "started": true,
                "restartCount": 0, "state": {"running": {"startedAt": "2026-11-16T09:01:00Z"}}, "lastState": {}}],
            "containerStatuses": [{"name": "api", "ready": ready, "restartCount": restarts,
                "image": "example.invalid/checkout:2.8.1",
                "imageID": "example.invalid/checkout@sha256:0000000000000000",
                "state": api, "lastState": last}]}
    })
}

fn oom() -> Value {
    json!({"terminated": {"exitCode": 137, "reason": "OOMKilled",
        "startedAt": "2026-11-16T14:05:00Z", "finishedAt": "2026-11-16T14:21:58Z"}})
}

fn pods() -> Vec<Value> {
    vec![
        pod(
            "checkout-api-7d8c9-q7k2b",
            "uid-q7k2b",
            json!({"waiting": {"reason": "CrashLoopBackOff", "message": "back-off 2m40s"}}),
            oom(),
            false,
            3,
        ),
        pod(
            "checkout-api-7d8c9-r4v8n",
            "uid-r4v8n",
            json!({"running": {"startedAt": "2026-11-16T14:22:00Z"}}),
            oom(),
            true,
            1,
        ),
        pod(
            "checkout-api-7d8c9-t6w3m",
            "uid-t6w3m",
            json!({"running": {"startedAt": "2026-11-16T09:01:00Z"}}),
            json!({}),
            true,
            0,
        ),
    ]
}

fn decode<T: Resource>(values: Vec<Value>) -> Vec<T> {
    values
        .into_iter()
        .filter_map(|v| T::decode(v).ok())
        .collect()
}

pub(crate) struct FixtureReader;

impl FixtureReader {
    fn source(c: &Connection, ns: &str, kind: okilum_core::kubernetes::Kind) -> Source {
        Source {
            context: c.context.clone(),
            cluster: c.cluster.clone(),
            server: c.server.clone(),
            namespace: Some(ns.into()),
            kind,
        }
    }
    fn list<T: Resource>(
        c: &Connection,
        ns: &str,
        items: Vec<Value>,
    ) -> Result<Observation<List<T>>, Failure> {
        Ok(Observation {
            source: Self::source(c, ns, T::KIND),
            observed_at: SystemTime::now(),
            value: List {
                items: decode(items),
                resource_version: Some(okilum_core::kubernetes::ResourceVersion("9001".into())),
                completeness: Completeness::Complete,
                rejected: Vec::new(),
                rejected_total: 0,
            },
        })
    }
}

type Read<T> = Result<Observation<List<T>>, Failure>;

impl Reader for FixtureReader {
    fn contexts(&self, _: &Cancel) -> Result<Contexts, Failure> {
        Ok(Contexts {
            current: Some(CONTEXT_NAME.into()),
            contexts: vec![KubeContext {
                name: CONTEXT_NAME.into(),
                cluster: Some("demo".into()),
                server: Some("https://demo.invalid:6443".into()),
                namespace: Some(NAMESPACE.into()),
            }],
        })
    }
    fn namespaces(&self, _: &Connection, _: &Cancel) -> Read<Namespace> {
        Err(Failure::Forbidden {
            detail: "namespaces is forbidden (synthetic)".into(),
        })
    }
    fn pods(&self, c: &Connection, ns: &str, _: &Cancel) -> Read<Pod> {
        Self::list(c, ns, pods())
    }
    fn deployments(&self, c: &Connection, ns: &str, _: &Cancel) -> Read<Deployment> {
        Self::list(
            c,
            ns,
            vec![json!({
                "metadata": meta("checkout-api", "dep-demo", json!([]), json!({"app": "checkout-api"})),
                "spec": {"replicas": 3, "selector": {"matchLabels": {"app": "checkout-api"}}},
                "status": {"observedGeneration": 18, "replicas": 3, "readyReplicas": 2,
                    "availableReplicas": 2, "updatedReplicas": 3, "unavailableReplicas": 1,
                    "conditions": [{"type": "Available", "status": "True", "reason": "MinimumReplicasAvailable"},
                                   {"type": "Progressing", "status": "True", "reason": "NewReplicaSetAvailable"}]}
            })],
        )
    }
    fn replica_sets(&self, c: &Connection, ns: &str, _: &Cancel) -> Read<ReplicaSet> {
        Self::list(
            c,
            ns,
            vec![json!({
                "metadata": meta("checkout-api-7d8c9", "rs-demo", owner("Deployment", "checkout-api", "dep-demo"),
                                 json!({"app": "checkout-api", "pod-template-hash": "7d8c9"})),
                "spec": {"replicas": 3, "selector": {"matchLabels": {"app": "checkout-api"}}},
                "status": {"replicas": 3, "readyReplicas": 2, "availableReplicas": 2}
            })],
        )
    }
    fn services(&self, c: &Connection, ns: &str, _: &Cancel) -> Read<Service> {
        Self::list(
            c,
            ns,
            vec![json!({
                "metadata": meta("checkout-api", "svc-demo", json!([]), json!({})),
                "spec": {"type": "ClusterIP", "clusterIP": "10.96.12.40", "selector": {"app": "checkout-api"},
                         "ports": [{"name": "https", "port": 443, "protocol": "TCP", "targetPort": 8080}]}
            })],
        )
    }
    fn slices(&self, c: &Connection, ns: &str, _: &Cancel) -> Read<EndpointSlice> {
        let endpoint = |ip: &str, uid: &str, name: &str, ready: bool| {
            json!({"addresses": [ip], "conditions": {"ready": ready, "serving": ready, "terminating": false},
                   "nodeName": "worker-a",
                   "targetRef": {"kind": "Pod", "name": name, "namespace": NAMESPACE, "uid": uid}})
        };
        Self::list(
            c,
            ns,
            vec![json!({
                "metadata": meta("checkout-api-x9f2", "slice-demo", json!([]),
                                 json!({"kubernetes.io/service-name": "checkout-api"})),
                "addressType": "IPv4",
                "endpoints": [
                    endpoint("10.42.1.18", "uid-q7k2b", "checkout-api-7d8c9-q7k2b", false),
                    endpoint("10.42.2.11", "uid-r4v8n", "checkout-api-7d8c9-r4v8n", true),
                    endpoint("10.42.3.20", "uid-t6w3m", "checkout-api-7d8c9-t6w3m", true),
                ],
                "ports": [{"name": "https", "port": 8080, "protocol": "TCP"}]
            })],
        )
    }
    fn events(&self, c: &Connection, ns: &str, _: &Cancel) -> Read<Event> {
        let event = |uid: &str,
                     name: &str,
                     reason: &str,
                     kind: &str,
                     count: i32,
                     at: &str,
                     msg: &str| {
            json!({"metadata": meta(&format!("{name}.{reason}"), &format!("ev-{uid}-{reason}"), json!([]), json!({})),
                   "involvedObject": {"kind": "Pod", "name": name, "namespace": NAMESPACE, "uid": uid},
                   "type": kind, "reason": reason, "message": msg, "count": count, "lastTimestamp": at,
                   "source": {"component": "kubelet"}})
        };
        Self::list(
            c,
            ns,
            vec![
                event(
                    "uid-q7k2b",
                    "checkout-api-7d8c9-q7k2b",
                    "BackOff",
                    "Warning",
                    14,
                    "2026-11-16T14:31:56Z",
                    "Back-off restarting failed container api",
                ),
                event(
                    "uid-r4v8n",
                    "checkout-api-7d8c9-r4v8n",
                    "Started",
                    "Normal",
                    2,
                    "2026-11-16T14:22:00Z",
                    "Started container api",
                ),
            ],
        )
    }
    fn hpas(&self, c: &Connection, ns: &str, _: &Cancel) -> Read<HorizontalPodAutoscaler> {
        Self::list(
            c,
            ns,
            vec![json!({
                "metadata": meta("checkout-api", "hpa-demo", json!([]), json!({})),
                "spec": {"scaleTargetRef": {"kind": "Deployment", "name": "checkout-api", "apiVersion": "apps/v1"},
                         "minReplicas": 3, "maxReplicas": 8},
                "status": {"currentReplicas": 3, "desiredReplicas": 3}
            })],
        )
    }
    fn pod(
        &self,
        c: &Connection,
        ns: &str,
        name: &str,
        _: &Cancel,
    ) -> Result<Observation<Pod>, Failure> {
        let found = decode::<Pod>(pods())
            .into_iter()
            .find(|p| p.meta.name == name)
            .ok_or_else(|| Failure::NotFound {
                detail: format!("pods {name:?} not found"),
            })?;
        Ok(Observation {
            source: Self::source(c, ns, okilum_core::kubernetes::Kind::Pod),
            observed_at: SystemTime::now(),
            value: found,
        })
    }
}

const LOG: &str = "2026-11-16T14:21:40Z INFO  checkout-api starting · image checkout:2.8.1\n\
2026-11-16T14:21:51Z WARN  cart cache warm-up: 412 MiB resident, limit 512 MiB\n\
2026-11-16T14:21:57Z ERROR allocation failed while building price index (heap 509 MiB)\n\
2026-11-16T14:21:58Z FATAL process received SIGKILL (OOMKilled)\n";

/// Log sessions without a process: one fixed snapshot, then Reading.
fn starter() -> dock::Starter {
    Rc::new(|_request, scope| {
        let (worker, _) = worker::Worker::detached();
        let (sender, receiver) = async_channel::unbounded();
        let snapshot = okilum_core::log::stream::Assembler::new("pod.log".into(), scope)
            .ok()
            .and_then(|mut a| a.append(LOG.as_bytes()).ok().flatten());
        let _ = sender.send_blocking(worker::Update {
            snapshot,
            status: worker::Status::Reading,
            diagnostic: String::new(),
            pending: 0,
        });
        (worker, receiver)
    })
}

struct Plan {
    lens: Lens,
    dark: bool,
    ready: Option<PathBuf>,
    export: Option<PathBuf>,
}

fn plan() -> Option<Plan> {
    let spec = std::env::var("OKILUM_KUBERNETES_HARNESS").ok()?;
    let (lens, mode) = spec.split_once(':').unwrap_or((spec.as_str(), "light"));
    let lens = match lens {
        "triage" => Lens::Triage,
        "service-path" => Lens::ServicePath,
        _ => Lens::Workspace,
    };
    Some(Plan {
        lens,
        dark: mode == "dark",
        ready: std::env::var_os("OKILUM_KUBERNETES_HARNESS_READY").map(PathBuf::from),
        export: std::env::var_os("OKILUM_KUBERNETES_HARNESS_EXPORT").map(PathBuf::from),
    })
}

/// Opens the harness window when asked; `false` leaves startup unchanged.
pub(crate) fn open(cx: &mut App) -> bool {
    let Some(plan) = plan() else {
        return false;
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1320.), px(900.)),
        })),
        focus: true,
        show: true,
        ..TitleBar::window_options()
    };
    let dark = plan.dark;
    let opened = cx.open_window(options, move |window, cx| {
        window.set_window_title("Kubernetes workspace");
        let mode = if dark {
            gpui_component::ThemeMode::Dark
        } else {
            gpui_component::ThemeMode::Light
        };
        gpui_component::Theme::change(mode, Some(window), cx);
        brand::apply_theme(cx);
        let reader: Arc<dyn Reader> = Arc::new(FixtureReader);
        let workspace = cx.new(|cx| {
            let mut ws = Workspace::new("kubernetes-ui-harness".into(), reader, window, cx);
            ws.dock.starter = starter();
            ws
        });
        let driven = workspace.clone();
        cx.spawn(async move |cx| drive(driven, plan, cx).await)
            .detach();
        cx.new(|cx| Root::new(workspace, window, cx))
    });
    if opened.is_err() {
        eprintln!("kubernetes-ui-harness: window could not be opened");
        std::process::exit(2);
    }
    cx.activate(true);
    true
}

async fn wait_until(
    workspace: &Entity<Workspace>,
    cx: &mut AsyncApp,
    done: impl Fn(&Workspace) -> bool,
) -> bool {
    for _ in 0..400 {
        if workspace.read_with(cx, |ws, _| done(ws)) {
            return true;
        }
        cx.background_executor()
            .timer(std::time::Duration::from_millis(25))
            .await;
    }
    false
}

async fn drive(workspace: Entity<Workspace>, plan: Plan, cx: &mut AsyncApp) {
    let ok = wait_until(&workspace, cx, |ws| ws.contexts.is_some()).await;
    if ok {
        workspace.update(cx, |ws, cx| ws.load(cx));
    }
    let ok = ok
        && wait_until(&workspace, cx, |ws| {
            ws.data.is_some() && ws.loading.is_none()
        })
        .await;
    if ok {
        workspace.update(cx, |ws, cx| choose(ws, plan.lens, cx));
    }
    let ok = ok && wait_until(&workspace, cx, |ws| ws.dock.view.is_some()).await;
    cx.background_executor()
        .timer(std::time::Duration::from_millis(1200))
        .await;
    if let (true, Some(dir)) = (ok, plan.export.as_ref()) {
        let bundle = workspace.read_with(cx, |ws, cx| ws.evidence_bundle(cx));
        if let Err(error) = evidence::write(dir, &bundle) {
            eprintln!("kubernetes-ui-harness: export failed: {error}");
        }
    }
    let line = if ok { "READY" } else { "FAILED" };
    println!("kubernetes-ui-harness: {line}");
    if let Some(ready) = plan.ready {
        let _ = std::fs::write(ready, line);
    }
}

/// The representative state of each approved composition.
fn choose(ws: &mut Workspace, lens: Lens, cx: &mut Context<Workspace>) {
    let Some(data) = ws.data.as_ref() else {
        return;
    };
    let recovered = data
        .pods()
        .iter()
        .find(|p| p.meta.uid == "uid-r4v8n")
        .cloned();
    let crashing = data
        .pods()
        .iter()
        .find(|p| p.meta.uid == "uid-q7k2b")
        .cloned();
    let service = model::latest(&data.services).and_then(|o| o.value.items.first().cloned());
    let scope = data.scope.clone();
    ws.set_lens(lens, cx);
    match lens {
        Lens::Workspace => {
            ws.set_section(model::Section::Containers, cx);
            if let Some(pod) = &crashing {
                ws.select(
                    Key::Container {
                        pod: pod.meta.name.clone(),
                        pod_uid: pod.meta.uid.clone(),
                        container: "api".into(),
                    },
                    cx,
                );
                if let Ok(target) = LogTarget::new(&scope, pod, "api", true) {
                    ws.request_logs(target, cx);
                }
            }
        }
        Lens::Triage => {
            if let Some(pod) = &recovered {
                ws.select(
                    Key::Container {
                        pod: pod.meta.name.clone(),
                        pod_uid: pod.meta.uid.clone(),
                        container: "api".into(),
                    },
                    cx,
                );
                if let Ok(target) = LogTarget::new(&scope, pod, "api", true) {
                    ws.request_logs(target, cx);
                }
            }
        }
        Lens::ServicePath => {
            if let Some(service) = &service {
                ws.select(
                    Key::object(okilum_core::kubernetes::Kind::Service, &service.meta),
                    cx,
                );
            }
            if let Some(pod) = &crashing {
                ws.select(
                    Key::object(okilum_core::kubernetes::Kind::Pod, &pod.meta),
                    cx,
                );
                if let Ok(target) = LogTarget::new(&scope, pod, "api", false) {
                    ws.request_logs(target, cx);
                }
            }
        }
    }
}
