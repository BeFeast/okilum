//! Terminal no-write reservations share the proposal journal lock and capacity.
use super::*;
use crate::proposal as api;

pub(super) fn validate_fence(root: &Path, journal: &Journal) -> Result<()> {
    if journal.terminal_dispositions.is_empty() {
        return Ok(());
    }
    let binding: serde_json::Value = serde_json::from_slice(&fs::read(
        root.parent()
            .context("proposal operational root absent")?
            .join("source/binding.json"),
    )?)?;
    ensure!(
        binding["required_proposal_terminal"] == "tessera-proposal-terminal/v1",
        "terminal reservations lack required source fence"
    );
    for receipt in journal.terminal_dispositions.values() {
        ensure!(
            receipt.workspace["brain_id"] == binding["brain_id"]
                && receipt.workspace["root"] == binding["root"],
            "terminal reservation workspace binding differs"
        );
    }
    Ok(())
}
impl Journal {
    pub(super) fn validate_terminal_dispositions(
        &self,
        operations: &mut BTreeSet<String>,
        external: &mut BTreeSet<String>,
    ) -> Result<()> {
        for (op, r) in &self.terminal_dispositions {
            r.request.validate(&r.request.source.actor_id)?;
            let draft = self
                .intents
                .get(&r.request.proposal_id)
                .and_then(|i| i.draft.as_ref())
                .context("terminal reservation proposal absent")?;
            ensure!(
                self.drafts_enabled
                    && r.schema == "tessera-proposal-terminal/v1"
                    && r.outcome == "not_applied"
                    && !r.replayed
                    && op == &r.request.operation_id
                    && r.workspace["brain_id"] == self.brain_id
                    && r.workspace["managed"] == true
                    && r.request.goal_id == draft.record.goal_id
                    && r.path
                        == draft
                            .projections
                            .first()
                            .context("terminal proposal projection absent")?
                            .write
                            .path
                    && r.path
                        == format!(
                            "{}/proposal-{}.md",
                            r.workspace["records_dir"]
                                .as_str()
                                .context("missing terminal records directory")?,
                            r.request.proposal_id
                        )
                    && operations.insert(op.clone())
                    && external.insert(r.request.source.external_key()?),
                "terminal reservation identity mismatch"
            );
            let api::Disposition::Snoozed { until } = &r.request.disposition else {
                anyhow::bail!("terminal reservation is not a Snooze");
            };
            ensure!(
                api::utc(until)? <= api::utc(&r.at)?,
                "terminal deadline has not elapsed"
            );
        }
        Ok(())
    }
}
impl Store {
    pub(super) fn terminal_identity_reserved(&self, id: &str, external: &str) -> bool {
        self.journal.terminal_dispositions.values().any(|r| {
            r.request.operation_id == id
                || r.request.source.external_key().ok().as_deref() == Some(external)
        })
    }
    pub(crate) fn terminal_replay(
        &self,
        workspace: &serde_json::Value,
        request: &api::Request,
    ) -> Result<Option<api::TerminalReceipt>> {
        self.healthy()?;
        for r in self.journal.terminal_dispositions.values() {
            if r.request.operation_id == request.operation_id
                || r.request.source.external_key()? == request.source.external_key()?
            {
                ensure!(
                    r.request == *request && r.workspace == *workspace,
                    "terminal identity reused with changed request or workspace"
                );
                let mut receipt = r.clone();
                receipt.replayed = true;
                return Ok(Some(receipt));
            }
        }
        Ok(None)
    }
    pub(crate) fn reserve_expired(
        &mut self,
        workspace: serde_json::Value,
        request: api::Request,
        path: String,
        at: String,
    ) -> Result<api::TerminalReceipt> {
        self.healthy()?;
        ensure!(
            !self.operation_reserved(&request.operation_id, &request.source.external_key()?)?,
            "proposal identity already reserved"
        );
        let d = self.draft(
            &self.journal.brain_id,
            request.goal_id.as_deref(),
            &request.proposal_id,
        )?;
        ensure!(
            !matches!(d.record.disposition, api::Disposition::Rejected),
            "proposal already rejected"
        );
        ensure!(
            d.adoption.is_none() && d.inbox_adoption.is_none(),
            "proposal adoption locks disposition"
        );
        ensure!(
            !d.pending()
                && d.latest_revision()? == request.expected_revision
                && d.projections[0].write.path == path,
            "proposal changed before terminal reservation"
        );
        let receipt = api::TerminalReceipt {
            schema: "tessera-proposal-terminal/v1".into(),
            outcome: "not_applied".into(),
            workspace,
            request,
            path,
            reason: api::TerminalReason::DeadlineElapsed,
            at,
            replayed: false,
        };
        let mut next = self.journal.clone();
        next.terminal_dispositions
            .insert(receipt.request.operation_id.clone(), receipt.clone());
        validate_fence(&self.root, &next)?;
        self.commit(next)?;
        Ok(receipt)
    }
}
