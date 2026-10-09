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
        r.source.require_proposal_adoption().unwrap();
        r.open_proposal_adoption().unwrap();
        r.enroll_proposal_inbox_adoption().unwrap();
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
pub(crate) fn ready(r: &mut Runner) -> String {
    let id = capture(r);
    start(r, &id, None);
    r.fixture_finish_proposal(&id, None, &result()).unwrap();
    id
}
pub(crate) fn adopt(r: &Runner, id: &str) -> InboxAdoptionRequest {
    let p = get(r, id, None);
    let source = identity();
    InboxAdoptionRequest {
        operation_id: source.message_id.clone(),
        proposal_id: id.into(),
        expected_revision: p.source.revision,
        capture_id: p.record.trigger.identity.record_id,
        expected_capture_revision: p.record.trigger.identity.source_revision,
        title: "Inspected workshop goal".into(),
        criteria: vec![crate::Criterion {
            id: "C1".into(),
            description: "Manual criteria retained".into(),
            requires_human: true,
        }],
        source,
    }
}
#[test]
fn inbox_adoption_all_cuts_recover_original_goal_and_receipts() {
    for cut in 0..8 {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let selected = goal(&mut r);
        let id = ready(&mut r);
        let req = adopt(&r, &id);
        match cut {
            0 => r.proposal_inbox_adoption_fault = Some(Fault::ParentIntent),
            1 => r.proposal_inbox_adoption_fault = Some(Fault::ChildIntent),
            2 => r.proposal_inbox_adoption_fault = Some(Fault::ChildSource),
            3 => r.proposal_inbox_adoption_fault = Some(Fault::ChildReceipt),
            4 => r.proposal_inbox_adoption_fault = Some(Fault::ParentReceipt),
            5 => {
                r.proposal_draft_fault =
                    Some(super::super::proposal_drafts::Fault::BeforeProjection)
            }
            6 => {
                r.proposal_draft_fault = Some(super::super::proposal_drafts::Fault::AfterProjection)
            }
            _ => r.proposal_draft_fault = Some(super::super::proposal_drafts::Fault::AfterReceipt),
        }
        assert!(
            r.adopt_proposal_inbox(req.clone(), "operator").is_err(),
            "cut {cut}"
        );
        let original = r
            .drafts()
            .unwrap()
            .inbox_adoption(&id)
            .unwrap()
            .target()
            .unwrap();
        let target_id = original.outcome().goal_id.clone();
        let outcome_before = original.outcome().clone();
        drop(r);
        let mut r = f.open();
        let receipt = r.adopt_proposal_inbox(req.clone(), "operator").unwrap();
        assert!(receipt.replayed);
        assert_eq!(receipt.target.goal_id, target_id);
        assert_eq!(receipt.child.outcome.origin, outcome_before.origin);
        assert_eq!(r.state.goal_id.as_ref(), Some(&selected));
        assert_eq!(r.goal_ids().len(), 2);
        assert!(r.state.pending_writes.is_empty());
        let detail = get(&r, &id, None);
        assert!(detail.record.goal_id.is_none());
        assert!(matches!(
            detail.record.disposition,
            api::Disposition::AdoptedInboxGoal { .. }
        ));
        let goal = r
            .with_goal(&target_id, |r| Ok(r.snapshot()?.goal.unwrap()))
            .unwrap();
        assert_eq!(goal.title, req.title);
        assert_eq!(goal.criteria, req.criteria);
        assert!(goal.stage_ids.is_empty());
        assert!(goal.task_ref.is_none());
        let before = inventory(f.dir.path());
        let replay = r.adopt_proposal_inbox(req.clone(), "operator").unwrap();
        assert_eq!(replay, receipt);
        assert_eq!(inventory(f.dir.path()), before);
        let mut changed = req;
        changed.title.push('!');
        assert!(r.adopt_proposal_inbox(changed, "operator").is_err());
    }
}
#[test]
fn inbox_parent_only_recovery_uses_frozen_capture_in_maintenance() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = ready(&mut r);
    let req = adopt(&r, &id);
    r.proposal_inbox_adoption_fault = Some(Fault::ParentIntent);
    assert!(r.adopt_proposal_inbox(req.clone(), "operator").is_err());
    let t = r
        .drafts()
        .unwrap()
        .inbox_adoption(&id)
        .unwrap()
        .target()
        .unwrap();
    let snapshot = t.outcome().origin.source_snapshot.clone();
    fs::write(r.root.join(&snapshot.path), "changed live capture").unwrap();
    r.inbox_planning_enabled = false;
    r.recover_inbox_adoption(&id).unwrap();
    let receipt = r.adopt_proposal_inbox(req.clone(), "operator").unwrap();
    assert_eq!(receipt.child.outcome.origin.source_snapshot, snapshot);
    assert_eq!(receipt.target.goal_id, t.outcome().goal_id);
    assert_eq!(
        fs::read(r.root.join(t.write().path.clone())).unwrap(),
        STANDARD.decode(&t.write().content_base64).unwrap()
    );
    // A second frozen proposal is a positive control for the maintenance gate.
    let next = ready(&mut r);
    let req = adopt(&r, &next);
    let before = inventory(&r.root);
    assert!(r.adopt_proposal_inbox(req.clone(), "operator").is_err());
    assert_eq!(inventory(&r.root), before);
    r.inbox_planning_enabled = true;
    assert!(r.adopt_proposal_inbox(req, "operator").is_ok());
}
#[test]
fn inbox_adoption_parent_child_aliases_cannot_escape_reservations() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = ready(&mut r);
    let req = adopt(&r, &id);
    assert!(r.adopt_proposal_inbox(req.clone(), "other actor").is_err());
    r.proposal_inbox_adoption_fault = Some(Fault::ParentIntent);
    assert!(r.adopt_proposal_inbox(req.clone(), "operator").is_err());
    let child = r
        .drafts()
        .unwrap()
        .inbox_adoption(&id)
        .unwrap()
        .target()
        .unwrap()
        .request()
        .clone();
    for operation in [&req.operation_id, &child.operation_id] {
        let capture = CaptureRequest {
            operation_id: operation.clone(),
            text: "must refuse".into(),
            source: req.source.clone(),
        };
        assert!(r.inbox_capture(capture, "operator").is_err());
    }
    assert!(r.inbox_plan(child.clone(), "operator").is_err());
    let mut alias = child.clone();
    alias.operation_id = Uuid::new_v4().to_string();
    assert!(r.inbox_plan(alias, "operator").is_err());
    let other = ready(&mut r);
    let mut reject = request(&r, &other, None, api::Disposition::Rejected);
    reject.operation_id = child.operation_id.clone();
    assert!(r.proposal_disposition(reject, "operator").is_err());
    let snooze = request(
        &r,
        &id,
        None,
        api::Disposition::Snoozed {
            until: "2099-01-01T00:00:00Z".into(),
        },
    );
    assert!(r.proposal_disposition(snooze, "operator").is_err());
    let original = r.adopt_proposal_inbox(req.clone(), "operator").unwrap();
    assert!(original.replayed);
    assert!(r.inbox_plan(child, "operator").is_err());
    assert_eq!(r.adopt_proposal_inbox(req, "operator").unwrap(), original);
    // Goal-bound proposal is a positive ownership control; it cannot adopt into Inbox.
    let g = goal(&mut r);
    let bound = decision(&mut r, &g);
    start(&mut r, &bound, Some(&g));
    r.fixture_finish_proposal(&bound, Some(&g), &result())
        .unwrap();
    let mut wrong = adopt(&r, &other);
    wrong.proposal_id = bound;
    assert!(r.adopt_proposal_inbox(wrong, "operator").is_err());
}
#[test]
fn inbox_adoption_target_and_proposal_conflicts_remain_local() {
    for conflict_goal in [true, false] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = ready(&mut r);
        let req = adopt(&r, &id);
        r.proposal_inbox_adoption_fault = Some(if conflict_goal {
            Fault::ChildIntent
        } else {
            Fault::ParentReceipt
        });
        assert!(r.adopt_proposal_inbox(req.clone(), "operator").is_err());
        let op = r.drafts().unwrap().inbox_adoption(&id).unwrap().clone();
        let path = if conflict_goal {
            op.target().unwrap().write().path.clone()
        } else {
            op.adopted_write.path.clone()
        };
        fs::write(r.root.join(&path), "Manual divergent source").unwrap();
        drop(r);
        let mut r = f.open();
        assert!(r.proposal_draft_issue.is_some());
        assert!(r.adopt_proposal_inbox(req, "operator").is_err());
        assert_eq!(
            fs::read(r.root.join(&path)).unwrap(),
            b"Manual divergent source"
        );
        let source = identity();
        let capture = r
            .inbox_capture(
                CaptureRequest {
                    operation_id: source.message_id.clone(),
                    source,
                    text: "unrelated capture still accepted".into(),
                },
                "operator",
            )
            .unwrap();
        assert!(r.inbox_get(&capture.capture_id).is_ok());
        assert!(r.state.pending_writes.is_empty());
    }
}
#[test]
fn inbox_adoption_completed_replay_survives_changed_capture_goal_and_proposal() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = ready(&mut r);
    let req = adopt(&r, &id);
    let first = r.adopt_proposal_inbox(req.clone(), "operator").unwrap();
    assert!(!first.replayed);
    let original = r.drafts().unwrap().inbox_adoption(&id).unwrap().clone();
    for path in [
        &original.target().unwrap().outcome().origin.path,
        &first.target.path,
        &original.adopted_write.path,
    ] {
        fs::write(r.root.join(path), "Manual edit after commitment").unwrap();
    }
    let before = inventory(f.dir.path());
    let mut replay = r.adopt_proposal_inbox(req, "operator").unwrap();
    assert!(replay.replayed);
    replay.replayed = false;
    assert_eq!(replay, first);
    assert_eq!(inventory(f.dir.path()), before);
}
#[test]
fn inbox_adoption_capability_refuses_preceding_context_writer() {
    let f = Fixture::new();
    let mut r = f.open();
    r.enroll_proposal_feed(1).unwrap();
    r.enroll_proposal_drafts().unwrap();
    r.source.require_proposal_adoption().unwrap();
    r.open_proposal_adoption().unwrap();
    let state = r.state_dir.join("source");
    let old =
        SourceStore::open_with_proposal_adoption(&f.brain, &r.root, &state, WriteBoundary::Managed)
            .unwrap();
    let mut write = SourceWrite {
        schema: SCHEMA.into(),
        operation_id: Uuid::new_v4().to_string(),
        brain_id: f.brain.clone(),
        path: "positive.md".into(),
        expected_revision: None,
        content_base64: STANDARD.encode("positive control"),
    };
    old.write(write.clone()).unwrap();
    r.enroll_proposal_inbox_adoption().unwrap();
    let before = inventory(f.dir.path());
    assert!(SourceStore::open_with_proposal_adoption(
        &f.brain,
        &r.root,
        &state,
        WriteBoundary::Managed
    )
    .is_err());
    assert!(old.write(write.clone()).is_err());
    write.operation_id = Uuid::new_v4().to_string();
    write.path = "must-not-write.md".into();
    assert!(old.write(write).is_err());
    assert_eq!(inventory(f.dir.path()), before);
}
#[test]
fn inbox_adoption_corrupt_parent_child_binding_refuses_before_recovery() {
    for corrupt in [
        "child_identity",
        "source_identity",
        "missing_fence",
        "child_receipt",
    ] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = ready(&mut r);
        let req = adopt(&r, &id);
        r.proposal_inbox_adoption_fault = Some(if corrupt == "child_receipt" {
            Fault::ChildReceipt
        } else {
            Fault::ChildIntent
        });
        assert!(r.adopt_proposal_inbox(req, "operator").is_err());
        let child = r
            .drafts()
            .unwrap()
            .inbox_adoption(&id)
            .unwrap()
            .child_operation_id()
            .to_string();
        let another = capture(&mut r);
        start(&mut r, &another, None);
        drop(r);
        let path = f.dir.path().join(if corrupt == "missing_fence" {
            "state/source/binding.json"
        } else {
            "state/state.json"
        });
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match corrupt {
            "missing_fence" => {
                value
                    .as_object_mut()
                    .unwrap()
                    .remove("required_proposal_inbox_adoption");
            }
            "child_identity" => {
                value["inbox_plan_journal"]["operations"][&child]["delegation"]
                    ["child_operation_id"] = Uuid::new_v4().to_string().into()
            }
            "source_identity" => {
                value["inbox_plan_journal"]["operations"][&child]["request"]["source"]["actor_id"] =
                    "someone else".into()
            }
            _ => {
                value["inbox_plan_journal"]["operations"][&child]["delegated_source_receipt"]
                    ["outcome"] = "unchanged".into()
            }
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let before = inventory(f.dir.path());
        assert!(Runner::open(f.config()).is_err(), "{corrupt}");
        assert_eq!(inventory(f.dir.path()), before, "{corrupt}");
    }
}
#[test]
fn inbox_adoption_capacity_refuses_before_child_and_recovers_at_admitted_bound() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = ready(&mut r);
    let req = adopt(&r, &id);
    let path = r.state_dir.join("proposal-intents-v1/journal.json");
    let original = fs::read(&path).unwrap();
    r.proposal_inbox_adoption_fault = Some(Fault::ParentIntent);
    assert!(r.adopt_proposal_inbox(req.clone(), "operator").is_err());
    let staged = fs::read(&path).unwrap();
    let growth = staged.len() - original.len();
    let mut journal: Value = serde_json::from_slice(&original).unwrap();
    let mut cursor = journal["cursor"].as_u64().unwrap();
    let mut size = original.len();
    let limit = 16 * 1024 * 1024 - growth - 1024;
    loop {
        let next = cursor + 1;
        let key = next.to_string();
        let delta = 2 * (key.len() + 64 + 6) + key.len() - cursor.to_string().len();
        if size + delta > limit {
            break;
        }
        journal["events"][&key] = id.clone().into();
        journal["event_bytes"][&key] = "a".repeat(64).into();
        cursor = next;
        size += delta;
    }
    journal["cursor"] = cursor.into();
    let padded = serde_json::to_vec(&journal).unwrap();
    assert_eq!(size, padded.len());
    fs::write(&path, &padded).unwrap();
    r.proposal_store
        .as_mut()
        .unwrap()
        .reload_without_recovery()
        .unwrap();
    let before = inventory(&r.root);
    let state_before = fs::read(r.state_dir.join("state.json")).unwrap();
    let error = r
        .adopt_proposal_inbox(req.clone(), "operator")
        .unwrap_err()
        .to_string();
    assert!(error.contains("including recovery reserve"), "{error}");
    assert_eq!(inventory(&r.root), before);
    assert_eq!(
        fs::read(r.state_dir.join("state.json")).unwrap(),
        state_before
    );
    assert_eq!(fs::read(&path).unwrap(), padded);
    let mut projected: Value = serde_json::from_slice(&staged).unwrap();
    let op = projected["intents"][&id]["draft"]["inbox_adoption"].clone();
    let frozen: Value = serde_json::from_slice(
        &STANDARD
            .decode(op["frozen_target_base64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    projected["intents"][&id]["draft"]["record"] = op["adopted_record"].clone();
    projected["intents"][&id]["draft"]["projections"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"write":op["adopted_write"],"receipt":null}));
    let projection_growth = serde_json::to_vec(&projected).unwrap().len() - staged.len();
    let child_reserve = serde_json::to_vec(&frozen["outcome"]).unwrap().len() + 16 * 1024;
    let admitted = 16 * 1024 * 1024 - growth - projection_growth - child_reserve - 16 * 1024 - 128;
    while size > admitted {
        let key = cursor.to_string();
        let previous = cursor - 1;
        size -= 2 * (key.len() + 64 + 6) + key.len() - previous.to_string().len();
        journal["events"].as_object_mut().unwrap().remove(&key);
        journal["event_bytes"].as_object_mut().unwrap().remove(&key);
        cursor = previous;
    }
    journal["cursor"] = cursor.into();
    let accepted = serde_json::to_vec(&journal).unwrap();
    assert_eq!(accepted.len(), size);
    assert!(admitted - size < 160);
    fs::write(&path, accepted).unwrap();
    r.proposal_store
        .as_mut()
        .unwrap()
        .reload_without_recovery()
        .unwrap();
    r.proposal_inbox_adoption_fault = Some(Fault::ParentIntent);
    let error = r
        .adopt_proposal_inbox(req.clone(), "operator")
        .unwrap_err()
        .to_string();
    assert!(error.contains("ParentIntent"), "{error}");
    drop(r.proposal_store.take());
    r.proposal_store = Some(
        crate::proposals::Store::open_bound(
            &r.state_dir,
            &r.state.brain_id,
            r.source.required_proposal_feed(),
        )
        .unwrap(),
    );
    r.recover_inbox_adoption(&id).unwrap();
    let receipt = r
        .drafts()
        .unwrap()
        .inbox_adoption_replay(&req)
        .unwrap()
        .flatten()
        .unwrap();
    assert_eq!(r.goal_ids(), vec![receipt.target.goal_id]);
    assert!(fs::metadata(path).unwrap().len() <= 16 * 1024 * 1024);
}
#[test]
#[ignore = "requires empty OKILUM_INBOX_ADOPTION_COMPAT_FIXTURE directory"]
fn export_inbox_adoption_compatibility_fixture() {
    let target =
        std::path::PathBuf::from(std::env::var_os("OKILUM_INBOX_ADOPTION_COMPAT_FIXTURE").unwrap());
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
    r.source.require_proposal_adoption().unwrap();
    r.open_proposal_adoption().unwrap();
    r.enroll_proposal_inbox_adoption().unwrap();
    let other = goal(&mut r);
    let id = ready(&mut r);
    let req = adopt(&r, &id);
    let next = ready(&mut r);
    let fresh = adopt(&r, &next);
    let cut = std::env::var("OKILUM_INBOX_ADOPTION_COMPAT_CUT").unwrap_or("parent".into());
    match cut.as_str() {
        "parent" => r.proposal_inbox_adoption_fault = Some(Fault::ParentIntent),
        "child" => r.proposal_inbox_adoption_fault = Some(Fault::ChildIntent),
        "source" => r.proposal_inbox_adoption_fault = Some(Fault::ChildSource),
        "child_receipt" => r.proposal_inbox_adoption_fault = Some(Fault::ChildReceipt),
        "parent_receipt" => r.proposal_inbox_adoption_fault = Some(Fault::ParentReceipt),
        "projection" => {
            r.proposal_draft_fault = Some(super::super::proposal_drafts::Fault::AfterProjection)
        }
        "receipt" => {
            r.proposal_draft_fault = Some(super::super::proposal_drafts::Fault::AfterReceipt)
        }
        _ => panic!("unsupported cut"),
    }
    assert!(r.adopt_proposal_inbox(req.clone(), "operator").is_err());
    let op = r.drafts().unwrap().inbox_adoption(&id).unwrap();
    fs::write(target.join("fixture.json"),serde_json::to_vec_pretty(&serde_json::json!({"brain_id":brain,"other_goal_id":other,"proposal_id":id,"request":req,"operation":op,"fresh_request":fresh,"cut":cut})).unwrap()).unwrap();
}
#[test]
fn inbox_adoption_compiled_feature_posture_rejects_fresh_maintenance_only() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = ready(&mut r);
    let req = adopt(&r, &id);
    let before = inventory(&r.root);
    let result = r.adopt_proposal_inbox(req, "operator");
    assert_eq!(result.is_ok(), super::super::inbox_plan::NEW_PLANS_ENABLED);
    if !super::super::inbox_plan::NEW_PLANS_ENABLED {
        assert_eq!(inventory(&r.root), before);
    }
}

#[test]
fn inbox_duplicate_delegated_row_cannot_validate_via_original_child() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = ready(&mut r);
    let req = adopt(&r, &id);
    r.adopt_proposal_inbox(req, "operator").unwrap();
    let child = r
        .drafts()
        .unwrap()
        .inbox_adoption(&id)
        .unwrap()
        .child_operation_id()
        .to_string();
    let next = capture(&mut r);
    start(&mut r, &next, None);
    drop(r);
    let path = f.dir.path().join("state/state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut duplicate = state["inbox_plan_journal"]["operations"][&child].clone();
    let fake = Uuid::new_v4().to_string();
    duplicate["request"]["operation_id"] = fake.clone().into();
    state["inbox_plan_journal"]["operations"][&fake] = duplicate;
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let before = inventory(f.dir.path());
    assert!(Runner::open(f.config()).is_err());
    assert_eq!(inventory(f.dir.path()), before);
}

#[test]
fn terminal_and_inbox_adoption_reserve_both_identities_and_lock_adopted_revision() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = ready(&mut r);
    let expired = request(
        &r,
        &id,
        None,
        api::Disposition::Snoozed {
            until: "2020-01-01T00:00:00Z".into(),
        },
    );
    assert!(matches!(
        r.proposal_disposition_outcome(expired.clone(), "operator")
            .unwrap(),
        api::DeliveryOutcome::NotApplied(_)
    ));
    let mut collision = adopt(&r, &id);
    collision.operation_id = expired.operation_id.clone();
    collision.source = expired.source.clone();
    assert!(r.adopt_proposal_inbox(collision, "operator").is_err());
    let accepted = adopt(&r, &id);
    r.adopt_proposal_inbox(accepted.clone(), "operator")
        .unwrap();
    let child = r
        .drafts()
        .unwrap()
        .inbox_adoption(&id)
        .unwrap()
        .child_operation_id()
        .to_string();
    let other = ready(&mut r);
    for operation in [&accepted.operation_id, &child] {
        let mut collision = request(
            &r,
            &other,
            None,
            api::Disposition::Snoozed {
                until: "2020-01-01T00:00:00Z".into(),
            },
        );
        collision.operation_id = operation.clone();
        assert!(r
            .proposal_disposition_outcome(collision, "operator")
            .is_err());
    }
    let mut collision = request(
        &r,
        &other,
        None,
        api::Disposition::Snoozed {
            until: "2020-01-01T00:00:00Z".into(),
        },
    );
    collision.source = accepted.source.clone();
    assert!(r
        .proposal_disposition_outcome(collision, "operator")
        .is_err());
    let locked = request(
        &r,
        &id,
        None,
        api::Disposition::Snoozed {
            until: "2020-01-01T00:00:00Z".into(),
        },
    );
    let before = inventory(f.dir.path());
    assert!(r.proposal_disposition_outcome(locked, "operator").is_err());
    assert_eq!(before, inventory(f.dir.path()));
    drop(r);
    let path = f.dir.path().join("state/proposal-intents-v1/journal.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut row = value["terminal_dispositions"]
        .as_object_mut()
        .unwrap()
        .remove(&expired.operation_id)
        .unwrap();
    row["request"]["operation_id"] = child.clone().into();
    value["terminal_dispositions"][&child] = row;
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let before = inventory(f.dir.path());
    assert!(Runner::open(f.config()).is_err());
    assert_eq!(before, inventory(f.dir.path()));
}

#[test]
fn expired_adopted_proposal_does_not_enroll_terminal_capability() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = ready(&mut r);
    let accepted = adopt(&r, &id);
    r.adopt_proposal_inbox(accepted.clone(), "operator")
        .unwrap();
    let expired = request(
        &r,
        &id,
        None,
        api::Disposition::Snoozed {
            until: "2020-01-01T00:00:00Z".into(),
        },
    );
    let before = inventory(f.dir.path());
    assert!(r.proposal_disposition_outcome(expired, "operator").is_err());
    assert_eq!(before, inventory(f.dir.path()));
    assert!(!r.source.required_proposal_terminal());
}
