//! The parts of T3's MCP results Okilum uses. Field names follow T3's
//! contracts (`packages/contracts/src/orchestratorMcp.ts`); fields Okilum
//! does not read are ignored, so a newer server stays readable.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub default_model_selection: Option<ModelSelection>,
}

/// T3's canonical model selection: a provider instance, a model of that
/// instance, and option selections keyed by the catalog's descriptor ids.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub instance_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ModelOption>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub thread_id: String,
    pub title: String,
    pub status: String,
    pub latest_run_id: Option<String>,
    pub provider_instance_id: String,
    pub model: String,
    pub runtime_mode: String,
    pub linked_pull_request: Option<Value>,
    #[serde(default)]
    pub settled: bool,
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub item_count: u64,
    pub updated_at: String,
}

impl ThreadSummary {
    pub fn pull_request_url(&self) -> Option<&str> {
        pull_request_url(self.linked_pull_request.as_ref())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDetail {
    pub thread_id: String,
    pub project_id: String,
    pub title: String,
    pub status: String,
    pub active_run_id: Option<String>,
    pub provider_instance_id: String,
    pub model: String,
    pub runtime_mode: String,
    pub linked_pull_request: Option<Value>,
    #[serde(default)]
    pub item_count: u64,
    #[serde(default)]
    pub pending_request_count: u64,
    pub updated_at: String,
}

impl ThreadDetail {
    pub fn pull_request_url(&self) -> Option<&str> {
        pull_request_url(self.linked_pull_request.as_ref())
    }
}

fn pull_request_url(linked: Option<&Value>) -> Option<&str> {
    linked?.get("url")?.as_str()
}

/// One timeline entry. An assistant message that is still streaming keeps
/// its `position` and grows; re-read it by `item_id` until `status` settles.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineItem {
    pub position: u64,
    pub item_id: String,
    pub run_id: Option<String>,
    pub message_id: Option<String>,
    pub created_by: Option<Value>,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub title: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub text_truncated: bool,
    pub next_text_offset: Option<u64>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPage {
    pub thread: ThreadDetail,
    #[serde(default)]
    pub items: Vec<TimelineItem>,
    /// `None` when the page is empty: keep the last cursor you had.
    pub next_position: Option<u64>,
    #[serde(default)]
    pub has_more: bool,
}

/// Arguments of one `t3_thread_read` call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadQuery {
    pub thread_id: String,
    pub after_position: Option<u64>,
    pub item_id: Option<String>,
    pub text_offset: Option<u64>,
    pub limit: Option<u32>,
    pub max_chars_per_item: Option<u32>,
}

impl ThreadQuery {
    pub fn messages(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            ..Self::default()
        }
    }

    pub fn after(mut self, position: u64) -> Self {
        self.after_position = Some(position);
        self
    }

    /// Re-read one item, from `text_offset` when its text was truncated.
    pub fn item(mut self, item_id: impl Into<String>, text_offset: Option<u64>) -> Self {
        self.item_id = Some(item_id.into());
        self.text_offset = text_offset;
        self
    }

    pub(crate) fn arguments(&self) -> Value {
        let mut args = json!({ "threadId": self.thread_id, "view": "messages" });
        let fields = [
            ("afterPosition", self.after_position.map(Value::from)),
            ("itemId", self.item_id.clone().map(Value::from)),
            ("textOffset", self.text_offset.map(Value::from)),
            ("limit", self.limit.map(Value::from)),
            ("maxCharsPerItem", self.max_chars_per_item.map(Value::from)),
        ];
        for (name, value) in fields {
            if let Some(value) = value {
                args[name] = value;
            }
        }
        args
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
    /// Waits behind the manager's active turn; the default for the remote.
    Queue,
    /// Starts at once, or steers a turn that can be steered.
    Auto,
}

impl SendMode {
    fn wire(self) -> &'static str {
        match self {
            SendMode::Queue => "queue",
            SendMode::Auto => "auto",
        }
    }
}

/// A message for a manager thread. The `client_request_id` is minted once,
/// with the message, and kept by the caller: re-sending the same value after
/// a dropped connection is how T3 knows not to deliver it twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub thread_id: String,
    pub text: String,
    pub mode: SendMode,
    pub client_request_id: String,
}

impl OutgoingMessage {
    pub fn queue(thread_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            text: text.into(),
            mode: SendMode::Queue,
            client_request_id: format!("okilum-{}", uuid::Uuid::new_v4()),
        }
    }

    pub(crate) fn arguments(&self) -> Value {
        json!({
            "threadId": self.thread_id,
            "message": self.text,
            "mode": self.mode.wire(),
            "clientRequestId": self.client_request_id,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendReceipt {
    pub thread_id: String,
    pub message_id: String,
    pub run_id: String,
    pub status: String,
    /// `started`, `queued`, `steered` or `restarted`. Acceptance, not completion.
    pub delivery: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadConfiguration {
    pub thread_id: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: String,
    pub interaction_mode: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// The ceiling of this client's credential.
    pub runtime_mode: String,
    #[serde(default)]
    pub providers: Vec<ProviderCapability>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapability {
    pub provider_instance_id: String,
    pub driver_kind: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub runtime_modes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retried_message_keeps_its_request_id() {
        let message = OutgoingMessage::queue("mcp:manager", "status?");
        let other = OutgoingMessage::queue("mcp:manager", "status?");
        assert_ne!(message.client_request_id, other.client_request_id);
        assert_eq!(message.arguments(), message.clone().arguments());
        assert_eq!(message.arguments()["mode"], "queue");
        assert!(message.client_request_id.len() <= 256);
    }

    #[test]
    fn a_query_sends_only_the_fields_it_sets() {
        let args = ThreadQuery::messages("t").after(7).arguments();
        assert_eq!(
            args,
            json!({ "threadId": "t", "view": "messages", "afterPosition": 7 })
        );
        let args = ThreadQuery::messages("t").item("i", Some(8000)).arguments();
        assert_eq!(args["itemId"], "i");
        assert_eq!(args["textOffset"], 8000);
        assert!(args.get("afterPosition").is_none());
    }

    #[test]
    fn model_selection_options_use_the_wire_shape() {
        let selection: ModelSelection = serde_json::from_value(json!({
            "instanceId": "claudeAgent",
            "model": "claude-opus-5-5",
            "options": [{ "id": "effort", "value": "medium" }]
        }))
        .unwrap();
        assert_eq!(selection.options[0].id, "effort");
        let bare = ModelSelection {
            instance_id: "codex".into(),
            model: "gpt-6-astra".into(),
            options: vec![],
        };
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({ "instanceId": "codex", "model": "gpt-6-astra" })
        );
    }
}
