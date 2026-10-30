use super::*;
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
fn scope(number: u64) -> Scope {
    let mut scope = Binding::parse(include_str!("../../../tests/fixtures/board-legacy.yaml"))
        .unwrap()
        .issues
        .remove(0);
    scope.number = number;
    scope.pull_requests = vec![42];
    scope
}
fn feed(platform: Platform, channel: Channel) -> Feed {
    Feed {
        platform,
        channel,
        url: "https://updates.example/feed.json".into(),
        format: Format::Receipt,
        metadata: None,
        artifact_name: None,
        next: "Try the exact build".into(),
        blocker: None,
    }
}
fn build() -> Build {
    Build {
        platform: Platform::Linux,
        channel: Channel::Beta,
        build: "100".into(),
        source: A.into(),
        artifact: "https://updates.example/build100.AppImage".into(),
    }
}
fn fixture_evidence(issue: u64) -> Evidence {
    Evidence {
        issue,
        pr: 42,
        platform: Platform::Linux,
        channel: Channel::Beta,
        build: "100".into(),
        source: A.into(),
        artifact: build().artifact,
        scenario: "Open the project Board".into(),
        installed_at: Some("2026-10-30T10:00:00Z".into()),
        accepted_by: Some("Oleg".into()),
        accepted_at: Some("2026-10-30T10:05:00Z".into()),
        note: "qa/board.md".into(),
    }
}
fn snapshot() -> Snapshot {
    let mut s = Snapshot::default();
    s.accept(
        Update {
            config: Ok(Some(Config {
                version: 1,
                feeds: vec![feed(Platform::Linux, Channel::Beta)],
                evidence: vec!["qa/board.md".into()],
            })),
            feeds: vec![((Platform::Linux, Channel::Beta), Ok(Some(build())))],
            artifacts: vec![(build().artifact, Ok(Artifact { available: true }))],
            ci: vec![(
                42,
                Ok(Ci {
                    pr: 42,
                    source: A.into(),
                    state: "success".into(),
                    links: vec![],
                    inclusion: BTreeMap::from([(A.into(), Ok(true))]),
                }),
            )],
            evidence: vec![(
                "qa/board.md".into(),
                Ok(vec![fixture_evidence(1), fixture_evidence(2)]),
            )],
        },
        100,
    );
    s
}
#[test]
fn accepted_old_build_and_other_os_do_not_accept_a_new_build() {
    let mut s = snapshot();
    assert!(
        s.rows(&scope(1), Platform::Linux, 100, 30)[0].accepted,
        "positive control: exact accepted identity"
    );
    assert!(
        s.rows(&scope(1), Platform::Macos, 100, 30).is_empty(),
        "another OS does not grant Ready"
    );
    let mut newer = build();
    newer.build = "101".into();
    newer.source = B.into();
    s.feeds
        .get_mut(&(Platform::Linux, Channel::Beta))
        .unwrap()
        .accept(Ok(Some(newer)), 101);
    s.ci.get_mut(&42)
        .unwrap()
        .value
        .inclusion
        .insert(B.into(), Ok(true));
    let row = &s.rows(&scope(1), Platform::Linux, 101, 30)[0];
    assert!(!row.accepted && !row.ready);
    assert_eq!(row.state, "Artifact metadata; scenario/evidence missing");
}
#[test]
fn multi_issue_pr_keeps_each_exact_evidence_and_does_not_borrow_a_different_pr() {
    let mut s = snapshot();
    for number in [1, 2] {
        assert!(s.rows(&scope(number), Platform::Linux, 100, 30)[0].accepted);
    }
    assert!(!s.rows(&scope(3), Platform::Linux, 100, 30)[0].ready);
    let e = &mut s.evidence.get_mut("qa/board.md").unwrap().value[0];
    e.pr = 43;
    let mut card = scope(1);
    card.pull_requests.push(43);
    assert!(
        !s.rows(&card, Platform::Linux, 100, 30)[0].accepted,
        "PR42 proof cannot establish PR43 evidence"
    );
}
#[test]
fn failures_and_stale_sources_keep_their_own_snapshots_without_false_ready() {
    let mut s = snapshot();
    s.feeds
        .get_mut(&(Platform::Linux, Channel::Beta))
        .unwrap()
        .accept(Err("feed offline".into()), 110);
    s.ci.get_mut(&42).unwrap().last_success = Some(110);
    let row = &s.rows(&scope(1), Platform::Linux, 110, 30)[0];
    assert!(row.build.is_some());
    assert!(!row.ready);
    assert_eq!(row.state, "Feed unavailable or stale");
    assert_eq!(
        s.feeds[&(Platform::Linux, Channel::Beta)].last_success,
        Some(100)
    );
    assert!(s
        .problems(110, 30)
        .iter()
        .any(|p| p.contains("feed offline") && p.contains("10s ago")));
    let fresh = snapshot();
    assert!(
        !fresh.rows(&scope(1), Platform::Linux, 161, 30)[0].ready,
        "stale metadata isn't available delivery"
    );
    s = snapshot();
    s.ci.get_mut(&42).unwrap().value.state = "failure".into();
    let row = &s.rows(&scope(1), Platform::Linux, 100, 30)[0];
    assert!(!row.ready);
    assert!(
        row.accepted,
        "explicit human fact remains separate from failed CI"
    );
    assert!(row.state.contains("CI failed"));
    s.ci.get_mut(&42).unwrap().value.inclusion.clear();
    assert_eq!(
        s.rows(&scope(1), Platform::Linux, 100, 30)[0].state,
        "Source inclusion unknown"
    );
}
#[test]
fn installation_is_not_acceptance_and_conflicting_notes_are_visible() {
    let mut s = snapshot();
    let records = &mut s.evidence.get_mut("qa/board.md").unwrap().value;
    records[0].accepted_by = None;
    records[0].accepted_at = None;
    let row = &s.rows(&scope(1), Platform::Linux, 100, 30)[0];
    assert!(row.ready && !row.accepted);
    assert!(row.evidence.as_ref().unwrap().installed_at.is_some());
    s.config
        .value
        .as_mut()
        .unwrap()
        .evidence
        .push("qa/conflict.md".into());
    s.evidence.insert(
        "qa/conflict.md".into(),
        Source {
            value: vec![fixture_evidence(1)],
            last_success: Some(100),
            error: None,
        },
    );
    assert_eq!(
        s.rows(&scope(1), Platform::Linux, 100, 30)[0].state,
        "Conflicting acceptance evidence"
    );
}
#[test]
fn appcast_windows_and_appimage_use_exact_publisher_identity_not_publication_time() {
    let mut f = feed(Platform::Macos, Channel::Beta);
    f.format = Format::Appcast;
    let xml = format!(
        r#"<rss xmlns:s="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:o="https://git.oklabs.uk/BeFeast/okilum/update-metadata"><channel><item><s:version>9</s:version><s:channel>stable</s:channel><o:source>{B}</o:source><enclosure url="https://updates.example/stable.zip"/></item><item><s:version>8</s:version><s:channel>beta</s:channel><o:source>{A}</o:source><enclosure url="https://updates.example/beta.zip"/></item></channel></rss>"#
    );
    let mut unexpected = |_: &str| -> Result<Vec<u8>> { panic!("unexpected request") };
    let b = read::parse(&f, xml.as_bytes(), &mut unexpected).unwrap();
    assert_eq!(b.build, "8");
    assert_eq!(b.source, A);
    f = feed(Platform::Linux, Channel::Beta);
    f.format = Format::Appimage;
    let bytes=serde_json::to_vec(&serde_json::json!({"platform":"linux-appimage","channel":"beta","build":88,"source":A,"key":"linux/builds/88/a.AppImage"})).unwrap();
    let b = read::parse(&f, &bytes, &mut unexpected).unwrap();
    assert_eq!(b.source, A);
    assert_eq!(
        b.artifact,
        "https://updates.example/linux/builds/88/a.AppImage"
    );
    f = feed(Platform::Windows, Channel::Stable);
    f.format = Format::Velopack;
    f.metadata = Some("https://updates.example/builds/{build}/release.json".into());
    let asset = serde_json::json!({"Type":"Full","PackageId":"BeFeast.Okilum","Version":"0.1.77","FileName":"BeFeast.Okilum-0.1.77-beta-full.nupkg","SHA256":"a","Size":5});
    let bytes = serde_json::to_vec(&serde_json::json!({"Assets":[asset.clone()]})).unwrap();
    let mut calls = 0;
    let b=read::parse(&f,&bytes,&mut |url| {calls+=1;assert!(url.ends_with("/77/release.json"));Ok(serde_json::to_vec(&serde_json::json!({"build":77,"version":"0.1.77","source":B,"feed":{"Assets":[asset.clone()]}})).unwrap())}).unwrap();
    assert_eq!(calls, 1);
    assert_eq!(b.source, B);
    assert!(
        read::parse(&f, &bytes, &mut |_| Ok(br#"{"build":78}"#.to_vec())).is_err(),
        "conflicting metadata is not a publication"
    );
}
#[test]
fn source_inclusion_has_positive_negative_and_unavailable_ancestry_controls() {
    for total in [Some(0), Some(2), None] {
        let mut calls = Vec::new();
        let mut b = build();
        b.source = B.into();
        let ci=read::read_ci("https://git.example/api/v1/repos/p/r",42,&[b],&mut |url|{
            calls.push(url.to_owned());
            let value=if url.ends_with("/pulls/42") {serde_json::json!({"merged":true,"merge_commit_sha":A,"head":{"sha":A}})}
                else if url.contains("/commits/") {serde_json::json!({"state":"success","statuses":[{"target_url":"/p/r/actions/runs/10/jobs/1"}]})}
                else {assert!(url.ends_with(&format!("/compare/{B}...{A}")));match total {Some(n)=>serde_json::json!({"total_commits":n,"commits":[]}),None=>serde_json::json!({})}};
            Ok(serde_json::to_vec(&value).unwrap())
        }).unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            ci.links,
            vec!["https://git.example/p/r/actions/runs/10/jobs/1"]
        );
        match total {
            Some(0) => assert_eq!(ci.inclusion[B], Ok(true)),
            Some(_) => assert_eq!(ci.inclusion[B], Ok(false)),
            None => assert!(ci.inclusion[B].is_err()),
        }
    }
}

#[test]
fn actions_artifact_is_exact_to_run_platform_binding_and_expiry() {
    let mut f = feed(Platform::Linux, Channel::Sprint);
    f.format = Format::Actions;
    f.url = "https://git.example/api/v1/repos/p/r/actions/runs/99".into();
    f.artifact_name = Some("linux-exact".into());
    let run = serde_json::to_vec(&serde_json::json!({"id":99,"index_in_repo":123,"commit_sha":A}))
        .unwrap();
    for expired in [false, true] {
        let mut count = 0;
        let result = read::parse(&f, &run, &mut |url| {
            count += 1;
            assert!(url.ends_with("/runs/99/artifacts"));
            Ok(serde_json::to_vec(&serde_json::json!({"artifacts":[
                {"name":"mac-exact","expired":false,"size_in_bytes":20,"archive_download_url":"https://git.example/mac.zip"},
                {"name":"linux-exact","expired":expired,"size_in_bytes":20,"archive_download_url":"https://git.example/linux.zip","workflow_run":{"head_sha":A}}
            ]})).unwrap())
        });
        assert_eq!(count, 1);
        if expired {
            assert!(result.is_err());
        } else {
            let b = result.unwrap();
            assert_eq!(b.build, "run-123");
            assert_eq!(b.artifact, "https://git.example/linux.zip");
            assert_eq!(b.platform, Platform::Linux);
        }
    }
}
struct Area(std::path::PathBuf);
impl Area {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("okilum-delivery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("ops")).unwrap();
        Self(path)
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn explicit_note_frontmatter_missing_renamed_and_invalid_bindings_fail_without_writes() {
    let area = Area::new();
    assert_eq!(config(&area.0).unwrap(), None);
    let note = area.0.join("scenario.md");
    let content=format!("---\ndelivery:\n  - issue: 1\n    pr: 42\n    platform: linux\n    channel: beta\n    build: '100'\n    source: '{A}'\n    artifact: https://updates.example/build100.AppImage\n    scenario: Open the exact build\n    accepted_by: Oleg\n    accepted_at: '2026-10-30T12:00:00Z'\n---\nActual ordinary scenario and evidence.\n");
    std::fs::write(&note, &content).unwrap();
    let stamp = std::fs::metadata(&note).unwrap().modified().unwrap();
    let values = evidence(&area.0, "scenario.md").unwrap();
    assert!(values[0].accepted());
    assert_eq!(values[0].note, "scenario.md");
    assert_eq!(std::fs::read_to_string(&note).unwrap(), content);
    assert_eq!(std::fs::metadata(&note).unwrap().modified().unwrap(), stamp);
    std::fs::rename(&note, area.0.join("renamed.md")).unwrap();
    assert!(evidence(&area.0, "scenario.md").is_err());
    assert!(evidence(&area.0, "../renamed.md").is_err());
    let binding = area.0.join("ops/board-delivery.toml");
    std::fs::write(&binding,"version=1\nevidence=['renamed.md']\n[[feeds]]\nplatform='linux'\nchannel='beta'\nformat='appimage'\nurl='https://updates.example/latest.json'\n").unwrap();
    assert_eq!(config(&area.0).unwrap().unwrap().feeds.len(), 1);
    let text = std::fs::read_to_string(&binding).unwrap();
    std::fs::write(
        &binding,
        text.replace(
            "https://updates.example/latest.json",
            "https://user:secret@updates.example/latest.json",
        ),
    )
    .unwrap();
    assert!(config(&area.0).is_err());
}

#[test]
fn merging_a_pr_does_not_lose_its_premerge_sprint_source() {
    let mut b = build();
    b.source = B.into();
    let mut count = 0;
    let ci = read::read_ci(
        "https://git.example/api/v1/repos/p/r",
        42,
        &[b],
        &mut |url| {
            count += 1;
            let value = if url.ends_with("/pulls/42") {
                serde_json::json!({"merged":true,"merge_commit_sha":A,"head":{"sha":B}})
            } else {
                assert!(url.ends_with(&format!("/commits/{B}/status")));
                serde_json::json!({"state":"success","statuses":[]})
            };
            Ok(serde_json::to_vec(&value).unwrap())
        },
    )
    .unwrap();
    assert_eq!(count, 2);
    assert_eq!(ci.source, B);
    assert_eq!(ci.inclusion[B], Ok(true));
}

#[test]
fn rebinding_a_feed_does_not_present_the_previous_urls_snapshot_as_the_new_source() {
    let mut s = snapshot();
    let mut config = s.config.value.clone().unwrap();
    config.feeds[0].url = "https://new.example/channel.json".into();
    s.accept(
        Update {
            config: Ok(Some(config)),
            feeds: vec![(
                (Platform::Linux, Channel::Beta),
                Err("new feed offline".into()),
            )],
            artifacts: vec![],
            ci: vec![],
            evidence: vec![],
        },
        110,
    );
    let row = &s.rows(&scope(1), Platform::Linux, 110, 30)[0];
    assert!(row.build.is_none());
    assert!(!row.ready && !row.accepted);
    assert_eq!(
        s.feeds[&(Platform::Linux, Channel::Beta)].last_success,
        None
    );
}
