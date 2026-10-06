//! Trusted source observations and exact reply intents. No network delivery here.
use crate::store::{Error, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use tessera_inbox_domain::{
    execution::{DeliveryState, Question, Reply},
    OwnerId,
};
use uuid::Uuid;

pub(crate) const SCHEMA: &str = "
CREATE TABLE execution_questions (
 owner_id TEXT NOT NULL, question_id TEXT NOT NULL, project_id TEXT NOT NULL,
 source_identity TEXT NOT NULL, observed_sequence INTEGER NOT NULL, body TEXT NOT NULL,
 PRIMARY KEY(owner_id,question_id), UNIQUE(owner_id,source_identity),
 FOREIGN KEY(owner_id,project_id) REFERENCES execution_projects(owner_id,project_id)
);
CREATE TABLE execution_replies (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
 owner_id TEXT NOT NULL, operation_id TEXT NOT NULL, question_id TEXT NOT NULL,
 request TEXT NOT NULL, question TEXT NOT NULL, state TEXT NOT NULL,
 delivery_id TEXT, error_code TEXT,
 UNIQUE(owner_id,operation_id),
 CHECK(state IN ('queued','accepted','delivered','rejected','uncertain')),
 FOREIGN KEY(owner_id,question_id) REFERENCES execution_questions(owner_id,question_id)
);
CREATE UNIQUE INDEX one_question_reply ON execution_replies(owner_id,question_id) WHERE state!='rejected';
PRAGMA user_version=7;";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReplyOperation {
    pub request: Reply,
    /// Immutable displayed question and source identity at the moment of consent.
    pub question: Question,
    pub state: DeliveryState,
    pub delivery_id: Option<String>,
    pub error_code: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ObservedQuestion {
    #[serde(flatten)]
    pub question: Question,
    pub pending_operation_id: Option<Uuid>,
    pub observed_at: i64,
    pub source_fresh: bool,
}
pub const SOURCE_FRESH_SECONDS: i64 = 30;
fn seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
fn fresh(observed: i64) -> bool {
    let age = seconds().checked_sub(observed);
    observed > 0 && age.is_some_and(|age| (0..=SOURCE_FRESH_SECONDS).contains(&age))
}
fn owner(who: OwnerId) -> Result<String, Error> {
    if who.0.is_nil() {
        return Err(Error::InvalidOwner);
    }
    Ok(who.0.to_string())
}
fn encode(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::InvalidStoredIdentity)
}
fn decode<T: serde::de::DeserializeOwned>(text: String) -> Result<T, Error> {
    serde_json::from_str(&text).map_err(|_| Error::InvalidStoredIdentity)
}
type StoredReply = (String, String, String, Option<String>, Option<String>);
fn operation(db: &Connection, who: &str, id: Uuid) -> Result<Option<ReplyOperation>, Error> {
    let raw: Option<StoredReply>=db.query_row(
        "SELECT request,question,state,delivery_id,error_code FROM execution_replies WHERE owner_id=?1 AND operation_id=?2",
        params![who,id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    raw.map(|(request, question, state, delivery_id, error_code)| {
        Ok(ReplyOperation {
            request: decode(request)?,
            question: decode(question)?,
            state: decode(format!("\"{state}\""))?,
            delivery_id,
            error_code,
        })
    })
    .transpose()
}
impl Store {
    /// Internal adapter entry point. The adapter authenticates and pins source
    /// scope; browser APIs cannot manufacture source questions. Sequence is an
    /// adapter-owned monotonic cursor, never parsed from an opaque revision.
    pub fn observe_execution_question(
        &mut self,
        who: OwnerId,
        question: &Question,
        sequence: u64,
    ) -> Result<(), Error> {
        let who = owner(who)?;
        question.validate()?;
        if sequence == 0 || sequence > i64::MAX as u64 {
            return Err(Error::InvalidPage);
        }
        let identity = encode(&question.source)?;
        let body = encode(question)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_projects WHERE owner_id=?1 AND project_id=?2)",
            params![who, question.project_id.to_string()],
            |r| r.get(0),
        )?;
        if !project_exists {
            return Err(Error::MissingItem);
        }
        let old: Option<(String,String,u64,String)>=tx.query_row("SELECT source_identity,project_id,observed_sequence,body FROM execution_questions WHERE owner_id=?1 AND question_id=?2",params![who,question.id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((source, project, cursor, prior)) = old {
            if source != identity || project != question.project_id.to_string() {
                return Err(Error::OperationConflict);
            }
            if sequence < cursor {
                return Err(Error::ExecutionRevisionConflict);
            }
            let previous: Question = decode(prior.clone())?;
            if previous.source_revision == question.source_revision
                && (previous.fields != question.fields || previous.approval != question.approval)
            {
                return Err(Error::OperationConflict);
            }
            if sequence == cursor {
                // A bridge upgrade may enrich display metadata at the same source cursor.
                let mut comparable = question.clone();
                comparable.thread_title = previous.thread_title.clone();
                if comparable != previous {
                    return Err(Error::OperationConflict);
                }
                tx.execute("UPDATE execution_questions SET observed_at=?1,body=?4 WHERE owner_id=?2 AND question_id=?3", params![seconds(),who,question.id.to_string(),body])?;
                tx.commit()?;
                return Ok(());
            }
        }
        let other: Option<String>=tx.query_row("SELECT question_id FROM execution_questions WHERE owner_id=?1 AND source_identity=?2",params![who,identity],|r|r.get(0)).optional()?;
        if other.is_some_and(|id| id != question.id.to_string()) {
            return Err(Error::OperationConflict);
        }
        tx.execute("INSERT INTO execution_questions(owner_id,question_id,project_id,source_identity,observed_sequence,body) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(owner_id,question_id) DO UPDATE SET observed_sequence=excluded.observed_sequence,body=excluded.body",params![who,question.id.to_string(),question.project_id.to_string(),identity,sequence,body])?;
        tx.execute(
            "UPDATE execution_questions SET observed_at=?1 WHERE owner_id=?2 AND question_id=?3",
            params![seconds(), who, question.id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn execution_question(
        &self,
        who: OwnerId,
        id: Uuid,
    ) -> Result<Option<ObservedQuestion>, Error> {
        let who = owner(who)?;
        let body: Option<(String,i64)> = self
            .connection
            .query_row(
                "SELECT body,observed_at FROM execution_questions WHERE owner_id=?1 AND question_id=?2",
                params![who, id.to_string()],
                |r| Ok((r.get(0)?,r.get(1)?)),
            )
            .optional()?;
        let Some((body, observed_at)) = body else {
            return Ok(None);
        };
        let mut question: Question = decode(body)?;
        let pending: Option<String>=self.connection.query_row("SELECT operation_id FROM execution_replies WHERE owner_id=?1 AND question_id=?2 AND state!='rejected'",params![who,id.to_string()],|r|r.get(0)).optional()?;
        let source_fresh = fresh(observed_at);
        if pending.is_some() || !source_fresh {
            question.can_reply = false;
        }
        Ok(Some(ObservedQuestion {
            question,
            observed_at,
            source_fresh,
            pending_operation_id: pending
                .map(|p| Uuid::parse_str(&p).map_err(|_| Error::InvalidStoredIdentity))
                .transpose()?,
        }))
    }
    pub fn execution_questions(
        &self,
        who: OwnerId,
        project: Uuid,
        after: &str,
        limit: u32,
    ) -> Result<Vec<ObservedQuestion>, Error> {
        if self.execution_project(who, project)?.is_none() {
            return Err(Error::MissingItem);
        }
        if limit == 0 || limit > 100 || (!after.is_empty() && Uuid::parse_str(after).is_err()) {
            return Err(Error::InvalidPage);
        }
        let mut query=self.connection.prepare("SELECT question_id FROM execution_questions WHERE owner_id=?1 AND project_id=?2 AND question_id>?3 ORDER BY question_id LIMIT ?4")?;
        let ids = query.query_map(
            params![owner(who)?, project.to_string(), after, limit],
            |r| r.get::<_, String>(0),
        )?;
        ids.map(|id| {
            self.execution_question(
                who,
                Uuid::parse_str(&id?).map_err(|_| Error::InvalidStoredIdentity)?,
            )?
            .ok_or(Error::MissingItem)
        })
        .collect()
    }
    pub fn execution_reply(&self, who: OwnerId, id: Uuid) -> Result<Option<ReplyOperation>, Error> {
        operation(&self.connection, &owner(who)?, id)
    }
    pub fn prepare_execution_reply(
        &mut self,
        who: OwnerId,
        request: &Reply,
    ) -> Result<ReplyOperation, Error> {
        let who = owner(who)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Exact replay must remain readable after revision changes/withdrawal.
        if let Some(old) = operation(&tx, &who, request.operation_id)? {
            return if old.request == *request {
                Ok(old)
            } else {
                Err(Error::OperationConflict)
            };
        }
        let body: Option<String> = tx
            .query_row(
                "SELECT body FROM execution_questions WHERE owner_id=?1 AND question_id=?2",
                params![who, request.question_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let question: Question = decode(body.ok_or(Error::MissingItem)?)?;
        let observed_at: i64 = tx.query_row(
            "SELECT observed_at FROM execution_questions WHERE owner_id=?1 AND question_id=?2",
            params![who, request.question_id.to_string()],
            |r| r.get(0),
        )?;
        if !fresh(observed_at) {
            return Err(Error::ExecutionRevisionConflict);
        }
        if question.source_revision != request.expected_revision {
            return Err(Error::ExecutionRevisionConflict);
        }
        let reserved: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_replies WHERE owner_id=?1 AND question_id=?2 AND state!='rejected')",params![who,request.question_id.to_string()],|r|r.get(0))?;
        if reserved || !question.can_reply {
            return Err(Error::ExecutionRevisionConflict);
        }
        question.validate_reply(request)?;
        let result = ReplyOperation {
            request: request.clone(),
            question,
            state: DeliveryState::Queued,
            delivery_id: None,
            error_code: None,
        };
        tx.execute("INSERT INTO execution_replies(owner_id,operation_id,question_id,request,question,state) VALUES(?1,?2,?3,?4,?5,'queued')",params![who,request.operation_id.to_string(),request.question_id.to_string(),encode(request)?,encode(&result.question)?])?;
        tx.commit()?;
        Ok(result)
    }
    /// Internal bridge reconciliation only; no browser can mark its own reply
    /// delivered. Queued work must become uncertain durably BEFORE network I/O.
    pub fn advance_execution_reply(
        &mut self,
        who: OwnerId,
        id: Uuid,
        expected: DeliveryState,
        next: DeliveryState,
        delivery_id: Option<&str>,
        error_code: Option<&str>,
    ) -> Result<ReplyOperation, Error> {
        // Validate the requested edge even on replay: an untouched queued row
        // is not evidence that a transition back to queued ever succeeded.
        use DeliveryState::*;
        if !matches!(
            (expected, next),
            (Queued, Uncertain)
                | (Queued, Rejected)
                | (Uncertain, Accepted)
                | (Uncertain, Delivered)
                | (Uncertain, Rejected)
                | (Accepted, Delivered)
                | (Accepted, Uncertain)
                | (Accepted, Rejected)
        ) {
            return Err(Error::InvalidExecutionTransition);
        }
        let who = owner(who)?;
        if delivery_id
            .is_some_and(|v| v.is_empty() || v.len() > 512 || v.chars().any(char::is_control))
            || error_code.is_some_and(|v| {
                v.is_empty()
                    || v.len() > 128
                    || !v
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
        {
            return Err(Error::InvalidExecutionTransition);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = operation(&tx, &who, id)?.ok_or(Error::MissingItem)?;
        if old.state == next
            && old.delivery_id.as_deref() == delivery_id
            && old.error_code.as_deref() == error_code
        {
            return Ok(old);
        }
        if old.state != expected {
            return Err(Error::InvalidExecutionTransition);
        }
        let state = encode(&next)?;
        tx.execute("UPDATE execution_replies SET state=?1,delivery_id=?2,error_code=?3 WHERE owner_id=?4 AND operation_id=?5",params![state.trim_matches('"'),delivery_id,error_code,who,id.to_string()])?;
        let result = operation(&tx, &who, id)?.ok_or(Error::MissingItem)?;
        tx.commit()?;
        Ok(result)
    }
}
