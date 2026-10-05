//! Only an exact A1-owned parent may delegate the retained child request.
use super::*;
use crate::proposals::{ChildReceipt, Delegation};
impl Runner {
    pub(in crate::runtime) fn validate_delegated_inventory(&self) -> Result<()> {
        for (id, intent) in &self.state.inbox_plan_journal.operations {
            ensure!(
                id == &intent.request.operation_id,
                "planning journal key differs from request"
            );
            match &intent.delegation {
                None => ensure!(
                    intent.delegated_source_receipt.is_none(),
                    "ordinary plan has delegated receipt"
                ),
                Some(binding) => {
                    ensure!(
                        self.source.required_proposal_inbox_adoption(),
                        "delegated plan lacks required capability"
                    );
                    let witness = self.drafts()?.delegation(&binding.proposal_id)?;
                    ensure!(
                        id == &binding.child_operation_id && binding == witness.binding(),
                        "delegated row key/binding differs from retained parent"
                    );
                    self.validate_delegated_intent(&witness)?;
                }
            }
        }
        Ok(())
    }
    fn validate_delegated_intent(&self, witness: &Delegation) -> Result<Option<ChildReceipt>> {
        self.drafts()?.validate_delegation(witness)?;
        let t = witness.target();
        let request = t.request();
        let original_source = match self.source.recovery_record(&t.write().operation_id) {
            Ok(original) => {
                ensure!(
                    original.request == *t.write(),
                    "pending delegated source request differs from parent"
                );
                Some(original)
            }
            Err(e) if e.code == tessera_core::source::ErrorCode::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let Some(intent) = self
            .state
            .inbox_plan_journal
            .operations
            .get(&request.operation_id)
        else {
            ensure!(
                original_source.is_none(),
                "delegated source has no original child journal"
            );
            return Ok(None);
        };
        let mut expected = t.outcome().clone();
        if intent.delegated_source_receipt.is_some() {
            expected.receipt.status = "committed".into();
        }
        ensure!(
            intent.delegation.as_ref() == Some(witness.binding())
                && intent.request == *request
                && intent.external_key == request.source.external_key()?
                && intent.source_operation_id == t.write().operation_id
                && intent.goal_revision == t.revision()
                && intent.outcome == expected,
            "delegated child differs from original parent target"
        );
        ensure!(
            !self
                .state
                .inbox_plan_journal
                .aliases
                .contains_key(&request.operation_id)
                && !self
                    .state
                    .inbox_plan_journal
                    .aliases
                    .values()
                    .any(|id| id == &request.operation_id),
            "delegated child cannot gain aliases"
        );
        if let Some(receipt) = &intent.delegated_source_receipt {
            ensure!(
                self.goal_ids().contains(&t.outcome().goal_id),
                "committed delegated goal slot is absent"
            );
            let source = self.source.recovery_record(&t.write().operation_id)?;
            ensure!(
                source.request == *t.write() && source.receipt.as_ref() == Some(receipt),
                "delegated original source receipt differs"
            );
            Ok(Some(ChildReceipt {
                outcome: intent.outcome.clone(),
                source: receipt.clone(),
            }))
        } else {
            Ok(None)
        }
    }
    pub(in crate::runtime) fn delegated_child_receipt(
        &self,
        witness: &Delegation,
    ) -> Result<Option<ChildReceipt>> {
        self.validate_delegated_intent(witness)
    }
    pub(in crate::runtime) fn recover_delegated_plan(
        &mut self,
        witness: &Delegation,
    ) -> Result<ChildReceipt> {
        self.drafts()?.validate_delegation(witness)?;
        ensure!(
            self.managed && self.source.required_proposal_inbox_adoption(),
            "delegated recovery requires managed Inbox adoption"
        );
        if let Some(receipt) = self.validate_delegated_intent(witness)? {
            return Ok(receipt);
        }
        let t = witness.target();
        let req = t.request();
        let external = req.source.external_key()?;
        ensure!(
            !self.inbox_identity_reserved(&req.operation_id, &external)
                && !self.attention_identity_reserved(&req.operation_id, &external),
            "delegated child identity belongs to another operation"
        );
        if !self
            .state
            .inbox_plan_journal
            .operations
            .contains_key(&req.operation_id)
        {
            ensure!(
                !self.state.inbox_plan_journal.recovery_required
                    && !self.plan_identity_reserved(&req.operation_id, &external),
                "delegated planning history unavailable or conflicting"
            );
            match self.source.recovery_record(&t.write().operation_id) {
                Err(e) if e.code == tessera_core::source::ErrorCode::NotFound => (),
                _ => anyhow::bail!("original child journal missing after source attempt"),
            }
            self.ensure_plan_enrollment()?;
            self.mutation(|r| {
                r.state.inbox_plan_journal.operations.insert(
                    req.operation_id.clone(),
                    Intent {
                        request: req.clone(),
                        external_key: external,
                        source_operation_id: t.write().operation_id.clone(),
                        goal_revision: t.revision().into(),
                        outcome: t.outcome().clone(),
                        delegation: Some(witness.binding().clone()),
                        delegated_source_receipt: None,
                    },
                );
                r.persist()
            })?;
            #[cfg(test)]
            self.inbox_adoption_fault(super::super::proposal_inbox_adoption::Fault::ChildIntent)?;
        }
        // This source operation stays owned by the delegated child. It must not
        // enter shared pending_writes, where a manual conflict would block capture.
        let receipt = self.source.write(t.write().clone())?;
        #[cfg(test)]
        self.inbox_adoption_fault(super::super::proposal_inbox_adoption::Fault::ChildSource)?;
        self.mutation(|r| {
            let intent = r
                .state
                .inbox_plan_journal
                .operations
                .get_mut(&req.operation_id)
                .context("child intent absent")?;
            intent.delegated_source_receipt = Some(receipt.clone());
            intent.outcome.receipt.status = "committed".into();
            let goal = t.outcome().goal_id.clone();
            if !r.goal_ids().contains(&goal) {
                if r.state.goal_id.is_none() {
                    r.state.goal_id = Some(goal.clone());
                    r.state.primary_goal_id = Some(goal);
                } else {
                    r.state.other_goals.insert(
                        goal.clone(),
                        GoalState {
                            goal_id: Some(goal),
                            ..GoalState::default()
                        },
                    );
                }
            }
            r.persist()
        })?;
        #[cfg(test)]
        self.inbox_adoption_fault(super::super::proposal_inbox_adoption::Fault::ChildReceipt)?;
        self.validate_delegated_intent(witness)?
            .context("child receipt absent")
    }
}
