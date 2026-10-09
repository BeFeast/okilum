//! Durable local operation dispositions; never Maestro execution commands.
use crate::maestro_links::{LinkRequest, UnlinkRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA: &str = "tessera-maestro-operation/v1";
pub const ENROLLMENT_FIELD: &str = "maestro_operation_dispositions";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Request {
    Link(LinkRequest),
    Unlink(UnlinkRequest),
    ApprovalDecision(Box<crate::maestro_control::Request>),
}
impl Request {
    pub fn operation_id(&self) -> &str {
        match self {
            Self::Link(r) => &r.operation_id,
            Self::Unlink(r) => &r.operation_id,
            Self::ApprovalDecision(r) => &r.operation_id,
        }
    }
    pub fn goal_id(&self) -> &str {
        match self {
            Self::Link(r) => &r.goal_id,
            Self::Unlink(r) => &r.goal_id,
            Self::ApprovalDecision(r) => &r.goal_id,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Link(_) => "link",
            Self::Unlink(_) => "unlink",
            Self::ApprovalDecision(_) => "approval_decision",
        }
    }
    pub fn body(&self) -> Value {
        match self {
            Self::Link(r) => serde_json::to_value(r).expect("typed link request"),
            Self::Unlink(r) => serde_json::to_value(r).expect("typed unlink request"),
            Self::ApprovalDecision(r) => serde_json::to_value(r).expect("typed approval decision"),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub operation_id: String,
    pub goal_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rejection {
    #[serde(default)]
    pub never_sent: bool,
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Disposition {
    pub schema: String,
    pub operation_id: String,
    pub goal_id: String,
    pub kind: Option<String>,
    pub request: Option<Value>,
    pub status: String,
    pub receipt: Option<Value>,
    pub rejection: Option<Rejection>,
}
impl Disposition {
    pub fn unknown(goal_id: &str, operation_id: &str) -> Self {
        Self {
            schema: SCHEMA.into(),
            operation_id: operation_id.into(),
            goal_id: goal_id.into(),
            kind: None,
            request: None,
            status: "unknown".into(),
            receipt: None,
            rejection: None,
        }
    }
}
#[derive(Debug)]
pub struct Error {
    pub disposition: Disposition,
    pub message: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

/// New dispositions must fail closed in successful-receipt-only readers.
pub(crate) fn enrollment_identity(mut marker: Value) -> anyhow::Result<Value> {
    let decision_version = marker
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Invalid Maestro enrollment marker"))?
        .remove(crate::maestro_control::ENROLLMENT_FIELD);
    anyhow::ensure!(
        decision_version.is_none() || decision_version == Some(serde_json::json!(1)),
        "Unknown Maestro decision enrollment version"
    );
    let version = marker
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Invalid Maestro enrollment marker"))?
        .remove(ENROLLMENT_FIELD);
    anyhow::ensure!(
        version.is_none() || version == Some(serde_json::json!(1)),
        "Unknown Maestro operation enrollment version"
    );
    Ok(marker)
}
