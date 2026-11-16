use super::*;
use serde_json::{json, Value};

fn connection(context: &str) -> Connection {
    Connection {
        context: context.into(),
        cluster: "synthetic-cluster".into(),
        server: "https://example.invalid".into(),
        kubeconfig: None,
        user: Some("synthetic-user".into()),
        default_namespace: "shop".into(),
    }
}
fn query(kind: Kind) -> Query {
    Query::List {
        connection: connection("synthetic/team"),
        namespace: "shop".into(),
        kind,
    }
}
fn object(kind: &str, api: &str) -> Value {
    json!({"kind": kind, "apiVersion": api,
        "metadata": {"name":"checkout", "namespace":"shop", "uid":"uid-a", "resourceVersion":"opaque/not-a-number"}})
}
fn pod_json() -> Value {
    let mut pod = object("Pod", "v1");
    pod["spec"] = json!({"containers":[
        {"name":"api", "image":"example.invalid/api:v1", "resources":{"requests":{"cpu":"100m"},"limits":{"memory":"128Mi"}},
            "readinessProbe":{"httpGet":{"path":"/ready","port":"http"},"periodSeconds":5}},
        {"name":"sidecar", "image":"example.invalid/sidecar:v1"}],
        "initContainers":[{"name":"init", "image":"example.invalid/init:v1"}],
        "ephemeralContainers":[{"name":"debug", "targetContainerName":"api", "image":"example.invalid/debug:v1"}]});
    pod["status"] = json!({"phase":"Running", "reason":"HistoricalPodReason",
        "containerStatuses":[
            {"name":"sidecar","ready":true,"restartCount":0,"state":{"running":{"startedAt":"2026-01-01T00:00:00Z"}},"lastState":{}},
            {"name":"api","ready":true,"restartCount":3,"imageID":"sha256:synthetic", "state":{"running":{"startedAt":"2026-01-01T01:00:00Z"}},
                "lastState":{"terminated":{"reason":"OOMKilled","exitCode":137,"startedAt":"2026-01-01T00:00:00Z","finishedAt":"2026-01-01T00:59:59Z"}}}],
        "initContainerStatuses":[{"name":"init","ready":true,"restartCount":0,"state":{"terminated":{"exitCode":0}}}]});
    pod
}
fn list(items: Vec<Value>) -> Value {
    json!({"apiVersion":"v1","kind":"List","metadata":{"resourceVersion":"list/opaque"},"items":items})
}
fn parsed(source: &Query, value: Value) -> Observation {
    parse(
        source,
        SystemTime::now(),
        &serde_json::to_vec(&value).unwrap(),
    )
    .unwrap()
}
fn items(observation: &Observation) -> &[Resource] {
    match &observation.data {
        Data::Resources { items, .. } => items,
        _ => panic!("expected resources"),
    }
}

#[test]
fn identity_separates_context_uid_and_opaque_revision() {
    let observation = parsed(&query(Kind::Pod), list(vec![pod_json()]));
    let resource = &items(&observation)[0];
    assert_eq!(resource.metadata.resource_version, "opaque/not-a-number");
    assert_ne!(
        resource.identity(&connection("a")),
        resource.identity(&connection("b"))
    );
    let mut replacement = resource.clone();
    replacement.metadata.uid = "uid-b".into();
    assert_ne!(
        resource.identity(&connection("a")),
        replacement.identity(&connection("a"))
    );
}

#[test]
fn recovered_api_history_never_contaminates_healthy_sidecar_or_unknown_ephemeral() {
    let observation = parsed(&query(Kind::Pod), list(vec![pod_json()]));
    let Facts::Pod(pod) = &items(&observation)[0].facts else {
        panic!()
    };
    let api = &pod.containers[0];
    let status = api.status.as_ref().unwrap();
    assert_eq!(status.ready, Some(true));
    assert_eq!(status.restart_count, Some(3));
    assert!(status.state.as_ref().unwrap().running.is_some());
    let previous = status
        .last_state
        .as_ref()
        .unwrap()
        .terminated
        .as_ref()
        .unwrap();
    assert_eq!(previous.reason.as_deref(), Some("OOMKilled"));
    assert_eq!(previous.exit_code, 137);
    assert_eq!(
        previous.finished_at.as_deref(),
        Some("2026-01-01T00:59:59Z")
    );
    assert_eq!(status.image_id.as_deref(), Some("sha256:synthetic"));
    assert_eq!(api.spec.resources.requests["cpu"], "100m");
    assert_eq!(api.spec.resources.limits["memory"], "128Mi");
    assert_eq!(
        api.spec.readiness_probe.as_ref().unwrap().period_seconds,
        Some(5)
    );
    let sidecar = pod.containers[1].status.as_ref().unwrap();
    assert_eq!(sidecar.ready, Some(true));
    assert_eq!(sidecar.restart_count, Some(0));
    assert!(sidecar.last_state.as_ref().unwrap().terminated.is_none());
    assert_eq!(pod.containers[2].role, ContainerRole::Init);
    assert_eq!(pod.containers[3].role, ContainerRole::Ephemeral);
    assert!(pod.containers[3].status.is_none());
}

#[test]
fn resolve_explicit_context_without_capturing_credentials() {
    let source = Query::Resolve {
        context: "synthetic/team".into(),
        kubeconfig: Some("synthetic config".into()),
    };
    let observation = parsed(
        &source,
        json!({"contexts":[{"name":"synthetic/team","context":{"cluster":"c","user":"u","namespace":"shop"}}],
        "clusters":[{"name":"c","cluster":{"server":"https://example.invalid", "certificate-authority-data":"synthetic-redacted"}}],
        "users":[{"name":"u","user":{"token":"synthetic-only"}}]}),
    );
    let Data::Connection(resolved) = observation.data else {
        panic!()
    };
    assert_eq!(resolved.cluster, "c");
    assert_eq!(resolved.default_namespace, "shop");
    assert!(!format!("{resolved:?}").contains("synthetic-only"));
    assert_eq!(
        source.args().unwrap(),
        [
            "--context=synthetic/team",
            "--kubeconfig=synthetic config",
            "config",
            "view",
            "--minify",
            "--output=json"
        ]
        .map(OsString::from)
    );
}

#[test]
fn typed_argv_cannot_widen_into_commands_or_namespace_options() {
    let mut source = query(Kind::Pod);
    if let Query::List { connection, .. } = &mut source {
        connection.context = "synthetic/$(echo unsafe) context".into();
    }
    let args = source.args().unwrap();
    assert_eq!(args[0], "--context=synthetic/$(echo unsafe) context");
    assert!(args.contains(&OsString::from(
        "--raw=/api/v1/namespaces/shop/pods?limit=500"
    )));
    assert!(!args
        .iter()
        .any(|arg| arg.to_string_lossy().contains("chunk-size")));
    assert!(!args.contains(&OsString::from("logs")));
    for name in ["", "--all-namespaces", "a;echo", "pods/api"] {
        let get = Query::Get {
            connection: connection("a"),
            namespace: "shop".into(),
            kind: Kind::Pod,
            name: name.into(),
        };
        assert!(get.args().is_err());
    }
    let events = Query::RelatedEvents {
        connection: connection("a"),
        namespace: "shop".into(),
        uid: "uid-a".into(),
    };
    assert!(events.args().unwrap().contains(&OsString::from(
        "--raw=/api/v1/namespaces/shop/events?limit=500&fieldSelector=involvedObject.uid%3Duid-a"
    )));
}

#[test]
fn empty_partial_invalid_and_stale_are_distinct() {
    let source = query(Kind::Pod);
    let empty = parsed(&source, list(vec![]));
    let Data::Resources { items, partial, .. } = &empty.data else {
        panic!()
    };
    assert!(items.is_empty() && partial.is_empty());
    let limited = parsed(&source, list(vec![pod_json(); MAX_ITEMS + 1]));
    let Data::Resources { items, partial, .. } = &limited.data else {
        panic!()
    };
    assert_eq!(items.len(), MAX_ITEMS);
    assert_eq!(partial, &[PartialReason::ItemLimit]);
    let mut continued = list(vec![]);
    continued["metadata"]["continue"] = "opaque-continue".into();
    let Data::Resources { partial, .. } = parsed(&source, continued).data else {
        panic!()
    };
    assert_eq!(partial, vec![PartialReason::ContinueToken]);
    for bytes in [b"not JSON".as_slice(), b"{}", b"{\"items\":[]}"] {
        assert_eq!(
            parse(&source, SystemTime::now(), bytes).unwrap_err().kind,
            FailureKind::InvalidJson
        );
    }
    assert_eq!(
        parse(&source, SystemTime::now(), &vec![b' '; MAX_JSON_BYTES + 1])
            .unwrap_err()
            .kind,
        FailureKind::Limit
    );
    let mut state = SourceState::new(source.clone());
    assert!(state.apply(Attempt {
        sequence: READ_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        source: source.clone(),
        finished_at: empty.observed_at,
        result: Ok(empty)
    }));
    assert!(!state.is_stale(SystemTime::now(), Duration::from_secs(60)));
    assert!(state.apply(Attempt {
        sequence: READ_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        source: source.clone(),
        finished_at: SystemTime::now(),
        result: Err(Failure::new(FailureKind::Forbidden, "synthetic denied"))
    }));
    assert!(state.is_stale(SystemTime::now(), Duration::from_secs(60)));
    assert!(state.retained.is_some());
    assert!(!state.apply(Attempt {
        sequence: READ_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        source: query(Kind::Service),
        finished_at: SystemTime::now(),
        result: Err(Failure::new(FailureKind::Unavailable, "other source"))
    }));
    let fresh = parsed(&source, list(vec![]));
    assert!(state.apply(Attempt {
        sequence: READ_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        source,
        finished_at: fresh.observed_at,
        result: Ok(fresh)
    }));
    assert!(!state.is_stale(SystemTime::now(), Duration::from_secs(60)));
}

#[test]
fn rejects_wrong_namespace_kind_name_and_event_uid() {
    let mut pod = pod_json();
    pod["metadata"]["namespace"] = "elsewhere".into();
    assert!(parse(
        &query(Kind::Pod),
        SystemTime::now(),
        &serde_json::to_vec(&list(vec![pod])).unwrap()
    )
    .is_err());
    assert!(parse(
        &query(Kind::Service),
        SystemTime::now(),
        &serde_json::to_vec(&list(vec![pod_json()])).unwrap()
    )
    .is_err());
    let source = Query::Get {
        connection: connection("a"),
        namespace: "shop".into(),
        kind: Kind::Pod,
        name: "different".into(),
    };
    assert!(parse(
        &source,
        SystemTime::now(),
        &serde_json::to_vec(&pod_json()).unwrap()
    )
    .is_err());
    let mut event = object("Event", "v1");
    event["involvedObject"] = json!({"uid":"uid-b"});
    let source = Query::RelatedEvents {
        connection: connection("a"),
        namespace: "shop".into(),
        uid: "uid-a".into(),
    };
    assert!(parse(
        &source,
        SystemTime::now(),
        &serde_json::to_vec(&list(vec![event])).unwrap()
    )
    .is_err());
}

#[test]
fn workload_service_endpoint_event_hpa_facts_stay_separate() {
    for (kind, wire, api, extra) in [
        (
            Kind::Deployment,
            "Deployment",
            "apps/v1",
            json!({"spec":{"replicas":3,"selector":{"matchLabels":{"app":"checkout"}}},"status":{"readyReplicas":2}}),
        ),
        (
            Kind::ReplicaSet,
            "ReplicaSet",
            "apps/v1",
            json!({"spec":{"selector":{"matchExpressions":[{"key":"app","operator":"Exists"}]}}}),
        ),
        (
            Kind::Service,
            "Service",
            "v1",
            json!({"spec":{"selector":{"app":"checkout"},"ports":[{"port":80,"targetPort":"http"}]}}),
        ),
        (
            Kind::EndpointSlice,
            "EndpointSlice",
            "discovery.k8s.io/v1",
            json!({"addressType":"IPv4","endpoints":[{"addresses":["192.0.2.1"],"conditions":{"ready":false},"targetRef":{"uid":"uid-a"}}],"ports":[{"port":8080}]}),
        ),
        (
            Kind::Event,
            "Event",
            "v1",
            json!({"involvedObject":{"uid":"uid-a"},"reason":"BackOff","count":3}),
        ),
        (
            Kind::Hpa,
            "HorizontalPodAutoscaler",
            "autoscaling/v2",
            json!({"spec":{"scaleTargetRef":{"kind":"Deployment","name":"checkout"},"maxReplicas":5},"status":{"conditions":[{"type":"ScalingActive","status":"False","reason":"FailedGetResourceMetric"}]}}),
        ),
    ] {
        let mut value = object(wire, api);
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let observation = parsed(&query(kind), list(vec![value]));
        assert_eq!(items(&observation)[0].facts.kind(), kind);
        if let Facts::Hpa(hpa) = &items(&observation)[0].facts {
            assert!(hpa.status.as_ref().unwrap().current_replicas.is_none());
            assert!(hpa.status.as_ref().unwrap().current_metrics.is_empty());
        }
    }
}

#[cfg(unix)]
fn fixture(script: &str, source: &Query) -> Session {
    let mut command = Command::new("sh");
    command.args(["-c", script, "synthetic-kubectl"]);
    command.args(source.args().unwrap());
    Session::spawn(command).unwrap()
}
#[cfg(unix)]
fn read_fixture(script: &str, source: &Query) -> Result<Observation, Failure> {
    collect(
        fixture(script, source),
        source,
        SystemTime::now(),
        &AtomicBool::new(false),
        Duration::from_secs(3),
    )
}

#[cfg(unix)]
#[test]
fn denied_namespace_listing_does_not_block_typed_namespaced_access() {
    let denied = read_fixture(
        "printf 'Error from server (Forbidden): namespaces' >&2; exit 1",
        &query(Kind::Namespace),
    )
    .unwrap_err();
    assert_eq!(denied.kind, FailureKind::Forbidden);
    let success = read_fixture(
        "printf '{\"apiVersion\":\"v1\",\"kind\":\"PodList\",\"items\":[]}'",
        &query(Kind::Pod),
    )
    .unwrap();
    assert!(items(&success).is_empty());
    assert_eq!(
        read_fixture(
            "printf 'token has expired: renew credentials' >&2; exit 1",
            &query(Kind::Pod)
        )
        .unwrap_err()
        .kind,
        FailureKind::Authentication
    );
    assert_eq!(
        read_fixture(
            "printf '{\"kind\":\"Status\",\"reason\":\"Forbidden\"}'; exit 1",
            &query(Kind::Pod)
        )
        .unwrap_err()
        .kind,
        FailureKind::Forbidden
    );
    assert_eq!(
        read_fixture("printf 'connection refused' >&2; exit 1", &query(Kind::Pod))
            .unwrap_err()
            .kind,
        FailureKind::Unavailable
    );
    assert_eq!(
        read_fixture("printf 'malformed'", &query(Kind::Pod))
            .unwrap_err()
            .kind,
        FailureKind::InvalidJson
    );
}

#[cfg(unix)]
#[test]
fn oversized_transport_and_stalled_helpers_hit_bounds_and_reap_child() {
    let source = query(Kind::Pod);
    let failure = read_fixture("while :; do printf '%8192s' x; done", &source).unwrap_err();
    assert_eq!(failure.kind, FailureKind::Limit);
    let mut session = fixture("printf 'started'; while :; do :; done", &source);
    // Positive control: this exact owned process ran and its pipe delivered data.
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(2));
        if let Some(Event::Data(bytes)) = session.next(Duration::from_millis(20)).unwrap() {
            assert_eq!(bytes, b"started");
            break;
        }
    }
    let pid = session.child.as_ref().unwrap().id();
    assert!(session
        .child
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_none());
    let failure = collect(
        session,
        &source,
        SystemTime::now(),
        &AtomicBool::new(false),
        Duration::from_millis(50),
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Timeout);
    #[cfg(target_os = "linux")]
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

#[cfg(unix)]
#[test]
fn cancellation_and_drop_reap_a_proven_live_child() {
    for cancelled in [false, true] {
        let source = query(Kind::Pod);
        let mut session = fixture("printf 'started'; while :; do :; done", &source);
        let start = Instant::now();
        loop {
            assert!(start.elapsed() < Duration::from_secs(2));
            if let Some(Event::Data(bytes)) = session.next(Duration::from_millis(20)).unwrap() {
                assert_eq!(bytes, b"started");
                break;
            }
        }
        let pid = session.child.as_ref().unwrap().id();
        assert!(session
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        if cancelled {
            let result = collect(
                session,
                &source,
                SystemTime::now(),
                &AtomicBool::new(true),
                Duration::from_secs(1),
            );
            assert_eq!(result.unwrap_err().kind, FailureKind::Cancelled);
        } else {
            drop(session);
        }
        #[cfg(target_os = "linux")]
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    let kubectl = Kubectl {
        executable: "/nonexistent/synthetic-kubectl".into(),
        search_path: None,
    };
    assert_eq!(
        kubectl
            .read(query(Kind::Pod), &AtomicBool::new(true))
            .result
            .unwrap_err()
            .kind,
        FailureKind::Cancelled
    );
    assert_eq!(
        kubectl
            .read(query(Kind::Pod), &AtomicBool::new(false))
            .result
            .unwrap_err()
            .kind,
        FailureKind::Unavailable
    );
}

#[test]
fn older_read_cannot_overwrite_newer_attempt_or_another_source() {
    let source = query(Kind::Pod);
    let older = Attempt {
        sequence: 1,
        source: source.clone(),
        finished_at: SystemTime::now(),
        result: Ok(parsed(&source, list(vec![pod_json()]))),
    };
    let newer = Attempt {
        sequence: 2,
        source: source.clone(),
        finished_at: SystemTime::now(),
        result: Ok(parsed(&source, list(vec![]))),
    };
    let mut state = SourceState::new(source.clone());
    assert!(state.apply(newer));
    assert!(!state.apply(older));
    assert!(items(state.retained.as_ref().unwrap()).is_empty());
    let forged = Attempt {
        sequence: 3,
        source,
        finished_at: SystemTime::now(),
        result: Ok(parsed(&query(Kind::Service), list(vec![]))),
    };
    assert!(!state.apply(forged));
}

#[test]
fn duplicate_status_does_not_silently_choose_a_container() {
    let mut pod = pod_json();
    pod["status"]["containerStatuses"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"api","ready":false}));
    assert_eq!(
        parse(
            &query(Kind::Pod),
            SystemTime::now(),
            &serde_json::to_vec(&list(vec![pod])).unwrap()
        )
        .unwrap_err()
        .kind,
        FailureKind::InvalidJson
    );
}

#[cfg(unix)]
#[test]
fn gui_executable_and_path_reach_synthetic_credential_helper() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("synthetic-credential-helper");
    std::fs::write(
        &helper,
        "#!/bin/sh\nprintf '{\"apiVersion\":\"v1\",\"kind\":\"PodList\",\"items\":[]}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = directory.path().join("synthetic kubectl");
    std::fs::write(&executable,"#!/bin/sh\n[ \"$1\" = '--context=synthetic/team' ] || exit 9\nsynthetic-credential-helper\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let kubectl = Kubectl {
        executable,
        search_path: Some(directory.path().as_os_str().into()),
    };
    let result = kubectl
        .read(query(Kind::Pod), &AtomicBool::new(false))
        .result
        .unwrap();
    assert!(items(&result).is_empty());
    // Negative control: without the helper's PATH the exact same child fails.
    let negative = Kubectl {
        executable: kubectl.executable.clone(),
        search_path: Some(OsString::from("/nonexistent/synthetic-path")),
    };
    assert!(negative
        .read(query(Kind::Pod), &AtomicBool::new(false))
        .result
        .is_err());
}

#[cfg(unix)]
#[test]
fn interactive_credential_requirement_is_surfaced_before_helper_exit() {
    let source = query(Kind::Pod);
    let start = Instant::now();
    // This process never exits voluntarily: waiting for exit would hit Timeout.
    let failure = read_fixture(
        "printf 'exec plugin: interactive MFA browser login required' >&2; while :; do :; done",
        &source,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::InteractiveLogin);
    assert!(failure.diagnostic.contains("MFA"));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[cfg(unix)]
#[test]
fn successful_stderr_is_bounded_and_not_resource_data() {
    let source = query(Kind::Pod);
    let observation = read_fixture("i=0; while [ \"$i\" -lt 10 ]; do printf '%8192s' x >&2; i=$((i+1)); done; printf '{\"apiVersion\":\"v1\",\"kind\":\"PodList\",\"items\":[]}'", &source).unwrap();
    assert!(items(&observation).is_empty());
    assert!(observation.diagnostic_truncated);
    assert_eq!(observation.diagnostic.len(), MAX_DIAGNOSTIC_BYTES);
}

#[test]
fn every_kind_has_a_fixed_server_page_and_get_has_no_pagination() {
    for (kind, prefix, resource) in [
        (Kind::Namespace, "/api/v1", "namespaces"),
        (Kind::Pod, "/api/v1/namespaces/shop", "pods"),
        (
            Kind::Deployment,
            "/apis/apps/v1/namespaces/shop",
            "deployments",
        ),
        (
            Kind::ReplicaSet,
            "/apis/apps/v1/namespaces/shop",
            "replicasets",
        ),
        (Kind::Service, "/api/v1/namespaces/shop", "services"),
        (
            Kind::EndpointSlice,
            "/apis/discovery.k8s.io/v1/namespaces/shop",
            "endpointslices",
        ),
        (Kind::Event, "/api/v1/namespaces/shop", "events"),
        (
            Kind::Hpa,
            "/apis/autoscaling/v2/namespaces/shop",
            "horizontalpodautoscalers",
        ),
    ] {
        let args = query(kind).args().unwrap();
        assert_eq!(
            args.last().unwrap(),
            &OsString::from(format!("--raw={prefix}/{resource}?limit=500"))
        );
        let get = Query::Get {
            connection: connection("synthetic/team"),
            namespace: "shop".into(),
            kind,
            name: "checkout".into(),
        };
        assert_eq!(
            get.args().unwrap().last().unwrap(),
            &OsString::from(format!("--raw={prefix}/{resource}/checkout"))
        );
    }
    for namespace in ["a.b", "a..b", "-shop", "shop-", &"a".repeat(64)] {
        let invalid = Query::List {
            connection: connection("a"),
            namespace: namespace.into(),
            kind: Kind::Pod,
        };
        assert!(invalid.args().is_err());
    }
}
