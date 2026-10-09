//! Public inspected-form wrappers over the original context and Inbox coordinators.
use super::*;
use crate::{proposal as api, proposals::AttemptState};
pub(super) const NEW_ADOPTIONS_ENABLED: bool = true;
impl Runner {
    pub(crate) fn proposals_adopt_writable(&self) -> bool {
        NEW_ADOPTIONS_ENABLED && self.managed && self.proposals_readable()
    }
    pub fn proposal_adopt(
        &mut self,
        request: api::AdoptRequest,
        actor: &str,
    ) -> Result<api::AdoptReceipt> {
        request.validate(actor)?;
        let workspace = self.workspace_identity();
        if let Some(receipt) = self
            .drafts()?
            .terminal_adoption_replay(&workspace, &request)?
        {
            return Ok(receipt);
        }
        // Original accepted operations reconcile before any current-source checks.
        let accepted = match &request {
            api::AdoptRequest::Context(r) => self.drafts()?.adoption_replay(r)?.is_some(),
            api::AdoptRequest::Inbox(r) => self.drafts()?.inbox_adoption_replay(r)?.is_some(),
        };
        if !accepted {
            ensure!(
                self.proposals_adopt_writable(),
                "new proposal adoption unavailable"
            );
            let external = request.source().external_key()?;
            ensure!(
                !self.inbox_identity_reserved(request.operation_id(), &external)
                    && !self.attention_identity_reserved(request.operation_id(), &external)
                    && !self.plan_identity_reserved(request.operation_id(), &external)
                    && !self.proposal_identity_reserved(request.operation_id(), &external)?,
                "adoption identity belongs to another operation"
            );
            let reason = match self.proposal_adopt_refusal(&request)? {
                Some(reason) => Some(reason),
                None => match self.validate_proposal_adopt_capacity(&request) {
                    Ok(()) => None,
                    Err(error) if error.downcast_ref::<api::PublicationCapacity>().is_some() => {
                        Some(api::AdoptRefusal::CapacityExceeded)
                    }
                    Err(error) => return Err(error),
                },
            };
            if let Some(reason) = reason {
                self.source.require_public_proposal_adoption()?;
                return self
                    .proposal_store
                    .as_mut()
                    .unwrap()
                    .reserve_terminal_adoption(workspace, request, reason, crate::inbox::now()?);
            }
            // Only an explicit validated form enables its existing coordinator.
            self.source.require_proposal_adoption()?;
            self.proposal_store.as_mut().unwrap().enable_adoption()?;
            if matches!(request, api::AdoptRequest::Inbox(_)) {
                self.source.require_proposal_inbox_adoption()?;
                self.proposal_store
                    .as_mut()
                    .unwrap()
                    .enable_inbox_adoption()?;
            }
        }
        let result = (|| -> Result<_> {
            Ok(match &request {
                api::AdoptRequest::Context(r) => {
                    let receipt = self.adopt_proposal_context(r.as_ref().clone(), actor)?;
                    (
                        receipt.at.clone(),
                        receipt.replayed,
                        api::AdoptOutcome::CommittedContext {
                            receipt: Box::new(receipt),
                        },
                    )
                }
                api::AdoptRequest::Inbox(r) => {
                    let receipt = self.adopt_proposal_inbox(r.as_ref().clone(), actor)?;
                    (
                        receipt.at.clone(),
                        receipt.replayed,
                        api::AdoptOutcome::CommittedInbox {
                            receipt: Box::new(receipt),
                        },
                    )
                }
            })
        })();
        let (at, replayed, result) = match result {
            Ok(result) => result,
            Err(error)
                if error.downcast_ref::<api::PublicationCapacity>().is_some() && !accepted =>
            {
                let retained = match &request {
                    api::AdoptRequest::Context(r) => self.drafts()?.adoption_replay(r)?.is_some(),
                    api::AdoptRequest::Inbox(r) => {
                        self.drafts()?.inbox_adoption_replay(r)?.is_some()
                    }
                };
                ensure!(
                    !retained,
                    "accepted adoption capacity remains pending: {error}"
                );
                self.source.require_public_proposal_adoption()?;
                return self
                    .proposal_store
                    .as_mut()
                    .unwrap()
                    .reserve_terminal_adoption(
                        workspace,
                        request,
                        api::AdoptRefusal::CapacityExceeded,
                        crate::inbox::now()?,
                    );
            }
            Err(error) => return Err(error),
        };
        Ok(api::AdoptReceipt {
            schema: "okilum-proposal-adopt/v1".into(),
            workspace,
            request,
            at,
            replayed,
            result,
        })
    }
    // Prepare only in memory. Validate the full retained candidate and completion
    // reserve before enrollment; no operation, target or source write is accepted.
    fn validate_proposal_adopt_capacity(&mut self, request: &api::AdoptRequest) -> Result<()> {
        let at = crate::inbox::now()?;
        match request {
            api::AdoptRequest::Inbox(r) => {
                let target = crate::inbox_plan::frozen_goal::prepare(
                    self,
                    r.child(&Uuid::new_v4().to_string()),
                )?;
                self.drafts()?.validate_inbox_adoption_capacity(
                    r.as_ref().clone(),
                    target,
                    self.state.records_dir.clone(),
                    at,
                )
            }
            api::AdoptRequest::Context(r) => {
                let target = self.with_goal(&r.goal_id, |runner| {
                    crate::context::frozen_target::prepare(
                        runner,
                        crate::context::frozen_target::Form {
                            goal_id: r.goal_id.clone(),
                            expected_goal_revision: r.expected_goal_revision.clone(),
                            query: r.query.clone(),
                            scope: r.scope.clone(),
                            citations: r.citations.clone(),
                            pinned_citation_ids: r.pinned_citation_ids.clone(),
                            guidance: r.guidance.clone(),
                        },
                    )
                })?;
                self.drafts()?.validate_adoption_capacity(
                    r.as_ref().clone(),
                    target,
                    self.state.records_dir.clone(),
                    at,
                )
            }
        }
    }
    fn proposal_adopt_refusal(
        &mut self,
        request: &api::AdoptRequest,
    ) -> Result<Option<api::AdoptRefusal>> {
        let draft = self.drafts()?.draft(
            &self.state.brain_id,
            request.goal_id(),
            request.proposal_id(),
        )?;
        ensure!(
            !draft.pending()
                && draft.adoption.as_ref().is_none_or(|a| a.pointer.is_some())
                && draft.inbox_adoption.as_ref().is_none_or(|a| a.projected),
            "proposal has uncertain pending adoption or projection"
        );
        let current = self
            .source
            .read_bounded(&draft.projections[0].write.path, 1024 * 1024)?;
        if current.revision != draft.latest_revision()?
            || request.expected_revision() != draft.latest_revision()?
        {
            return Ok(Some(api::AdoptRefusal::RevisionChanged));
        }
        let record = draft.record.clone();
        if !matches!(
            record.disposition,
            api::Disposition::Unreviewed | api::Disposition::Snoozed { .. }
        ) {
            return Ok(Some(api::AdoptRefusal::DispositionChanged));
        }
        if record.attempt.state != AttemptState::Draft || record.generated.is_none() {
            return Ok(Some(api::AdoptRefusal::AttemptIneligible));
        }
        // A read failure is not evidence of non-application. Only positively
        // observed revision/identity mismatches retire an exact retained request.
        let trigger = self
            .source
            .read_bounded(&record.trigger.source_path, 1024 * 1024)?;
        if trigger != record.captured.trigger_source {
            return Ok(Some(api::AdoptRefusal::InputChanged));
        }
        for citation in &record.captured.citations {
            if self
                .source
                .read_bounded(&citation.path, 1024 * 1024)?
                .revision
                != citation.revision
            {
                return Ok(Some(api::AdoptRefusal::InputChanged));
            }
        }
        if let Some(goal) = request.goal_id() {
            let current = self
                .source
                .read_bounded(&self.path("goal", goal), 1024 * 1024)?;
            if Some(&current.revision) != record.captured.goal_revision.as_ref() {
                return Ok(Some(api::AdoptRefusal::InputChanged));
            }
        }
        self.validate_proposal_input(&record.trigger, &record.captured)?;
        match request {
            api::AdoptRequest::Inbox(r) => {
                ensure!(
                    record.trigger.identity.record_id == r.capture_id
                        && record.trigger.identity.kind == crate::proposals::TriggerKind::Inbox,
                    "adoption form capture does not own proposal"
                );
                let capture = self.inbox_get(&r.capture_id)?;
                if capture.item.revision != r.expected_capture_revision {
                    return Ok(Some(api::AdoptRefusal::DestinationChanged));
                }
                ensure!(
                    self.inbox_plan_writable(),
                    "Inbox adoption planning unavailable"
                );
            }
            api::AdoptRequest::Context(r) => {
                if crate::context::frozen_target::validate_form_fields(
                    &r.query,
                    &r.scope,
                    &r.citations,
                    &r.pinned_citation_ids,
                    &r.guidance,
                )
                .is_err()
                {
                    return Ok(Some(api::AdoptRefusal::InvalidForm));
                }
                let goal = self
                    .source
                    .read_bounded(&self.path("goal", &r.goal_id), 1024 * 1024)?;
                if goal.revision != r.expected_goal_revision {
                    return Ok(Some(api::AdoptRefusal::DestinationChanged));
                }
                let selected = self.with_goal(&r.goal_id, |runner| {
                    Ok(runner
                        .application_state()
                        .get("reviewed_packet_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned))
                })?;
                if selected != r.expected_base_packet {
                    return Ok(Some(api::AdoptRefusal::DestinationChanged));
                }
                for citation in &r.citations {
                    let source = self.source.read_bounded(&citation.path, 1024 * 1024)?;
                    if source.revision != citation.revision {
                        return Ok(Some(api::AdoptRefusal::DestinationChanged));
                    }
                    if crate::retrieval::validate_citation(
                        &source,
                        citation,
                        &r.goal_id,
                        &r.scope.mode,
                        &self.state.records_dir,
                    )
                    .is_err()
                    {
                        return Ok(Some(api::AdoptRefusal::InvalidForm));
                    }
                }
                if let Some(base) = &r.expected_base_packet {
                    let packet = self.with_goal(&r.goal_id, |runner| {
                        crate::context::read(runner, &r.goal_id, base)
                    })?;
                    for citation in &packet.citations {
                        self.source.read_bounded(&citation.path, 1024 * 1024)?;
                    }
                    if !r.guidance.contains(&packet.text)
                        || packet.pinned_citation_ids.iter().any(|id| {
                            !r.pinned_citation_ids.contains(id)
                                || !packet
                                    .citations
                                    .iter()
                                    .find(|c| &c.citation_id == id)
                                    .is_some_and(|old| r.citations.contains(old))
                        })
                    {
                        return Ok(Some(api::AdoptRefusal::InvalidForm));
                    }
                    if Some(&packet.revision) != r.expected_base_revision.as_ref() || packet.stale {
                        return Ok(Some(api::AdoptRefusal::DestinationChanged));
                    }
                }
            }
        }
        Ok(None)
    }
}
#[cfg(test)]
mod tests;
