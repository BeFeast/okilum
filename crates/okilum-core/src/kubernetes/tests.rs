//! Synthetic transport and parser cases only: no cluster, kubeconfig or
//! credentials are touched. Process cases run a local `sh` fixture that
//! receives the production argv in place of kubectl.
use super::*;
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
#[cfg(unix)]
use std::time::Instant;
use std::time::{Duration, SystemTime};

// ---------- Synthetic transport ----------

struct Fake {
    routes: Vec<(&'static str, Raw)>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Fake {
    fn new(routes: Vec<(&'static str, Raw)>) -> Self {
        Self {
            routes,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Transport for Fake {
    fn run(&self, args: &[String], _: Limits, _: &Cancel) -> io::Result<Raw> {
        self.calls.borrow_mut().push(args.to_vec());
        let line = args.join(" ");
        self.routes
            .iter()
            .find(|(key, _)| line.contains(key))
            .map(|(_, raw)| raw.clone())
            .ok_or_else(|| io::Error::other(format!("no synthetic route for {line}")))
    }
}

fn ok(value: serde_json::Value) -> Raw {
    ok_bytes(value.to_string().into_bytes())
}

fn ok_bytes(stdout: Vec<u8>) -> Raw {
    Raw {
        stdout,
        stderr: Vec::new(),
        stderr_truncated: false,
        end: End::Exited { code: Some(0) },
        cleanup: Cleanup::NotNeeded,
    }
}

fn fail(code: i32, stderr: &str) -> Raw {
    Raw {
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
        stderr_truncated: false,
        end: End::Exited { code: Some(code) },
        cleanup: Cleanup::NotNeeded,
    }
}

fn connection(context: &str) -> Connection {
    Connection {
        context: context.into(),
        cluster: Some(format!("{context}-cluster")),
        server: Some("https://cluster.invalid:6443".into()),
        default_namespace: None,
    }
}

fn meta(name: &str, uid: &str, rv: &str) -> serde_json::Value {
    json!({"name": name, "namespace": "shop", "uid": uid, "resourceVersion": rv,
           "labels": {"app": "checkout"}})
}

fn list(kind: &str, items: Vec<serde_json::Value>) -> serde_json::Value {
    json!({"kind": format!("{kind}List"), "apiVersion": "v1",
           "metadata": {"resourceVersion": "9001"}, "items": items})
}

fn pod_json(uid: &str) -> serde_json::Value {
    json!({
        "kind": "Pod",
        "metadata": meta("checkout-7d9f-abcde", uid, "4711"),
        "spec": {
            "nodeName": "node-a",
            "initContainers": [
                {"name": "migrate", "image": "registry.invalid/migrate:1"},
                {"name": "proxy", "image": "registry.invalid/proxy:2", "restartPolicy": "Always",
                 "readinessProbe": {"tcpSocket": {"port": 15021}, "periodSeconds": 5}}
            ],
            "containers": [
                {"name": "api", "image": "registry.invalid/api:3",
                 "resources": {"requests": {"cpu": "250m", "memory": "256Mi"},
                               "limits": {"memory": "512Mi"}},
                 "livenessProbe": {"httpGet": {"path": "/healthz", "port": "http", "scheme": "HTTP"},
                                   "initialDelaySeconds": 10, "failureThreshold": 3},
                 "readinessProbe": {"exec": {"command": ["cat", "/secret/token"]}}}
            ],
            "ephemeralContainers": [{"name": "debugger", "image": "registry.invalid/busybox:1"}]
        },
        "status": {
            "phase": "Running",
            "podIP": "10.0.0.7",
            "qosClass": "Burstable",
            "conditions": [{"type": "Ready", "status": "False", "reason": "ContainersNotReady"}],
            "initContainerStatuses": [
                {"name": "migrate", "ready": true, "restartCount": 0,
                 "image": "registry.invalid/migrate:1", "imageID": "registry.invalid/migrate@sha256:aa",
                 "state": {"terminated": {"exitCode": 0, "reason": "Completed"}}, "lastState": {}},
                {"name": "proxy", "ready": true, "started": true, "restartCount": 0,
                 "image": "registry.invalid/proxy:2", "imageID": "registry.invalid/proxy@sha256:bb",
                 "state": {"running": {"startedAt": "2026-11-16T10:00:00Z"}}, "lastState": {}}
            ],
            "containerStatuses": [
                {"name": "api", "ready": false, "started": false, "restartCount": 5,
                 "image": "registry.invalid/api:3", "imageID": "registry.invalid/api@sha256:cc",
                 "state": {"waiting": {"reason": "CrashLoopBackOff", "message": "back-off 5m0s"}},
                 "lastState": {"terminated": {"exitCode": 137, "reason": "OOMKilled",
                     "startedAt": "2026-11-16T10:01:00Z", "finishedAt": "2026-11-16T10:02:00Z"}}}
            ],
            "ephemeralContainerStatuses": [
                {"name": "debugger", "ready": false, "restartCount": 0,
                 "state": {"running": {"startedAt": "2026-11-16T10:03:00Z"}}, "lastState": {}}
            ]
        }
    })
}

fn pods_route(raw: Raw) -> (&'static str, Raw) {
    ("--raw=/api/v1/namespaces/shop/pods?limit=", raw)
}

// ---------- Argv, discovery and namespaces ----------

#[test]
fn reads_are_fixed_raw_gets_with_the_explicit_context() {
    let fake = Fake::new(vec![pods_route(ok(list("Pod", vec![])))]);
    let client = Client::new(fake, Limits::default());
    let query = ListQuery {
        limit: 50,
        labels: BTreeMap::from([("app".into(), "checkout".into())]),
    };
    let listed = client
        .list::<Pod>(&connection("team/a"), "shop", &query, &Cancel::new())
        .unwrap();
    assert!(listed.value.is_empty() && listed.value.is_complete());
    assert_eq!(listed.source.context, "team/a");
    assert_eq!(listed.source.namespace.as_deref(), Some("shop"));
    let calls = client.transport_calls();
    assert_eq!(
        calls,
        [[
            "--context=team/a",
            "--request-timeout=20s",
            "get",
            "--raw=/api/v1/namespaces/shop/pods?limit=50&labelSelector=app%3Dcheckout",
        ]]
    );
    for forbidden in [
        "use-context",
        "set-context",
        "delete",
        "apply",
        "exec",
        "describe",
        "-o=wide",
    ] {
        assert!(
            !calls[0].iter().any(|a| a.contains(forbidden)),
            "{forbidden}"
        );
    }
}

impl Client<Fake> {
    fn transport_calls(&self) -> Vec<Vec<String>> {
        self.transport().calls.borrow().clone()
    }
}

#[test]
fn rejects_unsafe_targets_before_running_anything() {
    let client = Client::new(Fake::new(vec![]), Limits::default());
    let cancel = Cancel::new();
    let conn = connection("a");
    for ns in ["", "Shop", "a/b", "../x", "-x", "a?b"] {
        let result = client.list::<Pod>(&conn, ns, &ListQuery::default(), &cancel);
        assert!(
            matches!(result, Err(Failure::InvalidRequest { .. })),
            "{ns:?}"
        );
    }
    for name in ["", "a/b", "A", "x?limit=1", "x y"] {
        let result = client.get::<Pod>(&conn, "shop", name, &cancel);
        assert!(
            matches!(result, Err(Failure::InvalidRequest { .. })),
            "{name:?}"
        );
    }
    let bad_limit = ListQuery {
        limit: MAX_LIST_LIMIT + 1,
        ..Default::default()
    };
    assert!(client
        .list::<Pod>(&conn, "shop", &bad_limit, &cancel)
        .is_err());
    let bad_labels = ListQuery {
        labels: BTreeMap::from([("app".into(), "x,y=z".into())]),
        ..Default::default()
    };
    assert!(client
        .list::<Pod>(&conn, "shop", &bad_labels, &cancel)
        .is_err());
    let bad_context = connection("line\nbreak");
    assert!(client
        .list::<Pod>(&bad_context, "shop", &ListQuery::default(), &cancel)
        .is_err());
    assert!(client.transport_calls().is_empty());
}

#[test]
fn contexts_resolve_explicitly_and_keep_no_credentials() {
    let view = json!({
        "kind": "Config",
        "current-context": "dev",
        "clusters": [{"name": "c-dev", "cluster": {"server": "https://dev.invalid",
                      "certificate-authority-data": "DATA+OMITTED"}}],
        "contexts": [
            {"name": "dev", "context": {"cluster": "c-dev", "user": "u", "namespace": "shop"}},
            {"name": "prod", "context": {"cluster": "c-prod", "user": "u"}}
        ],
        "users": [{"name": "u", "user": {"token": "REDACTED",
                  "exec": {"command": "helper", "args": ["get-token"]}}}]
    });
    let client = Client::new(
        Fake::new(vec![("config view", ok(view))]),
        Limits::default(),
    );
    let contexts = client.contexts(&Cancel::new()).unwrap();
    assert_eq!(
        client.transport_calls(),
        [["config", "view", "--output=json", "--raw=false"]]
    );
    let current = contexts.resolve(None).unwrap();
    assert_eq!(current.context, "dev");
    assert_eq!(current.server.as_deref(), Some("https://dev.invalid"));
    assert_eq!(current.default_namespace.as_deref(), Some("shop"));
    let prod = contexts.resolve(Some("prod")).unwrap();
    assert_eq!((prod.context.as_str(), prod.server), ("prod", None));
    assert!(matches!(
        contexts.resolve(Some("missing")),
        Err(Failure::Configuration { .. })
    ));
    let debug = format!("{contexts:?}");
    assert!(!debug.contains("REDACTED") && !debug.contains("helper"));
    let none = Contexts {
        current: None,
        contexts: contexts.contexts.clone(),
    };
    assert!(matches!(
        none.resolve(None),
        Err(Failure::Configuration { .. })
    ));
}

#[test]
fn denied_namespace_listing_still_allows_a_typed_namespace() {
    let fake = Fake::new(vec![
        (
            "--raw=/api/v1/namespaces?limit=",
            fail(
                1,
                "Error from server (Forbidden): namespaces is forbidden: User \"synthetic\" cannot list resource \"namespaces\" at the cluster scope\n",
            ),
        ),
        pods_route(ok(list("Pod", vec![pod_json("uid-1")]))),
    ]);
    let client = Client::new(fake, Limits::default());
    let conn = connection("a");
    let cancel = Cancel::new();
    let listing = client.namespaces(&conn, &ListQuery::default(), &cancel);
    assert!(matches!(listing, Err(Failure::Forbidden { .. })));
    let choice = choose_namespace(&conn, Some("shop"), Some(&listing)).unwrap();
    assert_eq!(choice.namespace, "shop");
    assert!(matches!(
        choice.origin,
        NamespaceOrigin::TypedListingDenied(Failure::Forbidden { .. })
    ));
    let pods = client
        .list::<Pod>(&conn, &choice.namespace, &ListQuery::default(), &cancel)
        .unwrap();
    assert_eq!(pods.value.items.len(), 1);
    assert!(choose_namespace(&conn, Some("Not A Label"), Some(&listing)).is_err());
    assert_eq!(
        choose_namespace(&conn, None, None).unwrap().origin,
        NamespaceOrigin::KubectlDefault
    );
}

#[test]
fn listed_namespace_choice_distinguishes_listed_and_unlisted() {
    let names = list(
        "Namespace",
        vec![
            json!({"metadata": {"name": "shop", "uid": "ns-1", "resourceVersion": "1"},
                    "status": {"phase": "Active"}}),
        ],
    );
    let client = Client::new(
        Fake::new(vec![("--raw=/api/v1/namespaces?limit=", ok(names))]),
        Limits::default(),
    );
    let conn = connection("a");
    let listing = client.namespaces(&conn, &ListQuery::default(), &Cancel::new());
    assert_eq!(
        listing.as_ref().unwrap().value.items[0].phase.as_deref(),
        Some("Active")
    );
    let listed = choose_namespace(&conn, Some("shop"), Some(&listing)).unwrap();
    assert_eq!(listed.origin, NamespaceOrigin::TypedListed);
    let other = choose_namespace(&conn, Some("other"), Some(&listing)).unwrap();
    assert_eq!(other.origin, NamespaceOrigin::TypedNotListed);
}

// ---------- Identity ----------

#[test]
fn same_names_in_two_contexts_are_different_objects() {
    let pod = ok(pod_json("same-uid"));
    let fake = Fake::new(vec![("pods/checkout-7d9f-abcde", pod)]);
    let client = Client::new(fake, Limits::default());
    let cancel = Cancel::new();
    let a = client
        .get::<Pod>(
            &connection("cluster-a"),
            "shop",
            "checkout-7d9f-abcde",
            &cancel,
        )
        .unwrap();
    let b = client
        .get::<Pod>(
            &connection("cluster-b"),
            "shop",
            "checkout-7d9f-abcde",
            &cancel,
        )
        .unwrap();
    assert_eq!(a.id().compare(&b.id()), Sameness::Different);
    assert_ne!(a.source, b.source);
    // A related-events read must stay within the object's own context.
    let crossed = client.related_events(
        &connection("cluster-b"),
        &a.id(),
        &ListQuery::default(),
        &cancel,
    );
    assert!(matches!(crossed, Err(Failure::InvalidRequest { .. })));
    assert_eq!(client.transport_calls().len(), 2);
}

#[test]
fn recreated_object_keeps_name_but_not_identity_or_events() {
    let first = Client::new(
        Fake::new(vec![("pods/checkout-7d9f-abcde", ok(pod_json("uid-old")))]),
        Limits::default(),
    );
    let second = Client::new(
        Fake::new(vec![
            ("pods/checkout-7d9f-abcde", ok(pod_json("uid-new"))),
            (
                "events?",
                ok(list(
                    "Event",
                    vec![json!({
                        "metadata": {"name": "checkout.1", "namespace": "shop", "uid": "ev-1", "resourceVersion": "5"},
                        "involvedObject": {"kind": "Pod", "name": "checkout-7d9f-abcde", "uid": "uid-new"},
                        "type": "Warning", "reason": "BackOff", "message": "Back-off restarting",
                        "count": 4, "lastTimestamp": "2026-11-16T10:04:00Z",
                        "source": {"component": "kubelet"}
                    })],
                )),
            ),
        ]),
        Limits::default(),
    );
    let conn = connection("a");
    let cancel = Cancel::new();
    let old = first
        .get::<Pod>(&conn, "shop", "checkout-7d9f-abcde", &cancel)
        .unwrap();
    let new = second
        .get::<Pod>(&conn, "shop", "checkout-7d9f-abcde", &cancel)
        .unwrap();
    assert_eq!(old.id().compare(&new.id()), Sameness::Recreated);
    assert_eq!(new.id().compare(&new.id()), Sameness::Same);
    assert_eq!(
        new.value.meta.resource_version,
        ResourceVersion("4711".into())
    );
    let events = second
        .related_events(&conn, &new.id(), &ListQuery::default(), &cancel)
        .unwrap();
    let call = second.transport_calls().pop().unwrap();
    assert_eq!(
        call.last().unwrap(),
        "--raw=/api/v1/namespaces/shop/events?limit=500&fieldSelector=involvedObject.uid%3Duid-new"
    );
    let event = &events.value.items[0];
    assert_eq!(event.regarding.uid.as_deref(), Some("uid-new"));
    assert_eq!(event.reporter.as_deref(), Some("kubelet"));
    assert_eq!(event.count, Some(4));
}

// ---------- Per-container facts ----------

#[test]
fn containers_keep_their_own_state_kind_and_settings() {
    let pod = client_with_pod().value;
    let kinds: Vec<_> = pod
        .containers
        .iter()
        .map(|c| (c.name.as_str(), c.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("migrate", ContainerKind::Init),
            ("proxy", ContainerKind::Sidecar),
            ("api", ContainerKind::Regular),
            ("debugger", ContainerKind::Ephemeral),
        ]
    );
    let api = pod.container("api").unwrap();
    assert_eq!(
        api.health(),
        Health::Waiting {
            reason: Some("CrashLoopBackOff".into())
        }
    );
    assert_eq!(api.restart_count, Some(5));
    assert!(matches!(
        api.last_state,
        Some(State::Terminated { exit_code: 137, ref reason, .. }) if reason.as_deref() == Some("OOMKilled")
    ));
    assert_eq!(
        api.image_id.as_deref(),
        Some("registry.invalid/api@sha256:cc")
    );
    assert_eq!(api.resources.requests["cpu"], "250m");
    assert_eq!(api.resources.limits["memory"], "512Mi");
    let liveness = api.probes.liveness.as_ref().unwrap();
    assert_eq!(
        liveness.handler,
        ProbeHandler::Http {
            path: Some("/healthz".into()),
            port: "http".into(),
            scheme: Some("HTTP".into())
        }
    );
    assert_eq!(liveness.failure_threshold, Some(3));
    // The exec command is counted, never retained.
    assert_eq!(
        api.probes.readiness.as_ref().unwrap().handler,
        ProbeHandler::Exec { arguments: 2 }
    );
    assert!(!format!("{pod:?}").contains("/secret/token"));

    // The healthy sidecar does not inherit the failing app container's state.
    let proxy = pod.container("proxy").unwrap();
    assert_eq!(proxy.health(), Health::Running { ready: Some(true) });
    assert!(!proxy.previous_instance_observed());
    assert_eq!(
        pod.container("migrate").unwrap().health(),
        Health::Completed
    );
    assert_eq!(pod.container("migrate").unwrap().last_state, None);
    assert_eq!(
        pod.container("debugger").unwrap().health(),
        Health::Running { ready: Some(false) }
    );
    assert_eq!(pod.phase.as_deref(), Some("Running"));
    assert_eq!(pod.pod_ip.as_deref(), Some("10.0.0.7"));
}

fn client_with_pod() -> Observation<Pod> {
    let client = Client::new(
        Fake::new(vec![("pods/checkout-7d9f-abcde", ok(pod_json("uid-1")))]),
        Limits::default(),
    );
    client
        .get::<Pod>(
            &connection("a"),
            "shop",
            "checkout-7d9f-abcde",
            &Cancel::new(),
        )
        .unwrap()
}

#[test]
fn pending_containers_without_status_are_unreported() {
    let mut value = pod_json("uid-2");
    value["status"] = json!({"phase": "Pending"});
    let client = Client::new(
        Fake::new(vec![("pods/checkout-7d9f-abcde", ok(value))]),
        Limits::default(),
    );
    let pod = client
        .get::<Pod>(
            &connection("a"),
            "shop",
            "checkout-7d9f-abcde",
            &Cancel::new(),
        )
        .unwrap()
        .value;
    for container in &pod.containers {
        assert!(!container.status_reported);
        assert_eq!(container.health(), Health::Unreported);
    }
}

#[test]
fn previous_instance_absence_and_log_requests_only_build() {
    let observation = client_with_pod();
    let previous = log_request(&observation, "api", true, false, 1000).unwrap();
    assert_eq!(previous.context.as_deref(), Some("a"));
    assert_eq!(previous.namespace, "shop");
    assert!(previous
        .args()
        .unwrap()
        .contains(&"--previous=true".to_string()));
    // No previous instance observed for the sidecar: refused before kubectl.
    assert!(matches!(
        log_request(&observation, "proxy", true, false, 1000),
        Err(Failure::NotFound { .. })
    ));
    let current = log_request(&observation, "proxy", false, true, 10).unwrap();
    assert!(current
        .args()
        .unwrap()
        .contains(&"--follow=true".to_string()));
    assert!(matches!(
        log_request(&observation, "missing", false, false, 10),
        Err(Failure::NotFound { .. })
    ));
}

// ---------- Workload, network and autoscaling facts ----------

#[test]
fn decodes_workload_service_slice_and_hpa_facts() {
    let fake = Fake::new(vec![
        (
            "/deployments?",
            ok(list(
                "Deployment",
                vec![json!({
                    "metadata": meta("checkout", "dep-1", "10"),
                    "spec": {"replicas": 3, "selector": {"matchLabels": {"app": "checkout"}}},
                    "status": {"observedGeneration": 7, "replicas": 3, "readyReplicas": 2,
                               "availableReplicas": 2, "updatedReplicas": 3, "unavailableReplicas": 1,
                               "conditions": [{"type": "Available", "status": "False", "reason": "MinimumReplicasUnavailable"}]}
                })],
            )),
        ),
        (
            "/replicasets?",
            ok(list(
                "ReplicaSet",
                vec![json!({
                    "metadata": {"name": "checkout-7d9f", "uid": "rs-1", "resourceVersion": "11",
                                 "labels": {"pod-template-hash": "7d9f"},
                                 "ownerReferences": [{"kind": "Deployment", "name": "checkout", "uid": "dep-1", "controller": true}]},
                    "spec": {"replicas": 3}, "status": {"replicas": 3, "readyReplicas": 2}
                })],
            )),
        ),
        (
            "/services?",
            ok(list(
                "Service",
                vec![json!({
                    "metadata": meta("checkout", "svc-1", "12"),
                    "spec": {"type": "ClusterIP", "clusterIP": "10.96.0.10", "selector": {"app": "checkout"},
                             "ports": [{"name": "http", "protocol": "TCP", "port": 80, "targetPort": 8080}]}
                })],
            )),
        ),
        (
            "/endpointslices?",
            ok(list(
                "EndpointSlice",
                vec![json!({
                    "metadata": {"name": "checkout-x1", "uid": "es-1", "resourceVersion": "13",
                                 "labels": {"kubernetes.io/service-name": "checkout"}},
                    "addressType": "IPv4",
                    "endpoints": [{"addresses": ["10.0.0.7"], "conditions": {"ready": false, "serving": false, "terminating": false},
                                   "nodeName": "node-a", "targetRef": {"kind": "Pod", "name": "checkout-7d9f-abcde", "uid": "uid-1"}}],
                    "ports": null
                })],
            )),
        ),
        (
            "/horizontalpodautoscalers?",
            ok(list(
                "HorizontalPodAutoscaler",
                vec![json!({
                    "metadata": meta("checkout", "hpa-1", "14"),
                    "spec": {"scaleTargetRef": {"kind": "Deployment", "name": "checkout", "apiVersion": "apps/v1"},
                             "minReplicas": 2, "maxReplicas": 10},
                    "status": {"currentReplicas": 3, "desiredReplicas": 5,
                               "currentMetrics": [
                                   {"type": "Resource", "resource": {"name": "cpu", "current": {"averageUtilization": 93, "averageValue": "930m"}}},
                                   {"type": "Pods", "pods": {"metric": {"name": "rps"}, "current": {"averageValue": "120"}}}
                               ]}
                })],
            )),
        ),
    ]);
    let client = Client::new(fake, Limits::default());
    let (conn, q, c) = (connection("a"), ListQuery::default(), Cancel::new());
    let dep = client.list::<Deployment>(&conn, "shop", &q, &c).unwrap();
    let dep = &dep.value.items[0];
    assert_eq!(
        (dep.replicas, dep.ready, dep.unavailable),
        (Some(3), Some(2), Some(1))
    );
    assert_eq!(dep.selector["app"], "checkout");
    let rs = client.list::<ReplicaSet>(&conn, "shop", &q, &c).unwrap();
    let rs = &rs.value.items[0];
    assert_eq!(rs.template_hash.as_deref(), Some("7d9f"));
    assert!(rs.meta.owners[0].controller && rs.meta.owners[0].uid == "dep-1");
    let svc = client.list::<Service>(&conn, "shop", &q, &c).unwrap();
    let svc = &svc.value.items[0];
    assert_eq!(svc.ports[0].target_port.as_deref(), Some("8080"));
    assert_eq!(svc.cluster_ip.as_deref(), Some("10.96.0.10"));
    let slices = client.list::<EndpointSlice>(&conn, "shop", &q, &c).unwrap();
    let slice = &slices.value.items[0];
    assert_eq!(slice.service.as_deref(), Some("checkout"));
    assert_eq!(slice.endpoints[0].ready, Some(false));
    assert_eq!(
        slice.endpoints[0].target.as_ref().unwrap().uid.as_deref(),
        Some("uid-1")
    );
    assert!(slice.ports.is_empty());
    let hpa = client
        .list::<HorizontalPodAutoscaler>(&conn, "shop", &q, &c)
        .unwrap();
    let hpa = &hpa.value.items[0];
    assert_eq!(
        (hpa.min_replicas, hpa.max_replicas, hpa.desired_replicas),
        (Some(2), Some(10), Some(5))
    );
    assert_eq!(hpa.target.name, "checkout");
    assert_eq!(
        hpa.metrics,
        [
            MetricFact {
                kind: "Resource".into(),
                name: Some("cpu".into()),
                current: Some("93%".into())
            },
            MetricFact {
                kind: "Pods".into(),
                name: Some("rps".into()),
                current: Some("120".into())
            },
        ]
    );
    let paths: Vec<String> = client
        .transport_calls()
        .iter()
        .map(|c| c.last().unwrap().clone())
        .collect();
    assert!(paths[0].starts_with("--raw=/apis/apps/v1/namespaces/shop/deployments?"));
    assert!(paths[3].starts_with("--raw=/apis/discovery.k8s.io/v1/namespaces/shop/endpointslices?"));
    assert!(paths[4]
        .starts_with("--raw=/apis/autoscaling/v2/namespaces/shop/horizontalpodautoscalers?"));
}

// ---------- Empty, partial, malformed, oversized, failures ----------

#[test]
fn empty_partial_and_rejected_lists_stay_distinct() {
    let partial = json!({"kind": "PodList", "metadata": {"resourceVersion": "77", "continue": "token",
                         "remainingItemCount": 41},
                         "items": [pod_json("uid-1"), {"metadata": {"name": "no-uid", "resourceVersion": "1"}}]});
    let client = Client::new(Fake::new(vec![pods_route(ok(partial))]), Limits::default());
    let got = client
        .list::<Pod>(
            &connection("a"),
            "shop",
            &ListQuery {
                limit: 2,
                ..Default::default()
            },
            &Cancel::new(),
        )
        .unwrap()
        .value;
    assert_eq!(
        got.completeness,
        Completeness::Truncated {
            remaining: Some(41)
        }
    );
    assert_eq!(got.items.len(), 1);
    assert_eq!(got.rejected_total, 1);
    assert!(got.rejected[0].reason.contains("uid"));
    assert_eq!(got.resource_version, Some(ResourceVersion("77".into())));
    assert!(!got.is_empty() && !got.is_complete());

    // A server ignoring `limit` is cut and the cut is visible.
    let many = list(
        "Pod",
        (0..5).map(|i| pod_json(&format!("uid-{i}"))).collect(),
    );
    let client = Client::new(Fake::new(vec![pods_route(ok(many))]), Limits::default());
    let got = client
        .list::<Pod>(
            &connection("a"),
            "shop",
            &ListQuery {
                limit: 3,
                ..Default::default()
            },
            &Cancel::new(),
        )
        .unwrap()
        .value;
    assert_eq!(got.items.len(), 3);
    assert_eq!(
        got.completeness,
        Completeness::Truncated { remaining: None }
    );

    let empty = json!({"kind": "PodList", "metadata": {"resourceVersion": "78"}, "items": null});
    let client = Client::new(Fake::new(vec![pods_route(ok(empty))]), Limits::default());
    let got = client
        .list::<Pod>(
            &connection("a"),
            "shop",
            &ListQuery::default(),
            &Cancel::new(),
        )
        .unwrap()
        .value;
    assert!(got.is_empty() && got.is_complete());
}

fn failure_kind(failure: &Failure) -> &'static str {
    match failure {
        Failure::AuthExpired { .. } => "auth",
        Failure::InteractiveLogin { .. } => "interactive",
        Failure::Forbidden { .. } => "forbidden",
        Failure::Unavailable { .. } => "unavailable",
        Failure::Configuration { .. } => "configuration",
        Failure::NotFound { .. } => "notfound",
        Failure::Malformed { .. } => "malformed",
        Failure::Oversized { .. } => "oversized",
        Failure::TimedOut { .. } => "timeout",
        Failure::Cancelled => "cancelled",
        Failure::InvalidRequest { .. } => "invalid",
        Failure::Failed { .. } => "failed",
    }
}

#[test]
fn malformed_and_oversized_output_is_never_data() {
    let limited = Raw {
        end: End::StdoutLimit,
        ..ok_bytes(vec![b'{'; 64])
    };
    let cases = [
        (
            ok_bytes(b"{\"kind\":\"PodList\",\"items\":[".to_vec()),
            "malformed",
        ),
        (ok_bytes(b"<html>proxy login</html>".to_vec()), "malformed"),
        (ok(list("Service", vec![])), "malformed"),
        (
            ok(json!({"kind": "PodList", "items": {"not": "an array"}})),
            "malformed",
        ),
        (limited, "oversized"),
        (
            Raw {
                end: End::Deadline,
                ..ok_bytes(Vec::new())
            },
            "timeout",
        ),
        (
            Raw {
                end: End::Cancelled,
                ..ok_bytes(Vec::new())
            },
            "cancelled",
        ),
    ];
    for (raw, expected) in cases {
        let client = Client::new(Fake::new(vec![pods_route(raw)]), Limits::default());
        let result = client.list::<Pod>(
            &connection("a"),
            "shop",
            &ListQuery::default(),
            &Cancel::new(),
        );
        let failure = result.unwrap_err();
        assert_eq!(failure_kind(&failure), expected, "{failure:?}");
        if let Failure::Oversized { limit_bytes } = failure {
            assert_eq!(limit_bytes, DEFAULT_MAX_STDOUT);
        }
    }
    let client = Client::new(
        Fake::new(vec![(
            "pods/x",
            ok(json!({"kind": "Status", "status": "Failure"})),
        )]),
        Limits::default(),
    );
    assert!(matches!(
        client.get::<Pod>(&connection("a"), "shop", "x", &Cancel::new()),
        Err(Failure::Malformed { .. })
    ));
}

#[test]
fn failures_are_classified_without_guessing_success() {
    let cases = [
        ("error: You must be logged in to the server (Unauthorized)", "auth"),
        ("Unable to connect to the server: getting credentials: exec: executable aws failed with exit code 255 (token has expired)", "auth"),
        ("Unable to connect to the server: getting credentials: exec plugin is configured to use interactive mode, but stdin is not a terminal", "interactive"),
        ("Error from server (Forbidden): pods is forbidden: User \"synthetic\" cannot list resource \"pods\"", "forbidden"),
        ("Unable to connect to the server: dial tcp 192.0.2.1:6443: i/o timeout", "unavailable"),
        ("Error from server (ServiceUnavailable): the server is currently unable to handle the request", "unavailable"),
        ("error: context was not found for specified context: gone", "configuration"),
        ("Error from server (NotFound): pods \"x\" not found", "notfound"),
        ("something new and unexpected", "failed"),
    ];
    for (stderr, expected) in cases {
        let got = classify::from_stderr(Some(1), stderr.as_bytes());
        let kind = failure_kind(&got);
        assert_eq!(kind, expected, "{stderr}");
    }
    let spawn = classify::spawn_failure(&io::Error::from(io::ErrorKind::NotFound));
    assert!(spawn.summary().contains("PATH"));
}

#[test]
fn cancelled_before_start_runs_nothing() {
    let client = Client::new(Fake::new(vec![]), Limits::default());
    let cancel = Cancel::new();
    cancel.cancel();
    assert_eq!(
        client.list::<Pod>(&connection("a"), "shop", &ListQuery::default(), &cancel),
        Err(Failure::Cancelled)
    );
    assert!(client.transport_calls().is_empty());
}

// ---------- Retained observations ----------

fn observation(at: SystemTime, kind: Kind) -> Observation<u32> {
    Observation {
        source: Source {
            context: "a".into(),
            cluster: None,
            server: None,
            namespace: Some("shop".into()),
            kind,
        },
        observed_at: at,
        value: 1,
    }
}

#[test]
fn failures_keep_the_last_value_as_stale_per_source() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let max_age = Duration::from_secs(60);
    let first = observation(t0, Kind::Pod);
    let mut pods = Slot::new(first.source.clone());
    let mut events = Slot::<u32>::new(observation(t0, Kind::Event).source);
    assert_eq!(pods.view(t0, max_age), View::Unread);
    assert!(pods.record(Ok(first.clone()), t0));
    assert_eq!(pods.view(t0, max_age), View::Fresh(&first));

    let denied = Failure::Forbidden {
        detail: "events".into(),
    };
    assert!(events.record(Err(denied.clone()), t0));
    assert!(matches!(events.view(t0, max_age), View::Failed { .. }));
    assert_eq!(
        pods.view(t0, max_age),
        View::Fresh(&first),
        "other sources unaffected"
    );

    let t1 = t0 + Duration::from_secs(5);
    let expired = Failure::AuthExpired {
        detail: "token".into(),
    };
    assert!(pods.record(Err(expired.clone()), t1));
    assert_eq!(
        pods.view(t1, max_age),
        View::Stale {
            last: &first,
            why: Staleness::Failed {
                failure: &expired,
                at: t1
            }
        }
    );
    assert!(
        !pods.record(Err(Failure::Cancelled), t1),
        "cancel changes nothing"
    );
    assert!(
        !pods.record(Ok(observation(t0 - Duration::from_secs(1), Kind::Pod)), t1),
        "older refused"
    );
    assert!(
        !pods.record(Ok(observation(t1, Kind::Service)), t1),
        "other source refused"
    );

    let second = observation(t1, Kind::Pod);
    assert!(pods.record(Ok(second.clone()), t1));
    assert_eq!(pods.view(t1, max_age), View::Fresh(&second));
    let later = t1 + Duration::from_secs(61);
    assert!(matches!(
        pods.view(later, max_age),
        View::Stale {
            why: Staleness::Aged { .. },
            ..
        }
    ));
}

// ---------- Real process transport (synthetic sh fixture) ----------

#[cfg(unix)]
fn fixture(script: &'static str) -> Kubectl {
    Kubectl::with_command(move || {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", script, "fixture-kubectl"]);
        command
    })
}

#[cfg(unix)]
#[test]
fn process_receives_exact_argv_without_shell_interpolation() {
    let script = r#"
        [ "$1" = '--context=team/$(echo injected) a' ] || exit 9
        [ "$2" = '--request-timeout=20s' ] || exit 9
        [ "$3" = 'get' ] || exit 9
        [ "$4" = '--raw=/api/v1/namespaces/shop/pods?limit=500' ] || exit 9
        [ $# -eq 4 ] || exit 9
        printf '{"kind":"PodList","metadata":{"resourceVersion":"1"},"items":[]}'
    "#;
    let client = Client::new(fixture(script), Limits::default());
    let got = client
        .list::<Pod>(
            &connection("team/$(echo injected) a"),
            "shop",
            &ListQuery::default(),
            &Cancel::new(),
        )
        .unwrap();
    assert!(got.value.is_empty());
    // Positive control: the same fixture rejects a different argv.
    let other = client.list::<Pod>(
        &connection("team/b"),
        "shop",
        &ListQuery::default(),
        &Cancel::new(),
    );
    assert!(matches!(other, Err(Failure::Failed { code: Some(9), .. })));
}

#[cfg(unix)]
#[test]
fn process_failure_keeps_stderr_bounded_and_classified() {
    let script = r#"
        printf 'Error from server (Forbidden): pods is forbidden\n' >&2
        head -c 40000 /dev/zero | tr '\0' 'x' >&2
        exit 1
    "#;
    let raw = fixture(script)
        .run(&["get".into()], Limits::default(), &Cancel::new())
        .unwrap();
    assert_eq!(raw.end, End::Exited { code: Some(1) });
    assert_eq!(raw.stderr.len(), MAX_STDERR);
    assert!(raw.stderr_truncated);
    assert!(matches!(
        classify::from_stderr(Some(1), &raw.stderr),
        Failure::Forbidden { .. }
    ));
}

#[cfg(unix)]
#[test]
fn oversized_stdout_ends_the_process_promptly() {
    let limits = Limits {
        max_stdout: 64 * 1024,
        ..Limits::default()
    };
    let start = Instant::now();
    let raw = fixture("yes 0123456789abcdef")
        .run(&[], limits, &Cancel::new())
        .unwrap();
    assert_eq!(raw.end, End::StdoutLimit);
    assert_eq!(raw.cleanup, Cleanup::ProcessGroup);
    assert!(raw.stdout.len() <= limits.max_stdout);
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn deadline_ends_a_hung_credential_plugin() {
    let limits = Limits {
        deadline: Duration::from_millis(300),
        ..Limits::default()
    };
    let start = Instant::now();
    let raw = fixture("sleep 30")
        .run(&[], limits, &Cancel::new())
        .unwrap();
    assert_eq!(raw.end, End::Deadline);
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn a_descendant_holding_stderr_does_not_hold_the_result() {
    // Like a credential helper that leaves a background process behind; it
    // inherits stderr only (kubectl captures a plugin's stdout itself).
    let script = r#"
        (exec sleep 3 >/dev/null) &
        printf '{"kind":"PodList","metadata":{"resourceVersion":"1"},"items":[]}'
    "#;
    let start = Instant::now();
    let raw = fixture(script)
        .run(&[], Limits::default(), &Cancel::new())
        .unwrap();
    assert_eq!(raw.end, End::Exited { code: Some(0) });
    assert_eq!(raw.cleanup, Cleanup::NotNeeded);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(stat) => !stat
            .rsplit(')')
            .next()
            .unwrap_or_default()
            .trim_start()
            .starts_with('Z'),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn cancel_ends_kubectl_and_its_process_group_descendants() {
    let script = r#"
        sleep 30 &
        echo "$!" >&2
        wait
    "#;
    let cancel = Cancel::new();
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        trigger.cancel();
    });
    let start = Instant::now();
    let raw = fixture(script)
        .run(&[], Limits::default(), &cancel)
        .unwrap();
    canceller.join().unwrap();
    assert_eq!(raw.end, End::Cancelled);
    assert_eq!(raw.cleanup, Cleanup::ProcessGroup);
    assert!(start.elapsed() < Duration::from_secs(5));
    let pid: u32 = String::from_utf8_lossy(&raw.stderr).trim().parse().unwrap();
    // Positive control: the descendant existed and was reported before cancel.
    assert!(pid > 1);
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(pid) {
        assert!(
            Instant::now() < deadline,
            "descendant {pid} survived cancel"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn missing_kubectl_is_a_visible_unavailable_failure() {
    let client = Client::new(
        Kubectl::with_command(|| std::process::Command::new("/nonexistent/okilum-fixture-kubectl")),
        Limits::default(),
    );
    let result = client.contexts(&Cancel::new());
    assert!(matches!(result, Err(Failure::Unavailable { ref detail }) if detail.contains("PATH")));
    assert_eq!(Kubectl::default().program(), KUBECTL);
}
