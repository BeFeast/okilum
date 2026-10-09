//! Internal context destination transactions in the existing A1 journal.
#![cfg_attr(not(test), allow(dead_code))]
use super::*;
use crate::{
    context::frozen_target::FrozenTarget,
    proposal as api,
    retrieval::{Citation, SearchScope},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_core::source::{SourceWrite, WriteReceipt};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptionRequest {
    pub operation_id: String,
    pub proposal_id: String,
    pub goal_id: String,
    pub expected_revision: String,
    pub expected_goal_revision: String,
    pub source: crate::inbox::SourceIdentity,
    pub expected_base_packet: Option<String>,
    pub expected_base_revision: Option<String>,
    pub query: String,
    pub scope: SearchScope,
    pub citations: Vec<Citation>,
    pub pinned_citation_ids: Vec<String>,
    pub guidance: String,
}
impl AdoptionRequest {
    pub fn validate(&self, actor: &str) -> Result<()> {
        canonical_uuid(&self.operation_id)?;
        canonical_uuid(&self.goal_id)?;
        self.source.validate_native(actor)?;
        ensure!(
            valid_digest(&self.proposal_id)
                && self.expected_revision.starts_with("sha256:")
                && self.expected_revision.len() == 71
                && self.expected_goal_revision.starts_with("sha256:")
                && self.expected_goal_revision.len() == 71,
            "invalid adoption revision/identity"
        );
        ensure!(
            self.scope.goal_id == self.goal_id
                && self.expected_base_packet.is_some() == self.expected_base_revision.is_some(),
            "adoption base context identity mismatch"
        );
        if let Some(id) = &self.expected_base_packet {
            canonical_uuid(id)?;
        }
        for revision in [&self.expected_revision, &self.expected_goal_revision]
            .into_iter()
            .chain(self.expected_base_revision.as_ref())
        {
            ensure!(
                revision.strip_prefix("sha256:").is_some_and(valid_digest),
                "invalid adoption revision"
            );
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= 256 * 1024,
            "adoption form exceeds bound"
        );
        Ok(())
    }
    fn equal(&self, other: &Self) -> Result<bool> {
        Ok(self == other)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerOutcome {
    Updated,
    AlreadySelected,
    PreservedNewer,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptionReceipt {
    pub operation_id: String,
    pub proposal_id: String,
    pub goal_id: String,
    pub actor: String,
    pub at: String,
    pub target: api::ContextTarget,
    pub target_receipt: WriteReceipt,
    pub projection_receipt: WriteReceipt,
    pub pointer: PointerOutcome,
    pub replayed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    pub request: AdoptionRequest,
    pub records_dir: String,
    pub at: String,
    /// FrozenTarget has a checked restore; retain the original serialization.
    pub frozen_target_base64: String,
    pub target_receipt: Option<WriteReceipt>,
    pub pointer: Option<PointerOutcome>,
    /// Precomputed bounded canonical bytes reserve capacity before target writes.
    pub adopted_write: SourceWrite,
    pub adopted_record: api::Record,
    pub projected: bool,
}
impl Operation {
    pub fn target(&self) -> Result<FrozenTarget> {
        FrozenTarget::restore(
            &STANDARD.decode(&self.frozen_target_base64)?,
            &self.adopted_record.brain_id,
            &self.records_dir,
            &self.request.goal_id,
        )
    }
    pub fn reserves(&self, op: &str, external: &str) -> bool {
        self.request.operation_id == op
            || self.request.source.external_key().ok().as_deref() == Some(external)
    }
    pub fn validate(&self, draft: &super::drafts::Draft) -> Result<()> {
        self.request.validate(&self.request.source.actor_id)?;
        api::utc(&self.at)?;
        canonical_uuid(&self.adopted_write.operation_id)?;
        ensure!(
            self.adopted_write.schema == crate::SCHEMA,
            "invalid adoption projection schema"
        );
        let target = self.target()?;
        let packet = target.packet()?;
        ensure!(
            packet.goal_revision == self.request.expected_goal_revision
                && packet.query == self.request.query
                && packet.scope == self.request.scope
                && packet.citations == self.request.citations
                && packet.pinned_citation_ids == self.request.pinned_citation_ids
                && packet.text == self.request.guidance,
            "frozen target differs from accepted form"
        );
        let r = &self.adopted_record;
        let before = draft
            .projections
            .iter()
            .find(|p| {
                super::drafts::revision(&p.write).ok().as_ref()
                    == Some(&self.request.expected_revision)
            })
            .context("adoption base projection absent")?;
        let prior_bytes = STANDARD.decode(&before.write.content_base64)?;
        let prior_text = std::str::from_utf8(&prior_bytes)?
            .strip_prefix("---\n")
            .context("missing proposal metadata")?;
        let (yaml, _) = prior_text
            .split_once("\n---\n")
            .context("missing proposal metadata end")?;
        let prior: api::Record = serde_yaml::from_str(yaml)?;
        let mut expected = prior.clone();
        expected.disposition = api::Disposition::Adopted {
            target: target_ref(&target),
        };
        expected.history.push(api::History {
            operation_id: self.request.operation_id.clone(),
            expected_revision: self.request.expected_revision.clone(),
            actor: self.request.source.actor_id.clone(),
            at: self.at.clone(),
            disposition: expected.disposition.clone(),
        });
        ensure!(*r == expected, "adoption changed retained canonical fields");
        ensure!(
            r.id == draft.record.id
                && r.brain_id == draft.record.brain_id
                && r.goal_id == Some(self.request.goal_id.clone())
                && r.generated == draft.record.generated
                && r.captured == draft.record.captured
                && r.trigger == draft.record.trigger
                && r.attempt == draft.record.attempt
                && self.request.proposal_id == r.id
                && self.adopted_write.path == draft.projections[0].write.path
                && self.adopted_write.brain_id == r.brain_id
                && self.adopted_write.expected_revision.as_ref()
                    == Some(&self.request.expected_revision)
                && STANDARD.decode(&self.adopted_write.content_base64)? == r.bytes()?,
            "adoption canonical transaction mismatch"
        );
        ensure!(
            r.disposition
                == api::Disposition::Adopted {
                    target: target_ref(&target)
                }
                && r.history
                    .last()
                    .is_some_and(|h| h.operation_id == self.request.operation_id
                        && h.expected_revision == self.request.expected_revision
                        && h.actor == self.request.source.actor_id
                        && h.at == self.at
                        && h.disposition == r.disposition),
            "adoption history/target mismatch"
        );
        if let Some(receipt) = &self.target_receipt {
            ensure!(
                matches_target(&target, receipt),
                "adoption target receipt mismatch"
            );
        }
        ensure!(
            self.pointer.is_none()
                || (self.target_receipt.is_some()
                    && self.projected
                    && draft
                        .projections
                        .iter()
                        .any(|p| p.write == self.adopted_write && p.receipt.is_some())),
            "adoption pointer precedes target receipt"
        );
        if !self.projected {
            ensure!(
                draft.latest_revision()? == self.request.expected_revision,
                "pending adoption base was replaced"
            );
        }
        if self.projected {
            ensure!(
                draft.record == self.adopted_record
                    && draft
                        .projections
                        .last()
                        .is_some_and(|p| p.write == self.adopted_write),
                "adoption projection is not canonical tip"
            );
            ensure!(
                self.target_receipt.is_some()
                    && draft
                        .projections
                        .iter()
                        .any(|p| p.write == self.adopted_write),
                "adoption projection precedes target/pointer receipt"
            );
        }
        Ok(())
    }
}
fn matches_target(t: &FrozenTarget, r: &WriteReceipt) -> bool {
    r.operation_id == t.source_write().operation_id
        && r.path == t.source_write().path
        && r.previous_revision.is_none()
        && r.revision == t.revision()
}
fn target_ref(t: &FrozenTarget) -> api::ContextTarget {
    api::ContextTarget {
        goal_id: t.goal_id().into(),
        packet_id: t.packet_id().into(),
        path: t.source_write().path.clone(),
        revision: t.revision().into(),
    }
}
impl Store {
    pub(crate) fn enable_adoption(&mut self) -> Result<()> {
        ensure!(self.journal.drafts_enabled, "adoption needs drafts");
        if self.journal.adoption_enabled {
            return Ok(());
        }
        let mut next = self.journal.clone();
        next.adoption_enabled = true;
        self.commit(next)
    }
    pub(crate) fn adoption_enabled(&self) -> bool {
        self.journal.adoption_enabled
    }
    pub(crate) fn adoption_operations(&self) -> Result<Vec<String>> {
        self.healthy()?;
        Ok(self
            .journal
            .intents
            .iter()
            .filter(|(_, i)| i.draft.as_ref().is_some_and(|d| d.adoption.is_some()))
            .map(|(id, _)| id.clone())
            .collect())
    }
    pub(crate) fn adoption(&self, id: &str) -> Result<&Operation> {
        self.healthy()?;
        self.journal
            .intents
            .get(id)
            .and_then(|i| i.draft.as_ref())
            .and_then(|d| d.adoption.as_ref())
            .context("adoption operation absent")
    }
    pub(crate) fn adoption_replay(
        &self,
        request: &AdoptionRequest,
    ) -> Result<Option<Option<AdoptionReceipt>>> {
        self.healthy()?;
        let external = request.source.external_key()?;
        for i in self.journal.intents.values() {
            if let Some(d) = &i.draft {
                if let Some(a) = &d.adoption {
                    if a.reserves(&request.operation_id, &external) {
                        ensure!(
                            a.request.equal(request)?,
                            "adoption operation identity reused with changed payload"
                        );
                        let source = d
                            .projections
                            .iter()
                            .find(|p| p.write == a.adopted_write)
                            .and_then(|p| p.receipt.clone());
                        return Ok(Some(match (&a.target_receipt, &a.pointer, source) {
                            (Some(target), Some(pointer), Some(projection)) => {
                                Some(AdoptionReceipt {
                                    operation_id: request.operation_id.clone(),
                                    proposal_id: request.proposal_id.clone(),
                                    goal_id: request.goal_id.clone(),
                                    actor: a.request.source.actor_id.clone(),
                                    at: a.at.clone(),
                                    target: target_ref(&a.target()?),
                                    target_receipt: target.clone(),
                                    projection_receipt: projection,
                                    pointer: pointer.clone(),
                                    replayed: true,
                                })
                            }
                            _ => None,
                        }));
                    }
                }
            }
        }
        Ok(None)
    }
    pub(crate) fn validate_adoption_capacity(
        &self,
        request: AdoptionRequest,
        target: FrozenTarget,
        records_dir: String,
        at: String,
    ) -> Result<()> {
        let next = self.prepare_adoption(request, target, records_dir, at)?;
        self.validate_commit(&next)?;
        Ok(())
    }
    pub(crate) fn stage_adoption(
        &mut self,
        request: AdoptionRequest,
        target: FrozenTarget,
        records_dir: String,
        at: String,
    ) -> Result<()> {
        ensure!(
            self.journal.adoption_enabled,
            "adoption capability not enrolled"
        );
        let next = self.prepare_adoption(request, target, records_dir, at)?;
        self.commit(next)
    }
    fn prepare_adoption(
        &self,
        request: AdoptionRequest,
        target: FrozenTarget,
        records_dir: String,
        at: String,
    ) -> Result<Journal> {
        request.validate(&request.source.actor_id)?;
        ensure!(
            !self.operation_reserved(&request.operation_id, &request.source.external_key()?)?,
            "adoption identity already reserved"
        );
        let d = self.draft(
            &self.journal.brain_id,
            Some(&request.goal_id),
            &request.proposal_id,
        )?;
        ensure!(
            d.adoption.is_none()
                && d.inbox_adoption.is_none()
                && !d.pending()
                && d.latest_revision()? == request.expected_revision
                && d.record.generated.is_some()
                && matches!(
                    d.record.disposition,
                    api::Disposition::Unreviewed | api::Disposition::Snoozed { .. }
                ),
            "proposal is not eligible for adoption"
        );
        let mut record = d.record.clone();
        record.disposition = api::Disposition::Adopted {
            target: target_ref(&target),
        };
        record.history.push(api::History {
            operation_id: request.operation_id.clone(),
            expected_revision: request.expected_revision.clone(),
            actor: request.source.actor_id.clone(),
            at: at.clone(),
            disposition: record.disposition.clone(),
        });
        let write = SourceWrite {
            schema: crate::SCHEMA.into(),
            operation_id: Uuid::new_v4().to_string(),
            brain_id: record.brain_id.clone(),
            path: d.projections[0].write.path.clone(),
            expected_revision: Some(request.expected_revision.clone()),
            content_base64: STANDARD.encode(record.bytes()?),
        };
        let op = Operation {
            request: request.clone(),
            records_dir,
            at,
            frozen_target_base64: STANDARD.encode(serde_json::to_vec(&target)?),
            target_receipt: None,
            pointer: None,
            adopted_write: write,
            adopted_record: record,
            projected: false,
        };
        let mut next = self.journal.clone();
        next.adoption_enabled = true;
        next.intents
            .get_mut(&request.proposal_id)
            .unwrap()
            .draft
            .as_mut()
            .unwrap()
            .adoption = Some(op);
        Ok(next)
    }
    pub(crate) fn acknowledge_adoption_target(
        &mut self,
        id: &str,
        receipt: WriteReceipt,
    ) -> Result<()> {
        let mut next = self.journal.clone();
        let op = next
            .intents
            .get_mut(id)
            .and_then(|i| i.draft.as_mut())
            .and_then(|d| d.adoption.as_mut())
            .context("adoption missing")?;
        ensure!(
            matches_target(&op.target()?, &receipt)
                && op.target_receipt.as_ref().is_none_or(|r| r == &receipt),
            "adoption target receipt changed"
        );
        op.target_receipt = Some(receipt);
        self.commit(next)
    }
    pub(crate) fn acknowledge_adoption_pointer(
        &mut self,
        id: &str,
        pointer: PointerOutcome,
    ) -> Result<()> {
        let mut next = self.journal.clone();
        let op = next
            .intents
            .get_mut(id)
            .and_then(|i| i.draft.as_mut())
            .and_then(|d| d.adoption.as_mut())
            .context("adoption missing")?;
        ensure!(
            op.target_receipt.is_some() && op.pointer.as_ref().is_none_or(|p| p == &pointer),
            "adoption pointer receipt changed"
        );
        op.pointer = Some(pointer);
        self.commit(next)
    }
    pub(crate) fn project_adoption(&mut self, id: &str) -> Result<()> {
        let mut next = self.journal.clone();
        let d = next
            .intents
            .get_mut(id)
            .and_then(|i| i.draft.as_mut())
            .context("proposal missing")?;
        let op = d.adoption.as_mut().context("adoption missing")?;
        if op.projected {
            return Ok(());
        }
        ensure!(
            op.target_receipt.is_some(),
            "adoption destination not committed"
        );
        d.record = op.adopted_record.clone();
        d.projections.push(super::drafts::Projection {
            write: op.adopted_write.clone(),
            receipt: None,
        });
        op.projected = true;
        self.commit(next)
    }
}
