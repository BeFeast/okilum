//! Tier 1 commands for T3 protocol v2. Intents are immutable after publication;
//! retry sends the same command, message and target identities, never a new turn.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

mod prompt;
pub use prompt::{prompt_length_validation, MAX_PROMPT_UNITS};

mod approval_detail;
mod journal;
mod question_attachments;
mod state;
pub use journal::{Journal, Record};
pub use question_attachments::{validate_question_attachments, QuestionAttachments};
pub use state::{
    valid_answers, valid_answers_with_attachments, Request, State, APPROVAL_FALLBACK_OPTIONS,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DispatchMode {
    StartImmediately,
    QueueAfterActive,
    SteerActive {
        #[serde(rename = "targetRunId")]
        target_run_id: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Intent {
    #[serde(rename = "message.dispatch")]
    Message {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::stream::ChatAttachment>,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "dispatchMode")]
        mode: DispatchMode,
    },
    #[serde(rename = "run.interrupt")]
    Interrupt {
        #[serde(rename = "runId")]
        run_id: String,
    },
    #[serde(rename = "runtime-request.respond")]
    Respond {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        answers: Option<BTreeMap<String, Value>>,
        #[serde(
            default,
            rename = "attachmentsByQuestionId",
            skip_serializing_if = "BTreeMap::is_empty"
        )]
        attachments_by_question_id: QuestionAttachments,
    },
    #[serde(rename = "thread.user-input.dismiss")]
    Dismiss {
        #[serde(rename = "requestId")]
        request_id: String,
    },
}
impl Intent {
    pub fn message(text: String, mode: DispatchMode) -> Self {
        Self::Message {
            text,
            attachments: Vec::new(),
            mode,
            message_id: format!("okilum-{}", uuid::Uuid::new_v4()),
        }
    }
    pub fn message_with_attachments(
        text: String,
        mode: DispatchMode,
        attachments: Vec<crate::stream::ChatAttachment>,
    ) -> Self {
        Self::Message {
            text,
            mode,
            attachments,
            message_id: format!("okilum-{}", uuid::Uuid::new_v4()),
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Message {
                mode: DispatchMode::StartImmediately,
                ..
            } => "Start message",
            Self::Message {
                mode: DispatchMode::QueueAfterActive,
                ..
            } => "Queue message",
            Self::Message { .. } => "Steer message",
            Self::Interrupt { .. } => "Interrupt",
            Self::Respond {
                decision: Some(_), ..
            } => "Approval response",
            Self::Respond { .. } => "Question response",
            Self::Dismiss { .. } => "Dismiss question",
        }
    }
    fn validate(&self) -> bool {
        match self {
            Self::Message {
                text,
                attachments,
                message_id,
                mode,
            } => {
                (!text.trim().is_empty() || !attachments.is_empty())
                    && prompt_length_validation(text).is_none()
                    && crate::stream::validate_attachments(attachments).is_ok()
                    && !message_id.is_empty()
                    && match mode {
                        DispatchMode::SteerActive { target_run_id } => !target_run_id.is_empty(),
                        _ => true,
                    }
            }
            Self::Interrupt { run_id } => !run_id.is_empty(),
            Self::Respond {
                request_id,
                decision,
                answers,
                attachments_by_question_id,
            } => {
                !request_id.is_empty()
                    && match (decision, answers) {
                        (Some(d), None) => {
                            attachments_by_question_id.is_empty()
                                && matches!(
                                    d.as_str(),
                                    "accept"
                                        | "acceptForSession"
                                        | "acceptAlways"
                                        | "decline"
                                        | "cancel"
                                )
                        }
                        (None, Some(a)) => {
                            !a.is_empty()
                                && validate_question_attachments(attachments_by_question_id).is_ok()
                                && attachments_by_question_id
                                    .keys()
                                    .all(|id| a.contains_key(id))
                        }
                        _ => false,
                    }
            }
            Self::Dismiss { request_id } => !request_id.is_empty(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Command {
    #[serde(rename = "commandId")]
    pub command_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(flatten)]
    pub intent: Intent,
}
impl Command {
    pub fn new(thread_id: &str, intent: Intent) -> Self {
        Self {
            command_id: format!("okilum-{}", uuid::Uuid::new_v4()),
            thread_id: thread_id.into(),
            intent,
        }
    }
    pub fn wire(&self) -> crate::stream::StreamResult<Value> {
        if self.thread_id.is_empty()
            || !self.intent.validate()
            || self
                .command_id
                .strip_prefix("okilum-")
                .and_then(|x| uuid::Uuid::parse_str(x).ok())
                .is_none()
        {
            return Err(crate::stream::StreamError::Protocol(
                "invalid retained command",
            ));
        }
        let mut wire = serde_json::to_value(self).expect("command serializes");
        match &self.intent {
            Intent::Message { attachments, .. } => {
                wire["attachments"] =
                    serde_json::to_value(attachments).expect("attachments serialize");
                wire["createdBy"] = json!("user");
                wire["creationSource"] = json!("web");
            }
            Intent::Interrupt { .. } => wire["holdQueue"] = json!(true),
            _ => {}
        }
        Ok(wire)
    }
}
