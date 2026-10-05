//! Native planning retains a pinned inbox origin as unverified operator input.
use crate::{
    inbox::{self, Receipt, SourceIdentity},
    Criterion, Goal,
};
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use tessera_core::source::SourceSnapshot;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation_id: String,
    pub capture_id: String,
    pub expected_capture_revision: String,
    pub title: String,
    pub criteria: Vec<Criterion>,
    pub source: SourceIdentity,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Origin {
    pub schema: String,
    pub brain_id: String,
    pub capture_id: String,
    pub path: String,
    pub revision: String,
    pub text: String,
    pub source_snapshot: SourceSnapshot,
    pub operation_id: String,
    pub planned_by: SourceIdentity,
    pub planned_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Outcome {
    pub receipt: Receipt,
    pub goal_id: String,
    pub origin: Origin,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedGoal {
    pub goal_id: String,
    pub title: String,
}
impl Request {
    pub(crate) fn validate(&self, actor: &str) -> Result<()> {
        (|| -> Result<()> {
            inbox::canonical_id(&self.operation_id)?;
            inbox::canonical_id(&self.capture_id)?;
            self.source.validate_native(actor)?;
            ensure!(
                !self.title.trim().is_empty() && self.title.len() <= 512,
                "plan title must contain1–512 UTF-8 bytes"
            );
            ensure!(
                !self.criteria.is_empty() && self.criteria.len() <= 50,
                "plan requires1–50 outcome criteria"
            );
            let mut ids = std::collections::BTreeSet::new();
            ensure!(
                self.criteria.iter().all(|c| !c.id.is_empty()
                    && c.id.len() <= 128
                    && !c.description.trim().is_empty()
                    && c.description.len() <= 4096
                    && ids.insert(&c.id)),
                "criteria need unique IDs and bounded descriptions"
            );
            ensure!(
                self.expected_capture_revision.starts_with("sha256:")
                    && self.expected_capture_revision.len() == 71,
                "invalid expected capture revision"
            );
            Ok(())
        })()
        .map_err(|e| inbox::error("inbox_plan_invalid_request", e.to_string()))
    }
    pub(crate) fn digest(&self, brain: &str) -> Result<String> {
        Ok(inbox::hash(&serde_json::to_vec(&(
            brain,
            &self.capture_id,
            &self.expected_capture_revision,
            &self.title,
            &self.criteria,
            &self.source,
        ))?))
    }
}
pub fn origin(goal: &Goal) -> Result<Option<Origin>> {
    let Some(value) = goal.extra.get("origin_inbox") else {
        return Ok(None);
    };
    let origin: Origin = serde_json::from_value(value.clone())?;
    let snapshot = &origin.source_snapshot;
    ensure!(
        origin.schema == "ai-brain/inbox-origin-v1"
            && origin.brain_id == snapshot.brain_id
            && origin.path == snapshot.path
            && origin.revision == snapshot.revision,
        "original thought identity differs from retained snapshot"
    );
    inbox::canonical_id(&origin.capture_id)?;
    inbox::canonical_id(&origin.operation_id)?;
    let bytes = STANDARD.decode(&snapshot.content_base64)?;
    ensure!(
        format!("sha256:{}", inbox::hash(&bytes)) == origin.revision,
        "original thought snapshot revision differs"
    );
    let (record, body) = inbox::parse(std::str::from_utf8(&bytes)?)?;
    ensure!(
        record.id == origin.capture_id && record.brain_id == origin.brain_id && body == origin.text,
        "original thought content differs from retained snapshot"
    );
    Ok(Some(origin))
}
pub fn operator_input(goal: &Goal) -> Result<Option<String>> {
    Ok(origin(goal)?.map(|o|format!("## Original thought — operator input, unverified\n\nSource: {} ({}), capture {}\n\n{}\n",o.path,o.revision,o.capture_id,o.text)))
}

pub(crate) fn exclude_pinned_source(goal: &Goal, sources: &mut Vec<SourceSnapshot>) -> Result<()> {
    if let Some(origin) = origin(goal)? {
        sources.retain(|source| source.path != origin.path || source.revision != origin.revision);
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) mod frozen_goal;
