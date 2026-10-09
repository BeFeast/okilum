//! Correlated Discussion sends reuse the initial SourceWrite journal identity.
//! Recovery is a direct read of retained intent, never a provider retry.
use crate::{application::Conversation, discussion_context, retrieval::sha, Runner, SCHEMA};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_core::source::{
    RecoveryRecord, SourceSnapshot, SourceWrite, WriteOutcome, WriteReceipt,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

const REQUEST_SCHEMA: &str = "okilum-discussion-send-request/v1";
const RECEIPT_SCHEMA: &str = "okilum-discussion-send/v1";
const RESULT_SCHEMA: &str = "okilum-discussion-send-result/v1";
const MAX_PATH_BYTES: usize = 4096;
// Actual compact RecoveryRecord serialization at both payload ceilings plus a
// worst-escaped 4096-byte path is 268485317 bytes (49857 bytes of metadata).
// The test exercises the real bounded reader at normal max, this cap and cap+1.
const RECORD_METADATA_RESERVE: u64 = 64 * 1024;
pub(crate) const MAX_RECOVERY_BYTES: u64 = 4
    * (discussion_context::MAX_CANDIDATE as u64).div_ceil(3)
    + 4 * discussion_context::MAX_CONVERSATION.div_ceil(3)
    + RECORD_METADATA_RESERVE;

fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendRequest {
    pub operation_id: String,
    pub goal_id: String,
    pub expected_actor_id: String,
    #[serde(deserialize_with = "nullable")]
    pub conversation_id: Option<String>,
    pub message: String,
    pub source_paths: Vec<String>,
    pub request_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lookup {
    pub operation_id: String,
    pub goal_id: String,
    pub expected_actor_id: String,
    pub request_sha256: String,
}
impl SendRequest {
    pub(crate) fn key(&self) -> Lookup {
        Lookup {
            operation_id: self.operation_id.clone(),
            goal_id: self.goal_id.clone(),
            expected_actor_id: self.expected_actor_id.clone(),
            request_sha256: self.request_sha256.clone(),
        }
    }
    pub(crate) fn validate(&self, brain: &str) -> Result<()> {
        self.key().validate()?;
        if let Some(id) = &self.conversation_id {
            uuid(id)?;
        }
        ensure!(
            self.source_paths.len() <= 32 && self.source_paths.windows(2).all(|v| v[0] < v[1]),
            "manual paths must be sorted and unique, at most32"
        );
        ensure!(
            self.source_paths.iter().all(|p| p.len() <= MAX_PATH_BYTES),
            "manual path exceeds byte budget"
        );
        ensure!(
            self.request_sha256
                == digest(
                    brain,
                    &self.goal_id,
                    &self.expected_actor_id,
                    self.conversation_id.as_deref(),
                    &self.message,
                    &self.source_paths
                )?,
            "client request digest mismatch"
        );
        Ok(())
    }
}
impl Lookup {
    pub(crate) fn validate(&self) -> Result<()> {
        uuid(&self.operation_id)?;
        uuid(&self.goal_id)?;
        ensure!(
            !self.expected_actor_id.is_empty() && self.expected_actor_id.len() <= 4096,
            "invalid actor binding"
        );
        hash(&self.request_sha256)
    }
}
pub(crate) fn digest(
    brain: &str,
    goal: &str,
    actor: &str,
    conversation: Option<&str>,
    message: &str,
    paths: &[String],
) -> Result<String> {
    Ok(sha(&serde_json::to_vec(&(
        REQUEST_SCHEMA,
        brain,
        goal,
        actor,
        conversation,
        message,
        paths,
    ))?))
}
fn uuid(id: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(id)
            .map(|v| v.to_string())
            .ok()
            .as_deref()
            == Some(id),
        "UUID must be canonical"
    );
    Ok(())
}
fn hash(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid SHA256"
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub schema: String,
    pub operation_id: String,
    pub brain_id: String,
    pub goal_id: String,
    pub actor_id: String,
    #[serde(deserialize_with = "nullable")]
    pub original_conversation_id: Option<String>,
    pub conversation_id: String,
    pub turn_id: String,
    pub request_sha256: String,
    pub provider_request_sha256: String,
    pub context_receipt_sha256: String,
}
impl Receipt {
    pub(crate) fn new(
        brain: &str,
        request: &SendRequest,
        conversation: &str,
        turn: &discussion_context::Turn,
    ) -> Result<Self> {
        let (provider, context) = turn.send_hashes();
        Ok(Self {
            schema: RECEIPT_SCHEMA.into(),
            operation_id: request.operation_id.clone(),
            brain_id: brain.into(),
            goal_id: request.goal_id.clone(),
            actor_id: request.expected_actor_id.clone(),
            original_conversation_id: request.conversation_id.clone(),
            conversation_id: conversation.into(),
            turn_id: turn.turn_id.clone(),
            request_sha256: request.request_sha256.clone(),
            provider_request_sha256: provider.into(),
            context_receipt_sha256: context.into(),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    #[serde(deserialize_with = "nullable")]
    pub original_conversation_id: Option<String>,
    pub conversation_id: String,
    pub turn_id: String,
    pub provider_request_sha256: String,
    pub context_receipt_sha256: String,
    pub source_path: String,
    pub projection_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperationResult {
    pub schema: String,
    pub operation_id: String,
    pub brain_id: String,
    pub goal_id: String,
    pub actor_id: String,
    pub request_sha256: String,
    pub status: String,
    #[serde(deserialize_with = "nullable")]
    pub record: Option<Record>,
    #[serde(deserialize_with = "nullable")]
    pub source_receipt: Option<WriteReceipt>,
}
impl OperationResult {
    fn unknown(brain: &str, key: &Lookup) -> Self {
        Self {
            schema: RESULT_SCHEMA.into(),
            operation_id: key.operation_id.clone(),
            brain_id: brain.into(),
            goal_id: key.goal_id.clone(),
            actor_id: key.expected_actor_id.clone(),
            request_sha256: key.request_sha256.clone(),
            status: "unknown".into(),
            record: None,
            source_receipt: None,
        }
    }
    fn bounded(self) -> Result<Self> {
        ensure!(
            serde_json::to_vec(&self)?.len() <= RECORD_METADATA_RESERVE as usize,
            Error::recovery("operation result exceeds byte budget")
        );
        Ok(self)
    }
}
#[derive(Debug)]
pub(crate) struct Error {
    code: &'static str,
    message: String,
    terminal: Option<(String, Lookup)>,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl Error {
    pub(crate) fn recovery(error: impl std::fmt::Display) -> anyhow::Error {
        Self {
            code: "discussion_send_recovery_error",
            message: error.to_string(),
            terminal: None,
        }
        .into()
    }
    fn conflict() -> anyhow::Error {
        Self {
            code: "discussion_send_conflict",
            message: "operation UUID belongs to different original input or owner".into(),
            terminal: None,
        }
        .into()
    }
    pub(crate) fn rejected(
        brain: &str,
        key: Lookup,
        error: impl std::fmt::Display,
    ) -> anyhow::Error {
        Self {
            code: "discussion_send_rejected",
            message: error.to_string(),
            terminal: Some((brain.into(), key)),
        }
        .into()
    }
    pub(crate) fn value(&self) -> Value {
        let mut v = json!({"code":self.code,"message":self.message});
        if let Some((brain, key)) = &self.terminal {
            v.as_object_mut().unwrap().extend(json!({"operation_id":key.operation_id,"brain_id":brain,"goal_id":key.goal_id,"request_sha256":key.request_sha256,"recorded":false}).as_object().unwrap().clone());
        }
        v
    }
}

fn decode(encoded: &str, max: u64) -> Result<Vec<u8>> {
    ensure!(
        encoded.len() as u64 <= 4 * max.div_ceil(3),
        "preserved bytes exceed recovery budget"
    );
    let bytes = STANDARD
        .decode(encoded)
        .context("invalid retained Base64")?;
    ensure!(
        bytes.len() as u64 <= max,
        "preserved bytes exceed recovery budget"
    );
    Ok(bytes)
}
fn revision(bytes: &[u8]) -> String {
    format!("sha256:{}", sha(bytes))
}

fn validate_projection(runner: &Runner, write: &SourceWrite) -> Result<(Receipt, Record)> {
    uuid(&write.operation_id)?;
    if let Some(previous) = &write.expected_revision {
        hash(
            previous
                .strip_prefix("sha256:")
                .context("invalid expected revision")?,
        )?;
    }
    ensure!(
        write.schema == SCHEMA
            && write.brain_id == runner.workspace_identity()["brain_id"]
            && write.path.len() <= MAX_PATH_BYTES,
        "stored SourceWrite identity mismatch"
    );
    let bytes = decode(
        &write.content_base64,
        discussion_context::MAX_CANDIDATE as u64,
    )?;
    let projected_revision = revision(&bytes);
    let source = SourceSnapshot {
        schema: write.schema.clone(),
        brain_id: write.brain_id.clone(),
        path: write.path.clone(),
        revision: projected_revision.clone(),
        content_base64: write.content_base64.clone(),
        media_type: "text/markdown".into(),
    };
    let (metadata, _) = crate::runtime::parse_document(&source)?;
    let value = serde_json::to_value(metadata)?;
    ensure!(
        value["schema"] == SCHEMA
            && value["record_type"] == "conversation"
            && value["brain_id"] == write.brain_id,
        "invalid conversation projection ownership"
    );
    let receipt: Receipt = serde_json::from_value(
        value
            .get("discussion_send")
            .context("foreign source operation has no Discussion receipt")?
            .clone(),
    )?;
    ensure!(
        receipt.schema == RECEIPT_SCHEMA
            && receipt.operation_id == write.operation_id
            && receipt.brain_id == write.brain_id,
        "invalid Discussion receipt identity"
    );
    uuid(&receipt.goal_id)?;
    uuid(&receipt.conversation_id)?;
    uuid(&receipt.turn_id)?;
    if let Some(id) = &receipt.original_conversation_id {
        uuid(id)?;
        ensure!(
            id == &receipt.conversation_id,
            "original conversation target mismatch"
        );
    }
    ensure!(
        receipt.original_conversation_id.is_some() == write.expected_revision.is_some(),
        "original conversation target/revision mismatch"
    );
    hash(&receipt.request_sha256)?;
    hash(&receipt.provider_request_sha256)?;
    hash(&receipt.context_receipt_sha256)?;
    ensure!(
        !receipt.actor_id.is_empty() && receipt.actor_id.len() <= 4096,
        "invalid retained actor binding"
    );
    let c: Conversation = serde_json::from_value(value.clone())?;
    ensure!(
        c.id == receipt.conversation_id
            && c.goal_id == receipt.goal_id
            && c.path == write.path
            && write.path == runner.path("conversation", &c.id)
            && c.status == "running"
            && c.partial.is_empty()
            && c.error.is_none(),
        "initial conversation projection mismatch"
    );
    let message = c.messages.last().context("missing initial user message")?;
    ensure!(
        message.role == "user"
            && (receipt.original_conversation_id.is_some() || c.messages.len() == 1),
        "initial transcript mismatch"
    );
    let paths: Vec<_> = c.sources.iter().map(|s| s.path.clone()).collect();
    ensure!(
        paths.len() <= 32 && paths.windows(2).all(|v| v[0] < v[1]),
        "invalid retained manual source paths"
    );
    for s in &c.sources {
        ensure!(
            s.schema == SCHEMA && s.brain_id == receipt.brain_id && s.path.len() <= MAX_PATH_BYTES,
            "manual source ownership mismatch"
        );
        let data = decode(&s.content_base64, discussion_context::MAX_REQUEST as u64)?;
        ensure!(
            s.revision == revision(&data),
            "manual source revision mismatch"
        );
    }
    ensure!(
        receipt.request_sha256
            == digest(
                &receipt.brain_id,
                &receipt.goal_id,
                &receipt.actor_id,
                receipt.original_conversation_id.as_deref(),
                &message.text,
                &paths
            )?,
        "preserved client digest mismatch"
    );
    discussion_context::validate_send_projection(
        runner,
        &c,
        &value["discussion_context"],
        &receipt,
    )?;
    let record = Record {
        original_conversation_id: receipt.original_conversation_id.clone(),
        conversation_id: receipt.conversation_id.clone(),
        turn_id: receipt.turn_id.clone(),
        provider_request_sha256: receipt.provider_request_sha256.clone(),
        context_receipt_sha256: receipt.context_receipt_sha256.clone(),
        source_path: write.path.clone(),
        projection_revision: projected_revision,
    };
    Ok((receipt, record))
}

fn validate_journal(journal: &RecoveryRecord, record: &Record) -> Result<&'static str> {
    ensure!(
        journal.base.is_none(),
        "Discussion initial projection unexpectedly has a base snapshot"
    );
    let previous = journal
        .preimage_base64
        .as_deref()
        .map(|v| decode(v, discussion_context::MAX_CONVERSATION))
        .transpose()?;
    ensure!(
        journal.previous_revision == previous.as_deref().map(revision),
        "stored preimage revision mismatch"
    );
    for v in journal.divergent_observations_base64.iter().flatten() {
        decode(v, discussion_context::MAX_CONVERSATION)?;
    }
    ensure!(
        journal.receipt.is_none() || journal.conflict.is_none(),
        "contradictory projection receipt and conflict"
    );
    if let Some(receipt) = &journal.receipt {
        ensure!(
            receipt.operation_id == journal.request.operation_id
                && receipt.path == journal.request.path
                && receipt.previous_revision == journal.previous_revision
                && receipt.revision == record.projection_revision
                && journal.request.expected_revision == journal.previous_revision,
            "stored write receipt mismatch"
        );
        ensure!(
            receipt.outcome
                == if receipt.previous_revision.as_deref() == Some(&receipt.revision) {
                    WriteOutcome::Unchanged
                } else {
                    WriteOutcome::Written
                },
            "stored write outcome mismatch"
        );
        Ok("projected")
    } else if let Some(conflict) = &journal.conflict {
        ensure!(
            conflict.conflict_id == journal.request.operation_id
                && conflict.path == journal.request.path
                && conflict.expected_revision == journal.request.expected_revision
                && conflict.current_revision == journal.previous_revision
                && matches!(
                    conflict.reason.as_str(),
                    "stale_revision" | "unmanaged_writers"
                ),
            "stored projection conflict mismatch"
        );
        Ok("projection_conflict")
    } else {
        Ok("pending_projection")
    }
}

pub(crate) fn lookup(runner: &Runner, key: &Lookup) -> Result<OperationResult> {
    key.validate().map_err(Error::recovery)?;
    let brain = runner.workspace_identity()["brain_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let pending = runner
        .discussion_pending_write(&key.operation_id)
        .map_err(Error::recovery)?;
    let stored = match runner.discussion_recovery_record(&key.operation_id, MAX_RECOVERY_BYTES) {
        Ok(v) => Some(v),
        Err(e) if e.code == okilum_core::source::ErrorCode::NotFound => None,
        Err(e) => return Err(Error::recovery(e)),
    };
    let Some(write) = pending.or_else(|| stored.as_ref().map(|r| &r.request)) else {
        return OperationResult::unknown(&brain, key).bounded();
    };
    if let (Some(p), Some(s)) = (pending, stored.as_ref()) {
        ensure!(
            p == &s.request,
            Error::recovery("pending and SourceStore operation disagree")
        );
    }
    ensure!(
        write.operation_id == key.operation_id,
        Error::recovery("journal filename/operation UUID mismatch")
    );
    let (receipt, record) = validate_projection(runner, write).map_err(Error::recovery)?;
    if receipt.goal_id != key.goal_id
        || receipt.actor_id != key.expected_actor_id
        || receipt.request_sha256 != key.request_sha256
    {
        return Err(Error::conflict());
    }
    let status = stored
        .as_ref()
        .map(|r| validate_journal(r, &record))
        .transpose()
        .map_err(Error::recovery)?
        .unwrap_or("pending_projection");
    let mut result = OperationResult::unknown(&brain, key);
    result.status = status.into();
    result.record = Some(record);
    result.source_receipt = stored.and_then(|s| s.receipt);
    result.bounded()
}

pub(crate) enum Admission {
    NewlyCommitted(Box<discussion_context::Prepared>, OperationResult),
    ExistingOperation(OperationResult),
}
pub(crate) fn send(
    app: &crate::application::Application,
    runner: &mut Runner,
    request: SendRequest,
) -> Result<Admission> {
    let brain = runner.workspace_identity()["brain_id"]
        .as_str()
        .unwrap()
        .to_owned();
    request.validate(&brain).map_err(Error::recovery)?;
    let key = request.key();
    let previous = lookup(runner, &key)?;
    if previous.status != "unknown" {
        return Ok(Admission::ExistingOperation(previous));
    }
    if request.expected_actor_id != app.local_actor()
        || !runner.goal_ids().contains(&request.goal_id)
    {
        return Err(Error::rejected(
            &brain,
            key,
            "new send must match configured actor and existing goal",
        ));
    }
    let prepared = runner.with_goal(&request.goal_id, |r| app.chat_send_prepared(r, &request))?;
    let receipt = lookup(runner, &key)?;
    ensure!(
        receipt.status == "projected",
        Error::recovery("initial projection commit not confirmed; provider not dispatched")
    );
    Ok(Admission::NewlyCommitted(Box::new(prepared), receipt))
}

#[cfg(test)]
mod tests;
