//! Parent transaction ownership remains in A1; the existing planner owns its child.
#![cfg_attr(not(test), allow(dead_code))]
use super::*;
use crate::{inbox_plan::frozen_goal::FrozenGoal, proposal as api};
use base64::{engine::general_purpose::STANDARD, Engine};
use tessera_core::source::{SourceWrite, WriteReceipt};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InboxAdoptionRequest {
    pub operation_id: String,
    pub proposal_id: String,
    pub expected_revision: String,
    pub capture_id: String,
    pub expected_capture_revision: String,
    pub title: String,
    pub criteria: Vec<crate::Criterion>,
    pub source: crate::inbox::SourceIdentity,
}
impl InboxAdoptionRequest {
    pub fn validate(&self, actor: &str) -> Result<()> {
        ensure!(
            valid_digest(&self.proposal_id)
                && self
                    .expected_revision
                    .strip_prefix("sha256:")
                    .is_some_and(valid_digest),
            "invalid Inbox proposal identity"
        );
        self.child(&self.operation_id).validate(actor)?;
        ensure!(
            serde_json::to_vec(self)?.len() <= 256 * 1024,
            "Inbox adoption request exceeds bound"
        );
        Ok(())
    }
    pub fn child(&self, id: &str) -> crate::inbox_plan::Request {
        crate::inbox_plan::Request {
            operation_id: id.into(),
            capture_id: self.capture_id.clone(),
            expected_capture_revision: self.expected_capture_revision.clone(),
            title: self.title.clone(),
            criteria: self.criteria.clone(),
            source: self.source.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ParentBinding {
    pub brain_id: String,
    pub proposal_id: String,
    pub operation_id: String,
    pub expected_revision: String,
    pub child_operation_id: String,
    pub child_request_sha256: String,
}
/// Construction is private to the Store. The child rechecks against the owned Store.
pub(crate) struct Delegation {
    binding: ParentBinding,
    target: FrozenGoal,
}
impl Delegation {
    pub fn binding(&self) -> &ParentBinding {
        &self.binding
    }
    pub fn target(&self) -> &FrozenGoal {
        &self.target
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChildReceipt {
    pub outcome: crate::inbox_plan::Outcome,
    pub source: WriteReceipt,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxAdoptionReceipt {
    pub operation_id: String,
    pub proposal_id: String,
    pub target: api::InboxGoalTarget,
    pub child: ChildReceipt,
    pub projection: WriteReceipt,
    pub actor: String,
    pub at: String,
    pub replayed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    pub request: InboxAdoptionRequest,
    pub binding: ParentBinding,
    pub records_dir: String,
    pub at: String,
    pub frozen_target_base64: String,
    pub child_receipt: Option<ChildReceipt>,
    pub adopted_write: SourceWrite,
    pub adopted_record: api::Record,
    pub projected: bool,
}
impl Operation {
    pub fn target(&self) -> Result<FrozenGoal> {
        FrozenGoal::restore(
            &STANDARD.decode(&self.frozen_target_base64)?,
            &self.binding.brain_id,
            &self.records_dir,
        )
    }
    pub fn child_operation_id(&self) -> &str {
        &self.binding.child_operation_id
    }
    pub fn reserves(&self, id: &str, external: &str) -> bool {
        self.request.operation_id == id
            || self.child_operation_id() == id
            || self.request.source.external_key().ok().as_deref() == Some(external)
    }
    pub fn target_ref(&self) -> Result<api::InboxGoalTarget> {
        target_ref(&self.target()?)
    }
    pub fn validate(&self, d: &super::drafts::Draft) -> Result<()> {
        self.request.validate(&self.request.source.actor_id)?;
        api::utc(&self.at)?;
        let t = self.target()?;
        let req = &self.request;
        let w = &self.adopted_write;
        ensure!(
            self.binding == parent_binding(&d.record.brain_id, req, t.request())?
                && t.request() == &req.child(self.child_operation_id())
                && req.operation_id != self.child_operation_id(),
            "Inbox delegation/request binding mismatch"
        );
        ensure!(
            d.record.goal_id.is_none()
                && d.record.trigger.identity.kind == TriggerKind::Inbox
                && d.record.trigger.identity.record_id == req.capture_id
                && d.record.trigger.identity.source_revision == req.expected_capture_revision
                && d.record.captured.trigger_source == t.outcome().origin.source_snapshot,
            "Inbox adoption captured owner mismatch"
        );
        let prior = d
            .projections
            .iter()
            .find(|p| {
                super::drafts::revision(&p.write).ok().as_ref() == Some(&req.expected_revision)
            })
            .context("Inbox adoption prior projection absent")?;
        let bytes = STANDARD.decode(&prior.write.content_base64)?;
        let text = std::str::from_utf8(&bytes)?
            .strip_prefix("---\n")
            .context("proposal metadata absent")?;
        let (yaml, _) = text
            .split_once("\n---\n")
            .context("proposal metadata end absent")?;
        let mut expected: api::Record = serde_yaml::from_str(yaml)?;
        ensure!(
            expected.generated.is_some()
                && matches!(
                    expected.disposition,
                    api::Disposition::Unreviewed | api::Disposition::Snoozed { .. }
                ),
            "ineligible Inbox proposal"
        );
        expected.disposition = api::Disposition::AdoptedInboxGoal {
            target: self.target_ref()?,
        };
        expected.history.push(api::History {
            operation_id: req.operation_id.clone(),
            expected_revision: req.expected_revision.clone(),
            actor: req.source.actor_id.clone(),
            at: self.at.clone(),
            disposition: expected.disposition.clone(),
        });
        canonical_uuid(&w.operation_id)?;
        ensure!(
            expected == self.adopted_record
                && w.schema == crate::SCHEMA
                && w.brain_id == d.record.brain_id
                && w.path == d.projections[0].write.path
                && w.expected_revision.as_ref() == Some(&req.expected_revision)
                && STANDARD.decode(&w.content_base64)? == expected.bytes()?,
            "Inbox adoption canonical mismatch"
        );
        if let Some(receipt) = &self.child_receipt {
            validate_child(&t, receipt)?;
        }
        if self.projected {
            ensure!(
                self.child_receipt.is_some()
                    && d.record == expected
                    && d.projections.last().is_some_and(|p| p.write == *w),
                "Inbox projection precedes exact child receipt"
            );
        } else {
            ensure!(
                d.latest_revision()? == req.expected_revision,
                "Inbox adoption base changed"
            );
        }
        Ok(())
    }
}
fn parent_binding(
    brain: &str,
    request: &InboxAdoptionRequest,
    child: &crate::inbox_plan::Request,
) -> Result<ParentBinding> {
    Ok(ParentBinding {
        brain_id: brain.into(),
        proposal_id: request.proposal_id.clone(),
        operation_id: request.operation_id.clone(),
        expected_revision: request.expected_revision.clone(),
        child_operation_id: child.operation_id.clone(),
        child_request_sha256: child.digest(brain)?,
    })
}
fn target_ref(t: &FrozenGoal) -> Result<api::InboxGoalTarget> {
    Ok(api::InboxGoalTarget {
        goal_id: t.outcome().goal_id.clone(),
        path: t.write().path.clone(),
        revision: t.revision().into(),
        child_operation_id: t.request().operation_id.clone(),
    })
}
fn validate_child(t: &FrozenGoal, r: &ChildReceipt) -> Result<()> {
    let mut expected = t.outcome().clone();
    expected.receipt.status = "committed".into();
    ensure!(
        r.outcome == expected
            && r.source.operation_id == t.write().operation_id
            && r.source.path == t.write().path
            && r.source.previous_revision.is_none()
            && r.source.revision == t.revision(),
        "original child receipt mismatch"
    );
    Ok(())
}
impl Store {
    pub(crate) fn enable_inbox_adoption(&mut self) -> Result<()> {
        ensure!(
            self.journal.adoption_enabled,
            "Inbox adoption requires context adoption"
        );
        if self.journal.inbox_adoption_enabled {
            return Ok(());
        }
        let mut n = self.journal.clone();
        n.inbox_adoption_enabled = true;
        self.commit(n)
    }
    pub(crate) fn inbox_adoption_enabled(&self) -> bool {
        self.journal.inbox_adoption_enabled
    }
    pub(crate) fn inbox_adoption_ids(&self) -> Result<Vec<String>> {
        self.healthy()?;
        Ok(self
            .journal
            .intents
            .iter()
            .filter(|(_, i)| i.draft.as_ref().is_some_and(|d| d.inbox_adoption.is_some()))
            .map(|(id, _)| id.clone())
            .collect())
    }
    pub(crate) fn inbox_adoption(&self, id: &str) -> Result<&Operation> {
        self.healthy()?;
        self.journal
            .intents
            .get(id)
            .and_then(|i| i.draft.as_ref())
            .and_then(|d| d.inbox_adoption.as_ref())
            .context("Inbox adoption absent")
    }
    pub(crate) fn delegation(&self, id: &str) -> Result<Delegation> {
        let op = self.inbox_adoption(id)?;
        Ok(Delegation {
            binding: op.binding.clone(),
            target: op.target()?,
        })
    }
    pub(crate) fn validate_delegation(&self, witness: &Delegation) -> Result<()> {
        let op = self.inbox_adoption(&witness.binding.proposal_id)?;
        ensure!(
            op.binding == witness.binding && op.target()? == witness.target,
            "delegation no longer matches retained parent"
        );
        Ok(())
    }
    pub(crate) fn inbox_adoption_replay(
        &self,
        request: &InboxAdoptionRequest,
    ) -> Result<Option<Option<InboxAdoptionReceipt>>> {
        let external = request.source.external_key()?;
        for id in self.inbox_adoption_ids()? {
            let op = self.inbox_adoption(&id)?;
            if op.reserves(&request.operation_id, &external) {
                ensure!(
                    op.request == *request,
                    "Inbox adoption identity reused with changed request"
                );
                let d = self.draft(&self.journal.brain_id, None, &id)?;
                let p = d
                    .projections
                    .iter()
                    .find(|p| p.write == op.adopted_write)
                    .and_then(|p| p.receipt.as_ref());
                return Ok(Some(match (&op.child_receipt, p) {
                    (Some(child), Some(projection)) => Some(InboxAdoptionReceipt {
                        operation_id: request.operation_id.clone(),
                        proposal_id: id,
                        target: op.target_ref()?,
                        child: child.clone(),
                        projection: projection.clone(),
                        actor: request.source.actor_id.clone(),
                        at: op.at.clone(),
                        replayed: true,
                    }),
                    _ => None,
                }));
            }
        }
        Ok(None)
    }
    pub(crate) fn validate_inbox_adoption_capacity(
        &self,
        request: InboxAdoptionRequest,
        target: FrozenGoal,
        records_dir: String,
        at: String,
    ) -> Result<()> {
        let next = self.prepare_inbox_adoption(request, target, records_dir, at)?;
        self.validate_commit(&next)?;
        Ok(())
    }
    pub(crate) fn stage_inbox_adoption(
        &mut self,
        request: InboxAdoptionRequest,
        target: FrozenGoal,
        records_dir: String,
        at: String,
    ) -> Result<()> {
        ensure!(
            self.journal.inbox_adoption_enabled,
            "adoption capability not enrolled"
        );
        let next = self.prepare_inbox_adoption(request, target, records_dir, at)?;
        self.commit(next)
    }
    fn prepare_inbox_adoption(
        &self,
        request: InboxAdoptionRequest,
        target: FrozenGoal,
        records_dir: String,
        at: String,
    ) -> Result<Journal> {
        request.validate(&request.source.actor_id)?;
        ensure!(
            !self.operation_reserved(&request.operation_id, &request.source.external_key()?)?
                && !self.operation_reserved(
                    &target.request().operation_id,
                    &request.source.external_key()?
                )?,
            "Inbox identity reserved"
        );
        let d = self.draft(&self.journal.brain_id, None, &request.proposal_id)?;
        ensure!(
            d.adoption.is_none()
                && d.inbox_adoption.is_none()
                && !d.pending()
                && d.latest_revision()? == request.expected_revision,
            "proposal adoption revision unavailable"
        );
        let mut record = d.record.clone();
        record.disposition = api::Disposition::AdoptedInboxGoal {
            target: target_ref(&target)?,
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
        let operation = Operation {
            binding: parent_binding(&record.brain_id, &request, target.request())?,
            request: request.clone(),
            records_dir,
            at,
            frozen_target_base64: STANDARD.encode(serde_json::to_vec(&target)?),
            child_receipt: None,
            adopted_write: write,
            adopted_record: record,
            projected: false,
        };
        let mut next = self.journal.clone();
        next.adoption_enabled = true;
        next.inbox_adoption_enabled = true;
        next.intents
            .get_mut(&request.proposal_id)
            .unwrap()
            .draft
            .as_mut()
            .unwrap()
            .inbox_adoption = Some(operation);
        Ok(next)
    }
    pub(crate) fn acknowledge_inbox_child(
        &mut self,
        id: &str,
        receipt: ChildReceipt,
    ) -> Result<()> {
        let mut n = self.journal.clone();
        let op = n
            .intents
            .get_mut(id)
            .and_then(|i| i.draft.as_mut())
            .and_then(|d| d.inbox_adoption.as_mut())
            .context("Inbox adoption absent")?;
        validate_child(&op.target()?, &receipt)?;
        ensure!(
            op.child_receipt.as_ref().is_none_or(|r| r == &receipt),
            "child receipt changed"
        );
        op.child_receipt = Some(receipt);
        self.commit(n)
    }
    pub(crate) fn project_inbox_adoption(&mut self, id: &str) -> Result<()> {
        let mut n = self.journal.clone();
        let d = n
            .intents
            .get_mut(id)
            .and_then(|i| i.draft.as_mut())
            .context("Inbox proposal absent")?;
        let op = d.inbox_adoption.as_mut().context("Inbox adoption absent")?;
        if op.projected {
            return Ok(());
        }
        ensure!(op.child_receipt.is_some(), "child not acknowledged");
        d.record = op.adopted_record.clone();
        d.projections.push(super::drafts::Projection {
            write: op.adopted_write.clone(),
            receipt: None,
        });
        op.projected = true;
        self.commit(n)
    }
}
