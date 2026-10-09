//! Execution values; source identities are opaque and carry no filesystem authority.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectDraft {
    pub title: String,
    pub status: String,
    pub next_step: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SaveProject {
    pub operation_id: Uuid,
    pub project_id: Uuid,
    /// Zero creates; every update requires the currently observed revision.
    pub expected_revision: u64,
    pub draft: ProjectDraft,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Project {
    pub id: Uuid,
    pub revision: u64,
    pub draft: ProjectDraft,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SaveBrief {
    pub operation_id: Uuid,
    pub project_id: Uuid,
    pub brief_id: Uuid,
    pub expected_revision: u64,
    pub title: String,
    pub text: String,
    /// Opaque configured target. Never a user-supplied path, URL or shell command.
    pub target_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Brief {
    pub id: Uuid,
    pub project_id: Uuid,
    pub revision: u64,
    pub title: String,
    pub text: String,
    pub target_id: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    #[default]
    T3,
    Maestro,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceRecordKind {
    #[default]
    Question,
    Approval,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_question")]
    pub record_kind: SourceRecordKind,
    pub kind: SourceKind,
    pub instance_id: String,
    pub project_id: String,
    pub thread_id: String,
    pub question_id: String,
    /// T3 run/attempt identity or Maestro worker generation.
    pub generation: String,
}
fn is_question(kind: &SourceRecordKind) -> bool {
    *kind == SourceRecordKind::Question
}
/// Exact native approval details shown at consent, never inferred from a title.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub action: String,
    pub target: serde_json::Value,
    pub summary: String,
    pub risk: String,
    pub payload_hash: String,
    pub target_state_hash: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionState {
    Pending,
    Answered,
    Withdrawn,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionField {
    pub id: String,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
    pub allow_text: bool,
    pub multiple: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Question {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Approval>,
    /// Display metadata, never part of source identity or reply revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_title: Option<String>,
    pub id: Uuid,
    pub project_id: Uuid,
    pub source: QuestionSource,
    pub source_revision: String,
    pub state: QuestionState,
    pub fields: Vec<QuestionField>,
    pub can_reply: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnswerField {
    pub id: String,
    pub text: String,
    pub option_ids: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub operation_id: Uuid,
    pub question_id: Uuid,
    pub expected_revision: String,
    pub answers: Vec<AnswerField>,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    Accepted,
    Delivered,
    Rejected,
    Uncertain,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid execution request")]
pub struct InvalidExecution;
fn bounded(value: &str, max: usize, required: bool) -> bool {
    value.len() <= max && (!required || !value.trim().is_empty()) && !value.contains('\0')
}
fn identity(value: &str) -> bool {
    bounded(value, 512, true) && !value.chars().any(char::is_control)
}
impl SaveProject {
    pub fn validate(&self) -> Result<(), InvalidExecution> {
        if self.operation_id.is_nil()
            || self.project_id.is_nil()
            || self.expected_revision >= i64::MAX as u64
            || !bounded(&self.draft.title, 256, true)
            || !bounded(&self.draft.status, 256, false)
            || !bounded(&self.draft.next_step, 4096, false)
        {
            return Err(InvalidExecution);
        }
        Ok(())
    }
}
impl SaveBrief {
    pub fn validate(&self) -> Result<(), InvalidExecution> {
        if self.operation_id.is_nil()
            || self.project_id.is_nil()
            || self.brief_id.is_nil()
            || self.expected_revision >= i64::MAX as u64
            || !bounded(&self.title, 256, true)
            || !bounded(&self.text, 65536, true)
            || !identity(&self.target_id)
        {
            return Err(InvalidExecution);
        }
        Ok(())
    }
}
impl Question {
    pub fn validate(&self) -> Result<(), InvalidExecution> {
        let s = &self.source;
        match (&s.kind, &s.record_kind, &self.approval) {
            (SourceKind::T3, SourceRecordKind::Question, None) if s.worker_id.is_none() => {}
            (SourceKind::Maestro, SourceRecordKind::Question, None)
                if s.worker_id.as_ref().is_some_and(|w| identity(w)) => {}
            (SourceKind::Maestro, SourceRecordKind::Approval, Some(a)) => {
                let target = serde_json::to_vec(&a.target).map_err(|_| InvalidExecution)?;
                if a.repo.as_ref().is_some_and(|r| !bounded(r, 1024, true))
                    || s.worker_id.is_some()
                    || !identity(&a.action)
                    || a.action == "stop_worker"
                    || !a.target.is_object()
                    || target.len() > 16384
                    || !bounded(&a.summary, 16384, true)
                    || !identity(&a.risk)
                    || !identity(&a.payload_hash)
                    || a.target_state_hash.as_ref().is_some_and(|v| !identity(v))
                    || self.fields.len() != 1
                {
                    return Err(InvalidExecution);
                }
                let field = &self.fields[0];
                if field.id != "decision"
                    || field.allow_text
                    || field.multiple
                    || field.options.is_empty()
                    || field.options.iter().any(|o| {
                        !matches!(
                            (o.id.as_str(), o.label.as_str()),
                            ("approve", "Approve") | ("reject", "Reject")
                        )
                    })
                {
                    return Err(InvalidExecution);
                }
            }
            _ => return Err(InvalidExecution),
        }
        if self
            .thread_title
            .as_ref()
            .is_some_and(|title| !bounded(title, 1024, true))
            || self.id.is_nil()
            || self.project_id.is_nil()
            || ![
                &s.instance_id,
                &s.project_id,
                &s.thread_id,
                &s.question_id,
                &s.generation,
                &self.source_revision,
            ]
            .into_iter()
            .all(|s| identity(s))
            || self.fields.is_empty()
            || self.fields.len() > 10
            || (self.state != QuestionState::Pending && self.can_reply)
        {
            return Err(InvalidExecution);
        }
        let mut ids = std::collections::HashSet::new();
        let mut total = 0;
        for field in &self.fields {
            if !identity(&field.id)
                || !ids.insert(&field.id)
                || !bounded(&field.prompt, 32768, true)
                || field.options.len() > 100
                || (!field.allow_text && field.options.is_empty())
            {
                return Err(InvalidExecution);
            }
            total += field.prompt.len();
            let mut options = std::collections::HashSet::new();
            for option in &field.options {
                if !identity(&option.id)
                    || !options.insert(&option.id)
                    || !bounded(&option.label, 4096, true)
                {
                    return Err(InvalidExecution);
                }
                total += option.id.len() + option.label.len();
            }
        }
        if total > 65536 {
            return Err(InvalidExecution);
        }
        Ok(())
    }
    pub fn validate_reply(&self, reply: &Reply) -> Result<(), InvalidExecution> {
        self.validate()?;
        if reply.operation_id.is_nil()
            || reply.question_id != self.id
            || reply.expected_revision != self.source_revision
            || self.state != QuestionState::Pending
            || !self.can_reply
            || reply.answers.len() != self.fields.len()
        {
            return Err(InvalidExecution);
        }
        let mut ids = std::collections::HashSet::new();
        let mut total = 0;
        for answer in &reply.answers {
            let field = self
                .fields
                .iter()
                .find(|f| f.id == answer.id)
                .ok_or(InvalidExecution)?;
            if !ids.insert(&answer.id)
                || !bounded(&answer.text, 16384, false)
                || (!field.allow_text && !answer.text.is_empty())
                || (!field.multiple && answer.option_ids.len() > 1)
                || (answer.text.trim().is_empty() && answer.option_ids.is_empty())
            {
                return Err(InvalidExecution);
            }
            let mut selected = std::collections::HashSet::new();
            for option in &answer.option_ids {
                if !selected.insert(option) || !field.options.iter().any(|o| &o.id == option) {
                    return Err(InvalidExecution);
                }
            }
            total += answer.text.len();
        }
        if total > 65536 {
            return Err(InvalidExecution);
        }
        Ok(())
    }
}
