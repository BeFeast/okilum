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
