//! A1 identities and journal, consumed by the explicitly enrolled A2 feed.
//! A3 canonical drafts extend this same store; no provider is dispatched here.
//! Source intents, edits, polling and index events are never trigger inputs.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const SCHEMA: &str = "okilum-proposal-intents/v1";
const QUEUE_LIMIT: usize = 32;
const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    #[serde(rename = "inbox_saved")]
    Inbox,
    #[serde(rename = "decision_saved")]
    Decision,
    #[serde(rename = "result_saved")]
    Result,
}
impl TriggerKind {
    fn name(self) -> &'static str {
        match self {
            Self::Inbox => "inbox_saved",
            Self::Decision => "decision_saved",
            Self::Result => "result_saved",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub brain_id: String,
    pub kind: TriggerKind,
    pub record_id: String,
    pub source_revision: String,
    pub policy_version: u32,
}
impl Identity {
    pub fn id(&self) -> Result<String> {
        canonical_uuid(&self.brain_id)?;
        canonical_uuid(&self.record_id)?;
        bounded(&self.source_revision, 512)?;
        ensure!(self.policy_version > 0, "proposal policy must be positive");
        let version = self.policy_version.to_string();
        let fields = [
            &*self.brain_id,
            self.kind.name(),
            &*self.record_id,
            &*self.source_revision,
            &*version,
        ];
        let mut hash = Sha256::new();
        for field in fields {
            // str::len is the UTF-8 byte count, not Unicode scalar count.
            hash.update(field.len().to_string().as_bytes());
            hash.update(b":");
            hash.update(field.as_bytes());
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedTrigger {
    pub identity: Identity,
    pub goal_id: Option<String>,
    pub source_path: String,
    pub received_at: String,
}
impl CommittedTrigger {
    pub(super) fn validate(&self, brain: &str) -> Result<String> {
        ensure!(
            self.identity.brain_id == brain,
            "proposal belongs to another brain"
        );
        let id = self.identity.id()?;
        match (&self.identity.kind, &self.goal_id) {
            (TriggerKind::Inbox, None) => (),
            (TriggerKind::Decision | TriggerKind::Result, Some(goal)) => canonical_uuid(goal)?,
            _ => anyhow::bail!("proposal trigger has incorrect goal ownership"),
        }
        bounded(&self.source_path, 4096)?;
        ensure!(
            !self.source_path.starts_with('/')
                && !self.source_path.contains(['\\', ':'])
                && self
                    .source_path
                    .split('/')
                    .all(|p| !matches!(p, "" | "." | "..")),
            "invalid proposal source path"
        );
        let at = time::OffsetDateTime::parse(
            &self.received_at,
            &time::format_description::well_known::Rfc3339,
        )?;
        ensure!(
            at.offset() == time::UtcOffset::UTC,
            "trigger time must be UTC"
        );
        Ok(id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Queued,
    Running,
    Interrupted,
    Draft,
    Failed,
    Stale,
}

/// Non-secret frozen dispatch references. This slice never resolves credentials,
/// reads sources, constructs a prompt or performs a provider request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenInput {
    pub provider_identity: String,
    pub model: String,
    pub input_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<crate::proposal::GenerationInput>,
}
impl FrozenInput {
    fn validate(&self) -> Result<()> {
        if let Some(g) = &self.generation {
            g.validate()?;
            if let Some(settings) = &g.settings {
                ensure!(
                    settings.base_url == self.provider_identity && settings.model == self.model,
                    "frozen provider identity mismatch"
                );
            }
            ensure!(
                format!("{:x}", Sha256::digest(g.request_body.as_bytes())) == self.input_sha256,
                "frozen provider request digest mismatch"
            );
        }
        bounded(&self.provider_identity, 1024)?;
        bounded(&self.model, 256)?;
        ensure!(
            self.input_sha256.len() == 64
                && self
                    .input_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid frozen input hash"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub id: String,
    pub state: AttemptState,
    pub input: Option<FrozenInput>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    pub trigger: CommittedTrigger,
    pub attempt: Attempt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<drafts::Draft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_issue: Option<crate::proposal::GenerationIssue>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    drafts_enabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    adoption_enabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    inbox_adoption_enabled: bool,
    brain_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    feed_binding: Option<okilum_core::source::RequiredProposalFeed>,
    /// Persisted explicit activation boundary; history at/before it is ineligible.
    initial_cursor: u64,
    cursor: u64,
    /// Retain event bindings even when the same committed record is redelivered.
    events: BTreeMap<u64, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    event_bytes: BTreeMap<u64, String>,
    intents: BTreeMap<String, Intent>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    terminal_dispositions: BTreeMap<String, crate::proposal::TerminalReceipt>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    retries: BTreeMap<String, retry::Operation>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    terminal_adoptions: BTreeMap<String, crate::proposal::AdoptReceipt>,
}
impl Journal {
    fn validate(&self, brain: &str) -> Result<()> {
        ensure!(
            self.schema == SCHEMA && self.brain_id == brain,
            "proposal store schema or owner mismatch"
        );
        if let Some(binding) = &self.feed_binding {
            binding.validate()?;
            ensure!(
                binding.active && self.initial_cursor == 0,
                "invalid feed activation header"
            );
        }
        ensure!(
            self.cursor >= self.initial_cursor,
            "invalid proposal cursor"
        );
        ensure!(
            self.cursor - self.initial_cursor == self.events.len() as u64,
            "proposal cursor has a gap"
        );
        for (offset, (sequence, id)) in self.events.iter().enumerate() {
            ensure!(
                *sequence == self.initial_cursor + 1 + offset as u64,
                "unordered proposal events"
            );
            ensure!(
                self.intents.contains_key(id),
                "proposal event has no durable intent"
            );
        }
        ensure!(
            if self.feed_binding.is_some() {
                self.event_bytes.len() == self.events.len()
                    && self
                        .events
                        .keys()
                        .all(|k| self.event_bytes.get(k).is_some_and(|v| valid_digest(v)))
            } else {
                self.event_bytes.is_empty()
            },
            "proposal exact event byte receipts mismatch"
        );
        ensure!(
            !self.adoption_enabled || self.drafts_enabled,
            "adoption requires drafts capability"
        );
        ensure!(
            !self.inbox_adoption_enabled || self.adoption_enabled,
            "Inbox adoption requires context adoption capability"
        );
        let mut operation_ids = BTreeSet::new();
        let mut external_ids = BTreeSet::new();
        let mut revisions = drafts::ProjectionRevisions::new(self);
        self.validate_terminal_dispositions(&mut operation_ids, &mut external_ids)?;
        self.validate_retries(&mut operation_ids, &mut external_ids, &mut revisions)?;
        self.validate_terminal_adoptions(&mut operation_ids, &mut external_ids)?;
        let mut records = BTreeSet::new();
        let mut attempts = BTreeSet::new();
        for (id, intent) in &self.intents {
            ensure!(
                self.feed_binding
                    .as_ref()
                    .is_none_or(|b| b.policy_version == intent.trigger.identity.policy_version),
                "proposal policy differs from feed header"
            );
            ensure!(
                records.insert((
                    intent.trigger.identity.kind.name(),
                    &intent.trigger.identity.record_id
                )),
                "duplicate committed record with changed policy or revision"
            );
            ensure!(
                attempts.insert(&intent.attempt.id),
                "proposal attempt identity reused"
            );
            ensure!(
                *id == intent.trigger.validate(brain)?,
                "proposal identity differs from retained trigger"
            );
            ensure!(
                self.events.values().any(|event_id| event_id == id),
                "proposal intent has no event receipt"
            );
            canonical_uuid(&intent.attempt.id)?;
            if let Some(draft) = &intent.draft {
                for prior in &draft.record.attempt_history {
                    ensure!(
                        attempts.insert(&prior.attempt.id),
                        "prior attempt identity reused"
                    );
                }
                ensure!(
                    self.drafts_enabled,
                    "draft state without required capability"
                );
                for operation in draft.operations.values() {
                    ensure!(
                        operation_ids.insert(operation.request.operation_id.clone())
                            && external_ids.insert(operation.request.source.external_key()?),
                        "duplicate proposal operation identity"
                    );
                }
                if let Some(operation) = &draft.adoption {
                    ensure!(
                        operation_ids.insert(operation.request.operation_id.clone())
                            && external_ids.insert(operation.request.source.external_key()?),
                        "duplicate adoption operation identity"
                    );
                }
                if let Some(operation) = &draft.inbox_adoption {
                    ensure!(
                        self.inbox_adoption_enabled
                            && operation_ids.insert(operation.request.operation_id.clone())
                            && operation_ids.insert(operation.child_operation_id().to_string())
                            && external_ids.insert(operation.request.source.external_key()?),
                        "invalid or duplicate Inbox adoption identity"
                    );
                }
                draft.validate(id, intent, &mut revisions)?;
                ensure!(
                    draft.adoption.is_none() || self.adoption_enabled,
                    "adoption state without required capability"
                );
            }
            match (intent.attempt.state, &intent.attempt.input) {
                (AttemptState::Queued, None) => ensure!(
                    intent.generation_issue.is_none(),
                    "queued generation has terminal issue"
                ),
                (AttemptState::Failed | AttemptState::Stale, None)
                    if intent.generation_issue.is_some() && intent.draft.is_none() =>
                {
                    ensure!(
                        (intent.attempt.state == AttemptState::Stale)
                            == (intent.generation_issue
                                == Some(crate::proposal::GenerationIssue::SourceChanged)),
                        "preflight issue/state mismatch"
                    );
                }
                (
                    AttemptState::Queued
                    | AttemptState::Running
                    | AttemptState::Interrupted
                    | AttemptState::Draft
                    | AttemptState::Failed
                    | AttemptState::Stale,
                    Some(input),
                ) => input.validate()?,
                _ => anyhow::bail!("proposal attempt input/state mismatch"),
            }
        }
        ensure!(
            self.intents
                .values()
                .filter(|i| i.attempt.state == AttemptState::Queued)
                .count()
                <= QUEUE_LIMIT,
            "proposal queue exceeds capacity"
        );
        ensure!(
            self.intents
                .values()
                .filter(|i| i.attempt.state == AttemptState::Running)
                .count()
                <= 1,
            "multiple running proposal attempts"
        );
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Enqueue {
    Accepted { proposal_id: String },
    Replay { proposal_id: String },
    Backlog { cursor: u64 },
}
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Start {
    pub attempt: Attempt,
    /// Only a fresh durable transition grants a dispatch opportunity. A replay,
    /// including a running one, never grants another provider execution.
    pub newly_running: bool,
}
pub(crate) struct Store {
    root: PathBuf,
    journal: Journal,
    _lock: fs::File,
    poisoned: bool,
    #[cfg(test)]
    fault: Option<Fault>,
}
impl Store {
    /// Explicit future activation seam; never called by production startup.
    /// An existing journal cannot be initialized with another cursor or owner.
    pub fn initialize(operational: &Path, brain: &str, initial_cursor: u64) -> Result<Self> {
        Self::initialize_bound(operational, brain, initial_cursor, None)
    }
    pub(super) fn initialize_bound(
        operational: &Path,
        brain: &str,
        initial_cursor: u64,
        feed_binding: Option<okilum_core::source::RequiredProposalFeed>,
    ) -> Result<Self> {
        canonical_uuid(brain)?;
        let root = operational.join("proposal-intents-v1");
        fs::create_dir_all(&root)?;
        fs::File::open(operational)?.sync_all()?;
        let lock = acquire(&root)?;
        ensure!(
            !root.join("journal.json").try_exists()?,
            "proposal store already initialized; open it instead"
        );
        let journal = Journal {
            schema: SCHEMA.into(),
            drafts_enabled: false,
            adoption_enabled: false,
            inbox_adoption_enabled: false,
            brain_id: brain.into(),
            feed_binding,
            initial_cursor,
            cursor: initial_cursor,
            events: BTreeMap::new(),
            event_bytes: BTreeMap::new(),
            intents: BTreeMap::new(),
            terminal_dispositions: BTreeMap::new(),
            retries: BTreeMap::new(),
            terminal_adoptions: BTreeMap::new(),
        };
        let mut store = Self {
            root,
            journal: journal.clone(),
            _lock: lock,
            poisoned: false,
            #[cfg(test)]
            fault: None,
        };
        store.commit(journal)?;
        Ok(store)
    }
    /// Does not create a missing store. Unsupported/unowned state refuses before
    /// any journal mutation. Running recovery commits once, never queues a retry.
    pub fn open(operational: &Path, brain: &str) -> Result<Self> {
        Self::open_bound(operational, brain, None)
    }
    pub(super) fn open_bound(
        operational: &Path,
        brain: &str,
        binding: Option<&okilum_core::source::RequiredProposalFeed>,
    ) -> Result<Self> {
        Self::open_bound_inner(operational, brain, binding, true)
    }
    /// Retain the store lock while Runner checks original SourceStore receipts.
    pub(crate) fn open_bound_without_recovery(
        operational: &Path,
        brain: &str,
        binding: &okilum_core::source::RequiredProposalFeed,
    ) -> Result<Self> {
        Self::open_bound_inner(operational, brain, Some(binding), false)
    }
    pub(crate) fn reload_without_recovery(&mut self) -> Result<()> {
        let path = self.root.join("journal.json");
        ensure!(
            fs::metadata(&path)?.len() <= MAX_STATE_BYTES,
            "proposal store exceeds bound"
        );
        let journal: Journal = serde_json::from_slice(&fs::read(path)?)?;
        journal.validate(&self.journal.brain_id)?;
        terminal::validate_fence(&self.root, &journal)?;
        retry::validate_fence(&self.root, &journal)?;
        public_adoption::validate_fence(&self.root, &journal)?;
        ensure!(
            journal.feed_binding == self.journal.feed_binding
                && journal.drafts_enabled == self.journal.drafts_enabled
                && journal.adoption_enabled == self.journal.adoption_enabled
                && journal.inbox_adoption_enabled == self.journal.inbox_adoption_enabled,
            "proposal reload capability changed"
        );
        self.journal = journal;
        self.poisoned = false;
        Ok(())
    }
    fn open_bound_inner(
        operational: &Path,
        brain: &str,
        binding: Option<&okilum_core::source::RequiredProposalFeed>,
        recover: bool,
    ) -> Result<Self> {
        canonical_uuid(brain)?;
        let root = operational.join("proposal-intents-v1");
        ensure!(root.is_dir(), "proposal store is not initialized");
        let lock = acquire(&root)?;
        let path = root.join("journal.json");
        ensure!(
            fs::metadata(&path)?.len() <= MAX_STATE_BYTES,
            "proposal store exceeds bound"
        );
        let journal: Journal = serde_json::from_slice(&fs::read(path)?)?;
        journal.validate(brain)?;
        terminal::validate_fence(&root, &journal)?;
        retry::validate_fence(&root, &journal)?;
        public_adoption::validate_fence(&root, &journal)?;
        ensure!(
            journal.feed_binding.as_ref() == binding,
            "proposal feed header mismatch; no relabel allowed"
        );
        let mut store = Self {
            root,
            journal,
            _lock: lock,
            poisoned: false,
            #[cfg(test)]
            fault: None,
        };
        if recover {
            store.recover()?;
        }
        Ok(store)
    }
    /// Header/inventory validation without recovery writes, used before Runner
    /// validates its remaining state or commits anything during startup.
    pub(super) fn inspect_bound(
        operational: &Path,
        brain: &str,
        binding: &okilum_core::source::RequiredProposalFeed,
    ) -> Result<u64> {
        let root = operational.join("proposal-intents-v1");
        let _lock = acquire(&root)?;
        let file = fs::File::open(root.join("journal.json"))?;
        ensure!(
            file.metadata()?.len() <= MAX_STATE_BYTES,
            "proposal store exceeds bound"
        );
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_STATE_BYTES,
            "proposal store exceeds bound"
        );
        let journal: Journal = serde_json::from_slice(&bytes)?;
        journal.validate(brain)?;
        terminal::validate_fence(&root, &journal)?;
        retry::validate_fence(&root, &journal)?;
        public_adoption::validate_fence(&root, &journal)?;
        ensure!(
            journal.feed_binding.as_ref() == Some(binding),
            "proposal feed header mismatch"
        );
        Ok(journal.cursor)
    }
    pub(crate) fn recover(&mut self) -> Result<()> {
        self.healthy()?;
        let mut next = self.journal.clone();
        let mut recovered = false;
        for intent in next.intents.values_mut() {
            if intent.attempt.state == AttemptState::Running {
                intent.attempt.state = AttemptState::Interrupted;
                if let Some(draft) = &mut intent.draft {
                    draft.interrupted(&intent.attempt)?;
                }
                recovered = true;
            }
        }
        if recovered {
            self.commit(next)?;
        }
        Ok(())
    }
    fn healthy(&self) -> Result<()> {
        ensure!(
            !self.poisoned,
            "proposal persistence outcome uncertain; reopen store"
        );
        Ok(())
    }
    pub fn cursor(&self) -> Result<u64> {
        self.healthy()?;
        Ok(self.journal.cursor)
    }
    pub fn get(&self, brain: &str, goal: Option<&str>, id: &str) -> Result<&Intent> {
        self.healthy()?;
        ensure!(
            brain == self.journal.brain_id,
            "proposal belongs to another brain"
        );
        let intent = self.journal.intents.get(id).context("unknown proposal")?;
        ensure!(
            intent.trigger.goal_id.as_deref() == goal,
            "proposal belongs to another goal"
        );
        Ok(intent)
    }
    /// Sequence is from a future authoritative committed-trigger stream, not a
    /// caller-selected current cursor. Gaps and pre-activation replay are errors.
    pub fn enqueue(&mut self, sequence: u64, trigger: CommittedTrigger) -> Result<Enqueue> {
        self.enqueue_inner(sequence, trigger, None)
    }
    pub(super) fn enqueue_event(
        &mut self,
        sequence: u64,
        trigger: CommittedTrigger,
        sha256: String,
    ) -> Result<Enqueue> {
        ensure!(valid_digest(&sha256), "invalid exact event digest");
        self.enqueue_inner(sequence, trigger, Some(sha256))
    }
    fn enqueue_inner(
        &mut self,
        sequence: u64,
        trigger: CommittedTrigger,
        event_sha256: Option<String>,
    ) -> Result<Enqueue> {
        self.healthy()?;
        ensure!(
            self.journal.feed_binding.is_some() == event_sha256.is_some(),
            "proposal event binding required"
        );
        let id = trigger.validate(&self.journal.brain_id)?;
        ensure!(
            sequence > self.journal.initial_cursor,
            "historical trigger precedes activation"
        );
        if sequence <= self.journal.cursor {
            ensure!(
                self.journal.event_bytes.get(&sequence) == event_sha256.as_ref(),
                "event sequence reused with changed exact bytes"
            );
            ensure!(
                self.journal.events.get(&sequence) == Some(&id),
                "event sequence reused with changed identity"
            );
            ensure!(
                self.journal
                    .intents
                    .get(&id)
                    .is_some_and(|i| i.trigger == trigger),
                "event sequence reused with changed source metadata"
            );
            return Ok(Enqueue::Replay { proposal_id: id });
        }
        ensure!(
            self.journal.cursor.checked_add(1) == Some(sequence),
            "proposal event sequence has a gap"
        );
        let mut next = self.journal.clone();
        let replay = if let Some(prior) = next.intents.get(&id) {
            ensure!(
                prior.trigger == trigger,
                "proposal identity reused with changed source metadata"
            );
            true
        } else {
            // A committed record is not regenerated by a policy update or source
            // edit disguised as another event. True new records have a new UUID.
            ensure!(
                !next
                    .intents
                    .values()
                    .any(|i| i.trigger.identity.kind == trigger.identity.kind
                        && i.trigger.identity.record_id == trigger.identity.record_id),
                "committed record cannot be regenerated by policy or revision change"
            );
            if next
                .intents
                .values()
                .filter(|i| i.attempt.state == AttemptState::Queued)
                .count()
                >= QUEUE_LIMIT
            {
                return Ok(Enqueue::Backlog {
                    cursor: self.journal.cursor,
                });
            }
            next.intents.insert(
                id.clone(),
                Intent {
                    trigger,
                    draft: None,
                    generation_issue: None,
                    attempt: Attempt {
                        id: Uuid::new_v4().to_string(),
                        state: AttemptState::Queued,
                        input: None,
                    },
                },
            );
            false
        };
        if let Some(hash) = event_sha256 {
            next.event_bytes.insert(sequence, hash);
        }
        next.events.insert(sequence, id.clone());
        next.cursor = sequence;
        self.commit(next)?;
        Ok(if replay {
            Enqueue::Replay { proposal_id: id }
        } else {
            Enqueue::Accepted { proposal_id: id }
        })
    }
    pub fn mark_running(
        &mut self,
        brain: &str,
        goal: Option<&str>,
        id: &str,
        input: FrozenInput,
    ) -> Result<Start> {
        input.validate()?;
        let current = self.get(brain, goal, id)?;
        if current.attempt.state != AttemptState::Queued {
            ensure!(
                current.attempt.input.as_ref() == Some(&input),
                "attempt replay changed frozen input"
            );
            return Ok(Start {
                attempt: current.attempt.clone(),
                newly_running: false,
            });
        }
        ensure!(
            !self
                .journal
                .intents
                .values()
                .any(|i| i.attempt.state == AttemptState::Running),
            "a proposal attempt is already running"
        );
        let mut next = self.journal.clone();
        let attempt = &mut next
            .intents
            .get_mut(id)
            .context("unknown proposal")?
            .attempt;
        attempt.input = Some(input);
        attempt.state = AttemptState::Running;
        let result = Start {
            attempt: attempt.clone(),
            newly_running: true,
        };
        self.commit(next)?;
        Ok(result)
    }
    pub(crate) fn fail_generation_input(
        &mut self,
        id: &str,
        issue: crate::proposal::GenerationIssue,
    ) -> Result<()> {
        let mut next = self.journal.clone();
        let intent = next.intents.get_mut(id).context("unknown proposal")?;
        ensure!(
            intent.attempt.state == AttemptState::Queued && intent.draft.is_none(),
            "proposal already attempted"
        );
        intent.attempt.state = if issue == crate::proposal::GenerationIssue::SourceChanged {
            AttemptState::Stale
        } else {
            AttemptState::Failed
        };
        intent.generation_issue = Some(issue);
        self.commit(next)
    }

    fn validate_commit(&self, next: &Journal) -> Result<Vec<u8>> {
        self.healthy()?;
        next.validate(&self.journal.brain_id)?;
        retry::validate_fence(&self.root, next)?;
        public_adoption::validate_fence(&self.root, next)?;
        for intent in next.intents.values() {
            if matches!(
                intent.attempt.state,
                AttemptState::Queued | AttemptState::Running
            ) && intent
                .attempt
                .input
                .as_ref()
                .is_some_and(|i| i.generation.is_some())
            {
                if let Some(draft) = &intent.draft {
                    drafts::reserve_generation_canonical(&draft.record)?;
                }
            }
        }
        let bytes = serde_json::to_vec(&next)?;
        // Every running attempt must still fit after restart changes its state
        // to interrupted. Apply this reserve to all commits, including duplicate
        // event receipts admitted while an attempt is running.
        let mut recovery = next.clone();
        for intent in recovery.intents.values_mut() {
            if intent.attempt.state == AttemptState::Running {
                intent.attempt.state = AttemptState::Interrupted;
                if let Some(draft) = &mut intent.draft {
                    draft.interrupted(&intent.attempt)?;
                }
            }
        }
        // Project every reserved adoption in the worst-case completion image.
        // The intent already stores frozen target and adopted bytes. Reserve their
        // duplication into canonical projection plus all future receipt metadata.
        let mut adoption_receipts = 0usize;
        let mut inbox_child_reserve = 0usize;
        for draft in recovery
            .intents
            .values_mut()
            .filter_map(|i| i.draft.as_mut())
        {
            if let Some(operation) = &draft.inbox_adoption {
                if operation.child_receipt.is_none() {
                    inbox_child_reserve = inbox_child_reserve
                        .checked_add(
                            serde_json::to_vec(operation.target()?.outcome())?.len() + 16 * 1024,
                        )
                        .context("child receipt reserve overflow")?;
                }
                if !operation.projected {
                    draft.record = operation.adopted_record.clone();
                    draft.projections.push(drafts::Projection {
                        write: operation.adopted_write.clone(),
                        receipt: None,
                    });
                }
            }
            if let Some(operation) = &draft.adoption {
                if operation.target_receipt.is_none() {
                    adoption_receipts += 1;
                }
                if operation.pointer.is_none() {
                    adoption_receipts += 1;
                }
                if !operation.projected {
                    draft.record = operation.adopted_record.clone();
                    draft.projections.push(drafts::Projection {
                        write: operation.adopted_write.clone(),
                        receipt: None,
                    });
                }
            }
        }
        // Reserve all restart projection bytes plus the bounded source receipt
        // objects before any source mutation can make that recovery mandatory.
        let pending = recovery
            .intents
            .values()
            .filter_map(|i| i.draft.as_ref())
            .map(|d| d.projections.iter().filter(|p| p.receipt.is_none()).count())
            .sum::<usize>();
        // A new canonical projection is <=1MiB before base64; allow its encoded
        // request, bounded Generated record replacement and receipt metadata.
        // This reserve is additional to the concrete interruption image above.
        let generation_reserve = next
            .intents
            .values()
            .filter(|i| {
                matches!(
                    i.attempt.state,
                    AttemptState::Queued | AttemptState::Running
                ) && i
                    .attempt
                    .input
                    .as_ref()
                    .is_some_and(|v| v.generation.is_some())
            })
            .count()
            * (3 * 1024 * 1024);
        let reserve = pending
            .checked_add(adoption_receipts)
            .context("adoption reserve overflow")?
            .checked_mul(16 * 1024)
            .context("proposal receipt reserve overflow")?
            .checked_add(generation_reserve)
            .context("generation completion reserve overflow")?
            .checked_add(inbox_child_reserve)
            .context("Inbox receipt reserve overflow")?;
        crate::proposal::publication_size(
            bytes
                .len()
                .max(serde_json::to_vec(&recovery)?.len())
                .checked_add(reserve)
                .context("proposal capacity overflow")?,
            MAX_STATE_BYTES as usize,
            "proposal store exceeds bound including recovery reserve; cursor retained",
        )?;
        Ok(bytes)
    }

    fn commit(&mut self, next: Journal) -> Result<()> {
        let bytes = self.validate_commit(&next)?;
        let result = (|| -> Result<()> {
            let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            #[cfg(test)]
            if self.fault.take_if(|v| *v == Fault::BeforeReplace).is_some() {
                anyhow::bail!("injected crash before replacement");
            }
            file.persist(self.root.join("journal.json"))?;
            #[cfg(test)]
            if self.fault.take_if(|v| *v == Fault::AfterReplace).is_some() {
                anyhow::bail!("injected crash after replacement");
            }
            fs::File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.poisoned = true;
            return Err(error);
        }
        self.journal = next;
        Ok(())
    }
}
fn acquire(root: &Path) -> Result<fs::File> {
    let file = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("store.lock"))?;
    file.try_lock().context("proposal store already open")?;
    Ok(file)
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn canonical_uuid(value: &str) -> Result<()> {
    ensure!(
        Uuid::parse_str(value)?.to_string() == value,
        "UUID must use canonical spelling"
    );
    Ok(())
}
fn bounded(value: &str, max: usize) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control),
        "invalid bounded proposal field"
    );
    Ok(())
}
#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum Fault {
    BeforeReplace,
    AfterReplace,
}
#[cfg(test)]
mod tests;

pub(crate) mod drafts;
mod public_adoption;
mod retry;
mod terminal;
pub(crate) use drafts::DraftStart;
#[cfg(test)]
impl Store {
    pub(crate) fn inject_commit_failure(&mut self, after: bool) {
        self.fault = Some(if after {
            Fault::AfterReplace
        } else {
            Fault::BeforeReplace
        });
    }
}

mod adoption;
pub(crate) use adoption::PointerOutcome;
pub use adoption::{AdoptionReceipt, AdoptionRequest};

mod inbox_adoption;
pub(crate) use inbox_adoption::{ChildReceipt, Delegation, ParentBinding};
pub use inbox_adoption::{InboxAdoptionReceipt, InboxAdoptionRequest};
