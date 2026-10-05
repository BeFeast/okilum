//! Product-owned controls for retained suggestion generation.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRequest {
    pub operation_id: String,
    pub expected_revision: u64,
    pub enabled: bool,
}
impl SetRequest {
    pub(crate) fn validate(&self, actor: &str) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.operation_id)?.to_string() == self.operation_id,
            "invalid suggestions operation identity"
        );
        ensure!(
            !actor.trim().is_empty() && actor.len() <= 256 && !actor.chars().any(char::is_control),
            "invalid suggestions actor"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: String,
    pub workspace: serde_json::Value,
    pub request: SetRequest,
    pub actor: String,
    pub revision: u64,
    pub enabled: bool,
    pub replayed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provider {
    pub available: bool,
    pub model: Option<String>,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub schema: String,
    pub mode: String,
    pub revision: u64,
    pub enrolled: bool,
    pub queued: usize,
    pub running: usize,
    pub backlog: Option<String>,
    pub provider: Provider,
    pub can_change: bool,
}

/// A positively observed refusal before control acceptance. IO errors never use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    ProviderUnavailable,
    RevisionChanged,
    IdentityConflict,
    Unsupported,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Refusal {}
#[derive(Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub schema: String,
    pub status: String,
    pub workspace: serde_json::Value,
    pub request: SetRequest,
    pub receipt: Option<Receipt>,
    pub reason: Option<Refusal>,
}
