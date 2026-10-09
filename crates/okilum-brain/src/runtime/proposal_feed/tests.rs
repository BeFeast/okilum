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
}
fn request(text: &str) -> CaptureRequest {
    let operation_id = Uuid::new_v4().to_string();
    CaptureRequest {
        operation_id: operation_id.clone(),
        text: text.into(),
        source: SourceIdentity {
            channel: "native".into(),
            instance_id: Uuid::new_v4().to_string(),
            account_id: "local".into(),
            actor_id: "operator".into(),
            chat_id: None,
            topic_id: None,
            message_id: operation_id.clone(),
            update_id: operation_id,
            uri: None,
        },
    }
}
fn capture(r: &mut Runner) -> crate::inbox::Capture {
    r.inbox_capture(request("Original idea"), "operator")
        .unwrap()
}
fn feed(r: &Runner) -> &Journal {
    r.state.proposal_feed.as_ref().unwrap()
}
fn segment_path(r: &Runner, n: u64) -> PathBuf {
    r.state_dir
        .join("proposal-feed-v1")
        .join(format!("{}-{n:020}.json", feed(r).binding.epoch))
}
fn add_goal(r: &mut Runner) -> String {
    let id = Uuid::new_v4().to_string();
    r.create_goal(
        Goal {
            id: id.clone(),
            title: "Feed fixture".into(),
            status: "draft".into(),
            criteria: vec![Criterion {
                id: "C1".into(),
                description: "Observable outcome".into(),
                requires_human: true,
            }],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        },
        "# Goal".into(),
    )
    .unwrap();
    id
}
#[test]
fn producer_enrollment_has_no_backfill_and_replays_exact_inbox_event_after_source_edit() {
    let f = Fixture::new();
    let mut r = f.open();
    capture(&mut r);
    assert!(r.state.proposal_feed.is_none());
    r.enroll_proposal_feed(1).unwrap();
    assert_eq!(feed(&r).published, 0);
    let req = request("New event");
    let first = r.inbox_capture(req.clone(), "operator").unwrap();
    assert_eq!(feed(&r).acknowledged, 1);
    let bytes = fs::read(segment_path(&r, 1)).unwrap();
    let segment: Segment = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(segment.trigger.identity.record_id, first.capture_id);
    assert_eq!(segment.trigger.identity.source_revision, first.revision);
    assert_eq!(segment.trigger.received_at, first.received_at);
    fs::write(r.root.join(&first.path), "External edit").unwrap();
    r.state.proposal_feed.as_mut().unwrap().acknowledged = 0;
    r.persist().unwrap();
    drop(r);
    let mut r = f.open();
    assert_eq!(feed(&r).acknowledged, 1);
    assert_eq!(fs::read(segment_path(&r, 1)).unwrap(), bytes);
    assert!(r.inbox_capture(req, "operator").unwrap().receipt.replayed);
    assert_eq!(feed(&r).published, 1);
}
#[test]
fn consumer_queue_limit_never_blocks_core_captures_and_backlog_survives_restart() {
    let f = Fixture::new();
    let mut r = f.open();
    r.enroll_proposal_feed(1).unwrap();
    for _ in 0..40 {
        assert_eq!(capture(&mut r).receipt.status, "committed");
    }
    assert_eq!(feed(&r).published, 40);
    assert_eq!(feed(&r).spilled, 40);
    assert_eq!(feed(&r).acknowledged, 32);
    assert!(r.state.pending_writes.is_empty());
    drop(r);
    let mut r = f.open();
    assert_eq!(feed(&r).acknowledged, 32);
    capture(&mut r);
    assert_eq!(feed(&r).published, 41);
}
#[test]
fn spool_io_failure_is_optional_and_never_changes_secondary_goal_route() {
    let f = Fixture::new();
    let mut r = f.open();
    let primary = add_goal(&mut r);
    let secondary = add_goal(&mut r);
    r.enroll_proposal_feed(1).unwrap();
    let spool = r.state_dir.join("proposal-feed-v1");
    if spool.exists() {
        fs::remove_dir_all(&spool).unwrap();
    }
    fs::write(&spool, b"IO fault").unwrap();
    r.with_goal(&secondary, |r| {
        capture(r);
        assert_eq!(r.state.goal_id.as_deref(), Some(secondary.as_str()));
        r.attention("decision", "Secondary only");
        r.persist()
    })
    .unwrap();
    assert!(r.proposal_feed_issue.is_some());
    assert_eq!(feed(&r).staged.len(), 1);
    assert_eq!(feed(&r).spilled, 0);
    r.with_goal(&primary, |r| {
        assert!(!r
            .state
            .attention
            .iter()
            .any(|a| a.message == "Secondary only"));
        Ok(())
    })
    .unwrap();
    drop(r);
    let mut r = f.open();
    capture(&mut r);
    assert_eq!(feed(&r).staged.len(), 2);
    fs::remove_file(&spool).unwrap();
    r.flush_writes().unwrap();
    assert_eq!(feed(&r).acknowledged, 2);
}
#[test]
fn optional_checkpoint_failures_replay_immutable_bytes_without_losing_staging() {
    for fault in [Fault::Spill, Fault::Ack, Fault::AfterReplace] {
        let f = Fixture::new();
        let mut r = f.open();
        r.enroll_proposal_feed(1).unwrap();
        r.proposal_feed_fault = Some(fault);
        capture(&mut r);
        assert!(r.proposal_feed_issue.is_some());
        let bytes = fs::read(segment_path(&r, 1)).unwrap();
        if fault != Fault::Ack {
            assert_eq!(feed(&r).spilled, 0);
            assert_eq!(feed(&r).staged.len(), 1);
        }
        // An unrelated core checkpoint may conservatively restore the prior
        // optional cursor even after the replacement had actually succeeded.
        r.persist().unwrap();
        drop(r);
        let mut r = f.open();
        assert_eq!(feed(&r).acknowledged, 1);
        assert_eq!(fs::read(segment_path(&r, 1)).unwrap(), bytes);
        capture(&mut r);
        assert_eq!(feed(&r).acknowledged, 2);
    }
}
#[test]
fn incomplete_capture_checkpoint_recovers_exact_candidate_once() {
    for after_source in [false, true] {
        let f = Fixture::new();
        let mut r = f.open();
        r.enroll_proposal_feed(1).unwrap();
        if after_source {
            r.interrupt_after_write = Some(1);
        } else {
            r.interrupt_after_inbox_intent = true;
        }
        let req = request("Interrupted capture");
        assert!(r.inbox_capture(req.clone(), "operator").is_err());
        assert_eq!(feed(&r).published, 0);
        assert_eq!(feed(&r).candidates.len(), 1);
        let id = feed(&r).candidates[0].trigger.identity.record_id.clone();
        drop(r);
        let mut r = f.open();
        assert_eq!(feed(&r).acknowledged, 1);
        assert_eq!(r.inbox_capture(req, "operator").unwrap().capture_id, id);
        assert_eq!(feed(&r).published, 1);
    }
}
#[test]
fn changed_epoch_header_or_candidate_owner_refuses_before_runner_rewrites_state() {
    for mode in ["header", "candidate", "missing"] {
        let f = Fixture::new();
        let mut r = f.open();
        r.enroll_proposal_feed(1).unwrap();
        r.interrupt_after_inbox_intent = true;
        assert!(r.inbox_capture(request("pending"), "operator").is_err());
        if mode == "candidate" {
            r.state.proposal_feed.as_mut().unwrap().candidates[0]
                .trigger
                .identity
                .record_id = Uuid::new_v4().to_string();
            r.persist().unwrap();
        }
        let state = r.state_dir.join("state.json");
        let header = r.state_dir.join("proposal-intents-v1/journal.json");
        if mode == "header" {
            let mut v: Value = serde_json::from_slice(&fs::read(&header).unwrap()).unwrap();
            v["feed_binding"]["epoch"] = Uuid::new_v4().to_string().into();
            fs::write(&header, serde_json::to_vec(&v).unwrap()).unwrap();
        }
        if mode == "missing" {
            fs::remove_file(header).unwrap();
        }
        let bytes = fs::read(&state).unwrap();
        drop(r);
        assert!(Runner::open(f.config()).is_err());
        assert_eq!(fs::read(state).unwrap(), bytes);
    }
}
#[test]
fn modified_spool_bytes_are_not_accepted_as_exact_replay() {
    let f = Fixture::new();
    let mut r = f.open();
    r.enroll_proposal_feed(1).unwrap();
    capture(&mut r);
    let path = segment_path(&r, 1);
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(b' ');
    fs::write(&path, bytes).unwrap();
    r.state.proposal_feed.as_mut().unwrap().acknowledged = 0;
    r.persist().unwrap();
    r.pump_proposal_feed();
    assert_eq!(feed(&r).acknowledged, 0);
    assert!(r
        .proposal_feed_issue
        .as_ref()
        .unwrap()
        .contains("exact bytes"));
}

#[test]
fn third_ack_failure_retains_second_checkpoint_and_recovers_single_ahead_consumer() {
    let f = Fixture::new();
    let mut r = f.open();
    r.enroll_proposal_feed(1).unwrap();
    let spool = r.state_dir.join("proposal-feed-v1");
    if spool.exists() {
        fs::remove_dir_all(&spool).unwrap();
    }
    fs::write(&spool, b"blocked").unwrap();
    for _ in 0..4 {
        capture(&mut r);
    }
    fs::remove_file(&spool).unwrap();
    r.proposal_feed_fault = Some(Fault::Ack);
    r.proposal_feed_fault_after = 2;
    r.pump_proposal_feed();
    assert!(r.proposal_feed_issue.is_some());
    assert_eq!(feed(&r).acknowledged, 2);
    assert_eq!(feed(&r).spilled, 4);
    assert_eq!(
        Store::inspect_bound(&r.state_dir, &f.brain, &feed(&r).binding).unwrap(),
        3
    );
    r.persist().unwrap();
    drop(r);
    let r = f.open();
    assert_eq!(feed(&r).acknowledged, 4);
}
#[test]
fn preparing_activation_recovers_at_each_checkpoint_with_same_epoch() {
    for cut in 0..=3 {
        let f = Fixture::new();
        let mut r = f.open();
        capture(&mut r);
        let mut binding = RequiredProposalFeed {
            capability: "okilum-proposal-feed/v1".into(),
            epoch: Uuid::new_v4().to_string(),
            activation_id: Uuid::new_v4().to_string(),
            policy_version: 1,
            active: false,
        };
        r.source.require_proposal_feed(binding.clone()).unwrap();
        binding.active = true;
        if cut >= 1 {
            r.state.proposal_feed = Some(Journal {
                binding: binding.clone(),
                published: 0,
                spilled: 0,
                acknowledged: 0,
                candidates: vec![],
                staged: BTreeMap::new(),
            });
            r.persist().unwrap();
        }
        if cut >= 2 {
            Store::initialize_bound(&r.state_dir, &f.brain, 0, Some(binding.clone())).unwrap();
        }
        if cut >= 3 {
            r.source.require_proposal_feed(binding.clone()).unwrap();
        }
        drop(r);
        let mut r = f.open();
        assert_eq!(feed(&r).binding, binding);
        assert_eq!(feed(&r).published, 0);
        capture(&mut r);
        assert_eq!(feed(&r).acknowledged, 1);
    }
}

#[test]
fn enrolled_index_reads_and_rebuilds_without_mutating_feed_or_source_receipts() {
    use crate::retrieval::{BrainIndex, SearchRequest, SearchScope};
    let f = Fixture::new();
    let mut r = f.open();
    let goal = add_goal(&mut r);
    r.enroll_proposal_feed(1).unwrap();
    capture(&mut r);
    r.source
        .write(SourceWrite {
            schema: SCHEMA.into(),
            operation_id: Uuid::new_v4().to_string(),
            brain_id: f.brain.clone(),
            path: "knowledge.md".into(),
            expected_revision: None,
            content_base64: STANDARD
                .encode("# Knowledge\nUniquecompatibleprobe retained original idea"),
        })
        .unwrap();
    let before = serde_json::to_vec(&r.state.proposal_feed).unwrap();
    let bytes = fs::read(segment_path(&r, 1)).unwrap();
    let index = BrainIndex::start(
        f.brain.clone(),
        r.root.clone(),
        "records".into(),
        r.state_dir.clone(),
        true,
    )
    .unwrap();
    for _ in 0..200 {
        if index.status().status == "ready" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(index.status().status, "ready");
    let response = index
        .search(SearchRequest {
            query: "Uniquecompatibleprobe".into(),
            scope: SearchScope {
                goal_id: goal,
                mode: "project".into(),
                path_prefix: None,
                include_paths: vec!["knowledge.md".into()],
                exclude_paths: vec![],
            },
            mode: "lexical".into(),
            limit: 10,
            max_excerpt_bytes: 4096,
        })
        .unwrap();
    assert!(serde_json::to_string(&response)
        .unwrap()
        .contains("knowledge.md"));
    let generation = index.status().generation;
    index.rebuild().unwrap();
    for _ in 0..200 {
        let status = index.status();
        if status.status == "ready" && status.generation != generation {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(index.status().status, "ready");
    assert_ne!(
        index.status().generation,
        generation,
        "forced rebuild must actually publish a generation"
    );
    assert_eq!(serde_json::to_vec(&r.state.proposal_feed).unwrap(), before);
    assert_eq!(fs::read(segment_path(&r, 1)).unwrap(), bytes);
}

/// Exports a disposable enrolled fixture for actual binary compatibility probes.
/// Normal tests never write outside their TempDir or enroll a live workspace.
#[test]
#[ignore = "requires empty OKILUM_PROPOSAL_COMPAT_FIXTURE directory"]
fn export_proposal_compatibility_fixture() {
    let target = PathBuf::from(
        std::env::var("OKILUM_PROPOSAL_COMPAT_FIXTURE").expect("fixture destination required"),
    );
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
    let goal = add_goal(&mut r);
    let stage = Uuid::new_v4().to_string();
    let context = Uuid::new_v4().to_string();
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
            id: context.clone(),
            goal_id: goal.clone(),
            stage_id: stage,
            goal_revision: r.goal_source().unwrap().revision,
            goal: "Preserve original context".into(),
            decisions: vec!["Original decision".into()],
            constraints: vec!["No provider".into()],
            sources: vec![],
            previous_result_id: None,
            next_step: "Fixture only".into(),
            extra: BTreeMap::new(),
        },
        Uuid::new_v4().to_string(),
        BTreeMap::new(),
    )
    .unwrap();
    r.enroll_proposal_feed(1).unwrap();
    let capture = capture(&mut r);
    r.proposal_feed_fault = Some(Fault::Spill);
    let second = r
        .inbox_capture(request("Staged immutable event"), "operator")
        .unwrap();
    r.proposal_feed_fault = Some(Fault::Spill);
    r.interrupt_after_inbox_intent = true;
    assert!(r
        .inbox_capture(request("Pending source candidate"), "operator")
        .is_err());
    assert_eq!(feed(&r).staged.len(), 1);
    assert_eq!(feed(&r).candidates.len(), 1);
    let description = serde_json::json!({"brain_id":brain,"goal_id":goal,"capture_id":capture.capture_id,"capture_path":capture.path,"binding":feed(&r).binding,"staged_capture_id":second.capture_id,"context_id":context,"expected_acknowledged":3});
    fs::write(
        target.join("fixture.json"),
        serde_json::to_vec_pretty(&description).unwrap(),
    )
    .unwrap();
}

#[test]
fn preparing_activation_refuses_corrupt_core_inventory_before_any_state_checkpoint() {
    let f = Fixture::new();
    let mut r = f.open();
    r.source
        .require_proposal_feed(RequiredProposalFeed {
            capability: "okilum-proposal-feed/v1".into(),
            epoch: Uuid::new_v4().to_string(),
            activation_id: Uuid::new_v4().to_string(),
            policy_version: 1,
            active: false,
        })
        .unwrap();
    let path = r.state_dir.join("state.json");
    let before = fs::read(&path).unwrap();
    fs::write(r.state_dir.join("attention-enrollment.json"), b"{}").unwrap();
    drop(r);
    assert!(Runner::open(f.config()).is_err());
    assert_eq!(fs::read(path).unwrap(), before);
}
