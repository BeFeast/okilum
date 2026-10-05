//! Workspace inbox records and bounded, provider-independent wire types.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tessera_core::source::SourceSnapshot;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub channel: String,
    pub instance_id: String,
    pub account_id: String,
    pub actor_id: String,
    pub chat_id: Option<String>,
    pub topic_id: Option<String>,
    pub message_id: String,
    pub update_id: String,
    pub uri: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub operation_id: String,
    pub text: String,
    pub source: SourceIdentity,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    pub operation_id: String,
    pub status: String,
    pub request_sha256: String,
    pub replayed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capture {
    pub capture_id: String,
    pub path: String,
    pub revision: String,
    pub received_at: String,
    pub receipt: Receipt,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub capture_id: String,
    pub path: String,
    pub revision: String,
    pub received_at: String,
    pub status: String,
    pub title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Get {
    pub planned_goals: Vec<crate::inbox_plan::PlannedGoal>,
    pub item: Item,
    pub text: String,
    pub source: SourceSnapshot,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct List {
    pub items: Vec<Item>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub observed_at: String,
    pub generation: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Record {
    pub schema: String,
    pub record_type: String,
    pub brain_id: String,
    pub id: String,
    pub status: String,
    pub received_at: String,
    pub source: SourceIdentity,
}
#[derive(Debug)]
pub struct InboxError {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for InboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for InboxError {}
pub(crate) fn error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    InboxError {
        code,
        message: message.into(),
    }
    .into()
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn canonical_id(value: &str) -> Result<()> {
    ensure!(
        Uuid::parse_str(value)?.to_string() == value,
        "ID must be a canonical UUID"
    );
    Ok(())
}
pub(crate) fn now() -> Result<String> {
    Ok(time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?)
}
pub(crate) enum SourceAuthority<'a> {
    Native(&'a str),
    Connector(&'a crate::connector::Authorized),
}
impl SourceAuthority<'_> {
    pub(crate) fn actor(&self) -> &str {
        match self {
            Self::Native(actor) => actor,
            Self::Connector(context) => context.actor(),
        }
    }
    pub(crate) fn validate(&self, source: &SourceIdentity) -> Result<()> {
        match self {
            Self::Native(actor) => source.validate_native(actor),
            Self::Connector(context) => {
                source.validate()?;
                context.validate_source(source)
            }
        }
    }
}
impl SourceIdentity {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.channel.as_str(), "native" | "telegram"),
            "unknown source channel"
        );
        for value in [
            &self.instance_id,
            &self.account_id,
            &self.actor_id,
            &self.message_id,
            &self.update_id,
        ]
        .into_iter()
        .chain(self.chat_id.iter())
        .chain(self.topic_id.iter())
        {
            ensure!(
                !value.trim().is_empty()
                    && value.len() <= 256
                    && !value.chars().any(char::is_control),
                "invalid source identifier"
            );
        }
        if let Some(uri) = &self.uri {
            ensure!(
                uri.len() <= 2048 && !uri.chars().any(char::is_control),
                "invalid source URI"
            );
        }
        Ok(())
    }
    pub(crate) fn validate_native(&self, actor: &str) -> Result<()> {
        self.validate()?;
        ensure!(
            self.channel == "native",
            "trusted Telegram connector is not configured"
        );
        ensure!(
            self.actor_id == actor && self.account_id == "local",
            "source actor does not match the local actor"
        );
        canonical_id(&self.instance_id)?;
        canonical_id(&self.message_id)?;
        canonical_id(&self.update_id)?;
        ensure!(
            self.chat_id.is_none() && self.topic_id.is_none(),
            "native capture cannot choose a remote route"
        );
        Ok(())
    }
    pub(crate) fn external_key(&self) -> Result<String> {
        // The upstream update identity remains stable across actor/content edits;
        // those changes must conflict with its original request digest.
        Ok(hash(&serde_json::to_vec(&(
            &self.channel,
            &self.instance_id,
            &self.account_id,
            &self.chat_id,
            &self.topic_id,
            &self.update_id,
        ))?))
    }
}
impl CaptureRequest {
    pub(crate) fn validate(&self, authority: &SourceAuthority<'_>) -> Result<()> {
        (|| -> Result<()> {
            canonical_id(&self.operation_id)?;
            ensure!(
                !self.text.trim().is_empty() && self.text.len() <= 65_536,
                "capture text must contain 1–65536 UTF-8 bytes"
            );
            authority.validate(&self.source)?;
            Ok(())
        })()
        .map_err(|e| error("inbox_invalid_request", e.to_string()))
    }
    pub(crate) fn digest(&self, brain: &str) -> Result<String> {
        Ok(hash(&serde_json::to_vec(&(
            brain,
            &self.text,
            &self.source,
        ))?))
    }
}
/// Column-zero delimiters only, preserving every body byte.
pub(crate) fn parse(text: &str) -> Result<(Record, &str)> {
    let mut lines = text.split_inclusive('\n');
    let opening = lines.next().context("missing frontmatter")?;
    ensure!(
        crate::retrieval::frontmatter_delimiter(
            opening.strip_prefix('\u{feff}').unwrap_or(opening)
        ),
        "missing frontmatter"
    );
    let mut offset = opening.len();
    for line in lines {
        if crate::retrieval::frontmatter_delimiter(line) {
            let record = serde_yaml::from_str(&text[opening.len()..offset])?;
            return Ok((record, &text[offset + line.len()..]));
        }
        offset += line.len();
    }
    anyhow::bail!("unterminated inbox frontmatter")
}
pub(crate) fn validate_record(
    text: &str,
    path: &str,
    records_dir: &str,
    brain: &str,
) -> Result<(Record, String)> {
    (|| -> Result<_> {
        let (record, body) = parse(text)?;
        canonical_id(&record.id)?;
        ensure!(
            record.schema == crate::SCHEMA
                && record.record_type == "inbox"
                && record.brain_id == brain
                && record.status == "captured"
                && path == format!("{records_dir}/inbox-{}.md", record.id),
            "inbox identity/path mismatch"
        );
        let time = time::OffsetDateTime::parse(
            &record.received_at,
            &time::format_description::well_known::Rfc3339,
        )?;
        ensure!(time.offset().is_utc(), "inbox time must be UTC");
        record.source.validate()?;
        Ok((record, body.to_owned()))
    })()
    .map_err(|e| error("inbox_record_invalid", format!("{path}: {e}")))
}
