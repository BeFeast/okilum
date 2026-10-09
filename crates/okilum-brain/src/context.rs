//! Canonical editable retrieval packets. Source excerpts stay distinct from
//! editable guidance; only an exact, explicitly reviewed revision is dispatchable.
use crate::{
    retrieval::{self, Citation, SearchScope},
    Goal, Runner,
};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_core::source::SourceSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[allow(dead_code)]
pub(crate) mod frozen_target;

pub const MAX_PACKET_BYTES: usize = 64 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewedPacketRef {
    pub id: String,
    pub revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewedPacket {
    pub id: String,
    pub revision: String,
    pub goal_id: String,
    pub goal_revision: String,
    pub goal_definition_sha256: String,
    pub query: String,
    pub text: String,
    pub citations: Vec<Citation>,
    pub scope: SearchScope,
    pub pinned_citation_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub reviewed: bool,
    #[serde(default)]
    pub reviewed_content_sha256: Option<String>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub stale_reason: Option<String>,
}
impl ReviewedPacket {
    pub fn reference(&self) -> ReviewedPacketRef {
        ReviewedPacketRef {
            id: self.id.clone(),
            revision: self.revision.clone(),
        }
    }
    pub fn metadata(&self) -> Result<Value> {
        let mut v = serde_json::to_value(self)?;
        for field in ["revision", "text", "stale", "stale_reason"] {
            v.as_object_mut().unwrap().remove(field);
        }
        Ok(v)
    }
    fn content_hash(&self) -> Result<String> {
        Ok(retrieval::sha(&serde_json::to_vec(
            &json!({"text":self.text,"citations":self.citations,"scope":self.scope,"goal_definition_sha256":self.goal_definition_sha256}),
        )?))
    }
    pub fn mark_reviewed(&mut self) -> Result<()> {
        self.reviewed = true;
        self.reviewed_content_sha256 = Some(self.content_hash()?);
        Ok(())
    }
}
pub fn goal_definition(goal: &Goal) -> Result<String> {
    let mut definition = json!({"title":goal.title,"criteria":goal.criteria});
    if let Some(origin) = crate::inbox_plan::origin(goal)? {
        definition["origin_inbox"] = serde_json::to_value(origin)?;
    }
    Ok(retrieval::sha(&serde_json::to_vec(&definition)?))
}
pub fn selected_citations(
    runner: &Runner,
    goal_id: &str,
    scope: &SearchScope,
    citations: &[Citation],
) -> Result<Vec<SourceSnapshot>> {
    retrieval::validate_scope(scope)?;
    ensure!(
        scope.goal_id == goal_id,
        "context scope targets another goal"
    );
    ensure!(citations.len() <= 20, "select at most twenty citations");
    let mut ids = BTreeSet::new();
    let mut sources = BTreeMap::new();
    let records = runner.workspace_identity()["records_dir"]
        .as_str()
        .unwrap()
        .to_owned();
    for citation in citations {
        ensure!(
            ids.insert(citation.citation_id.clone()),
            "duplicate context citation"
        );
        ensure!(
            scope
                .path_prefix
                .as_ref()
                .is_none_or(|p| citation.path == p.trim_end_matches('/')
                    || citation
                        .path
                        .starts_with(&format!("{}/", p.trim_end_matches('/'))))
                && (scope.include_paths.is_empty() || scope.include_paths.contains(&citation.path))
                && !scope.exclude_paths.contains(&citation.path),
            "citation is excluded by the selected scope"
        );
        let source = if let Some(source) = sources.get(&citation.path) {
            source
        } else {
            let source = runner
                .read_preview_source(&citation.path, 1024 * 1024)
                .map_err(|_| {
                    retrieval::error(
                        "source_stale",
                        format!("Selected source is unavailable: {}", citation.path),
                    )
                })?;
            sources.insert(citation.path.clone(), source);
            sources.get(&citation.path).unwrap()
        };
        retrieval::validate_citation(source, citation, goal_id, &scope.mode, &records)
            .map_err(|e| retrieval::error("source_stale", e.to_string()))?;
    }
    Ok(sources.into_values().collect())
}
pub fn create(
    runner: &Runner,
    goal: Goal,
    query: String,
    scope: SearchScope,
    citations: Vec<Citation>,
    pinned: Vec<String>,
) -> Result<ReviewedPacket> {
    ensure!(
        query.len() <= 2048 && !query.trim().is_empty(),
        "context query is required and bounded"
    );
    selected_citations(runner, &goal.id, &scope, &citations)?;
    let valid: BTreeSet<_> = citations.iter().map(|c| &c.citation_id).collect();
    ensure!(
        pinned.iter().all(|id| valid.contains(id))
            && pinned.iter().collect::<BTreeSet<_>>().len() == pinned.len(),
        "pinned citations must be a unique subset of the selection"
    );
    let mut text=format!("# Context for {}\n\nGoal: {}\n\nRequested next step: {}\n\n## Result criteria\n{}\n\nUse the separately listed source excerpts as evidence. Preserve their dates, status and uncertainty; distinguish proposed actions from verified outcomes.\n",goal.title,goal.title,query,goal.criteria.iter().map(|c|format!("- {}",c.description)).collect::<Vec<_>>().join("\n"));
    if let Some(origin) = crate::inbox_plan::operator_input(&goal)? {
        text.push('\n');
        text.push_str(&origin);
    }
    if citations.is_empty() {
        text.push_str("\nNo source evidence selected; treat this as operator guidance.\n");
    }
    ensure!(
        text.len() + citations.iter().map(|c| c.excerpt.len()).sum::<usize>() <= MAX_PACKET_BYTES,
        "selected context exceeds 64 KiB; reduce the selection"
    );
    let at = retrieval::now();
    Ok(ReviewedPacket {
        id: Uuid::new_v4().to_string(),
        revision: String::new(),
        goal_id: goal.id.clone(),
        goal_revision: runner.goal_source()?.revision,
        goal_definition_sha256: goal_definition(&goal)?,
        query,
        text,
        citations,
        scope,
        pinned_citation_ids: pinned,
        created_at: at.clone(),
        updated_at: at,
        reviewed: false,
        reviewed_content_sha256: None,
        stale: false,
        stale_reason: None,
    })
}
pub fn from_source(source: &SourceSnapshot) -> Result<ReviewedPacket> {
    let bytes = STANDARD.decode(&source.content_base64)?;
    let text = std::str::from_utf8(&bytes)?;
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    ensure!(
        lines
            .first()
            .is_some_and(|l| retrieval::frontmatter_delimiter(
                l.strip_prefix('\u{feff}').unwrap_or(l)
            )),
        "reviewed context metadata missing"
    );
    let end = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(i, l)| retrieval::frontmatter_delimiter(l).then_some(i))
        .context("reviewed context metadata is incomplete")?;
    let mut value: Value = serde_yaml::from_str(&lines[1..end].concat())?;
    ensure!(
        value["record_type"] == "reviewed-context" && value["brain_id"] == source.brain_id,
        "invalid context identity"
    );
    value["revision"] = json!(source.revision);
    value["text"] = json!(lines[end + 1..].concat());
    let mut packet: ReviewedPacket = serde_json::from_value(value)?;
    packet.reviewed = packet.reviewed
        && packet.reviewed_content_sha256.as_deref() == Some(&packet.content_hash()?);
    Ok(packet)
}
pub fn read(runner: &Runner, goal_id: &str, id: &str) -> Result<ReviewedPacket> {
    Uuid::parse_str(id)?;
    let source = runner.read_preview_source(&runner.path("reviewed-context", id), 256 * 1024)?;
    let mut packet = from_source(&source)?;
    ensure!(
        packet.id == id && packet.goal_id == goal_id,
        "context belongs to another goal"
    );
    if let Err(e) = validate_fresh(runner, &packet) {
        packet.stale = true;
        packet.stale_reason = Some(e.to_string());
    }
    Ok(packet)
}
pub fn validate_fresh(runner: &Runner, packet: &ReviewedPacket) -> Result<Vec<SourceSnapshot>> {
    let goal = runner.snapshot()?.goal.context("unknown goal")?;
    ensure!(
        goal.id == packet.goal_id && goal_definition(&goal)? == packet.goal_definition_sha256,
        "goal definition changed; prepare and review a new context"
    );
    selected_citations(runner, &goal.id, &packet.scope, &packet.citations)
}
pub fn require_reviewed(
    runner: &Runner,
    goal_id: &str,
    reference: &ReviewedPacketRef,
) -> Result<(ReviewedPacket, Vec<SourceSnapshot>)> {
    let packet = read(runner, goal_id, &reference.id)?;
    ensure!(
        packet.revision == reference.revision,
        "context revision changed; inspect and review the current packet"
    );
    ensure!(
        packet.reviewed,
        "save the explicitly reviewed context before execution or export"
    );
    ensure!(
        packet.text.len()
            + packet
                .citations
                .iter()
                .map(|c| c.excerpt.len())
                .sum::<usize>()
            <= MAX_PACKET_BYTES,
        "reviewed context budget exceeded"
    );
    let sources = validate_fresh(runner, &packet)?;
    Ok((packet, sources))
}
