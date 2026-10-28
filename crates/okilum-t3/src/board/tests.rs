use super::*;

const CONFIG: &str = include_str!("../../../../docs/examples/okilum-board.yaml");
fn issue(number: u64, state: &str) -> Issue {
    Issue {
        number,
        title: format!("Issue {number}"),
        state: state.into(),
        body: "Actual issue body".into(),
        labels: Vec::new(),
        open_pull_requests: Vec::new(),
    }
}
#[test]
fn explicit_stage_scope_is_stable_across_open_and_closed_issues() {
    let binding = Binding::parse(CONFIG).unwrap();
    let expected = [
        1731, 1733, 1734, 1756, 1759, 1760, 1762, 1767, 1713, 1727, 1729, 1732, 1746, 1755,
    ];
    assert_eq!(
        binding.issues.iter().map(|s| s.number).collect::<Vec<_>>(),
        expected
    );
    for s in &binding.issues {
        assert_eq!(
            column(s, Some(&issue(s.number, "open")), &[]),
            Column::Queue
        );
        assert_eq!(
            column(s, Some(&issue(s.number, "closed")), &[]),
            Column::Checks
        );
    }
    assert_eq!(binding.issues.len(), 14); // No repo-wide open/closed query supplies membership.
}
#[test]
fn current_task_tracks_new_active_runs_without_manual_execution_ids() {
    let mut scope = Binding::parse(CONFIG).unwrap().issues.remove(0);
    let mut thread: ThreadSummary = serde_json::from_value(serde_json::json!({
        "threadId":"t1","title":"executor","status":"running","latestRunId":"r1","activeRunId":"r1",
        "providerInstanceId":"codex","model":"model","runtimeMode":"full-access","updatedAt":"now"
    }))
    .unwrap();
    assert_eq!(
        column(
            &scope,
            Some(&issue(scope.number, "open")),
            &[thread.clone()]
        ),
        Column::Queue
    );
    scope.current_task = true;
    scope.agent_thread_id = Some("t1".into());
    assert_eq!(
        column(
            &scope,
            Some(&issue(scope.number, "open")),
            &[thread.clone()]
        ),
        Column::Working
    );
    thread.latest_run_id = Some("unrelated-later-run".into());
    thread.active_run_id = Some("unrelated-later-run".into());
    assert_eq!(
        column(
            &scope,
            Some(&issue(scope.number, "open")),
            &[thread.clone()]
        ),
        Column::Working
    );
    assert_eq!(
        column(&scope, Some(&issue(scope.number, "closed")), &[thread]),
        Column::Checks
    );
}
#[test]
fn source_failures_are_independent_and_partial_reads_keep_previous_issues() {
    let mut s = Snapshot::new("A".into());
    let t = s.begin().unwrap();
    s.issues(
        &t,
        vec![(1, Ok(issue(1, "open"))), (2, Ok(issue(2, "closed")))],
        10,
    );
    s.threads(&t, Err("T3 disconnected".into()), 11);
    assert_eq!(s.forgejo.last_success, Some(10));
    assert!(s.t3.error.is_some());
    let t = s.begin().unwrap();
    s.threads(&t, Ok(vec![]), 20);
    s.issues(
        &t,
        vec![
            (1, Ok(issue(1, "closed"))),
            (2, Err("Forgejo refused".into())),
        ],
        21,
    );
    assert_eq!(s.forgejo.value[&1].state, "closed");
    assert_eq!(s.forgejo.value[&2].state, "closed");
    assert_eq!(s.forgejo.last_success, Some(10));
    assert_eq!(s.t3.last_success, Some(20));
    assert!(s
        .forgejo
        .error
        .as_ref()
        .unwrap()
        .contains("#2: Forgejo refused"));
    assert!(s.t3.error.is_none());
    let t = s.begin().unwrap();
    s.issues(
        &t,
        vec![
            (1, Err("Forgejo offline".into())),
            (2, Err("Forgejo offline".into())),
        ],
        40,
    );
    s.threads(&t, Err("T3 offline".into()), 41);
    assert_eq!(s.forgejo.value.len(), 2);
    assert_eq!(s.forgejo.last_success, Some(10));
    assert_eq!(s.t3.last_success, Some(20));
}
#[test]
fn hidden_or_switched_project_refuses_late_answers_and_refreshes_coalesce() {
    let mut a = Snapshot::new("A".into());
    let old = a.begin().unwrap();
    assert!(a.begin().is_none());
    a.issues(&old, vec![(1, Ok(issue(1, "open")))], 1);
    assert!(a.busy());
    a.invalidate(); // Same operation called when the Board is hidden.
    let new = a.begin().unwrap();
    a.threads(&old, Ok(vec![]), 10);
    assert!(a.busy());
    assert_eq!(a.t3.last_success, None);
    let mut b = Snapshot::new("B".into());
    b.issues(&new, vec![(1, Ok(issue(1, "closed")))], 20);
    assert!(b.forgejo.value.is_empty());
    a.threads(&new, Ok(vec![]), 30);
    a.issues(&new, vec![(1, Ok(issue(1, "closed")))], 31);
    assert!(!a.busy());
    assert_eq!(a.forgejo.value[&1].state, "closed");
}
#[test]
fn malformed_bindings_and_unsafe_note_or_build_targets_are_visible_errors() {
    for edit in [
        CONFIG.replace("\"version\": 1", "\"version\": 2"),
        CONFIG.replace(
            "https://git.oklabs.uk/BeFeast/okilum",
            "https://user:secret@git.oklabs.uk/BeFeast/okilum",
        ),
        CONFIG.replace("planning/stage-06-defects-t3-poc-logs.md", "../private.md"),
        CONFIG.replace("\"refresh_seconds\": 30", "\"refresh_seconds\": 0"),
        CONFIG.replace("\"number\": 1733", "\"number\": 1731"),
        CONFIG.replace(
            "https://git.oklabs.uk/BeFeast/okilum/actions/runs/13697",
            "javascript:run()",
        ),
    ] {
        assert!(Binding::parse(&edit).is_err(), "accepted unsafe binding");
    }
    assert!(!relative_note("/outside.md"));
    assert!(!relative_note("folder/../../outside.md"));
    assert!(relative_note("decisions/Решение.md"));
}
#[test]
fn reading_binding_preserves_existing_manifest_and_refuses_a_repository_mismatch() {
    let area = std::env::temp_dir().join(format!("okilum-board-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(area.join("ops")).unwrap();
    let original =
        "repo: {host: git.oklabs.uk, full_name: BeFeast/okilum}\nt3: {preserved: true}\n";
    std::fs::write(area.join("ops/project.yaml"), original).unwrap();
    assert!(Binding::read(&area).unwrap().is_none());
    std::fs::write(area.join("ops/board.yaml"), CONFIG).unwrap();
    assert_eq!(Binding::read(&area).unwrap().unwrap().issues.len(), 14);
    assert_eq!(
        std::fs::read_to_string(area.join("ops/project.yaml")).unwrap(),
        original
    );
    std::fs::write(
        area.join("ops/project.yaml"),
        original.replace("BeFeast/okilum", "someone/else"),
    )
    .unwrap();
    assert!(Binding::read(&area).is_err());
    std::fs::remove_dir_all(area).unwrap();
}

#[test]
fn human_delivery_stages_are_explicit_and_labels_keep_their_source() {
    let mut scope = Binding::parse(CONFIG).unwrap().issues.remove(0);
    let closed = issue(scope.number, "closed");
    assert_eq!(column(&scope, Some(&closed), &[]), Column::Checks);
    scope.stage = Some(HumanStage::Ready);
    assert_eq!(column(&scope, Some(&closed), &[]), Column::Ready);
    scope.stage = Some(HumanStage::Verified);
    assert_eq!(column(&scope, Some(&closed), &[]), Column::Verified);
    let issue: Issue = serde_json::from_value(serde_json::json!({
        "number":1767,"title":"Board","state":"open",
        "labels":[{"id":1,"name":"fr"},{"name":"from-oleg"}]
    }))
    .unwrap();
    assert!(issue.has_label("fr"));
    assert!(issue.has_label("from-oleg"));
    assert!(!issue.has_label("bug"));
}

#[test]
fn attention_is_explicit_and_cannot_supply_an_unsafe_destination() {
    let original = Binding::parse(CONFIG).unwrap();
    assert!(original.attention.is_empty());
    let mut source: serde_json::Value = serde_json::from_str(CONFIG).unwrap();
    source["attention"] = serde_json::json!([
        {"kind":"decision","title":"Choose next slice","subline":"Read the decision first","link":"decisions/next.md"},
        {"kind":"try","title":"Try the current build","subline":"No acceptance inferred","link":"https://git.oklabs.uk/BeFeast/okilum/issues/1767"},
        {"kind":"hands","title":"Owner setup","subline":"A manual step","link":"ops/setup.md"}
    ]);
    let attention = Binding::parse(&source.to_string()).unwrap();
    assert_eq!(attention.attention.len(), 3);
    assert!(original.same_issue_source(&attention));
    for link in [
        "../escape.md",
        "/private.md",
        "javascript:alert(1)",
        "https://user:secret@example.org/action",
    ] {
        source["attention"][0]["link"] = serde_json::json!(link);
        assert!(
            Binding::parse(&source.to_string()).is_err(),
            "accepted {link}"
        );
    }
}

#[test]
fn presentation_edits_keep_the_same_issue_source_but_membership_changes_do_not() {
    let original = Binding::parse(CONFIG).unwrap();
    let mut edited = original.clone();
    edited.issues[0].expected = "Updated human wording".into();
    edited.issues[0].stuck = true;
    edited.issues[0].stage = Some(HumanStage::Ready);
    assert!(original.same_issue_source(&edited));
    edited.issues[0].number = 99999;
    assert!(!original.same_issue_source(&edited));
}

#[test]
fn live_agent_bindings_are_literal_and_ambiguous_lists_keep_the_previous_snapshot() {
    let original = Binding::parse(CONFIG).unwrap();
    assert_eq!(original.stuck_after_seconds, 1800);
    assert!(original.issues.iter().all(|s| s.agent_thread_id.is_none()));
    let mut config: serde_json::Value = serde_json::from_str(CONFIG).unwrap();
    config["stuck_after_seconds"] = serde_json::json!(0);
    assert!(Binding::parse(&config.to_string()).is_err());
    config["stuck_after_seconds"] = serde_json::json!(60);
    config["issues"][0]["agent_thread_id"] = serde_json::json!("agent-a");
    let bound = Binding::parse(&config.to_string()).unwrap();
    assert_eq!(bound.issues[0].agent_thread_id.as_deref(), Some("agent-a"));
    assert_eq!(
        column(
            &bound.issues[0],
            Some(&issue(bound.issues[0].number, "open")),
            &[]
        ),
        Column::Queue
    );
    let thread: ThreadSummary = serde_json::from_value(serde_json::json!({
        "threadId":"agent-a","title":"agent","status":"running","latestRunId":"run-a",
        "providerInstanceId":"codex","model":"model","runtimeMode":"full-access","updatedAt":"now"
    }))
    .unwrap();
    let mut snapshot = Snapshot::new("A".into());
    let first = snapshot.begin().unwrap();
    snapshot.threads(&first, Ok(vec![thread.clone()]), 10);
    snapshot.issues(&first, vec![], 10);
    let second = snapshot.begin().unwrap();
    snapshot.threads(&second, Ok(vec![thread.clone(), thread]), 20);
    assert!(snapshot.t3.error.is_some());
    assert_eq!(snapshot.t3.value.len(), 1);
    assert_eq!(snapshot.t3.last_success, Some(10));
}

#[test]
fn open_pr_reference_promotes_only_its_open_issue_without_claiming_delivery() {
    let mut scope = Binding::parse(CONFIG).unwrap().issues.remove(0);
    let mut open = issue(scope.number, "open");
    assert_eq!(column(&scope, Some(&open), &[]), Column::Queue);
    open.open_pull_requests = vec![1850];
    assert_eq!(column(&scope, Some(&open), &[]), Column::Working);
    open.state = "closed".into();
    assert_eq!(column(&scope, Some(&open), &[]), Column::Checks);
    scope.stage = Some(HumanStage::Verified);
    assert_eq!(column(&scope, Some(&open), &[]), Column::Verified);
}
#[test]
fn pr_reference_matching_excludes_other_repositories_and_number_prefixes() {
    let repo = url::Url::parse("https://git.oklabs.uk/BeFeast/okilum").unwrap();
    let mut pull = PullRequest {
        number: 1850,
        state: "open".into(),
        title: String::new(),
        body: String::new(),
        merged: false,
    };
    for body in [
        "Fixes #1860.",
        "Refs #1860 (slice 1)",
        "https://git.oklabs.uk/BeFeast/okilum/issues/1860",
    ] {
        pull.body = body.into();
        assert!(pull.references(&repo, 1860), "{body}");
    }
    for body in [
        "#18601",
        "other/repo#1860",
        "https://other.example/issues/1860",
        "#1860suffix",
        "https://git.oklabs.uk/BeFeast/okilum/issues/18601",
    ] {
        pull.body = body.into();
        assert!(!pull.references(&repo, 1860), "{body}");
    }
}
#[test]
fn an_active_executor_never_promotes_its_entire_backlog_or_ambiguous_threads() {
    let mut scope = Binding::parse(CONFIG).unwrap().issues.remove(0);
    scope.agent_thread_id = Some("current".into());
    let mut thread:ThreadSummary=serde_json::from_value(serde_json::json!({
        "threadId":"current","title":"#1860 is only a title","status":"running","latestRunId":"new-run","activeRunId":"new-run",
        "providerInstanceId":"codex","model":"model","runtimeMode":"full-access","updatedAt":"now"
    })).unwrap();
    let open = issue(scope.number, "open");
    assert_eq!(
        column(&scope, Some(&open), &[thread.clone()]),
        Column::Queue
    );
    scope.current_task = true;
    assert_eq!(
        column(&scope, Some(&open), &[thread.clone()]),
        Column::Working
    );
    assert_eq!(column(&scope, None, &[thread.clone()]), Column::Queue);
    assert_eq!(
        column(&scope, Some(&open), &[thread.clone(), thread.clone()]),
        Column::Queue
    );
    thread.settled = true;
    assert_eq!(
        column(&scope, Some(&open), &[thread.clone()]),
        Column::Queue
    );
    thread.settled = false;
    thread.status = "completed".into();
    assert_eq!(
        column(&scope, Some(&open), &[thread.clone()]),
        Column::Queue
    );
    thread.status = "queued".into();
    assert_eq!(
        column(&scope, Some(&open), &[thread.clone()]),
        Column::Queue
    );
    thread.status = "preparing".into();
    assert_eq!(
        column(&scope, Some(&open), &[thread.clone()]),
        Column::Working
    );
    thread.active_run_id = None;
    assert_eq!(column(&scope, Some(&open), &[thread]), Column::Queue);
}
#[test]
fn current_registry_rotation_supersedes_stale_card_ids() {
    let area = std::env::temp_dir().join(format!("okilum-board-rotation-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(area.join("ops")).unwrap();
    std::fs::write(
        area.join("ops/project.yaml"),
        "repo: {host: git.oklabs.uk, full_name: BeFeast/okilum}\n",
    )
    .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(CONFIG).unwrap();
    value["issues"][0]["executor"] = serde_json::json!("Editing");
    value["issues"][0]["agent_thread_id"] = serde_json::json!("mcp:old");
    std::fs::write(area.join("ops/board.yaml"), value.to_string()).unwrap();
    let registry="## Current threads\n| Role | Thread | Current task |\n|---|---|---|\n| Manager | `mcp:manager` | |\n| Editing | `mcp:launch:current` | #1731 |\n";
    std::fs::write(area.join("ops/thread-ids.md"), registry).unwrap();
    let binding = Binding::read(&area).unwrap().unwrap();
    assert_eq!(
        binding.issues[0].agent_thread_id.as_deref(),
        Some("mcp:launch:current")
    );
    let mut thread: ThreadSummary = serde_json::from_value(serde_json::json!({
        "threadId":"mcp:old","title":"old","status":"running","latestRunId":"run","activeRunId":"run",
        "providerInstanceId":"codex","model":"model","runtimeMode":"full-access","updatedAt":"now"
    }))
    .unwrap();
    let scope = &binding.issues[0];
    let open = issue(scope.number, "open");
    assert_eq!(column(scope, Some(&open), &[thread.clone()]), Column::Queue);
    thread.thread_id = "mcp:launch:current".into();
    assert_eq!(column(scope, Some(&open), &[thread]), Column::Working);
    std::fs::write(area.join("ops/thread-ids.md"), "invalid").unwrap();
    let unknown = Binding::read(&area).unwrap().unwrap();
    assert!(unknown.issues[0].agent_thread_id.is_none());
    assert!(unknown.registry_error.is_some());
    std::fs::remove_dir_all(area).unwrap();
}

#[test]
fn open_pr_reader_checks_real_responses_and_refuses_truncated_or_failed_sources() {
    use std::io::{Read, Write};
    fn serve(status: &str, headers: &str, body: &str) -> (url::Url, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = url::Url::parse(&format!(
            "http://{}/owner/repo",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let response=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n{body}",body.len());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..n])
                .starts_with("GET /api/v1/repos/owner/repo/pulls?state=open&limit=50&page=1 "));
            stream.write_all(response.as_bytes()).unwrap();
        });
        (url, worker)
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let (repo, worker) = serve(
        "200 OK",
        "",
        r##"[{"number":1,"state":"open","body":"Refs #1860"},{"number":2,"state":"closed"},{"number":3,"state":"open","merged":true}]"##,
    );
    let pulls = read_open_pulls(&client, &repo).unwrap();
    worker.join().unwrap();
    assert_eq!(pulls.len(), 1);
    assert!(pulls[0].references(&repo, 1860));
    for (status, headers, body) in [
        (
            "200 OK",
            "Link: <http://example.invalid/page2>; rel=\"next\"\r\n",
            "[]",
        ),
        ("503 Unavailable", "", "[]"),
        ("200 OK", "", "invalid"),
    ] {
        let (repo, worker) = serve(status, headers, body);
        assert!(read_open_pulls(&client, &repo).is_err());
        worker.join().unwrap();
    }
}
