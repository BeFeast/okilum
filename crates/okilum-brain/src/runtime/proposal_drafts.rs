use super::*;
use crate::{proposal as api, proposals::Store};

impl Runner {
    pub(super) fn open_proposal_drafts(&mut self) -> Result<()> {
        if !self.source.required_proposal_drafts() {
            let path = self.state_dir.join("proposal-intents-v1/journal.json");
            if path.exists() {
                let state: Value = serde_json::from_slice(&fs::read(path)?)?;
                ensure!(
                    state.get("drafts_enabled") != Some(&Value::Bool(true)),
                    "proposal draft state lacks required source fence"
                );
            }
            return Ok(());
        }
        let binding = self
            .source
            .required_proposal_feed()
            .context("proposal drafts need feed binding")?;
        self.proposal_store = Some(Store::open_bound_without_recovery(
            &self.state_dir,
            &self.state.brain_id,
            binding,
        )?);
        ensure!(
            !self.proposal_store.as_ref().unwrap().adoption_enabled()
                || self.source.required_proposal_adoption(),
            "proposal adoption state lacks required source fence"
        );
        ensure!(
            self.source.required_proposal_generation()
                || !self
                    .drafts()?
                    .intents()?
                    .values()
                    .any(|i| i.generation_issue.is_some()
                        || i.attempt
                            .input
                            .as_ref()
                            .is_some_and(|v| v.generation.is_some())),
            "generation state lacks required source fence"
        );
        self.proposal_store.as_mut().unwrap().enable_drafts()?;
        for intent in self.drafts()?.intents()?.values() {
            if let Some(draft) = &intent.draft {
                if let Some(adoption) = &draft.adoption {
                    if let Some(receipt) = &adoption.target_receipt {
                        let target = adoption.target()?;
                        let original = self
                            .source
                            .recovery_record(&target.source_write().operation_id)?;
                        ensure!(
                            original.request == *target.source_write()
                                && original.receipt.as_ref() == Some(receipt),
                            "adoption original target receipt mismatch"
                        );
                    }
                }
                for projection in &draft.projections {
                    if let Some(receipt) = &projection.receipt {
                        let original = self
                            .source
                            .recovery_record(&projection.write.operation_id)?;
                        ensure!(
                            original.request == projection.write
                                && original.receipt.as_ref() == Some(receipt),
                            "proposal original source receipt mismatch"
                        );
                    }
                }
            }
        }
        self.validate_inbox_adoption_inventory()?;
        self.proposal_store.as_mut().unwrap().recover()?;
        // A retained proposal source conflict must not disable ordinary Inbox,
        // other goals or reads. Its exact operation remains pending for review.
        self.proposal_draft_issue = self
            .recover_draft_projections()
            .err()
            .map(|_| "proposal projection requires recovery".into());
        Ok(())
    }
    pub(crate) fn proposals_readable(&self) -> bool {
        self.proposal_store.is_some()
    }
    pub(crate) fn proposals_writable(&self) -> bool {
        self.managed && self.proposals_readable()
    }
    pub(super) fn proposal_identity_reserved(&self, op: &str, external: &str) -> Result<bool> {
        if self.suggestions_identity_reserved(op) {
            return Ok(true);
        }
        self.proposal_store
            .as_ref()
            .map(|s| s.operation_reserved(op, external))
            .transpose()
            .map(|v| v.unwrap_or(false))
    }
    pub(super) fn drafts(&self) -> Result<&Store> {
        self.proposal_store
            .as_ref()
            .context("proposal drafts unavailable; workspace is not enrolled")
    }
    pub(super) fn recover_draft_projections(&mut self) -> Result<()> {
        let ids = self
            .drafts()?
            .intents()?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for id in ids {
            if let Err(error) = self.recover_draft_projections_for(Some(&id)) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    pub(super) fn recover_draft_projections_for(&mut self, proposal: Option<&str>) -> Result<()> {
        while let Some((id, write)) = self.drafts()?.pending_projection_for(proposal)? {
            #[cfg(test)]
            self.draft_fault(Fault::BeforeProjection)?;
            let receipt = self.source.write(write.clone())?;
            #[cfg(test)]
            self.draft_fault(Fault::AfterProjection)?;
            self.proposal_store
                .as_mut()
                .unwrap()
                .acknowledge_projection(&id, &write, receipt)?;
            #[cfg(test)]
            self.draft_fault(Fault::AfterReceipt)?;
        }
        Ok(())
    }
    pub(super) fn validate_proposal_input(
        &self,
        trigger: &crate::proposals::CommittedTrigger,
        input: &api::CapturedInput,
    ) -> Result<()> {
        ensure!(
            input.trigger_source.brain_id == self.state.brain_id
                && input.trigger_source.path == trigger.source_path
                && input.trigger_source.revision == trigger.identity.source_revision
                && self.source.read_bounded(&trigger.source_path, 64 * 1024)?
                    == input.trigger_source,
            "proposal trigger source changed"
        );
        ensure!(
            input.citations.len() <= 20
                && input
                    .citations
                    .iter()
                    .map(|c| c.excerpt.len())
                    .sum::<usize>()
                    <= 48 * 1024
                && serde_json::to_vec(input)?.len() <= 64 * 1024
                && input.omissions.len() <= 50
                && input.omissions.iter().all(|s| s.len() <= 4096),
            "proposal input exceeds bound"
        );
        let mut ids = std::collections::BTreeSet::new();
        match &trigger.goal_id {
            None => ensure!(
                input.goal_revision.is_none() && input.citations.is_empty(),
                "unplanned Inbox input cannot infer goal knowledge"
            ),
            Some(goal) => {
                let current = self.source.read(&self.path("goal", goal))?;
                ensure!(
                    Some(&current.revision) == input.goal_revision.as_ref(),
                    "proposal goal revision changed"
                );
                for citation in &input.citations {
                    ensure!(
                        ids.insert(&citation.citation_id),
                        "duplicate proposal citation"
                    );
                    crate::retrieval::validate_citation(
                        &self.source.read_bounded(&citation.path, 1024 * 1024)?,
                        citation,
                        goal,
                        "goal",
                        &self.state.records_dir,
                    )?;
                }
            }
        }
        Ok(())
    }
    pub fn proposal_get(&self, request: api::Lookup) -> Result<api::Detail> {
        let draft = self.drafts()?.draft(
            &self.state.brain_id,
            request.goal_id.as_deref(),
            &request.proposal_id,
        )?;
        let (record, source) = draft.visible()?;
        let current = self.source.read_bounded(&source.path, 1024 * 1024).ok();
        let mut stale_reasons = vec![];
        if current
            .as_ref()
            .is_none_or(|s| s.revision != source.revision)
        {
            stale_reasons.push("canonical proposal source changed or unavailable".into());
        }
        if self
            .validate_proposal_input(&record.trigger, &record.captured)
            .is_err()
        {
            stale_reasons.push("captured proposal input changed, incomplete or unavailable".into());
        }
        Ok(api::Detail {
            record,
            source,
            current_revision: current.map(|s| s.revision),
            stale_reasons,
            projection_pending: draft.pending()
                || draft.inbox_adoption.as_ref().is_some_and(|a| !a.projected)
                || draft.adoption.as_ref().is_some_and(|a| a.pointer.is_none()),
        })
    }
    pub fn proposal_list(&self, request: api::ListRequest) -> Result<api::Page> {
        ensure!(
            request.limit > 0 && request.limit <= 100,
            "proposal page limit must be1-100"
        );
        if let Some(goal) = &request.goal_id {
            uuid(goal)?;
        }
        let store = self.drafts()?;
        let mut ids = store
            .intents()?
            .iter()
            .filter(|(_, i)| i.trigger.goal_id == request.goal_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if let Some(cursor) = &request.cursor {
            ensure!(
                ids.contains(cursor),
                "proposal cursor is outside owner scope"
            );
            ids.retain(|id| id > cursor);
        }
        let more = ids.len() > request.limit;
        ids.truncate(request.limit);
        let next_cursor = if more { ids.last().cloned() } else { None };
        let items = ids
            .iter()
            .filter(|id| store.intents().is_ok_and(|v| v[*id].draft.is_some()))
            .cloned()
            .map(|proposal_id| {
                self.proposal_get(api::Lookup {
                    proposal_id,
                    goal_id: request.goal_id.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let generation = ids
            .iter()
            .filter_map(|id| {
                let i = &store.intents().ok()?[id];
                i.draft.is_none().then(|| api::QueuedGeneration {
                    proposal_id: id.clone(),
                    trigger: i.trigger.clone(),
                    attempt: i.attempt.clone(),
                    issue: i.generation_issue.clone(),
                })
            })
            .collect();
        Ok(api::Page {
            generation,
            items,
            next_cursor,
            backlog: self
                .proposal_backlog()
                .or(self.proposal_draft_issue.clone()),
        })
    }
    /// The terminal outcome is a durable reservation, never an error-string heuristic.
    pub fn proposal_disposition_outcome(
        &mut self,
        request: api::Request,
        actor: &str,
    ) -> Result<api::DeliveryOutcome> {
        request.validate(actor)?;
        if let Some(receipt) = self
            .drafts()?
            .terminal_replay(&self.workspace_identity(), &request)?
        {
            return Ok(api::DeliveryOutcome::NotApplied(Box::new(receipt)));
        }
        match self.proposal_disposition(request.clone(), actor) {
            Ok(receipt) => Ok(api::DeliveryOutcome::Committed(Box::new(receipt))),
            Err(error) => {
                let Some(expired) = error.downcast_ref::<ExpiredSnooze>() else {
                    return Err(error);
                };
                let (path, at) = (expired.path.clone(), expired.at.clone());
                self.source.require_proposal_terminal()?;
                let workspace = self.workspace_identity();
                let receipt = self
                    .proposal_store
                    .as_mut()
                    .unwrap()
                    .reserve_expired(workspace, request, path, at)?;
                Ok(api::DeliveryOutcome::NotApplied(Box::new(receipt)))
            }
        }
    }
    pub fn proposal_disposition(
        &mut self,
        request: api::Request,
        actor: &str,
    ) -> Result<api::Receipt> {
        request.validate(actor)?;
        ensure!(
            self.drafts()?
                .terminal_replay(&self.workspace_identity(), &request)?
                .is_none(),
            "proposal operation has a terminal no-write outcome"
        );
        let external = request.source.external_key()?;
        ensure!(
            !self.inbox_identity_reserved(&request.operation_id, &external)
                && !self.attention_identity_reserved(&request.operation_id, &external)
                && !self.plan_identity_reserved(&request.operation_id, &external),
            "proposal identity belongs to another operation"
        );
        if self.drafts()?.disposition_pending(&request)? {
            self.recover_draft_projections_for(Some(&request.proposal_id))?;
        }
        // Exact committed replay precedes current source checks, including an expired snooze.
        if let Some(receipt) = self
            .drafts()?
            .disposition_replay(&self.state.brain_id, &request)?
        {
            return Ok(receipt);
        }
        ensure!(
            !self.proposal_identity_reserved(&request.operation_id, &external)?,
            "proposal identity belongs to another operation"
        );
        let draft = self.drafts()?.draft(
            &self.state.brain_id,
            request.goal_id.as_deref(),
            &request.proposal_id,
        )?;
        ensure!(
            !matches!(draft.record.disposition, api::Disposition::Rejected),
            "proposal already rejected"
        );
        ensure!(
            draft.adoption.is_none() && draft.inbox_adoption.is_none(),
            "proposal adoption locks disposition"
        );
        ensure!(
            self.proposals_writable(),
            "proposal workspace is not writable"
        );
        self.recover_draft_projections_for(Some(&request.proposal_id))?;
        let detail = self.proposal_get(api::Lookup {
            proposal_id: request.proposal_id.clone(),
            goal_id: request.goal_id.clone(),
        })?;
        ensure!(
            detail.source.revision == request.expected_revision
                && detail.stale_reasons.is_empty()
                && !detail.projection_pending,
            "proposal or input changed; inspect current sources"
        );
        let at = crate::inbox::now()?;
        if let api::Disposition::Snoozed { until } = &request.disposition {
            if api::utc(until)? <= api::utc(&at)? {
                return Err(ExpiredSnooze {
                    path: detail.source.path,
                    at,
                }
                .into());
            }
        }
        self.proposal_store.as_mut().unwrap().stage_disposition(
            &self.state.brain_id,
            request.clone(),
            &at,
        )?;
        self.recover_draft_projections_for(Some(&request.proposal_id))?;
        let mut receipt = self
            .drafts()?
            .disposition_replay(&self.state.brain_id, &request)?
            .context("proposal receipt absent")?;
        receipt.replayed = false;
        Ok(receipt)
    }
    /// Fixture-only A3 activation. No public service/CLI/provider switch.
    #[cfg(test)]
    pub(super) fn enroll_proposal_drafts(&mut self) -> Result<()> {
        self.source.require_proposal_drafts()?;
        self.open_proposal_drafts()
    }
    #[cfg(test)]
    pub(super) fn fixture_start_proposal(
        &mut self,
        id: &str,
        goal: Option<&str>,
        captured: api::CapturedInput,
    ) -> Result<()> {
        let trigger = self
            .drafts()?
            .get(&self.state.brain_id, goal, id)?
            .trigger
            .clone();
        self.validate_proposal_input(&trigger, &captured)?;
        let input = crate::proposals::FrozenInput {
            generation: None,
            provider_identity: "fixture-provider".into(),
            model: "deterministic-v1".into(),
            input_sha256: format!("{:x}", Sha256::digest(serde_json::to_vec(&captured)?)),
        };
        let path = self.path("proposal", id);
        let at = crate::inbox::now()?;
        self.proposal_store.as_mut().unwrap().begin_draft(
            &self.state.brain_id,
            goal,
            id,
            crate::proposals::DraftStart {
                input,
                captured,
                actor: "fixture-provider".into(),
                at,
                path,
            },
        )?;
        self.recover_draft_projections()
    }
    #[cfg(test)]
    pub(super) fn fixture_finish_proposal(
        &mut self,
        id: &str,
        goal: Option<&str>,
        output: &[u8],
    ) -> Result<bool> {
        let draft = self.drafts()?.draft(&self.state.brain_id, goal, id)?;
        let parsed = if output.len() > 32 * 1024 {
            None
        } else {
            serde_json::from_slice::<api::Generated>(output).ok()
        };
        let result = if self
            .validate_proposal_input(&draft.record.trigger, &draft.record.captured)
            .is_err()
        {
            Err(api::Failure::SourceChanged)
        } else {
            parsed
                .filter(|g| g.validate(&draft.record.captured).is_ok())
                .ok_or(api::Failure::MalformedOutput)
        };
        let at = crate::inbox::now()?;
        let changed = self.proposal_store.as_mut().unwrap().finish_draft(
            &self.state.brain_id,
            goal,
            id,
            result,
            &at,
        )?;
        self.recover_draft_projections()?;
        Ok(changed)
    }
    #[cfg(test)]
    fn draft_fault(&mut self, f: Fault) -> Result<()> {
        if self.proposal_draft_fault == Some(f) {
            self.proposal_draft_fault = None;
            anyhow::bail!("injected proposal crash {f:?}");
        }
        Ok(())
    }
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    BeforeProjection,
    AfterProjection,
    AfterReceipt,
}
#[cfg(test)]
mod tests;

#[derive(Debug)]
struct ExpiredSnooze {
    path: String,
    at: String,
}
impl std::fmt::Display for ExpiredSnooze {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "snooze deadline must be in the future")
    }
}
impl std::error::Error for ExpiredSnooze {}
