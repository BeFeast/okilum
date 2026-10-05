//! Definite never-applied public forms reserve the common A1 namespace.
use super::*;
use crate::proposal as api;
pub(super) fn validate_fence(root: &Path, journal: &Journal) -> Result<()> {
    if journal.terminal_adoptions.is_empty() {
        return Ok(());
    }
    let binding: serde_json::Value = serde_json::from_slice(&fs::read(
        root.parent()
            .context("operational root missing")?
            .join("source/binding.json"),
    )?)?;
    ensure!(
        binding["required_public_proposal_adoption"] == "tessera-proposal-adopt/v1",
        "public adoption receipts lack source fence"
    );
    for receipt in journal.terminal_adoptions.values() {
        ensure!(
            receipt.workspace["brain_id"] == binding["brain_id"]
                && receipt.workspace["root"] == binding["root"],
            "public adoption workspace differs from source binding"
        );
    }
    Ok(())
}
impl Journal {
    pub(super) fn validate_terminal_adoptions(
        &self,
        operations: &mut BTreeSet<String>,
        external: &mut BTreeSet<String>,
    ) -> Result<()> {
        for (id, r) in &self.terminal_adoptions {
            r.request.validate(&r.request.source().actor_id)?;
            api::utc(&r.at)?;
            let draft = self
                .intents
                .get(r.request.proposal_id())
                .and_then(|i| i.draft.as_ref())
                .context("terminal adoption proposal absent")?;
            ensure!(
                self.drafts_enabled
                    && r.schema == "tessera-proposal-adopt/v1"
                    && !r.replayed
                    && matches!(r.result, api::AdoptOutcome::NotApplied { .. })
                    && r.request.operation_id() == id
                    && r.workspace["brain_id"] == self.brain_id
                    && r.workspace["managed"] == true
                    && r.request.goal_id() == draft.record.goal_id.as_deref()
                    && draft.projections[0].write.path
                        == format!(
                            "{}/proposal-{}.md",
                            r.workspace["records_dir"]
                                .as_str()
                                .context("records directory absent")?,
                            r.request.proposal_id()
                        )
                    && operations.insert(id.clone())
                    && external.insert(r.request.source().external_key()?),
                "terminal adoption identity mismatch"
            );
        }
        Ok(())
    }
}
impl Store {
    pub(super) fn terminal_adoption_identity_reserved(&self, id: &str, external: &str) -> bool {
        self.journal.terminal_adoptions.values().any(|r| {
            r.request.operation_id() == id
                || r.request.source().external_key().ok().as_deref() == Some(external)
        })
    }
    pub(crate) fn terminal_adoption_replay(
        &self,
        workspace: &serde_json::Value,
        request: &api::AdoptRequest,
    ) -> Result<Option<api::AdoptReceipt>> {
        self.healthy()?;
        for receipt in self.journal.terminal_adoptions.values() {
            if receipt.request.operation_id() == request.operation_id()
                || receipt.request.source().external_key()? == request.source().external_key()?
            {
                ensure!(
                    receipt.request == *request && receipt.workspace == *workspace,
                    "adoption identity reused with changed payload or workspace"
                );
                let mut receipt = receipt.clone();
                receipt.replayed = true;
                return Ok(Some(receipt));
            }
        }
        Ok(None)
    }
    pub(crate) fn reserve_terminal_adoption(
        &mut self,
        workspace: serde_json::Value,
        request: api::AdoptRequest,
        reason: api::AdoptRefusal,
        at: String,
    ) -> Result<api::AdoptReceipt> {
        self.healthy()?;
        ensure!(
            !self.operation_reserved(request.operation_id(), &request.source().external_key()?)?,
            "adoption operation identity already reserved"
        );
        let receipt = api::AdoptReceipt {
            schema: "tessera-proposal-adopt/v1".into(),
            workspace,
            request,
            at,
            replayed: false,
            result: api::AdoptOutcome::NotApplied { reason },
        };
        let mut next = self.journal.clone();
        next.terminal_adoptions
            .insert(receipt.request.operation_id().into(), receipt.clone());
        self.commit(next)?;
        Ok(receipt)
    }
}
