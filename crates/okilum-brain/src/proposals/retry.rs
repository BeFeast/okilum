//! Explicit Retry transactions share A1's lock, reservations and projection chain.
use super::*;
use crate::proposal as api;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Operation {
    receipt: api::RetryReceipt,
    source_operation_id: Option<String>,
}

pub(super) fn validate_fence(root: &Path, journal: &Journal) -> Result<()> {
    if journal.retries.is_empty() {
        return Ok(());
    }
    let binding: serde_json::Value = serde_json::from_slice(&fs::read(
        root.parent()
            .context("operational root missing")?
            .join("source/binding.json"),
    )?)?;
    ensure!(
        binding["required_proposal_retry"] == "okilum-proposal-retry/v1",
        "retry state lacks required source fence"
    );
    for op in journal.retries.values() {
        ensure!(
            op.receipt.workspace["brain_id"] == binding["brain_id"]
                && op.receipt.workspace["root"] == binding["root"],
            "retry workspace differs from source binding"
        );
    }
    Ok(())
}
impl Journal {
    pub(super) fn validate_retries(
        &self,
        operations: &mut BTreeSet<String>,
        external: &mut BTreeSet<String>,
        revisions: &mut drafts::ProjectionRevisions<'_>,
    ) -> Result<()> {
        let mut archived_operations = BTreeSet::new();
        for (proposal_id, intent) in &self.intents {
            if let Some(draft) = &intent.draft {
                for archived in &draft.record.attempt_history {
                    let receipt = &self
                        .retries
                        .get(&archived.retry_operation_id)
                        .context("archived attempt lacks retry operation")?
                        .receipt;
                    ensure!(
                        archived_operations.insert(&archived.retry_operation_id)
                            && receipt.request.proposal_id == *proposal_id
                            && matches!(&receipt.result,
                                api::RetryOutcome::Committed { previous_attempt_id, .. }
                                    if *previous_attempt_id == archived.attempt.id),
                        "archived attempt does not bind a unique committed retry"
                    );
                }
            }
        }
        // Index each immutable projection once per validation. A linear scan
        // hashing all earlier historical snapshots for every Retry grows cubically.
        let mut projection_revisions = BTreeMap::new();
        for (proposal_id, intent) in &self.intents {
            if let Some(draft) = &intent.draft {
                if !draft.record.attempt_history.is_empty() {
                    for (position, projection) in draft.projections.iter().enumerate() {
                        projection_revisions.insert(
                            (proposal_id.as_str(), revisions.get(proposal_id, position)?),
                            projection,
                        );
                    }
                }
            }
        }
        for (id, operation) in &self.retries {
            let r = &operation.receipt;
            r.request.validate(&r.request.source.actor_id)?;
            api::utc(&r.at)?;
            let d = self
                .intents
                .get(&r.request.proposal_id)
                .and_then(|i| i.draft.as_ref())
                .context("retry proposal missing")?;
            ensure!(
                r.schema == "okilum-proposal-retry/v1"
                    && !r.replayed
                    && r.request.operation_id == *id
                    && r.workspace["brain_id"] == self.brain_id
                    && r.workspace["managed"] == true
                    && r.request.goal_id == d.record.goal_id
                    && r.path == d.projections[0].write.path
                    && operations.insert(id.clone())
                    && external.insert(r.request.source.external_key()?),
                "retry identity/namespace mismatch"
            );
            match &r.result {
                api::RetryOutcome::NotApplied { .. } => ensure!(
                    operation.source_operation_id.is_none(),
                    "terminal retry has source operation"
                ),
                api::RetryOutcome::Committed {
                    previous_attempt_id,
                    attempt_id,
                    previous_revision,
                    revision,
                } => {
                    canonical_uuid(attempt_id)?;
                    let prior = d
                        .record
                        .attempt_history
                        .iter()
                        .find(|a| a.retry_operation_id == *id)
                        .context("retry previous attempt absent")?;
                    let (position, p) = d
                        .projections
                        .iter()
                        .enumerate()
                        .find(|(_, p)| {
                            Some(&p.write.operation_id) == operation.source_operation_id.as_ref()
                        })
                        .context("retry projection absent")?;
                    let queued = drafts::decode_record(&base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &p.write.content_base64,
                    )?)?;
                    let previous = projection_revisions
                        .get(&(r.request.proposal_id.as_str(), previous_revision.clone()))
                        .context("retry predecessor projection absent")?;
                    let previous = drafts::decode_record(&base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &previous.write.content_base64,
                    )?)?;
                    ensure!(
                        queued.attempt.state == AttemptState::Queued
                            && queued.attempt.id == *attempt_id
                            && queued.attempt.input == previous.attempt.input
                            && queued.attempt_history.last() == Some(prior)
                            && prior.attempt == previous.attempt
                            && prior.failure == previous.failure
                            && queued.attempt_history.len() == previous.attempt_history.len() + 1
                            && queued.attempt_history[..previous.attempt_history.len()]
                                == previous.attempt_history,
                        "retry immutable attempt history differs from original projections"
                    );
                    ensure!(
                        prior.attempt.id == *previous_attempt_id
                            && prior.archived_at == r.at
                            && previous_revision == &r.request.expected_revision
                            && p.write.expected_revision.as_ref() == Some(previous_revision)
                            && revisions.get(&r.request.proposal_id, position)? == *revision
                            && (d.record.attempt.id == *attempt_id
                                || d.record
                                    .attempt_history
                                    .iter()
                                    .any(|a| a.attempt.id == *attempt_id)),
                        "retry attempt/projection binding mismatch"
                    );
                }
            }
        }
        Ok(())
    }
}
impl Store {
    pub(super) fn retry_identity_reserved(&self, id: &str, external: &str) -> bool {
        self.journal.retries.values().any(|o| {
            o.receipt.request.operation_id == id
                || o.receipt.request.source.external_key().ok().as_deref() == Some(external)
        })
    }
    pub(crate) fn retry_replay(
        &self,
        workspace: &serde_json::Value,
        request: &api::RetryRequest,
    ) -> Result<Option<api::RetryReceipt>> {
        self.healthy()?;
        for op in self.journal.retries.values() {
            let r = &op.receipt;
            if r.request.operation_id == request.operation_id
                || r.request.source.external_key()? == request.source.external_key()?
            {
                ensure!(
                    r.workspace == *workspace && r.request == *request,
                    "retry identity reused with changed payload"
                );
                if let Some(source_op) = &op.source_operation_id {
                    let d = self.draft(
                        &self.journal.brain_id,
                        request.goal_id.as_deref(),
                        &request.proposal_id,
                    )?;
                    ensure!(
                        d.projections
                            .iter()
                            .any(|p| p.write.operation_id == *source_op && p.receipt.is_some()),
                        "retry projection still pending"
                    );
                }
                let mut reply = r.clone();
                reply.replayed = true;
                return Ok(Some(reply));
            }
        }
        Ok(None)
    }
    pub(crate) fn retry_pending(&self, request: &api::RetryRequest) -> Result<bool> {
        self.healthy()?;
        for op in self.journal.retries.values() {
            let r = &op.receipt;
            if r.request.operation_id == request.operation_id
                || r.request.source.external_key()? == request.source.external_key()?
            {
                ensure!(
                    r.request == *request,
                    "retry identity reused with changed payload"
                );
                return Ok(op.source_operation_id.as_ref().is_some_and(|id| {
                    self.journal.intents[&request.proposal_id]
                        .draft
                        .as_ref()
                        .unwrap()
                        .projections
                        .iter()
                        .any(|p| p.write.operation_id == *id && p.receipt.is_none())
                }));
            }
        }
        Ok(false)
    }
    pub(crate) fn retry_canonical_fits(
        &self,
        request: &api::RetryRequest,
        at: &str,
    ) -> Result<bool> {
        self.healthy()?;
        let mut record = self
            .draft(
                &self.journal.brain_id,
                request.goal_id.as_deref(),
                &request.proposal_id,
            )?
            .record
            .clone();
        archive_retry(&mut record, request, at);
        drafts::generation_canonical_fits(&record)
    }
    pub(crate) fn reserve_retry(
        &mut self,
        workspace: serde_json::Value,
        request: api::RetryRequest,
        refusal: Option<api::RetryRefusal>,
        at: String,
    ) -> Result<()> {
        self.healthy()?;
        ensure!(
            !self.operation_reserved(&request.operation_id, &request.source.external_key()?)?,
            "retry operation identity already reserved"
        );
        let current = self.draft(
            &self.journal.brain_id,
            request.goal_id.as_deref(),
            &request.proposal_id,
        )?;
        ensure!(!current.pending(), "proposal projection is uncertain");
        let mut next = self.journal.clone();
        let intent = next.intents.get_mut(&request.proposal_id).unwrap();
        let draft = intent.draft.as_mut().unwrap();
        let path = draft.projections[0].write.path.clone();
        let (result, source_operation_id) = match refusal {
            Some(reason) => (api::RetryOutcome::NotApplied { reason }, None),
            None => {
                ensure!(
                    self.journal
                        .intents
                        .values()
                        .filter(|i| i.attempt.state == AttemptState::Queued)
                        .count()
                        < QUEUE_LIMIT,
                    "proposal queue full; retry not accepted"
                );
                ensure!(
                    matches!(
                        intent.attempt.state,
                        AttemptState::Failed | AttemptState::Interrupted
                    ) && matches!(
                        draft.record.disposition,
                        api::Disposition::Unreviewed | api::Disposition::Snoozed { .. }
                    ) && draft.adoption.is_none()
                        && draft.inbox_adoption.is_none()
                        && draft.latest_revision()? == request.expected_revision
                        && intent.attempt.input.as_ref().is_some_and(|i| i
                            .generation
                            .as_ref()
                            .is_some_and(|g| g.settings.is_some())),
                    "proposal cannot retry"
                );
                ensure!(
                    draft.record.attempt_history.len() < 32,
                    "proposal attempt history full"
                );
                let previous_attempt_id = intent.attempt.id.clone();
                archive_retry(&mut draft.record, &request, &at);
                intent.attempt = draft.record.attempt.clone();
                draft.queue(path.clone())?;
                (
                    api::RetryOutcome::Committed {
                        previous_attempt_id,
                        attempt_id: intent.attempt.id.clone(),
                        previous_revision: request.expected_revision.clone(),
                        revision: draft.latest_revision()?,
                    },
                    Some(draft.projections.last().unwrap().write.operation_id.clone()),
                )
            }
        };
        let receipt = api::RetryReceipt {
            schema: "okilum-proposal-retry/v1".into(),
            workspace,
            request: request.clone(),
            path,
            at,
            replayed: false,
            result,
        };
        next.retries.insert(
            request.operation_id,
            Operation {
                receipt,
                source_operation_id,
            },
        );
        self.commit(next)
    }
    pub(crate) fn start_queued_retry(
        &mut self,
        brain: &str,
        goal: Option<&str>,
        id: &str,
    ) -> Result<()> {
        let d = self.draft(brain, goal, id)?;
        ensure!(
            matches!(
                d.record.disposition,
                api::Disposition::Unreviewed | api::Disposition::Snoozed { .. }
            ) && d.adoption.is_none()
                && d.inbox_adoption.is_none(),
            "retry disposition is terminal"
        );
        ensure!(
            !d.pending()
                && d.record.attempt.state == AttemptState::Queued
                && !d.record.attempt_history.is_empty(),
            "retry not ready for dispatch"
        );
        ensure!(
            !self
                .journal
                .intents
                .values()
                .any(|i| i.attempt.state == AttemptState::Running),
            "another attempt running"
        );
        let mut next = self.journal.clone();
        let i = next.intents.get_mut(id).unwrap();
        i.attempt.state = AttemptState::Running;
        let d = i.draft.as_mut().unwrap();
        d.record.attempt = i.attempt.clone();
        d.queue(d.projections[0].write.path.clone())?;
        self.commit(next)
    }
}

fn archive_retry(record: &mut api::Record, request: &api::RetryRequest, at: &str) {
    record.attempt_history.push(api::ArchivedAttempt {
        attempt: record.attempt.clone(),
        failure: record.failure.clone(),
        archived_at: at.into(),
        retry_operation_id: request.operation_id.clone(),
    });
    record.attempt.id = Uuid::new_v4().to_string();
    record.attempt.state = AttemptState::Queued;
    record.failure = None;
}
