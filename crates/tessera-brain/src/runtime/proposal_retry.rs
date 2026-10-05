use super::*;
use crate::{application::ChatSettings, proposal as api, proposals::AttemptState};

// Preserving maintenance disables only new acceptance, never receipt recovery.
pub(super) const NEW_RETRIES_ENABLED: bool = true;
impl Runner {
    pub(crate) fn proposals_retry_writable(&self) -> bool {
        NEW_RETRIES_ENABLED
            && self.managed
            && self.source.required_proposal_generation()
            && self.proposals_readable()
    }
    pub fn proposal_retry(
        &mut self,
        request: api::RetryRequest,
        actor: &str,
        settings: Option<&ChatSettings>,
    ) -> Result<api::RetryReceipt> {
        request.validate(actor)?;
        if self.drafts()?.retry_pending(&request)? {
            self.recover_draft_projections_for(Some(&request.proposal_id))?;
        }
        let workspace = self.workspace_identity();
        if let Some(receipt) = self.drafts()?.retry_replay(&workspace, &request)? {
            return Ok(receipt);
        }
        let external = request.source.external_key()?;
        ensure!(
            !self.inbox_identity_reserved(&request.operation_id, &external)
                && !self.attention_identity_reserved(&request.operation_id, &external)
                && !self.plan_identity_reserved(&request.operation_id, &external)
                && !self.proposal_identity_reserved(&request.operation_id, &external)?,
            "retry identity belongs to another operation"
        );
        ensure!(
            self.proposals_retry_writable(),
            "new proposal Retry unavailable"
        );
        let draft = self.drafts()?.draft(
            &self.state.brain_id,
            request.goal_id.as_deref(),
            &request.proposal_id,
        )?;
        ensure!(
            !draft.pending()
                && draft.adoption.as_ref().is_none_or(|a| a.pointer.is_some())
                && draft.inbox_adoption.as_ref().is_none_or(|a| a.projected),
            "proposal has uncertain pending operation"
        );
        let current = self
            .source
            .read_bounded(&draft.projections[0].write.path, 1024 * 1024)?;
        let at = crate::inbox::now()?;
        let mut refusal = if current.revision != draft.latest_revision()?
            || request.expected_revision != draft.latest_revision()?
        {
            Some(api::RetryRefusal::RevisionChanged)
        } else if !matches!(
            draft.record.disposition,
            api::Disposition::Unreviewed | api::Disposition::Snoozed { .. }
        ) {
            Some(api::RetryRefusal::DispositionChanged)
        } else if !matches!(
            draft.record.attempt.state,
            AttemptState::Failed | AttemptState::Interrupted
        ) || draft.record.attempt_history.len() >= 32
            || draft
                .record
                .attempt
                .input
                .as_ref()
                .and_then(|i| i.generation.as_ref())
                .is_none_or(|g| g.settings.is_none())
        {
            Some(api::RetryRefusal::AttemptIneligible)
        } else {
            let generation = draft
                .record
                .attempt
                .input
                .as_ref()
                .unwrap()
                .generation
                .as_ref()
                .unwrap()
                .clone();
            let trigger = draft.record.trigger.clone();
            let captured = draft.record.captured.clone();
            // Read errors retain uncertainty; positive revision mismatches are terminal.
            let trigger_now = self
                .source
                .read_bounded(&trigger.source_path, 1024 * 1024)?;
            let mut changed =
                trigger_now != captured.trigger_source || generation.settings.as_ref() != settings;
            for citation in &captured.citations {
                changed |= self
                    .source
                    .read_bounded(&citation.path, 1024 * 1024)?
                    .revision
                    != citation.revision;
            }
            if let Some(goal) = &request.goal_id {
                let original = generation
                    .goal_source
                    .as_ref()
                    .context("frozen goal source absent")?;
                changed |= self
                    .source
                    .read_bounded(&self.path("goal", goal), 1024 * 1024)?
                    != *original;
                let brief = self.with_goal(goal, |r| r.goal_context_brief(goal))?;
                changed |= Some(brief) != generation.goal_brief;
            }
            if changed {
                Some(api::RetryRefusal::InputChanged)
            } else {
                self.validate_proposal_input(&trigger, &captured)?;
                None
            }
        };
        if refusal.is_none() && !self.drafts()?.retry_canonical_fits(&request, &at)? {
            refusal = Some(api::RetryRefusal::CapacityExceeded);
        }
        self.source.require_proposal_retry()?;
        self.proposal_store.as_mut().unwrap().reserve_retry(
            workspace.clone(),
            request.clone(),
            refusal,
            at,
        )?;
        self.recover_draft_projections_for(Some(&request.proposal_id))?;
        let mut receipt = self
            .drafts()?
            .retry_replay(&workspace, &request)?
            .context("retry receipt absent after publication")?;
        receipt.replayed = false;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests;
