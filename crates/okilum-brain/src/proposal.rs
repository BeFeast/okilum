//! Canonical unverified proposals. No provider transport, retry or adoption API.
pub use crate::proposals::{
    Attempt, AttemptState, CommittedTrigger, FrozenInput, Identity, TriggerKind,
};
use crate::{inbox, retrieval::Citation};
use anyhow::{ensure, Context, Result};
use okilum_core::source::SourceSnapshot;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "okilum-proposal/v1";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationIssue {
    SourceChanged,
    InputUnavailable,
}
impl std::fmt::Display for GenerationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GenerationIssue {}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueuedGeneration {
    pub proposal_id: String,
    pub trigger: CommittedTrigger,
    pub attempt: Attempt,
    pub issue: Option<GenerationIssue>,
}
/// Exact non-secret dispatch input, retained before network access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationInput {
    pub schema: String,
    pub settings: Option<crate::application::ChatSettings>,
    pub goal_brief: Option<serde_json::Value>,
    pub goal_source: Option<SourceSnapshot>,
    pub request_body: String,
}
impl GenerationInput {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == "okilum-proposal-prompt/v1" && self.request_body.len() <= 64 * 1024,
            "invalid bounded frozen prompt"
        );
        let body: serde_json::Value = serde_json::from_str(&self.request_body)?;
        ensure!(
            body["stream"] == true && body["messages"].as_array().is_some_and(|v| !v.is_empty()),
            "invalid frozen model messages"
        );
        if let Some(settings) = &self.settings {
            ensure!(
                !settings.base_url.trim().is_empty()
                    && !settings.base_url.chars().any(char::is_control)
                    && !settings.model.trim().is_empty()
                    && !settings.model.chars().any(char::is_control),
                "invalid frozen provider identity"
            );
            ensure!(
                settings.base_url.len() <= 1024
                    && settings.model.len() <= 256
                    && settings.api_key_env.len() <= 4096
                    && body["model"] == settings.model,
                "frozen settings mismatch"
            );
        }
        Ok(())
    }
    pub(crate) fn validate_captured(
        &self,
        trigger: &CommittedTrigger,
        captured: &CapturedInput,
    ) -> Result<()> {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let decode = |source: &SourceSnapshot| -> Result<String> {
            let bytes = base64::engine::general_purpose::STANDARD.decode(&source.content_base64)?;
            ensure!(
                source.revision == format!("sha256:{:x}", Sha256::digest(&bytes)),
                "frozen source digest mismatch"
            );
            Ok(String::from_utf8(bytes)?)
        };
        let trigger_text = decode(&captured.trigger_source)?;
        ensure!(
            captured.trigger_source.brain_id == trigger.identity.brain_id
                && captured.trigger_source.path == trigger.source_path
                && captured.trigger_source.revision == trigger.identity.source_revision,
            "frozen trigger identity mismatch"
        );
        let goal_text = self.goal_source.as_ref().map(decode).transpose()?;
        match (&trigger.goal_id, &self.goal_source, &self.goal_brief) {
            (None, None, None) => ensure!(
                captured.goal_revision.is_none(),
                "Inbox goal input mismatch"
            ),
            (Some(goal), Some(source), Some(brief)) => {
                let directory = std::path::Path::new(&trigger.source_path)
                    .parent()
                    .context("trigger directory missing")?;
                ensure!(
                    source.brain_id == trigger.identity.brain_id
                        && source.path
                            == directory.join(format!("goal-{goal}.md")).to_string_lossy()
                        && Some(&source.revision) == captured.goal_revision.as_ref()
                        && brief["goal_revision"] == source.revision
                        && brief["goal_id"] == *goal,
                    "frozen goal identity/revision mismatch"
                );
            }
            _ => anyhow::bail!("frozen goal input missing"),
        }
        let body: serde_json::Value = serde_json::from_str(&self.request_body)?;
        let payload: serde_json::Value = serde_json::from_str(
            body["messages"][1]["content"]
                .as_str()
                .context("frozen user input missing")?,
        )?;
        ensure!(
            payload["trigger"] == serde_json::to_value(trigger)?
                && payload["trigger_text"] == trigger_text
                && payload["citations"] == serde_json::to_value(&captured.citations)?
                && payload["omissions"] == serde_json::to_value(&captured.omissions)?
                && payload["goal_text"] == serde_json::to_value(goal_text)?
                && payload["goal_brief"] == serde_json::to_value(&self.goal_brief)?,
            "frozen messages differ from retained provenance"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedInput {
    pub trigger_source: SourceSnapshot,
    pub citations: Vec<Citation>,
    pub goal_revision: Option<String>,
    pub omissions: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generated {
    pub title: String,
    pub criteria: Vec<String>,
    pub rationale: String,
    pub open_questions: Vec<String>,
    pub citation_ids: Vec<String>,
}
impl Generated {
    pub(crate) fn validate(&self, input: &CapturedInput) -> Result<()> {
        ensure!(
            serde_json::to_vec(self)?.len() <= 32 * 1024,
            "proposal output exceeds bound"
        );
        ensure!(
            !self.title.trim().is_empty() && self.title.len() <= 512,
            "invalid proposed title"
        );
        ensure!(
            !self.criteria.is_empty()
                && self.criteria.len() <= 50
                && self
                    .criteria
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= 4096),
            "invalid proposed criteria"
        );
        ensure!(
            !self.rationale.trim().is_empty()
                && self.rationale.len() <= 8192
                && self.open_questions.len() <= 50
                && self.open_questions.iter().all(|s| s.len() <= 4096),
            "invalid proposed rationale/questions"
        );
        let ids: std::collections::BTreeSet<_> = self.citation_ids.iter().collect();
        ensure!(
            ids.len() == self.citation_ids.len()
                && ids
                    .iter()
                    .all(|id| input.citations.iter().any(|c| &c.citation_id == *id)),
            "proposal references uncaptured citation"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    MalformedOutput,
    ProviderUnavailable,
    ProviderFailed,
    ProviderTimeout,
    SourceChanged,
    Interrupted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxGoalTarget {
    pub goal_id: String,
    pub path: String,
    pub revision: String,
    pub child_operation_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextTarget {
    pub goal_id: String,
    pub packet_id: String,
    pub path: String,
    pub revision: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Disposition {
    AdoptedInboxGoal { target: InboxGoalTarget },
    Unreviewed,
    Rejected,
    Adopted { target: ContextTarget },
    Snoozed { until: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub operation_id: String,
    pub expected_revision: String,
    pub actor: String,
    pub at: String,
    pub disposition: Disposition,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: String,
    pub record_type: String,
    pub id: String,
    pub brain_id: String,
    pub goal_id: Option<String>,
    pub trigger: CommittedTrigger,
    pub attempt: Attempt,
    pub captured: CapturedInput,
    pub created_at: String,
    pub generation_actor: String,
    pub generated_at: Option<String>,
    pub verification: String,
    pub generated: Option<Generated>,
    pub failure: Option<Failure>,
    pub disposition: Disposition,
    pub history: Vec<History>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempt_history: Vec<ArchivedAttempt>,
}
impl Record {
    pub(crate) fn bytes(&self) -> Result<Vec<u8>> {
        let bytes = self.serialized_bytes()?;
        publication_size(
            bytes.len(),
            1024 * 1024 - 1024,
            "canonical proposal exceeds reader byte bound including recovery reserve",
        )?;
        Ok(bytes)
    }
    // Admission may inspect an oversized hypothetical candidate without writing it.
    pub(crate) fn serialized_bytes(&self) -> Result<Vec<u8>> {
        let body = match &self.generated {
            Some(g) => format!(
                "# {}\n\nUnverified AI proposal.\n\n{}\n\n## Proposed criteria\n\n{}\n",
                g.title,
                g.rationale,
                g.criteria
                    .iter()
                    .map(|s| format!("- {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            None => "# Pending AI proposal\n\nNo validated generated result is available.\n".into(),
        };
        let bytes = format!("---\n{}---\n{}", serde_yaml::to_string(self)?, body).into_bytes();
        Ok(bytes)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub proposal_id: String,
    pub goal_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    pub goal_id: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Detail {
    pub record: Record,
    pub source: SourceSnapshot,
    pub current_revision: Option<String>,
    pub stale_reasons: Vec<String>,
    pub projection_pending: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub generation: Vec<QueuedGeneration>,
    pub items: Vec<Detail>,
    pub next_cursor: Option<String>,
    pub backlog: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation_id: String,
    pub proposal_id: String,
    pub goal_id: Option<String>,
    pub expected_revision: String,
    pub disposition: Disposition,
    pub source: inbox::SourceIdentity,
}
impl Request {
    pub(crate) fn validate(&self, actor: &str) -> Result<()> {
        inbox::canonical_id(&self.operation_id)?;
        self.source.validate_native(actor)?;
        ensure!(
            self.proposal_id.len() == 64 && self.proposal_id.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid proposal ID"
        );
        ensure!(
            self.expected_revision.starts_with("sha256:") && self.expected_revision.len() == 71,
            "invalid proposal revision"
        );
        if let Some(id) = &self.goal_id {
            inbox::canonical_id(id)?;
        }
        match &self.disposition {
            Disposition::Rejected => (),
            Disposition::Snoozed { until } => {
                utc(until)?;
            }
            _ => anyhow::bail!("only reject and snooze are supported"),
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    pub operation_id: String,
    pub proposal_id: String,
    pub path: String,
    pub previous_revision: String,
    pub revision: String,
    pub actor: String,
    pub at: String,
    pub disposition: Disposition,
    pub replayed: bool,
}
pub(crate) fn utc(s: &str) -> Result<time::OffsetDateTime> {
    let value = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)?;
    ensure!(
        value.offset() == time::UtcOffset::UTC,
        "proposal time must be UTC"
    );
    Ok(value)
}

/// Durable proof that the exact disposition request can never apply.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TerminalReceipt {
    pub schema: String,
    pub outcome: String,
    pub workspace: serde_json::Value,
    pub request: Request,
    pub path: String,
    pub reason: TerminalReason,
    pub at: String,
    pub replayed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalReason {
    DeadlineElapsed,
}
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum DeliveryOutcome {
    Committed(Box<Receipt>),
    NotApplied(Box<TerminalReceipt>),
}

/// Original attempt preserved when an explicit Retry is accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedAttempt {
    pub attempt: Attempt,
    pub failure: Option<Failure>,
    pub archived_at: String,
    pub retry_operation_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryRequest {
    pub operation_id: String,
    pub proposal_id: String,
    pub goal_id: Option<String>,
    pub expected_revision: String,
    pub source: inbox::SourceIdentity,
}
impl RetryRequest {
    pub(crate) fn validate(&self, actor: &str) -> Result<()> {
        // The operation/owner/revision/actor binding is identical to dispositions.
        Request {
            operation_id: self.operation_id.clone(),
            proposal_id: self.proposal_id.clone(),
            goal_id: self.goal_id.clone(),
            expected_revision: self.expected_revision.clone(),
            source: self.source.clone(),
            disposition: Disposition::Rejected,
        }
        .validate(actor)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetryOutcome {
    Committed {
        previous_attempt_id: String,
        attempt_id: String,
        previous_revision: String,
        revision: String,
    },
    NotApplied {
        reason: RetryRefusal,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryRefusal {
    RevisionChanged,
    DispositionChanged,
    AttemptIneligible,
    InputChanged,
    CapacityExceeded,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryReceipt {
    pub schema: String,
    pub workspace: serde_json::Value,
    pub request: RetryRequest,
    pub path: String,
    pub at: String,
    pub replayed: bool,
    pub result: RetryOutcome,
}

/// Complete reviewed form bound to the original proposal and destination owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "destination", rename_all = "snake_case")]
pub enum AdoptRequest {
    Context(Box<crate::proposals::AdoptionRequest>),
    Inbox(Box<crate::proposals::InboxAdoptionRequest>),
}
impl AdoptRequest {
    pub fn validate(&self, actor: &str) -> Result<()> {
        match self {
            Self::Context(r) => r.validate(actor),
            Self::Inbox(r) => r.validate(actor),
        }
    }
    pub fn operation_id(&self) -> &str {
        match self {
            Self::Context(r) => &r.operation_id,
            Self::Inbox(r) => &r.operation_id,
        }
    }
    pub fn proposal_id(&self) -> &str {
        match self {
            Self::Context(r) => &r.proposal_id,
            Self::Inbox(r) => &r.proposal_id,
        }
    }
    pub fn goal_id(&self) -> Option<&str> {
        match self {
            Self::Context(r) => Some(&r.goal_id),
            Self::Inbox(_) => None,
        }
    }
    pub fn source(&self) -> &crate::inbox::SourceIdentity {
        match self {
            Self::Context(r) => &r.source,
            Self::Inbox(r) => &r.source,
        }
    }
    pub fn expected_revision(&self) -> &str {
        match self {
            Self::Context(r) => &r.expected_revision,
            Self::Inbox(r) => &r.expected_revision,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdoptRefusal {
    RevisionChanged,
    DispositionChanged,
    AttemptIneligible,
    InputChanged,
    DestinationChanged,
    InvalidForm,
    CapacityExceeded,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdoptOutcome {
    CommittedContext {
        receipt: Box<crate::proposals::AdoptionReceipt>,
    },
    CommittedInbox {
        receipt: Box<crate::proposals::InboxAdoptionReceipt>,
    },
    NotApplied {
        reason: AdoptRefusal,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptReceipt {
    pub schema: String,
    pub workspace: serde_json::Value,
    pub request: AdoptRequest,
    pub at: String,
    pub replayed: bool,
    pub result: AdoptOutcome,
}

#[derive(Debug)]
pub(crate) struct PublicationCapacity(&'static str);
impl std::fmt::Display for PublicationCapacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for PublicationCapacity {}
pub(crate) fn publication_size(size: usize, limit: usize, message: &'static str) -> Result<()> {
    if size > limit {
        return Err(PublicationCapacity(message).into());
    }
    Ok(())
}
