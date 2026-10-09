use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_brain::*;
use okilum_core::source::{SourceWrite, WriteBoundary};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
const WHEN: &str = "2026-09-05T12:00:00Z";
fn id(n: u32) -> String {
    format!("01000000-0000-4000-8000-{n:012}")
}
struct Fixture {
    temp: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        fs::create_dir(temp.path().join("runtime")).unwrap();
        Self { temp }
    }
    fn config(&self) -> RunnerConfig {
        RunnerConfig {
            brain_id: id(1),
            root: self.temp.path().join("brain"),
            operational_dir: self.temp.path().join("runtime"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        }
    }
    fn open(&self) -> Runner {
        Runner::open(self.config()).unwrap()
    }
    fn prepared(&self, human: bool) -> Runner {
        self.prepared_with_sources(human, vec![])
    }
    fn prepared_with_sources(&self, human: bool, sources: Vec<SourceRef>) -> Runner {
        let mut r = self.open();
        r.create_goal(Goal {id:id(2),title:"Explain a change".into(),status:"draft".into(),criteria:vec![Criterion {id:"C1".into(),description:"A sourced result exists".into(),requires_human:human}],stage_ids:vec![],task_ref:Some(json!({"provider":"todoist","external_id":"task-1","observed_status":"open","observed_at":WHEN})),extra:BTreeMap::new()},"\n# Goal\noriginal body\n".into()).unwrap();
        let revision = r.goal_source().unwrap().revision;
        r.prepare_stage(
            Stage {
                id: id(3),
                goal_id: id(2),
                engine: "t3".into(),
                status: "ready".into(),
                criterion_ids: vec!["C1".into()],
                context_id: id(4),
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            ContextPacket {
                id: id(4),
                goal_id: id(2),
                stage_id: id(3),
                goal_revision: revision,
                goal: "Explain a change".into(),
                decisions: vec![],
                constraints: vec!["Do not change code".into()],
                sources,
                previous_result_id: None,
                next_step: "Read and explain".into(),
                extra: BTreeMap::new(),
            },
            id(5),
            BTreeMap::new(),
        )
        .unwrap();
        r
    }
}
fn binding() -> EngineRef {
    EngineRef {
        engine: "t3".into(),
        instance_id: "test-instance".into(),
        thread_id: Some("thread-1".into()),
        turn_id: Some("turn-1".into()),
        task_id: None,
    }
}
struct Fake {
    starts: Arc<AtomicUsize>,
    lose_reply: bool,
    reject: bool,
    events: Vec<EngineEvent>,
    unknown: bool,
    catchup_error: bool,
}
impl Fake {
    fn new() -> Self {
        Self {
            starts: Arc::new(AtomicUsize::new(0)),
            lose_reply: false,
            reject: false,
            events: vec![],
            unknown: false,
            catchup_error: false,
        }
    }
}
impl Adapter for Fake {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: "t3".into(),
            cancel: false,
        }
    }
    fn start(&mut self, _: &StartEnvelope) -> anyhow::Result<StartReply> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        if self.reject {
            return Ok(StartReply::Rejected {
                reason: "proven not started".into(),
            });
        }
        if self.lose_reply {
            anyhow::bail!("reply lost after external work started")
        };
        Ok(StartReply::Accepted { binding: binding() })
    }
    fn observe(
        &mut self,
        _: &EngineRef,
        _: &BTreeMap<String, String>,
    ) -> anyhow::Result<Vec<EngineEvent>> {
        Ok(std::mem::take(&mut self.events))
    }
    fn reconcile(
        &mut self,
        _: &StartEnvelope,
        _: Option<&EngineRef>,
    ) -> anyhow::Result<ReconcileReply> {
        if self.catchup_error {
            anyhow::bail!("t3_snapshot_changed_during_catchup");
        }
        if self.unknown {
            Ok(ReconcileReply::Unknown {
                reason: "provider cannot establish identity".into(),
            })
        } else {
            Ok(ReconcileReply::Running {
                binding: binding(),
                evidence: "provider exact operation mapping".into(),
            })
        }
    }
}
fn event(_r: &Runner, seq: u64, payload: EventPayload) -> EngineEvent {
    EngineEvent {
        operation_id: id(5),
        engine_ref: binding(),
        event_id: format!("event-{seq}"),
        stream_id: "thread-1/turn-1".into(),
        sequence: Some(seq),
        cursor: Some(seq.to_string()),
        observed_at: WHEN.into(),
        payload,
    }
}
fn outcome(r: &Runner) -> Outcome {
    Outcome {
        outcome: "succeeded".into(),
        summary: "Sourced explanation".into(),
        sources: vec![],
        evidence: vec![Evidence {
            id: "E1".into(),
            kind: "artifact".into(),
            source: SourceRef {
                uri: "fixture://explanation.md".into(),
                revision: Some("sha256:fixture".into()),
                locator: None,
            },
            description: "Explanation exists".into(),
            observed_at: WHEN.into(),
            status: "passed".into(),
        }],
        verification: "verified".into(),
        criterion_evaluations: vec![CriterionEvaluation {
            criterion_id: "C1".into(),
            goal_revision: r.snapshot().unwrap().dispatch.unwrap().packet.goal_revision,
            status: "passed".into(),
            evidence_ids: vec!["E1".into()],
            evaluated_by: "fixture verifier".into(),
            evaluated_at: WHEN.into(),
        }],
    }
}
fn replace_goal(r: &mut Runner, edit: impl FnOnce(&mut serde_yaml::Mapping, &mut String)) {
    let s = r.goal_source().unwrap();
    let text = String::from_utf8(STANDARD.decode(&s.content_base64).unwrap()).unwrap();
    let (front, body) = text
        .strip_prefix("---\n")
        .unwrap()
        .split_once("\n---\n")
        .unwrap();
    let mut map: serde_yaml::Mapping = serde_yaml::from_str(front).unwrap();
    let mut body = body.to_string();
    edit(&mut map, &mut body);
    r.write_source(SourceWrite {
        schema: SCHEMA.into(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        brain_id: id(1),
        path: s.path,
        expected_revision: Some(s.revision),
        content_base64: STANDARD.encode(format!(
            "---\n{}---\n{}",
            serde_yaml::to_string(&map).unwrap(),
            body
        )),
    })
    .unwrap();
}

#[test]
fn lost_start_reply_restarts_with_same_operation_and_reconciles_without_redispatch() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut a = Fake::new();
    a.lose_reply = true;
    assert_eq!(
        r.start(&mut a).unwrap().phase.as_deref(),
        Some("indeterminate")
    );
    assert!(r.start(&mut a).is_err());
    assert_eq!(a.starts.load(Ordering::SeqCst), 1);
    drop(r);
    let mut r = f.open();
    assert_eq!(r.snapshot().unwrap().dispatch.unwrap().operation_id, id(5));
    r.reconcile(&mut a).unwrap();
    assert_eq!(r.snapshot().unwrap().binding, Some(binding()));
    assert_eq!(a.starts.load(Ordering::SeqCst), 1);
    a.events
        .push(event(&r, 1, EventPayload::Outcome(outcome(&r))));
    assert_eq!(r.poll(&mut a).unwrap().goal.unwrap().status, "completed");
}
#[test]
fn uncertain_reconciliation_remains_blocked_and_does_not_start() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut a = Fake::new();
    a.lose_reply = true;
    a.unknown = true;
    r.start(&mut a).unwrap();
    r.reconcile(&mut a).unwrap();
    assert!(r.start(&mut a).is_err());
    assert_eq!(r.snapshot().unwrap().stage.unwrap().status, "blocked");
    assert_eq!(a.starts.load(Ordering::SeqCst), 1);
}
#[test]
fn replay_unbound_and_late_status_cannot_duplicate_result_or_regress_completion() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    let done = event(&r, 5, EventPayload::Outcome(outcome(&r)));
    let first = r.ingest(done.clone()).unwrap();
    assert_eq!(r.ingest(done).unwrap().event_count, first.event_count);
    for (seq, state) in [(4, "running"), (6, "blocked"), (7, "cancelled")] {
        r.ingest(event(
            &r,
            seq,
            EventPayload::Status {
                state: state.into(),
            },
        ))
        .unwrap();
    }
    let mut old = event(&r, 8, EventPayload::Outcome(outcome(&r)));
    old.engine_ref.turn_id = Some("older-turn".into());
    r.ingest(old).unwrap();
    let s = r.snapshot().unwrap();
    assert_eq!(s.stage.unwrap().status, "completed");
    assert_eq!(s.goal.unwrap().status, "completed");
    assert_eq!(s.phase.as_deref(), Some("outcome_ready"));
    assert_eq!(
        s.attention.len(),
        first.attention.len(),
        "late ignored status must not create a new blocker"
    );
    r.reconcile(&mut a).unwrap();
    assert_eq!(
        r.snapshot().unwrap().phase.as_deref(),
        Some("outcome_ready")
    );
    assert_eq!(
        fs::read_dir(f.temp.path().join("brain/records"))
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("result-"))
            .count(),
        1
    );
}
#[test]
fn failed_ingest_does_not_poison_event_identity_before_durable_commit() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    let mut invalid = outcome(&r);
    invalid.outcome = "not-a-status".into();
    assert!(r
        .ingest(event(&r, 1, EventPayload::Outcome(invalid)))
        .is_err());
    assert_eq!(r.snapshot().unwrap().event_count, 0);
    assert_eq!(
        r.ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
            .unwrap()
            .goal
            .unwrap()
            .status,
        "completed"
    );
}
#[test]
fn engine_success_without_evidence_and_empty_or_changed_criteria_never_complete() {
    for variant in ["missing", "empty", "changed"] {
        let f = Fixture::new();
        let mut r = f.prepared(false);
        let mut a = Fake::new();
        r.start(&mut a).unwrap();
        let mut result = outcome(&r);
        if variant == "missing" {
            result.evidence.clear();
        }
        if variant == "empty" {
            replace_goal(&mut r, |m, _| {
                m.insert("criteria".into(), serde_yaml::Value::Sequence(vec![]));
            });
        }
        if variant == "changed" {
            replace_goal(&mut r, |m, _| {
                m.get_mut("criteria").unwrap().as_sequence_mut().unwrap()[0]["description"] =
                    "Different requirement".into();
            });
        }
        assert_ne!(
            r.ingest(event(&r, 1, EventPayload::Outcome(result)))
                .unwrap()
                .goal
                .unwrap()
                .status,
            "completed",
            "{variant}"
        );
    }
}
#[test]
fn human_receipt_is_canonical_and_reevaluates_existing_result_without_another_engine_turn() {
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    let s = r
        .ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    assert_ne!(s.goal.unwrap().status, "completed");
    let result_id = s.stage.unwrap().result_ids[0].clone();
    let s = r
        .accept_human(
            "C1".into(),
            "test operator".into(),
            WHEN.into(),
            SourceRef {
                uri: "fixture://actual-review".into(),
                revision: None,
                locator: None,
            },
        )
        .unwrap();
    assert_eq!(s.goal.unwrap().status, "completed");
    assert_eq!(s.stage.unwrap().result_ids, vec![result_id.clone()]);
    assert_eq!(a.starts.load(Ordering::SeqCst), 1);
    assert!(r
        .result(&result_id)
        .unwrap()
        .outcome
        .evidence
        .iter()
        .any(|e| e.kind == "human_acceptance"));
    assert_eq!(
        fs::read_dir(f.temp.path().join("brain/records"))
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("evidence-"))
            .count(),
        1
    );
    drop(r);
    assert_eq!(
        f.open().snapshot().unwrap().goal.unwrap().status,
        "completed"
    );
}
#[test]
fn goal_projection_preserves_unknown_frontmatter_and_body_newlines() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    let body = "# Kept\r\n\r\n[[target|alias]]\r\n";
    replace_goal(&mut r, |m, b| {
        m.insert(
            "custom_nested".into(),
            serde_yaml::from_str("owner: user\nflag: true\n").unwrap(),
        );
        *b = body.into();
    });
    r.ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    let text = String::from_utf8(
        STANDARD
            .decode(r.goal_source().unwrap().content_base64)
            .unwrap(),
    )
    .unwrap();
    assert!(text.ends_with(body));
    assert!(text.contains("custom_nested:"));
    assert!(text.contains("owner: user"));
    // Mechanical status/result reference changes do not invalidate accepted criteria.
    assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "completed");
}
#[test]
fn runtime_is_single_owner_and_missing_journal_is_not_permission_to_repeat_work() {
    let f = Fixture::new();
    let r = f.prepared(false);
    assert!(Runner::open(f.config()).is_err());
    drop(r);
    fs::remove_file(f.temp.path().join("runtime/state.json")).unwrap();
    assert!(Runner::open(f.config()).is_err());
}

#[test]
fn changing_or_removing_criteria_after_completion_invalidates_the_displayed_receipt() {
    for empty in [false, true] {
        let f = Fixture::new();
        let mut r = f.prepared(false);
        let mut a = Fake::new();
        r.start(&mut a).unwrap();
        let done = r
            .ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
            .unwrap();
        let result_id = done.stage.unwrap().result_ids[0].clone();
        assert_eq!(done.goal.unwrap().status, "completed");
        replace_goal(&mut r, |map, _| {
            if empty {
                map.insert("criteria".into(), serde_yaml::Value::Sequence(vec![]));
            } else {
                map.get_mut("criteria").unwrap().as_sequence_mut().unwrap()[0]["description"] =
                    "A new criterion".into();
            }
        });
        let invalidated = r.snapshot().unwrap();
        assert_eq!(invalidated.goal.unwrap().status, "blocked");
        assert!(!invalidated.attention.iter().any(|a| a.kind == "final"));
        assert!(invalidated
            .attention
            .iter()
            .any(|a| a.message.starts_with("Recorded completion no longer")));
        assert_eq!(
            r.result(&result_id).unwrap().outcome.verification,
            "verified",
            "historical receipt is preserved"
        );
        drop(r);
        assert_eq!(f.open().snapshot().unwrap().goal.unwrap().status, "blocked");
    }
}

#[test]
fn completed_attention_projects_current_criteria_without_erasing_history() {
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    r.ingest(event(
        &r,
        1,
        EventPayload::Status {
            state: "blocked".into(),
        },
    ))
    .unwrap();
    r.ingest(event(
        &r,
        2,
        EventPayload::Attention {
            message: "Check a separate constraint".into(),
        },
    ))
    .unwrap();
    let waiting = r
        .ingest(event(&r, 3, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    assert!(waiting
        .attention
        .iter()
        .any(|a| a.message == "outcome saved; goal criteria remain unmet"));
    let completed = r
        .accept_human(
            "C1".into(),
            "test operator".into(),
            WHEN.into(),
            SourceRef {
                uri: "fixture://actual-review".into(),
                revision: None,
                locator: None,
            },
        )
        .unwrap();
    assert_eq!(completed.goal.unwrap().status, "completed");
    assert!(!completed
        .attention
        .iter()
        .any(|a| a.message == "outcome saved; goal criteria remain unmet"));
    assert!(completed.attention.iter().any(|a| a.kind == "final"));
    assert!(!completed
        .attention
        .iter()
        .any(|a| a.kind == "blocker" && a.message == "engine needs attention"));
    assert!(completed
        .attention
        .iter()
        .any(|a| a.message == "Check a separate constraint"));
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(f.temp.path().join("runtime/state.json")).unwrap())
            .unwrap();
    assert!(journal["attention"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["message"] == "outcome saved; goal criteria remain unmet"));
    drop(r);
    let mut r = f.open();
    assert!(!r
        .snapshot()
        .unwrap()
        .attention
        .iter()
        .any(|a| a.message == "outcome saved; goal criteria remain unmet"));
    // Explicitly reopening the goal also invalidates the final attention, even
    // though historical evidence and the completed stage are unchanged.
    replace_goal(&mut r, |map, _| {
        map.insert("status".into(), "active".into());
    });
    let reopened = r.snapshot().unwrap();
    assert_eq!(reopened.stage.unwrap().status, "completed");
    assert!(!reopened.attention.iter().any(|a| a.kind == "final"));
    assert!(reopened
        .attention
        .iter()
        .any(|a| a.message == "outcome saved; goal criteria remain unmet"));
    assert!(!reopened.attention.iter().any(|a| a.kind == "blocker"));
    assert!(reopened
        .attention_history
        .iter()
        .any(|a| a.kind == "blocker"));
}

#[test]
fn record_id_substitution_through_source_api_is_rejected() {
    for kind in ["goal", "stage"] {
        let f = Fixture::new();
        let mut r = f.prepared(false);
        let path = format!(
            "records/{kind}-{}.md",
            if kind == "goal" { id(2) } else { id(3) }
        );
        let source = r.read_source(&path).unwrap();
        let original = String::from_utf8(STANDARD.decode(&source.content_base64).unwrap()).unwrap();
        let old_id = if kind == "goal" { id(2) } else { id(3) };
        let changed = original.replace(&format!("id: {old_id}\n"), &format!("id: {}\n", id(90)));
        assert_ne!(original, changed);
        r.write_source(SourceWrite {
            schema: SCHEMA.into(),
            operation_id: uuid::Uuid::new_v4().to_string(),
            brain_id: id(1),
            path,
            expected_revision: Some(source.revision),
            content_base64: STANDARD.encode(changed),
        })
        .unwrap();
        assert!(
            r.snapshot()
                .unwrap_err()
                .to_string()
                .contains("identity mismatch"),
            "{kind}"
        );
    }
}

#[test]
fn unbound_outcome_replay_after_reconciliation_projects_same_intake() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut a = Fake::new();
    a.lose_reply = true;
    r.start(&mut a).unwrap();
    let done = event(&r, 1, EventPayload::Outcome(outcome(&r)));
    let first = r.ingest(done.clone()).unwrap();
    assert_eq!(first.event_count, 1);
    assert!(first.stage.unwrap().result_ids.is_empty());
    drop(r);
    let mut r = f.open();
    r.reconcile(&mut a).unwrap();
    let recovered = r.ingest(done.clone()).unwrap();
    assert_eq!(recovered.event_count, 1);
    assert_eq!(recovered.goal.unwrap().status, "completed");
    let result_ids = recovered.stage.unwrap().result_ids;
    assert_eq!(result_ids.len(), 1);
    assert_eq!(
        r.ingest(done).unwrap().stage.unwrap().result_ids,
        result_ids
    );
    assert_eq!(a.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn human_acceptance_retains_receipt_without_reviving_cancelled_stage() {
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    r.ingest(event(
        &r,
        1,
        EventPayload::Status {
            state: "cancelled".into(),
        },
    ))
    .unwrap();
    let late = r
        .ingest(event(&r, 2, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    assert_eq!(late.stage.as_ref().unwrap().status, "cancelled");
    let result_id = late.stage.unwrap().result_ids[0].clone();
    let reviewed = r
        .accept_human(
            "C1".into(),
            "test operator".into(),
            WHEN.into(),
            SourceRef {
                uri: "fixture://actual-review".into(),
                revision: None,
                locator: None,
            },
        )
        .unwrap();
    assert_eq!(reviewed.stage.unwrap().status, "cancelled");
    assert_ne!(reviewed.goal.unwrap().status, "completed");
    assert_eq!(reviewed.phase.as_deref(), Some("cancelled"));
    assert!(r
        .result(&result_id)
        .unwrap()
        .outcome
        .evidence
        .iter()
        .any(|e| e.kind == "human_acceptance"));
    drop(r);
    assert_eq!(
        f.open().snapshot().unwrap().stage.unwrap().status,
        "cancelled"
    );
}

#[test]
fn human_receipt_for_another_goal_cannot_verify_current_goal() {
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    let done = r
        .ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    let result_id = done.stage.unwrap().result_ids[0].clone();
    r.accept_human(
        "C1".into(),
        "test operator".into(),
        WHEN.into(),
        SourceRef {
            uri: "fixture://review".into(),
            revision: None,
            locator: None,
        },
    )
    .unwrap();
    assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "completed");
    let result = r.result(&result_id).unwrap();
    let evidence = result
        .outcome
        .evidence
        .iter()
        .find(|e| e.kind == "human_acceptance")
        .unwrap();
    let path = evidence
        .source
        .uri
        .strip_prefix(&format!("brain://{}/", id(1)))
        .unwrap();
    let source = r.read_source(path).unwrap();
    let original = String::from_utf8(STANDARD.decode(&source.content_base64).unwrap()).unwrap();
    let changed = original.replace(
        &format!("goal_id: {}", id(2)),
        &format!("goal_id: {}", id(91)),
    );
    assert_ne!(original, changed);
    r.write_source(SourceWrite {
        schema: SCHEMA.into(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        brain_id: id(1),
        path: source.path,
        expected_revision: Some(source.revision),
        content_base64: STANDARD.encode(changed),
    })
    .unwrap();
    assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "blocked");
}

#[test]
fn explicit_saved_evidence_review_completes_nonhuman_criterion_and_renders_result() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    let mut unverified = outcome(&r);
    unverified.criterion_evaluations.clear();
    unverified.evidence[0].status = "unverified".into();
    let snap = r
        .ingest(event(&r, 1, EventPayload::Outcome(unverified)))
        .unwrap();
    assert_ne!(snap.goal.unwrap().status, "completed");
    let result_id = snap.stage.unwrap().result_ids[0].clone();
    let evaluation = CriterionEvaluation {
        criterion_id: "C1".into(),
        goal_revision: String::new(),
        status: "passed".into(),
        evidence_ids: vec!["E1".into()],
        evaluated_by: "actual reviewer".into(),
        evaluated_at: WHEN.into(),
    };
    let mut invalid = evaluation.clone();
    invalid.evidence_ids = vec!["invented".into()];
    assert!(r.evaluate_result(result_id.clone(), invalid).is_err());
    assert_ne!(r.snapshot().unwrap().goal.unwrap().status, "completed");
    let completed = r
        .evaluate_result(result_id.clone(), evaluation.clone())
        .unwrap();
    assert_eq!(completed.goal.unwrap().status, "completed");
    assert!(completed
        .attention
        .iter()
        .any(|a| a.kind == "final" && a.message == "result review recorded"));
    assert!(!completed
        .attention
        .iter()
        .any(|a| a.message == "outcome saved; goal criteria remain unmet"));
    assert_eq!(
        r.result(&result_id).unwrap().outcome.verification,
        "verified"
    );
    let source = r
        .read_source(&format!("records/result-{result_id}.md"))
        .unwrap();
    let text = String::from_utf8(STANDARD.decode(source.content_base64).unwrap()).unwrap();
    let body = text.split("\n---\n").nth(1).unwrap();
    assert!(body.contains("Sourced explanation"));
    assert!(body.contains("fixture://explanation.md"));
    assert!(body.contains("Verification: verified"));
    let mut failed = evaluation;
    failed.status = "failed".into();
    let reopened = r.evaluate_result(result_id, failed).unwrap();
    assert_eq!(reopened.goal.unwrap().status, "active");
    assert!(!reopened.attention.iter().any(|a| a.kind == "final"));
    assert!(reopened
        .attention
        .iter()
        .any(|a| a.kind == "decision" && a.message == "result review recorded"));
}

#[test]
fn artifact_review_cannot_satisfy_human_criterion() {
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    let result = r
        .ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
        .unwrap()
        .stage
        .unwrap()
        .result_ids[0]
        .clone();
    assert!(r
        .evaluate_result(
            result,
            CriterionEvaluation {
                criterion_id: "C1".into(),
                goal_revision: String::new(),
                status: "passed".into(),
                evidence_ids: vec!["E1".into()],
                evaluated_by: "reviewer".into(),
                evaluated_at: WHEN.into()
            }
        )
        .is_err());
    assert_ne!(r.snapshot().unwrap().goal.unwrap().status, "completed");
}

fn second_goal(r: &mut Runner) {
    r.create_goal(
        Goal {
            id: id(20),
            title: "Second thought".into(),
            status: "draft".into(),
            criteria: vec![Criterion {
                id: "C1".into(),
                description: "Separate outcome".into(),
                requires_human: true,
            }],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        },
        "\n# Second thought\n".into(),
    )
    .unwrap();
}
fn followup(
    r: &mut Runner,
    previous: Option<String>,
    goal: u32,
    stage: u32,
) -> anyhow::Result<Snapshot> {
    r.with_goal(&id(goal), |r| {
        let revision = r.goal_source()?.revision;
        r.prepare_stage(
            Stage {
                id: id(stage),
                goal_id: id(goal),
                engine: "t3".into(),
                status: "ready".into(),
                criterion_ids: vec!["C1".into()],
                context_id: id(stage + 1),
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            ContextPacket {
                id: id(stage + 1),
                goal_id: id(goal),
                stage_id: id(stage),
                goal_revision: revision,
                goal: "Follow up".into(),
                decisions: vec![],
                constraints: vec![],
                sources: vec![],
                previous_result_id: previous,
                next_step: "Review next step".into(),
                extra: BTreeMap::new(),
            },
            id(stage + 2),
            BTreeMap::new(),
        )
    })
}
#[test]
fn two_goals_route_replay_and_late_results_without_changing_default_goal() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    second_goal(&mut r);
    followup(&mut r, None, 20, 30).unwrap();
    let first_event = event(&r, 1, EventPayload::Outcome(outcome(&r)));
    r.with_goal(&id(20), |r| {
        assert_eq!(r.goal_ids(), vec![id(2), id(20)]);
        r.start(&mut a)?;
        // The event is explicitly routed to its original dispatch even while
        // servicing another goal. No selected UI goal participates in ownership.
        assert_eq!(r.ingest(first_event.clone())?.goal.unwrap().id, id(2));
        assert_eq!(r.snapshot()?.goal.unwrap().id, id(20));
        assert!(r.snapshot()?.stage.unwrap().result_ids.is_empty());
        Ok(())
    })
    .unwrap();
    assert_eq!(r.snapshot().unwrap().goal.unwrap().id, id(2));
    assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "completed");
    let original_result = r.snapshot().unwrap().stage.unwrap().result_ids[0].clone();
    drop(r);
    let mut r = f.open();
    assert_eq!(r.goals().unwrap().len(), 2);
    r.with_goal(&id(20), |r| {
        assert_eq!(r.snapshot()?.phase.as_deref(), Some("indeterminate"));
        r.reconcile(&mut a)?;
        assert_eq!(r.snapshot()?.dispatch.unwrap().operation_id, id(32));
        Ok(())
    })
    .unwrap();
    r.ingest(first_event).unwrap();
    assert_eq!(
        r.snapshot().unwrap().stage.unwrap().result_ids,
        vec![original_result]
    );
    assert_eq!(a.starts.load(Ordering::SeqCst), 2);
}
#[test]
fn followup_preserves_review_and_requires_new_acceptance_for_new_result() {
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    assert!(
        followup(&mut r, None, 2, 30).is_err(),
        "active stage forbids successor"
    );
    r.ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    r.accept_human(
        "C1".into(),
        "Oleg".into(),
        WHEN.into(),
        SourceRef {
            uri: "fixture://actual-review".into(),
            revision: None,
            locator: None,
        },
    )
    .unwrap();
    let original = r.snapshot().unwrap();
    let result_id = original.stage.unwrap().result_ids[0].clone();
    let result_bytes = fs::read(
        f.temp
            .path()
            .join(format!("brain/records/result-{result_id}.md")),
    )
    .unwrap();
    let context_bytes = fs::read(
        f.temp
            .path()
            .join(format!("brain/records/context-{}.md", id(4))),
    )
    .unwrap();
    assert!(
        followup(&mut r, None, 2, 30).is_err(),
        "must explicitly name predecessor result"
    );
    followup(&mut r, Some(result_id.clone()), 2, 30).unwrap();
    assert_eq!(
        r.snapshot().unwrap().goal.unwrap().stage_ids,
        vec![id(3), id(30)]
    );
    assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "active");
    r.start(&mut a).unwrap();
    let mut next = event(&r, 2, EventPayload::Outcome(outcome(&r)));
    next.operation_id = id(32);
    r.ingest(next).unwrap();
    assert_eq!(
        r.snapshot().unwrap().goal.unwrap().status,
        "active",
        "prior human review cannot accept a different result"
    );
    let successor = r.snapshot().unwrap().stage.unwrap().result_ids;
    let mut late = event(&r, 3, EventPayload::Outcome(outcome(&r)));
    late.event_id = "late-prior-terminal".into();
    r.ingest(late).unwrap();
    assert_eq!(r.snapshot().unwrap().stage.unwrap().result_ids, successor);
    assert_eq!(
        fs::read(
            f.temp
                .path()
                .join(format!("brain/records/result-{result_id}.md"))
        )
        .unwrap(),
        result_bytes
    );
    assert_eq!(
        fs::read(
            f.temp
                .path()
                .join(format!("brain/records/context-{}.md", id(4)))
        )
        .unwrap(),
        context_bytes
    );
    drop(r);
    let r = f.open();
    assert_eq!(r.stages().unwrap().len(), 2);
    assert_eq!(
        r.result(&result_id).unwrap().outcome.verification,
        "verified"
    );
}
#[test]
fn attention_is_scoped_to_stage_and_legacy_unrelated_decisions_survive() {
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    r.ingest(event(
        &r,
        1,
        EventPayload::Status {
            state: "blocked".into(),
        },
    ))
    .unwrap();
    r.ingest(event(
        &r,
        2,
        EventPayload::Attention {
            message: "Review this stage".into(),
        },
    ))
    .unwrap();
    r.ingest(event(&r, 3, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    let result_id = r.snapshot().unwrap().stage.unwrap().result_ids[0].clone();
    drop(r);
    // Emulate an older journal: no ownership on known lifecycle alerts and a
    // separate legacy decision whose relationship to any stage is unknown.
    let path = f.temp.path().join("runtime/state.json");
    let mut journal: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let known_ids: Vec<_> = journal["attention"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["message"] != "Review this stage")
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    for id in known_ids {
        journal["attention_stage_ids"]
            .as_object_mut()
            .unwrap()
            .remove(&id);
    }
    journal["attention"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"legacy-other","kind":"decision","message":"Unrelated legacy decision"}));
    fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let mut r = f.open();
    followup(&mut r, Some(result_id), 2, 30).unwrap();
    let next = r.snapshot().unwrap();
    assert_eq!(next.attention.len(), 1);
    assert_eq!(next.attention[0].message, "Unrelated legacy decision");
    assert!(next
        .attention_history
        .iter()
        .any(|a| a.message == "Review this stage"));
    assert!(next
        .attention_history
        .iter()
        .any(|a| a.message == "engine needs attention"));
    r.start(&mut a).unwrap();
    let mut again = event(
        &r,
        4,
        EventPayload::Status {
            state: "blocked".into(),
        },
    );
    again.operation_id = id(32);
    r.ingest(again).unwrap();
    assert!(r
        .snapshot()
        .unwrap()
        .attention
        .iter()
        .any(|a| a.message == "engine needs attention"));
    drop(r);
    let next = f.open().snapshot().unwrap();
    assert_eq!(
        next.attention_history
            .iter()
            .filter(|a| a.message == "engine needs attention")
            .count(),
        2
    );
    assert!(next
        .attention
        .iter()
        .any(|a| a.message == "Unrelated legacy decision"));
    assert!(!next
        .attention
        .iter()
        .any(|a| a.message == "outcome saved; goal criteria remain unmet"));
}

#[test]
fn legacy_journal_adaptation_preserves_ids_human_evidence_and_read_only_routing() {
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let mut a = Fake::new();
    r.start(&mut a).unwrap();
    r.ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    r.accept_human(
        "C1".into(),
        "Oleg".into(),
        WHEN.into(),
        SourceRef {
            uri: "fixture://review".into(),
            revision: None,
            locator: None,
        },
    )
    .unwrap();
    let original = r.snapshot().unwrap();
    let result_id = original.stage.as_ref().unwrap().result_ids[0].clone();
    let receipt = serde_json::to_value(r.result(&result_id).unwrap()).unwrap();
    drop(r);
    let journal = f.temp.path().join("runtime/state.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    for key in [
        "primary_goal_id",
        "other_goals",
        "previous_stages",
        "prepared_changes",
    ] {
        value.as_object_mut().unwrap().remove(key);
    }
    fs::write(&journal, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut r = f.open();
    assert_eq!(
        serde_json::to_value(r.result(&result_id).unwrap()).unwrap(),
        receipt
    );
    assert_eq!(r.snapshot().unwrap().binding, original.binding);
    second_goal(&mut r);
    let bytes = fs::read(&journal).unwrap();
    r.with_goal(&id(20), |r| {
        assert_eq!(r.snapshot()?.goal.unwrap().id, id(20));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        fs::read(&journal).unwrap(),
        bytes,
        "selection must not persist execution changes"
    );
    assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "completed");
    assert_eq!(a.starts.load(Ordering::SeqCst), 1);
}

fn prepared_change(r: &Runner, number: u32, change: PreparedChange) -> PreparedChangeRequest {
    let snapshot = r.snapshot().unwrap();
    PreparedChangeRequest {
        operation_id: id(number),
        goal_id: snapshot.goal.unwrap().id,
        expected: snapshot.prepared_guard.unwrap(),
        change,
    }
}

#[test]
fn prepared_revision_preserves_frozen_sources_and_replays_after_restart_and_start() {
    let f = Fixture::new();
    let sources = vec![SourceRef {
        uri: "brain:sources/engineering.md".into(),
        revision: Some("sha256:frozen-engineering-source".into()),
        locator: Some("Decision".into()),
    }];
    let mut r = f.prepared_with_sources(true, sources);
    let before = r.snapshot().unwrap();
    let old = before.dispatch.unwrap();
    let old_context = r
        .read_source(&format!("records/context-{}.md", old.context_id))
        .unwrap();
    let request = prepared_change(
        &r,
        98,
        PreparedChange::Revise {
            next_step: "Read the complete instruction, включая окончание.".into(),
        },
    );
    let receipt = r.change_prepared(request.clone()).unwrap();
    let now = r.snapshot().unwrap();
    assert!(now.can_change_prepared);
    assert!(now.requires_guarded_start);
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(f.temp.path().join("runtime/state.json")).unwrap())
            .unwrap();
    assert_eq!(journal["dispatch"]["phase"], "prepared_edited");
    let legacy_startable = |phase: &str| ["prepared", "not_started"].contains(&phase);
    assert!(
        legacy_startable("prepared"),
        "positive control for the old allowlist"
    );
    assert!(!legacy_startable(
        journal["dispatch"]["phase"].as_str().unwrap()
    ));
    assert_eq!(now.prepared_guard, receipt.replacement);
    let replacement = now.dispatch.unwrap();
    assert_ne!(replacement.stage_id, old.stage_id);
    assert_ne!(replacement.context_id, old.context_id);
    assert_ne!(replacement.operation_id, old.operation_id);
    let mut expected_packet = old.packet.clone();
    expected_packet.id = replacement.context_id.clone();
    expected_packet.stage_id = replacement.stage_id.clone();
    expected_packet.next_step = "Read the complete instruction, включая окончание.".into();
    assert_eq!(replacement.packet, expected_packet);
    assert_eq!(replacement.target, old.target);
    let mut expected_goal = before.goal.unwrap();
    expected_goal.stage_ids.push(replacement.stage_id.clone());
    assert_eq!(now.goal.unwrap(), expected_goal);
    assert_eq!(r.read_source(&old_context.path).unwrap(), old_context);
    let stages = r.stages().unwrap();
    assert_eq!(stages.len(), 2);
    assert_eq!(stages[0].status, "cancelled");
    assert_eq!(stages[0].extra["prepared_disposition"], "superseded");
    assert_eq!(stages[1].criterion_ids, stages[0].criterion_ids);
    assert_eq!(r.change_prepared(request.clone()).unwrap(), receipt);
    let mut changed = request.clone();
    changed.change = PreparedChange::Discard;
    assert!(r.change_prepared(changed).is_err());
    drop(r);
    let mut r = f.open();
    assert_eq!(r.change_prepared(request.clone()).unwrap(), receipt);
    let mut adapter = Fake::new();
    assert!(
        r.start(&mut adapter).is_err(),
        "legacy Start must fail closed after revisions"
    );
    assert!(r
        .start_expected(&mut adapter, Some(request.expected.clone()))
        .is_err());
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 0);
    assert!(
        r.reconcile(&mut adapter).is_err(),
        "reconciliation cannot turn an undispatched revision into running"
    );
    r.start_expected(&mut adapter, receipt.replacement.clone())
        .unwrap();
    assert_eq!(
        r.change_prepared(request).unwrap(),
        receipt,
        "lost edit ack remains reconcilable after Start"
    );
    assert_eq!(r.snapshot().unwrap().phase.as_deref(), Some("running"));
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn discarded_initial_and_followup_keep_history_and_can_prepare_again() {
    for has_previous in [false, true] {
        let f = Fixture::new();
        let mut r = f.prepared(true);
        let mut adapter = Fake::new();
        let prior = if has_previous {
            r.start(&mut adapter).unwrap();
            let result = r
                .ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
                .unwrap()
                .stage
                .unwrap()
                .result_ids[0]
                .clone();
            followup(&mut r, Some(result.clone()), 2, 30).unwrap();
            Some(result)
        } else {
            None
        };
        let original_result = prior
            .as_ref()
            .map(|id| r.read_source(&format!("records/result-{id}.md")).unwrap());
        let original_context = r.snapshot().unwrap().dispatch.unwrap().context_id;
        let original_context = r
            .read_source(&format!("records/context-{original_context}.md"))
            .unwrap();
        let revise = prepared_change(
            &r,
            98,
            PreparedChange::Revise {
                next_step: "Corrected complete stage".into(),
            },
        );
        r.change_prepared(revise).unwrap();
        let discard = prepared_change(&r, 99, PreparedChange::Discard);
        let receipt = r.change_prepared(discard.clone()).unwrap();
        assert!(receipt.replacement.is_none());
        let discarded = r.snapshot().unwrap();
        assert_eq!(discarded.phase.as_deref(), Some("discarded"));
        assert_eq!(discarded.stage.unwrap().status, "cancelled");
        assert!(!discarded.can_change_prepared);
        assert_eq!(discarded.dispatch.unwrap().packet.previous_result_id, prior);
        assert!(r.active_engine().is_none());
        assert!(
            r.reconcile(&mut adapter).is_err(),
            "discard cannot be revived through provider reconciliation"
        );
        assert!(r
            .start_expected(&mut adapter, discarded.prepared_guard)
            .is_err());
        drop(r);
        let mut r = f.open();
        assert_eq!(r.change_prepared(discard).unwrap(), receipt);
        followup(&mut r, prior.clone(), 2, 40).unwrap();
        assert_eq!(
            r.snapshot()
                .unwrap()
                .dispatch
                .unwrap()
                .packet
                .previous_result_id,
            prior
        );
        assert_eq!(r.stages().unwrap().len(), if has_previous { 4 } else { 3 });
        assert_eq!(
            r.read_source(&original_context.path).unwrap(),
            original_context
        );
        if let Some(source) = original_result {
            assert_eq!(r.read_source(&source.path).unwrap(), source);
        }
        assert_eq!(
            adapter.starts.load(Ordering::SeqCst),
            usize::from(has_previous)
        );
    }
}

#[test]
fn prepared_changes_reject_stale_guards_and_sent_or_uncertain_stages() {
    for lose_reply in [false, true] {
        let f = Fixture::new();
        let mut r = f.prepared(false);
        let request = prepared_change(&r, 98, PreparedChange::Discard);
        let mut a = Fake::new();
        a.lose_reply = lose_reply;
        r.start_expected(&mut a, Some(request.expected.clone()))
            .unwrap();
        let before = r.snapshot().unwrap();
        assert!(r.change_prepared(request.clone()).is_err());
        let mut fresh = request;
        fresh.expected = before.prepared_guard.clone().unwrap();
        assert!(
            r.change_prepared(fresh).is_err(),
            "fresh guard must not permit edits after attempted dispatch"
        );
        assert_eq!(r.snapshot().unwrap().dispatch, before.dispatch);
        assert_eq!(a.starts.load(Ordering::SeqCst), 1);
    }
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut request = prepared_change(&r, 98, PreparedChange::Discard);
    request.expected.stage_revision = "sha256:stale".into();
    assert!(r.change_prepared(request).is_err());
    assert_eq!(r.stages().unwrap().len(), 1);
}

#[test]
fn prepared_edit_start_race_has_one_winner_and_never_dispatches_unreviewed_context() {
    use std::sync::{Barrier, Mutex};
    for discard in [false, true] {
        for _ in 0..6 {
            let f = Fixture::new();
            let r = f.prepared(false);
            let request = prepared_change(
                &r,
                98,
                if discard {
                    PreparedChange::Discard
                } else {
                    PreparedChange::Revise {
                        next_step: "Revised instruction".into(),
                    }
                },
            );
            let expected = request.expected.clone();
            let runner = Arc::new(Mutex::new(r));
            let barrier = Arc::new(Barrier::new(2));
            let count = Arc::new(AtomicUsize::new(0));
            let edit_runner = runner.clone();
            let edit_barrier = barrier.clone();
            let edit = std::thread::spawn(move || {
                edit_barrier.wait();
                edit_runner.lock().unwrap().change_prepared(request).is_ok()
            });
            let start_runner = runner.clone();
            let start_barrier = barrier.clone();
            let start_count = count.clone();
            let start = std::thread::spawn(move || {
                let mut a = Fake::new();
                a.starts = start_count;
                start_barrier.wait();
                start_runner
                    .lock()
                    .unwrap()
                    .start_expected(&mut a, Some(expected))
                    .is_ok()
            });
            let edited = edit.join().unwrap();
            let started = start.join().unwrap();
            assert_ne!(edited, started);
            assert_eq!(count.load(Ordering::SeqCst), usize::from(started));
            let state = runner.lock().unwrap().snapshot().unwrap();
            if started {
                assert_eq!(state.dispatch.unwrap().packet.next_step, "Read and explain");
            } else {
                assert_eq!(
                    state.phase.as_deref(),
                    Some(if discard { "discarded" } else { "prepared" })
                );
            }
        }
    }
}

#[test]
fn prepared_revision_never_refreshes_changed_goal_or_context_and_rejected_start_stays_immutable() {
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let request = prepared_change(
        &r,
        98,
        PreparedChange::Revise {
            next_step: "New instruction".into(),
        },
    );
    replace_goal(&mut r, |map, _| {
        map["criteria"][0]["description"] = serde_yaml::Value::String("Different criterion".into());
    });
    assert!(r
        .change_prepared(request)
        .unwrap_err()
        .to_string()
        .contains("goal criteria changed"));
    let discard = prepared_change(&r, 99, PreparedChange::Discard);
    r.change_prepared(discard).unwrap();
    followup(&mut r, None, 2, 30).unwrap();
    let request = prepared_change(&r, 100, PreparedChange::Discard);
    let source = r
        .read_source(&format!(
            "records/context-{}.md",
            request.expected.context_id
        ))
        .unwrap();
    let mut content = STANDARD.decode(source.content_base64).unwrap();
    content.extend_from_slice(b"\nExternal note edit\n");
    r.write_source(SourceWrite {
        schema: SCHEMA.into(),
        operation_id: id(101),
        brain_id: id(1),
        path: source.path,
        expected_revision: Some(source.revision),
        content_base64: STANDARD.encode(content),
    })
    .unwrap();
    assert!(r
        .change_prepared(request)
        .unwrap_err()
        .to_string()
        .contains("context bytes changed"));
    let f = Fixture::new();
    let mut r = f.prepared(false);
    let mut adapter = Fake::new();
    adapter.reject = true;
    r.start(&mut adapter).unwrap();
    assert_eq!(r.snapshot().unwrap().phase.as_deref(), Some("not_started"));
    let request = prepared_change(&r, 98, PreparedChange::Discard);
    assert!(r.change_prepared(request).is_err());
    let guard = r.snapshot().unwrap().prepared_guard;
    r.start_expected(&mut adapter, guard).unwrap();
    assert_eq!(
        adapter.starts.load(Ordering::SeqCst),
        2,
        "proven not_started retains guarded retry"
    );
}

#[test]
fn workspace_attention_projects_every_goal_without_writes_or_provider_calls() {
    use okilum_brain::application::Application;
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let app = Application::unconfigured();
    let mut adapter = Fake::new();
    r.start(&mut adapter).unwrap();
    r.ingest(event(
        &r,
        1,
        EventPayload::Status {
            state: "blocked".into(),
        },
    ))
    .unwrap();
    r.ingest(event(&r, 2, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    second_goal(&mut r);
    followup(&mut r, None, 20, 30).unwrap();
    r.with_goal(&id(20), |r| {
        r.start(&mut adapter)?;
        let mut alert = event(
            r,
            3,
            EventPayload::Status {
                state: "blocked".into(),
            },
        );
        alert.operation_id = id(32);
        r.ingest(alert)?;
        Ok(())
    })
    .unwrap();
    let before = fs::read(f.temp.path().join("runtime/state.json")).unwrap();
    let goal_before = r.goal_source().unwrap();
    let projection = app.workspace_attention(&mut r).unwrap();
    assert_eq!(projection["goal_count"], 2);
    assert_eq!(projection["running_goal_count"], 1);
    assert_eq!(projection["workspace"], r.workspace_identity());
    time::OffsetDateTime::parse(
        projection["observed_at"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let items = projection["items"].as_array().unwrap();
    assert!(items.iter().any(|item| item["goal_id"] == id(2)
        && item["kind"] == "decision"
        && item["stage_id"] == id(3)
        && item["result_id"].is_string()));
    assert!(items.iter().any(|item| item["goal_id"] == id(20)
        && item["kind"] == "blocker"
        && item["stage_id"] == id(30)
        && item["result_id"].is_null()));
    assert!(!items
        .iter()
        .any(|item| item["goal_id"] == id(2) && item["kind"] == "blocker"));
    for goal in [id(2), id(20)] {
        r.with_goal(&goal, |r| {
            let selected = app.snapshot(r)?;
            let projected: Vec<_> = items
                .iter()
                .filter(|item| item["goal_id"] == goal)
                .map(|item| item["attention_id"].clone())
                .collect();
            let expected: Vec<_> = selected["attention"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].clone())
                .collect();
            assert_eq!(projected, expected);
            // Even when called while routed to another goal, restore that route.
            app.workspace_attention(r)?;
            assert_eq!(r.snapshot()?.goal.unwrap().id, goal);
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(
        r.goal_source().unwrap().content_base64,
        goal_before.content_base64
    );
    assert_eq!(
        fs::read(f.temp.path().join("runtime/state.json")).unwrap(),
        before
    );
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 2);
    drop(r);
    let mut r = f.open();
    assert_eq!(
        app.workspace_attention(&mut r).unwrap()["items"],
        projection["items"]
    );
}

#[test]
fn workspace_attention_keeps_late_navigation_identity_and_hides_predecessor_alerts() {
    use okilum_brain::application::Application;
    let f = Fixture::new();
    let mut r = f.prepared(true);
    let app = Application::unconfigured();
    let mut adapter = Fake::new();
    r.start(&mut adapter).unwrap();
    r.ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
        .unwrap();
    let projection = app.workspace_attention(&mut r).unwrap();
    let old = projection["items"][0].clone();
    let predecessor = old["result_id"].as_str().unwrap().to_owned();
    followup(&mut r, Some(predecessor.clone()), 2, 30).unwrap();
    let fresh = app.workspace_attention(&mut r).unwrap();
    assert!(fresh["items"].as_array().unwrap().is_empty());
    assert_eq!(old["goal_id"], id(2));
    assert_eq!(old["stage_id"], id(3));
    assert_eq!(old["current_stage_id"], id(3));
    assert_eq!(r.result(&predecessor).unwrap().stage_id, id(3));
    let selected = app.snapshot(&r).unwrap();
    assert_eq!(selected["stage"]["id"], id(30));
    assert!(selected["attention_history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == old["attention_id"]));
    assert_eq!(
        selected["attention_stage_ids"][old["attention_id"].as_str().unwrap()],
        id(3)
    );
    // A late projection is a retained observation, never permission to change
    // the current stage or reinterpret the old attention's owner.
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn workspace_attention_keeps_unknown_legacy_ownership_and_fails_closed_on_unreadable_goal() {
    use okilum_brain::application::Application;
    let f = Fixture::new();
    let mut r = f.prepared(true);
    second_goal(&mut r);
    drop(r);
    let path = f.temp.path().join("runtime/state.json");
    let mut journal: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    journal["attention"].as_array_mut().unwrap().push(json!({
        "id":"legacy-decision","kind":"decision","message":"Review retained decision"
    }));
    fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let mut r = f.open();
    let app = Application::unconfigured();
    let projection = app.workspace_attention(&mut r).unwrap();
    let legacy = projection["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["attention_id"] == "legacy-decision")
        .unwrap();
    assert_eq!(legacy["goal_id"], id(2));
    assert!(legacy["stage_id"].is_null());
    assert!(legacy["result_id"].is_null());
    assert_eq!(legacy["current_stage_id"], id(3));
    let before = fs::read(&path).unwrap();
    fs::remove_file(
        f.temp
            .path()
            .join(format!("brain/records/goal-{}.md", id(20))),
    )
    .unwrap();
    assert!(
        app.workspace_attention(&mut r).is_err(),
        "unreadable goal is not an empty inbox"
    );
    assert_eq!(r.snapshot().unwrap().goal.unwrap().id, id(2));
    assert_eq!(fs::read(&path).unwrap(), before);
}

fn catchup_attention(runner: &Runner) -> Option<String> {
    runner
        .snapshot()
        .unwrap()
        .attention
        .iter()
        .find(|a| a.kind == "blocker" && a.message == "t3_snapshot_changed_during_catchup")
        .map(|a| a.id.clone())
}

#[test]
fn catchup_blocker_resolves_only_after_correlated_terminal_receipt_without_erasing_history() {
    use okilum_brain::application::Application;
    for terminal in ["succeeded", "failed", "cancelled"] {
        let f = Fixture::new();
        let mut r = f.prepared(true);
        let mut adapter = Fake::new();
        adapter.catchup_error = true;
        r.start(&mut adapter).unwrap();
        r.reconcile(&mut adapter).unwrap();
        let alert_id = catchup_attention(&r).expect("uncertain catchup must require attention");
        adapter.catchup_error = false;
        r.reconcile(&mut adapter).unwrap();
        assert_eq!(
            catchup_attention(&r),
            Some(alert_id.clone()),
            "running is not resolution"
        );
        let mut result = outcome(&r);
        result.outcome = terminal.into();
        r.ingest(event(&r, 1, EventPayload::Outcome(result)))
            .unwrap();
        let before = fs::read(f.temp.path().join("runtime/state.json")).unwrap();
        let app = Application::unconfigured();
        assert_eq!(catchup_attention(&r), None);
        let selected = app.snapshot(&r).unwrap();
        let workspace = app.workspace_attention(&mut r).unwrap();
        assert!(!selected["attention"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == alert_id));
        assert!(!workspace["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["attention_id"] == alert_id));
        assert!(selected["attention_history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == alert_id));
        assert_eq!(selected["attention_stage_ids"][&alert_id], id(3));
        assert_eq!(
            selected["goal"]["status"], "active",
            "saved output does not satisfy human criteria"
        );
        assert!(selected["attention"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["kind"] == "decision"));
        assert_eq!(
            fs::read(f.temp.path().join("runtime/state.json")).unwrap(),
            before
        );
        assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
        drop(r);
        let r = f.open();
        assert_eq!(catchup_attention(&r), None);
        assert!(r
            .snapshot()
            .unwrap()
            .attention_history
            .iter()
            .any(|a| a.id == alert_id));
    }
}

#[test]
fn catchup_blocker_keeps_unreadable_mismatched_unknown_and_nonterminal_receipts_visible() {
    use okilum_brain::application::Application;
    for mismatch in [
        "goal",
        "stage",
        "operation",
        "binding",
        "missing",
        "unreadable",
        "unknown_outcome",
        "running",
        "stage_running",
        "unknown_owner",
        "no_turn",
        "empty_identity",
        "no_result",
    ] {
        let f = Fixture::new();
        let mut r = f.prepared(true);
        let mut adapter = Fake::new();
        adapter.catchup_error = true;
        r.start(&mut adapter).unwrap();
        r.reconcile(&mut adapter).unwrap();
        let alert_id = catchup_attention(&r).unwrap();
        adapter.catchup_error = false;
        r.reconcile(&mut adapter).unwrap();
        r.ingest(event(&r, 1, EventPayload::Outcome(outcome(&r))))
            .unwrap();
        let result_id = r.snapshot().unwrap().stage.unwrap().result_ids[0].clone();
        let result_path = f
            .temp
            .path()
            .join(format!("brain/records/result-{result_id}.md"));
        let stage_path = f
            .temp
            .path()
            .join(format!("brain/records/stage-{}.md", id(3)));
        drop(r);
        // Emulate retained/external inconsistency; a historical or unreadable
        // receipt must not be promoted into proof that the current alert resolved.
        let edit_record = |path: &std::path::Path, key: &str, value: serde_yaml::Value| {
            let text = fs::read_to_string(path).unwrap();
            let (front, body) = text
                .strip_prefix("---\n")
                .unwrap()
                .split_once("\n---\n")
                .unwrap();
            let mut map: serde_yaml::Mapping = serde_yaml::from_str(front).unwrap();
            map.insert(key.into(), value);
            fs::write(
                path,
                format!("---\n{}---\n{body}", serde_yaml::to_string(&map).unwrap()),
            )
            .unwrap();
        };
        let journal_path = f.temp.path().join("runtime/state.json");
        let mut journal: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        match mismatch {
            "goal" => edit_record(&result_path, "goal_id", id(90).into()),
            "stage" => edit_record(&result_path, "stage_id", id(91).into()),
            "operation" => edit_record(&result_path, "operation_id", id(92).into()),
            "binding" => {
                let mut other = binding();
                other.turn_id = Some("different-turn".into());
                edit_record(
                    &result_path,
                    "engine_ref",
                    serde_yaml::to_value(other).unwrap(),
                );
            }
            "missing" => fs::remove_file(&result_path).unwrap(),
            "unreadable" => fs::write(&result_path, "invalid record bytes").unwrap(),
            "unknown_outcome" => edit_record(&result_path, "outcome", "unknown".into()),
            "running" => journal["dispatch"]["phase"] = json!("running"),
            "stage_running" => edit_record(&stage_path, "status", "running".into()),
            "unknown_owner" => {
                journal["attention_stage_ids"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&alert_id);
            }
            "no_turn" => journal["dispatch"]["binding"]["turn_id"] = serde_json::Value::Null,
            "empty_identity" => {
                let mut empty = binding();
                empty.instance_id.clear();
                edit_record(
                    &result_path,
                    "engine_ref",
                    serde_yaml::to_value(&empty).unwrap(),
                );
                journal["dispatch"]["binding"] = serde_json::to_value(empty).unwrap();
            }
            "no_result" => edit_record(
                &stage_path,
                "result_ids",
                serde_yaml::Value::Sequence(vec![]),
            ),
            _ => unreachable!(),
        }
        fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
        let mut r = f.open();
        let before = fs::read(&journal_path).unwrap();
        assert_eq!(catchup_attention(&r), Some(alert_id.clone()), "{mismatch}");
        let app = Application::unconfigured();
        let selected = app.snapshot(&r).unwrap();
        let workspace = app.workspace_attention(&mut r).unwrap();
        assert!(
            selected["attention"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["id"] == alert_id),
            "{mismatch}"
        );
        assert!(
            workspace["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["attention_id"] == alert_id),
            "{mismatch}"
        );
        assert_eq!(fs::read(&journal_path).unwrap(), before, "{mismatch}");
    }
}
