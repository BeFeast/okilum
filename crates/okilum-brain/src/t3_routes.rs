//! Explicit future-only T3 target selection. Durable state belongs to Runner.
use crate::application::T3Settings;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
pub const SCHEMA: &str = "okilum-t3-target/v1";

pub(crate) fn digest<T: Serialize>(value: &T) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("serializable route value"))
    )
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guard {
    pub revision: String,
    pub active_generation: Option<String>,
    pub inventory_digest: String,
    pub candidate_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blocker {
    pub code: String,
    pub operation_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Association {
    KnownRoute {
        generation_id: String,
    },
    HistoricalTerminalUnroutable {
        envelope_digest: String,
        receipt_digest: String,
        result_digest: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssociationView {
    pub operation_id: String,
    pub association: Association,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofSummary {
    pub operation_id: String,
    pub artifact_sha256: String,
    pub envelope_sha256: String,
    pub provenance: String,
    pub source_locator: String,
}
impl From<&crate::t3_compat::VerifiedProof> for ProofSummary {
    fn from(proof: &crate::t3_compat::VerifiedProof) -> Self {
        Self {
            operation_id: proof.operation_id.clone(),
            artifact_sha256: proof.artifact_sha256.clone(),
            envelope_sha256: proof.envelope_sha256.clone(),
            provenance: proof.provenance.clone(),
            source_locator: proof.source_locator.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    #[serde(default)]
    pub compatibility_manifest: Option<crate::t3_compat::Manifest>,
    #[serde(default)]
    pub compatibility_proofs: Vec<ProofSummary>,
    pub schema: String,
    pub candidate: T3Settings,
    pub guard: Guard,
    pub previous: Option<T3Settings>,
    pub environment_changed: bool,
    pub project_changed: bool,
    pub associations: Vec<AssociationView>,
    pub blockers: Vec<Blocker>,
    pub ready: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptRequest {
    pub operation_id: String,
    pub review: Review,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: String,
    pub operation_id: String,
    pub generation_id: String,
    pub previous_generation: Option<String>,
    pub future_only: bool,
    pub request_digest: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Generation {
    pub id: String,
    pub settings: T3Settings,
    pub provenance: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transition {
    pub request: AdoptRequest,
    pub receipt: Receipt,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Journal {
    #[serde(default)]
    pub compatibility_proofs: BTreeMap<String, crate::t3_compat::VerifiedProof>,
    pub active: Option<String>,
    pub generations: BTreeMap<String, Generation>,
    pub associations: BTreeMap<String, Association>,
    pub transitions: BTreeMap<String, Transition>,
    pub baseline_settings: Option<T3Settings>,
}
impl Journal {
    pub fn active_settings(&self) -> Option<&T3Settings> {
        self.active
            .as_ref()
            .and_then(|id| self.generations.get(id))
            .map(|g| &g.settings)
    }
    pub fn known(&self, operation: &str) -> Result<Option<&T3Settings>> {
        match self.associations.get(operation) {
            Some(Association::KnownRoute { generation_id }) => Ok(Some(
                &self
                    .generations
                    .get(generation_id)
                    .ok_or_else(|| anyhow::anyhow!("t3_generation_missing"))?
                    .settings,
            )),
            Some(Association::HistoricalTerminalUnroutable { .. }) => {
                anyhow::bail!("historical_operation_nonreplayable")
            }
            None if self.active.is_some() => anyhow::bail!("t3_operation_generation_missing"),
            None => Ok(None),
        }
    }
}
pub(crate) fn generation(settings: &T3Settings, provenance: &str) -> Generation {
    Generation {
        id: format!("t3-{}", digest(settings)),
        settings: settings.clone(),
        provenance: provenance.into(),
    }
}
pub(crate) fn validate(settings: &T3Settings) -> Result<()> {
    let url = reqwest::Url::parse(&settings.base_url)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid_t3_url"
    );
    ensure!(
        [
            &settings.environment_id,
            &settings.project_id,
            &settings.model_instance_id,
            &settings.model,
            &settings.token_env
        ]
        .iter()
        .all(|s| !s.trim().is_empty()),
        "invalid_t3_settings"
    );
    ensure!(
        [
            "approval-required",
            "auto-accept-edits",
            "auto",
            "full-access"
        ]
        .contains(&settings.runtime_mode.as_str())
            && ["default", "plan"].contains(&settings.interaction_mode.as_str()),
        "invalid_t3_modes"
    );
    Ok(())
}
/// Only ticket/config/project discovery. Never creates a thread or provider turn.
pub(crate) fn discover(candidate: &T3Settings) -> Result<Value> {
    validate(candidate)?;
    let token = crate::application::credential(&candidate.token_env)?;
    crate::t3::discover(&candidate.base_url, &token)
}
pub(crate) fn discovery_blocker(
    candidate: &T3Settings,
    observed: &Result<Value>,
) -> Option<&'static str> {
    match observed {
        Err(_) => Some("candidate_discovery_unavailable"),
        Ok(value) if value["environment_id"].as_str() != Some(&candidate.environment_id) => {
            Some("candidate_environment_mismatch")
        }
        Ok(value)
            if !value["projects"].as_array().is_some_and(|p| {
                p.iter()
                    .any(|p| p["id"].as_str() == Some(&candidate.project_id))
            }) =>
        {
            Some("candidate_project_missing")
        }
        _ => None,
    }
}
/// Standalone local eligibility inventory. Never opens Runner or resolves credentials.
pub fn inspect(operational: &Path, brain: &Path) -> Result<Value> {
    inspect_with_manifest(operational, brain, None)
}
pub fn inspect_with_manifest(
    operational: &Path,
    brain: &Path,
    manifest: Option<&crate::t3_compat::Manifest>,
) -> Result<Value> {
    let bytes = std::fs::read(operational.join("state.json"))?;
    let settings = std::fs::read(crate::settings::path(operational))?;
    let config = crate::settings::decode_config(&settings)?;
    let report =
        crate::runtime::t3_routes::inspect_bytes(&bytes, &config, operational, brain, manifest)?;
    ensure!(
        crate::runtime::t3_routes::inspect_bytes(&bytes, &config, operational, brain, manifest)?
            == report,
        "inventory_changed"
    );
    ensure!(
        std::fs::read(operational.join("state.json"))? == bytes
            && std::fs::read(crate::settings::path(operational))? == settings,
        "inventory_changed"
    );
    Ok(report)
}
pub(crate) fn view(journal: Option<&Journal>, fallback: Option<&T3Settings>) -> Value {
    json!({"schema":SCHEMA,"active_generation":journal.and_then(|j|j.active.as_ref()),
        "active":journal.and_then(Journal::active_settings).or(fallback),
        "generations":journal.map(|j|j.generations.values().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "historical_terminal_unroutable_count":journal.map(|j|j.associations.values().filter(|a|matches!(a, Association::HistoricalTerminalUnroutable { .. })).count()).unwrap_or(0),
        "future_only":true})
}

pub(crate) fn adapter(settings: &T3Settings, operational: &Path) -> Result<crate::t3::T3Adapter> {
    validate(settings)?;
    crate::t3::T3Adapter::new(crate::t3::T3Config {
        base_url: settings.base_url.clone(),
        bearer_token: crate::application::credential(&settings.token_env)?,
        environment_id: settings.environment_id.clone(),
        project_id: settings.project_id.clone(),
        model_instance_id: settings.model_instance_id.clone(),
        model: settings.model.clone(),
        runtime_mode: settings.runtime_mode.clone(),
        interaction_mode: settings.interaction_mode.clone(),
        timeout: std::time::Duration::from_secs(30),
        receipt_dir: operational.join("t3-receipts"),
    })
}
