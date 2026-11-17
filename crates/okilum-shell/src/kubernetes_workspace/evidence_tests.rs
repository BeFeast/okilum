//! Synthetic objects and bytes only (no cluster, kubeconfig or credentials).
use super::*;
use crate::kubernetes_workspace::model::tests::{fail, fixture};
use ::core::prelude::v1::test;
use okilum_core::kubernetes::Kind;
use okilum_core::log::stream::Assembler;

const LOG: &[u8] =
    b"2026-11-16T10:03:59Z INFO start\r\n2026-11-16T10:04:00Z ERROR out of memory \xc3\xa9\n";

fn session(data: &Data) -> (Session, LogTarget) {
    let pod = data.pods().iter().find(|p| p.meta.uid == "pod-a").unwrap();
    let target = LogTarget::new(&data.scope, pod, "api", true).unwrap();
    (
        Session {
            target: target.clone(),
            phase: Phase::Ended,
            follow: false,
            verified_at: Some(
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_794_000_000),
            ),
            received: true,
            // Never exported, even when it looks like credential output.
            diagnostic: "exec plugin: Bearer sk-SYNTHETIC-DIAGNOSTIC".into(),
            pending_bytes: 7,
        },
        target,
    )
}

fn bundle(data: &Data, with_log: bool) -> Bundle {
    let key = Key::Container {
        pod: "checkout-a".into(),
        pod_uid: "pod-a".into(),
        container: "api".into(),
    };
    let (session, target) = session(data);
    let snapshot = Assembler::new("pod.log".into(), "evidence".into())
        .unwrap()
        .append(LOG)
        .unwrap();
    build(Inputs {
        data: Some(data),
        selected: Some(&key),
        related_uids: vec!["pod-a".into()],
        lens: "Triage",
        session: with_log.then_some(&session),
        displayed: with_log.then_some(&target),
        tail_lines: 1000,
        view: with_log.then(|| LogViewState {
            snapshot,
            filter: json!({"active": true, "query": "level:error", "rows_shown": 1, "rows_loaded": 2}),
        }),
        generated_at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_794_000_100),
        app_version: "0.0.0-test",
        app_source: "synthetic",
    })
}

fn file<'a>(bundle: &'a Bundle, path: &str) -> &'a [u8] {
    &bundle
        .files
        .iter()
        .find(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("{path} in bundle"))
        .1
}

fn manifest(bundle: &Bundle) -> Value {
    serde_json::from_slice(file(bundle, "manifest.json")).unwrap()
}

#[test]
fn bundle_carries_scope_sources_identity_and_exact_log_bytes() {
    let data = fixture();
    let b = bundle(&data, true);
    let names: Vec<&str> = b.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        names,
        [
            "facts/selection.yaml",
            "facts/events.json",
            "logs/checkout-a_api_previous.log",
            "manifest.json",
            "SHA256SUMS"
        ]
    );
    assert!(b
        .name
        .starts_with("okilum-kubernetes-evidence-prod-east-shop-20261106T212140Z"));
    assert_eq!(
        file(&b, "logs/checkout-a_api_previous.log"),
        LOG,
        "exact bytes, CRLF and UTF-8 kept"
    );
    let m = manifest(&b);
    assert_eq!(m["format"], FORMAT);
    assert_eq!(m["read_only"], true);
    assert_eq!(m["scope"]["context"], "prod-east");
    assert_eq!(m["scope"]["namespace"], "shop");
    let sources = m["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 7);
    assert_eq!(sources[0]["kind"], "Pod");
    assert_eq!(sources[0]["state"], "fresh");
    assert_eq!(sources[0]["last_read"]["items"], 3);
    assert_eq!(
        sources[6]["state"], "unread",
        "HPAs were never read in the fixture"
    );
    assert_eq!(m["selection"]["uid"], "pod-a");
    assert_eq!(m["selection"]["resource_version"], "17");
    assert_eq!(m["selection"]["container"]["name"], "api");
    assert_eq!(
        m["selection"]["container"]["previous_instance_observed"],
        true
    );
    let log = &m["log_session"];
    assert_eq!(log["target"]["pod_uid"], "pod-a");
    assert_eq!(log["target"]["instance"], "previous");
    assert_eq!(log["uid_verified_at"], "2026-11-06T21:20:00.000Z");
    assert_eq!(log["uid_against_latest_pods_read"], "same");
    assert_eq!(log["lines"], 2);
    assert_eq!(log["bytes"], LOG.len());
    assert_eq!(log["incomplete_trailing_bytes_not_exported"], 7);
    assert_eq!(log["limits"]["max_lines"], 20_000);
    assert_eq!(log["view_filter"]["query"], "level:error");
    assert_eq!(log["log_file_is_unfiltered"], true);
    assert!(log["coverage"]
        .as_str()
        .unwrap()
        .contains("up to 1000 initial lines"));
    assert!(log["coverage"].as_str().unwrap().contains("by name"));
}

#[test]
fn every_artifact_hash_matches_its_bytes() {
    let b = bundle(&fixture(), true);
    let m = manifest(&b);
    for artifact in m["artifacts"].as_array().unwrap() {
        let path = artifact["path"].as_str().unwrap();
        assert_eq!(artifact["sha256"], sha256(file(&b, path)), "{path}");
        assert_eq!(artifact["bytes"], file(&b, path).len());
    }
    let sums = String::from_utf8(file(&b, "SHA256SUMS").to_vec()).unwrap();
    let lines: Vec<&str> = sums.lines().collect();
    assert_eq!(
        lines.len(),
        b.files.len() - 1,
        "every file except SHA256SUMS itself"
    );
    for line in lines {
        let (hash, path) = line.split_once("  ").unwrap();
        assert_eq!(hash, sha256(file(&b, path)), "{path}");
    }
    // Positive control: a changed byte no longer matches.
    let mut tampered = file(&b, "manifest.json").to_vec();
    tampered[0] ^= 1;
    assert_ne!(sha256(&tampered), sha256(file(&b, "manifest.json")));
}

#[test]
fn credentials_helper_output_and_unretained_values_never_enter_a_bundle() {
    let mut data = fixture();
    fail(
        &mut data.events,
        Failure::AuthExpired {
            detail: "token sk-SYNTHETIC-AUTH-DETAIL expired".into(),
        },
    );
    fail(
        &mut data.slices,
        Failure::Forbidden {
            detail: "endpointslices.discovery.k8s.io is forbidden".into(),
        },
    );
    let b = bundle(&data, true);
    let all: Vec<u8> = b
        .files
        .iter()
        .flat_map(|(_, bytes)| bytes.clone())
        .collect();
    let all = String::from_utf8_lossy(&all);
    for secret in [
        "sk-SYNTHETIC-AUTH-DETAIL",
        "sk-SYNTHETIC-DIAGNOSTIC",
        "do-not-retain",
        "/secret/token",
    ] {
        assert!(!all.contains(secret), "{secret} leaked into the bundle");
    }
    let m = manifest(&b);
    let event_source = &m["sources"][5];
    assert_eq!(event_source["state"], "stale");
    assert_eq!(
        event_source["latest_failure"]["failure"]["kind"],
        "sign-in expired"
    );
    assert!(event_source["latest_failure"]["failure"]["detail"].is_null());
    // The API server's own answer is kept for a denied read.
    let slices = &m["sources"][4];
    assert!(slices["latest_failure"]["failure"]["detail"]
        .as_str()
        .unwrap()
        .contains("forbidden"));
    assert!(m["excluded"].as_array().unwrap().len() >= 4);
}

#[test]
fn bundle_without_a_session_states_no_log_and_selection_gone_is_explicit() {
    let mut data = fixture();
    let b = bundle(&data, false);
    let m = manifest(&b);
    assert!(m["log_session"].is_null());
    assert!(!b.files.iter().any(|(p, _)| p.starts_with("logs/")));
    // The selected Pod was recreated: the bundle says so instead of
    // describing the new object as the selected one.
    crate::kubernetes_workspace::model::tests::put(
        &mut data.pods,
        vec![crate::kubernetes_workspace::model::tests::recovered_pod(
            "checkout-a",
            "pod-a2",
            "rs-1",
        )],
    );
    let m = manifest(&bundle(&data, false));
    assert_eq!(m["selection"]["state"], "not in the latest read");
    assert_eq!(m["selection"]["kind"], Kind::Pod.name());
    assert_eq!(
        m["selection"]["same_name_replacement_uid"], "pod-a2",
        "the same-named Pod is reported as a replacement, never substituted"
    );
}

#[test]
fn write_creates_a_new_directory_whole_and_never_replaces_one() {
    let dir = tempfile::tempdir().unwrap();
    let b = bundle(&fixture(), true);
    let path = write(dir.path(), &b).unwrap();
    assert_eq!(path, dir.path().join(&b.name));
    for (name, bytes) in &b.files {
        assert_eq!(&std::fs::read(path.join(name)).unwrap(), bytes, "{name}");
    }
    let err = write(dir.path(), &b).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, [b.name.clone()], "no partial directory remains");
}
