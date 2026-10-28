//! Tier 1 commands for T3 protocol v2. Intents are immutable after publication;
//! retry sends the same command, message and target identities, never a new turn.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

mod journal;
mod state;
pub use journal::{Journal, Record};
pub use state::{valid_answers, Request, State};

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
            mode,
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
                message_id,
                mode,
            } => {
                !text.trim().is_empty()
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
            } => {
                !request_id.is_empty()
                    && match (decision, answers) {
                        (Some(d), None) => matches!(
                            d.as_str(),
                            "accept" | "acceptForSession" | "acceptAlways" | "decline" | "cancel"
                        ),
                        (None, Some(a)) => !a.is_empty(),
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
        match self.intent {
            Intent::Message { .. } => {
                wire["attachments"] = json!([]);
                wire["createdBy"] = json!("user");
                wire["creationSource"] = json!("web");
            }
            Intent::Interrupt { .. } => wire["holdQueue"] = json!(true),
            _ => {}
        }
        Ok(wire)
    }
}
