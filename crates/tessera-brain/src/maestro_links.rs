//! Local link/observation values. Linked work never becomes a dispatch receipt.
use crate::maestro::{self, Discovery, Issue, Settings};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const NEW_LINKS_ENABLED: bool = true;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinkRequest {
    pub operation_id: String,
    pub goal_id: String,
    pub selection_guard: String,
    pub project_id: String,
    pub project_name: String,
    pub repo: String,
    pub issue_number: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnlinkRequest {
    pub operation_id: String,
    pub goal_id: String,
    pub expected_link_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Link {
    pub id: String,
    pub goal_id: String,
    pub instance: Value,
    pub project_id: String,
    pub project_name: String,
    #[serde(default)]
    pub project_url: Option<String>,
    pub repo: String,
    pub issue_number: u64,
    pub active: bool,
    pub created_at: String,
    pub last_seen: Option<String>,
    pub last_remote: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub observation_ids: Vec<String>,
    pub latest: Option<Observation>,
    #[serde(default)]
    pub transition: u64,
    #[serde(default)]
    pub latest_semantic: Option<String>,
    #[serde(default)]
    pub attention_generation: u64,
}
pub use tessera_core::maestro_observation::Observation;
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Journal {
    pub links: BTreeMap<String, Link>,
    pub operations: BTreeMap<String, Value>,
    pub observations: BTreeMap<String, Observation>,
    #[serde(default)]
    pub recovery_required: bool,
}
impl Journal {
    pub fn active(&self, goal: &str) -> Option<&Link> {
        self.links.values().find(|l| l.goal_id == goal && l.active)
    }
    pub fn guard_config(&self, settings: Option<&Settings>) -> Result<()> {
        ensure!(!self.links.values().any(|l|l.active&&Some(l.instance.clone())!=settings.map(Settings::identity)),"Maestro configuration is bound to linked work; unlink it before changing the instance or origin");
        Ok(())
    }
}
pub fn selected<'a>(
    request: &LinkRequest,
    discovery: &'a Discovery,
) -> Result<(&'a maestro::Project, &'a Issue)> {
    let p = discovery
        .projects
        .iter()
        .find(|p| {
            p.project_id == request.project_id
                && p.name == request.project_name
                && p.repo == request.repo
        })
        .context("Maestro project identity changed")?;
    ensure!(!p.stale, "Maestro project snapshot is stale");
    let issue = p
        .issues
        .iter()
        .find(|i| i.number == request.issue_number)
        .context("Issue is not represented in the current Maestro snapshot")?;
    ensure!(
        maestro::selection_guard(&discovery.instance, p, issue) == request.selection_guard,
        "Maestro selection changed; review the current issue again"
    );
    Ok((p, issue))
}
pub fn choices(discovery: &Discovery) -> Value {
    json!({"schema":"tessera-maestro-observation/v1","instance":discovery.instance,"observed_at":discovery.observed_at,"refreshed_at":discovery.refreshed_at,"unsupported_projects":discovery.unsupported_projects,"controls_enabled":false,"projects":discovery.projects.iter().map(|p|json!({"project_id":p.project_id,"name":p.name,"repo":p.repo,"paused":p.paused,"dashboard_url":p.dashboard_url,"stale":p.stale,"issues":p.issues.iter().map(|i|json!({"issue":i,"selection_guard":maestro::selection_guard(&discovery.instance,p,i)})).collect::<Vec<_>>()})).collect::<Vec<_>>()})
}
/// Canonical link receipts exclude polling state and transient provider prose.
pub(crate) fn link_record(link: &Link) -> Value {
    json!({
        "id": link.id,
        "goal_id": link.goal_id,
        "instance": link.instance,
        "project_id": link.project_id,
        "project_name": link.project_name,
        "repo": link.repo,
        "issue_number": link.issue_number,
        "active": link.active,
        "created_at": link.created_at,
        "status": if link.active { "linked" } else { "unlinked" },
    })
}

pub(crate) fn observation_body(o: &Observation, link: &Link) -> String {
    let mut body=format!("# Observed Maestro work — {} #{}\n\nObserved at {}. Verification: unverified.\n\nProject: {}. This is linked external work, not a Tessera dispatch or completed goal.\n",link.repo,link.issue_number,o.observed_at,link.project_name);
    if let Some(url) = &o.issue.url {
        body.push_str(&format!("\n[Original issue](<{url}>)\n"));
    }
    for a in &o.issue.attempts {
        body.push_str(&format!(
            "\n- Attempt {} / generation {:?}: {}.\n",
            a.slot, a.generation, a.status
        ));
        if let Some(url) = &a.pr_url {
            body.push_str(&format!("  [Original PR](<{url}>)\n"));
        }
    }
    body
}

/// Canonical observations omit transient provider explanations and approval prose.
/// These remain visible in the operational projection, never treated as results.
pub(crate) fn canonical_issue(issue: &Issue) -> Issue {
    let mut value = issue.clone();
    for attempt in &mut value.attempts {
        attempt.reason.clear();
    }
    for approval in &mut value.approvals {
        approval.summary.clear();
        approval.dashboard_url = None;
    }
    value
}
