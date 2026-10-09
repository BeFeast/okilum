//! Dormant committed-source adapter. Enrollment is fixture-only in A2.
//! Runner state stages immutable metadata/bytes; spool IO never gates core writes.
use super::*;
use crate::proposals::{CommittedTrigger, Enqueue, Identity, Store, TriggerKind};
use okilum_core::source::{RequiredProposalFeed, WriteReceipt};
use std::io::Read;

const MAX_SEGMENT: u64 = 64 * 1024;
const MATERIALIZE_BATCH: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    binding: RequiredProposalFeed,
    published: u64,
    spilled: u64,
    acknowledged: u64,
    candidates: Vec<Candidate>,
    /// Exact serialized bytes survive feature-graph changes and lost responses.
    staged: BTreeMap<u64, String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRef {
    operation_id: String,
    path: String,
    expected_revision: Option<String>,
    revision: String,
}
impl SourceRef {
    fn from_write(write: &SourceWrite) -> Result<Self> {
        Ok(Self {
            operation_id: write.operation_id.clone(),
            path: write.path.clone(),
            expected_revision: write.expected_revision.clone(),
            revision: format!(
                "sha256:{:x}",
                Sha256::digest(STANDARD.decode(&write.content_base64)?)
            ),
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    trigger: CommittedTrigger,
    dependencies: Vec<SourceRef>,
    committed: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    schema: String,
    binding: RequiredProposalFeed,
    sequence: u64,
    trigger: CommittedTrigger,
    /// Canonical source recovery records retain original actor/source/result
    /// bytes. Never reread mutable Markdown or duplicate a whole outcome here.
    sources: Vec<SourceRef>,
}
impl Journal {
    fn validate(&self, brain: &str) -> Result<()> {
        self.binding.validate()?;
        ensure!(
            self.binding.active,
            "feed state must retain active identity"
        );
        ensure!(
            self.acknowledged <= self.spilled && self.spilled <= self.published,
            "feed cursor mismatch"
        );
        ensure!(
            self.published - self.spilled == self.staged.len() as u64,
            "feed staging gap"
        );
        for (offset, (sequence, bytes)) in self.staged.iter().enumerate() {
            ensure!(
                *sequence == self.spilled + offset as u64 + 1,
                "feed staging sequence gap"
            );
            validate_segment(bytes.as_bytes(), &self.binding, *sequence, brain)?;
        }
        for candidate in &self.candidates {
            candidate.trigger.validate(brain)?;
            ensure!(
                candidate.trigger.identity.policy_version == self.binding.policy_version,
                "candidate policy differs from binding"
            );
            let unique: std::collections::BTreeSet<_> = candidate.committed.iter().collect();
            ensure!(
                unique.len() == candidate.committed.len(),
                "duplicate candidate receipt"
            );
            ensure!(
                !candidate.dependencies.is_empty() && candidate.dependencies.len() <= 3,
                "invalid candidate dependencies"
            );
            ensure!(
                candidate.dependencies[0].expected_revision.is_none(),
                "trigger must create new source"
            );
            ensure!(
                candidate
                    .committed
                    .iter()
                    .all(|id| candidate.dependencies.iter().any(|r| &r.operation_id == id)),
                "unknown candidate receipt"
            );
        }
        Ok(())
    }
}
fn validate_segment(
    bytes: &[u8],
    binding: &RequiredProposalFeed,
    sequence: u64,
    brain: &str,
) -> Result<Segment> {
    ensure!(
        bytes.len() as u64 <= MAX_SEGMENT,
        "feed segment exceeds bound"
    );
    let segment: Segment = serde_json::from_slice(bytes)?;
    ensure!(
        segment.schema == "tessera-committed-feed/v1"
            && segment.binding == *binding
            && segment.sequence == sequence,
        "feed segment owner/epoch/sequence mismatch"
    );
    segment.trigger.validate(brain)?;
    ensure!(
        segment.trigger.identity.policy_version == binding.policy_version,
        "feed policy mismatch"
    );
    let primary = segment.sources.first().context("feed source missing")?;
    ensure!(
        segment.sources.len() <= 3
            && primary.expected_revision.is_none()
            && primary.path == segment.trigger.source_path
            && primary.revision == segment.trigger.identity.source_revision,
        "feed source metadata mismatch"
    );
    Ok(segment)
}

impl Runner {
    /// Explicit activation; called after product control intent is durable or by tests.
    pub(super) fn enroll_proposal_feed(&mut self, policy_version: u32) -> Result<()> {
        self.flush_writes()?;
        ensure!(
            !self.state.inbox_recovery_required
                && !self.state.attention_journal.recovery_required
                && !self.state.inbox_plan_journal.recovery_required
                && !self.state.maestro_journal.recovery_required,
            "feed enrollment requires recovered core inventory"
        );
        ensure!(
            self.state.proposal_feed.is_none() && self.source.required_proposal_feed().is_none(),
            "feed already enrolled"
        );
        let binding = RequiredProposalFeed {
            capability: "tessera-proposal-feed/v1".into(),
            epoch: Uuid::new_v4().to_string(),
            activation_id: Uuid::new_v4().to_string(),
            policy_version,
            active: false,
        };
        self.source.require_proposal_feed(binding)?;
        self.suggestions_cut(2)?;
        self.recover_proposal_enrollment()
    }

    pub(super) fn recover_proposal_enrollment(&mut self) -> Result<()> {
        let binding = self.source.required_proposal_feed().cloned();
        match binding {
            None => ensure!(
                self.state.proposal_feed.is_none(),
                "feed state without required binding"
            ),
            Some(mut binding) => {
                let preparing = !binding.active;
                binding.active = true;
                if preparing {
                    ensure!(
                        !self.state.inbox_recovery_required
                            && !self.state.attention_journal.recovery_required
                            && !self.state.inbox_plan_journal.recovery_required
                            && !self.state.maestro_journal.recovery_required,
                        "feed activation requires recovered core inventory"
                    );
                }
                if self.state.proposal_feed.is_none() {
                    ensure!(
                        preparing && self.state.pending_writes.is_empty(),
                        "missing feed state or nonquiet activation"
                    );
                    self.state.proposal_feed = Some(Journal {
                        binding: binding.clone(),
                        published: 0,
                        spilled: 0,
                        acknowledged: 0,
                        candidates: Vec::new(),
                        staged: BTreeMap::new(),
                    });
                    self.persist()?;
                }
                let feed = self.state.proposal_feed.as_ref().unwrap();
                ensure!(
                    feed.binding == binding,
                    "required feed differs from Runner state"
                );
                feed.validate(&self.state.brain_id)?;
                self.validate_feed_inventory()?;
                if preparing {
                    if self
                        .state_dir
                        .join("proposal-intents-v1/journal.json")
                        .try_exists()?
                    {
                        Store::inspect_bound(&self.state_dir, &self.state.brain_id, &binding)?;
                    } else {
                        Store::initialize_bound(
                            &self.state_dir,
                            &self.state.brain_id,
                            0,
                            Some(binding.clone()),
                        )?;
                    }
                    self.source.require_proposal_feed(binding.clone())?;
                }
                let cursor = Store::inspect_bound(&self.state_dir, &self.state.brain_id, &binding)?;
                let feed = self.state.proposal_feed.as_ref().unwrap();
                ensure!(
                    cursor >= feed.acknowledged
                        && cursor <= feed.acknowledged.saturating_add(1)
                        && cursor <= feed.spilled,
                    "consumer/source cursor mismatch"
                );
            }
        }
        Ok(())
    }

    pub(super) fn stage_proposal_candidate(
        &mut self,
        kind: TriggerKind,
        record_id: &str,
        goal_id: Option<String>,
        received_at: &str,
        operations: &[String],
    ) -> Result<()> {
        let Some(feed) = &mut self.state.proposal_feed else {
            return Ok(());
        };
        let dependencies = operations
            .iter()
            .map(|id| {
                let write = self
                    .state
                    .pending_writes
                    .iter()
                    .find(|w| &w.operation_id == id)
                    .context("candidate write not pending")?;
                SourceRef::from_write(write)
            })
            .collect::<Result<Vec<_>>>()?;
        let primary = dependencies.first().context("candidate source missing")?;
        ensure!(
            primary.expected_revision.is_none(),
            "candidate source is not create-only"
        );
        let trigger = CommittedTrigger {
            identity: Identity {
                brain_id: self.state.brain_id.clone(),
                kind,
                record_id: record_id.into(),
                source_revision: primary.revision.clone(),
                policy_version: feed.binding.policy_version,
            },
            goal_id,
            source_path: primary.path.clone(),
            received_at: received_at.into(),
        };
        trigger.validate(&self.state.brain_id)?;
        feed.candidates.push(Candidate {
            trigger,
            dependencies,
            committed: Vec::new(),
        });
        Ok(())
    }

    pub(super) fn finalize_proposal_write(
        &mut self,
        write: &SourceWrite,
        receipt: &WriteReceipt,
    ) -> Result<()> {
        if self.state.pending_writes.len() == 1 {
            if let Some(feed) = &self.state.proposal_feed {
                for candidate in &feed.candidates {
                    self.validate_committed_sources(&candidate.trigger, &candidate.dependencies)?;
                }
            }
        }
        let Some(feed) = &mut self.state.proposal_feed else {
            return Ok(());
        };
        for candidate in &mut feed.candidates {
            if let Some(source) = candidate
                .dependencies
                .iter()
                .find(|r| r.operation_id == write.operation_id)
            {
                ensure!(
                    *source == SourceRef::from_write(write)?
                        && receipt.operation_id == source.operation_id
                        && receipt.path == source.path
                        && receipt.revision == source.revision
                        && receipt.previous_revision == source.expected_revision,
                    "candidate receipt mismatch"
                );
                if !candidate.committed.contains(&source.operation_id) {
                    candidate.committed.push(source.operation_id.clone());
                }
            }
        }
        // Publish only in the final checkpoint of the complete canonical batch.
        if self.state.pending_writes.len() == 1 {
            for candidate in &feed.candidates {
                ensure!(
                    candidate
                        .dependencies
                        .iter()
                        .all(|r| candidate.committed.contains(&r.operation_id)),
                    "candidate batch lacks receipt"
                );
                feed.published = feed
                    .published
                    .checked_add(1)
                    .context("feed sequence exhausted")?;
                let segment = Segment {
                    schema: "tessera-committed-feed/v1".into(),
                    binding: feed.binding.clone(),
                    sequence: feed.published,
                    trigger: candidate.trigger.clone(),
                    sources: candidate.dependencies.clone(),
                };
                let bytes = serde_json::to_string(&segment)?;
                validate_segment(
                    bytes.as_bytes(),
                    &feed.binding,
                    feed.published,
                    &self.state.brain_id,
                )?;
                feed.staged.insert(feed.published, bytes);
            }
            feed.candidates.clear();
        }
        Ok(())
    }

    pub(super) fn proposal_backlog(&self) -> Option<String> {
        self.proposal_feed_issue.clone().or_else(|| {
            self.state
                .proposal_feed
                .as_ref()
                .filter(|f| f.published > f.acknowledged || !f.candidates.is_empty())
                .map(|_| "committed proposal feed has retained backlog".into())
        })
    }

    /// Optional work may fail independently of acknowledged core work. Retain
    /// the immediately previous feed checkpoint and retry on the next pump.
    pub(super) fn pump_proposal_feed(&mut self) {
        if self.state.proposal_feed.is_none() {
            return;
        }
        let result = self
            .materialize_proposal_feed()
            .and_then(|_| self.consume_proposal_feed());
        self.proposal_feed_issue = result.err().map(|error| error.to_string());
    }

    /// Only optional cursors/staging change here. On uncertain persistence keep
    /// the immediately previous checkpoint in memory. Rewriting it is safe:
    /// immutable segments replay exactly, and A1 can be only one event ahead.
    /// Never reload/reroute core State from an optional error path.
    fn checkpoint_feed(&mut self, next: Journal, spill: bool) -> Result<()> {
        let previous = self.state.proposal_feed.replace(next);
        let result = (|| {
            #[cfg(test)]
            if self.proposal_feed_fault == Some(if spill { Fault::Spill } else { Fault::Ack }) {
                if self.proposal_feed_fault_after > 0 {
                    self.proposal_feed_fault_after -= 1;
                } else {
                    self.proposal_feed_fault = None;
                    anyhow::bail!("injected feed checkpoint failure");
                }
            }
            let _ = spill;
            self.persist()?;
            #[cfg(test)]
            if self.proposal_feed_fault == Some(Fault::AfterReplace) {
                self.proposal_feed_fault = None;
                anyhow::bail!("injected feed checkpoint lost response");
            }
            Ok(())
        })();
        if result.is_err() {
            self.state.proposal_feed = previous;
        }
        result
    }
    fn materialize_proposal_feed(&mut self) -> Result<()> {
        let root = self.state_dir.join("proposal-feed-v1");
        fs::create_dir_all(&root)?;
        ensure!(
            !fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "feed directory cannot be symlink"
        );
        File::open(&self.state_dir)?.sync_all()?;
        for _ in 0..MATERIALIZE_BATCH {
            let feed = self.state.proposal_feed.as_ref().unwrap();
            let Some((&sequence, bytes)) = feed.staged.first_key_value() else {
                break;
            };
            let path = root.join(format!("{}-{sequence:020}.json", feed.binding.epoch));
            if path.try_exists()? {
                ensure!(
                    read_segment(&path)? == bytes.as_bytes(),
                    "immutable feed segment differs from staged bytes"
                );
                File::open(&path)?.sync_all()?;
            } else {
                let mut file = tempfile::NamedTempFile::new_in(&root)?;
                file.write_all(bytes.as_bytes())?;
                file.as_file().sync_all()?;
                file.persist_noclobber(&path)?;
            }
            File::open(&root)?.sync_all()?;
            let mut next = self.state.proposal_feed.as_ref().unwrap().clone();
            next.staged.remove(&sequence);
            next.spilled = sequence;
            self.checkpoint_feed(next, true)?;
        }
        Ok(())
    }
    fn validate_committed_sources(
        &self,
        trigger: &CommittedTrigger,
        sources: &[SourceRef],
    ) -> Result<()> {
        let writes = sources
            .iter()
            .map(|source| {
                let recovery = self.source.recovery_record(&source.operation_id)?;
                ensure!(
                    SourceRef::from_write(&recovery.request)? == *source,
                    "feed source request changed"
                );
                let receipt = recovery.receipt.context("feed source not committed")?;
                ensure!(
                    receipt.operation_id == source.operation_id
                        && receipt.path == source.path
                        && receipt.revision == source.revision
                        && receipt.previous_revision == source.expected_revision,
                    "feed source receipt changed"
                );
                Ok(recovery.request)
            })
            .collect::<Result<Vec<_>>>()?;
        self.validate_source_metadata(trigger, &writes)
    }
    fn validate_source_metadata(
        &self,
        trigger: &CommittedTrigger,
        writes: &[SourceWrite],
    ) -> Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        let documents = writes
            .iter()
            .map(|write| {
                ensure!(
                    write.brain_id == self.state.brain_id
                        && write.schema == SCHEMA
                        && ids.insert(&write.operation_id),
                    "feed dependency identity mismatch"
                );
                uuid(&write.operation_id)?;
                let source = SourceRef::from_write(write)?;
                let (mapping, _) = parse_document(&SourceSnapshot {
                    schema: SCHEMA.into(),
                    brain_id: write.brain_id.clone(),
                    path: write.path.clone(),
                    revision: source.revision,
                    content_base64: write.content_base64.clone(),
                    media_type: "text/markdown".into(),
                })?;
                let doc = serde_yaml::Value::Mapping(mapping);
                ensure!(
                    doc["schema"].as_str() == Some(SCHEMA)
                        && doc["brain_id"].as_str() == Some(&self.state.brain_id),
                    "feed canonical source owner mismatch"
                );
                Ok(doc)
            })
            .collect::<Result<Vec<_>>>()?;
        let primary = documents
            .first()
            .context("feed canonical primary missing")?;
        let kind = match trigger.identity.kind {
            TriggerKind::Inbox => "inbox",
            TriggerKind::Decision => "decision",
            TriggerKind::Result => "result",
        };
        ensure!(
            primary["record_type"].as_str() == Some(kind)
                && primary["id"].as_str() == Some(&trigger.identity.record_id)
                && primary["received_at"].as_str() == Some(&trigger.received_at),
            "feed canonical trigger metadata mismatch"
        );
        ensure!(
            writes[0].expected_revision.is_none()
                && writes[0].path == self.path(kind, &trigger.identity.record_id)
                && writes[0].path == trigger.source_path
                && SourceRef::from_write(&writes[0])?.revision == trigger.identity.source_revision,
            "feed canonical source identity mismatch"
        );
        match trigger.identity.kind {
            TriggerKind::Inbox => ensure!(
                writes.len() == 1 && trigger.goal_id.is_none(),
                "inbox feed dependency mismatch"
            ),
            TriggerKind::Decision => ensure!(
                writes.len() == 1 && primary["goal_id"].as_str() == trigger.goal_id.as_deref(),
                "decision feed ownership mismatch"
            ),
            TriggerKind::Result => {
                ensure!(writes.len() == 3, "result requires complete source batch");
                let result: ResultRecord = serde_yaml::from_value(primary.clone())?;
                let stage: Stage = serde_yaml::from_value(documents[1].clone())?;
                let goal: Goal = serde_yaml::from_value(documents[2].clone())?;
                ensure!(
                    Some(result.goal_id.as_str()) == trigger.goal_id.as_deref()
                        && result.stage_id == stage.id
                        && result.goal_id == goal.id
                        && stage.goal_id == goal.id
                        && stage.result_ids.contains(&result.id)
                        && goal.stage_ids.contains(&stage.id),
                    "result dependency graph mismatch"
                );
                ensure!(
                    documents[1]["record_type"].as_str() == Some("stage")
                        && documents[2]["record_type"].as_str() == Some("goal")
                        && writes[1].path == self.path("stage", &stage.id)
                        && writes[2].path == self.path("goal", &goal.id),
                    "result dependency path/type mismatch"
                );
            }
        }
        Ok(())
    }
    fn validate_feed_inventory(&self) -> Result<()> {
        let feed = self.state.proposal_feed.as_ref().unwrap();
        for write in &self.state.pending_writes {
            if write.expected_revision.is_some()
                || !["inbox", "decision", "result"].iter().any(|kind| {
                    write
                        .path
                        .starts_with(&format!("{}/{kind}-", self.state.records_dir))
                        && write.path.ends_with(".md")
                })
            {
                continue;
            }
            let bytes = STANDARD.decode(&write.content_base64)?;
            let snapshot = SourceSnapshot {
                schema: write.schema.clone(),
                brain_id: write.brain_id.clone(),
                path: write.path.clone(),
                revision: format!("sha256:{:x}", Sha256::digest(&bytes)),
                content_base64: write.content_base64.clone(),
                media_type: "text/markdown".into(),
            };
            let (document, _) = parse_document(&snapshot)?;
            if document
                .get(serde_yaml::Value::String("record_type".into()))
                .and_then(|v| v.as_str())
                .is_some_and(|kind| matches!(kind, "inbox" | "decision" | "result"))
            {
                ensure!(
                    feed.candidates
                        .iter()
                        .filter(|c| c
                            .dependencies
                            .first()
                            .is_some_and(|s| s.operation_id == write.operation_id))
                        .count()
                        == 1,
                    "new pending source lost its feed candidate"
                );
            }
        }
        for candidate in &feed.candidates {
            let writes = candidate
                .dependencies
                .iter()
                .map(|source| {
                    if candidate.committed.contains(&source.operation_id) {
                        let recovery = self.source.recovery_record(&source.operation_id)?;
                        let receipt = recovery.receipt.context("candidate receipt missing")?;
                        ensure!(
                            receipt.operation_id == source.operation_id
                                && receipt.path == source.path
                                && receipt.revision == source.revision
                                && receipt.previous_revision == source.expected_revision,
                            "candidate receipt mismatch"
                        );
                        ensure!(
                            SourceRef::from_write(&recovery.request)? == *source,
                            "candidate source changed"
                        );
                        Ok(recovery.request)
                    } else {
                        let write = self
                            .state
                            .pending_writes
                            .iter()
                            .find(|w| w.operation_id == source.operation_id)
                            .context("candidate lost pending write")?;
                        ensure!(
                            SourceRef::from_write(write)? == *source,
                            "candidate pending source changed"
                        );
                        Ok(write.clone())
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            self.validate_source_metadata(&candidate.trigger, &writes)?;
        }
        for (sequence, bytes) in &feed.staged {
            let segment = validate_segment(
                bytes.as_bytes(),
                &feed.binding,
                *sequence,
                &self.state.brain_id,
            )?;
            self.validate_committed_sources(&segment.trigger, &segment.sources)?;
        }
        Ok(())
    }
    fn consume_proposal_feed(&mut self) -> Result<()> {
        let feed = self.state.proposal_feed.as_ref().unwrap();
        if feed.acknowledged == feed.spilled {
            return Ok(());
        }
        let retained = self.proposal_store.is_some();
        let mut consumer = match self.proposal_store.take() {
            Some(store) => store,
            None => Store::open_bound(&self.state_dir, &self.state.brain_id, Some(&feed.binding))?,
        };
        let result = (|| -> Result<()> {
            for _ in 0..MATERIALIZE_BATCH {
                let feed = self.state.proposal_feed.as_ref().unwrap();
                if feed.acknowledged == feed.spilled {
                    break;
                }
                let sequence = feed.acknowledged + 1;
                let path = self
                    .state_dir
                    .join("proposal-feed-v1")
                    .join(format!("{}-{sequence:020}.json", feed.binding.epoch));
                let bytes = read_segment(&path)?;
                let event =
                    validate_segment(&bytes, &feed.binding, sequence, &self.state.brain_id)?;
                self.validate_committed_sources(&event.trigger, &event.sources)?;
                match consumer.enqueue_event(
                    sequence,
                    event.trigger,
                    format!("{:x}", Sha256::digest(&bytes)),
                )? {
                    Enqueue::Accepted { .. } | Enqueue::Replay { .. } => {
                        let mut next = self.state.proposal_feed.as_ref().unwrap().clone();
                        next.acknowledged = sequence;
                        self.checkpoint_feed(next, false)?;
                    }
                    Enqueue::Backlog { .. } => break,
                }
            }
            Ok(())
        })();
        if retained {
            if result.is_err() {
                // The optional enqueue committed atomically or not at all. Reload
                // its exact journal without treating a live attempt as restarted.
                let recovered = consumer.reload_without_recovery();
                self.proposal_store = Some(consumer);
                recovered?;
            } else {
                self.proposal_store = Some(consumer);
            }
        }
        result
    }
}
fn read_segment(path: &std::path::Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_SEGMENT,
        "invalid feed segment file"
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_SEGMENT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_SEGMENT,
        "feed segment exceeds bound"
    );
    Ok(bytes)
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Fault {
    Spill,
    Ack,
    AfterReplace,
}
#[cfg(test)]
mod tests;
