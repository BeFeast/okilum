//! Runner-owned T3 selection transactions and all-goal terminal admission.
use super::*;
use crate::{
    application::{Application, ApplicationConfig, T3Settings},
    t3_routes as api,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::path::Path;

struct Inventory {
    compatibility_proofs: BTreeMap<String, crate::t3_compat::VerifiedProof>,
    associations: BTreeMap<String, api::Association>,
    generations: BTreeMap<String, api::Generation>,
    blockers: Vec<api::Blocker>,
    evidence: Vec<String>,
}
impl Inventory {
    fn block(&mut self, code: &str, operation: Option<&str>) {
        self.blockers.push(api::Blocker {
            code: code.into(),
            operation_id: operation.map(str::to_owned),
        });
    }
    fn digest(&self) -> String {
        api::digest(&(
            &self.associations,
            &self.compatibility_proofs,
            &self.generations,
            &self.evidence,
            &self.blockers,
        ))
    }
}
fn record<T: DeserializeOwned>(
    brain: &Path,
    records: &str,
    kind: &str,
    id: &str,
) -> Result<(T, String)> {
    uuid(id)?;
    let bytes = fs::read(brain.join(records).join(format!("{kind}-{id}.md")))?;
    let text = std::str::from_utf8(&bytes)?;
    let mut lines = text.lines();
    ensure!(lines.next() == Some("---"), "invalid source frontmatter");
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    ensure!(closed, "invalid source frontmatter");
    Ok((serde_yaml::from_str(&yaml)?, api::digest(&bytes)))
}
fn provider_core(outcome: &Outcome) -> Value {
    json!({"outcome":outcome.outcome,"summary":outcome.summary,"sources":outcome.sources,
        "evidence":outcome.evidence.iter().filter(|e|e.kind!="human_acceptance").map(|e| {
            let mut value=serde_json::to_value(e).expect("evidence"); value.as_object_mut().unwrap().remove("status"); value
        }).collect::<Vec<_>>()})
}
fn settled(
    state: &State,
    slot: &GoalState,
    dispatch: &Dispatch,
    brain: &Path,
    operational: &Path,
    human: &BTreeMap<String, HumanAcceptance>,
    compatibility: Option<&crate::t3_compat::VerifiedProof>,
) -> Result<(String, String, String)> {
    ensure!(
        ["outcome_ready", "cancelled"].contains(&dispatch.phase.as_str()),
        "provider_work_unsettled"
    );
    let env = &dispatch.envelope;
    ensure!(
        env.schema == SCHEMA
            && env.packet.id == env.context_id
            && env.packet.goal_id == env.goal_id
            && env.packet.stage_id == env.stage_id,
        "envelope_identity_mismatch"
    );
    let environment = env
        .target
        .get("environment_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("environment_missing")?;
    ensure!(
        env.target
            .get("project_id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        "project_missing"
    );
    valid_time(
        env.target
            .get("created_at")
            .and_then(Value::as_str)
            .context("created_at_missing")?,
    )?;
    let binding = dispatch
        .binding
        .as_ref()
        .context("terminal_binding_missing")?;
    ensure!(
        binding.engine == "t3" && binding.instance_id == environment,
        "binding_identity_mismatch"
    );
    uuid(&env.operation_id)?;
    let path = operational
        .join("t3-receipts")
        .join(format!("{}.json", env.operation_id));
    let event =
        crate::t3::read_terminal_receipt(&path, env, Some(binding), environment, compatibility)?
            .context("terminal_receipt_missing")?;
    let EventPayload::Outcome(outcome) = &event.payload else {
        anyhow::bail!("terminal_outcome_missing")
    };
    ensure!(
        ["succeeded", "failed", "cancelled"].contains(&outcome.outcome.as_str()),
        "terminal_outcome_unknown"
    );
    ensure!(
        slot.events
            .iter()
            .any(|entry| entry.projected && entry.reason == "accepted" && entry.event == event),
        "terminal_projection_missing"
    );
    ensure!(
        !slot
            .events
            .iter()
            .any(|entry| entry.event.operation_id == env.operation_id && entry.reason == "unbound"),
        "correlation_uncertain"
    );
    let sequence = event.sequence.context("terminal_sequence_missing")?;
    ensure!(
        dispatch
            .sequences
            .get(&event.stream_id)
            .is_some_and(|s| *s >= sequence),
        "terminal_sequence_unacknowledged"
    );
    if dispatch.sequences.get(&event.stream_id) == Some(&sequence) {
        ensure!(
            event.cursor.as_ref() == dispatch.cursors.get(&event.stream_id),
            "terminal_cursor_unacknowledged"
        );
    }
    let (stage, stage_digest) = record::<Stage>(brain, &state.records_dir, "stage", &env.stage_id)?;
    ensure!(
        stage.id == env.stage_id
            && stage.context_id == env.context_id
            && stage.goal_id == env.goal_id
            && stage.engine == "t3"
            && ["completed", "outcome_ready", "cancelled"].contains(&stage.status.as_str()),
        "terminal_stage_missing"
    );
    ensure!(stage.result_ids.len() == 1, "terminal_result_ambiguous");
    let (result, result_file_digest) =
        record::<ResultRecord>(brain, &state.records_dir, "result", &stage.result_ids[0])?;
    ensure!(
        result.id == stage.result_ids[0]
            && result.goal_id == env.goal_id
            && result.stage_id == env.stage_id
            && result.operation_id == env.operation_id
            && result.engine_ref == *binding
            && result.received_at == event.observed_at,
        "terminal_result_identity_mismatch"
    );
    // Local evidence/criteria review changes status, verification and criterion
    // evaluations. It cannot rewrite provider summary/sources/evidence identity.
    ensure!(
        provider_core(&result.outcome) == provider_core(outcome),
        "terminal_result_core_mismatch"
    );
    ensure!(
        ["unverified", "verified", "partial", "failed"]
            .contains(&result.outcome.verification.as_str()),
        "unsupported_review_transformation"
    );
    for old in &outcome.evidence {
        let current = result
            .outcome
            .evidence
            .iter()
            .find(|e| e.id == old.id)
            .context("provider_evidence_missing")?;
        ensure!(
            current.status == old.status
                || ["passed", "failed", "unverified"].contains(&current.status.as_str()),
            "unsupported_review_transformation"
        );
    }
    let retained = if slot
        .dispatch
        .as_ref()
        .is_some_and(|d| d.envelope.operation_id == env.operation_id)
    {
        slot.retained_goal.as_ref()
    } else {
        slot.previous_stages
            .values()
            .find(|s| s.dispatch.envelope.operation_id == env.operation_id)
            .and_then(|s| s.retained_goal.as_ref())
    };
    let statuses_changed = outcome.evidence.iter().any(|old| {
        result
            .outcome
            .evidence
            .iter()
            .any(|new| old.id == new.id && old.status != new.status)
    });
    if statuses_changed {
        let retained = retained.context("retained_goal_missing")?;
        let (mapping, _) = parse_document(retained)?;
        let goal: Goal = serde_yaml::from_value(serde_yaml::Value::Mapping(mapping))?;
        // A later review replaces the criterion's evaluation but does not reset
        // earlier selected evidence statuses. Current schema cannot reconstruct
        // those superseded selections; require a valid owning review, not a
        // nonexistent append-only per-evidence review history.
        ensure!(
            result
                .outcome
                .criterion_evaluations
                .iter()
                .any(|evaluation| goal
                    .criteria
                    .iter()
                    .any(|c| c.id == evaluation.criterion_id && !c.requires_human)
                    && evaluation.goal_revision == retained.revision
                    && ["passed", "failed", "unverified"].contains(&evaluation.status.as_str())
                    && !evaluation.evaluated_by.trim().is_empty()
                    && valid_time(&evaluation.evaluated_at).is_ok()
                    && !evaluation.evidence_ids.is_empty()
                    && evaluation.evidence_ids.iter().all(|id| result
                        .outcome
                        .evidence
                        .iter()
                        .any(|e| e.id == *id && e.kind != "human_acceptance"))),
            "unsupported_review_transformation"
        );
    }
    for evaluation in &result.outcome.criterion_evaluations {
        if outcome.criterion_evaluations.contains(evaluation) {
            continue;
        }
        let retained = retained.context("retained_goal_missing")?;
        let (mapping, _) = parse_document(retained)?;
        let goal: Goal = serde_yaml::from_value(serde_yaml::Value::Mapping(mapping))?;
        let criterion = goal
            .criteria
            .iter()
            .find(|c| c.id == evaluation.criterion_id)
            .context("review_criterion_unknown")?;
        ensure!(
            evaluation.goal_revision == retained.revision
                && ["passed", "failed", "unverified"].contains(&evaluation.status.as_str())
                && !evaluation.evaluated_by.trim().is_empty()
                && valid_time(&evaluation.evaluated_at).is_ok()
                && !evaluation.evidence_ids.is_empty()
                && evaluation.evidence_ids.iter().all(|id| result
                    .outcome
                    .evidence
                    .iter()
                    .any(|e| e.id == *id
                        && (e.kind == "human_acceptance") == criterion.requires_human)),
            "unsupported_review_transformation"
        );
        if criterion.requires_human {
            let acceptance = human
                .get(&criterion.id)
                .context("human_evidence_unproven")?;
            ensure!(
                evaluation.evaluated_by == acceptance.actor
                    && evaluation.evaluated_at == acceptance.observed_at
                    && evaluation.evidence_ids == [acceptance.id.clone()],
                "human_evidence_unproven"
            );
        }
    }
    for original in &outcome.criterion_evaluations {
        ensure!(
            result
                .outcome
                .criterion_evaluations
                .iter()
                .any(|current| current.criterion_id == original.criterion_id),
            "unsupported_review_transformation"
        );
    }
    for evidence in result
        .outcome
        .evidence
        .iter()
        .filter(|e| e.kind == "human_acceptance")
    {
        let acceptance = human
            .values()
            .find(|h| h.id == evidence.id)
            .context("human_evidence_unproven")?;
        let (saved, _) =
            record::<HumanAcceptance>(brain, &state.records_dir, "evidence", &acceptance.id)?;
        let relative = format!("{}/evidence-{}.md", state.records_dir, acceptance.id);
        let bytes = fs::read(brain.join(&relative))?;
        let revision = format!("sha256:{:x}", Sha256::digest(&bytes));
        ensure!(
            serde_json::to_value(&saved)? == serde_json::to_value(acceptance)?
                && evidence.source.uri == format!("brain://{}/{}", state.brain_id, relative)
                && evidence.source.revision.as_ref() == Some(&revision)
                && evidence.source.locator.is_none()
                && evidence.observed_at == acceptance.observed_at
                && evidence.description == acceptance.definition.description
                && evidence.status == "passed",
            "human_evidence_unproven"
        );
    }
    let receipt_digest = api::digest(&fs::read(path)?);
    let core_digest = api::digest(&(
        result.id,
        result.operation_id,
        result.engine_ref,
        provider_core(&result.outcome),
    ));
    Ok((
        receipt_digest,
        core_digest,
        api::digest(&(stage_digest, result_file_digest)),
    ))
}
fn inventory(
    state: &State,
    config: &ApplicationConfig,
    operational: &Path,
    brain: &Path,
    manifest: Option<&crate::t3_compat::Manifest>,
) -> Inventory {
    let mut result = Inventory {
        compatibility_proofs: state
            .t3_routes
            .as_ref()
            .map(|j| j.compatibility_proofs.clone())
            .unwrap_or_default(),
        associations: BTreeMap::new(),
        generations: state
            .t3_routes
            .as_ref()
            .map(|j| j.generations.clone())
            .unwrap_or_default(),
        blockers: vec![],
        evidence: vec![],
    };
    let mut supplied = BTreeMap::new();
    if let Some(manifest) = manifest {
        if manifest.entries.len() > 256 {
            result.block("compatibility_manifest_limit", None);
            return result;
        }
        for entry in &manifest.entries {
            if supplied.insert(entry.operation_id.clone(), entry).is_some() {
                result.block(
                    "compatibility_operation_duplicated",
                    Some(&entry.operation_id),
                );
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    if !state.pending_writes.is_empty() {
        result.block("source_projection_pending", None);
    }
    for slot in std::iter::once(&state.current).chain(state.other_goals.values()) {
        if slot
            .application
            .get("conversations")
            .and_then(Value::as_object)
            .is_some_and(|v| v.values().any(|c| c["status"] == "running"))
        {
            result.block("running_conversation", None);
        }
        for (dispatch, human) in slot
            .dispatch
            .iter()
            .map(|d| (d, &slot.human_acceptances))
            .chain(
                slot.previous_stages
                    .values()
                    .map(|s| (&s.dispatch, &s.human_acceptances)),
            )
        {
            let operation = &dispatch.envelope.operation_id;
            let stage = record::<Stage>(
                brain,
                &state.records_dir,
                "stage",
                &dispatch.envelope.stage_id,
            );
            let t3_identity = dispatch.binding.as_ref().is_some_and(|b| b.engine == "t3")
                || dispatch.envelope.target.contains_key("environment_id")
                || dispatch.envelope.target.contains_key("project_id");
            if !t3_identity && stage.as_ref().is_ok_and(|(s, _)| s.engine != "t3") {
                if !["outcome_ready", "cancelled"].contains(&dispatch.phase.as_str()) {
                    result.block("provider_work_unsettled", Some(operation));
                }
                continue;
            }
            seen.insert(operation.clone());
            if let Some(entry) = supplied.get(operation) {
                let checked = (|| -> Result<crate::t3_compat::VerifiedProof> {
                    uuid(operation)?;
                    let receipt: Value = serde_json::from_slice(&fs::read(
                        operational
                            .join("t3-receipts")
                            .join(format!("{operation}.json")),
                    )?)?;
                    crate::t3_compat::verify(
                        entry,
                        &dispatch.envelope,
                        receipt["fingerprint"]
                            .as_str()
                            .context("receipt fingerprint missing")?,
                    )
                })();
                match checked {
                    Ok(proof) => {
                        if result
                            .compatibility_proofs
                            .get(operation)
                            .is_some_and(|old| old != &proof)
                        {
                            result.block("compatibility_proof_conflict", Some(operation));
                            continue;
                        }
                        result.compatibility_proofs.insert(operation.clone(), proof);
                    }
                    Err(_) => {
                        result.block("compatibility_proof_invalid", Some(operation));
                        continue;
                    }
                }
            }
            let proof = settled(
                state,
                slot,
                dispatch,
                brain,
                operational,
                human,
                result.compatibility_proofs.get(operation),
            );
            let (receipt_digest, result_digest, projection_digest) = match proof {
                Ok(proof) => proof,
                Err(error) => {
                    let message = error.to_string();
                    // Only predefined codes escape; source/provider errors may include content.
                    let code = if [
                        "provider_work_unsettled",
                        "terminal_receipt_missing",
                        "t3_receipt_corrupt",
                        "t3_receipt_identity_conflict",
                        "terminal_projection_missing",
                        "correlation_uncertain",
                        "terminal_sequence_unacknowledged",
                        "terminal_cursor_unacknowledged",
                        "terminal_result_core_mismatch",
                        "terminal_result_identity_mismatch",
                    ]
                    .contains(&message.as_str())
                    {
                        message.as_str()
                    } else {
                        "terminal_evidence_unproven"
                    };
                    result.block(code, Some(operation));
                    continue;
                }
            };
            result.evidence.push(projection_digest);
            let envelope_digest = api::digest(&dispatch.envelope);
            let existing = state
                .t3_routes
                .as_ref()
                .and_then(|j| j.associations.get(operation));
            let association = if let Some(existing) = existing {
                match existing {
                    api::Association::HistoricalTerminalUnroutable {
                        envelope_digest: e,
                        receipt_digest: r,
                        result_digest: p,
                    } => {
                        if (e, r, p) != (&envelope_digest, &receipt_digest, &result_digest) {
                            result.block("historical_proof_changed", Some(operation));
                            continue;
                        }
                        existing.clone()
                    }
                    api::Association::KnownRoute { generation_id } => {
                        let Some(generation) = result.generations.get(generation_id) else {
                            result.block("generation_missing", Some(operation));
                            continue;
                        };
                        if !matches_target(&generation.settings, dispatch) {
                            result.block("generation_identity_mismatch", Some(operation));
                            continue;
                        }
                        existing.clone()
                    }
                }
            } else if state.t3_routes.is_some() {
                result.block("operation_generation_missing", Some(operation));
                continue;
            } else {
                let pin = &slot.application["provider_identity"]["t3"];
                if pin.is_null() {
                    api::Association::HistoricalTerminalUnroutable {
                        envelope_digest,
                        receipt_digest,
                        result_digest,
                    }
                } else {
                    let mut fields = pin.clone();
                    let Some(saved) = config.t3.as_ref() else {
                        result.block("legacy_credential_reference_missing", Some(operation));
                        continue;
                    };
                    fields["token_env"] = json!(saved.token_env);
                    let Ok(settings) = serde_json::from_value::<T3Settings>(fields) else {
                        result.block("legacy_origin_unproven", Some(operation));
                        continue;
                    };
                    if api::validate(&settings).is_err() || !matches_target(&settings, dispatch) {
                        result.block("legacy_identity_mismatch", Some(operation));
                        continue;
                    }
                    let generation = api::generation(&settings, "retained_application_routing_pin");
                    let association = api::Association::KnownRoute {
                        generation_id: generation.id.clone(),
                    };
                    result.generations.insert(generation.id.clone(), generation);
                    association
                }
            };
            if result
                .associations
                .insert(operation.clone(), association)
                .is_some()
            {
                result.block("operation_identity_duplicated", Some(operation));
            }
        }
    }
    for operation in supplied.keys() {
        if !seen.contains(operation) {
            result.block("compatibility_operation_missing", Some(operation));
        }
    }

    result
}
fn matches_target(settings: &T3Settings, dispatch: &Dispatch) -> bool {
    dispatch
        .envelope
        .target
        .get("environment_id")
        .and_then(Value::as_str)
        == Some(&settings.environment_id)
        && dispatch
            .envelope
            .target
            .get("project_id")
            .and_then(Value::as_str)
            == Some(&settings.project_id)
        && dispatch
            .binding
            .as_ref()
            .is_some_and(|b| b.instance_id == settings.environment_id)
}
pub(crate) fn inspect_bytes(
    bytes: &[u8],
    config: &ApplicationConfig,
    operational: &Path,
    brain: &Path,
    manifest: Option<&crate::t3_compat::Manifest>,
) -> Result<Value> {
    let state = decode_state(bytes)?;
    let checked = inventory(&state, config, operational, brain, manifest);
    Ok(
        json!({"schema":"tessera-t3-inventory/v1","read_only":true,"eligible":checked.blockers.is_empty(),
        "blockers":checked.blockers,"associations":checked.associations,"compatibility_proofs":checked.compatibility_proofs.values().map(api::ProofSummary::from).collect::<Vec<_>>(),"inventory_digest":checked.digest()}),
    )
}
impl Runner {
    pub(crate) fn t3_has_generations(&self) -> bool {
        self.state.t3_routes.is_some()
    }

    pub(crate) fn t3_route_view(&self, config: &ApplicationConfig) -> Value {
        api::view(self.state.t3_routes.as_ref(), config.t3.as_ref())
    }
    pub(crate) fn t3_future_settings(&self, fallback: Option<&T3Settings>) -> Option<T3Settings> {
        self.state
            .t3_routes
            .as_ref()
            .and_then(|j| j.active_settings())
            .or(fallback)
            .cloned()
    }
    pub(crate) fn t3_operation_proof(
        &self,
        operation: &str,
    ) -> Option<crate::t3_compat::VerifiedProof> {
        self.state
            .t3_routes
            .as_ref()
            .and_then(|j| j.compatibility_proofs.get(operation))
            .cloned()
    }
    pub(crate) fn t3_operation_settings(&self, operation: &str) -> Result<Option<T3Settings>> {
        match &self.state.t3_routes {
            Some(j) => Ok(j.known(operation)?.cloned()),
            None => Ok(None),
        }
    }
    pub(crate) fn t3_settings_guard(&self, candidate: Option<&T3Settings>) -> Result<()> {
        if let Some(j) = &self.state.t3_routes {
            ensure!(
                candidate == j.baseline_settings.as_ref(),
                "T3 changes require explicit future-target adoption after generations are enabled"
            );
        }
        Ok(())
    }
    pub(crate) fn t3_pin_operation(
        &mut self,
        operation: &str,
        target: &BTreeMap<String, Value>,
    ) -> Result<()> {
        let Some(j) = &mut self.state.t3_routes else {
            return Ok(());
        };
        let active = j.active.clone().context("active_generation_missing")?;
        let settings = &j
            .generations
            .get(&active)
            .context("active_generation_missing")?
            .settings;
        ensure!(
            target.get("environment_id").and_then(Value::as_str) == Some(&settings.environment_id)
                && target.get("project_id").and_then(Value::as_str) == Some(&settings.project_id),
            "future_target_mismatch"
        );
        ensure!(
            !j.associations.contains_key(operation),
            "operation_generation_already_exists"
        );
        j.associations.insert(
            operation.into(),
            api::Association::KnownRoute {
                generation_id: active,
            },
        );
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn t3_review(
        &self,
        config: &ApplicationConfig,
        candidate: T3Settings,
        observed: &Result<Value>,
    ) -> api::Review {
        self.t3_review_with_manifest(config, candidate, observed, None)
    }
    pub(crate) fn t3_review_with_manifest(
        &self,
        config: &ApplicationConfig,
        candidate: T3Settings,
        observed: &Result<Value>,
        manifest: Option<crate::t3_compat::Manifest>,
    ) -> api::Review {
        let mut checked = inventory(
            &self.state,
            config,
            &self.state_dir,
            &self.root,
            manifest.as_ref(),
        );
        if self.route_recovery_required {
            checked.block("route_recovery_required", None);
        }
        let inventory_digest = checked.digest();
        if api::validate(&candidate).is_err() {
            checked.block("invalid_candidate", None);
        }
        if let Some(code) = api::discovery_blocker(&candidate, observed) {
            checked.block(code, None);
        }
        let previous = self.t3_future_settings(config.t3.as_ref());
        let mut normalized = self.state.clone();
        if let Some(primary) = normalized.primary_goal_id.clone() {
            let _ = normalized.route(&primary);
        }
        api::Review {
            compatibility_manifest: manifest,
            compatibility_proofs: checked
                .compatibility_proofs
                .values()
                .map(api::ProofSummary::from)
                .collect(),
            schema: api::SCHEMA.into(),
            guard: api::Guard {
                revision: api::digest(&(&normalized, Application::provider_identity(config))),
                active_generation: self.state.t3_routes.as_ref().and_then(|j| j.active.clone()),
                inventory_digest,
                candidate_digest: api::digest(&candidate),
            },
            environment_changed: previous
                .as_ref()
                .is_some_and(|p| p.environment_id != candidate.environment_id),
            project_changed: previous
                .as_ref()
                .is_some_and(|p| p.project_id != candidate.project_id),
            candidate,
            previous,
            ready: checked.blockers.is_empty(),
            blockers: checked.blockers,
            associations: checked
                .associations
                .into_iter()
                .map(|(operation_id, association)| api::AssociationView {
                    operation_id,
                    association,
                })
                .collect(),
        }
    }
    pub(crate) fn t3_transition_receipt(
        &self,
        request: &api::AdoptRequest,
    ) -> Result<Option<api::Receipt>> {
        let durable = decode_state(&fs::read(self.state_dir.join("state.json"))?)?;
        if let Some(old) = durable
            .t3_routes
            .as_ref()
            .and_then(|j| j.transitions.get(&request.operation_id))
        {
            ensure!(old.request == *request, "transition_operation_conflict");
            return Ok(Some(old.receipt.clone()));
        }
        Ok(None)
    }
    pub(crate) fn t3_adopt(
        &mut self,
        config: &ApplicationConfig,
        request: api::AdoptRequest,
        observed: &Result<Value>,
    ) -> Result<api::Receipt> {
        if let Some(receipt) = self.t3_transition_receipt(&request)? {
            return Ok(receipt);
        }
        ensure!(
            self.managed,
            "future target adoption requires managed workspace"
        );
        uuid(&request.operation_id)?;
        let current = self.t3_review_with_manifest(
            config,
            request.review.candidate.clone(),
            observed,
            request.review.compatibility_manifest.clone(),
        );
        ensure!(
            current.ready && request.review == current,
            "prepared_selection_stale_or_blocked"
        );
        let checked = inventory(
            &self.state,
            config,
            &self.state_dir,
            &self.root,
            request.review.compatibility_manifest.as_ref(),
        );
        ensure!(
            checked.blockers.is_empty()
                && checked.digest() == request.review.guard.inventory_digest,
            "inventory_changed"
        );
        let selected =
            api::generation(&request.review.candidate, "explicit_future_target_adoption");
        let receipt = api::Receipt {
            schema: "tessera-t3-target-receipt/v1".into(),
            operation_id: request.operation_id.clone(),
            generation_id: selected.id.clone(),
            previous_generation: request.review.guard.active_generation.clone(),
            future_only: true,
            request_digest: api::digest(&request),
        };
        let before = self.state.clone();
        let attempt = self.mutation(|r| {
            let j = r.state.t3_routes.get_or_insert_with(|| api::Journal {
                baseline_settings: config.t3.clone(),
                ..Default::default()
            });
            for (id, proof) in checked.compatibility_proofs {
                j.compatibility_proofs.entry(id).or_insert(proof);
            }
            for (id, generation) in checked.generations {
                j.generations.entry(id).or_insert(generation);
            }
            for (id, association) in checked.associations {
                j.associations.entry(id).or_insert(association);
            }
            j.generations
                .entry(selected.id.clone())
                .or_insert(selected.clone());
            j.active = Some(selected.id.clone());
            j.transitions.insert(
                request.operation_id.clone(),
                api::Transition {
                    request: request.clone(),
                    receipt: receipt.clone(),
                },
            );
            #[cfg(test)]
            if r.t3_transition_fault.get() == Some(1) {
                r.t3_transition_fault.set(None);
                anyhow::bail!("injected transition before commit");
            }
            r.persist()?;
            #[cfg(test)]
            match r.t3_transition_fault.take() {
                Some(2) => anyhow::bail!("injected transition after commit"),
                Some(3) => {
                    fs::rename(
                        r.state_dir.join("state.json"),
                        r.state_dir.join("state.injected"),
                    )?;
                    anyhow::bail!("injected unreadable durable result");
                }
                _ => {}
            }
            Ok(receipt.clone())
        });
        if attempt.is_err() {
            match fs::read(self.state_dir.join("state.json"))
                .ok()
                .and_then(|bytes| decode_state(&bytes).ok())
            {
                Some(durable) => self.state = durable,
                None => {
                    self.state = before;
                    self.route_recovery_required = true;
                }
            }
        }
        attempt
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    const WHEN: &str = "2026-10-03T12:00:00Z";
    fn id(n: u32) -> String {
        format!("01000000-0000-4000-8000-{n:012}")
    }
    fn config(temp: &Path) -> RunnerConfig {
        RunnerConfig {
            brain_id: id(1),
            root: temp.join("brain"),
            operational_dir: temp.join("runtime"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        }
    }
    fn settings(temp: &Path) -> T3Settings {
        T3Settings {
            base_url: "http://127.0.0.1:21100".into(),
            token_env: format!("file:{}", temp.join("token").display()),
            environment_id: "old-environment".into(),
            project_id: "same-project".into(),
            model_instance_id: "fixture-provider".into(),
            model: "fixture-model".into(),
            runtime_mode: "approval-required".into(),
            interaction_mode: "default".into(),
        }
    }
    fn candidate(old: &T3Settings) -> T3Settings {
        T3Settings {
            base_url: "http://127.0.0.1:21101".into(),
            environment_id: "new-environment".into(),
            ..old.clone()
        }
    }
    fn observed(candidate: &T3Settings) -> Result<Value> {
        Ok(
            json!({"environment_id":candidate.environment_id,"projects":[{"id":candidate.project_id}]}),
        )
    }
    fn prepare(r: &mut Runner, settings: &T3Settings, n: u32, previous: Option<String>) {
        let goal = r.state.goal_id.clone().unwrap();
        let stage_id = id(n);
        let context_id = id(n + 1);
        let revision = r.record::<Goal>("goal", &goal).unwrap().1.revision;
        r.prepare_stage(
            Stage {
                id: stage_id.clone(),
                goal_id: goal.clone(),
                engine: "t3".into(),
                status: "ready".into(),
                criterion_ids: vec!["C1".into()],
                context_id: context_id.clone(),
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            ContextPacket {
                id: context_id,
                goal_id: goal,
                stage_id,
                goal_revision: revision,
                goal: "Fixture".into(),
                decisions: vec![],
                constraints: vec![],
                sources: vec![],
                previous_result_id: previous,
                next_step: "Check".into(),
                extra: BTreeMap::new(),
            },
            id(n + 2),
            BTreeMap::from([
                ("environment_id".into(), json!(settings.environment_id)),
                ("project_id".into(), json!(settings.project_id)),
                ("created_at".into(), json!(WHEN)),
            ]),
        )
        .unwrap();
    }
    pub(crate) fn fixture() -> (tempfile::TempDir, Runner, ApplicationConfig, Application) {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        fs::create_dir(temp.path().join("runtime")).unwrap();
        fs::write(temp.path().join("token"), "fixture-only").unwrap();
        let mut r = Runner::open(config(temp.path())).unwrap();
        let cfg = ApplicationConfig {
            actor: "fixture".into(),
            chat: None,
            todoist: None,
            maestro: None,
            t3: Some(settings(temp.path())),
        };
        let (app, _) =
            Application::configure(cfg.clone(), &temp.path().join("runtime"), &mut r).unwrap();
        let goal = Goal {
            id: id(2),
            title: "Fixture".into(),
            status: "draft".into(),
            criteria: vec![
                Criterion {
                    id: "C1".into(),
                    description: "Reviewed evidence".into(),
                    requires_human: false,
                },
                Criterion {
                    id: "C2".into(),
                    description: "Human criterion".into(),
                    requires_human: true,
                },
            ],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        };
        r.create_goal(goal, "# Fixture".into()).unwrap();
        prepare(&mut r, cfg.t3.as_ref().unwrap(), 10, None);
        let envelope = r.state.dispatch.as_ref().unwrap().envelope.clone();
        let binding = EngineRef {
            engine: "t3".into(),
            instance_id: cfg.t3.as_ref().unwrap().environment_id.clone(),
            thread_id: Some(crate::t3::T3Adapter::thread_id(&envelope).unwrap()),
            turn_id: Some("turn-old".into()),
            task_id: None,
        };
        r.apply_start(StartReply::Accepted {
            binding: binding.clone(),
        })
        .unwrap();
        let event = EngineEvent {
            operation_id: envelope.operation_id.clone(),
            engine_ref: binding,
            event_id: "terminal-event".into(),
            stream_id: "stream".into(),
            sequence: Some(1),
            cursor: Some("cursor-1".into()),
            observed_at: WHEN.into(),
            payload: EventPayload::Outcome(Outcome {
                outcome: "succeeded".into(),
                summary: "Terminal, awaiting human evidence review".into(),
                sources: vec![],
                evidence: ["e1", "e2"]
                    .iter()
                    .map(|id| Evidence {
                        id: (*id).into(),
                        kind: "engine_response".into(),
                        source: SourceRef {
                            uri: format!("fixture:{id}"),
                            revision: None,
                            locator: None,
                        },
                        description: "Provider evidence".into(),
                        observed_at: WHEN.into(),
                        status: "unverified".into(),
                    })
                    .collect(),
                verification: "unverified".into(),
                criterion_evaluations: vec![],
            }),
        };
        fs::write(
            r.state_dir
                .join("t3-receipts")
                .join(format!("{}.json", envelope.operation_id)),
            serde_json::to_vec(&json!({"fingerprint":api::digest(&envelope),"event":event}))
                .unwrap(),
        )
        .unwrap();
        r.ingest(event).unwrap();
        r.create_goal(
            Goal {
                id: id(3),
                title: "Empty unrelated goal".into(),
                status: "draft".into(),
                criteria: vec![Criterion {
                    id: "C1".into(),
                    description: "Empty".into(),
                    requires_human: false,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "# Empty".into(),
        )
        .unwrap();
        (temp, r, cfg, app)
    }
    pub(crate) fn unknown_origin_fixture(
    ) -> (tempfile::TempDir, Runner, ApplicationConfig, Application) {
        let (temp, mut r, cfg, app) = fixture();
        r.state.application["provider_identity"] = Value::Null;
        r.persist().unwrap();
        (temp, r, cfg, app)
    }
    pub(crate) fn request(r: &Runner, cfg: &ApplicationConfig) -> api::AdoptRequest {
        let candidate = candidate(cfg.t3.as_ref().unwrap());
        api::AdoptRequest {
            operation_id: id(100),
            review: r.t3_review(cfg, candidate.clone(), &observed(&candidate)),
        }
    }
    #[test]
    fn completed_history_unreviewed_result_new_generation_new_operation_same_goal() {
        let (temp, mut r, cfg, app) = fixture();
        assert_eq!(r.state.dispatch.as_ref().unwrap().phase, "outcome_ready");
        let old = r.state.dispatch.as_ref().unwrap().clone();
        let before = serde_json::to_vec(&old).unwrap();
        let receipt_path = r
            .state_dir
            .join("t3-receipts")
            .join(format!("{}.json", old.envelope.operation_id));
        let receipt_bytes = fs::read(&receipt_path).unwrap();
        let old_url = app.snapshot(&r).unwrap()["thread_url"].clone();
        let req = request(&r, &cfg);
        assert!(req.review.ready, "{:?}", req.review.blockers);
        let bytes = fs::read(r.state_dir.join("state.json")).unwrap();
        assert_eq!(request(&r, &cfg), req);
        assert_eq!(
            fs::read(r.state_dir.join("state.json")).unwrap(),
            bytes,
            "prepare never persists"
        );
        let receipt = r
            .t3_adopt(&cfg, req.clone(), &observed(&req.review.candidate))
            .unwrap();
        assert_eq!(
            r.t3_adopt(
                &cfg,
                req.clone(),
                &Err(anyhow::anyhow!("offline after commit"))
            )
            .unwrap(),
            receipt
        );
        assert_eq!(
            serde_json::to_vec(r.state.dispatch.as_ref().unwrap()).unwrap(),
            before
        );
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
        assert_eq!(app.snapshot(&r).unwrap()["thread_url"], old_url);
        let mut changed = req.clone();
        changed.review.candidate.model = "another".into();
        assert!(r
            .t3_adopt(&cfg, changed, &observed(&req.review.candidate))
            .is_err());
        assert!(r.t3_settings_guard(Some(&req.review.candidate)).is_err());
        assert!(r.t3_settings_guard(cfg.t3.as_ref()).is_ok());
        let previous = r
            .record::<Stage>("stage", &old.envelope.stage_id)
            .unwrap()
            .0
            .result_ids[0]
            .clone();
        prepare(&mut r, &req.review.candidate, 20, Some(previous));
        let new = r.state.dispatch.as_ref().unwrap();
        assert_ne!(new.envelope.operation_id, old.envelope.operation_id);
        assert_ne!(
            crate::t3::T3Adapter::thread_id(&new.envelope).unwrap(),
            crate::t3::T3Adapter::thread_id(&old.envelope).unwrap()
        );
        assert_eq!(new.envelope.goal_id, old.envelope.goal_id);
        assert_eq!(
            r.t3_operation_settings(&new.envelope.operation_id)
                .unwrap()
                .unwrap(),
            req.review.candidate
        );
        assert_eq!(
            r.t3_operation_settings(&old.envelope.operation_id)
                .unwrap()
                .unwrap(),
            *cfg.t3.as_ref().unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&r.state.previous_stages[&old.envelope.stage_id].dispatch).unwrap(),
            before
        );
        let journal = fs::read(r.state_dir.join("state.json")).unwrap();
        assert!(
            serde_json::from_slice::<State>(&journal).is_err(),
            "old binary must refuse wrapper"
        );
        drop(r);
        let r = Runner::open(config(temp.path())).unwrap();
        assert_eq!(r.t3_transition_receipt(&req).unwrap(), Some(receipt));
        assert_eq!(fs::read(receipt_path).unwrap(), receipt_bytes);
    }
    #[test]
    fn startup_and_reconnect_preserve_unknown_origin_and_known_origin_control() {
        for known in [false, true] {
            for saved_settings in [false, true] {
                let (temp, mut r, cfg, _) = if known {
                    fixture()
                } else {
                    unknown_origin_fixture()
                };
                let original_identity = r.state.application["provider_identity"].clone();
                let original = r.state.dispatch.as_ref().unwrap().clone();
                let original_bytes = serde_json::to_vec(&original).unwrap();
                let receipt_path = r
                    .state_dir
                    .join("t3-receipts")
                    .join(format!("{}.json", original.envelope.operation_id));
                let receipt_bytes = fs::read(&receipt_path).unwrap();
                drop(r);
                r = Runner::open(config(temp.path())).unwrap();
                let app = if saved_settings {
                    crate::settings::apply(cfg.clone(), &temp.path().join("runtime"), &mut r, None)
                        .unwrap()
                        .0
                } else {
                    Application::configure(cfg.clone(), &temp.path().join("runtime"), &mut r)
                        .unwrap()
                        .0
                };
                // Direct bind is also used by connector reconnect/save paths.
                app.bind_target(&mut r).unwrap();
                assert_eq!(app.guard_legacy_t3_route(&r).is_ok(), known);
                assert_eq!(r.state.application["provider_identity"], original_identity);
                assert_eq!(!app.snapshot(&r).unwrap()["thread_url"].is_null(), known);
                let before_review = fs::read(r.state_dir.join("state.json")).unwrap();
                let req = request(&r, &cfg);
                assert!(req.review.ready, "{:?}", req.review.blockers);
                assert_eq!(
                    matches!(
                        req.review.associations[0].association,
                        api::Association::KnownRoute { .. }
                    ),
                    known
                );
                assert_eq!(
                    fs::read(r.state_dir.join("state.json")).unwrap(),
                    before_review
                );
                r.t3_adopt(&cfg, req.clone(), &observed(&req.review.candidate))
                    .unwrap();
                assert_eq!(r.state.application["provider_identity"], original_identity);
                assert_eq!(!app.snapshot(&r).unwrap()["thread_url"].is_null(), known);
                assert_eq!(
                    r.t3_route_view(&cfg)["historical_terminal_unroutable_count"],
                    json!(usize::from(!known))
                );
                drop(r);
                r = Runner::open(config(temp.path())).unwrap();
                let app = Application::configure(cfg.clone(), &temp.path().join("runtime"), &mut r)
                    .unwrap()
                    .0;
                app.bind_target(&mut r).unwrap();
                assert_eq!(r.state.application["provider_identity"], original_identity);
                assert_eq!(!app.snapshot(&r).unwrap()["thread_url"].is_null(), known);
                assert_eq!(
                    serde_json::to_vec(r.state.dispatch.as_ref().unwrap()).unwrap(),
                    original_bytes
                );
                assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
                if !known {
                    assert!(r
                        .t3_operation_settings(&original.envelope.operation_id)
                        .is_err());
                }
                let previous = r
                    .record::<Stage>("stage", &original.envelope.stage_id)
                    .unwrap()
                    .0
                    .result_ids[0]
                    .clone();
                prepare(&mut r, &req.review.candidate, 20, Some(previous));
                let future = &r.state.dispatch.as_ref().unwrap().envelope;
                assert_eq!(
                    r.t3_operation_settings(&future.operation_id)
                        .unwrap()
                        .unwrap(),
                    req.review.candidate
                );
                assert_ne!(future.operation_id, original.envelope.operation_id);
                assert_eq!(future.goal_id, original.envelope.goal_id);
                assert_eq!(
                    serde_json::to_vec(
                        &r.state.previous_stages[&original.envelope.stage_id].dispatch
                    )
                    .unwrap(),
                    original_bytes
                );
            }
        }
    }
    #[test]
    fn empty_goal_can_bind_without_retroactively_binding_another_goals_history() {
        let (temp, mut r, cfg, _) = unknown_origin_fixture();
        let historical_goal = r.state.goal_id.clone().unwrap();
        r.state.route(&id(3)).unwrap();
        assert!(!r.current_has_external_work());
        let app = Application::configure(cfg.clone(), &temp.path().join("runtime"), &mut r)
            .unwrap()
            .0;
        assert_eq!(
            r.state.application["provider_identity"],
            Application::provider_identity(&cfg)
        );
        assert!(r.state.other_goals[&historical_goal].application["provider_identity"].is_null());
        r.state.route(&historical_goal).unwrap();
        app.bind_target(&mut r).unwrap();
        assert!(r.state.application["provider_identity"].is_null());
        assert!(app.snapshot(&r).unwrap()["thread_url"].is_null());
        assert!(request(&r, &cfg).review.ready);
    }
    #[test]
    fn missing_t3_member_and_orphan_events_never_gain_a_configured_origin() {
        for missing in [false, true] {
            let (temp, mut r, cfg, _) = fixture();
            let mut pin = Application::provider_identity(&cfg);
            if missing {
                pin.as_object_mut().unwrap().remove("t3");
            } else {
                pin["t3"] = Value::Null;
            }
            r.state.application["provider_identity"] = pin.clone();
            r.state.dispatch = None;
            r.state.previous_stages.clear();
            assert!(
                !r.state.events.is_empty(),
                "event-only retained history positive control"
            );
            assert!(r.current_has_external_work());
            assert!(r.has_external_work());
            r.persist().unwrap();
            let bytes = fs::read(r.state_dir.join("state.json")).unwrap();
            assert!(
                Application::configure(cfg.clone(), &temp.path().join("runtime"), &mut r).is_err()
            );
            assert!(crate::settings::apply(
                cfg.clone(),
                &temp.path().join("runtime"),
                &mut r,
                None
            )
            .is_err());
            assert_eq!(r.state.application["provider_identity"], pin);
            assert_eq!(fs::read(r.state_dir.join("state.json")).unwrap(), bytes);
            // Whole-null historical identity remains unknown even when only
            // event intake survives; the historical routing pin is not repaired.
            r.state.application["provider_identity"] = Value::Null;
            r.persist().unwrap();
            let app = Application::configure(cfg, &temp.path().join("runtime"), &mut r)
                .unwrap()
                .0;
            app.bind_target(&mut r).unwrap();
            assert!(r.state.application["provider_identity"].is_null());
            assert!(app.snapshot(&r).unwrap()["thread_url"].is_null());
        }
    }
    #[test]
    fn task_first_goal_pins_new_work_without_rebinding_unknown_history() {
        for retained in [false, true] {
            let (_temp, mut r, _cfg, app) = unknown_origin_fixture();
            if !retained {
                r.state.route(&id(3)).unwrap();
            }
            let goal = r.state.goal_id.clone().unwrap();
            let observed = serde_json::from_value(json!({
                "binding":{"provider":"todoist","instance_id":"fixture","goal_id":goal,"external_id":"task-1"},
                "observed_at":WHEN,"task":{"id":"task-1","project_id":"fixture","content":"Synthetic task",
                "description":"","checked":false,"is_deleted":false,"labels":[],"priority":1,
                "due":null,"updated_at":null,"completed_at":null}
            })).unwrap();
            app.task_link_observed(&mut r, goal, observed).unwrap();
            assert_eq!(r.state.application["provider_identity"].is_null(), retained);
            assert_eq!(app.guard_legacy_t3_route(&r).is_ok(), !retained);
        }
    }
    #[test]
    fn new_discussion_never_supplies_origin_for_retained_unknown_stage() {
        let (_temp, mut r, _cfg, mut app) = unknown_origin_fixture();
        app.chat = Some(crate::chat::ChatConfig {
            api_key: "synthetic".into(),
            base_url: "http://127.0.0.1:9".into(),
            model: "synthetic".into(),
            idle_timeout: std::time::Duration::from_secs(1),
        });
        let goal = r.state.goal_id.clone().unwrap();
        let _ = app
            .chat_start(&mut r, goal, "Synthetic discussion".into(), vec![], None)
            .unwrap();
        assert!(r.state.application["provider_identity"].is_null());
        assert!(!r.state.application["conversations"]
            .as_object()
            .unwrap()
            .is_empty());
        assert!(app.snapshot(&r).unwrap()["thread_url"].is_null());
    }
    #[test]
    fn previous_stage_alone_is_retained_work_for_origin_binding() {
        let (temp, mut r, cfg, _) = unknown_origin_fixture();
        let dispatch = r.state.dispatch.take().unwrap();
        let stage_id = dispatch.envelope.stage_id.clone();
        // Exercise the conservative binding boundary even when the current slot
        // has no dispatch but retains a predecessor operation.
        r.state.previous_stages.insert(
            stage_id,
            super::super::StageHistory {
                dispatch,
                retained_goal: None,
                human_acceptances: BTreeMap::new(),
            },
        );
        assert!(r.current_has_external_work());
        let _ = Application::configure(cfg, &temp.path().join("runtime"), &mut r).unwrap();
        assert!(r.state.application["provider_identity"].is_null());
    }
    #[test]
    fn null_pin_terminal_history_is_local_only_and_never_inferred_origin() {
        let (_temp, mut r, cfg, app) = fixture();
        r.state.application["provider_identity"] = Value::Null;
        r.persist().unwrap();
        let operation = r
            .state
            .dispatch
            .as_ref()
            .unwrap()
            .envelope
            .operation_id
            .clone();
        let req = request(&r, &cfg);
        assert!(req.review.ready, "{:?}", req.review.blockers);
        assert!(matches!(
            req.review.associations[0].association,
            api::Association::HistoricalTerminalUnroutable { .. }
        ));
        r.t3_adopt(&cfg, req.clone(), &observed(&req.review.candidate))
            .unwrap();
        assert!(r.t3_operation_settings(&operation).is_err());
        assert_eq!(app.snapshot(&r).unwrap()["thread_url"], Value::Null);
        fs::remove_file(
            r.state_dir
                .join("t3-receipts")
                .join(format!("{operation}.json")),
        )
        .unwrap();
        assert!(
            r.t3_operation_settings(&operation).is_err(),
            "missing receipt never restores network routing"
        );
    }
    #[test]
    fn crash_boundaries_use_durable_receipt_and_unreadable_outcome_stays_unknown() {
        for fault in [1, 2, 3] {
            let (temp, mut r, cfg, _) = fixture();
            let req = request(&r, &cfg);
            r.t3_transition_fault.set(Some(fault));
            assert!(r
                .t3_adopt(&cfg, req.clone(), &observed(&req.review.candidate))
                .is_err());
            match fault {
                1 => {
                    assert!(r.t3_transition_receipt(&req).unwrap().is_none());
                    assert!(!r.t3_has_generations());
                }
                2 => {
                    assert!(r.t3_transition_receipt(&req).unwrap().is_some());
                    assert!(r.t3_has_generations());
                }
                3 => {
                    assert!(r.t3_transition_receipt(&req).is_err());
                    assert!(r.route_recovery_required);
                    assert!(!r.t3_has_generations());
                    assert!(r.mutation(|_| Ok(())).is_err());
                    fs::rename(
                        r.state_dir.join("state.injected"),
                        r.state_dir.join("state.json"),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            drop(r);
            let mut reopened = Runner::open(config(temp.path())).unwrap();
            let receipt = reopened
                .t3_adopt(&cfg, req.clone(), &observed(&req.review.candidate))
                .unwrap();
            assert_eq!(receipt.operation_id, req.operation_id);
        }
    }
    #[test]
    fn routes_wrapper_preserves_unrelated_enrollment_and_detects_lost_journals() {
        let (temp, mut r, cfg, _) = fixture();
        r.ensure_attention_enrollment().unwrap();
        r.ensure_plan_enrollment().unwrap();
        fs::write(
            r.state_dir.join("maestro-links-enrollment.json"),
            serde_json::to_vec(&r.workspace_identity()).unwrap(),
        )
        .unwrap();
        let req = request(&r, &cfg);
        r.t3_adopt(&cfg, req.clone(), &observed(&req.review.candidate))
            .unwrap();
        drop(r);
        let reopened = Runner::open(config(temp.path())).unwrap();
        assert!(!reopened.state.attention_journal.recovery_required);
        assert!(!reopened.state.inbox_plan_journal.recovery_required);
        assert!(!reopened.state.maestro_journal.recovery_required);
        drop(reopened);
        let path = temp.path().join("runtime/state.json");
        let raw = fs::read(&path).unwrap();
        let mut value: Value = serde_json::from_slice(&raw).unwrap();
        let payload = value.get_mut("state").unwrap().as_object_mut().unwrap();
        payload.remove("attention_journal");
        payload.remove("inbox_plan_journal");
        payload.remove("maestro_journal");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let reopened = Runner::open(config(temp.path())).unwrap();
        assert!(reopened.state.attention_journal.recovery_required);
        assert!(reopened.state.inbox_plan_journal.recovery_required);
        assert!(reopened.state.maestro_journal.recovery_required);
    }
    #[test]
    fn edited_source_engine_cannot_hide_retained_t3_binding() {
        let (_temp, mut r, cfg, _) = fixture();
        let stage_id = r.state.dispatch.as_ref().unwrap().envelope.stage_id.clone();
        let mut stage = r.record::<Stage>("stage", &stage_id).unwrap().0;
        stage.engine = "another-engine".into();
        r.queue("stage", &stage_id, &stage, None).unwrap();
        r.persist().unwrap();
        r.flush_writes().unwrap();
        assert!(!request(&r, &cfg).review.ready);
    }
    #[test]
    fn supplied_original_bytes_proof_is_durable_and_used_by_production_receipt_reconcile() {
        let (temp, mut r, cfg, _) = fixture();
        let env = &mut r.state.dispatch.as_mut().unwrap().envelope;
        env.packet
            .extra
            .insert("source_excerpts".into(), json!([{"z":"same","a":1}]));
        let current = env.clone();
        let mut original = current.clone();
        original
            .packet
            .extra
            .insert("source_excerpts".into(), json!([{"a":1,"z":"same"}]));
        assert_eq!(original, current);
        assert_ne!(
            serde_json::to_vec(&original).unwrap(),
            serde_json::to_vec(&current).unwrap()
        );
        let mut artifact = serde_json::to_value(&r.state).unwrap();
        artifact["dispatch"]["envelope"] = serde_json::to_value(&original).unwrap();
        let bytes = serde_json::to_vec(&artifact).unwrap();
        let artifact_path = temp.path().join("original-state.json");
        fs::write(&artifact_path, &bytes).unwrap();
        let receipt_path = r
            .state_dir
            .join("t3-receipts")
            .join(format!("{}.json", current.operation_id));
        let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt["fingerprint"] = json!(api::digest(&original));
        fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        let immutable_receipt = fs::read(&receipt_path).unwrap();
        r.persist().unwrap();
        let manifest = crate::t3_compat::Manifest {
            entries: vec![crate::t3_compat::ArtifactEntry {
                operation_id: current.operation_id.clone(),
                artifact_path: artifact_path.clone(),
                artifact_sha256: format!("{:x}", Sha256::digest(&bytes)),
                provenance: "fixture original compact source journal".into(),
            }],
        };
        assert!(
            !request(&r, &cfg).review.ready,
            "without supplied original proof mismatch stays refused"
        );
        let mut req = request(&r, &cfg);
        req.review = r.t3_review_with_manifest(
            &cfg,
            req.review.candidate.clone(),
            &observed(&req.review.candidate),
            Some(manifest),
        );
        assert!(req.review.ready, "{:?}", req.review.blockers);
        assert_eq!(req.review.compatibility_proofs.len(), 1);
        r.t3_adopt(&cfg, req.clone(), &observed(&req.review.candidate))
            .unwrap();
        assert_eq!(fs::read(&receipt_path).unwrap(), immutable_receipt);
        assert_eq!(r.state.dispatch.as_ref().unwrap().envelope, current);
        fs::remove_file(&artifact_path).unwrap();
        assert!(
            request(&r, &cfg).review.ready,
            "retained proof needs no original artifact pathname"
        );
        drop(r);
        let r = Runner::open(config(temp.path())).unwrap();
        let proof = r.t3_operation_proof(&current.operation_id).unwrap();
        let mut adapter =
            crate::t3_routes::adapter(cfg.t3.as_ref().unwrap(), &r.state_dir).unwrap();
        adapter.retain_compatibility_proof(proof);
        assert!(
            matches!(
                adapter
                    .reconcile(
                        &current,
                        r.state.dispatch.as_ref().unwrap().binding.as_ref()
                    )
                    .unwrap(),
                ReconcileReply::OutcomeAvailable { .. }
            ),
            "production adapter must recover preserved receipt before any old endpoint request"
        );
        assert!(r.t3_operation_proof(&current.operation_id).is_some());
        assert_eq!(fs::read(&receipt_path).unwrap(), immutable_receipt);
    }
    #[test]
    fn raw_current_unknown_missing_and_duplicate_fields_refuse_before_startup_persistence() {
        let (temp, r, _cfg, _) = fixture();
        let path = r.state_dir.join("state.json");
        let original = fs::read(&path).unwrap();
        drop(r);
        for variation in 0..3 {
            let mut value: Value = serde_json::from_slice(&original).unwrap();
            let bytes = match variation {
                0 => {
                    value["dispatch"]["envelope"]["unknown"] = json!("must not disappear");
                    serde_json::to_vec(&value).unwrap()
                }
                1 => {
                    value["dispatch"]["envelope"]["packet"]
                        .as_object_mut()
                        .unwrap()
                        .remove("previous_result_id");
                    serde_json::to_vec(&value).unwrap()
                }
                _ => String::from_utf8(original.clone())
                    .unwrap()
                    .replacen(
                        "\"target\":{",
                        "\"target\":{\"project_id\":\"duplicate\",",
                        1,
                    )
                    .into_bytes(),
            };
            fs::write(&path, &bytes).unwrap();
            assert!(Runner::open(config(temp.path())).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    #[test]
    fn repeated_unverified_review_preserves_superseded_evidence_statuses() {
        let (_temp, mut r, cfg, _) = fixture();
        let result_id = r
            .record::<Stage>(
                "stage",
                &r.state.dispatch.as_ref().unwrap().envelope.stage_id,
            )
            .unwrap()
            .0
            .result_ids[0]
            .clone();
        for (status, evidence) in [("passed", "e1"), ("unverified", "e2")] {
            r.evaluate_result(
                result_id.clone(),
                CriterionEvaluation {
                    criterion_id: "C1".into(),
                    goal_revision: "set by source".into(),
                    status: status.into(),
                    evidence_ids: vec![evidence.into()],
                    evaluated_by: "fixture-reviewer".into(),
                    evaluated_at: WHEN.into(),
                },
            )
            .unwrap();
            let req = request(&r, &cfg);
            assert!(req.review.ready, "{:?}", req.review.blockers);
        }
        let result = r.record::<ResultRecord>("result", &result_id).unwrap().0;
        assert_eq!(result.outcome.evidence[0].status, "passed");
        assert_eq!(
            result.outcome.criterion_evaluations[0].evidence_ids,
            vec!["e2"]
        );
    }
    #[test]
    fn legitimate_human_review_does_not_become_provider_uncertainty() {
        let (_temp, mut r, cfg, _) = fixture();
        r.accept_human(
            "C2".into(),
            "fixture-reviewer".into(),
            WHEN.into(),
            SourceRef {
                uri: "fixture:human-decision".into(),
                revision: None,
                locator: None,
            },
        )
        .unwrap();
        let reviewed = request(&r, &cfg);
        assert!(reviewed.review.ready, "{:?}", reviewed.review.blockers);
    }
    #[test]
    fn edited_stage_identity_and_unsupported_review_changes_refuse() {
        for field in ["id", "context_id"] {
            let (_temp, mut r, cfg, _) = fixture();
            let id = r.state.dispatch.as_ref().unwrap().envelope.stage_id.clone();
            let mut stage =
                serde_json::to_value(r.record::<Stage>("stage", &id).unwrap().0).unwrap();
            stage[field] = json!("edited");
            r.queue("stage", &id, &stage, None).unwrap();
            r.persist().unwrap();
            r.flush_writes().unwrap();
            assert!(!request(&r, &cfg).review.ready);
        }
        let (_temp, mut r, cfg, _) = fixture();
        let stage = r
            .record::<Stage>(
                "stage",
                &r.state.dispatch.as_ref().unwrap().envelope.stage_id,
            )
            .unwrap()
            .0;
        let mut result = r
            .record::<ResultRecord>("result", &stage.result_ids[0])
            .unwrap()
            .0;
        result
            .outcome
            .criterion_evaluations
            .push(CriterionEvaluation {
                criterion_id: "unknown".into(),
                goal_revision: "unknown".into(),
                status: "passed".into(),
                evidence_ids: vec!["unknown".into()],
                evaluated_by: "actor".into(),
                evaluated_at: WHEN.into(),
            });
        r.queue("result", &result.id, &result, None).unwrap();
        r.persist().unwrap();
        r.flush_writes().unwrap();
        assert!(!request(&r, &cfg).review.ready);
    }
    #[test]
    fn negative_evidence_and_multigoal_uncertainty_refuse_before_persist() {
        let (_temp, mut r, cfg, _) = fixture();
        let original = r.state.clone();
        for phase in ["prepared", "running", "indeterminate", "unknown"] {
            r.state = original.clone();
            r.state.other_goals.get_mut(&id(3)).unwrap().dispatch =
                Some(r.state.dispatch.clone().unwrap());
            r.state
                .other_goals
                .get_mut(&id(3))
                .unwrap()
                .dispatch
                .as_mut()
                .unwrap()
                .phase = phase.into();
            assert!(!request(&r, &cfg).review.ready, "phase {phase}");
        }
        r.state = original.clone();
        r.state.other_goals.get_mut(&id(3)).unwrap().application =
            json!({"conversations":{"x":{"status":"running"}}});
        assert!(!request(&r, &cfg).review.ready);
        r.state = original.clone();
        let req = request(&r, &cfg);
        r.state
            .dispatch
            .as_mut()
            .unwrap()
            .cursors
            .insert("stream".into(), "different".into());
        assert!(!request(&r, &cfg).review.ready);
        r.state = original.clone();
        r.state
            .dispatch
            .as_mut()
            .unwrap()
            .binding
            .as_mut()
            .unwrap()
            .turn_id = Some("different".into());
        assert!(!request(&r, &cfg).review.ready);
        r.state = original.clone();
        r.state.events[0].projected = false;
        assert!(!request(&r, &cfg).review.ready);
        r.state = original.clone();
        let path = r.state_dir.join("t3-receipts").join(format!(
            "{}.json",
            r.state.dispatch.as_ref().unwrap().envelope.operation_id
        ));
        let receipt = fs::read(&path).unwrap();
        fs::write(&path, b"{}").unwrap();
        assert!(!request(&r, &cfg).review.ready);
        fs::write(&path, receipt).unwrap();
        let before = fs::read(r.state_dir.join("state.json")).unwrap();
        assert!(r
            .t3_adopt(&cfg, req.clone(), &Err(anyhow::anyhow!("offline")))
            .is_err());
        assert_eq!(fs::read(r.state_dir.join("state.json")).unwrap(), before);
        let wrong = Ok(json!({"environment_id":"wrong","projects":[{"id":"same-project"}]}));
        assert!(r.t3_adopt(&cfg, req, &wrong).is_err());
        assert_eq!(fs::read(r.state_dir.join("state.json")).unwrap(), before);
    }
}
