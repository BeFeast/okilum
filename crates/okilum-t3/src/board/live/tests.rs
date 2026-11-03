use super::*;
use serde_json::json;

fn binding() -> Binding {
    Binding::parse(include_str!("../../../tests/fixtures/board-legacy.yaml")).unwrap()
}
fn issue(number: u64, state: &str) -> serde_json::Value {
    json!({"number": number, "title": format!("Issue {number}"), "state":state})
}
fn pull(number: u64, state: &str, body: &str) -> serde_json::Value {
    json!({"number":number, "title":"Work", "state":state, "body":body,
        "merged":state=="closed", "merged_at":if state=="closed" {json!("1970-01-02T00:00:00Z")} else {json!(null)},
        "updated_at":"1970-01-02T00:00:00Z", "head":{"sha":"abc123"}})
}
#[test]
fn repository_membership_closure_and_live_pr_links_need_no_manifest_edit() {
    let mut binding = binding();
    binding.issues.clear();
    let repo = repository_url(&binding.repository).unwrap();
    let mut closed = false;
    for cycle in 0..2 {
        let issues = read_with(&binding, &repo, 86500, &mut |path| {
            Ok(match path {
                "issues?state=open&type=issues&limit=50&page=1" => {
                    if closed {
                        json!([])
                    } else {
                        json!([issue(42, "open"), issue(43, "open")])
                    }
                }
                "pulls?state=open&limit=50&page=1" => {
                    if closed {
                        json!([])
                    } else {
                        json!([pull(7, "open", "Closes #42")])
                    }
                }
                "pulls?state=closed&sort=recentupdate&limit=50&page=1" => json!([]),
                "commits/abc123/status" => json!({"state":"pending"}),
                unexpected => panic!("unexpected source {unexpected}"),
            })
        })
        .unwrap();
        let issues = issues.into_iter().map(|i| (i.number, i)).collect();
        let projected = project(&binding, &issues, 86500);
        if cycle == 0 {
            assert_eq!(
                projected.issues.len(),
                2,
                "positive control: discovery actually fired"
            );
            assert_eq!(projected.issues[0].pull_requests, vec![7]);
            assert!(checks(&issues[&42]));
            assert_eq!(
                column(&projected.issues[0], Some(&issues[&42]), &[]),
                Column::Checks
            );
            let mut successful = issues[&42].clone();
            successful.live_pulls[0].checks = "success".into();
            assert_eq!(
                column(&projected.issues[0], Some(&successful), &[]),
                Column::Working
            );
            assert!(!checks(&issues[&43]));
        } else {
            assert!(
                projected.issues.is_empty(),
                "closed issues leave Queue/Checks on next refresh"
            );
        }
        closed = true;
    }
    assert!(
        binding.issues.is_empty(),
        "derived state never writes the binding"
    );
}
#[test]
fn merged_pr_keeps_closed_issue_and_link_for_one_day_without_claiming_delivery() {
    let mut binding = binding();
    binding.issues.clear();
    let repo = repository_url(&binding.repository).unwrap();
    let issues = read_with(&binding, &repo, 86500, &mut |path| {
        Ok(match path {
            "issues?state=open&type=issues&limit=50&page=1"
            | "pulls?state=open&limit=50&page=1" => json!([]),
            "pulls?state=closed&sort=recentupdate&limit=50&page=1" => {
                json!([pull(7, "closed", "Closes #42")])
            }
            "issues/42" => issue(42, "closed"),
            unexpected => panic!("unexpected source {unexpected}"),
        })
    })
    .unwrap()
    .into_iter()
    .map(|i| (i.number, i))
    .collect();
    let projected = project(&binding, &issues, 86500);
    assert_eq!(projected.issues[0].pull_requests, vec![7]);
    assert_eq!(
        column(&projected.issues[0], Some(&issues[&42]), &[]),
        Column::Checks,
        "merge alone cannot claim an available build"
    );
    assert!(project(&binding, &issues, 172800).issues.is_empty());
}
#[test]
fn explicit_pins_survive_closure_and_missing_pages_are_errors() {
    let mut binding = binding();
    binding.issues.truncate(1);
    binding.issues[0].pinned = true;
    let number = binding.issues[0].number;
    let repo = repository_url(&binding.repository).unwrap();
    let issues = read_with(&binding, &repo, 86500, &mut |path| {
        if path == format!("issues/{number}") {
            return Ok(issue(number, "closed"));
        }
        Ok(json!([]))
    })
    .unwrap()
    .into_iter()
    .map(|i| (i.number, i))
    .collect();
    assert_eq!(project(&binding, &issues, 86500).issues.len(), 1);
    let error = read_with(&binding, &repo, 86500, &mut |path| {
        if path.contains("page=2") {
            anyhow::bail!("offline page failure");
        }
        Ok(json!((1..=50)
            .map(|n| issue(n, "open"))
            .collect::<Vec<_>>()))
    })
    .unwrap_err();
    assert!(error.to_string().contains("offline page failure"));
}
#[test]
fn failed_or_late_repository_read_cannot_remove_the_last_good_cards() {
    let mut snapshot = Snapshot::new("project".into());
    let ticket = snapshot.begin().unwrap();
    snapshot.live_issues(
        &ticket,
        Ok(vec![serde_json::from_value(issue(42, "open")).unwrap()]),
        10,
    );
    snapshot.threads(&ticket, Ok(Vec::new()), 10);
    let ticket = snapshot.begin().unwrap();
    snapshot.live_issues(&ticket, Err("page failed".into()), 20);
    assert!(snapshot.forgejo.value.contains_key(&42));
    assert_eq!(snapshot.forgejo.last_success, Some(10));
    snapshot.invalidate();
    snapshot.live_issues(&ticket, Ok(Vec::new()), 30);
    assert!(snapshot.forgejo.value.contains_key(&42));
    let ticket = snapshot.begin().unwrap();
    snapshot.live_issues(&ticket, Ok(Vec::new()), 40);
    assert!(snapshot.forgejo.value.is_empty());
}

#[test]
fn discovered_merge_enters_can_try_only_with_existing_exact_build_delivery_proof() {
    use super::super::delivery as d;
    let mut binding = binding();
    binding.issues.clear();
    let mut merged: Issue = serde_json::from_value(issue(42, "closed")).unwrap();
    merged.live_pulls.push(Pull {
        number: 7,
        merged_at: Some(86400),
        checks: String::new(),
    });
    let projected = project(&binding, &BTreeMap::from([(42, merged)]), 86500);
    let scope = &projected.issues[0];
    let track = (d::Platform::Linux, d::Channel::Beta);
    let build = d::Build {
        platform: track.0,
        channel: track.1,
        build: "100".into(),
        source: "a".repeat(40),
        artifact: "https://build.example/100.AppImage".into(),
        published_at: None,
        release_url: None,
    };
    let mut delivery = d::Snapshot::default();
    delivery.config.accept(
        Ok(Some(d::Config {
            version: 1,
            evidence: Vec::new(),
            feeds: vec![d::Feed {
                platform: track.0,
                channel: track.1,
                url: "https://build.example/feed".into(),
                format: d::Format::Receipt,
                metadata: None,
                artifact_name: None,
                next: String::new(),
                blocker: None,
            }],
        })),
        86500,
    );
    delivery
        .feeds
        .entry(track)
        .or_default()
        .accept(Ok(Some(build.clone())), 86500);
    delivery
        .artifacts
        .entry(build.artifact.clone())
        .or_default()
        .accept(Ok(d::Artifact { available: true }), 86500);
    assert!(
        !delivery.rows(scope, track.0, 86500, 30)[0].ready,
        "published artifact alone is not inclusion"
    );
    delivery.ci.entry(7).or_default().accept(
        Ok(d::Ci {
            pr: 7,
            source: build.source.clone(),
            state: "success".into(),
            links: Vec::new(),
            inclusion: BTreeMap::from([(build.source.clone(), Ok(true))]),
        }),
        86500,
    );
    assert!(
        !delivery.rows(scope, track.0, 86500, 30)[0].ready,
        "inclusion still requires the existing scenario gate"
    );
    delivery
        .config
        .value
        .as_mut()
        .unwrap()
        .evidence
        .push("try.md".into());
    delivery
        .evidence
        .entry("try.md".into())
        .or_default()
        .accept(
            Ok(vec![d::Evidence {
                issue: 42,
                pr: 7,
                platform: track.0,
                channel: track.1,
                build: build.build.clone(),
                source: build.source.clone(),
                artifact: build.artifact.clone(),
                scenario: "Open Board and inspect the live card".into(),
                installed_at: None,
                accepted_by: None,
                accepted_at: None,
                note: "try.md".into(),
            }]),
            86500,
        );
    assert!(
        delivery.rows(scope, track.0, 86500, 30)[0].ready,
        "live PR link reaches the existing delivery gate"
    );
    assert!(
        !delivery.rows(scope, track.0, 86561, 30)[0].ready,
        "stale proof cannot promote a card"
    );
}

#[test]
fn real_http_client_revalidates_and_projects_close_merge_and_failure_without_manifest_edits() {
    use crate::testing::{FakeServer, Response};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let phase = Arc::new(AtomicUsize::new(0));
    let state = phase.clone();
    let server = FakeServer::start(move |request| {
        assert_eq!(request.method, "GET");
        assert!(
            request.header("authorization").is_none(),
            "Manager credentials must not reach Forgejo"
        );
        let phase = state.load(Ordering::SeqCst);
        if phase == 3 {
            return Response::empty(500);
        }
        let etag = format!("\"phase-{phase}\"");
        if request.header("if-none-match") == Some(etag.as_str()) {
            return Response::empty(304);
        }
        let value = match request
            .target
            .trim_start_matches("/api/v1/repos/example/project/")
        {
            "issues?state=open&type=issues&limit=50&page=1" => {
                if phase == 0 {
                    json!([issue(42, "open"), issue(43, "open")])
                } else {
                    json!([])
                }
            }
            "pulls?state=open&limit=50&page=1" => {
                if phase == 0 {
                    json!([pull(7, "open", "Fixes #42")])
                } else {
                    json!([])
                }
            }
            "pulls?state=closed&sort=recentupdate&limit=50&page=1" => {
                if phase == 2 {
                    json!([pull(7, "closed", "Fixes #42")])
                } else {
                    json!([])
                }
            }
            "issues/42" => issue(42, "closed"),
            "commits/abc123/status" => json!({"state":"pending"}),
            other => panic!("unexpected fixture request {other}"),
        };
        Response::json(200, value).with_header("etag", &etag)
    });
    let mut binding = binding();
    binding.issues.clear();
    let original = binding.clone();
    let repo = repository_url(&binding.repository).unwrap();
    let base = format!("{}/api/v1/repos/example/project", server.origin);
    let mut reader = Reader::default();
    let mut snapshot = Snapshot::new("project".into());
    for (cycle, at) in [(0, 86500), (0, 86510), (1, 86530), (2, 86560), (3, 86590)] {
        phase.store(cycle, Ordering::SeqCst);
        let ticket = snapshot.begin().unwrap();
        let result = reader
            .read_at(&binding, &repo, &base, at)
            .map_err(|e| format!("{e:#}"));
        snapshot.live_issues(&ticket, result, at);
        snapshot.threads(&ticket, Ok(Vec::new()), at);
        let projected = project(&binding, &snapshot.forgejo.value, at);
        match cycle {
            0 => {
                assert_eq!(
                    projected.issues.len(),
                    2,
                    "positive control: real HTTP discovery ran"
                );
                assert_eq!(
                    column(
                        &projected.issues[0],
                        Some(&snapshot.forgejo.value[&42]),
                        &[]
                    ),
                    Column::Checks
                );
                assert_eq!(
                    column(
                        &projected.issues[1],
                        Some(&snapshot.forgejo.value[&43]),
                        &[]
                    ),
                    Column::Queue
                );
            }
            1 => assert!(
                projected.issues.is_empty(),
                "closed Queue and Checks cards leave at next 30 s read"
            ),
            2 | 3 => {
                assert_eq!(projected.issues.len(), 1);
                assert_eq!(
                    projected.pull_url(projected.issues[0].pull_requests[0]),
                    format!("{}/pulls/7", binding.repository)
                );
            }
            _ => unreachable!(),
        }
    }
    assert_eq!(snapshot.forgejo.last_success, Some(86560));
    assert!(snapshot.forgejo.error.as_ref().unwrap().contains("500"));
    assert!(
        server
            .requests()
            .iter()
            .any(|r| r.header("if-none-match").is_some()),
        "positive control: production cache sends conditional headers"
    );
    assert_eq!(binding, original, "all discovery and links are derived");
}

#[test]
fn real_http_reader_honours_retry_after_and_never_follows_redirects() {
    use crate::testing::{FakeServer, Response};
    let server = FakeServer::start(|_| Response::empty(429).with_header("retry-after", "120"));
    let binding = binding();
    let repo = repository_url(&binding.repository).unwrap();
    let mut reader = Reader::default();
    let error = reader
        .read_at(&binding, &repo, &server.origin, 86500)
        .unwrap_err();
    assert!(error.to_string().contains("retry in 120 s"));
    let error = reader
        .read_at(&binding, &repo, &server.origin, 86530)
        .unwrap_err();
    assert!(error.to_string().contains("requested a retry"));
    assert_eq!(server.requests().len(), 1, "backoff must send no requests");

    let destination = FakeServer::start(|_| panic!("redirect destination must never be contacted"));
    let target = destination.origin.clone();
    let redirect =
        FakeServer::start(move |_| Response::empty(302).with_header("location", &target));
    assert!(reader
        .read_at(&binding, &repo, &redirect.origin, 86560)
        .is_err());
    assert_eq!(
        redirect.requests().len(),
        1,
        "a new origin does not inherit the old origin's backoff"
    );
    assert!(destination.requests().is_empty());
}

#[test]
fn real_http_partial_or_unprimed_conditional_responses_are_never_empty_successes() {
    use crate::testing::{FakeServer, Response};
    let binding = binding();
    let repo = repository_url(&binding.repository).unwrap();
    for response in [304, 206, 401] {
        let server = FakeServer::start(move |_| Response::empty(response));
        assert!(Reader::default()
            .read_at(&binding, &repo, &server.origin, 86500)
            .is_err());
        assert_eq!(server.requests().len(), 1);
    }
    let server = FakeServer::start(|_| {
        Response::json(200, json!([issue(42, "open")]))
            .with_header("link", "<https://fixture.invalid/page2>; rel=\"next\"")
    });
    let error = Reader::default()
        .read_at(&binding, &repo, &server.origin, 86500)
        .unwrap_err();
    assert!(error.to_string().contains("short page"));
}

#[test]
fn missing_or_duplicate_annotations_are_visible_and_never_pick_an_owner() {
    let mut binding = binding();
    binding.issues.truncate(1);
    let mut scope = binding.issues[0].clone();
    let number = scope.number;
    scope.executor = "Other owner".into();
    binding.issues.push(scope);
    let issues = [
        (
            number,
            serde_json::from_value(issue(number, "open")).unwrap(),
        ),
        (42, serde_json::from_value(issue(42, "open")).unwrap()),
    ]
    .into();
    let projected = project(&binding, &issues, 86500);
    assert!(mapping_problem(&binding, number)
        .unwrap()
        .contains("ambiguous"));
    assert!(mapping_problem(&binding, 42)
        .unwrap()
        .contains("No card annotation"));
    assert!(projected.issues.iter().all(|s| s.executor == "Unassigned"));
    binding.issues.pop();
    assert_eq!(mapping_problem(&binding, number), None);
    assert_eq!(
        project(&binding, &issues, 86500)
            .issues
            .iter()
            .find(|s| s.number == number)
            .unwrap()
            .executor,
        binding.issues[0].executor,
        "positive control: one annotation keeps the explicit owner"
    );
}

#[test]
fn waiting_reports_read_all_comments_in_existing_refresh_and_fail_incomplete_sources() {
    let binding = binding();
    let repo = repository_url(&binding.repository).unwrap();
    let mut report = issue(42, "open");
    report["created_at"] = json!("1970-01-02T00:00:00Z");
    report["labels"] = json!([{"name":"from-tester"}]);
    report["comments"] = json!(1);
    report["user"] = json!({"id":4,"login":"filing-bot"});
    for commenter in [4, 5] {
        let mut reads = 0;
        let issues = read_with(&binding, &repo, 86500, &mut |path| {
            Ok(match path {
                "issues?state=open&type=issues&limit=50&page=1" => json!([report.clone()]),
                "issues/42/comments?&limit=50&page=1" => {
                    reads += 1;
                    json!([{"user":{"id":commenter}}])
                }
                "pulls?state=open&limit=50&page=1"
                | "pulls?state=closed&sort=recentupdate&limit=50&page=1" => json!([]),
                unexpected => panic!("unexpected source {unexpected}"),
            })
        })
        .unwrap();
        assert_eq!(reads, 1, "positive control: comment reader actually ran");
        assert_eq!(waiting::new_report(&issues[0], 86500), commenter == 4);
    }
    let failure = read_with(&binding, &repo, 86500, &mut |path| {
        if path.starts_with("issues?state=open") {
            return Ok(json!([report.clone()]));
        }
        anyhow::bail!("comments unavailable")
    })
    .unwrap_err();
    assert!(failure.to_string().contains("comments unavailable"));
    let incomplete = read_with(&binding, &repo, 86500, &mut |path| {
        if path.starts_with("issues?state=open") {
            return Ok(json!([report.clone()]));
        }
        Ok(json!([]))
    })
    .unwrap_err();
    assert!(incomplete.to_string().contains("comments changed"));
}

#[test]
fn waiting_report_comment_missing_identity_is_not_a_successful_answer() {
    let binding = binding();
    let repo = repository_url(&binding.repository).unwrap();
    let error=read_with(&binding,&repo,86500,&mut |path| {
        if path.starts_with("issues?state=open") { return Ok(json!([{"number":42,"title":"Report","state":"open","labels":[{"name":"from-oleg"}],"created_at":"1970-01-02T00:00:00Z","comments":1,"user":{"id":4}}])); }
        Ok(json!([{"user":null}]))
    }).unwrap_err();
    assert!(error.to_string().contains("no author identity"));
}
