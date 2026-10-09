//! Extensions to the existing A1 journal. Exact source operations precede writes.
use super::*;
use crate::proposal as api;
use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_core::source::{SourceSnapshot, SourceWrite, WriteReceipt};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Projection {
    pub write: SourceWrite,
    pub receipt: Option<WriteReceipt>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    pub request: api::Request,
    pub receipt: api::Receipt,
    pub source_operation_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Draft {
    pub record: api::Record,
    pub projections: Vec<Projection>,
    pub operations: BTreeMap<String, Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adoption: Option<super::adoption::Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_adoption: Option<super::inbox_adoption::Operation>,
}

/// Actual-byte revisions shared only while one immutable journal is validated.
pub(super) struct ProjectionRevisions<'a> {
    journal: &'a Journal,
    values: BTreeMap<(&'a str, usize), String>,
}
impl<'a> ProjectionRevisions<'a> {
    pub(super) fn new(journal: &'a Journal) -> Self {
        Self {
            journal,
            values: BTreeMap::new(),
        }
    }
    pub(super) fn get(&mut self, id: &str, position: usize) -> Result<String> {
        // Resolve the owning map key and position; claimed operation IDs and
        // receipt revisions cannot alias different retained projections.
        let (key, intent) = self
            .journal
            .intents
            .get_key_value(id)
            .context("proposal revision owner absent")?;
        let projection = intent
            .draft
            .as_ref()
            .and_then(|draft| draft.projections.get(position))
            .context("proposal revision position absent")?;
        let value = match self.values.entry((key.as_str(), position)) {
            std::collections::btree_map::Entry::Occupied(value) => value.into_mut(),
            std::collections::btree_map::Entry::Vacant(value) => {
                value.insert(revision(&projection.write)?)
            }
        };
        Ok(value.clone())
    }
}
impl Draft {
    pub(super) fn validate(
        &self,
        id: &str,
        intent: &Intent,
        revisions: &mut ProjectionRevisions<'_>,
    ) -> Result<()> {
        let r = &self.record;
        ensure!(
            r.schema == api::SCHEMA
                && r.record_type == "proposal"
                && r.id == id
                && r.brain_id == intent.trigger.identity.brain_id
                && r.goal_id == intent.trigger.goal_id
                && r.trigger == intent.trigger
                && r.attempt == intent.attempt
                && r.verification == "unverified",
            "proposal draft owner mismatch"
        );
        if let Some(g) = r.attempt.input.as_ref().and_then(|v| v.generation.as_ref()) {
            g.validate_captured(&r.trigger, &r.captured)?;
        }
        ensure!(
            r.attempt_history.len() <= 32,
            "attempt history exceeds bound"
        );
        let mut previous_ids = BTreeSet::new();
        for prior in &r.attempt_history {
            canonical_uuid(&prior.attempt.id)?;
            canonical_uuid(&prior.retry_operation_id)?;
            api::utc(&prior.archived_at)?;
            ensure!(
                prior.attempt.id != r.attempt.id
                    && previous_ids.insert(&prior.attempt.id)
                    && matches!(
                        prior.attempt.state,
                        AttemptState::Failed | AttemptState::Interrupted
                    )
                    && prior.attempt.input == r.attempt.input,
                "attempt history/input mismatch"
            );
            prior
                .attempt
                .input
                .as_ref()
                .context("previous attempt input absent")?
                .validate()?;
        }
        api::utc(&r.created_at)?;
        if let Some(at) = &r.generated_at {
            api::utc(at)?;
        }
        if let Some(g) = &r.generated {
            g.validate(&r.captured)?;
        }
        ensure!(
            r.generated.is_some() == r.generated_at.is_some(),
            "proposal result timestamp mismatch"
        );
        ensure!(
            matches!(
                (r.attempt.state, &r.generated, &r.failure),
                (AttemptState::Draft, Some(_), None)
                    | (AttemptState::Queued | AttemptState::Running, None, None)
                    | (AttemptState::Interrupted, None, _)
                    | (AttemptState::Failed | AttemptState::Stale, None, Some(_))
            ),
            "proposal generation state/result mismatch"
        );
        let mut prior = None;
        let mut pending = false;
        let mut ids = BTreeSet::new();
        for (position, p) in self.projections.iter().enumerate() {
            ensure!(
                ids.insert(&p.write.operation_id)
                    && p.write.brain_id == r.brain_id
                    && p.write.expected_revision == prior,
                "proposal projection chain mismatch"
            );
            canonical_uuid(&p.write.operation_id)?;
            ensure!(
                p.write.path == self.projections[0].write.path
                    && p.write.path.len() <= 4096
                    && p.write.schema == okilum_core::source::SCHEMA,
                "proposal projection path/schema mismatch"
            );
            let revision = revisions.get(id, position)?;
            if let Some(receipt) = &p.receipt {
                ensure!(
                    !pending && matches_receipt_revision(&p.write, receipt, &revision),
                    "proposal projection receipt mismatch"
                );
            } else {
                pending = true;
            }
            prior = Some(revision);
        }
        let last = self
            .projections
            .last()
            .context("proposal has no retained projection")?;
        ensure!(
            STANDARD.decode(&last.write.content_base64)? == r.bytes()?,
            "proposal canonical bytes differ from retained record"
        );
        for (op, o) in &self.operations {
            o.request.validate(&o.request.source.actor_id)?;
            let projection = self
                .projections
                .iter()
                .find(|p| p.write.operation_id == o.source_operation_id)
                .context("proposal disposition projection missing")?;
            let history = r
                .history
                .iter()
                .find(|h| h.operation_id == *op)
                .context("canonical disposition history missing")?;
            ensure!(
                o.receipt.actor == o.request.source.actor_id
                    && o.receipt.actor == history.actor
                    && o.receipt.at == history.at
                    && o.receipt.disposition == o.request.disposition
                    && o.receipt.disposition == history.disposition
                    && o.receipt.previous_revision == o.request.expected_revision
                    && o.receipt.previous_revision == history.expected_revision
                    && projection.write.expected_revision.as_ref()
                        == Some(&o.request.expected_revision)
                    && !o.receipt.replayed,
                "proposal disposition canonical receipt mismatch"
            );
            api::utc(&o.receipt.at)?;
            ensure!(
                op == &o.request.operation_id
                    && op == &o.receipt.operation_id
                    && o.request.proposal_id == id
                    && o.request.goal_id == r.goal_id
                    && self.projections.iter().enumerate().any(|(position, p)| p
                        .write
                        .operation_id
                        == o.source_operation_id
                        && p.write.path == o.receipt.path
                        && revisions.get(id, position).ok().as_ref() == Some(&o.receipt.revision)),
                "proposal disposition receipt binding mismatch"
            );
        }
        ensure!(
            self.adoption.is_none() || self.inbox_adoption.is_none(),
            "multiple adoption destinations"
        );
        if let Some(adoption) = &self.inbox_adoption {
            adoption.validate(self)?;
        }
        if let Some(adoption) = &self.adoption {
            adoption.validate(self)?;
        }
        Ok(())
    }
    pub(super) fn interrupted(&mut self, attempt: &Attempt) -> Result<()> {
        self.record.attempt = attempt.clone();
        self.record.failure = Some(api::Failure::Interrupted);
        self.queue(self.projections[0].write.path.clone())
    }
    pub(crate) fn pending(&self) -> bool {
        self.projections.iter().any(|p| p.receipt.is_none())
    }
    pub(crate) fn latest_revision(&self) -> Result<String> {
        revision(&self.projections.last().context("missing projection")?.write)
    }
    pub(super) fn queue(&mut self, path: String) -> Result<()> {
        let expected_revision = self
            .projections
            .last()
            .map(|p| revision(&p.write))
            .transpose()?;
        self.projections.push(Projection {
            write: SourceWrite {
                schema: okilum_core::source::SCHEMA.into(),
                operation_id: Uuid::new_v4().to_string(),
                brain_id: self.record.brain_id.clone(),
                path,
                expected_revision,
                content_base64: STANDARD.encode(self.record.bytes()?),
            },
            receipt: None,
        });
        Ok(())
    }
    pub(crate) fn visible(&self) -> Result<(api::Record, SourceSnapshot)> {
        // A generated result is not visible until its exact projection receipt.
        let p = self
            .projections
            .iter()
            .rev()
            .find(|p| p.receipt.is_some())
            .unwrap_or(&self.projections[0]);
        let bytes = STANDARD.decode(&p.write.content_base64)?;
        let record = decode_record(&bytes)?;
        Ok((
            record,
            SourceSnapshot {
                schema: p.write.schema.clone(),
                brain_id: p.write.brain_id.clone(),
                path: p.write.path.clone(),
                revision: revision(&p.write)?,
                content_base64: p.write.content_base64.clone(),
                media_type: "text/markdown".into(),
            },
        ))
    }
}
pub(super) fn decode_record(bytes: &[u8]) -> Result<api::Record> {
    let text = std::str::from_utf8(bytes)?;
    let rest = text
        .strip_prefix("---\n")
        .context("proposal frontmatter missing")?;
    let mut end = 0;
    let mut found = false;
    for line in rest.split_inclusive('\n') {
        if crate::retrieval::frontmatter_delimiter(line) {
            found = true;
            break;
        }
        end += line.len();
    }
    ensure!(found, "proposal frontmatter missing");
    let record = serde_yaml::from_str(&rest[..end])?;
    Ok(record)
}
pub(crate) fn revision(write: &SourceWrite) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(STANDARD.decode(&write.content_base64)?)
    ))
}
fn matches_receipt(w: &SourceWrite, r: &WriteReceipt) -> Result<bool> {
    Ok(r.operation_id == w.operation_id
        && r.path == w.path
        && r.previous_revision == w.expected_revision
        && r.revision == revision(w)?)
}
fn matches_receipt_revision(w: &SourceWrite, r: &WriteReceipt, revision: &str) -> bool {
    r.operation_id == w.operation_id
        && r.path == w.path
        && r.previous_revision == w.expected_revision
        && r.revision == revision
}
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_CANONICAL_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(1024 * 1024 - 1024) };
    pub(crate) static TEST_CANDIDATE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// Generated JSON is <=32KiB; YAML and visible text need publication headroom.
/// Queued records also reserve the transition to Running.
pub(super) fn reserve_generation_canonical(record: &api::Record) -> Result<()> {
    ensure!(
        generation_canonical_fits(record)?,
        "canonical proposal exceeds generation publication reserve"
    );
    Ok(())
}
pub(super) fn generation_canonical_fits(record: &api::Record) -> Result<bool> {
    let bytes = record.serialized_bytes()?.len();
    let reserve = if record.attempt.state == AttemptState::Queued {
        257 * 1024
    } else {
        256 * 1024
    };
    #[cfg(test)]
    let limit = TEST_CANONICAL_LIMIT.with(|value| value.get());
    #[cfg(not(test))]
    let limit = 1024 * 1024 - 1024;
    #[cfg(test)]
    TEST_CANDIDATE_BYTES.with(|value| value.set(bytes));
    Ok(bytes.checked_add(reserve).is_some_and(|size| size <= limit))
}

impl Store {
    pub(crate) fn enable_drafts(&mut self) -> Result<()> {
        if self.journal.drafts_enabled {
            return Ok(());
        }
        let mut next = self.journal.clone();
        next.drafts_enabled = true;
        self.commit(next)
    }
    pub(crate) fn disposition_pending(&self, request: &api::Request) -> Result<bool> {
        for d in self
            .journal
            .intents
            .values()
            .filter_map(|i| i.draft.as_ref())
        {
            for o in d.operations.values() {
                if o.request.operation_id == request.operation_id
                    || o.request.source.external_key()? == request.source.external_key()?
                {
                    ensure!(
                        o.request == *request,
                        "proposal operation identity reused with changed payload"
                    );
                    return Ok(d.projections.iter().any(|p| {
                        p.write.operation_id == o.source_operation_id && p.receipt.is_none()
                    }));
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn intents(&self) -> Result<&BTreeMap<String, Intent>> {
        self.healthy()?;
        Ok(&self.journal.intents)
    }
    pub(crate) fn draft(&self, brain: &str, goal: Option<&str>, id: &str) -> Result<&Draft> {
        self.get(brain, goal, id)?
            .draft
            .as_ref()
            .context("proposal has no canonical draft")
    }
    pub(crate) fn operation_reserved(&self, id: &str, external: &str) -> Result<bool> {
        self.healthy()?;
        Ok(self.terminal_adoption_identity_reserved(id, external)
            || self.terminal_identity_reserved(id, external)
            || self.retry_identity_reserved(id, external)
            || self
                .journal
                .intents
                .values()
                .filter_map(|i| i.draft.as_ref())
                .any(|d| {
                    d.inbox_adoption
                        .as_ref()
                        .is_some_and(|a| a.reserves(id, external))
                        || d.adoption
                            .as_ref()
                            .is_some_and(|a| a.reserves(id, external))
                        || d.operations.contains_key(id)
                        || d.operations.values().any(|o| {
                            o.request.source.external_key().ok().as_deref() == Some(external)
                        })
                }))
    }
    pub(crate) fn begin_draft(
        &mut self,
        brain: &str,
        goal: Option<&str>,
        id: &str,
        start: DraftStart,
    ) -> Result<()> {
        let DraftStart {
            input,
            captured,
            actor,
            at,
            path,
        } = start;
        ensure!(
            self.journal.drafts_enabled,
            "canonical proposal capability is not enrolled"
        );
        input.validate()?;
        api::utc(&at)?;
        bounded(&actor, 256)?;
        let current = self.get(brain, goal, id)?;
        if let Some(d) = &current.draft {
            ensure!(
                current.attempt.input.as_ref() == Some(&input)
                    && d.record.captured == captured
                    && d.record.generation_actor == actor,
                "proposal attempt input changed"
            );
            return Ok(());
        }
        ensure!(
            current.attempt.state == AttemptState::Queued
                && !self
                    .journal
                    .intents
                    .values()
                    .any(|i| i.attempt.state == AttemptState::Running),
            "proposal attempt cannot start"
        );
        let mut next = self.journal.clone();
        let i = next.intents.get_mut(id).unwrap();
        i.attempt.state = AttemptState::Running;
        i.attempt.input = Some(input);
        let record = api::Record {
            schema: api::SCHEMA.into(),
            record_type: "proposal".into(),
            id: id.into(),
            brain_id: brain.into(),
            goal_id: goal.map(str::to_string),
            trigger: i.trigger.clone(),
            attempt: i.attempt.clone(),
            captured,
            created_at: at,
            generation_actor: actor,
            generated_at: None,
            verification: "unverified".into(),
            generated: None,
            failure: None,
            disposition: api::Disposition::Unreviewed,
            history: vec![],
            attempt_history: vec![],
        };
        let mut draft = Draft {
            record,
            projections: vec![],
            operations: BTreeMap::new(),
            adoption: None,
            inbox_adoption: None,
        };
        draft.queue(path)?;
        i.draft = Some(draft);
        self.commit(next)
    }
    pub(crate) fn finish_draft(
        &mut self,
        brain: &str,
        goal: Option<&str>,
        id: &str,
        result: std::result::Result<api::Generated, api::Failure>,
        at: &str,
    ) -> Result<bool> {
        api::utc(at)?;
        let i = self.get(brain, goal, id)?;
        let d = i.draft.as_ref().context("proposal not started")?;
        if matches!(d.record.disposition, api::Disposition::Rejected) {
            return Ok(false);
        }
        if i.attempt.state == AttemptState::Draft {
            ensure!(
                result.as_ref().ok() == d.record.generated.as_ref(),
                "completion replay changed generated output"
            );
            return Ok(false);
        }
        ensure!(
            i.attempt.state == AttemptState::Running,
            "proposal attempt is no longer running"
        );
        ensure!(
            !d.pending(),
            "pending proposal projection requires recovery"
        );
        if let Ok(g) = &result {
            g.validate(&d.record.captured)?;
        }
        let mut next = self.journal.clone();
        let i = next.intents.get_mut(id).unwrap();
        let d = i.draft.as_mut().unwrap();
        match result {
            Ok(g) => {
                i.attempt.state = AttemptState::Draft;
                d.record.generated = Some(g);
                d.record.generated_at = Some(at.into());
            }
            Err(f) => {
                i.attempt.state = if f == api::Failure::SourceChanged {
                    AttemptState::Stale
                } else {
                    AttemptState::Failed
                };
                d.record.failure = Some(f);
            }
        }
        d.record.attempt = i.attempt.clone();
        let path = d.projections[0].write.path.clone();
        d.queue(path)?;
        self.commit(next)?;
        Ok(true)
    }
    pub(crate) fn disposition_replay(
        &self,
        brain: &str,
        request: &api::Request,
    ) -> Result<Option<api::Receipt>> {
        self.healthy()?;
        ensure!(brain == self.journal.brain_id, "wrong proposal workspace");
        let external = request.source.external_key()?;
        for d in self
            .journal
            .intents
            .values()
            .filter_map(|i| i.draft.as_ref())
        {
            for (id, o) in &d.operations {
                if id == &request.operation_id || o.request.source.external_key()? == external {
                    ensure!(
                        o.request == *request,
                        "proposal operation identity reused with changed payload"
                    );
                    let committed = d.projections.iter().any(|p| {
                        p.write.operation_id == o.source_operation_id && p.receipt.is_some()
                    });
                    ensure!(committed, "proposal operation projection requires recovery");
                    let mut receipt = o.receipt.clone();
                    receipt.replayed = true;
                    return Ok(Some(receipt));
                }
            }
        }
        Ok(None)
    }
    pub(crate) fn stage_disposition(
        &mut self,
        brain: &str,
        request: api::Request,
        at: &str,
    ) -> Result<()> {
        ensure!(
            !self.operation_reserved(&request.operation_id, &request.source.external_key()?)?,
            "proposal identity is already reserved"
        );
        ensure!(
            !self.operation_reserved(&request.operation_id, &request.source.external_key()?)?,
            "proposal operation identity already reserved"
        );
        let i = self.get(brain, request.goal_id.as_deref(), &request.proposal_id)?;
        let d = i.draft.as_ref().context("proposal has no draft")?;
        ensure!(
            d.adoption.is_none() && d.inbox_adoption.is_none(),
            "proposal revision is reserved for adoption"
        );
        ensure!(
            !d.pending() && d.latest_revision()? == request.expected_revision,
            "proposal revision changed"
        );
        ensure!(
            !matches!(d.record.disposition, api::Disposition::Rejected),
            "proposal already rejected"
        );
        let proposal_id = request.proposal_id.clone();
        let mut next = self.journal.clone();
        let d = next
            .intents
            .get_mut(&request.proposal_id)
            .unwrap()
            .draft
            .as_mut()
            .unwrap();
        if matches!(request.disposition, api::Disposition::Rejected)
            && matches!(
                d.record.attempt.state,
                AttemptState::Queued | AttemptState::Running
            )
        {
            d.record.attempt.state = AttemptState::Interrupted;
        }
        d.record.disposition = request.disposition.clone();
        d.record.history.push(api::History {
            operation_id: request.operation_id.clone(),
            expected_revision: request.expected_revision.clone(),
            actor: request.source.actor_id.clone(),
            at: at.into(),
            disposition: request.disposition.clone(),
        });
        let path = d.projections[0].write.path.clone();
        d.queue(path.clone())?;
        let receipt = api::Receipt {
            operation_id: request.operation_id.clone(),
            proposal_id: request.proposal_id.clone(),
            path,
            previous_revision: request.expected_revision.clone(),
            revision: d.latest_revision()?,
            actor: request.source.actor_id.clone(),
            at: at.into(),
            disposition: request.disposition.clone(),
            replayed: false,
        };
        d.operations.insert(
            request.operation_id.clone(),
            Operation {
                request,
                receipt,
                source_operation_id: d.projections.last().unwrap().write.operation_id.clone(),
            },
        );
        let i = next.intents.get_mut(&proposal_id).unwrap();
        i.attempt = i.draft.as_ref().unwrap().record.attempt.clone();
        self.commit(next)
    }
    pub(crate) fn pending_projection(&self) -> Result<Option<(String, SourceWrite)>> {
        self.pending_projection_for(None)
    }
    pub(crate) fn pending_projection_for(
        &self,
        proposal: Option<&str>,
    ) -> Result<Option<(String, SourceWrite)>> {
        self.healthy()?;
        Ok(self
            .journal
            .intents
            .iter()
            .filter(|(id, _)| proposal.is_none_or(|p| p == id.as_str()))
            .find_map(|(id, i)| {
                i.draft.as_ref().and_then(|d| {
                    d.projections
                        .iter()
                        .find(|p| p.receipt.is_none())
                        .map(|p| (id.clone(), p.write.clone()))
                })
            }))
    }
    pub(crate) fn acknowledge_projection(
        &mut self,
        id: &str,
        write: &SourceWrite,
        receipt: WriteReceipt,
    ) -> Result<()> {
        ensure!(
            matches_receipt(write, &receipt)?,
            "proposal source receipt mismatch"
        );
        let mut next = self.journal.clone();
        let d = next
            .intents
            .get_mut(id)
            .and_then(|i| i.draft.as_mut())
            .context("proposal missing")?;
        let p = d
            .projections
            .iter_mut()
            .find(|p| p.write.operation_id == write.operation_id)
            .context("projection missing")?;
        ensure!(
            p.write == *write && p.receipt.as_ref().is_none_or(|r| r == &receipt),
            "proposal source operation changed"
        );
        p.receipt = Some(receipt);
        self.commit(next)
    }
}

pub(crate) struct DraftStart {
    pub input: FrozenInput,
    pub captured: api::CapturedInput,
    pub actor: String,
    pub at: String,
    pub path: String,
}
