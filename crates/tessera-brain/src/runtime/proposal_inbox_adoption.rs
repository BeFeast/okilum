//! Original Inbox adoption coordinator, used by the inspected public form.
use super::*;
use crate::proposals::{InboxAdoptionReceipt, InboxAdoptionRequest};
impl Runner {
    pub(super) fn validate_inbox_adoption_inventory(&self) -> Result<()> {
        if let Some(store) = &self.proposal_store {
            ensure!(
                !store.inbox_adoption_enabled() || self.source.required_proposal_inbox_adoption(),
                "Inbox adoption journal lacks source fence"
            );
            for id in store.inbox_adoption_ids()? {
                let op = store.inbox_adoption(&id)?;
                ensure!(
                    op.records_dir == self.state.records_dir,
                    "Inbox adoption records directory mismatch"
                );
                let witness = store.delegation(&id)?;
                let child = self.delegated_child_receipt(&witness)?;
                if let Some(receipt) = &op.child_receipt {
                    ensure!(
                        child.as_ref() == Some(receipt),
                        "parent acknowledged child receipt missing or changed"
                    );
                }
            }
        }
        self.validate_delegated_inventory()
    }
    pub(super) fn open_proposal_inbox_adoption(&mut self) -> Result<()> {
        if !self.source.required_proposal_inbox_adoption() {
            return Ok(());
        }
        self.proposal_store
            .as_mut()
            .context("Inbox adoption store absent")?
            .enable_inbox_adoption()?;
        for id in self.drafts()?.inbox_adoption_ids()? {
            if self.recover_inbox_adoption(&id).is_err() {
                self.proposal_draft_issue = Some("Inbox adoption requires recovery".into());
            }
        }
        Ok(())
    }
    fn recover_inbox_adoption(&mut self, id: &str) -> Result<()> {
        let witness = self.drafts()?.delegation(id)?;
        let receipt = self.recover_delegated_plan(&witness)?;
        self.proposal_store
            .as_mut()
            .unwrap()
            .acknowledge_inbox_child(id, receipt)?;
        #[cfg(test)]
        self.inbox_adoption_fault(Fault::ParentReceipt)?;
        self.proposal_store
            .as_mut()
            .unwrap()
            .project_inbox_adoption(id)?;
        self.recover_draft_projections_for(Some(id))
    }
    #[cfg(test)]
    fn enroll_proposal_inbox_adoption(&mut self) -> Result<()> {
        self.source.require_proposal_inbox_adoption()?;
        self.open_proposal_inbox_adoption()
    }
    pub(super) fn adopt_proposal_inbox(
        &mut self,
        request: InboxAdoptionRequest,
        actor: &str,
    ) -> Result<InboxAdoptionReceipt> {
        request.validate(actor)?;
        let replay = self.drafts()?.inbox_adoption_replay(&request)?;
        if let Some(Some(receipt)) = replay {
            return Ok(receipt);
        }
        if replay.is_some() {
            self.recover_inbox_adoption(&request.proposal_id)?;
            return self
                .drafts()?
                .inbox_adoption_replay(&request)?
                .flatten()
                .context("Inbox adoption receipt absent");
        }
        ensure!(
            self.source.required_proposal_inbox_adoption()
                && self.drafts()?.inbox_adoption_enabled()
                && self.inbox_plan_writable(),
            "new Inbox adoption unavailable"
        );
        let external = request.source.external_key()?;
        ensure!(
            !self.inbox_identity_reserved(&request.operation_id, &external)
                && !self.attention_identity_reserved(&request.operation_id, &external)
                && !self.plan_identity_reserved(&request.operation_id, &external)
                && !self.proposal_identity_reserved(&request.operation_id, &external)?,
            "Inbox parent identity belongs to another operation"
        );
        let detail = self.proposal_get(crate::proposal::Lookup {
            proposal_id: request.proposal_id.clone(),
            goal_id: None,
        })?;
        ensure!(
            detail.source.revision == request.expected_revision
                && detail.stale_reasons.is_empty()
                && !detail.projection_pending,
            "Inbox proposal or input changed"
        );
        let child = request.child(&Uuid::new_v4().to_string());
        ensure!(
            !self.inbox_identity_reserved(&child.operation_id, &external)
                && !self.attention_identity_reserved(&child.operation_id, &external)
                && !self.plan_identity_reserved(&child.operation_id, &external)
                && !self.proposal_identity_reserved(&child.operation_id, &external)?,
            "child identity already belongs to another operation"
        );
        let frozen = crate::inbox_plan::frozen_goal::prepare(self, child)?;
        self.proposal_store.as_mut().unwrap().stage_inbox_adoption(
            request.clone(),
            frozen,
            self.state.records_dir.clone(),
            crate::inbox::now()?,
        )?;
        #[cfg(test)]
        self.inbox_adoption_fault(Fault::ParentIntent)?;
        self.recover_inbox_adoption(&request.proposal_id)?;
        let mut receipt = self
            .drafts()?
            .inbox_adoption_replay(&request)?
            .flatten()
            .context("Inbox adoption receipt absent")?;
        receipt.replayed = false;
        Ok(receipt)
    }
    #[cfg(test)]
    pub(super) fn inbox_adoption_fault(&mut self, fault: Fault) -> Result<()> {
        if self.proposal_inbox_adoption_fault == Some(fault) {
            self.proposal_inbox_adoption_fault = None;
            anyhow::bail!("injected Inbox adoption crash {fault:?}");
        }
        Ok(())
    }
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    ParentIntent,
    ChildIntent,
    ChildSource,
    ChildReceipt,
    ParentReceipt,
}
#[cfg(test)]
pub(super) mod tests;
