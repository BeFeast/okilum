//! Guarded remote decisions reuse the existing local Maestro journal.
use super::*;
use crate::{
    maestro::Settings, maestro_control as control, maestro_links::Link, maestro_operations::Request,
};
impl Runner {
    pub(crate) fn maestro_control_link(
        &self,
        goal: &str,
        link_id: &str,
        active: bool,
    ) -> Result<Link> {
        uuid(goal)?;
        uuid(link_id)?;
        ensure!(
            self.managed && !self.state.maestro_journal.recovery_required,
            "Managed Maestro history is unavailable"
        );
        let link = self
            .state
            .maestro_journal
            .links
            .get(link_id)
            .context("Original Maestro link unavailable")?;
        ensure!(
            link.goal_id == goal && (!active || link.active),
            "Original Maestro link is no longer active for this goal"
        );
        Ok(link.clone())
    }
    pub(crate) fn maestro_decision_scope(
        &self,
        request: &control::Request,
        config: &Settings,
    ) -> Result<()> {
        request.validate()?;
        let link = self.maestro_control_link(&request.goal_id, &request.expected_link_id, true)?;
        let expected = &request.review.expected;
        ensure!(
            request.instance == config.identity() && request.instance == link.instance,
            "Original Maestro instance changed"
        );
        ensure!(
            expected.project_id == link.project_id
                && expected.project_name == link.project_name
                && expected.project_repo == link.repo
                && request.review.target.issue == link.issue_number,
            "Approval review does not belong to the linked issue"
        );
        ensure!(
            control::NEW_DECISIONS_ENABLED,
            "Approval sends are disabled; retained decisions remain available for reconciliation"
        );
        Ok(())
    }
    pub(crate) fn maestro_decision_commit(
        &mut self,
        request: &control::Request,
        receipt: &control::Receipt,
        execution_status: &str,
    ) -> Result<Value> {
        let operation = Request::ApprovalDecision(Box::new(request.clone()));
        self.mutation(|runner| {
            request.validate()?;
            ensure!(receipt.matches(request),"Provider receipt does not match original decision");
            let prior=runner.maestro_operation_get(&request.goal_id,&request.operation_id)?;
            ensure!(prior.kind.as_deref()==Some("approval_decision") && prior.request==Some(operation.body()),"No matching retained approval decision");
            if prior.status=="committed" { return Ok(prior.receipt.unwrap()); }
            ensure!(prior.status=="pending","Approval intention is not pending");
            let link=runner.maestro_control_link(&request.goal_id,&request.expected_link_id,false)?;
            ensure!(link.instance==request.instance,"Original linked instance differs");
            let value=serde_json::json!({"operation_id":request.operation_id,"goal_id":request.goal_id,"link_id":request.expected_link_id,"instance":request.instance,"decision_receipt":receipt,"execution_status":execution_status});
            let record=serde_json::json!({"goal_id":request.goal_id,"link_id":request.expected_link_id,"instance":request.instance,"review":request.review,"decision_receipt":receipt,"execution_status":execution_status,"verification":"unverified"});
            runner.queue("maestro-observation",&request.operation_id,&record,Some(&format!("# Recorded Maestro decision\n\n{} #{} / PR {} / head {}.\n\nDecision: {:?}. Original actor: {}.\n\nExecution status observed: {}. This receipt does not complete a Okilum stage or prove this head was merged. Maestro may update a behind-base branch and skip execution for revalidation.\n",link.repo,link.issue_number,request.review.target.pr,request.review.target.head_sha,receipt.decision,receipt.actor,execution_status)))?;
            runner.state.maestro_journal.operations.insert(request.operation_id.clone(),serde_json::json!({"kind":"approval_decision","request":request,"status":"committed","receipt":value,"rejection":null}));
            ensure!(serde_json::to_vec(&runner.state.maestro_journal)?.len() <= 32 * 1024 * 1024, "Maestro retained operation budget reached");
            runner.persist()?;
            runner.flush_writes()?;
            Ok(value)
        })
    }
    pub(crate) fn maestro_decision_history(&self, goal: &str) -> Vec<Value> {
        self.state
            .maestro_journal
            .operations
            .values()
            .filter(|record| {
                record["kind"] == "approval_decision" && record["request"]["goal_id"] == goal
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        tempfile::TempDir,
        Runner,
        RunnerConfig,
        control::Request,
        Settings,
    ) {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("brain/records")).unwrap();
        fs::create_dir(t.path().join("runtime")).unwrap();
        let config = RunnerConfig {
            brain_id: Uuid::new_v4().to_string(),
            root: t.path().join("brain"),
            operational_dir: t.path().join("runtime"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let mut r = Runner::open(RunnerConfig {
            brain_id: config.brain_id.clone(),
            root: config.root.clone(),
            operational_dir: config.operational_dir.clone(),
            records_dir: config.records_dir.clone(),
            boundary: config.boundary,
        })
        .unwrap();
        let goal = Uuid::new_v4().to_string();
        r.create_goal(
            Goal {
                id: goal.clone(),
                title: "Approval fixture".into(),
                status: "active".into(),
                criteria: vec![Criterion {
                    id: "C1".into(),
                    description: "Needs actual acceptance".into(),
                    requires_human: true,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "# Fixture\n".into(),
        )
        .unwrap();
        let d = crate::maestro::tests::discovery();
        let p = &d.projects[0];
        let settings = crate::maestro::tests::settings(d.instance["base_url"].as_str().unwrap());
        let linked = r
            .maestro_link(
                crate::maestro_links::LinkRequest {
                    operation_id: Uuid::new_v4().to_string(),
                    goal_id: goal.clone(),
                    selection_guard: crate::maestro::selection_guard(&d.instance, p, &p.issues[0]),
                    project_id: p.project_id.clone(),
                    project_name: p.name.clone(),
                    repo: p.repo.clone(),
                    issue_number: 42,
                },
                &d,
            )
            .unwrap();
        let req=serde_json::from_value(serde_json::json!({"operation_id":Uuid::new_v4().to_string(),"goal_id":goal,"expected_link_id":linked["link_id"],"instance":d.instance,"review":{"expected":{"version":"v1","project_id":p.project_id,"project_name":p.name,"project_repo":p.repo,"approval_id":"approval-1","created_at":"2026-09-08T06:00:00Z","decision_revision":"v1:opaque"},"decision_id":"decision-1","action":"merge_pr","target":{"issue":42,"pr":9,"head_sha":"a".repeat(40)},"summary":"Merge exact head","risk":"low","evidence":["Checks passed"]},"decision":"approved","actor":"operator","reason":"Reviewed"})).unwrap();
        (t, r, config, req, settings)
    }
    #[test]
    fn approval_pending_survives_restart_rejects_abandon_and_commits_original_scope_after_unlink() {
        let (_t, mut r, c, req, settings) = fixture();
        let op = Request::ApprovalDecision(Box::new(req.clone()));
        let before = serde_json::to_value(&r.state.current).unwrap();
        r.maestro_decision_scope(&req, &settings).unwrap();
        r.maestro_operation_begin(&op).unwrap();
        assert_eq!(
            r.maestro_decision_refuse_unsent(&req, "Late refusal")
                .unwrap()
                .status,
            "pending"
        );
        let marker: Value = serde_json::from_slice(
            &fs::read(c.operational_dir.join("maestro-links-enrollment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marker[control::ENROLLMENT_FIELD], 1);
        assert_eq!(marker[crate::maestro_operations::ENROLLMENT_FIELD], 1);
        // The predecessor removes only its known operation version and refuses extra identity.
        let mut old = marker.clone();
        old.as_object_mut()
            .unwrap()
            .remove(crate::maestro_operations::ENROLLMENT_FIELD);
        assert_ne!(old, r.workspace_identity());
        assert!(r
            .maestro_operation_reject(&op, "abandoned", "Not a remote revocation")
            .is_err());
        drop(r);
        let mut r = Runner::open(c).unwrap();
        assert_eq!(
            r.maestro_operation_get(&req.goal_id, &req.operation_id)
                .unwrap()
                .status,
            "pending"
        );
        let mut changed = req.clone();
        changed.reason = "Changed after send".into();
        assert!(r
            .maestro_operation_begin(&Request::ApprovalDecision(Box::new(changed)))
            .is_err());
        r.maestro_unlink(crate::maestro_links::UnlinkRequest {
            operation_id: Uuid::new_v4().to_string(),
            goal_id: req.goal_id.clone(),
            expected_link_id: req.expected_link_id.clone(),
        })
        .unwrap();
        let marker: Value = serde_json::from_slice(
            &fs::read(r.state_dir.join("maestro-links-enrollment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marker[control::ENROLLMENT_FIELD], 1);
        assert!(r.maestro_decision_scope(&req, &settings).is_err());
        let receipt = control::Receipt {
            expected: req.review.expected.clone(),
            decision: control::Decision::Approved,
            actor: "original-author".into(),
            reason: "Original reason".into(),
            at: "2026-09-08T06:01:00Z".into(),
        };
        let mut wrong = receipt.clone();
        wrong.expected.created_at = "2026-09-09T06:00:00Z".into();
        assert!(r.maestro_decision_commit(&req, &wrong, "executed").is_err());
        let saved = r
            .maestro_decision_commit(&req, &receipt, "execution_failed")
            .unwrap();
        assert_eq!(saved["decision_receipt"]["actor"], "original-author");
        assert_eq!(r.maestro_operation_replay(&op).unwrap(), Some(saved));
        assert_eq!(serde_json::to_value(&r.state.current).unwrap(), before);
        assert!(r.state.maestro_journal.active(&req.goal_id).is_none());
    }
    #[test]
    fn approval_scope_rejects_cross_issue_project_and_instance_before_enrollment() {
        let (_t, r, c, req, settings) = fixture();
        for field in ["issue", "project", "instance"] {
            let mut changed = req.clone();
            match field {
                "issue" => changed.review.target.issue = 43,
                "project" => changed.review.expected.project_repo = "other/repo".into(),
                _ => {
                    changed.instance["instance_id"] = serde_json::json!(Uuid::new_v4().to_string())
                }
            };
            assert!(r.maestro_decision_scope(&changed, &settings).is_err());
        }
        let marker: Value = serde_json::from_slice(
            &fs::read(c.operational_dir.join("maestro-links-enrollment.json")).unwrap(),
        )
        .unwrap();
        assert!(marker.get(control::ENROLLMENT_FIELD).is_none());
    }
}
