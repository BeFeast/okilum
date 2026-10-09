//! Explicit immutable brief launches. Operator targets grant authority, not drafts.
use crate::store::{Error, Store};
use okilum_inbox_domain::{execution::Brief, OwnerId};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) const SCHEMA: &str = "CREATE TABLE execution_launches (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT, owner_id TEXT NOT NULL,
 operation_id TEXT NOT NULL, brief_id TEXT NOT NULL, revision INTEGER NOT NULL,
 body TEXT NOT NULL, UNIQUE(owner_id,operation_id), UNIQUE(owner_id,brief_id,revision),
 FOREIGN KEY(owner_id,brief_id,revision) REFERENCES execution_briefs(owner_id,brief_id,revision)
); PRAGMA user_version=9;";
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    pub label: String,
    pub repository: String,
    /// Pinned commit, not a moving branch. Native T3 prepares a fresh worktree.
    pub base_commit: String,
    pub model_selection: serde_json::Value,
    pub runtime_mode: String,
    pub interaction_mode: String,
}
impl Target {
    pub fn validate(&self) -> bool {
        [&self.id, &self.label, &self.repository]
            .into_iter()
            .all(|s| !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
            && self.base_commit.len() == 40
            && self.base_commit.bytes().all(|b| b.is_ascii_hexdigit())
            && self
                .model_selection
                .get("instanceId")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty())
            && self
                .model_selection
                .get("model")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty())
            && self.model_selection.to_string().len() <= 4096
            && matches!(
                self.runtime_mode.as_str(),
                "approval-required" | "full-access"
            )
            && matches!(self.interaction_mode.as_str(), "default" | "plan")
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TargetSnapshot {
    pub project_id: Uuid,
    pub instance_id: String,
    pub source_project_id: String,
    pub target: Target,
}
impl TargetSnapshot {
    pub fn revision(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).expect("typed target is serializable"))
        )
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub operation_id: Uuid,
    pub brief_id: Uuid,
    pub expected_revision: u64,
    pub target_revision: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Uncertain,
    Accepted,
    Preparing,
    Running,
    Completed,
    Failed,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Operation {
    pub request: Launch,
    pub brief: Brief,
    pub target: TargetSnapshot,
    pub thread_id: Uuid,
    pub command_id: Uuid,
    pub message_id: Uuid,
    pub state: State,
    pub run_id: Option<String>,
    pub worktree_path: Option<String>,
    pub error_code: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub expected: State,
    pub next: State,
    pub run_id: Option<String>,
    pub worktree_path: Option<String>,
    pub error_code: Option<String>,
}
fn encode(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::InvalidStoredIdentity)
}
fn decode(body: String) -> Result<Operation, Error> {
    serde_json::from_str(&body).map_err(|_| Error::InvalidStoredIdentity)
}
impl Store {
    pub fn execution_launch(&self, who: OwnerId, id: Uuid) -> Result<Option<Operation>, Error> {
        let body: Option<String> = self
            .connection
            .query_row(
                "SELECT body FROM execution_launches WHERE owner_id=?1 AND operation_id=?2",
                params![who.0.to_string(), id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        body.map(decode).transpose()
    }
    pub fn prepare_execution_launch(
        &mut self,
        who: OwnerId,
        request: &Launch,
        target: &TargetSnapshot,
    ) -> Result<Operation, Error> {
        if who.0.is_nil() || request.operation_id.is_nil() || request.brief_id.is_nil() {
            return Err(Error::InvalidOwner);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT body FROM execution_launches WHERE owner_id=?1 AND operation_id=?2",
                params![who.0.to_string(), request.operation_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            let old = decode(old)?;
            return if old.request == *request {
                Ok(old)
            } else {
                Err(Error::OperationConflict)
            };
        }
        if !target.target.validate() || request.target_revision != target.revision() {
            return Err(Error::ExecutionRevisionConflict);
        }
        let body:Option<String>=tx.query_row("SELECT body FROM execution_briefs WHERE owner_id=?1 AND brief_id=?2 ORDER BY revision DESC LIMIT 1",params![who.0.to_string(),request.brief_id.to_string()],|r|r.get(0)).optional()?;
        let brief: Brief = serde_json::from_str(&body.ok_or(Error::MissingItem)?)
            .map_err(|_| Error::InvalidStoredIdentity)?;
        if brief.revision != request.expected_revision
            || brief.project_id != target.project_id
            || brief.target_id != target.target.id
        {
            return Err(Error::ExecutionRevisionConflict);
        }
        let reserved:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_launches WHERE owner_id=?1 AND brief_id=?2 AND revision=?3)",params![who.0.to_string(),brief.id.to_string(),brief.revision],|r|r.get(0))?;
        if reserved {
            return Err(Error::OperationConflict);
        }
        let op = Operation {
            request: request.clone(),
            brief,
            target: target.clone(),
            thread_id: Uuid::new_v4(),
            command_id: Uuid::new_v4(),
            message_id: Uuid::new_v4(),
            state: State::Queued,
            run_id: None,
            worktree_path: None,
            error_code: None,
        };
        tx.execute("INSERT INTO execution_launches(owner_id,operation_id,brief_id,revision,body) VALUES(?1,?2,?3,?4,?5)",params![who.0.to_string(),request.operation_id.to_string(),op.brief.id.to_string(),op.brief.revision,encode(&op)?])?;
        tx.commit()?;
        Ok(op)
    }
    pub fn execution_launches(
        &self,
        who: OwnerId,
        project: Uuid,
        after: u64,
        limit: u32,
    ) -> Result<Vec<(u64, Operation)>, Error> {
        if after > i64::MAX as u64 || limit == 0 || limit > 101 {
            return Err(Error::InvalidPage);
        }
        let mut query=self.connection.prepare("SELECT sequence,body FROM execution_launches WHERE owner_id=?1 AND json_extract(body,'$.brief.project_id')=?2 AND sequence>?3 ORDER BY sequence LIMIT ?4")?;
        let rows = query.query_map(
            params![who.0.to_string(), project.to_string(), after, limit],
            |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?)),
        )?;
        rows.map(|row| {
            let (seq, body) = row?;
            Ok((seq, decode(body)?))
        })
        .collect()
    }
    pub fn advance_execution_launch(
        &mut self,
        who: OwnerId,
        id: Uuid,
        p: &Progress,
    ) -> Result<Operation, Error> {
        use State::*;
        if !matches!(
            (p.expected, p.next),
            (Queued, Uncertain)
                | (
                    Uncertain,
                    Accepted | Preparing | Running | Completed | Failed
                )
                | (
                    Accepted,
                    Accepted | Uncertain | Preparing | Running | Completed | Failed
                )
                | (
                    Preparing,
                    Preparing | Uncertain | Running | Completed | Failed
                )
                | (Running, Running | Uncertain | Completed | Failed)
        ) {
            return Err(Error::InvalidExecutionTransition);
        }
        if [&p.run_id, &p.worktree_path]
            .into_iter()
            .flatten()
            .any(|s| s.is_empty() || s.len() > 2048 || s.chars().any(char::is_control))
            || p.error_code.as_ref().is_some_and(|s| {
                s.is_empty()
                    || s.len() > 128
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
        {
            return Err(Error::InvalidExecutionTransition);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let body: Option<String> = tx
            .query_row(
                "SELECT body FROM execution_launches WHERE owner_id=?1 AND operation_id=?2",
                params![who.0.to_string(), id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let mut op = decode(body.ok_or(Error::MissingItem)?)?;
        if op.state == p.next
            && op.run_id == p.run_id
            && op.worktree_path == p.worktree_path
            && op.error_code == p.error_code
        {
            return Ok(op);
        }
        if op.state != p.expected
            || (op.run_id.is_some() && op.run_id != p.run_id)
            || (op.worktree_path.is_some() && op.worktree_path != p.worktree_path)
        {
            return Err(Error::InvalidExecutionTransition);
        }
        op.state = p.next;
        op.run_id = p.run_id.clone();
        op.worktree_path = p.worktree_path.clone();
        op.error_code = p.error_code.clone();
        tx.execute(
            "UPDATE execution_launches SET body=?1 WHERE owner_id=?2 AND operation_id=?3",
            params![encode(&op)?, who.0.to_string(), id.to_string()],
        )?;
        tx.commit()?;
        Ok(op)
    }
}
