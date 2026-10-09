//! Exact observed attention identity, attributed decisions and scoped seen state.
use crate::inbox::{canonical_id, hash, Receipt, SourceIdentity};
use anyhow::{ensure, Result};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub goal_id: String,
    pub attention_id: String,
    pub revision: String,
    pub goal_title: String,
    pub goal_status: String,
    pub stage_id: Option<String>,
    pub result_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub allowed_actions: Vec<String>,
    pub current: bool,
    pub seen: bool,
    pub seen_at: Option<String>,
    pub actor_id: String,
    pub channel: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct List {
    pub items: Vec<Item>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub generation: String,
    pub observed_at: String,
    pub delivery_cursor: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    pub goal_id: String,
    pub attention_id: String,
    pub expected_revision: String,
    #[serde(deserialize_with = "required_nullable")]
    pub stage_id: Option<String>,
}
fn required_nullable<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mutation {
    pub operation_id: String,
    #[serde(flatten)]
    pub target: Target,
    pub source: SourceIdentity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    #[serde(flatten)]
    pub mutation: Mutation,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<String>,
    pub receipt: Receipt,
    pub item: Option<Item>,
}
#[derive(Debug)]
pub struct AttentionError {
    pub code: &'static str,
    pub message: String,
    pub current: Option<Value>,
}
impl std::fmt::Display for AttentionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for AttentionError {}
pub(crate) fn error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AttentionError {
        code,
        message: message.into(),
        current: None,
    }
    .into()
}
pub(crate) fn stale(current: Option<&Item>) -> anyhow::Error {
    AttentionError {
        code: "attention_stale",
        message: "Attention ownership or revision changed; review the current item".into(),
        current: Some(serde_json::to_value(current).unwrap_or(Value::Null)),
    }
    .into()
}
pub(crate) fn channel(channel: Option<&str>) -> Result<&str> {
    let channel = channel.unwrap_or("native");
    if channel != "native" {
        return Err(error(
            "attention_unsupported",
            "Trusted remote attention connector is not configured",
        ));
    }
    Ok(channel)
}
pub(crate) fn identity(goal: &str, attention: &str) -> Result<()> {
    canonical_id(goal)?;
    ensure!(
        !attention.trim().is_empty()
            && attention.len() <= 256
            && !attention.chars().any(char::is_control),
        "invalid attention identity"
    );
    Ok(())
}
impl Mutation {
    pub(crate) fn validate(&self, authority: &crate::inbox::SourceAuthority<'_>) -> Result<()> {
        (|| -> Result<()> {
            canonical_id(&self.operation_id)?;
            identity(&self.target.goal_id, &self.target.attention_id)?;
            if let Some(stage) = &self.target.stage_id {
                canonical_id(stage)?;
            }
            let revision = &self.target.expected_revision;
            ensure!(
                revision.len() == 71
                    && revision.starts_with("sha256:")
                    && revision[7..]
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "invalid attention revision"
            );
            authority.validate(&self.source)?;
            Ok(())
        })()
        .map_err(|e| error("attention_invalid_request", e.to_string()))
    }
    pub(crate) fn digest(&self, brain: &str, action: &str, text: Option<&str>) -> Result<String> {
        Ok(hash(&serde_json::to_vec(&(
            brain,
            action,
            &self.target,
            &self.source,
            text,
        ))?))
    }
}
