use super::*;
use crate::inbox::{CaptureRequest, SourceIdentity};
struct Fixture {
    dir: tempfile::TempDir,
    brain: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("brain/records")).unwrap();
        fs::create_dir(dir.path().join("state")).unwrap();
        Self {
            dir,
            brain: Uuid::new_v4().to_string(),
        }
    }
    fn config(&self) -> RunnerConfig {
        RunnerConfig {
            brain_id: self.brain.clone(),
            root: self.dir.path().join("brain"),
            records_dir: "records".into(),
            operational_dir: self.dir.path().join("state"),
            boundary: WriteBoundary::Managed,
        }
    }
    fn open(&self) -> Runner {
        Runner::open(self.config()).unwrap()
    }
    fn enrolled(&self) -> Runner {
        let mut r = self.open();
        r.enroll_proposal_feed(1).unwrap();
        r.enroll_proposal_drafts().unwrap();
        r
    }
}
fn identity() -> SourceIdentity {
    let id = Uuid::new_v4().to_string();
    SourceIdentity {
        channel: "native".into(),
        instance_id: Uuid::new_v4().to_string(),
        account_id: "local".into(),
        actor_id: "operator".into(),
        chat_id: None,
        topic_id: None,
        message_id: id.clone(),
        update_id: id,
        uri: None,
    }
}
fn capture(r: &mut Runner) -> String {
    let source = identity();
    let c = r
        .inbox_capture(
            CaptureRequest {
                operation_id: source.message_id.clone(),
                text: "Prepare the workshop visit".into(),
                source,
            },
            "operator",
        )
        .unwrap();
    r.drafts()
        .unwrap()
        .intents()
        .unwrap()
        .iter()
        .find(|(_, i)| i.trigger.identity.record_id == c.capture_id)
        .unwrap()
        .0
        .clone()
}
fn start(r: &mut Runner, id: &str, goal: Option<&str>) {
    let trigger = r
        .drafts()
        .unwrap()
        .get(&r.state.brain_id, goal, id)
        .unwrap()
        .trigger
        .clone();
    let input = api::CapturedInput {
        trigger_source: r.source.read(&trigger.source_path).unwrap(),
        citations: vec![],
        goal_revision: goal.map(|g| r.source.read(&r.path("goal", g)).unwrap().revision),
        omissions: vec!["Fixture has no additional citations".into()],
    };
    r.fixture_start_proposal(id, goal, input).unwrap();
}
fn result() -> Vec<u8> {
    serde_json::to_vec(&api::Generated {
        title: "Prepare visit kit".into(),
        criteria: vec!["Kit contains the appointment details".into()],
        rationale: "The captured thought needs preparation".into(),
        open_questions: vec!["Which date?".into()],
        citation_ids: vec![],
    })
    .unwrap()
}
fn get(r: &Runner, id: &str, goal: Option<&str>) -> api::Detail {
    r.proposal_get(api::Lookup {
        proposal_id: id.into(),
        goal_id: goal.map(str::to_string),
    })
    .unwrap()
}
fn request(
    r: &Runner,
    id: &str,
    goal: Option<&str>,
    disposition: api::Disposition,
) -> api::Request {
    let source = identity();
    api::Request {
        operation_id: source.message_id.clone(),
        proposal_id: id.into(),
        goal_id: goal.map(str::to_string),
        expected_revision: get(r, id, goal).source.revision,
        disposition,
        source,
    }
}
fn inventory(p: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(root: &std::path::Path, p: &std::path::Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(p).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out)
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    walk(p, p, &mut out);
    out
}
#[test]
fn pending_result_disposition_reopen_retains_exact_source_and_feed() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    let pending = get(&r, &id, None);
    assert!(pending.record.generated.is_none());
    assert_eq!(pending.record.attempt.state, api::AttemptState::Running);
    assert_eq!(pending.record.verification, "unverified");
    let before = inventory(&r.state_dir.join("proposal-feed-v1"));
    let state_before = fs::read(r.state_dir.join("proposal-intents-v1/journal.json")).unwrap();
    let _ = r
        .proposal_list(api::ListRequest {
            goal_id: None,
            limit: 10,
            cursor: None,
        })
        .unwrap();
    assert_eq!(
        state_before,
        fs::read(r.state_dir.join("proposal-intents-v1/journal.json")).unwrap()
    );
    // A core capture pumps the feed while the same attempt is running; no false restart.
    capture(&mut r);
    assert_eq!(
        get(&r, &id, None).record.attempt.state,
        api::AttemptState::Running
    );
    assert!(r.fixture_finish_proposal(&id, None, &result()).unwrap());
    let generated = get(&r, &id, None);
    assert!(generated.record.generated.is_some());
    assert!(generated.stale_reasons.is_empty());
    assert!(!r.fixture_finish_proposal(&id, None, &result()).unwrap());
    let req = request(
        &r,
        &id,
        None,
        api::Disposition::Snoozed {
            until: "2099-01-01T01:00:00Z".into(),
        },
    );
    let receipt = r.proposal_disposition(req.clone(), "operator").unwrap();
    assert!(!receipt.replayed);
    let final_doc = get(&r, &id, None);
    assert_eq!(final_doc.record.generated, generated.record.generated);
    assert_eq!(final_doc.record.history.len(), 1);
    let exact = fs::read(r.root.join(&receipt.path)).unwrap();
    drop(r);
    let mut r = f.open();
    assert_eq!(fs::read(r.root.join(&receipt.path)).unwrap(), exact);
    let replay = r.proposal_disposition(req.clone(), "operator").unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.revision, receipt.revision);
    for (path, bytes) in before {
        assert_eq!(
            fs::read(r.state_dir.join("proposal-feed-v1").join(path)).unwrap(),
            bytes
        );
    }
    // Later source drift cannot invalidate an exact committed receipt replay.
    fs::write(
        r.root.join(&final_doc.record.trigger.source_path),
        "external change",
    )
    .unwrap();
    assert!(
        r.proposal_disposition(req.clone(), "operator")
            .unwrap()
            .replayed
    );
    let mut changed = req;
    changed.disposition = api::Disposition::Rejected;
    assert!(r.proposal_disposition(changed, "operator").is_err());
    assert!(!get(&r, &id, None).stale_reasons.is_empty());
}
#[test]
fn reject_wins_late_completion_and_does_not_hold_running_slot() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    let req = request(&r, &id, None, api::Disposition::Rejected);
    r.proposal_disposition(req, "operator").unwrap();
    let before = inventory(&r.root);
    assert!(!r.fixture_finish_proposal(&id, None, &result()).unwrap());
    assert_eq!(inventory(&r.root), before);
    let other = capture(&mut r);
    start(&mut r, &other, None);
    assert!(r.fixture_finish_proposal(&other, None, &result()).unwrap());
    let d = get(&r, &id, None);
    assert_eq!(d.record.disposition, api::Disposition::Rejected);
    assert!(d.record.generated.is_none());
}
#[test]
fn lost_ack_and_projection_crashes_recover_original_disposition() {
    for fault in [
        Fault::BeforeProjection,
        Fault::AfterProjection,
        Fault::AfterReceipt,
    ] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = capture(&mut r);
        start(&mut r, &id, None);
        r.fixture_finish_proposal(&id, None, &result()).unwrap();
        let req = request(&r, &id, None, api::Disposition::Rejected);
        r.proposal_draft_fault = Some(fault);
        assert!(r.proposal_disposition(req.clone(), "operator").is_err());
        let projected = r
            .drafts()
            .unwrap()
            .draft(&r.state.brain_id, None, &id)
            .unwrap()
            .projections
            .last()
            .unwrap()
            .write
            .clone();
        drop(r);
        let mut r = f.open();
        let receipt = r.proposal_disposition(req, "operator").unwrap();
        assert!(receipt.replayed);
        assert_eq!(
            fs::read(r.root.join(&projected.path)).unwrap(),
            STANDARD.decode(&projected.content_base64).unwrap()
        );
        let recovery = r.source.recovery_record(&projected.operation_id).unwrap();
        assert_eq!(recovery.request, projected);
        assert_eq!(recovery.receipt.unwrap().revision, receipt.revision);
        assert_eq!(get(&r, &id, None).record.history.len(), 1);
    }
}
#[test]
fn result_projection_crashes_never_expose_uncommitted_generation() {
    for fault in [
        Fault::BeforeProjection,
        Fault::AfterProjection,
        Fault::AfterReceipt,
    ] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = capture(&mut r);
        start(&mut r, &id, None);
        r.proposal_draft_fault = Some(fault);
        assert!(r.fixture_finish_proposal(&id, None, &result()).is_err());
        let d = get(&r, &id, None);
        assert_eq!(d.record.generated.is_some(), fault == Fault::AfterReceipt);
        let original = r
            .drafts()
            .unwrap()
            .draft(&r.state.brain_id, None, &id)
            .unwrap()
            .projections
            .last()
            .unwrap()
            .write
            .clone();
        drop(r);
        let r = f.open();
        let d = get(&r, &id, None);
        assert!(d.record.generated.is_some());
        assert!(!d.projection_pending);
        assert_eq!(d.source.content_base64, original.content_base64);
    }
}
#[test]
fn initial_projection_crash_is_create_only_and_restart_is_interrupted() {
    for fault in [
        Fault::BeforeProjection,
        Fault::AfterProjection,
        Fault::AfterReceipt,
    ] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = capture(&mut r);
        let trigger = r
            .drafts()
            .unwrap()
            .get(&r.state.brain_id, None, &id)
            .unwrap()
            .trigger
            .clone();
        let input = api::CapturedInput {
            trigger_source: r.source.read(&trigger.source_path).unwrap(),
            citations: vec![],
            goal_revision: None,
            omissions: vec![],
        };
        r.proposal_draft_fault = Some(fault);
        assert!(r.fixture_start_proposal(&id, None, input).is_err());
        let first = r
            .drafts()
            .unwrap()
            .draft(&r.state.brain_id, None, &id)
            .unwrap()
            .projections[0]
            .write
            .clone();
        assert!(first.expected_revision.is_none());
        drop(r);
        let mut r = f.open();
        let d = get(&r, &id, None);
        assert_eq!(d.record.attempt.state, api::AttemptState::Interrupted);
        assert!(d.record.generated.is_none());
        assert_eq!(
            r.source
                .recovery_record(&first.operation_id)
                .unwrap()
                .request,
            first
        );
        assert!(r.fixture_finish_proposal(&id, None, &result()).is_err());
    }
}
#[test]
fn invalid_response_is_sanitized_and_stale_source_is_explicit() {
    for output in [
        b"RAW_SECRET_provider_failure".to_vec(),
        br#"{"title":"bad","secret":"RAW_SECRET"}"#.to_vec(),
        vec![b'x'; 32769],
    ] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = capture(&mut r);
        start(&mut r, &id, None);
        r.fixture_finish_proposal(&id, None, &output).unwrap();
        let d = get(&r, &id, None);
        assert_eq!(d.record.failure, Some(api::Failure::MalformedOutput));
        assert!(d.record.generated.is_none());
        assert!(
            !String::from_utf8_lossy(&STANDARD.decode(d.source.content_base64).unwrap())
                .contains("RAW_SECRET")
        );
    }
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    let d = get(&r, &id, None);
    fs::write(r.root.join(&d.record.trigger.source_path), "Changed").unwrap();
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let d = get(&r, &id, None);
    assert_eq!(d.record.failure, Some(api::Failure::SourceChanged));
    assert!(!d.stale_reasons.is_empty());
    let req = request(&r, &id, None, api::Disposition::Rejected);
    assert!(r.proposal_disposition(req, "operator").is_err());
}
#[test]
fn proposal_operation_namespace_is_shared_bidirectionally() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let req = request(&r, &id, None, api::Disposition::Rejected);
    r.proposal_disposition(req.clone(), "operator").unwrap();
    let capture_req = CaptureRequest {
        operation_id: req.operation_id,
        text: "must not capture".into(),
        source: req.source,
    };
    assert!(r.inbox_capture(capture_req, "operator").is_err());
    let id = capture(&mut r);
    start(&mut r, &id, None);
    let source = identity();
    let captured = r
        .inbox_capture(
            CaptureRequest {
                operation_id: source.message_id.clone(),
                text: "positive capture".into(),
                source: source.clone(),
            },
            "operator",
        )
        .unwrap();
    let mut req = request(&r, &id, None, api::Disposition::Rejected);
    req.operation_id = captured.receipt.operation_id;
    req.source = source;
    assert!(r.proposal_disposition(req, "operator").is_err());
}
#[test]
fn capability_fence_refuses_a2_and_existing_upgraded_handle_before_writes() {
    let f = Fixture::new();
    let mut r = f.open();
    r.enroll_proposal_feed(1).unwrap();
    let source_dir = r.state_dir.join("source");
    let old = SourceStore::open_with_proposal_feed(
        &f.brain,
        &r.root,
        &source_dir,
        WriteBoundary::Managed,
    )
    .unwrap();
    let write = SourceWrite {
        schema: SCHEMA.into(),
        operation_id: Uuid::new_v4().to_string(),
        brain_id: f.brain.clone(),
        path: "positive.md".into(),
        expected_revision: None,
        content_base64: STANDARD.encode("positive control"),
    };
    old.write(write.clone()).unwrap();
    r.enroll_proposal_drafts().unwrap();
    let before = inventory(&f.dir.path().join("state"));
    assert!(SourceStore::open_with_proposal_feed(
        &f.brain,
        &r.root,
        &source_dir,
        WriteBoundary::Managed
    )
    .is_err());
    let mut new = write;
    new.operation_id = Uuid::new_v4().to_string();
    new.path = "must-not-write.md".into();
    assert!(old.write(new).is_err());
    assert_eq!(inventory(&f.dir.path().join("state")), before);
    assert!(!r.root.join("must-not-write.md").exists());
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let readonly = SourceStore::open_read_only(&f.brain, &r.root, &source_dir).unwrap();
    assert!(readonly.read(&get(&r, &id, None).source.path).is_ok());
}
fn goal(r: &mut Runner) -> String {
    let id = Uuid::new_v4().to_string();
    r.create_goal(
        Goal {
            id: id.clone(),
            title: "Prepare workshop visit".into(),
            status: "draft".into(),
            criteria: vec![Criterion {
                id: "C1".into(),
                description: "Observed preparation".into(),
                requires_human: true,
            }],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        },
        "# Workshop visit".into(),
    )
    .unwrap();
    id
}
fn decision(r: &mut Runner, goal: &str) -> String {
    r.with_goal(goal, |r| {
        r.attention("decision", "Choose date");
        r.persist()
    })
    .unwrap();
    let app = crate::application::Application::unconfigured();
    let items = app.attention_items(r).unwrap();
    let list = r
        .attention_list(items, "operator", None, None, None)
        .unwrap();
    let item = list.items.iter().find(|i| i.goal_id == goal).unwrap();
    let source = identity();
    let request = crate::attention::Mutation {
        operation_id: source.message_id.clone(),
        source,
        target: crate::attention::Target {
            goal_id: goal.into(),
            attention_id: item.attention_id.clone(),
            expected_revision: item.revision.clone(),
            stage_id: item.stage_id.clone(),
        },
    };
    let outcome = r
        .attention_mutate(
            request,
            "save_decision",
            Some("Tuesday is suitable"),
            "operator",
            |r| app.attention_items(r),
        )
        .unwrap();
    r.drafts()
        .unwrap()
        .intents()
        .unwrap()
        .iter()
        .find(|(_, i)| Some(&i.trigger.source_path) == outcome.path.as_ref())
        .unwrap()
        .0
        .clone()
}
#[test]
fn two_similar_goals_and_unplanned_inbox_retain_owners_citations_and_selection() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let a = goal(&mut r);
    let b = goal(&mut r);
    let ia = decision(&mut r, &a);
    let ib = decision(&mut r, &b);
    let inbox = capture(&mut r);
    let selection = r.state.goal_id.clone();
    for (id, owner) in [
        (&ia, Some(a.as_str())),
        (&ib, Some(b.as_str())),
        (&inbox, None),
    ] {
        start(&mut r, id, owner);
        r.fixture_finish_proposal(id, owner, &result()).unwrap();
        let page = r
            .proposal_list(api::ListRequest {
                goal_id: owner.map(str::to_string),
                limit: 10,
                cursor: None,
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(&page.items[0].record.id, id);
    }
    assert_eq!(r.state.goal_id, selection);
    assert!(r
        .proposal_get(api::Lookup {
            proposal_id: ia.clone(),
            goal_id: Some(b.clone())
        })
        .is_err());
    let mut req = request(&r, &ia, Some(&a), api::Disposition::Rejected);
    req.goal_id = Some(b);
    assert!(r.proposal_disposition(req, "operator").is_err());
    let trigger = r
        .drafts()
        .unwrap()
        .get(&r.state.brain_id, Some(&a), &ia)
        .unwrap()
        .trigger
        .clone();
    let snapshot = r.source.read(&trigger.source_path).unwrap();
    let text = String::from_utf8(STANDARD.decode(&snapshot.content_base64).unwrap()).unwrap();
    let mut citation = crate::retrieval::Citation {
        citation_id: "forged".into(),
        path: snapshot.path.clone(),
        revision: snapshot.revision.clone(),
        start_line: 1,
        end_line: 1,
        locator: "L1-L1".into(),
        excerpt: text.lines().next().unwrap().to_string(),
        metadata: Default::default(),
    };
    let input = api::CapturedInput {
        trigger_source: snapshot,
        citations: vec![citation.clone()],
        goal_revision: Some(r.source.read(&r.path("goal", &a)).unwrap().revision),
        omissions: vec![],
    };
    assert!(r.validate_proposal_input(&trigger, &input).is_err());
    // A real retrieval citation is the positive control for shape/ownership checks.
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let start = lines
        .iter()
        .rposition(|line| line.contains("Tuesday is suitable"))
        .unwrap()
        + 1;
    citation = crate::retrieval::Citation {
        citation_id: crate::retrieval::citation_id(
            &input.trigger_source.path,
            &input.trigger_source.revision,
            start,
            start,
        ),
        path: input.trigger_source.path.clone(),
        revision: input.trigger_source.revision.clone(),
        start_line: start,
        end_line: start,
        locator: format!("L{start}-L{start}"),
        excerpt: lines[start - 1].into(),
        metadata: crate::retrieval::source_metadata(&text),
    };
    let mut input = input;
    input.citations = vec![citation];
    r.validate_proposal_input(&trigger, &input).unwrap();
    let mut generated: api::Generated = serde_json::from_slice(&result()).unwrap();
    generated.citation_ids = vec![input.citations[0].citation_id.clone()];
    generated.validate(&input).unwrap();
    generated.citation_ids[0] = "unknown".into();
    assert!(generated.validate(&input).is_err());
}
#[test]
fn proposal_changed_on_disk_blocks_new_disposition_but_keeps_both_versions() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let req = request(&r, &id, None, api::Disposition::Rejected);
    let d = get(&r, &id, None);
    fs::write(r.root.join(&d.source.path), "External proposal revision").unwrap();
    assert!(r.proposal_disposition(req, "operator").is_err());
    let detail = get(&r, &id, None);
    assert_eq!(detail.source, d.source);
    assert_ne!(detail.current_revision, Some(d.source.revision));
    assert!(!detail.stale_reasons.is_empty());
    assert_eq!(
        fs::read(r.root.join(&d.source.path)).unwrap(),
        b"External proposal revision"
    );
}
#[test]
fn canonical_citations_export_losslessly_and_recovery_does_not_refresh_them() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let goal = goal(&mut r);
    let id = decision(&mut r, &goal);
    let trigger = r
        .drafts()
        .unwrap()
        .get(&r.state.brain_id, Some(&goal), &id)
        .unwrap()
        .trigger
        .clone();
    let source = r.source.read(&trigger.source_path).unwrap();
    let text = String::from_utf8(STANDARD.decode(&source.content_base64).unwrap()).unwrap();
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let n = lines
        .iter()
        .rposition(|l| l.contains("Tuesday is suitable"))
        .unwrap()
        + 1;
    let citation = crate::retrieval::Citation {
        citation_id: crate::retrieval::citation_id(&source.path, &source.revision, n, n),
        path: source.path.clone(),
        revision: source.revision.clone(),
        start_line: n,
        end_line: n,
        locator: format!("L{n}-L{n}"),
        excerpt: lines[n - 1].into(),
        metadata: crate::retrieval::source_metadata(&text),
    };
    let captured = api::CapturedInput {
        trigger_source: source,
        citations: vec![citation.clone()],
        goal_revision: Some(r.source.read(&r.path("goal", &goal)).unwrap().revision),
        omissions: vec!["Missing date confirmation".into()],
    };
    r.fixture_start_proposal(&id, Some(&goal), captured)
        .unwrap();
    let mut generated: api::Generated = serde_json::from_slice(&result()).unwrap();
    generated.citation_ids = vec![citation.citation_id.clone()];
    generated.rationale =
        "An embedded delimiter must remain source text\n---\nStill rationale".into();
    r.fixture_finish_proposal(&id, Some(&goal), &serde_json::to_vec(&generated).unwrap())
        .unwrap();
    let doc = get(&r, &id, Some(&goal));
    assert_eq!(doc.record.generated, Some(generated));
    assert_eq!(doc.record.captured.citations, vec![citation.clone()]);
    let archive = f.dir.path().join("exact.tar");
    r.source.export_exact(&archive).unwrap();
    let restored = f.dir.path().join("restored");
    fs::create_dir(&restored).unwrap();
    tar::Archive::new(fs::File::open(&archive).unwrap())
        .unpack(&restored)
        .unwrap();
    assert_eq!(
        fs::read(restored.join("brain").join(&doc.source.path)).unwrap(),
        STANDARD.decode(&doc.source.content_base64).unwrap()
    );
    fs::write(
        r.root.join(&citation.path),
        format!("{text}\nNew decision\n"),
    )
    .unwrap();
    drop(r);
    let r = f.open();
    let stale = get(&r, &id, Some(&goal));
    assert!(!stale.stale_reasons.is_empty());
    assert_eq!(stale.source, doc.source);
    assert_eq!(stale.record.captured.citations, vec![citation]);
}
#[test]
#[ignore = "requires empty TESSERA_DRAFT_COMPAT_FIXTURE directory"]
fn export_proposal_draft_compatibility_fixture() {
    let target =
        std::path::PathBuf::from(std::env::var_os("TESSERA_DRAFT_COMPAT_FIXTURE").unwrap());
    assert!(target.is_dir() && fs::read_dir(&target).unwrap().next().is_none());
    fs::create_dir_all(target.join("brain/records")).unwrap();
    fs::create_dir(target.join("state")).unwrap();
    let brain = Uuid::new_v4().to_string();
    let mut r = Runner::open(RunnerConfig {
        brain_id: brain.clone(),
        root: target.join("brain"),
        records_dir: "records".into(),
        operational_dir: target.join("state"),
        boundary: WriteBoundary::Managed,
    })
    .unwrap();
    r.enroll_proposal_feed(1).unwrap();
    r.enroll_proposal_drafts().unwrap();
    let a = goal(&mut r);
    let b = goal(&mut r);
    let pa = decision(&mut r, &a);
    let pb = decision(&mut r, &b);
    let inbox = capture(&mut r);
    for (id, owner) in [
        (&pa, Some(a.as_str())),
        (&pb, Some(b.as_str())),
        (&inbox, None),
    ] {
        start(&mut r, id, owner);
        r.fixture_finish_proposal(id, owner, &result()).unwrap();
    }
    let mut req = request(&r, &pa, Some(&a), api::Disposition::Rejected);
    req.source.actor_id = "local operator".into();
    r.proposal_draft_fault = Some(Fault::AfterProjection);
    assert!(r
        .proposal_disposition(req.clone(), "local operator")
        .is_err());
    let meta = serde_json::json!({"brain_id":brain,"goal_a":a,"goal_b":b,"proposal_a":pa,"proposal_b":pb,"inbox_proposal":inbox,"pending_request":req,"pending_write":r.drafts().unwrap().pending_projection().unwrap().unwrap().1});
    fs::write(
        target.join("fixture.json"),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .unwrap();
}
#[test]
fn actual_committed_engine_result_generates_without_changing_execution() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let goal = goal(&mut r);
    let stage = Uuid::new_v4().to_string();
    let context = Uuid::new_v4().to_string();
    let operation = Uuid::new_v4().to_string();
    r.prepare_stage(
        Stage {
            id: stage.clone(),
            goal_id: goal.clone(),
            engine: "t3".into(),
            status: "ready".into(),
            criterion_ids: vec!["C1".into()],
            context_id: context.clone(),
            result_ids: vec![],
            extra: BTreeMap::new(),
        },
        ContextPacket {
            id: context,
            goal_id: goal.clone(),
            stage_id: stage,
            goal_revision: r.goal_source().unwrap().revision,
            goal: "Prepare workshop visit".into(),
            decisions: vec![],
            constraints: vec![],
            sources: vec![],
            previous_result_id: None,
            next_step: "Fixture stage".into(),
            extra: BTreeMap::new(),
        },
        operation.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let engine_ref = EngineRef {
        engine: "t3".into(),
        instance_id: "fixture".into(),
        thread_id: Some("fixture-thread".into()),
        turn_id: Some("fixture-turn".into()),
        task_id: None,
    };
    r.apply_start(StartReply::Accepted {
        binding: engine_ref.clone(),
    })
    .unwrap();
    let event = EngineEvent {
        operation_id: operation,
        engine_ref,
        event_id: "result".into(),
        stream_id: "fixture-thread/fixture-turn".into(),
        sequence: Some(1),
        cursor: Some("1".into()),
        observed_at: "2026-09-07T10:00:00Z".into(),
        payload: EventPayload::Outcome(Outcome {
            outcome: "succeeded".into(),
            summary: "Kit prepared; human check remains".into(),
            sources: vec![],
            evidence: vec![],
            verification: "unverified".into(),
            criterion_evaluations: vec![],
        }),
    };
    r.ingest(event.clone()).unwrap();
    let id = r
        .drafts()
        .unwrap()
        .intents()
        .unwrap()
        .iter()
        .find(|(_, i)| i.trigger.identity.kind == crate::proposals::TriggerKind::Result)
        .unwrap()
        .0
        .clone();
    let before = inventory(&r.root);
    let state_before = serde_json::to_value(&r.state).unwrap();
    start(&mut r, &id, Some(&goal));
    r.fixture_finish_proposal(&id, Some(&goal), &result())
        .unwrap();
    let d = get(&r, &id, Some(&goal));
    assert_eq!(
        d.record.trigger.identity.kind,
        crate::proposals::TriggerKind::Result
    );
    assert_eq!(d.record.goal_id, Some(goal));
    assert!(d.record.generated.is_some());
    for (path, bytes) in before {
        assert_eq!(fs::read(r.root.join(path)).unwrap(), bytes);
    }
    assert_eq!(serde_json::to_value(&r.state).unwrap(), state_before);
    let count = r.drafts().unwrap().intents().unwrap().len();
    r.ingest(event).unwrap();
    assert_eq!(r.drafts().unwrap().intents().unwrap().len(), count);
}
#[test]
fn pending_projection_conflict_is_local_to_proposal_and_does_not_disable_core() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let other = capture(&mut r);
    start(&mut r, &other, None);
    r.fixture_finish_proposal(&other, None, &result()).unwrap();
    let req = request(&r, &id, None, api::Disposition::Rejected);
    r.proposal_draft_fault = Some(Fault::BeforeProjection);
    assert!(r.proposal_disposition(req.clone(), "operator").is_err());
    let pending = r.drafts().unwrap().pending_projection().unwrap().unwrap().1;
    let d = get(&r, &id, None);
    let changed = format!(
        "{}\nExternal note\n",
        String::from_utf8(STANDARD.decode(&d.source.content_base64).unwrap()).unwrap()
    );
    fs::write(r.root.join(&d.source.path), changed.as_bytes()).unwrap();
    drop(r);
    let mut r = f.open();
    assert!(r.proposal_draft_issue.is_some());
    let d = get(&r, &id, None);
    assert!(d.projection_pending && !d.stale_reasons.is_empty());
    assert!(r.proposal_disposition(req, "operator").is_err());
    assert_eq!(
        fs::read(r.root.join(&d.source.path)).unwrap(),
        changed.as_bytes()
    );
    assert_eq!(
        r.drafts().unwrap().pending_projection().unwrap().unwrap().1,
        pending
    );
    capture(&mut r);
    let req = request(&r, &other, None, api::Disposition::Rejected);
    assert!(r.proposal_disposition(req, "operator").is_ok());
}
#[test]
fn optional_feed_commit_failure_reloads_without_interrupting_or_blocking_core() {
    for after in [false, true] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let previous = capture(&mut r);
        start(&mut r, &previous, None);
        r.fixture_finish_proposal(&previous, None, &result())
            .unwrap();
        let reserved = request(&r, &previous, None, api::Disposition::Rejected);
        r.proposal_disposition(reserved.clone(), "operator")
            .unwrap();
        let id = capture(&mut r);
        start(&mut r, &id, None);
        r.proposal_store
            .as_mut()
            .unwrap()
            .inject_commit_failure(after);
        let source = identity();
        assert_eq!(
            r.inbox_capture(
                CaptureRequest {
                    operation_id: source.message_id.clone(),
                    source,
                    text: "Survives optional feed failure".into()
                },
                "operator"
            )
            .unwrap()
            .receipt
            .status,
            "committed"
        );
        assert_eq!(
            get(&r, &id, None).record.attempt.state,
            api::AttemptState::Running
        );
        assert!(r
            .inbox_capture(
                CaptureRequest {
                    operation_id: reserved.operation_id.clone(),
                    source: reserved.source.clone(),
                    text: "Must remain reserved".into()
                },
                "operator"
            )
            .is_err());
        capture(&mut r);
        assert_eq!(
            get(&r, &id, None).record.attempt.state,
            api::AttemptState::Running
        );
        assert!(r.fixture_finish_proposal(&id, None, &result()).unwrap());
    }
}
#[test]
fn altered_disposition_receipt_refuses_before_replay_or_source_mutation() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let req = request(&r, &id, None, api::Disposition::Rejected);
    r.proposal_disposition(req.clone(), "operator").unwrap();
    drop(r);
    let mut r = f.open();
    assert!(
        r.proposal_disposition(req.clone(), "operator")
            .unwrap()
            .replayed
    );
    drop(r);
    let path = f.dir.path().join("state/proposal-intents-v1/journal.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["intents"][&id]["draft"]["operations"][&req.operation_id]["receipt"]["actor"] =
        "Someone else".into();
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let before = inventory(&f.dir.path().join("brain"));
    let journal = fs::read(&path).unwrap();
    assert!(Runner::open(f.config()).is_err());
    assert_eq!(inventory(&f.dir.path().join("brain")), before);
    assert_eq!(fs::read(path).unwrap(), journal);
}
#[test]
fn inbox_and_goal_proposals_allow_index_rebuild_but_never_become_evidence() {
    use crate::retrieval::{BrainIndex, SearchRequest, SearchScope};
    let f = Fixture::new();
    let mut r = f.enrolled();
    let owner = goal(&mut r);
    let inbox = capture(&mut r);
    let bound = decision(&mut r, &owner);
    for (id, goal) in [(&inbox, None), (&bound, Some(owner.as_str()))] {
        start(&mut r, id, goal);
        r.fixture_finish_proposal(id, goal, &result()).unwrap();
        let d = get(&r, id, goal);
        let text = String::from_utf8(STANDARD.decode(&d.source.content_base64).unwrap()).unwrap();
        assert_eq!(
            crate::retrieval::source_owner(&text, &d.source.path, "records", &f.brain).unwrap(),
            goal.map(str::to_string)
        );
        assert!(!crate::retrieval::record_visible(
            &crate::retrieval::source_metadata(&text),
            &owner,
            "project"
        ));
        let lines = text.split_inclusive('\n').collect::<Vec<_>>();
        let n = lines
            .iter()
            .rposition(|line| line.starts_with("# "))
            .unwrap()
            + 1;
        let citation = crate::retrieval::Citation {
            citation_id: crate::retrieval::citation_id(&d.source.path, &d.source.revision, n, n),
            path: d.source.path.clone(),
            revision: d.source.revision.clone(),
            start_line: n,
            end_line: n,
            locator: format!("L{n}-L{n}"),
            excerpt: lines[n - 1].into(),
            metadata: crate::retrieval::source_metadata(&text),
        };
        assert!(
            crate::retrieval::validate_citation(&d.source, &citation, &owner, "project", "records")
                .unwrap_err()
                .to_string()
                .contains("outside the selected knowledge scope"),
            "exact proposal excerpt must not bypass retrieval exclusion"
        );
    }
    r.source
        .write(SourceWrite {
            schema: SCHEMA.into(),
            operation_id: Uuid::new_v4().to_string(),
            brain_id: f.brain.clone(),
            path: "knowledge.md".into(),
            expected_revision: None,
            content_base64: STANDARD.encode(
                "# Prepare visit kit\nPrepare visit kit is ordinary knowledge positive control.\n",
            ),
        })
        .unwrap();
    let index = BrainIndex::start(
        f.brain.clone(),
        r.root.clone(),
        "records".into(),
        r.state_dir.clone(),
        true,
    )
    .unwrap();
    for rebuild in [false, true] {
        let before = index.status().generation;
        if rebuild {
            index.rebuild().unwrap();
        }
        for _ in 0..300 {
            let status = index.status();
            if status.status == "ready" && (!rebuild || status.generation != before) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(index.status().status, "ready");
        if rebuild {
            assert_ne!(index.status().generation, before);
        }
        let found = index
            .search(SearchRequest {
                query: "Prepare visit kit".into(),
                scope: SearchScope {
                    goal_id: owner.clone(),
                    mode: "project".into(),
                    path_prefix: None,
                    include_paths: vec![],
                    exclude_paths: vec![],
                },
                mode: "lexical".into(),
                limit: 20,
                max_excerpt_bytes: 4096,
            })
            .unwrap();
        let bytes = serde_json::to_string(&found).unwrap();
        assert!(bytes.contains("knowledge.md"));
        assert!(!bytes.contains("proposal-"));
    }
}

fn expired_request(r: &Runner, id: &str) -> api::Request {
    request(
        r,
        id,
        None,
        api::Disposition::Snoozed {
            until: "2020-01-01T00:00:00Z".into(),
        },
    )
}
fn terminal(outcome: api::DeliveryOutcome) -> api::TerminalReceipt {
    let api::DeliveryOutcome::NotApplied(r) = outcome else {
        panic!("expected terminal receipt")
    };
    *r
}
#[test]
fn expired_disposition_terminal_survives_restart_and_reserves_exact_identity() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let req = expired_request(&r, &id);
    let before = get(&r, &id, None).source;
    let receipt = terminal(
        r.proposal_disposition_outcome(req.clone(), "operator")
            .unwrap(),
    );
    assert!(!receipt.replayed);
    assert_eq!(receipt.request, req);
    assert_eq!(before, get(&r, &id, None).source);
    assert!(r.source.required_proposal_terminal());
    drop(r);
    let mut r = f.open();
    let replay = terminal(
        r.proposal_disposition_outcome(req.clone(), "operator")
            .unwrap(),
    );
    assert!(replay.replayed);
    assert_eq!(replay.at, receipt.at);
    let mut changed = req.clone();
    changed.disposition = api::Disposition::Rejected;
    assert!(r.proposal_disposition_outcome(changed, "operator").is_err());
    let mut changed = req.clone();
    changed.operation_id = Uuid::new_v4().to_string();
    assert!(r.proposal_disposition_outcome(changed, "operator").is_err());
    assert!(r.proposal_disposition(req.clone(), "operator").is_err());
    assert!(r
        .proposal_identity_reserved(&req.operation_id, &req.source.external_key().unwrap())
        .unwrap());
    let new = request(&r, &id, None, api::Disposition::Rejected);
    assert!(matches!(
        r.proposal_disposition_outcome(new, "operator").unwrap(),
        api::DeliveryOutcome::Committed(_)
    ));
    assert_eq!(get(&r, &id, None).record.history.len(), 1);
    assert!(matches!(
        r.proposal_disposition_outcome(req, "operator").unwrap(),
        api::DeliveryOutcome::NotApplied(_)
    ));
}
#[test]
fn terminal_expiry_keeps_stale_missing_wrong_owner_and_wrong_revision_uncertain() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let req = expired_request(&r, &id);
    let path = r
        .root
        .join(get(&r, &id, None).record.captured.trigger_source.path);
    let original = fs::read(&path).unwrap();
    fs::write(&path, b"changed").unwrap();
    assert!(r
        .proposal_disposition_outcome(req.clone(), "operator")
        .is_err());
    fs::remove_file(&path).unwrap();
    assert!(r
        .proposal_disposition_outcome(req.clone(), "operator")
        .is_err());
    fs::write(&path, original).unwrap();
    let mut wrong = req.clone();
    wrong.goal_id = Some(Uuid::new_v4().to_string());
    assert!(r.proposal_disposition_outcome(wrong, "operator").is_err());
    let mut wrong = req.clone();
    wrong.expected_revision = format!("sha256:{}", "f".repeat(64));
    assert!(r.proposal_disposition_outcome(wrong, "operator").is_err());
    assert!(!r.source.required_proposal_terminal());
    assert!(matches!(
        r.proposal_disposition_outcome(req, "operator").unwrap(),
        api::DeliveryOutcome::NotApplied(_)
    ));
}
#[test]
fn terminal_publication_failures_never_acknowledge_and_reopen_reconciles() {
    for after in [false, true] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = capture(&mut r);
        start(&mut r, &id, None);
        r.fixture_finish_proposal(&id, None, &result()).unwrap();
        let req = expired_request(&r, &id);
        let before = get(&r, &id, None).source;
        r.proposal_store
            .as_mut()
            .unwrap()
            .inject_commit_failure(after);
        assert!(r
            .proposal_disposition_outcome(req.clone(), "operator")
            .is_err());
        assert!(r
            .proposal_disposition_outcome(req.clone(), "operator")
            .is_err());
        drop(r);
        let mut r = f.open();
        let receipt = terminal(r.proposal_disposition_outcome(req, "operator").unwrap());
        assert_eq!(receipt.replayed, after);
        assert_eq!(before, get(&r, &id, None).source);
    }
}
#[test]
fn terminal_fence_survives_empty_upgrade_and_missing_fence_refuses_open() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    r.source.require_proposal_terminal().unwrap();
    drop(r);
    let mut r = f.open();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let req = expired_request(&r, &id);
    terminal(r.proposal_disposition_outcome(req, "operator").unwrap());
    drop(r);
    let path = f.dir.path().join("state/source/binding.json");
    let mut binding: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    binding
        .as_object_mut()
        .unwrap()
        .remove("required_proposal_terminal");
    fs::write(&path, serde_json::to_vec(&binding).unwrap()).unwrap();
    let before = fs::read(f.dir.path().join("state/proposal-intents-v1/journal.json")).unwrap();
    assert!(Runner::open(f.config()).is_err());
    assert_eq!(
        before,
        fs::read(f.dir.path().join("state/proposal-intents-v1/journal.json")).unwrap()
    );
}

#[test]
fn terminal_fence_blocks_preopened_source_writer_and_legacy_fresh_open() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let old = SourceStore::open_with_proposals(
        &f.brain,
        &f.dir.path().join("brain"),
        &f.dir.path().join("state/source"),
        WriteBoundary::Managed,
    )
    .unwrap();
    let source = get(&r, &id, None).source;
    let positive = tessera_core::source::SourceWrite {
        schema: tessera_core::source::SCHEMA.into(),
        operation_id: Uuid::new_v4().to_string(),
        brain_id: f.brain.clone(),
        path: "positive-terminal-fence.md".into(),
        expected_revision: None,
        content_base64: "cG9zaXRpdmU=".into(),
    };
    let positive_receipt = old.write(positive.clone()).unwrap();
    assert_eq!(old.write(positive.clone()).unwrap(), positive_receipt);
    let req = expired_request(&r, &id);
    terminal(r.proposal_disposition_outcome(req, "operator").unwrap());
    let write = tessera_core::source::SourceWrite {
        schema: tessera_core::source::SCHEMA.into(),
        operation_id: Uuid::new_v4().to_string(),
        brain_id: f.brain.clone(),
        path: source.path.clone(),
        expected_revision: Some(source.revision.clone()),
        content_base64: source.content_base64.clone(),
    };
    assert!(old.write(write).is_err());
    assert!(old.write(positive).is_err());
    assert!(SourceStore::open(
        &f.brain,
        &f.dir.path().join("brain"),
        &f.dir.path().join("state/source"),
        WriteBoundary::Managed
    )
    .is_err());
    assert_eq!(get(&r, &id, None).source, source);
}

#[test]
fn terminal_tampering_refuses_startup_without_mutation_or_panic() {
    for field in ["request", "path", "reason", "at", "empty_projection"] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = capture(&mut r);
        start(&mut r, &id, None);
        r.fixture_finish_proposal(&id, None, &result()).unwrap();
        let req = expired_request(&r, &id);
        terminal(
            r.proposal_disposition_outcome(req.clone(), "operator")
                .unwrap(),
        );
        drop(r);
        let path = f.dir.path().join("state/proposal-intents-v1/journal.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match field {
            "request" => {
                value["terminal_dispositions"][&req.operation_id]["request"]["operation_id"] =
                    Uuid::new_v4().to_string().into()
            }
            "path" => {
                value["terminal_dispositions"][&req.operation_id]["path"] =
                    "records/another.md".into()
            }
            "reason" => {
                value["terminal_dispositions"][&req.operation_id]["reason"] = "made_up".into()
            }
            "at" => {
                value["terminal_dispositions"][&req.operation_id]["at"] =
                    "2010-01-01T00:00:00Z".into()
            }
            _ => value["intents"][&id]["draft"]["projections"] = serde_json::json!([]),
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let before = inventory(f.dir.path());
        assert!(Runner::open(f.config()).is_err(), "{field}");
        assert_eq!(before, inventory(f.dir.path()));
    }
}

#[test]
fn expired_snooze_on_rejected_proposal_refuses_without_enrollment_or_inventory_change() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r);
    start(&mut r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    let reject = request(&r, &id, None, api::Disposition::Rejected);
    let committed = r.proposal_disposition(reject.clone(), "operator").unwrap();
    let expired = expired_request(&r, &id);
    let before = inventory(f.dir.path());
    let error = r
        .proposal_disposition_outcome(expired.clone(), "operator")
        .unwrap_err();
    assert_eq!(error.to_string(), "proposal already rejected");
    assert!(!r.source.required_proposal_terminal());
    assert_eq!(inventory(f.dir.path()), before);
    // The Store guard also refuses before checking/enrolling any terminal fence.
    let workspace = r.workspace_identity();
    let path = get(&r, &id, None).source.path;
    assert_eq!(
        r.proposal_store
            .as_mut()
            .unwrap()
            .reserve_expired(workspace, expired, path, crate::inbox::now().unwrap())
            .unwrap_err()
            .to_string(),
        "proposal already rejected"
    );
    assert_eq!(inventory(f.dir.path()), before);
    let api::DeliveryOutcome::Committed(replayed) =
        r.proposal_disposition_outcome(reject, "operator").unwrap()
    else {
        panic!("committed reject must still replay");
    };
    assert!(replayed.replayed);
    assert_eq!(replayed.at, committed.at);
    assert_eq!(inventory(f.dir.path()), before);
}
