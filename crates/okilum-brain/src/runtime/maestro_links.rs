//! Journal-owned observation records, independent of engine dispatch state.
use super::*;
use crate::{
    maestro::{self, Discovery, Settings},
    maestro_links::{self as api, Link, LinkRequest, Observation, UnlinkRequest},
};

impl Runner {
    pub(crate) fn maestro_guard_config(&self, settings: Option<&Settings>) -> Result<()> {
        self.state.maestro_journal.guard_config(settings)
    }
    pub(crate) fn maestro_links_writable(&self) -> bool {
        self.managed
            && api::NEW_LINKS_ENABLED
            && !self.state.maestro_journal.recovery_required
            && self.state.maestro_journal.links.len() < 256
            && self.state.maestro_journal.operations.len() < 2048
    }
    pub(crate) fn maestro_goal_links_token(&self, goal: &str) -> String {
        maestro::digest(
            &self
                .state
                .maestro_journal
                .links
                .values()
                .filter(|link| link.goal_id == goal)
                .map(|link| (&link.id, link.active))
                .collect::<Vec<_>>(),
        )
    }
    pub(crate) fn maestro_has_links(&self) -> bool {
        self.state.maestro_journal.links.values().any(|l| l.active)
    }
    pub(crate) fn maestro_link_view(&self, goal: &str) -> Result<Value> {
        ensure!(self.goal_ids().iter().any(|g| g == goal), "unknown goal");
        let j = &self.state.maestro_journal;
        Ok(
            serde_json::json!({"schema":"okilum-maestro-observation/v1","goal_id":goal,"link":j.active(goal),"history":j.links.values().filter(|l|l.goal_id==goal).collect::<Vec<_>>(),"decisions":self.maestro_decision_history(goal),"approval_sends_enabled":crate::maestro_control::NEW_DECISIONS_ENABLED,"controls_enabled":false,"recovery_required":j.recovery_required,"source_paths":{"link":j.active(goal).map(|l|self.path("maestro-link",&l.id)),"observations":j.observations.values().filter(|o|o.goal_id==goal).map(|o|(&o.id,self.path("maestro-observation",&o.id))).collect::<BTreeMap<_,_>>()}}),
        )
    }
    pub(crate) fn maestro_links_token(&self) -> String {
        maestro::digest(
            &self
                .state
                .maestro_journal
                .links
                .values()
                .filter(|l| l.active)
                .map(|l| (&l.id, &l.instance))
                .collect::<Vec<_>>(),
        )
    }
    pub(crate) fn check_maestro_inventory(&mut self) -> Result<()> {
        let marker = self.state_dir.join("maestro-links-enrollment.json");
        if marker.exists() {
            ensure!(
                crate::maestro_operations::enrollment_identity(serde_json::from_slice::<Value>(
                    &fs::read(marker)?
                )?)? == self.workspace_identity(),
                "Maestro link enrollment identity mismatch"
            );
            let disk: Value = super::journal_value(&fs::read(self.state_dir.join("state.json"))?)?;
            if disk.get("maestro_journal").is_none() {
                self.state.maestro_journal.recovery_required = true;
            }
        }
        // Canonical receipts also detect an older executable dropping new journal fields.
        for entry in fs::read_dir(self.root.join(&self.state.records_dir))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("maestro-link-") && name.ends_with(".md") {
                let id = name
                    .trim_start_matches("maestro-link-")
                    .trim_end_matches(".md");
                if !self.state.maestro_journal.links.contains_key(id) {
                    self.state.maestro_journal.recovery_required = true;
                }
            }
        }
        Ok(())
    }
    fn enroll_maestro(&self) -> Result<()> {
        let path = self.state_dir.join("maestro-links-enrollment.json");
        if path.exists() {
            return Ok(());
        }
        let mut temp = tempfile::NamedTempFile::new_in(&self.state_dir)?;
        serde_json::to_writer(&mut temp, &self.workspace_identity())?;
        temp.flush()?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(path)?;
        File::open(&self.state_dir)?.sync_all()?;
        Ok(())
    }
    pub(crate) fn maestro_link(
        &mut self,
        request: LinkRequest,
        discovery: &Discovery,
    ) -> Result<Value> {
        let operation = crate::maestro_operations::Request::Link(request.clone());
        let result = self.mutation(|runner| {
            if let Some(receipt) = runner.maestro_operation_begin(&operation)? {
                return Ok(receipt);
            }
            if !api::NEW_LINKS_ENABLED {
                return runner.maestro_reject_result(
                    &operation,
                    "new_links_disabled",
                    "New Maestro links are disabled by this maintenance build",
                );
            }
            if runner.state.maestro_journal.links.len() >= 256 {
                return runner.maestro_reject_result(
                    &operation,
                    "history_limit",
                    "Maestro link history limit reached",
                );
            }
            if runner
                .state
                .maestro_journal
                .active(&request.goal_id)
                .is_some()
            {
                return runner.maestro_reject_result(
                    &operation,
                    "link_unavailable",
                    "Goal already has linked Maestro work",
                );
            }
            let (project, _) = match api::selected(&request, discovery) {
                Ok(selected) => selected,
                Err(error) => {
                    return runner.maestro_reject_result(
                        &operation,
                        "selection_changed",
                        &error.to_string(),
                    )
                }
            };
            runner.flush_writes()?;
            runner.enroll_maestro()?;
            let link = Link {
                id: Uuid::new_v4().to_string(),
                goal_id: request.goal_id.clone(),
                instance: discovery.instance.clone(),
                project_id: request.project_id.clone(),
                project_name: request.project_name.clone(),
                project_url: project.dashboard_url.clone(),
                repo: request.repo.clone(),
                issue_number: request.issue_number,
                active: true,
                created_at: discovery.observed_at.clone(),
                last_seen: None,
                last_remote: None,
                status: "linked".into(),
                error: None,
                observation_ids: vec![],
                latest: None,
                transition: 0,
                latest_semantic: None,
                attention_generation: 0,
            };
            let receipt = serde_json::json!({
                "operation_id": request.operation_id,
                "link_id": link.id,
                "goal_id": request.goal_id,
                "linked": true,
            });
            runner.queue(
                "maestro-link",
                &link.id,
                &api::link_record(&link),
                Some(&format!(
                    "# Linked Maestro work\n\n{} #{}. Observation only; no work was started.\n",
                    link.repo, link.issue_number,
                )),
            )?;
            runner
                .state
                .maestro_journal
                .links
                .insert(link.id.clone(), link);
            runner.state.maestro_journal.operations.insert(
                request.operation_id.clone(),
                serde_json::json!({"request": request, "receipt": receipt}),
            );
            runner.persist()?;
            runner.flush_writes()?;
            runner.maestro_observe_inner(discovery)?;
            Ok(receipt)
        });
        result.map_err(|error| self.maestro_operation_error(&operation, error))
    }
    pub(crate) fn maestro_unlink(&mut self, request: UnlinkRequest) -> Result<Value> {
        let operation = crate::maestro_operations::Request::Unlink(request.clone());
        let result = self.mutation(|runner| {
            if let Some(receipt) = runner.maestro_operation_begin(&operation)? {
                return Ok(receipt);
            }
            if !runner.state.maestro_journal.links.get(&request.expected_link_id)
                .is_some_and(|link| link.active && link.goal_id == request.goal_id) {
                return runner.maestro_reject_result(&operation, "link_changed", "Maestro link changed");
            }
            runner.flush_writes()?;
            let link = runner.state.maestro_journal.links.get_mut(&request.expected_link_id).unwrap();
            link.active = false;
            link.status = "unlinked".into();
            let link = link.clone();
            runner.queue(
                "maestro-link", &link.id, &api::link_record(&link),
                Some(&format!(
                    "# Previously linked Maestro work\n\n{} #{}. Observation stopped; history preserved.\n",
                    link.repo, link.issue_number,
                )),
            )?;
            let receipt = serde_json::json!({
                "operation_id": request.operation_id,
                "link_id": link.id,
                "goal_id": request.goal_id,
                "unlinked": true,
            });
            runner.state.maestro_journal.operations.insert(
                request.operation_id.clone(),
                serde_json::json!({"request": request, "receipt": receipt}),
            );
            runner.persist()?;
            runner.flush_writes()?;
            Ok(receipt)
        });
        result.map_err(|error| self.maestro_operation_error(&operation, error))
    }
    pub(crate) fn maestro_observe(&mut self, d: &Discovery) -> Result<()> {
        self.mutation(|r| {
            r.flush_writes()?;
            r.maestro_observe_inner(d)
        })
    }
    fn maestro_observe_inner(&mut self, d: &Discovery) -> Result<()> {
        ensure!(
            !self.state.maestro_journal.recovery_required,
            "Maestro recovery must be reconciled"
        );
        let ids = self
            .state
            .maestro_journal
            .links
            .values()
            .filter(|l| l.active && l.instance == d.instance)
            .map(|l| l.id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let mut link = self.state.maestro_journal.links[&id].clone();
            let remote_time = time::OffsetDateTime::parse(
                &d.refreshed_at,
                &time::format_description::well_known::Rfc3339,
            )?;
            let previous_time = link
                .last_remote
                .as_ref()
                .map(|t| {
                    time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                })
                .transpose()?;
            if previous_time.is_some_and(|t| t > remote_time) {
                continue;
            }
            let same_snapshot = previous_time == Some(remote_time);
            let project = d.projects.iter().find(|p| {
                p.project_id == link.project_id
                    && p.name == link.project_name
                    && p.repo == link.repo
            });
            let issue =
                project.and_then(|p| p.issues.iter().find(|i| i.number == link.issue_number));
            link.last_seen = Some(d.observed_at.clone());
            link.last_remote = Some(d.refreshed_at.clone());
            if project.is_none_or(|p| p.stale) || issue.is_none() {
                if link.status != "unknown" {
                    link.attention_generation = link
                        .attention_generation
                        .checked_add(1)
                        .context("Maestro attention generation exhausted")?;
                }
                link.status = "unknown".into();
                link.error = Some(
                    "Linked Maestro work is missing, changed or stale; last evidence is retained."
                        .into(),
                );
                self.state.maestro_journal.links.insert(id, link);
                continue;
            }
            let project = project.unwrap();
            let issue = issue.unwrap();
            link.status = "observed".into();
            link.error = None;
            link.project_url = project.dashboard_url.clone();
            // An unchanged provider snapshot can still age or recover connectivity.
            // Its evidence cannot change until a newer provider snapshot arrives.
            if same_snapshot {
                self.state.maestro_journal.links.insert(id, link);
                continue;
            }
            let canonical_issue = api::canonical_issue(issue);
            let semantic = maestro::digest(&(link.id.clone(), project.paused, &canonical_issue));
            if link.latest_semantic.as_ref() != Some(&semantic) {
                link.transition = link
                    .transition
                    .checked_add(1)
                    .context("Maestro observation sequence exhausted")?;
                link.latest_semantic = Some(semantic.clone());
            }
            let transition_key = maestro::digest(&(&link.id, link.transition, &semantic));
            let observation_id =
                Uuid::new_v5(&Uuid::NAMESPACE_URL, transition_key.as_bytes()).to_string();
            let observation = Observation {
                id: observation_id.clone(),
                link_id: id.clone(),
                goal_id: link.goal_id.clone(),
                observed_at: d.observed_at.clone(),
                remote_at: d.refreshed_at.clone(),
                paused: project.paused,
                issue: issue.clone(),
                verification: "unverified".into(),
            };
            if !self
                .state
                .maestro_journal
                .observations
                .contains_key(&observation_id)
            {
                ensure!(
                    link.observation_ids.len() < 512,
                    "Maestro observation history limit reached"
                );
                let mut canonical_observation = observation.clone();
                canonical_observation.issue = canonical_issue;
                self.queue(
                    "maestro-observation",
                    &observation_id,
                    &canonical_observation,
                    Some(&api::observation_body(&observation, &link)),
                )?;
                link.observation_ids.push(observation_id.clone());
                self.state
                    .maestro_journal
                    .observations
                    .insert(observation_id, observation.clone());
            }
            link.latest = Some(observation);
            self.state.maestro_journal.links.insert(id, link);
        }
        ensure!(
            serde_json::to_vec(&self.state.maestro_journal)?.len() <= 32 * 1024 * 1024,
            "Maestro retained observation budget reached"
        );
        self.persist()?;
        self.flush_writes()
    }
    pub(crate) fn maestro_failed(&mut self, instance: &Value, persistence: bool) -> Result<()> {
        self.mutation(|r| {
            for link in r
                .state
                .maestro_journal
                .links
                .values_mut()
                .filter(|l| l.active && &l.instance == instance)
            {
                let next = if persistence {
                    "recovery_required"
                } else {
                    "disconnected"
                };
                if link.status != next {
                    link.attention_generation = link
                        .attention_generation
                        .checked_add(1)
                        .context("Maestro attention generation exhausted")?;
                }
                link.status = next.into();
                link.error = Some(
                    if persistence {
                        "Maestro observation could not be retained; previous evidence is preserved."
                    } else {
                        "Maestro is unavailable; last observed evidence is retained."
                    }
                    .into(),
                );
            }
            r.persist()
        })
    }
    pub(crate) fn maestro_attention(&self, goal: &str) -> Vec<Value> {
        let Some(link) = self.state.maestro_journal.active(goal) else {
            return vec![];
        };
        let message = if let Some(error) = &link.error {
            Some((
                "blocker",
                error.clone(),
                format!("connection-{}", link.attention_generation),
            ))
        } else {
            link.latest.as_ref().and_then(|observation| {
                if observation.issue.approvals.iter().any(|approval| approval.status == "pending") {
                    Some((
                        "decision",
                        "Linked Maestro work requires a decision. Open the original approval in Maestro.".into(),
                        observation.id.clone(),
                    ))
                } else if observation.issue.attempts.iter().any(|attempt| attempt.needs_attention) {
                    Some((
                        "blocker",
                        "Linked Maestro work needs attention. Open its original context.".into(),
                        observation.id.clone(),
                    ))
                } else {
                    let in_progress = observation.issue.attempts.iter().any(|attempt| {
                        attempt.live || ["running", "queued", "pr_open"].contains(&attempt.status.as_str())
                    });
                    let landed = observation.issue.attempts.iter().any(|attempt| {
                        attempt.generation.is_some() && attempt.started_at.is_some()
                            && ["code_landed", "merged", "completed"].contains(&attempt.status.as_str())
                    });
                    (!in_progress && landed).then(|| (
                        "final",
                        "Maestro reports an outcome for linked work; evidence remains unverified.".into(),
                        observation.id.clone(),
                    ))
                }
            })
        };
        message.map(|(kind,message,event)|serde_json::json!({"attention_id":format!("maestro-{}-{event}",link.id),"goal_id":goal,"stage_id":null,"current_stage_id":null,"result_id":null,"kind":kind,"message":message})).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Runner, RunnerConfig) {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("brain/records")).unwrap();
        fs::create_dir(t.path().join("runtime")).unwrap();
        let config = RunnerConfig {
            brain_id: Uuid::new_v4().to_string(),
            root: t.path().join("brain"),
            operational_dir: t.path().join("runtime"),
            records_dir: "records".into(),
            boundary: okilum_core::source::WriteBoundary::Managed,
        };
        let mut runner = Runner::open(RunnerConfig {
            brain_id: config.brain_id.clone(),
            root: config.root.clone(),
            operational_dir: config.operational_dir.clone(),
            records_dir: config.records_dir.clone(),
            boundary: config.boundary,
        })
        .unwrap();
        runner
            .create_goal(
                Goal {
                    id: "01000000-0000-4000-8000-000000000042".into(),
                    title: "Fixture".into(),
                    status: "active".into(),
                    criteria: vec![Criterion {
                        id: "C1".into(),
                        description: "Observed evidence still needs verification".into(),
                        requires_human: false,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: BTreeMap::new(),
                },
                "# Fixture\n".into(),
            )
            .unwrap();
        (t, runner, config)
    }
    fn request(d: &Discovery) -> LinkRequest {
        let p = &d.projects[0];
        let i = &p.issues[0];
        LinkRequest {
            operation_id: Uuid::new_v4().to_string(),
            goal_id: "01000000-0000-4000-8000-000000000042".into(),
            selection_guard: maestro::selection_guard(&d.instance, p, i),
            project_id: p.project_id.clone(),
            project_name: p.name.clone(),
            repo: p.repo.clone(),
            issue_number: i.number,
        }
    }
    #[test]
    fn pending_and_abandoned_intents_survive_restart_and_prevent_delayed_first_link() {
        use crate::maestro_operations::Request;
        let (_t, mut r, c) = fixture();
        let d = maestro::tests::discovery();
        let req = request(&d);
        let operation = Request::Link(req.clone());
        let unknown = r
            .maestro_operation_get(&req.goal_id, &req.operation_id)
            .unwrap();
        assert_eq!(unknown.status, "unknown");
        assert!(unknown.request.is_none());
        assert!(r.maestro_operation_begin(&operation).unwrap().is_none());
        drop(r);
        let mut r = Runner::open(RunnerConfig {
            brain_id: c.brain_id.clone(),
            root: c.root.clone(),
            operational_dir: c.operational_dir.clone(),
            records_dir: c.records_dir.clone(),
            boundary: c.boundary,
        })
        .unwrap();
        let pending = r
            .maestro_operation_get(&req.goal_id, &req.operation_id)
            .unwrap();
        assert_eq!(pending.status, "pending");
        assert_eq!(pending.request, Some(operation.body()));
        let abandoned = r
            .maestro_operation_reject(&operation, "abandoned", "Abandoned locally")
            .unwrap();
        assert_eq!(abandoned.status, "rejected");
        // Tombstone an ID whose first request has not even arrived.
        let delayed = request(&d);
        r.maestro_operation_reject(
            &Request::Link(delayed.clone()),
            "abandoned",
            "Abandoned locally",
        )
        .unwrap();
        drop(r);
        let mut r = Runner::open(c).unwrap();
        for original in [req, delayed] {
            let error = r.maestro_link(original.clone(), &d).unwrap_err();
            let typed = error
                .downcast_ref::<crate::maestro_operations::Error>()
                .unwrap();
            assert_eq!(typed.disposition.status, "rejected");
            assert_eq!(
                typed.disposition.rejection.as_ref().unwrap().code,
                "abandoned"
            );
            assert!(r.maestro_link_view(&original.goal_id).unwrap()["link"].is_null());
        }
        assert!(r.state.maestro_journal.links.is_empty());
    }
    #[test]
    fn committed_receipt_wins_abandonment_and_operation_identity_is_immutable() {
        use crate::maestro_operations::Request;
        let (_t, mut r, c) = fixture();
        let d = maestro::tests::discovery();
        let req = request(&d);
        let operation = Request::Link(req.clone());
        let receipt = r.maestro_link(req.clone(), &d).unwrap();
        let committed = r
            .maestro_operation_reject(&operation, "abandoned", "Abandoned locally")
            .unwrap();
        assert_eq!(committed.status, "committed");
        assert_eq!(committed.receipt, Some(receipt.clone()));
        let mut changed = req.clone();
        changed.issue_number += 1;
        assert!(r
            .maestro_operation_reject(&Request::Link(changed), "abandoned", "Changed")
            .is_err());
        let unlink = UnlinkRequest {
            operation_id: req.operation_id.clone(),
            goal_id: req.goal_id.clone(),
            expected_link_id: receipt["link_id"].as_str().unwrap().into(),
        };
        assert!(r
            .maestro_operation_reject(&Request::Unlink(unlink.clone()), "abandoned", "Changed")
            .is_err());
        let unlink = UnlinkRequest {
            operation_id: Uuid::new_v4().to_string(),
            ..unlink
        };
        let unlinked = r.maestro_unlink(unlink.clone()).unwrap();
        let abandoned = r
            .maestro_operation_reject(&Request::Unlink(unlink.clone()), "abandoned", "Lost reply")
            .unwrap();
        assert_eq!(abandoned.receipt, Some(unlinked.clone()));
        drop(r);
        let mut r = Runner::open(c).unwrap();
        assert_eq!(r.maestro_unlink(unlink.clone()).unwrap(), unlinked);
        let stale = UnlinkRequest {
            operation_id: Uuid::new_v4().to_string(),
            ..unlink
        };
        let error = r.maestro_unlink(stale.clone()).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::maestro_operations::Error>()
                .unwrap()
                .disposition
                .rejection
                .as_ref()
                .unwrap()
                .code,
            "link_changed"
        );
        assert_eq!(
            r.maestro_operation_get(&stale.goal_id, &stale.operation_id)
                .unwrap()
                .status,
            "rejected"
        );
    }
    #[test]
    fn source_projection_failure_reports_committed_and_restart_recovers_receipt() {
        let (_t, mut r, c) = fixture();
        let d = maestro::tests::discovery();
        let req = request(&d);
        r.interrupt_after_write = Some(1);
        let error = r.maestro_link(req.clone(), &d).unwrap_err();
        let disposition = &error
            .downcast_ref::<crate::maestro_operations::Error>()
            .unwrap()
            .disposition;
        assert_eq!(disposition.status, "committed");
        assert!(!r.state.pending_writes.is_empty());
        let receipt = disposition.receipt.clone().unwrap();
        drop(r);
        let mut r = Runner::open(c).unwrap();
        assert!(r.state.pending_writes.is_empty());
        assert_eq!(r.maestro_link(req, &d).unwrap(), receipt);
        assert_eq!(r.state.maestro_journal.links.len(), 1);
    }
    #[test]
    fn restart_replay_and_duplicate_polls_preserve_one_link_and_no_stage() {
        let (_t, mut r, c) = fixture();
        let d = maestro::tests::discovery();
        let req = request(&d);
        let goal = r.goal_source().unwrap();
        let receipt = r.maestro_link(req.clone(), &d).unwrap();
        assert_eq!(r.maestro_link(req.clone(), &d).unwrap(), receipt);
        r.maestro_observe(&d).unwrap();
        assert_eq!(r.state.maestro_journal.observations.len(), 1);
        assert!(r.snapshot().unwrap().stage.is_none());
        assert_eq!(r.goal_source().unwrap(), goal);
        drop(r);
        let mut r = Runner::open(c).unwrap();
        assert_eq!(r.maestro_link(req.clone(), &d).unwrap(), receipt);
        assert_eq!(r.state.maestro_journal.observations.len(), 1);
        let mut changed = req;
        changed.issue_number = 43;
        assert!(r.maestro_link(changed, &d).is_err());
    }
    #[test]
    fn approval_only_link_observation_and_restart_never_invent_execution() {
        let (_t, mut r, c) = fixture();
        let d = maestro::tests::approval_only_discovery();
        let req = request(&d);
        let goal = r.goal_source().unwrap();
        let receipt = r.maestro_link(req.clone(), &d).unwrap();
        r.maestro_observe(&d).unwrap();
        let observation = r
            .state
            .maestro_journal
            .observations
            .values()
            .next()
            .unwrap();
        assert!(observation.issue.attempts.is_empty());
        assert_eq!(observation.issue.approvals[0].id, "approval-1");
        assert_eq!(observation.verification, "unverified");
        let attention = r.maestro_attention(&req.goal_id);
        assert_eq!(attention.len(), 1);
        assert_eq!(attention[0]["kind"], "decision");
        assert!(attention[0]["stage_id"].is_null());
        assert!(r.snapshot().unwrap().stage.is_none());
        assert_eq!(r.goal_source().unwrap(), goal);
        drop(r);
        let mut r = Runner::open(c).unwrap();
        assert_eq!(r.maestro_link(req.clone(), &d).unwrap(), receipt);
        assert!(r
            .state
            .maestro_journal
            .links
            .values()
            .next()
            .unwrap()
            .latest
            .as_ref()
            .unwrap()
            .issue
            .attempts
            .is_empty());
        let mut decided = d.clone();
        decided.refreshed_at = "2026-09-07T01:00:15Z".into();
        decided.observed_at = "2026-09-07T01:00:16Z".into();
        decided.projects[0].issues[0].approvals[0].status = "approved".into();
        r.maestro_observe(&decided).unwrap();
        assert!(r.maestro_attention(&req.goal_id).is_empty());
        assert_eq!(r.goal_source().unwrap(), goal);
        assert!(r.snapshot().unwrap().stage.is_none());
    }
    #[test]
    fn approval_only_link_rejects_changed_selection_and_stale_snapshot() {
        let (_t, mut r, _) = fixture();
        let d = maestro::tests::approval_only_discovery();
        for stale in [false, true] {
            let req = request(&d);
            let mut changed = d.clone();
            if stale {
                changed.projects[0].stale = true;
            } else {
                changed.projects[0].issues[0].approvals[0].status = "rejected".into();
            }
            assert!(r.maestro_link(req, &changed).is_err());
            assert!(r.state.maestro_journal.links.is_empty());
            assert!(r.state.maestro_journal.observations.is_empty());
        }
    }
    #[test]
    fn new_generation_retains_old_evidence_and_old_poll_cannot_regress_it() {
        let (_t, mut r, _) = fixture();
        let d = maestro::tests::discovery();
        let req = request(&d);
        r.maestro_link(req, &d).unwrap();
        let first = r
            .state
            .maestro_journal
            .observations
            .values()
            .next()
            .unwrap()
            .clone();
        let mut next = d.clone();
        next.refreshed_at = "2026-09-07T01:00:15Z".into();
        next.observed_at = "2026-09-07T01:00:16Z".into();
        next.projects[0].issues[0].attempts[0].generation = Some(2);
        r.maestro_observe(&next).unwrap();
        r.maestro_observe(&d).unwrap();
        assert_eq!(r.state.maestro_journal.observations.len(), 2);
        let link = r.state.maestro_journal.links.values().next().unwrap();
        assert_eq!(
            link.latest.as_ref().unwrap().issue.attempts[0].generation,
            Some(2)
        );
        assert_eq!(
            r.state.maestro_journal.observations[&first.id]
                .issue
                .attempts[0]
                .generation,
            Some(1)
        );
    }
    #[test]
    fn unlink_discards_late_observation_and_settings_cannot_reroute_active_work() {
        let (_t, mut r, _) = fixture();
        let d = maestro::tests::discovery();
        let req = request(&d);
        r.maestro_link(req.clone(), &d).unwrap();
        assert!(r.maestro_guard_config(None).is_err());
        let before = r.maestro_links_token();
        let id = r
            .state
            .maestro_journal
            .active(&req.goal_id)
            .unwrap()
            .id
            .clone();
        let u = UnlinkRequest {
            operation_id: Uuid::new_v4().to_string(),
            goal_id: req.goal_id.clone(),
            expected_link_id: id,
        };
        let receipt = r.maestro_unlink(u.clone()).unwrap();
        assert_eq!(r.maestro_unlink(u).unwrap(), receipt);
        assert_ne!(before, r.maestro_links_token());
        r.maestro_observe(&d).unwrap();
        assert_eq!(r.state.maestro_journal.observations.len(), 1);
        assert!(r.maestro_guard_config(None).is_ok());
        assert!(r.maestro_attention(&req.goal_id).is_empty());
    }
    #[test]
    fn unlink_canonical_receipt_excludes_live_provider_prose_and_polling_bookkeeping() {
        let (_t, mut r, _) = fixture();
        let mut d = maestro::tests::discovery();
        d.projects[0].issues[0].attempts[0].reason = "transient provider explanation".into();
        let req = request(&d);
        let receipt = r.maestro_link(req.clone(), &d).unwrap();
        let id = receipt["link_id"].as_str().unwrap().to_owned();
        r.maestro_unlink(UnlinkRequest {
            operation_id: Uuid::new_v4().to_string(),
            goal_id: req.goal_id,
            expected_link_id: id.clone(),
        })
        .unwrap();
        let note = r.source.read(&r.path("maestro-link", &id)).unwrap();
        let text = String::from_utf8(STANDARD.decode(note.content_base64).unwrap()).unwrap();
        assert!(!text.contains("transient provider explanation"));
        assert!(!text.contains("last_remote") && !text.contains("attention_generation"));
        assert!(text.contains("active: false") && text.contains("Observation stopped"));
        assert!(r.state.maestro_journal.links[&id].latest.is_some());
    }
    #[test]
    fn unchanged_snapshot_still_ages_and_recovers_connectivity_without_new_evidence() {
        let (_t, mut r, _) = fixture();
        let mut d = maestro::tests::discovery();
        let req = request(&d);
        r.maestro_link(req.clone(), &d).unwrap();
        r.maestro_failed(&d.instance, false).unwrap();
        assert!(!r.maestro_attention(&req.goal_id).is_empty());
        r.maestro_observe(&d).unwrap();
        assert!(r.maestro_attention(&req.goal_id).is_empty());
        let first = r
            .state
            .maestro_journal
            .active(&req.goal_id)
            .unwrap()
            .latest
            .clone()
            .unwrap();
        d.projects[0].stale = true;
        d.observed_at = "2026-09-07T01:16:00Z".into();
        r.maestro_observe(&d).unwrap();
        let link = r.state.maestro_journal.active(&req.goal_id).unwrap();
        assert_eq!(link.status, "unknown");
        assert_eq!(link.last_seen.as_deref(), Some(d.observed_at.as_str()));
        assert_eq!(link.latest.as_ref().unwrap().id, first.id);
        assert_eq!(r.state.maestro_journal.observations.len(), 1);
        assert!(!r.maestro_attention(&req.goal_id).is_empty());
    }
    #[test]
    fn journal_loss_after_enrollment_is_visible_and_never_recreates_link() {
        let (t, mut r, c) = fixture();
        let d = maestro::tests::discovery();
        let req = request(&d);
        r.maestro_link(req.clone(), &d).unwrap();
        drop(r);
        let path = t.path().join("runtime/state.json");
        let mut disk: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        disk.as_object_mut().unwrap().remove("maestro_journal");
        fs::write(path, serde_json::to_vec(&disk).unwrap()).unwrap();
        let mut r = Runner::open(c).unwrap();
        assert!(r.state.maestro_journal.recovery_required);
        assert!(!r.maestro_links_writable());
        assert!(r.maestro_link(req, &d).is_err());
    }
    fn raw_attention(r: &mut Runner) -> Result<Vec<Value>> {
        crate::application::Application::unconfigured().attention_items(r)
    }
    fn attention_list(r: &mut Runner) -> crate::attention::List {
        let raw = raw_attention(r).unwrap();
        r.attention_list(raw, "operator", None, None, None).unwrap()
    }
    #[test]
    fn recurring_blocker_after_ack_and_recovery_has_new_durable_transition() {
        let (_t, mut r, c) = fixture();
        let mut a = maestro::tests::discovery();
        a.projects[0].issues[0].attempts[0].needs_attention = true;
        let req = request(&a);
        r.maestro_link(req, &a).unwrap();
        let old = attention_list(&mut r)
            .items
            .into_iter()
            .find(|i| i.current)
            .unwrap();
        let op = Uuid::new_v4().to_string();
        let mutation = crate::attention::Mutation {
            operation_id: op.clone(),
            target: crate::attention::Target {
                goal_id: old.goal_id.clone(),
                attention_id: old.attention_id.clone(),
                expected_revision: old.revision.clone(),
                stage_id: None,
            },
            source: crate::inbox::SourceIdentity {
                channel: "native".into(),
                instance_id: Uuid::new_v4().to_string(),
                account_id: "local".into(),
                actor_id: "operator".into(),
                chat_id: None,
                topic_id: None,
                message_id: op.clone(),
                update_id: op,
                uri: None,
            },
        };
        r.attention_mutate(mutation, "ack_seen", None, "operator", raw_attention)
            .unwrap();
        let mut b = a.clone();
        b.refreshed_at = "2026-09-07T01:00:15Z".into();
        b.projects[0].issues[0].attempts[0].needs_attention = false;
        r.maestro_observe(&b).unwrap();
        a.refreshed_at = "2026-09-07T01:00:30Z".into();
        r.maestro_observe(&a).unwrap();
        let new = attention_list(&mut r)
            .items
            .into_iter()
            .find(|i| i.current)
            .unwrap();
        assert_ne!(old.attention_id, new.attention_id);
        assert!(!new.seen);
        assert_eq!(r.state.maestro_journal.observations.len(), 3);
        drop(r);
        let mut r = Runner::open(c).unwrap();
        a.refreshed_at = "2026-09-07T01:00:45Z".into();
        r.maestro_observe(&a).unwrap();
        assert_eq!(r.state.maestro_journal.observations.len(), 3);
        assert_eq!(
            attention_list(&mut r)
                .items
                .into_iter()
                .find(|i| i.current)
                .unwrap()
                .attention_id,
            new.attention_id
        );
    }
    #[test]
    fn recurring_connection_failure_is_new_but_consecutive_failures_dedup() {
        let (_t, mut r, _) = fixture();
        let mut d = maestro::tests::discovery();
        let req = request(&d);
        r.maestro_link(req.clone(), &d).unwrap();
        r.maestro_failed(&d.instance, false).unwrap();
        let first = r.maestro_attention(&req.goal_id)[0]["attention_id"].clone();
        r.maestro_failed(&d.instance, false).unwrap();
        assert_eq!(r.maestro_attention(&req.goal_id)[0]["attention_id"], first);
        d.refreshed_at = "2026-09-07T01:00:15Z".into();
        r.maestro_observe(&d).unwrap();
        assert!(r.maestro_attention(&req.goal_id).is_empty());
        r.maestro_failed(&d.instance, false).unwrap();
        assert_ne!(r.maestro_attention(&req.goal_id)[0]["attention_id"], first);
    }
    #[test]
    fn changed_approval_summary_updates_live_projection_without_claiming_payload_revision() {
        let (_t, mut r, _) = fixture();
        let mut d = maestro::tests::discovery();
        d.projects[0].issues[0].approvals.push(maestro::Approval {
            id: "same-approval".into(),
            action: "edit_issue_body".into(),
            status: "pending".into(),
            summary: "First displayed proposal".into(),
            dashboard_url: None,
        });
        let req = request(&d);
        r.maestro_link(req.clone(), &d).unwrap();
        let old = r.maestro_attention(&req.goal_id)[0]["attention_id"].clone();
        d.refreshed_at = "2026-09-07T01:00:15Z".into();
        d.projects[0].issues[0].approvals[0].summary = "Updated provider proposal".into();
        r.maestro_observe(&d).unwrap();
        assert_eq!(r.state.maestro_journal.observations.len(), 1);
        assert_eq!(r.maestro_attention(&req.goal_id)[0]["attention_id"], old);
        let link = r.state.maestro_journal.active(&req.goal_id).unwrap();
        assert_eq!(
            link.latest.as_ref().unwrap().issue.approvals[0].summary,
            "Updated provider proposal"
        );
        let note = r
            .source
            .read(&r.path("maestro-observation", &link.observation_ids[0]))
            .unwrap();
        let bytes = STANDARD.decode(&note.content_base64).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            !text.contains("First displayed proposal")
                && !text.contains("Updated provider proposal")
        );
    }
}
