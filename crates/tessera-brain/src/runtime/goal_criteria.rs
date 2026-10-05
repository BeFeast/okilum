//! Guarded criterion edits reuse exact SourceStore operations and receipts.
use super::*;
use tessera_core::source::{ErrorCode, WriteOutcome, WriteReceipt};

const MAX_SOURCE: u64 = 8 * 1024 * 1024;
const MAX_RECOVERY: u64 = 64 * 1024 * 1024;

impl Runner {
    fn criteria_edit_eligible(&self, goal: &Goal) -> Result<()> {
        ensure!(
            self.managed(),
            "Outcome criteria require a writable managed brain"
        );
        ensure!(
            self.state.goal_id.as_deref() == Some(goal.id.as_str()),
            "Goal owner changed"
        );
        let Some(dispatch) = &self.state.dispatch else {
            ensure!(
                self.state.stage_id.is_none(),
                "Current stage state is unavailable; inspect it before editing criteria"
            );
            return Ok(());
        };
        ensure!(
            matches!(
                dispatch.phase.as_str(),
                "outcome_ready" | "cancelled" | "discarded"
            ),
            "Finish or discard the current stage before editing outcome criteria (state: {})",
            dispatch.phase
        );
        let stage_id = self
            .state
            .stage_id
            .as_ref()
            .context("Current stage identity is unavailable")?;
        ensure!(
            dispatch.envelope.goal_id == goal.id
                && &dispatch.envelope.stage_id == stage_id
                && goal.stage_ids.contains(stage_id),
            "Current stage does not match this goal"
        );
        let (stage, _) = self.record::<Stage>("stage", stage_id)?;
        ensure!(
            stage.goal_id == goal.id && stage.id == *stage_id,
            "Saved stage owner does not match this goal"
        );
        if dispatch.phase == "outcome_ready" {
            ensure!(
                matches!(stage.status.as_str(), "outcome_ready" | "completed"),
                "Saved stage has no terminal outcome"
            );
            let result_id = stage
                .result_ids
                .last()
                .context("Saved stage outcome is unavailable")?;
            let result = self.result(result_id)?;
            ensure!(
                result.id == *result_id
                    && result.goal_id == goal.id
                    && result.stage_id == stage.id
                    && result.operation_id == dispatch.envelope.operation_id
                    && dispatch.binding.as_ref() == Some(&result.engine_ref)
                    && matches!(
                        result.outcome.outcome.as_str(),
                        "succeeded" | "failed" | "cancelled"
                    ),
                "Saved outcome does not match this stage"
            );
        } else {
            ensure!(
                stage.status == "cancelled",
                "Saved stage is not cancelled or discarded"
            );
        }
        Ok(())
    }

    pub fn goal_criteria_get(&self, goal_id: &str) -> Result<Value> {
        uuid(goal_id)?;
        ensure!(
            self.state.goal_id.as_deref() == Some(goal_id),
            "Goal owner changed"
        );
        let source = self
            .source
            .read_bounded(&self.path("goal", goal_id), MAX_SOURCE)?;
        let (mapping, _) = parse_document(&source)?;
        let goal: Goal = serde_yaml::from_value(serde_yaml::Value::Mapping(mapping))?;
        let inspection = tessera_core::goal_criteria::inspect(&source, goal_id);
        let reason = self
            .criteria_edit_eligible(&goal)
            .err()
            .map(|e| e.to_string())
            .or_else(|| inspection.as_ref().err().cloned());
        Ok(
            serde_json::json!({"goal_id":goal_id,"source":source,"editable":reason.is_none(),"reason":reason,
            "criteria":inspection.ok()}),
        )
    }

    pub fn goal_criteria_write(
        &mut self,
        goal_id: &str,
        request: SourceWrite,
        base: SourceSnapshot,
    ) -> Result<WriteReceipt> {
        uuid(goal_id)?;
        ensure!(
            self.managed(),
            "Outcome criteria require a writable managed brain"
        );
        ensure!(
            self.state.goal_id.as_deref() == Some(goal_id)
                && request.brain_id == self.state.brain_id
                && request.path == self.path("goal", goal_id),
            "Criteria save belongs to another goal or brain"
        );
        tessera_core::goal_criteria::validate_write(&base, &request, goal_id)
            .map_err(anyhow::Error::msg)?;
        // Returning a known receipt is read-only, even after a later Start or
        // source edit. An unresolved intent may still write and gets no bypass.
        match self
            .source
            .recovery_record_bounded(&request.operation_id, MAX_RECOVERY)
        {
            Ok(record) => {
                ensure!(
                    record.request == request && record.base.as_ref() == Some(&base),
                    "Save identity already belongs to different criteria or base bytes"
                );
                if let Some(receipt) = record.receipt {
                    use sha2::{Digest, Sha256};
                    let bytes = STANDARD.decode(&request.content_base64)?;
                    let revision = format!("sha256:{:x}", Sha256::digest(&bytes));
                    ensure!(
                        record.conflict.is_none()
                            && receipt.operation_id == request.operation_id
                            && receipt.path == request.path
                            && receipt.previous_revision == request.expected_revision
                            && receipt.previous_revision == record.previous_revision
                            && receipt.revision == revision
                            && receipt.outcome
                                == if request.expected_revision.as_ref() == Some(&revision) {
                                    WriteOutcome::Unchanged
                                } else {
                                    WriteOutcome::Written
                                },
                        "Stored criteria receipt is inconsistent"
                    );
                    ensure!(
                        record.preimage_base64.as_ref() == Some(&base.content_base64),
                        "Stored criteria preimage is inconsistent"
                    );
                    return Ok(receipt);
                }
            }
            Err(error) if error.code == ErrorCode::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.flush_writes()?;
        let (goal, _) = self.record::<Goal>("goal", goal_id)?;
        self.criteria_edit_eligible(&goal)?;
        // The service owner mutex covers eligibility through CAS; the source
        // lock additionally serializes cooperating managed source writers.
        Ok(self.source.write_with_base(request, Some(base))?)
    }

    pub(super) fn goal_completion_valid(&self, goal: &Goal, state: &GoalState) -> bool {
        state
            .stage_id
            .as_deref()
            .and_then(|id| self.record::<Stage>("stage", id).ok())
            .filter(|(stage, _)| stage.goal_id == goal.id && goal.stage_ids.contains(&stage.id))
            .and_then(|(stage, _)| {
                stage
                    .result_ids
                    .last()
                    .and_then(|id| self.result(id).ok())
                    .filter(|result| result.stage_id == stage.id)
            })
            .filter(|result| result.goal_id == goal.id)
            .is_some_and(|result| {
                self.criteria_pass_in_state(goal, &result.outcome, state)
                    .unwrap_or(false)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::goal_criteria::{inspect, transform};
    fn id(n: u32) -> String {
        format!("02000000-0000-4000-8000-{n:012}")
    }
    fn fixture() -> (tempfile::TempDir, Runner) {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        fs::create_dir(temp.path().join("runtime")).unwrap();
        let mut r = Runner::open(RunnerConfig {
            brain_id: id(1),
            root: temp.path().join("brain"),
            operational_dir: temp.path().join("runtime"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        })
        .unwrap();
        r.create_goal(
            Goal {
                id: id(2),
                title: "Criteria review".into(),
                status: "active".into(),
                criteria: vec![Criterion {
                    id: "C1".into(),
                    description: "Proof".into(),
                    requires_human: false,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "\n# Original body\n\nKeep **all** of this.\n".into(),
        )
        .unwrap();
        (temp, r)
    }
    fn write(r: &Runner, description: &str) -> (SourceSnapshot, SourceWrite) {
        let base = r.goal_source().unwrap();
        let mut rows = inspect(&base, &id(2)).unwrap();
        rows[0].description = description.into();
        let proposed = transform(&base, &id(2), &rows).unwrap();
        let request = SourceWrite {
            schema: "ai-brain/v1".into(),
            operation_id: Uuid::new_v4().to_string(),
            brain_id: base.brain_id.clone(),
            path: base.path.clone(),
            expected_revision: Some(base.revision.clone()),
            content_base64: STANDARD.encode(proposed),
        };
        (base, request)
    }
    fn prepare(r: &mut Runner) {
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
                goal: "Criteria review".into(),
                decisions: vec![],
                constraints: vec![],
                sources: vec![],
                previous_result_id: None,
                next_step: "Verify".into(),
                extra: BTreeMap::new(),
            },
            id(5),
            BTreeMap::new(),
        )
        .unwrap();
    }
    fn complete(r: &mut Runner) {
        prepare(r);
        let goal_revision = r.state.retained_goal.as_ref().unwrap().revision.clone();
        let binding = EngineRef {
            engine: "t3".into(),
            instance_id: "fixture".into(),
            thread_id: Some("thread".into()),
            turn_id: Some("turn".into()),
            task_id: None,
        };
        r.apply_start(StartReply::Accepted {
            binding: binding.clone(),
        })
        .unwrap();
        r.ingest(EngineEvent {
            operation_id: id(5),
            engine_ref: binding,
            event_id: "done".into(),
            stream_id: "thread/turn".into(),
            sequence: Some(1),
            cursor: Some("1".into()),
            observed_at: "2026-09-08T10:00:00Z".into(),
            payload: EventPayload::Outcome(Outcome {
                outcome: "succeeded".into(),
                summary: "Proof".into(),
                sources: vec![],
                verification: "unverified".into(),
                evidence: vec![Evidence {
                    id: "E1".into(),
                    kind: "artifact".into(),
                    source: SourceRef {
                        uri: "fixture://proof".into(),
                        revision: None,
                        locator: None,
                    },
                    description: "Proof".into(),
                    observed_at: "2026-09-08T10:00:00Z".into(),
                    status: "passed".into(),
                }],
                criterion_evaluations: vec![CriterionEvaluation {
                    criterion_id: "C1".into(),
                    goal_revision,
                    status: "passed".into(),
                    evidence_ids: vec!["E1".into()],
                    evaluated_by: "verifier".into(),
                    evaluated_at: "2026-09-08T10:00:00Z".into(),
                }],
            }),
        })
        .unwrap();
    }
    #[test]
    fn criteria_save_and_stage_admission_use_actual_state() {
        let (_temp, mut r) = fixture();
        let (base, request) = write(&r, "Updated proof");
        assert_eq!(r.goal_criteria_get(&id(2)).unwrap()["editable"], true);
        r.goal_criteria_write(&id(2), request, base).unwrap();
        prepare(&mut r);
        let (base, request) = write(&r, "After stage");
        for phase in [
            "prepared",
            "prepared_edited",
            "not_started",
            "submitting",
            "running",
            "indeterminate",
            "unknown",
        ] {
            r.state.dispatch.as_mut().unwrap().phase = phase.into();
            assert!(
                r.goal_criteria_write(&id(2), request.clone(), base.clone())
                    .is_err(),
                "{phase}"
            );
            assert_eq!(r.goal_source().unwrap(), base);
            assert_eq!(r.goal_criteria_get(&id(2)).unwrap()["editable"], false);
        }
        r.state.dispatch.as_mut().unwrap().phase = "cancelled".into();
        assert!(
            r.goal_criteria_write(&id(2), request.clone(), base.clone())
                .is_err(),
            "stage must actually be cancelled"
        );
        let path = r.root.join(r.path("stage", &id(3)));
        let text = fs::read_to_string(&path)
            .unwrap()
            .replace("status: ready", "status: cancelled");
        fs::write(path, text).unwrap();
        r.goal_criteria_write(&id(2), request, base).unwrap();
    }
    #[test]
    fn criteria_receipt_replays_before_phase_source_or_pending_flush() {
        let (_temp, mut r) = fixture();
        let (base, request) = write(&r, "Saved proof");
        let receipt = r
            .goal_criteria_write(&id(2), request.clone(), base.clone())
            .unwrap();
        prepare(&mut r);
        let path = r.root.join(&base.path);
        let later = format!(
            "{}\nLater external body\n",
            fs::read_to_string(&path).unwrap()
        );
        fs::write(&path, &later).unwrap();
        r.state.pending_writes.push(request.clone());
        assert_eq!(
            r.goal_criteria_write(&id(2), request.clone(), base.clone())
                .unwrap(),
            receipt
        );
        assert_eq!(r.state.pending_writes.len(), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), later);
        let mut changed = request.clone();
        let mut rows = inspect(&base, &id(2)).unwrap();
        rows[0].description = "Different reuse".into();
        changed.content_base64 = STANDARD.encode(transform(&base, &id(2), &rows).unwrap());
        assert!(r.goal_criteria_write(&id(2), changed, base).is_err());
    }
    #[test]
    fn criteria_stale_source_retains_three_way_conflict_without_merge() {
        let (_temp, mut r) = fixture();
        let (base, request) = write(&r, "Proposed proof");
        let path = r.root.join(&base.path);
        let current = format!(
            "{}\nExternal body edit\n",
            fs::read_to_string(&path).unwrap()
        );
        fs::write(&path, &current).unwrap();
        assert!(r
            .goal_criteria_write(&id(2), request.clone(), base.clone())
            .is_err());
        let conflict = r
            .source_conflict(&base.brain_id, &base.path, &request.operation_id)
            .unwrap();
        assert_eq!(conflict.base, Some(base));
        assert_eq!(conflict.proposed.content_base64, request.content_base64);
        assert_eq!(
            STANDARD
                .decode(conflict.current.unwrap().content_base64)
                .unwrap(),
            current.as_bytes()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), current);
    }
    #[test]
    fn criteria_unknown_intent_does_not_bypass_stage_guard() {
        let (_temp, mut r) = fixture();
        prepare(&mut r);
        let (base, request) = write(&r, "Pending proof");
        let journal = serde_json::json!({"base":base,"request":request,"preimage_base64":base.content_base64,"previous_revision":base.revision,"receipt":null,"conflict":null,"divergent_observations_base64":[]});
        let path = r
            .state_dir
            .join("source")
            .join(format!("{}.json", request.operation_id));
        fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(r
            .goal_criteria_write(&id(2), request, base.clone())
            .is_err());
        assert_eq!(r.goal_source().unwrap(), base);
        assert_eq!(fs::read(path).unwrap(), before);
    }
    #[test]
    fn criteria_corrupt_receipts_and_journals_fail_closed() {
        let (_temp, mut r) = fixture();
        let (base, request) = write(&r, "Saved proof");
        r.goal_criteria_write(&id(2), request.clone(), base.clone())
            .unwrap();
        let path = r
            .state_dir
            .join("source")
            .join(format!("{}.json", request.operation_id));
        let record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for key in ["operation_id", "path", "revision", "previous_revision"] {
            let mut bad = record.clone();
            bad["receipt"][key] = serde_json::json!("wrong");
            fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(
                r.goal_criteria_write(&id(2), request.clone(), base.clone())
                    .is_err(),
                "{key}"
            );
        }
        fs::write(&path, b"{unknown journal").unwrap();
        assert!(r
            .goal_criteria_write(&id(2), request.clone(), base.clone())
            .is_err());
        File::create(&path)
            .unwrap()
            .set_len(MAX_RECOVERY + 1)
            .unwrap();
        assert!(r.goal_criteria_write(&id(2), request, base).is_err());
    }
    #[test]
    fn criteria_completed_projection_changes_in_snapshot_and_other_goal_list() {
        let (_temp, mut r) = fixture();
        complete(&mut r);
        assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "completed");
        assert_eq!(r.goals().unwrap()[0].status, "completed");
        let (base, request) = write(&r, "New proof definition");
        r.goal_criteria_write(&id(2), request, base).unwrap();
        assert_eq!(
            r.record::<Goal>("goal", &id(2)).unwrap().0.status,
            "completed"
        );
        assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "blocked");
        assert_eq!(r.goals().unwrap()[0].status, "blocked");
        r.create_goal(
            Goal {
                id: id(20),
                title: "Other".into(),
                status: "active".into(),
                criteria: vec![Criterion {
                    id: "C2".into(),
                    description: "Other proof".into(),
                    requires_human: true,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "\n# Other\n".into(),
        )
        .unwrap();
        r.state.route(&id(20)).unwrap();
        assert_eq!(r.state.goal_id, Some(id(20)));
        assert_eq!(
            r.goals()
                .unwrap()
                .iter()
                .find(|g| g.id == id(2))
                .unwrap()
                .status,
            "blocked"
        );
        r.with_goal(&id(2), |r| {
            assert_eq!(r.snapshot()?.goal.unwrap().status, "blocked");
            Ok(())
        })
        .unwrap();
    }
}
