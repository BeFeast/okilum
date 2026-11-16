//! Synthetic objects only (no cluster, kubeconfig or credentials).
use super::*;
use ::core::prelude::v1::test;
use okilum_core::kubernetes::{Completeness, Resource};
use serde_json::{json, Value};

pub(crate) fn connection(context: &str) -> Connection {
    Connection {
        context: context.into(),
        cluster: Some(format!("{context}-cluster")),
        server: Some("https://cluster.invalid:6443".into()),
        default_namespace: None,
    }
}

pub(crate) fn scope(context: &str, namespace: &str) -> Scope {
    Scope {
        connection: connection(context),
        namespace: namespace.into(),
    }
}

pub(crate) fn meta(name: &str, uid: &str, owners: Value, labels: Value) -> Value {
    json!({"name": name, "namespace": "shop", "uid": uid, "resourceVersion": "17",
           "labels": labels, "ownerReferences": owners})
}

pub(crate) fn owner(kind: &str, name: &str, uid: &str) -> Value {
    json!([{"apiVersion": "apps/v1", "kind": kind, "name": name, "uid": uid, "controller": true}])
}

/// A Ready Pod whose app container recovered from an OOMKill, next to a
/// healthy native sidecar with no restarts.
pub(crate) fn recovered_pod(name: &str, uid: &str, rs_uid: &str) -> Pod {
    Pod::decode(json!({
        "metadata": meta(name, uid, owner("ReplicaSet", "checkout-7d9f", rs_uid), json!({"app": "checkout"})),
        "spec": {
            "nodeName": "node-a",
            "initContainers": [{"name": "proxy", "image": "registry.invalid/proxy:2", "restartPolicy": "Always"}],
            "containers": [{"name": "api", "image": "registry.invalid/api:3",
                            "readinessProbe": {"exec": {"command": ["cat", "/secret/token"]}},
                            "env": [{"name": "TOKEN", "value": "do-not-retain"}]}]
        },
        "status": {
            "phase": "Running",
            "initContainerStatuses": [
                {"name": "proxy", "ready": true, "started": true, "restartCount": 0,
                 "state": {"running": {"startedAt": "2026-11-16T10:00:00Z"}}, "lastState": {}}],
            "containerStatuses": [
                {"name": "api", "ready": true, "started": true, "restartCount": 3,
                 "state": {"running": {"startedAt": "2026-11-16T10:05:00Z"}},
                 "lastState": {"terminated": {"exitCode": 137, "reason": "OOMKilled",
                    "startedAt": "2026-11-16T10:01:00Z", "finishedAt": "2026-11-16T10:04:00Z"}}}]
        }
    }))
    .unwrap()
}

pub(crate) fn crashing_pod(name: &str, uid: &str, rs_uid: &str) -> Pod {
    Pod::decode(json!({
        "metadata": meta(name, uid, owner("ReplicaSet", "checkout-7d9f", rs_uid), json!({"app": "checkout"})),
        "spec": {"containers": [{"name": "api", "image": "registry.invalid/api:3"},
                                {"name": "metrics", "image": "registry.invalid/metrics:1"}]},
        "status": {
            "phase": "Running",
            "containerStatuses": [
                {"name": "api", "ready": false, "restartCount": 7,
                 "state": {"waiting": {"reason": "CrashLoopBackOff"}},
                 "lastState": {"terminated": {"exitCode": 1, "reason": "Error"}}},
                {"name": "metrics", "ready": true, "restartCount": 0,
                 "state": {"running": {}}, "lastState": {}}]
        }
    }))
    .unwrap()
}

pub(crate) fn replica_set(name: &str, uid: &str, deployment_uid: &str) -> ReplicaSet {
    ReplicaSet::decode(json!({
        "metadata": meta(name, uid, owner("Deployment", "checkout", deployment_uid), json!({"app": "checkout"})),
        "spec": {"replicas": 2, "selector": {"matchLabels": {"app": "checkout"}}},
        "status": {"replicas": 2, "readyReplicas": 1, "availableReplicas": 1}
    }))
    .unwrap()
}

pub(crate) fn deployment(uid: &str) -> Deployment {
    Deployment::decode(json!({
        "metadata": meta("checkout", uid, json!([]), json!({"app": "checkout"})),
        "spec": {"replicas": 2, "selector": {"matchLabels": {"app": "checkout"}}},
        "status": {"observedGeneration": 4, "replicas": 2, "readyReplicas": 1,
                   "availableReplicas": 1, "updatedReplicas": 2, "unavailableReplicas": 1,
                   "conditions": [{"type": "Available", "status": "False",
                                   "reason": "MinimumReplicasUnavailable"}]}
    }))
    .unwrap()
}

pub(crate) fn service(selector: Value) -> Service {
    Service::decode(json!({
        "metadata": meta("checkout", "svc-1", json!([]), json!({})),
        "spec": {"type": "ClusterIP", "clusterIP": "10.96.0.10", "selector": selector,
                 "ports": [{"port": 80, "protocol": "TCP", "targetPort": 8080}]}
    }))
    .unwrap()
}

pub(crate) fn slice(pod_uid: &str, ready: bool) -> EndpointSlice {
    EndpointSlice::decode(json!({
        "metadata": meta("checkout-abc", "slice-1", json!([]),
                         json!({"kubernetes.io/service-name": "checkout"})),
        "addressType": "IPv4",
        "endpoints": [{"addresses": ["10.0.0.7"], "conditions": {"ready": ready, "serving": ready},
                       "targetRef": {"kind": "Pod", "name": "checkout-a", "namespace": "shop", "uid": pod_uid}}],
        "ports": [{"port": 8080, "protocol": "TCP"}]
    }))
    .unwrap()
}

pub(crate) fn event(
    uid: &str,
    kind: &str,
    name: &str,
    reason: &str,
    count: i32,
    last: &str,
) -> Event {
    Event::decode(json!({
        "metadata": meta(&format!("{name}.{reason}.{last}"), &format!("ev-{reason}-{last}"), json!([]), json!({})),
        "involvedObject": {"kind": kind, "name": name, "namespace": "shop", "uid": uid},
        "type": if reason == "Scheduled" { "Normal" } else { "Warning" },
        "reason": reason, "message": format!("{reason} at {last}"), "count": count,
        "lastTimestamp": last
    }))
    .unwrap()
}

pub(crate) fn put<T>(slot: &mut Slot<List<T>>, items: Vec<T>) {
    let ticket = slot.begin();
    let observation = Observation {
        source: slot.source().clone(),
        observed_at: SystemTime::now(),
        value: List {
            items,
            resource_version: None,
            completeness: Completeness::Complete,
            rejected: Vec::new(),
            rejected_total: 0,
        },
    };
    assert!(slot.record(ticket, Ok(observation), SystemTime::now()));
}

pub(crate) fn fail<T>(slot: &mut Slot<List<T>>, failure: Failure) {
    let ticket = slot.begin();
    assert!(slot.record(ticket, Err(failure), SystemTime::now()));
}

pub(crate) fn fixture() -> Data {
    let mut data = Data::new(scope("prod-east", "shop"));
    put(
        &mut data.pods,
        vec![
            recovered_pod("checkout-a", "pod-a", "rs-1"),
            crashing_pod("checkout-b", "pod-b", "rs-1"),
            // Names the same ReplicaSet but with a UID that is not observed.
            crashing_pod("orphan-c", "pod-c", "rs-other"),
        ],
    );
    put(
        &mut data.replica_sets,
        vec![replica_set("checkout-7d9f", "rs-1", "dep-1")],
    );
    put(&mut data.deployments, vec![deployment("dep-1")]);
    put(
        &mut data.services,
        vec![service(json!({"app": "checkout"}))],
    );
    put(&mut data.slices, vec![slice("pod-a", true)]);
    put(
        &mut data.events,
        vec![
            event(
                "pod-b",
                "Pod",
                "checkout-b",
                "BackOff",
                4,
                "2026-11-16T10:06:00Z",
            ),
            event(
                "pod-b",
                "Pod",
                "checkout-b",
                "BackOff",
                2,
                "2026-11-16T10:09:00Z",
            ),
            event(
                "pod-a",
                "Pod",
                "checkout-a",
                "Scheduled",
                1,
                "2026-11-16T10:00:00Z",
            ),
            event(
                "rs-1",
                "ReplicaSet",
                "checkout-7d9f",
                "SuccessfulCreate",
                1,
                "2026-11-16T09:59:00Z",
            ),
            event(
                "pod-old",
                "Pod",
                "checkout-b",
                "Killing",
                1,
                "2026-11-16T09:00:00Z",
            ),
        ],
    );
    data
}

#[test]
fn recovered_pod_keeps_previous_oom_and_sidecar_inherits_nothing() {
    let pod = recovered_pod("checkout-a", "pod-a", "rs-1");
    let row = pod_row(&pod);
    assert_eq!(
        row.ready.as_deref(),
        Some("2/2"),
        "sidecar counts as kubectl does"
    );
    assert_eq!(row.restarts, Some(3));
    assert_eq!(row.reasons, ["api: last: OOMKilled (exit 137)"]);
    assert_eq!(row.tone, Tone::Warning, "recovered, not healthy-looking");
    let proxy = container_row(&pod, pod.container("proxy").unwrap());
    assert_eq!(proxy.restarts, Some(0));
    assert!(
        proxy.reasons.is_empty(),
        "no failure copied into the sidecar"
    );
    assert_eq!(proxy.tone, Tone::Good);
    assert_eq!(proxy.status, "Running");
    let api = container_row(&pod, pod.container("api").unwrap());
    assert_eq!(api.reasons, ["last: OOMKilled (exit 137)"]);
    assert_eq!(api.ready.as_deref(), Some("ready"));
    assert!(pod.container("api").unwrap().previous_instance_observed());
    assert!(!pod.container("proxy").unwrap().previous_instance_observed());
}

#[test]
fn readiness_restarts_and_reasons_are_separate_per_container() {
    let pod = crashing_pod("checkout-b", "pod-b", "rs-1");
    let row = pod_row(&pod);
    assert_eq!(row.ready.as_deref(), Some("1/2"));
    assert_eq!(row.restarts, Some(7));
    assert_eq!(
        row.reasons,
        ["api: CrashLoopBackOff", "api: last: Error (exit 1)"]
    );
    assert_eq!(row.status, "Running", "phase stays the phase");
    assert_eq!(row.tone, Tone::Danger);
    let metrics = container_row(&pod, pod.container("metrics").unwrap());
    assert!(metrics.reasons.is_empty() && metrics.tone == Tone::Good);
    assert_eq!(metrics.restarts, Some(0));
}

#[test]
fn ownership_links_only_observed_matching_uids() {
    let data = fixture();
    let pods = data.pods();
    let chain = owner_chain(&data, &pods[0].meta);
    assert_eq!(chain.len(), 2, "Pod → ReplicaSet → Deployment");
    assert_eq!(chain[0].kind_name, "ReplicaSet");
    assert!(matches!(&chain[0].key, Some(Key::Object { uid, .. }) if uid == "rs-1"));
    assert!(matches!(&chain[1].key, Some(Key::Object { uid, .. }) if uid == "dep-1"));
    // Same owner name, unobserved UID: recorded but never linked.
    let orphan = owner_chain(&data, &pods[2].meta);
    assert_eq!(orphan.len(), 1);
    assert_eq!(orphan[0].name, "checkout-7d9f");
    assert!(orphan[0].key.is_none() && orphan[0].note.is_some());
    let dep = &latest(&data.deployments).unwrap().value.items[0];
    let children: Vec<_> = pods_of_deployment(&data, dep)
        .iter()
        .map(|p| p.meta.uid.clone())
        .collect();
    assert_eq!(children, ["pod-a", "pod-b"], "the orphan is not a child");
}

#[test]
fn selection_endpoint_membership_and_unknown_endpoints_stay_distinct() {
    let mut data = fixture();
    let svc = latest(&data.services).unwrap().value.items[0].clone();
    let selected: Vec<_> = pods_selected_by(&data, &svc)
        .iter()
        .map(|p| p.meta.name.clone())
        .collect();
    assert_eq!(selected, ["checkout-a", "checkout-b"], "label selection");
    assert_eq!(memberships(&data, "pod-a").len(), 1);
    assert!(
        memberships(&data, "pod-b").is_empty(),
        "selected is not a member"
    );
    assert_eq!(
        service_endpoints(&data, &svc),
        Endpoints::Known {
            ready: 1,
            total: 1,
            partial: false
        }
    );
    let svc_row = service_row(&data, &svc);
    assert_eq!(svc_row.ready.as_deref(), Some("1/1"));
    // A forbidden slice read with nothing earlier: unknown, never zero.
    let mut denied = Data::new(data.scope.clone());
    fail(
        &mut denied.slices,
        Failure::Forbidden {
            detail: "endpointslices".into(),
        },
    );
    let row = service_row(&denied, &svc);
    assert_eq!(row.status, "Endpoints unknown (forbidden)");
    assert_eq!(row.tone, Tone::Neutral);
    assert!(row.ready.is_none());
    // A later failure keeps the earlier slices (stale), still counted.
    fail(
        &mut data.slices,
        Failure::Unavailable {
            detail: "network".into(),
        },
    );
    assert!(matches!(
        service_endpoints(&data, &svc),
        Endpoints::Known { ready: 1, .. }
    ));
    assert!(freshness(&data.slices, SystemTime::now())
        .0
        .starts_with("stale since"));
    // A Service never owns Pods.
    assert!(owners(&data, &svc.meta).is_empty());
}

#[test]
fn events_group_by_involved_object_and_ignore_older_uids() {
    let data = fixture();
    let dep = &latest(&data.deployments).unwrap().value.items[0];
    let mut uids = vec![dep.meta.uid.as_str()];
    let rs = replica_sets_of(&data, dep);
    uids.extend(rs.iter().map(|r| r.meta.uid.as_str()));
    let pods = pods_of_deployment(&data, dep);
    uids.extend(pods.iter().map(|p| p.meta.uid.as_str()));
    let groups = event_groups(&data, &uids);
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].involved, "Pod/checkout-b");
    assert_eq!(groups[0].reason, "BackOff");
    assert_eq!(groups[0].count, 6);
    assert_eq!(
        groups[0].message, "BackOff at 2026-11-16T10:09:00Z",
        "newest message"
    );
    assert!(groups
        .iter()
        .any(|g| g.involved == "ReplicaSet/checkout-7d9f"));
    assert!(
        groups.iter().all(|g| g.reason != "Killing"),
        "events of an earlier Pod with the same name are not mixed in"
    );
    // A recovered container's lastState is container status, not an Event.
    assert!(event_groups(&data, &["pod-a"])
        .iter()
        .all(|g| !g.reason.contains("OOM")));
}

#[test]
fn log_target_pins_uid_and_detects_replacement() {
    let mut data = fixture();
    let pod = data.pods()[0].clone();
    let current = LogTarget::new(&data.scope, &pod, "api", false).unwrap();
    assert_eq!(current.pod_uid, "pod-a");
    assert_eq!(current.context, "prod-east");
    assert!(current.label().contains("Pod UID pod-a"));
    assert!(LogTarget::new(&data.scope, &pod, "api", true).is_ok());
    assert!(
        LogTarget::new(&data.scope, &pod, "proxy", true).is_err(),
        "no previous instance observed"
    );
    assert!(LogTarget::new(&data.scope, &pod, "missing", false).is_err());
    assert_eq!(check_listed(&current, &data), UidCheck::Same);
    // The name now refers to a recreated Pod.
    put(
        &mut data.pods,
        vec![recovered_pod("checkout-a", "pod-a2", "rs-1")],
    );
    assert_eq!(
        check_listed(&current, &data),
        UidCheck::Replaced {
            uid: "pod-a2".into()
        }
    );
    put(&mut data.pods, vec![]);
    assert_eq!(check_listed(&current, &data), UidCheck::Missing);
    // Another scope says nothing about the pinned Pod.
    let other = Data::new(scope("staging", "shop"));
    assert_eq!(check_listed(&current, &other), UidCheck::Unknown);
    let fetched = Observation {
        source: data.scope.source(Kind::Pod),
        observed_at: SystemTime::now(),
        value: recovered_pod("checkout-a", "pod-a2", "rs-1"),
    };
    assert_eq!(
        check_fetched(&current, &fetched),
        UidCheck::Replaced {
            uid: "pod-a2".into()
        }
    );
    let same = Observation {
        value: pod,
        ..fetched
    };
    assert_eq!(check_fetched(&current, &same), UidCheck::Same);
}

#[test]
fn selection_by_uid_offers_but_never_substitutes_a_replacement() {
    let mut data = fixture();
    let key = Key::object(Kind::Pod, &data.pods()[0].meta);
    assert!(matches!(resolve(&data, &key), Resolved::Pod(p) if p.meta.uid == "pod-a"));
    put(
        &mut data.pods,
        vec![recovered_pod("checkout-a", "pod-a2", "rs-1")],
    );
    match resolve(&data, &key) {
        Resolved::Gone {
            replacement: Some(Key::Object { uid, .. }),
        } => assert_eq!(uid, "pod-a2"),
        _ => panic!("the old UID is gone and the new one is only offered"),
    }
}

#[test]
fn empty_forbidden_and_unread_are_different_states() {
    let s = scope("prod-east", "shop");
    let mut data = Data::new(s.clone());
    assert_eq!(empty_reason(&data.pods, "Pods", &s), "Pods not read yet.");
    fail(
        &mut data.pods,
        Failure::Forbidden {
            detail: "pods is forbidden".into(),
        },
    );
    assert!(empty_reason(&data.pods, "Pods", &s).contains("could not be read: Forbidden"));
    assert_eq!(freshness(&data.pods, SystemTime::now()).1, Tone::Danger);
    let mut empty = Data::new(s.clone());
    put(&mut empty.pods, vec![]);
    assert!(empty_reason(&empty.pods, "Pods", &s).contains("successful read"));
}

#[test]
fn filter_covers_names_statuses_reasons_and_images() {
    let data = fixture();
    let names = |section, q| -> Vec<String> {
        rows(&data, section, q)
            .into_iter()
            .map(|r| r.title)
            .collect()
    };
    assert_eq!(names(Section::Pods, "oomkilled"), ["checkout-a"]);
    assert_eq!(names(Section::Containers, "proxy:2"), ["proxy"]);
    assert_eq!(
        names(Section::Pods, "CRASHLOOP"),
        ["checkout-b", "orphan-c"]
    );
    assert_eq!(names(Section::Deployments, ""), ["checkout"]);
    let dep = &rows(&data, Section::Deployments, "")[0];
    assert_eq!(dep.ready.as_deref(), Some("1/2"));
    assert_eq!(dep.tone, Tone::Danger);
    assert!(dep
        .reasons
        .contains(&"Available: MinimumReplicasUnavailable".to_string()));
    assert_eq!(rows(&data, Section::Containers, "").len(), 6);
}

#[test]
fn manifest_is_typed_and_never_contains_unretained_values() {
    let data = fixture();
    let pod = &data.pods()[0];
    let text = manifest(&Resolved::Pod(pod)).unwrap();
    assert!(text.contains("uid: pod-a"));
    assert!(text.contains("initContainers:"));
    assert!(text.contains("restartPolicy: Always"));
    assert!(text.contains("reason: OOMKilled"));
    assert!(text.contains("lastState:"));
    assert!(!text.contains("secret") && !text.contains("do-not-retain"));
    let dep = &latest(&data.deployments).unwrap().value.items[0];
    let text = manifest(&Resolved::Deployment(dep)).unwrap();
    assert!(text.contains("kind: Deployment") && text.contains("matchLabels:"));
}
