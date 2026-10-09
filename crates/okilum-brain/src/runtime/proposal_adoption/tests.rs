use super::*;
use crate::inbox::{CaptureRequest, SourceIdentity};
use crate::proposal as api;
pub(crate) struct Fixture {
    dir: tempfile::TempDir,
    brain: String,
}
impl Fixture {
    pub(crate) fn new() -> Self {
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
    pub(crate) fn open(&self) -> Runner {
        Runner::open(self.config()).unwrap()
    }
    pub(crate) fn enrolled(&self) -> Runner {
        let mut r = self.open();
        r.enroll_proposal_feed(1).unwrap();
        r.enroll_proposal_drafts().unwrap();
        r.enroll_proposal_adoption().unwrap();
        r
    }
}
pub(crate) fn identity() -> SourceIdentity {
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
pub(crate) fn get(r: &Runner, id: &str, goal: Option<&str>) -> api::Detail {
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
pub(crate) fn inventory(p: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
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
pub(crate) fn goal(r: &mut Runner) -> String {
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
pub(crate) fn ready(r: &mut Runner) -> (String, String) {
    let g = goal(r);
    let id = decision(r, &g);
    start(r, &id, Some(&g));
    r.fixture_finish_proposal(&id, Some(&g), &result()).unwrap();
    (g, id)
}
pub(crate) fn adoption(r: &Runner, g: &str, id: &str) -> AdoptionRequest {
    let source = identity();
    AdoptionRequest {
        operation_id: source.message_id.clone(),
        proposal_id: id.into(),
        goal_id: g.into(),
        expected_revision: get(r, id, Some(g)).source.revision,
        expected_goal_revision: r.source.read(&r.path("goal", g)).unwrap().revision,
        source,
        expected_base_packet: None,
        expected_base_revision: None,
        query: "workshop preparation".into(),
        scope: crate::retrieval::SearchScope {
            goal_id: g.into(),
            mode: "goal".into(),
            ..Default::default()
        },
        citations: vec![],
        pinned_citation_ids: vec![],
        guidance: "Preserve the operator's workshop instructions.".into(),
    }
}
#[test]
fn adoption_all_crash_cuts_replay_one_original_unreviewed_target() {
    for cut in 0..8 {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let (g, id) = ready(&mut r);
        let req = adoption(&r, &g, &id);
        match cut {
            0 => r.proposal_adoption_fault = Some(Fault::AfterIntent),
            1 => r.proposal_adoption_fault = Some(Fault::BeforeTarget),
            2 => r.proposal_adoption_fault = Some(Fault::AfterTarget),
            3 => r.proposal_adoption_fault = Some(Fault::AfterTargetReceipt),
            4 => {
                r.proposal_draft_fault =
                    Some(super::super::proposal_drafts::Fault::BeforeProjection)
            }
            5 => {
                r.proposal_draft_fault = Some(super::super::proposal_drafts::Fault::AfterProjection)
            }
            6 => r.proposal_draft_fault = Some(super::super::proposal_drafts::Fault::AfterReceipt),
            _ => r.proposal_adoption_fault = Some(Fault::AfterPointer),
        }
        assert!(
            r.adopt_proposal_context(req.clone(), "operator").is_err(),
            "cut {cut}"
        );
        let retained = r.drafts().unwrap().adoption(&id).unwrap().target().unwrap();
        let original_id = retained.packet_id().to_string();
        let revision = retained.revision().to_string();
        drop(r);
        let mut r = f.open();
        let receipt = r.adopt_proposal_context(req.clone(), "operator").unwrap();
        assert!(receipt.replayed);
        assert_eq!(receipt.target.packet_id, original_id);
        assert_eq!(receipt.target.revision, revision);
        assert_eq!(
            fs::read_dir(r.root.join("records"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e
                    .file_name()
                    .to_string_lossy()
                    .starts_with("reviewed-context-"))
                .count(),
            1
        );
        let packet = crate::context::read(&r, &g, &original_id).unwrap();
        assert!(!packet.reviewed);
        assert_eq!(packet.text, req.guidance);
        assert!(crate::context::require_reviewed(
            &r,
            &g,
            &crate::context::ReviewedPacketRef {
                id: original_id.clone(),
                revision
            }
        )
        .is_err());
        let outcome = get(&r, &id, Some(&g));
        assert!(matches!(
            outcome.record.disposition,
            api::Disposition::Adopted { .. }
        ));
        assert_eq!(r.application_state()["reviewed_packet_id"], original_id);
        let before = inventory(f.dir.path());
        r.adopt_proposal_context(req.clone(), "operator").unwrap();
        assert_eq!(inventory(f.dir.path()), before);
        let mut changed = req;
        changed.guidance.push('!');
        assert!(r.adopt_proposal_context(changed, "operator").is_err());
    }
}
#[test]
fn adoption_replay_survives_newer_sources_selection_and_preserves_other_goal() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let (g, id) = ready(&mut r);
    let other = goal(&mut r);
    r.state.route(&other).unwrap();
    r.persist().unwrap();
    let req = adoption(&r, &g, &id);
    let before = r.state.goal_id.clone();
    r.proposal_adoption_fault = Some(Fault::AfterTargetReceipt);
    assert!(r.adopt_proposal_context(req.clone(), "operator").is_err());
    let newer = Uuid::new_v4().to_string();
    r.with_goal(&g,|r|r.checkpoint_application(serde_json::json!({"conversations":{},"mutations":{},"task":null,"reviewed_packet_id":newer,"unknown_future_field":{"keep":1}}),None,None)).unwrap();
    assert_eq!(r.state.goal_id, before);
    let other_state = r.application_state();
    drop(r);
    let mut r = f.open();
    let restored_selection = r.state.goal_id.clone();
    let receipt = r.adopt_proposal_context(req.clone(), "operator").unwrap();
    assert_eq!(
        receipt.pointer,
        crate::proposals::PointerOutcome::PreservedNewer
    );
    assert_eq!(r.state.goal_id, restored_selection);
    r.with_goal(&other, |r| {
        assert_eq!(r.application_state(), other_state);
        Ok(())
    })
    .unwrap();
    r.with_goal(&g, |r| {
        assert_eq!(r.application_state()["reviewed_packet_id"], newer);
        assert_eq!(r.application_state()["unknown_future_field"]["keep"], 1);
        Ok(())
    })
    .unwrap();
    fs::write(r.root.join(&receipt.target.path), "Manual target edit").unwrap();
    fs::write(r.root.join(r.path("proposal", &id)), "Manual proposal edit").unwrap();
    fs::write(r.root.join(r.path("goal", &g)), "Manual goal edit").unwrap();
    let original = serde_json::to_value(&receipt).unwrap();
    let before = inventory(f.dir.path());
    assert_eq!(
        serde_json::to_value(r.adopt_proposal_context(req, "operator").unwrap()).unwrap(),
        original
    );
    assert_eq!(inventory(f.dir.path()), before);
}
#[test]
fn adoption_pending_manual_conflict_retains_target_and_does_not_block_capture() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let (g, id) = ready(&mut r);
    let req = adoption(&r, &g, &id);
    r.proposal_adoption_fault = Some(Fault::AfterTargetReceipt);
    assert!(r.adopt_proposal_context(req.clone(), "operator").is_err());
    let target = r.drafts().unwrap().adoption(&id).unwrap().target().unwrap();
    let target_bytes = fs::read(r.root.join(&target.source_write().path)).unwrap();
    let path = r.root.join(r.path("proposal", &id));
    let original = fs::read(&path).unwrap();
    fs::write(&path, "Manual proposal update").unwrap();
    let reject = request(&r, &id, Some(&g), api::Disposition::Rejected);
    assert!(r.proposal_disposition(reject, "operator").is_err());
    drop(r);
    let mut r = f.open();
    assert!(r.proposal_draft_issue.is_some());
    assert!(r.adopt_proposal_context(req.clone(), "operator").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"Manual proposal update");
    assert_eq!(
        fs::read(r.root.join(&target.source_write().path)).unwrap(),
        target_bytes
    );
    capture(&mut r);
    let other = goal(&mut r);
    let _other_id = decision(&mut r, &other);
    // SourceStore preserves an indeterminate conflict; no automatic overwrite is attempted.
    assert!(r
        .drafts()
        .unwrap()
        .adoption(&id)
        .unwrap()
        .target_receipt
        .is_some());
    assert_ne!(original, fs::read(&path).unwrap());
}
#[test]
fn adoption_identity_reservation_is_bidirectional_and_actor_bound() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let (g, id) = ready(&mut r);
    let req = adoption(&r, &g, &id);
    assert!(r
        .adopt_proposal_context(req.clone(), "someone-else")
        .is_err());
    r.proposal_adoption_fault = Some(Fault::AfterIntent);
    assert!(r.adopt_proposal_context(req.clone(), "operator").is_err());
    let capture_req = CaptureRequest {
        operation_id: req.operation_id.clone(),
        source: req.source.clone(),
        text: "must not capture".into(),
    };
    assert!(r.inbox_capture(capture_req, "operator").is_err());
    let other = capture(&mut r);
    start(&mut r, &other, None);
    r.fixture_finish_proposal(&other, None, &result()).unwrap();
    let mut reject = request(&r, &other, None, api::Disposition::Rejected);
    reject.operation_id = req.operation_id.clone();
    reject.source = req.source.clone();
    assert!(r.proposal_disposition(reject, "operator").is_err());
    let other_goal = goal(&mut r);
    let other_draft = decision(&mut r, &other_goal);
    start(&mut r, &other_draft, Some(&other_goal));
    r.fixture_finish_proposal(&other_draft, Some(&other_goal), &result())
        .unwrap();
    let source = identity();
    r.inbox_capture(
        CaptureRequest {
            operation_id: source.message_id.clone(),
            source: source.clone(),
            text: "reserved capture".into(),
        },
        "operator",
    )
    .unwrap();
    let mut adopt = adoption(&r, &other_goal, &other_draft);
    adopt.operation_id = source.message_id.clone();
    adopt.source = source;
    assert!(r.adopt_proposal_context(adopt, "operator").is_err());
    let mut altered = req.clone();
    altered.goal_id = other_goal;
    assert!(r.adopt_proposal_context(altered, "operator").is_err());
    let mut snooze = request(
        &r,
        &id,
        Some(&g),
        api::Disposition::Snoozed {
            until: "2099-01-01T00:00:00Z".into(),
        },
    );
    snooze.expected_revision = req.expected_revision;
    assert!(r.proposal_disposition(snooze, "operator").is_err());
}
#[test]
fn adoption_preserves_manual_base_pins_guidance_and_requires_current_base() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let (g, id) = ready(&mut r);
    let app = crate::application::Application::unconfigured();
    let text = "# Evidence\n\nPinned workshop source.\n";
    fs::write(r.root.join("evidence.md"), text).unwrap();
    let source = r.read_source("evidence.md").unwrap();
    let citation = crate::retrieval::Citation {
        citation_id: crate::retrieval::citation_id("evidence.md", &source.revision, 1, 3),
        path: "evidence.md".into(),
        revision: source.revision,
        start_line: 1,
        end_line: 3,
        locator: "L1-L3".into(),
        excerpt: text.into(),
        metadata: Default::default(),
    };
    let scope = crate::retrieval::SearchScope {
        goal_id: g.clone(),
        mode: "project".into(),
        ..Default::default()
    };
    let base = app
        .context_prepare(
            &mut r,
            g.clone(),
            "operator context".into(),
            scope.clone(),
            vec![citation.clone()],
            vec![citation.citation_id.clone()],
        )
        .unwrap();
    let base: crate::context::ReviewedPacket =
        serde_json::from_value(base["packet"].clone()).unwrap();
    let manual = "Original manual guidance — 雪\nKeep this exactly.";
    app.context_revise(
        &mut r,
        g.clone(),
        base.id.clone(),
        base.revision,
        manual.into(),
    )
    .unwrap();
    let base = crate::context::read(&r, &g, &base.id).unwrap();
    let mut req = adoption(&r, &g, &id);
    req.expected_base_packet = Some(base.id.clone());
    req.expected_base_revision = Some(base.revision.clone());
    req.scope = scope;
    req.citations = vec![citation.clone()];
    req.pinned_citation_ids = vec![citation.citation_id];
    req.guidance = format!("{}\n\nAdded proposal guidance.", base.text);
    let before = inventory(f.dir.path());
    let mut bad = req.clone();
    bad.pinned_citation_ids.clear();
    assert!(r.adopt_proposal_context(bad, "operator").is_err());
    let mut bad = req.clone();
    bad.guidance = "replacement drops manual content".into();
    assert!(r.adopt_proposal_context(bad, "operator").is_err());
    let mut bad = req.clone();
    bad.expected_base_revision = Some(format!("sha256:{}", "0".repeat(64)));
    assert!(r.adopt_proposal_context(bad, "operator").is_err());
    assert_eq!(inventory(f.dir.path()), before);
    let receipt = r.adopt_proposal_context(req.clone(), "operator").unwrap();
    let packet = crate::context::read(&r, &g, &receipt.target.packet_id).unwrap();
    assert_eq!(packet.text, req.guidance);
    assert_eq!(packet.citations, req.citations);
    assert_eq!(packet.pinned_citation_ids, req.pinned_citation_ids);
    assert!(!packet.reviewed);
    assert!(!receipt.replayed);
}
#[test]
fn adoption_capability_refuses_a3_fresh_and_preopened_writers() {
    let f = Fixture::new();
    let mut r = f.open();
    r.enroll_proposal_feed(1).unwrap();
    r.enroll_proposal_drafts().unwrap();
    let state = r.state_dir.join("source");
    let old = SourceStore::open_with_proposals(&f.brain, &r.root, &state, WriteBoundary::Managed)
        .unwrap();
    let mut write = SourceWrite {
        schema: SCHEMA.into(),
        operation_id: Uuid::new_v4().to_string(),
        brain_id: f.brain.clone(),
        path: "positive.md".into(),
        expected_revision: None,
        content_base64: STANDARD.encode("positive"),
    };
    old.write(write.clone()).unwrap();
    r.enroll_proposal_adoption().unwrap();
    let before = inventory(f.dir.path());
    assert!(
        SourceStore::open_with_proposals(&f.brain, &r.root, &state, WriteBoundary::Managed)
            .is_err()
    );
    write.operation_id = Uuid::new_v4().to_string();
    write.path = "must-not-write.md".into();
    assert!(old.write(write).is_err());
    assert_eq!(inventory(f.dir.path()), before);
    let read = SourceStore::open_read_only(&f.brain, &r.root, &state).unwrap();
    assert!(read.read("positive.md").is_ok());
    drop(r);
    let mut r = f.open();
    capture(&mut r);
}
#[test]
fn adoption_corrupt_retained_form_is_refused_without_source_mutation() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let (g, id) = ready(&mut r);
    let req = adoption(&r, &g, &id);
    r.proposal_adoption_fault = Some(Fault::AfterIntent);
    assert!(r.adopt_proposal_context(req, "operator").is_err());
    drop(r);
    let path = f.dir.path().join("state/proposal-intents-v1/journal.json");
    let mut journal: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    journal["intents"][&id]["draft"]["adoption"]["request"]["guidance"] = "different form".into();
    fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let before = inventory(f.dir.path());
    assert!(Runner::open(f.config()).is_err());
    assert_eq!(inventory(f.dir.path()), before);
}
#[test]
fn adoption_capacity_reserves_completion_before_any_target_write() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let (g, id) = ready(&mut r);
    let req = adoption(&r, &g, &id);
    let path = r.state_dir.join("proposal-intents-v1/journal.json");
    let original = fs::read(&path).unwrap();
    r.proposal_adoption_fault = Some(Fault::AfterIntent);
    assert!(r.adopt_proposal_context(req.clone(), "operator").is_err());
    let staged = fs::read(&path).unwrap();
    let intent_growth = staged.len() - original.len();
    // Duplicate-delivery receipt padding is a store-capacity fixture; no real
    // feed is advanced. Leave room for the intent but not its final projection.
    let mut journal: Value = serde_json::from_slice(&original).unwrap();
    let limit = 16 * 1024 * 1024 - intent_growth - 1024;
    let mut length = original.len();
    let mut cursor = journal["cursor"].as_u64().unwrap();
    loop {
        let next = cursor + 1;
        let key = next.to_string();
        let growth = 2 * (key.len() + 64 + 6) + key.len() - cursor.to_string().len();
        if length + growth > limit {
            break;
        }
        journal["events"][&key] = Value::String(id.clone());
        journal["event_bytes"][&key] = Value::String("a".repeat(64));
        cursor = next;
        length += growth;
    }
    journal["cursor"] = cursor.into();
    let padded = serde_json::to_vec(&journal).unwrap();
    assert!(padded.len() + intent_growth < 16 * 1024 * 1024);
    fs::write(&path, &padded).unwrap();
    r.proposal_store
        .as_mut()
        .unwrap()
        .reload_without_recovery()
        .unwrap();
    let sources = inventory(&r.root);
    let failure = r
        .adopt_proposal_context(req.clone(), "operator")
        .unwrap_err()
        .to_string();
    assert!(failure.contains("including recovery reserve"), "{failure}");
    assert_eq!(inventory(&r.root), sources);
    assert_eq!(fs::read(&path).unwrap(), padded);
    assert!(r.drafts().unwrap().adoption(&id).is_err());
    // Positive control at the admitted reserve boundary (one event granule plus
    // 128 bytes of clock-format tolerance). Reopen the actual Store, then recover.
    let mut projected: Value = serde_json::from_slice(&staged).unwrap();
    let op = projected["intents"][&id]["draft"]["adoption"].clone();
    projected["intents"][&id]["draft"]["record"] = op["adopted_record"].clone();
    projected["intents"][&id]["draft"]["projections"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"write":op["adopted_write"],"receipt":null}));
    let projection_growth = serde_json::to_vec(&projected).unwrap().len() - staged.len();
    let admitted = 16 * 1024 * 1024 - intent_growth - projection_growth - 3 * 16 * 1024 - 128;
    length = padded.len();
    while length > admitted {
        let key = cursor.to_string();
        let previous = cursor - 1;
        length -= 2 * (key.len() + 64 + 6) + key.len() - previous.to_string().len();
        journal["events"].as_object_mut().unwrap().remove(&key);
        journal["event_bytes"].as_object_mut().unwrap().remove(&key);
        cursor = previous;
    }
    journal["cursor"] = cursor.into();
    let accepted = serde_json::to_vec(&journal).unwrap();
    assert_eq!(accepted.len(), length);
    assert!(admitted - length < 160);
    fs::write(&path, &accepted).unwrap();
    r.proposal_store
        .as_mut()
        .unwrap()
        .reload_without_recovery()
        .unwrap();
    r.proposal_adoption_fault = Some(Fault::AfterIntent);
    let failure = r
        .adopt_proposal_context(req.clone(), "operator")
        .unwrap_err()
        .to_string();
    assert!(failure.contains("AfterIntent"), "{failure}");
    drop(r.proposal_store.take());
    r.proposal_store = Some(
        crate::proposals::Store::open_bound(
            &r.state_dir,
            &r.state.brain_id,
            r.source.required_proposal_feed(),
        )
        .unwrap(),
    );
    r.recover_context_adoption(&id).unwrap();
    let receipt = r
        .drafts()
        .unwrap()
        .adoption_replay(&req)
        .unwrap()
        .flatten()
        .unwrap();
    assert_eq!(receipt.target.goal_id, g);
    assert!(fs::metadata(&path).unwrap().len() <= 16 * 1024 * 1024);
}
#[test]
#[ignore = "requires empty OKILUM_ADOPTION_COMPAT_FIXTURE directory"]
fn export_proposal_adoption_compatibility_fixture() {
    let target =
        std::path::PathBuf::from(std::env::var_os("OKILUM_ADOPTION_COMPAT_FIXTURE").unwrap());
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
    r.enroll_proposal_adoption().unwrap();
    let (g, id) = ready(&mut r);
    let other = goal(&mut r);
    let app = crate::application::Application::unconfigured();
    let evidence = "# Workshop evidence\n\nOriginal source for pinned context.\n";
    fs::write(r.root.join("evidence.md"), evidence).unwrap();
    let source = r.read_source("evidence.md").unwrap();
    let citation = crate::retrieval::Citation {
        citation_id: crate::retrieval::citation_id("evidence.md", &source.revision, 1, 3),
        path: "evidence.md".into(),
        revision: source.revision,
        start_line: 1,
        end_line: 3,
        locator: "L1-L3".into(),
        excerpt: evidence.into(),
        metadata: Default::default(),
    };
    let scope = crate::retrieval::SearchScope {
        goal_id: g.clone(),
        mode: "project".into(),
        ..Default::default()
    };
    let base = app
        .context_prepare(
            &mut r,
            g.clone(),
            "Original operator context".into(),
            scope.clone(),
            vec![citation.clone()],
            vec![citation.citation_id.clone()],
        )
        .unwrap();
    let base: crate::context::ReviewedPacket =
        serde_json::from_value(base["packet"].clone()).unwrap();
    app.context_revise(
        &mut r,
        g.clone(),
        base.id.clone(),
        base.revision,
        "Manual compatibility guidance — 雪. Keep exact pins.".into(),
    )
    .unwrap();
    let base = crate::context::read(&r, &g, &base.id).unwrap();
    let mut req = adoption(&r, &g, &id);
    req.expected_base_packet = Some(base.id);
    req.expected_base_revision = Some(base.revision);
    req.scope = scope;
    req.citations = vec![citation.clone()];
    req.pinned_citation_ids = vec![citation.citation_id];
    req.guidance = format!("{}\n\nAdditional inspected proposal guidance.", base.text);
    let cut = std::env::var("OKILUM_ADOPTION_COMPAT_CUT").unwrap_or("intent".into());
    match cut.as_str() {
        "intent" => r.proposal_adoption_fault = Some(Fault::AfterIntent),
        "target" => r.proposal_adoption_fault = Some(Fault::AfterTarget),
        "target_receipt" => r.proposal_adoption_fault = Some(Fault::AfterTargetReceipt),
        "projection" => {
            r.proposal_draft_fault = Some(super::super::proposal_drafts::Fault::AfterProjection)
        }
        "projection_receipt" => {
            r.proposal_draft_fault = Some(super::super::proposal_drafts::Fault::AfterReceipt)
        }
        "pointer" => r.proposal_adoption_fault = Some(Fault::AfterPointer),
        _ => panic!("unsupported adoption cut"),
    }
    assert!(r.adopt_proposal_context(req.clone(), "operator").is_err());
    let op = r.drafts().unwrap().adoption(&id).unwrap();
    fs::write(target.join("fixture.json"),serde_json::to_vec_pretty(&serde_json::json!({"brain_id":brain,"goal_id":g,"other_goal_id":other,"proposal_id":id,"request":req,"operation":op,"cut":cut})).unwrap()).unwrap();
}
#[test]
fn adoption_missing_fence_and_tampered_original_receipt_refuse_before_recovery() {
    for removed_fence in [true, false] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let (g, id) = ready(&mut r);
        let req = adoption(&r, &g, &id);
        r.proposal_adoption_fault = Some(Fault::AfterTargetReceipt);
        assert!(r.adopt_proposal_context(req, "operator").is_err());
        // An unrelated running attempt is a positive control for restart writes.
        let another = capture(&mut r);
        start(&mut r, &another, None);
        drop(r);
        let path = if removed_fence {
            f.dir.path().join("state/source/binding.json")
        } else {
            f.dir.path().join("state/proposal-intents-v1/journal.json")
        };
        let mut v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if removed_fence {
            v.as_object_mut()
                .unwrap()
                .remove("required_proposal_adoption");
        } else {
            v["intents"][&id]["draft"]["adoption"]["target_receipt"]["outcome"] =
                "unchanged".into();
        }
        fs::write(path, serde_json::to_vec(&v).unwrap()).unwrap();
        let before = inventory(f.dir.path());
        assert!(Runner::open(f.config()).is_err());
        assert_eq!(inventory(f.dir.path()), before);
    }
}
