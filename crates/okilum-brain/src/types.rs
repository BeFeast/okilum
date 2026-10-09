//! Transport-neutral ai-brain/v1 values. Provider IDs are scoped to a binding;
//! none of these values contains provider credentials.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const SCHEMA: &str = "ai-brain/v1";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Criterion {
    pub id: String,
    pub description: String,
    pub requires_human: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceRef {
    pub uri: String,
    pub revision: Option<String>,
    pub locator: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub status: String,
    pub criteria: Vec<Criterion>,
    pub stage_ids: Vec<String>,
    pub task_ref: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stage {
    pub id: String,
    pub goal_id: String,
    pub engine: String,
    pub status: String,
    pub criterion_ids: Vec<String>,
    pub context_id: String,
    pub result_ids: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ContextPacket {
    pub id: String,
    pub goal_id: String,
    pub stage_id: String,
    pub goal_revision: String,
    pub goal: String,
    pub decisions: Vec<String>,
    pub constraints: Vec<String>,
    pub sources: Vec<SourceRef>,
    pub previous_result_id: Option<String>,
    pub next_step: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
/// Exact review boundary shared by prepared-stage edits and guarded Start.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedStageGuard {
    pub stage_id: String,
    pub stage_revision: String,
    pub context_id: String,
    pub context_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PreparedChange {
    Revise { next_step: String },
    Discard,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedChangeRequest {
    pub operation_id: String,
    pub goal_id: String,
    pub expected: PreparedStageGuard,
    #[serde(flatten)]
    pub change: PreparedChange,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedChangeReceipt {
    pub operation_id: String,
    pub goal_id: String,
    pub action: String,
    pub previous: PreparedStageGuard,
    pub replacement: Option<PreparedStageGuard>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineRef {
    pub engine: String,
    pub instance_id: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub task_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StartEnvelope {
    pub schema: String,
    pub operation_id: String,
    pub goal_id: String,
    pub stage_id: String,
    pub context_id: String,
    pub context_revision: String,
    pub packet: ContextPacket,
    pub target: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub engine: String,
    pub cancel: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StartReply {
    Accepted { binding: EngineRef },
    Rejected { reason: String },
    Indeterminate { reason: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReconcileReply {
    Running {
        binding: EngineRef,
        evidence: String,
    },
    OutcomeAvailable {
        binding: EngineRef,
        events: Vec<EngineEvent>,
        evidence: String,
    },
    NotStarted {
        evidence: String,
    },
    Unknown {
        reason: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub id: String,
    pub kind: String,
    pub source: SourceRef,
    pub description: String,
    pub observed_at: String,
    pub status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CriterionEvaluation {
    pub criterion_id: String,
    pub goal_revision: String,
    pub status: String,
    pub evidence_ids: Vec<String>,
    pub evaluated_by: String,
    pub evaluated_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Outcome {
    pub outcome: String,
    pub summary: String,
    pub sources: Vec<SourceRef>,
    pub evidence: Vec<Evidence>,
    pub verification: String,
    pub criterion_evaluations: Vec<CriterionEvaluation>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum EventPayload {
    Status { state: String },
    Outcome(Outcome),
    Attention { message: String },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EngineEvent {
    pub operation_id: String,
    pub engine_ref: EngineRef,
    pub event_id: String,
    pub stream_id: String,
    pub sequence: Option<u64>,
    pub cursor: Option<String>,
    pub observed_at: String,
    #[serde(flatten)]
    pub payload: EventPayload,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ResultRecord {
    pub id: String,
    pub goal_id: String,
    pub stage_id: String,
    pub operation_id: String,
    pub engine_ref: EngineRef,
    pub received_at: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attention {
    pub id: String,
    pub kind: String,
    pub message: String,
}

/// Implementations must return provider evidence for NotStarted. A transport
/// error is indeterminate, never proof that an execution did not start.
pub trait Adapter: Send {
    fn capabilities(&self) -> Capabilities;
    fn start(&mut self, envelope: &StartEnvelope) -> anyhow::Result<StartReply>;
    fn observe(
        &mut self,
        binding: &EngineRef,
        cursors: &BTreeMap<String, String>,
    ) -> anyhow::Result<Vec<EngineEvent>>;
    fn reconcile(
        &mut self,
        envelope: &StartEnvelope,
        binding: Option<&EngineRef>,
    ) -> anyhow::Result<ReconcileReply>;
}
