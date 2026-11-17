//! Synthetic objects only (no cluster, kubeconfig or credentials).
use super::*;
use crate::kubernetes_workspace::model::tests::{
    crashing_pod, fail, fixture, meta, owner, put, recovered_pod, scope, service,
};
use ::core::prelude::v1::test;
use okilum_core::kubernetes::{Failure, Resource};
use serde_json::{json, Value};

pub(crate) fn healthy_pod(name: &str, uid: &str, rs_uid: &str) -> Pod {
    Pod::decode(json!({
        "metadata": meta(name, uid, owner("ReplicaSet", "checkout-7d9f", rs_uid), json!({"app": "checkout"})),
        "spec": {"containers": [{"name": "api", "image": "registry.invalid/api:3",
                                 "readinessProbe": {"httpGet": {"path": "/ready", "port": 8080}}}]},
        "status": {"phase": "Running",
            "containerStatuses": [{"name": "api", "ready": true, "restartCount": 0,
                "state": {"running": {}}, "lastState": {}}]}
    }))
    .unwrap()
}

/// One slice for Service `checkout` with endpoints `(pod uid, ready)`.
pub(crate) fn slice_of(name: &str, endpoints: &[(&str, bool)]) -> EndpointSlice {
    let endpoints: Vec<Value> = endpoints
        .iter()
        .enumerate()
        .map(|(i, (uid, ready))| {
            json!({"addresses": [format!("10.0.0.{}", i + 10)],
                   "conditions": {"ready": ready, "serving": ready, "terminating": false},
                   "targetRef": {"kind": "Pod", "name": format!("pod-{uid}"), "namespace": "shop", "uid": uid}})
        })
        .collect();
    EndpointSlice::decode(json!({
        "metadata": meta(name, &format!("uid-{name}"), json!([]),
                         json!({"kubernetes.io/service-name": "checkout"})),
        "addressType": "IPv4",
        "endpoints": endpoints,
        "ports": [{"name": "http", "port": 8080, "protocol": "TCP"}]
    }))
    .unwrap()
}

fn observations(data: &Data) -> Vec<Observation> {
    match triage(data) {
        Triage::Ready { observations, .. } => observations,
        other => panic!("expected observations, got {other:?}"),
    }
}

#[test]
fn triage_keeps_recovered_failures_after_readiness_and_orders_by_category() {
    let data = fixture();
    let obs = observations(&data);
    let titles: Vec<_> = obs.iter().map(|o| (o.title.as_str(), o.category)).collect();
    // Failing containers first (deployment Available=False too), recovered last.
    assert_eq!(
        titles,
        [
            ("checkout", Category::CurrentFailure),
            ("api", Category::CurrentFailure),
            ("api", Category::CurrentFailure),
            ("api", Category::Recovered),
        ]
    );
    let recovered = obs.last().unwrap();
    assert!(matches!(&recovered.key, Key::Container { pod_uid, .. } if pod_uid == "pod-a"));
    assert_eq!(recovered.findings.len(), 1);
    assert_eq!(recovered.findings[0].rule, 3);
    assert_eq!(
        recovered.findings[0].detail,
        "Last termination: OOMKilled (exit 137)"
    );
    assert_eq!(
        recovered.findings[0].at.as_deref(),
        Some("2026-11-16T10:04:00Z")
    );
    assert!(recovered.subtitle.contains("3 restarts (cumulative)"));
    // The healthy sidecar of the recovered Pod is not an observation.
    assert!(obs
        .iter()
        .all(|o| !matches!(&o.key, Key::Container { container, .. } if container == "proxy")));
    // A crashing container carries both its current and its last failure.
    let crashing = &obs[1];
    assert_eq!(
        crashing.findings.iter().map(|f| f.rule).collect::<Vec<_>>(),
        [0, 3]
    );
}

#[test]
fn triage_summary_states_denominators_and_cumulative_restarts() {
    let data = fixture();
    let Triage::Ready { summary, stale, .. } = triage(&data) else {
        panic!()
    };
    assert_eq!(summary.pods, 3);
    assert_eq!(summary.pods_ready, 1, "only the recovered Pod is Ready");
    assert_eq!(summary.pods_with_observations, 3, "Ready does not clear it");
    assert_eq!(summary.restarts, Some(3 + 7 + 7));
    assert!(summary.complete && stale.is_none());
}

#[test]
fn healthy_namespace_has_no_observations_but_a_full_denominator() {
    let mut data = Data::new(scope("prod-east", "shop"));
    put(
        &mut data.pods,
        vec![
            healthy_pod("a", "u-a", "rs-1"),
            healthy_pod("b", "u-b", "rs-1"),
        ],
    );
    let Triage::Ready {
        summary,
        observations,
        ..
    } = triage(&data)
    else {
        panic!()
    };
    assert!(observations.is_empty());
    assert_eq!((summary.pods, summary.pods_ready), (2, 2));
    assert_eq!(summary.restarts, Some(0));
}

#[test]
fn triage_unknown_and_stale_are_not_zero() {
    let mut data = Data::new(scope("prod-east", "shop"));
    assert_eq!(triage(&data), Triage::Unread);
    fail(
        &mut data.pods,
        Failure::Forbidden {
            detail: "pods".into(),
        },
    );
    assert!(
        matches!(triage(&data), Triage::Unknown(why) if why.contains("forbidden") && why.contains("not zero"))
    );
    let mut stale = fixture();
    fail(
        &mut stale.pods,
        Failure::Unavailable {
            detail: "network".into(),
        },
    );
    let Triage::Ready {
        stale,
        observations,
        ..
    } = triage(&stale)
    else {
        panic!()
    };
    assert!(stale.unwrap().contains("unavailable"));
    assert_eq!(observations.len(), 4, "earlier observations are kept");
}

#[test]
fn triage_order_is_stable_and_keys_survive_refresh() {
    let mut data = fixture();
    let before: Vec<Key> = observations(&data).into_iter().map(|o| o.key).collect();
    // The same objects again, listed in another order.
    put(
        &mut data.pods,
        vec![
            crashing_pod("orphan-c", "pod-c", "rs-other"),
            recovered_pod("checkout-a", "pod-a", "rs-1"),
            crashing_pod("checkout-b", "pod-b", "rs-1"),
        ],
    );
    let after: Vec<Key> = observations(&data).into_iter().map(|o| o.key).collect();
    assert_eq!(before, after);
}

#[test]
fn service_path_separates_routing_membership_and_ownership() {
    let mut data = fixture();
    put(
        &mut data.pods,
        vec![
            healthy_pod("checkout-a", "pod-a", "rs-1"),
            crashing_pod("checkout-b", "pod-b", "rs-1"),
            healthy_pod("checkout-new", "pod-n", "rs-1"),
        ],
    );
    put(
        &mut data.slices,
        vec![slice_of(
            "checkout-abc",
            &[("pod-a", true), ("pod-b", false), ("pod-gone", false)],
        )],
    );
    let svc = model::latest(&data.services).unwrap().value.items[0].clone();
    let path = service_path(&data, &svc);
    assert_eq!(path.ports, ["80/TCP → 8080"]);
    assert_eq!(path.endpoint_counts(), Some((1, 3)));
    let Routing::Known {
        slices, partial, ..
    } = &path.routing
    else {
        panic!()
    };
    assert!(!partial);
    let targets = &slices[0].endpoints;
    assert!(matches!(targets[0].target, Target::Pod(p) if p.meta.uid == "pod-a"));
    assert_eq!(targets[1].ready, Some(false));
    // A UID the Pods read does not contain is shown as missing, never matched by name.
    assert!(matches!(
        &targets[2].target,
        Target::MissingPod { uid: Some(uid), pods_complete: true, .. } if uid == "pod-gone"
    ));
    // Selected by labels but not an endpoint: membership still changing.
    let pending: Vec<_> = path
        .selected_not_endpoints
        .iter()
        .map(|p| p.meta.name.as_str())
        .collect();
    assert_eq!(pending, ["checkout-new"]);
    // Ownership is a separate relation, through matching UIDs.
    let pod = data.pods().iter().find(|p| p.meta.uid == "pod-a").unwrap();
    assert_eq!(owning_deployment(&data, pod).unwrap().meta.uid, "dep-1");
}

#[test]
fn zero_ready_endpoints_unknown_endpoints_and_empty_selector_differ() {
    let mut data = fixture();
    let svc = model::latest(&data.services).unwrap().value.items[0].clone();
    put(
        &mut data.slices,
        vec![slice_of(
            "checkout-abc",
            &[("pod-a", false), ("pod-b", false)],
        )],
    );
    assert_eq!(
        service_path(&data, &svc).endpoint_counts(),
        Some((0, 2)),
        "zero ready of two known endpoints"
    );
    put(&mut data.slices, vec![]);
    assert_eq!(service_path(&data, &svc).endpoint_counts(), Some((0, 0)));
    let mut denied = Data::new(data.scope.clone());
    fail(
        &mut denied.slices,
        Failure::Forbidden {
            detail: "endpointslices".into(),
        },
    );
    let path = service_path(&denied, &svc);
    assert!(matches!(&path.routing, Routing::Unknown(why) if why.contains("forbidden")));
    assert_eq!(path.endpoint_counts(), None, "unknown, not zero");
    assert!(
        path.selected_not_endpoints.is_empty(),
        "no membership claim"
    );
    // Stale slices keep their membership but say so.
    put(
        &mut data.slices,
        vec![slice_of("checkout-abc", &[("pod-a", true)])],
    );
    fail(
        &mut data.slices,
        Failure::Unavailable {
            detail: "network".into(),
        },
    );
    assert!(matches!(
        &service_path(&data, &svc).routing,
        Routing::Known { stale: Some(_), .. }
    ));
    // No selector: nothing is "selected", slices (if any) are external.
    let manual = service(json!({}));
    let path = service_path(&data, &manual);
    assert!(path.selected_not_endpoints.is_empty());
    let Routing::Known { slices, .. } = &path.routing else {
        panic!()
    };
    assert!(slices[0].endpoints.iter().all(|e| !e.not_selected));
}

#[test]
fn endpoint_pod_no_longer_selected_is_flagged() {
    let mut data = fixture();
    let narrowed = service(json!({"app": "checkout", "track": "canary"}));
    put(
        &mut data.slices,
        vec![slice_of("checkout-abc", &[("pod-a", true)])],
    );
    let path = service_path(&data, &narrowed);
    let Routing::Known { slices, .. } = &path.routing else {
        panic!()
    };
    assert!(slices[0].endpoints[0].not_selected);
}

#[test]
fn find_service_resolves_by_uid_only() {
    let data = fixture();
    let svc = &model::latest(&data.services).unwrap().value.items[0];
    let key = Key::object(Kind::Service, &svc.meta);
    assert_eq!(find_service(&data, &key).unwrap().meta.uid, "svc-1");
    let other = Key::Object {
        kind: Kind::Service,
        name: "checkout".into(),
        uid: "svc-old".into(),
    };
    assert!(find_service(&data, &other).is_none());
}
