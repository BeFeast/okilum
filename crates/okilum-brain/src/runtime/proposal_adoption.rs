//! Original context adoption coordinator, used by the inspected public form.
use super::*;
use crate::proposals::{AdoptionReceipt, AdoptionRequest};

impl Runner {
    pub(super) fn open_proposal_adoption(&mut self) -> Result<()> {
        if !self.source.required_proposal_adoption() {
            return Ok(());
        }
        self.proposal_store
            .as_mut()
            .context("adoption drafts absent")?
            .enable_adoption()?;
        for id in self.drafts()?.adoption_operations()? {
            if self.recover_context_adoption(&id).is_err() {
                self.proposal_draft_issue = Some("proposal adoption requires recovery".into());
            }
        }
        Ok(())
    }
    fn recover_context_adoption(&mut self, id: &str) -> Result<()> {
        let operation = self.drafts()?.adoption(id)?.clone();
        ensure!(
            operation.records_dir == self.state.records_dir,
            "adoption records directory differs from workspace"
        );
        let target = operation.target()?;
        if let Some(receipt) = operation.target_receipt {
            let original = self
                .source
                .recovery_record(&target.source_write().operation_id)?;
            ensure!(
                original.request == *target.source_write()
                    && original.receipt.as_ref() == Some(&receipt),
                "adoption original target receipt mismatch"
            );
        } else {
            #[cfg(test)]
            self.adoption_fault(Fault::BeforeTarget)?;
            // SourceStore replays the original operation before mutable source
            // checks. Never infer success from current packet bytes or allocate.
            let receipt = self.source.write(target.source_write().clone())?;
            #[cfg(test)]
            self.adoption_fault(Fault::AfterTarget)?;
            self.proposal_store
                .as_mut()
                .unwrap()
                .acknowledge_adoption_target(id, receipt)?;
            #[cfg(test)]
            self.adoption_fault(Fault::AfterTargetReceipt)?;
        }
        self.proposal_store.as_mut().unwrap().project_adoption(id)?;
        self.recover_draft_projections_for(Some(id))?;
        if self.drafts()?.adoption(id)?.pointer.is_none() {
            let expected = operation.request.expected_base_packet.clone();
            let pointer = self.with_goal(target.goal_id(), |runner| {
                crate::application::Application::adopt_context_pointer(
                    runner,
                    expected.as_deref(),
                    target.packet_id(),
                )
            })?;
            #[cfg(test)]
            self.adoption_fault(Fault::AfterPointer)?;
            self.proposal_store
                .as_mut()
                .unwrap()
                .acknowledge_adoption_pointer(id, pointer)?;
        }
        Ok(())
    }
    /// Explicit fixture enrollment; no ambient feature activation.
    #[cfg(test)]
    fn enroll_proposal_adoption(&mut self) -> Result<()> {
        self.source.require_proposal_adoption()?;
        self.proposal_store
            .as_mut()
            .context("drafts absent")?
            .enable_adoption()?;
        self.open_proposal_adoption()
    }
    pub(super) fn adopt_proposal_context(
        &mut self,
        request: AdoptionRequest,
        actor: &str,
    ) -> Result<AdoptionReceipt> {
        request.validate(actor)?;
        let replay = self.drafts()?.adoption_replay(&request)?;
        if let Some(Some(receipt)) = replay {
            return Ok(receipt);
        }
        if replay.is_some() {
            self.recover_context_adoption(&request.proposal_id)?;
            return self
                .drafts()?
                .adoption_replay(&request)?
                .flatten()
                .context("adoption receipt absent");
        }
        ensure!(
            self.managed
                && self.source.required_proposal_adoption()
                && self.drafts()?.adoption_enabled(),
            "adoption unavailable"
        );
        let external = request.source.external_key()?;
        ensure!(
            !self.inbox_identity_reserved(&request.operation_id, &external)
                && !self.attention_identity_reserved(&request.operation_id, &external)
                && !self.plan_identity_reserved(&request.operation_id, &external)
                && !self.proposal_identity_reserved(&request.operation_id, &external)?,
            "adoption identity belongs to another operation"
        );
        let detail = self.proposal_get(crate::proposal::Lookup {
            proposal_id: request.proposal_id.clone(),
            goal_id: Some(request.goal_id.clone()),
        })?;
        ensure!(
            detail.source.revision == request.expected_revision
                && detail.stale_reasons.is_empty()
                && !detail.projection_pending,
            "proposal or captured input changed"
        );
        let target = self.with_goal(&request.goal_id, |runner| {
            let current = runner.application_state();
            ensure!(
                current.get("reviewed_packet_id").and_then(Value::as_str)
                    == request.expected_base_packet.as_deref(),
                "base context selection changed"
            );
            if let Some(base) = &request.expected_base_packet {
                let base = crate::context::read(runner, &request.goal_id, base)?;
                ensure!(
                    Some(&base.revision) == request.expected_base_revision.as_ref() && !base.stale,
                    "base context revision changed"
                );
                // The complete inspected form may add guidance and citations, but
                // cannot silently discard the existing manual content or pins.
                ensure!(
                    request.guidance.contains(&base.text),
                    "existing manual guidance must be preserved"
                );
                for id in &base.pinned_citation_ids {
                    ensure!(
                        request.pinned_citation_ids.contains(id)
                            && base
                                .citations
                                .iter()
                                .find(|c| &c.citation_id == id)
                                .is_some_and(|old| request.citations.contains(old)),
                        "existing pin identity must be preserved"
                    );
                }
            }
            crate::context::frozen_target::prepare(
                runner,
                crate::context::frozen_target::Form {
                    goal_id: request.goal_id.clone(),
                    expected_goal_revision: request.expected_goal_revision.clone(),
                    query: request.query.clone(),
                    scope: request.scope.clone(),
                    citations: request.citations.clone(),
                    pinned_citation_ids: request.pinned_citation_ids.clone(),
                    guidance: request.guidance.clone(),
                },
            )
        })?;
        self.proposal_store.as_mut().unwrap().stage_adoption(
            request.clone(),
            target,
            self.state.records_dir.clone(),
            crate::inbox::now()?,
        )?;
        #[cfg(test)]
        self.adoption_fault(Fault::AfterIntent)?;
        self.recover_context_adoption(&request.proposal_id)?;
        let mut receipt = self
            .drafts()?
            .adoption_replay(&request)?
            .flatten()
            .context("adoption receipt absent")?;
        receipt.replayed = false;
        Ok(receipt)
    }
    #[cfg(test)]
    fn adoption_fault(&mut self, fault: Fault) -> Result<()> {
        if self.proposal_adoption_fault == Some(fault) {
            self.proposal_adoption_fault = None;
            anyhow::bail!("injected adoption crash {fault:?}");
        }
        Ok(())
    }
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    AfterIntent,
    BeforeTarget,
    AfterTarget,
    AfterTargetReceipt,
    AfterPointer,
}
#[cfg(test)]
pub(super) mod tests;
