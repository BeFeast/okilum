//! Synthetic objects and bytes only (no cluster, kubeconfig or credentials).
use super::*;
use crate::kubernetes_workspace::model::tests::{fail, fixture, put, recovered_pod};
use ::core::prelude::v1::test;
use okilum_core::kubernetes::{Cancel, Cleanup, Client, End, Kind, Limits, ListQuery, Pod, Raw};
use okilum_core::log::stream::Assembler;

const LOG: &[u8] =
    b"2026-11-16T10:03:59Z INFO start\r\n2026-11-16T10:04:00Z ERROR out of memory \xc3\xa9\n";
const T0: u64 = 1_794_000_000;

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

fn target(data: &Data, previous: bool) -> LogTarget {
    let pod = data.pods().iter().find(|p| p.meta.uid == "pod-a").unwrap();
    LogTarget::new(&data.scope, pod, "api", previous).unwrap()
}

fn session(target: LogTarget, phase: Phase, verified_at: Option<SystemTime>) -> Session {
    Session {
        target,
        phase,
        follow: false,
        verified_at,
        received: true,
        // Never exported, even when it looks like credential output.
        diagnostic: "exec plugin: Bearer sk-SYNTHETIC-DIAGNOSTIC".into(),
        pending_bytes: 7,
    }
}

fn shown(target: LogTarget, generation: u64, verified_at: SystemTime) -> Shown {
    Shown {
        target,
        generation,
        verified_at: Some(verified_at),
        follow: false,
        tail_lines: 1000,
    }
}

/// `log`: the pinned session, the rows' source and the current generation.
fn bundle_with(data: &Data, log: Option<(&Session, Option<&Shown>, u64)>) -> Bundle {
    let key = Key::Container {
        pod: "checkout-a".into(),
        pod_uid: "pod-a".into(),
        container: "api".into(),
    };
    let snapshot = Assembler::new("pod.log".into(), "evidence".into())
        .unwrap()
        .append(LOG)
        .unwrap();
    let rows = log.and_then(|(_, shown, _)| shown);
    build(Inputs {
        data: Some(data),
        selected: Some(&key),
        related_uids: vec!["pod-a".into()],
        lens: "Triage",
        session: log.map(|(s, _, _)| s),
        displayed: rows,
        current_generation: log.map_or(0, |(_, _, g)| g),
        tail_lines: 1000,
        view: rows.map(|_| LogViewState {
            snapshot,
            filter: json!({"active": true, "query": "level:error", "rows_shown": 1, "rows_loaded": 2}),
        }),
        generated_at: at(T0 + 100),
        app_version: "0.0.0-test",
        app_source: "synthetic",
    })
}

fn bundle(data: &Data, with_log: bool) -> Bundle {
    let pinned = target(data, true);
    let s = session(pinned.clone(), Phase::Ended, Some(at(T0)));
    let rows = shown(pinned, 4, at(T0));
    bundle_with(data, with_log.then_some((&s, Some(&rows), 4)))
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

fn everything(bundle: &Bundle) -> String {
    let all: Vec<u8> = bundle
        .files
        .iter()
        .flat_map(|(_, bytes)| bytes.clone())
        .collect();
    String::from_utf8_lossy(&all).into_owned()
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
    let rows = &m["log_rows"];
    assert_eq!(rows["file"], "logs/checkout-a_api_previous.log");
    assert_eq!(rows["from_session"]["target"]["pod_uid"], "pod-a");
    assert_eq!(rows["from_session"]["target"]["instance"], "previous");
    assert_eq!(
        rows["from_session"]["uid_verified_at"],
        "2026-11-06T21:20:00.000Z"
    );
    assert_eq!(rows["from_session"]["is_the_pinned_session"], true);
    assert_eq!(rows["lines"], 2);
    assert_eq!(rows["bytes"], LOG.len());
    assert_eq!(rows["incomplete_trailing_bytes_not_exported"], 7);
    assert_eq!(rows["limits"]["max_lines"], 20_000);
    assert_eq!(rows["view_filter"]["query"], "level:error");
    assert_eq!(rows["log_file_is_unfiltered"], true);
    assert!(rows["coverage"]
        .as_str()
        .unwrap()
        .contains("up to 1000 initial lines"));
    assert!(rows["coverage"].as_str().unwrap().contains("by name"));
    let session = &m["log_session"];
    assert_eq!(session["phase"], "ended");
    assert_eq!(session["uid_against_latest_pods_read"], "same");
    assert_eq!(session["has_sent_rows"], true);
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
    let b = bundle(&data, true);
    let all = everything(&b);
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
    assert!(event_source["latest_failure"]["failure"]["server_message"].is_null());
    assert!(m["excluded"].as_array().unwrap().len() >= 4);
}

/// Review finding 1: a failed UID check is kept typed; its credential text
/// never reaches the bundle through the session's phase.
#[test]
fn a_failed_verification_exports_its_kind_not_its_text() {
    let data = fixture();
    for failure in [
        Failure::AuthExpired {
            detail: "exec plugin printed token sk-SYNTHETIC-VERIFY-AUTH".into(),
        },
        Failure::InteractiveLogin {
            detail: "open https://login.invalid/?code=sk-SYNTHETIC-VERIFY-MFA".into(),
        },
        Failure::Configuration {
            detail: "client-key-data sk-SYNTHETIC-VERIFY-CONFIG".into(),
        },
        Failure::Unavailable {
            detail: "helper sk-SYNTHETIC-VERIFY-NET\nUnable to connect to the server".into(),
        },
    ] {
        let kind = model::failure_kind(&failure);
        let s = session(target(&data, false), Phase::NotStarted(failure), None);
        let b = bundle_with(&data, Some((&s, None, 1)));
        let all = everything(&b);
        assert!(!all.contains("sk-SYNTHETIC-VERIFY"), "{kind}: {all}");
        let m = manifest(&b);
        assert_eq!(
            m["log_session"]["phase"],
            "not started: check before kubectl logs failed"
        );
        assert_eq!(m["log_session"]["phase_detail"]["kind"], kind);
        assert!(m["log_session"]["phase_detail"]["detail"].is_null());
        assert!(m["log_rows"].is_null(), "no rows were received");
    }
}

struct Mixed(Raw);
impl okilum_core::kubernetes::Transport for Mixed {
    fn run(&self, _: &[String], _: Limits, _: &Cancel) -> std::io::Result<Raw> {
        Ok(self.0.clone())
    }
}

/// A failure from the real classifier for this kubectl stderr.
fn classified(stderr: &str) -> Failure {
    let client = Client::new(
        Mixed(Raw {
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            stderr_truncated: false,
            end: End::Exited { code: Some(1) },
            cleanup: Cleanup::NotNeeded,
        }),
        Limits::default(),
    );
    client
        .list::<Pod>(
            &fixture().scope.connection,
            "shop",
            &ListQuery::default(),
            &Cancel::new(),
        )
        .unwrap_err()
}

/// Review finding 2: the classifier keeps all bounded stderr in a detail;
/// only the server's own line is exported, never helper output before it.
#[test]
fn mixed_helper_output_and_server_denial_exports_only_the_server_line() {
    let server = r#"Error from server (Forbidden): pods is forbidden: User "demo" cannot list resource "pods" in API group "" in the namespace "shop""#;
    let denied = classified(&format!(
        "I1117 exec_credential.go:42] helper debug token=sk-SYNTHETIC-HELPER\n{server}\n"
    ));
    assert!(
        matches!(&denied, Failure::Forbidden { detail } if detail.contains("sk-SYNTHETIC-HELPER")),
        "positive control: the real classifier keeps helper output in the detail: {denied:?}"
    );
    let unavailable = classified(
        "helper refresh failed sk-SYNTHETIC-HELPER2\nUnable to connect to the server: dial tcp 10.0.0.1:6443: i/o timeout",
    );
    assert!(
        matches!(unavailable, Failure::Unavailable { .. }),
        "{unavailable:?}"
    );
    let mut data = fixture();
    fail(&mut data.slices, denied);
    fail(&mut data.services, unavailable);
    let b = bundle(&data, true);
    let all = everything(&b);
    assert!(!all.contains("sk-SYNTHETIC-HELPER"), "helper output leaked");
    let m = manifest(&b);
    let slices = &m["sources"][4]["latest_failure"]["failure"];
    assert_eq!(slices["kind"], "forbidden");
    assert_eq!(slices["server_message"], server);
    assert!(slices["detail"].is_null());
    let services = &m["sources"][3]["latest_failure"]["failure"];
    assert_eq!(services["kind"], "unavailable");
    assert!(services["detail"].is_null() && services["server_message"].is_null());
}

/// Review finding 3: rows keep the provenance of the session that sent them.
#[test]
fn rows_after_retarget_failed_check_or_restart_keep_their_own_session() {
    let data = fixture();
    let current = target(&data, false);
    let previous = target(&data, true);
    let rows = shown(current.clone(), 3, at(T0));

    // Retargeted to the previous instance, verified, no data yet.
    let pending = session(previous.clone(), Phase::Receiving, Some(at(T0 + 50)));
    let m = manifest(&bundle_with(&data, Some((&pending, Some(&rows), 4))));
    assert_eq!(m["log_rows"]["file"], "logs/checkout-a_api_current.log");
    assert_eq!(
        m["log_rows"]["from_session"]["target"]["instance"],
        "current"
    );
    assert_eq!(
        m["log_rows"]["from_session"]["uid_verified_at"],
        "2026-11-06T21:20:00.000Z"
    );
    assert_eq!(
        m["log_rows"]["from_session"]["is_the_pinned_session"],
        false
    );
    assert_eq!(
        m["log_rows"]["incomplete_trailing_bytes_not_exported"],
        "unknown: from an earlier session"
    );
    assert_eq!(m["log_session"]["target"]["instance"], "previous");
    assert_eq!(
        m["log_session"]["uid_verified_at"],
        "2026-11-06T21:20:50.000Z"
    );
    assert_eq!(m["log_session"]["has_sent_rows"], false);

    // The retarget's check failed: still the earlier rows, labelled as such.
    let failed = session(
        previous,
        Phase::NotStarted(Failure::Forbidden {
            detail: "pods is forbidden".into(),
        }),
        None,
    );
    let m = manifest(&bundle_with(&data, Some((&failed, Some(&rows), 4))));
    assert_eq!(m["log_rows"]["file"], "logs/checkout-a_api_current.log");
    assert_eq!(
        m["log_rows"]["from_session"]["is_the_pinned_session"],
        false
    );
    assert!(m["log_session"]["uid_verified_at"].is_null());

    // Restarted on the same target: same name, but another session.
    let restarted = session(current, Phase::Verifying, None);
    let m = manifest(&bundle_with(&data, Some((&restarted, Some(&rows), 4))));
    assert_eq!(
        m["log_rows"]["from_session"]["is_the_pinned_session"],
        false
    );
    assert_eq!(m["log_session"]["has_sent_rows"], false);
    // Positive control: the same generation is the pinned session.
    let m = manifest(&bundle_with(&data, Some((&restarted, Some(&rows), 3))));
    assert_eq!(m["log_rows"]["from_session"]["is_the_pinned_session"], true);
}

#[test]
fn bundle_without_a_session_states_no_log_and_selection_gone_is_explicit() {
    let mut data = fixture();
    let b = bundle(&data, false);
    let m = manifest(&b);
    assert!(m["log_session"].is_null() && m["log_rows"].is_null());
    assert!(!b.files.iter().any(|(p, _)| p.starts_with("logs/")));
    // The selected Pod was recreated: the bundle says so instead of
    // describing the new object as the selected one.
    put(
        &mut data.pods,
        vec![recovered_pod("checkout-a", "pod-a2", "rs-1")],
    );
    let m = manifest(&bundle(&data, false));
    assert_eq!(m["selection"]["state"], "not in the latest read");
    assert_eq!(m["selection"]["kind"], Kind::Pod.name());
    assert_eq!(
        m["selection"]["same_name_replacement_uid"], "pod-a2",
        "the same-named Pod is reported as a replacement, never substituted"
    );
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
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
    assert_eq!(
        entries(dir.path()),
        std::slice::from_ref(&b.name),
        "no staging remains"
    );
}

/// Review finding 4: another writer's staging, a name claimed after any
/// check, and colliding concurrent exports are never removed or replaced.
#[test]
fn write_never_touches_other_writers_and_never_replaces_late_claims() {
    let dir = tempfile::tempdir().unwrap();
    let b = bundle(&fixture(), true);
    // An unrelated staging directory with the old deterministic name.
    let foreign = dir.path().join(format!(".{}.partial", b.name));
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("keep"), b"other writer").unwrap();
    // The target name, created empty by someone else.
    let target = dir.path().join(&b.name);
    std::fs::create_dir(&target).unwrap();
    let err = write(dir.path(), &b).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(
        entries(&target).is_empty(),
        "an empty directory at the name is not replaced"
    );
    assert_eq!(
        std::fs::read(foreign.join("keep")).unwrap(),
        b"other writer"
    );
    std::fs::remove_dir(&target).unwrap();

    // Two exports with the same name at the same time: one wins, whole.
    let parent = dir.path().to_path_buf();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let (parent, barrier) = (parent.clone(), barrier.clone());
            let files = b.files.clone();
            let name = b.name.clone();
            std::thread::spawn(move || {
                barrier.wait();
                write(&parent, &Bundle { name, files })
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert!(results
        .iter()
        .any(|r| matches!(r, Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists)));
    for (name, bytes) in &b.files {
        assert_eq!(&std::fs::read(target.join(name)).unwrap(), bytes, "{name}");
    }
    assert_eq!(
        std::fs::read(foreign.join("keep")).unwrap(),
        b"other writer",
        "another writer's staging is never removed"
    );
    let mut expected = vec![b.name.clone(), format!(".{}.partial", b.name)];
    expected.sort();
    assert_eq!(entries(dir.path()), expected, "no staging of ours remains");
}
