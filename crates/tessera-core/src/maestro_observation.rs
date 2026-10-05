//! Saved Maestro observation payloads shared with the read-only shell.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attempt {
    pub slot: String,
    pub generation: Option<u64>,
    pub started_at: Option<String>,
    pub status: String,
    pub live: bool,
    pub needs_attention: bool,
    pub reason: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Approval {
    pub id: String,
    pub action: String,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub dashboard_url: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub url: Option<String>,
    pub attempts: Vec<Attempt>,
    pub approvals: Vec<Approval>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub id: String,
    pub link_id: String,
    pub goal_id: String,
    pub observed_at: String,
    pub remote_at: String,
    pub paused: bool,
    pub issue: Issue,
    pub verification: String,
}
