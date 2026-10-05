//! Dispatch control is separate from generation receipts and source feed identity.
use super::*;
use crate::{proposals::AttemptState, suggestions as api};

// Preserving maintenance changes only this and existing NEW_* constants.
pub(super) const NEW_SETTINGS_ENABLED: bool = true;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Control {
    schema: String,
    revision: u64,
    enabled: bool,
    initial_enabled: bool,
    refusals: BTreeMap<String, Refused>,
    pending: Option<api::Receipt>,
    receipts: BTreeMap<String, api::Receipt>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Refused {
    workspace: Value,
    request: api::SetRequest,
    actor: String,
    reason: api::Refusal,
}
#[cfg(test)]
impl Control {
    pub(super) fn has_refusals(&self) -> bool {
        !self.refusals.is_empty()
    }
    pub(super) fn has_final_receipt(&self) -> bool {
        self.pending.is_none() && !self.receipts.is_empty()
    }
}
impl Default for Control {
    fn default() -> Self {
        Self {
            schema: "tessera-suggestions-control/v1".into(),
            revision: 0,
            enabled: false,
            initial_enabled: false,
            refusals: BTreeMap::new(),
            pending: None,
            receipts: BTreeMap::new(),
        }
    }
}
impl Runner {
    pub(super) fn suggestions_cut(&mut self, point: u8) -> Result<()> {
        #[cfg(test)]
        if self.suggestions_fault == Some(point) {
            self.suggestions_fault = None;
            anyhow::bail!("injected suggestions cut{point}");
        }
        let _ = point;
        Ok(())
    }
    pub(crate) fn suggestions_controlled(&self) -> bool {
        self.source.required_suggestions_control()
    }
    pub(super) fn suggestions_dispatch_allowed(&self) -> bool {
        if !self.source.required_suggestions_control() {
            return true;
        }
        self.state
            .suggestions
            .as_ref()
            .is_some_and(|s| s.pending.is_none() && s.enabled)
    }
    pub(super) fn suggestions_identity_reserved(&self, operation: &str) -> bool {
        self.state.suggestions.as_ref().is_some_and(|s| {
            s.receipts.contains_key(operation)
                || s.refusals.contains_key(operation)
                || s.pending
                    .as_ref()
                    .is_some_and(|p| p.request.operation_id == operation)
        })
    }
    pub(crate) fn suggestions_status(&self, provider: api::Provider) -> Result<api::Status> {
        let enrolled = self.source.required_proposal_generation();
        let control = self.state.suggestions.as_ref();
        let mut queued = 0;
        let mut running = 0;
        if let Some(store) = &self.proposal_store {
            for intent in store.intents()?.values() {
                queued += usize::from(intent.attempt.state == AttemptState::Queued);
                running += usize::from(intent.attempt.state == AttemptState::Running);
            }
        }
        Ok(api::Status {
            schema: "tessera-suggestions/v1".into(),
            mode: if !enrolled {
                "disabled"
            } else if self.proposal_generation_enabled() {
                "enabled"
            } else {
                "paused"
            }
            .into(),
            revision: control.map_or(0, |s| s.revision),
            enrolled,
            queued,
            running,
            backlog: self
                .proposal_backlog()
                .or_else(|| self.proposal_draft_issue.clone()),
            provider,
            can_change: self.managed
                && NEW_SETTINGS_ENABLED
                && control.is_none_or(|s| s.pending.is_none()),
        })
    }
    pub(crate) fn suggestions_set(
        &mut self,
        request: api::SetRequest,
        actor: &str,
        provider_available: bool,
    ) -> Result<api::Receipt> {
        self.mutation(|r| r.suggestions_set_inner(request, actor, provider_available))
    }
    fn suggestions_set_inner(
        &mut self,
        request: api::SetRequest,
        actor: &str,
        provider_available: bool,
    ) -> Result<api::Receipt> {
        request.validate(actor)?;
        let workspace = self.workspace_identity();
        if let Some(control) = &self.state.suggestions {
            if let Some(previous) = control.refusals.get(&request.operation_id) {
                if previous.request != request || previous.workspace != workspace {
                    return Err(api::Refusal::IdentityConflict.into());
                }
                return Err(previous.reason.into());
            }
            let previous = control.receipts.get(&request.operation_id).or_else(|| {
                control
                    .pending
                    .as_ref()
                    .filter(|p| p.request.operation_id == request.operation_id)
            });
            if let Some(previous) = previous {
                if previous.request != request || previous.workspace != workspace {
                    return Err(api::Refusal::IdentityConflict.into());
                }
                self.recover_suggestions_control()?;
                self.open_suggestions_store_if_needed()?;
                let mut receipt = self.state.suggestions.as_ref().unwrap().receipts
                    [&request.operation_id]
                    .clone();
                receipt.replayed = true;
                return Ok(receipt);
            }
        }
        ensure!(
            self.managed && NEW_SETTINGS_ENABLED,
            "suggestions settings unavailable; original request remains recoverable"
        );
        // Settings have no upstream alias, but share operation IDs with Inbox,
        // Attention, planning and every proposal mutation in both directions.
        let external = "";
        if self.inbox_identity_reserved(&request.operation_id, external)
            || self.attention_identity_reserved(&request.operation_id, external)
            || self.plan_identity_reserved(&request.operation_id, external)
            || self.proposal_identity_reserved(&request.operation_id, external)?
        {
            return Err(api::Refusal::IdentityConflict.into());
        }
        let control = self.state.suggestions.clone().unwrap_or_else(|| {
            let enabled = self.source.required_proposal_generation();
            Control {
                enabled,
                initial_enabled: enabled,
                ..Control::default()
            }
        });
        ensure!(
            control.pending.is_none(),
            "suggestions activation needs recovery"
        );
        let refusal = if control.revision != request.expected_revision {
            Some(api::Refusal::RevisionChanged)
        } else if request.enabled && !provider_available {
            Some(api::Refusal::ProviderUnavailable)
        } else {
            None
        };
        if let Some(reason) = refusal {
            self.source.require_suggestions_control()?;
            let mut next = control;
            next.refusals.insert(
                request.operation_id.clone(),
                Refused {
                    workspace,
                    request,
                    actor: actor.into(),
                    reason,
                },
            );
            self.state.suggestions = Some(next);
            self.persist()?;
            return Err(reason.into());
        }
        self.source.require_suggestions_control()?;
        self.suggestions_cut(0)?;
        let receipt = api::Receipt {
            schema: "tessera-suggestions-receipt/v1".into(),
            workspace,
            request: request.clone(),
            actor: actor.into(),
            revision: control
                .revision
                .checked_add(1)
                .context("suggestions revision exhausted")?,
            enabled: request.enabled,
            replayed: false,
        };
        self.state.suggestions = Some(Control {
            pending: Some(receipt.clone()),
            ..control
        });
        self.persist()?;
        self.suggestions_cut(1)?;
        self.recover_suggestions_control()?;
        self.open_suggestions_store_if_needed()?;
        Ok(receipt)
    }
    fn open_suggestions_store_if_needed(&mut self) -> Result<()> {
        // Never recover a live worker's running attempt during a control command.
        // At startup the normal Runner open sequence owns store reconstruction.
        if self.source.required_proposal_drafts() && self.proposal_store.is_none() {
            self.open_proposal_drafts()?;
        }
        Ok(())
    }
    pub(super) fn recover_suggestions_control(&mut self) -> Result<()> {
        let Some(control) = self.state.suggestions.as_ref() else {
            // A crash after the fence but before the intent is deliberately
            // disabled. A repeated explicit command can finish setup.
            return Ok(());
        };
        ensure!(
            self.source.required_suggestions_control(),
            "suggestions state lacks required source fence"
        );
        ensure!(
            control.schema == "tessera-suggestions-control/v1",
            "unsupported suggestions control"
        );
        let workspace = self.workspace_identity();
        ensure!(
            control.receipts.len() as u64 == control.revision,
            "suggestions receipt sequence gap"
        );
        for (id, refusal) in &control.refusals {
            refusal.request.validate(&refusal.actor)?;
            ensure!(
                id == &refusal.request.operation_id
                    && refusal.workspace == workspace
                    && !control.receipts.contains_key(id)
                    && matches!(
                        refusal.reason,
                        api::Refusal::ProviderUnavailable | api::Refusal::RevisionChanged
                    ),
                "invalid terminal suggestions refusal"
            );
        }
        let mut revisions = std::collections::BTreeSet::new();
        for (id, receipt) in &control.receipts {
            receipt.request.validate(&receipt.actor)?;
            ensure!(
                id == &receipt.request.operation_id
                    && receipt.workspace == workspace
                    && receipt.schema == "tessera-suggestions-receipt/v1"
                    && !receipt.replayed
                    && receipt.enabled == receipt.request.enabled
                    && receipt.revision > 0
                    && receipt.revision <= control.revision
                    && receipt.request.expected_revision.checked_add(1) == Some(receipt.revision)
                    && revisions.insert(receipt.revision),
                "invalid suggestions receipt binding"
            );
        }
        if let Some(latest) = control
            .receipts
            .values()
            .find(|r| r.revision == control.revision)
        {
            ensure!(
                latest.enabled == control.enabled,
                "suggestions mode differs from final receipt"
            );
        }
        let Some(pending) = control.pending.clone() else {
            ensure!(
                !control.enabled
                    || (self.source.required_proposal_generation()
                        && self.source.required_proposal_drafts()
                        && self
                            .source
                            .required_proposal_feed()
                            .is_some_and(|b| b.active)),
                "enabled suggestions lack completed activation"
            );
            ensure!(
                control.revision > 0 || control.enabled == control.initial_enabled,
                "suggestions initial mode mismatch"
            );
            return Ok(());
        };
        pending.request.validate(&pending.actor)?;
        ensure!(
            pending.workspace == workspace
                && pending.schema == "tessera-suggestions-receipt/v1"
                && !pending.replayed
                && pending.enabled == pending.request.enabled
                && pending.request.expected_revision == control.revision
                && Some(pending.revision) == control.revision.checked_add(1)
                && !control.receipts.contains_key(&pending.request.operation_id)
                && !control.refusals.contains_key(&pending.request.operation_id),
            "invalid pending suggestions request"
        );
        if pending.enabled {
            if self.source.required_proposal_feed().is_none() {
                self.enroll_proposal_feed(1)?;
            } else if self.proposal_store.is_none() {
                self.recover_proposal_enrollment()?;
            }
            self.suggestions_cut(3)?;
            self.source.require_proposal_drafts()?;
            self.suggestions_cut(4)?;
            self.source.require_proposal_generation()?;
            self.suggestions_cut(5)?;
        }
        let control = self.state.suggestions.as_mut().unwrap();
        control.revision = pending.revision;
        control.enabled = pending.enabled;
        control
            .receipts
            .insert(pending.request.operation_id.clone(), pending);
        control.pending = None;
        self.persist()?;
        self.suggestions_cut(6)
    }
}

#[cfg(test)]
mod tests;
