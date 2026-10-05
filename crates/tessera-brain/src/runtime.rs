use crate::types::*;
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use tessera_core::source::{SourceSnapshot, SourceStore, SourceWrite, WriteBoundary};
use uuid::Uuid;

mod attention;
pub(crate) mod discussion_decision;
mod discussion_decision_reuse;
mod goal_brief;
mod goal_criteria;
mod inbox;
mod inbox_plan;
mod maestro_control;
mod maestro_links;
mod maestro_operations;
mod proposal_adoption;
mod proposal_drafts;
mod proposal_feed;
mod proposal_generation;
mod proposal_inbox_adoption;
mod proposal_retry;
mod suggestions;
pub(crate) mod t3_routes;

pub struct RunnerConfig {
    pub brain_id: String,
    pub root: PathBuf,
    pub operational_dir: PathBuf,
    /// Existing relative directory for this POC's owned Markdown records.
    pub records_dir: String,
    pub boundary: WriteBoundary,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Dispatch {
    envelope: StartEnvelope,
    phase: String,
    binding: Option<EngineRef>,
    sequences: BTreeMap<String, u64>,
    cursors: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct EventIntake {
    event: EngineEvent,
    projected: bool,
    reason: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    t3_routes: Option<crate::t3_routes::Journal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    suggestions: Option<suggestions::Control>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposal_feed: Option<proposal_feed::Journal>,
    #[serde(default)]
    maestro_journal: crate::maestro_links::Journal,
    brain_id: String,
    records_dir: String,
    pending_writes: Vec<SourceWrite>,
    #[serde(default)]
    inbox_operations: BTreeMap<String, inbox::Intent>,
    #[serde(default)]
    inbox_aliases: BTreeMap<String, String>,
    #[serde(default)]
    inbox_recovery_required: bool,
    #[serde(default)]
    attention_journal: attention::Journal,
    #[serde(default)]
    inbox_plan_journal: inbox_plan::Journal,
    #[serde(default)]
    prepared_changes: BTreeMap<String, PreparedChangeOutcome>,
    /// Keep the original POC goal in the original flattened fields on disk.
    /// Additional goals have the same independently owned journal shape.
    #[serde(flatten)]
    current: GoalState,
    #[serde(default)]
    primary_goal_id: Option<String>,
    #[serde(default)]
    other_goals: BTreeMap<String, GoalState>,
}
#[derive(Clone, Serialize, Deserialize)]
struct PreparedChangeOutcome {
    request: PreparedChangeRequest,
    receipt: PreparedChangeReceipt,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct GoalState {
    #[serde(default)]
    application: Value,
    goal_id: Option<String>,
    stage_id: Option<String>,
    dispatch: Option<Dispatch>,
    retained_goal: Option<SourceSnapshot>,
    events: Vec<EventIntake>,
    attention: Vec<Attention>,
    #[serde(default)]
    attention_stage_ids: BTreeMap<String, String>,
    human_acceptances: BTreeMap<String, HumanAcceptance>,
    #[serde(default)]
    previous_stages: BTreeMap<String, StageHistory>,
}
impl GoalState {
    fn has_external_work(&self) -> bool {
        self.dispatch.is_some()
            || !self.previous_stages.is_empty()
            || !self.events.is_empty()
            || self.application.get("task").is_some_and(|v| !v.is_null())
            || ["mutations", "conversations"].iter().any(|key| {
                self.application
                    .get(key)
                    .and_then(Value::as_object)
                    .is_some_and(|v| !v.is_empty())
            })
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct StageHistory {
    dispatch: Dispatch,
    retained_goal: Option<SourceSnapshot>,
    human_acceptances: BTreeMap<String, HumanAcceptance>,
}
impl std::ops::Deref for State {
    type Target = GoalState;
    fn deref(&self) -> &GoalState {
        &self.current
    }
}
impl std::ops::DerefMut for State {
    fn deref_mut(&mut self) -> &mut GoalState {
        &mut self.current
    }
}
/// Version wrapper deliberately omits legacy root fields: old binaries fail closed.
fn journal_value(bytes: &[u8]) -> Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    if value.get("schema").is_some() {
        ensure!(
            value["schema"] == "tessera-runtime/routes-v2" && value["state"].is_object(),
            "unsupported runtime journal version"
        );
        ensure!(
            value["state"]["t3_routes"].is_object(),
            "route journal missing"
        );
        Ok(value["state"].clone())
    } else {
        ensure!(
            value.get("t3_routes").is_none_or(Value::is_null),
            "route journal needs downgrade fence"
        );
        Ok(value)
    }
}
fn decode_state(bytes: &[u8]) -> Result<State> {
    let state: State = serde_json::from_value(journal_value(bytes)?)?;
    // Validate actual persisted bytes before startup can persist a lossy typed
    // decode. A reserialized State cannot prove absence of unknown/duplicate keys.
    let mut current_envelopes = Vec::new();
    for slot in std::iter::once(&state.current).chain(state.other_goals.values()) {
        for dispatch in slot
            .dispatch
            .iter()
            .chain(slot.previous_stages.values().map(|s| &s.dispatch))
        {
            if dispatch.binding.as_ref().is_some_and(|b| b.engine == "t3")
                || state.t3_routes.as_ref().is_some_and(|j| {
                    j.compatibility_proofs
                        .contains_key(&dispatch.envelope.operation_id)
                })
                || dispatch.envelope.target.contains_key("environment_id")
                || dispatch.envelope.target.contains_key("project_id")
            {
                current_envelopes.push(&dispatch.envelope);
            }
        }
    }
    if !current_envelopes.is_empty() {
        crate::t3_compat::validate_current_artifacts(bytes, &current_envelopes)?;
    }
    Ok(state)
}

impl State {
    fn has_active_provider_work(&self) -> bool {
        std::iter::once(&self.current)
            .chain(self.other_goals.values())
            .any(|slot| {
                slot.dispatch
                    .as_ref()
                    .is_some_and(|d| !["outcome_ready", "cancelled"].contains(&d.phase.as_str()))
                    || slot
                        .application
                        .get("conversations")
                        .and_then(Value::as_object)
                        .is_some_and(|conversations| {
                            conversations.values().any(|c| c["status"] == "running")
                        })
            })
    }
    /// Provider routing remains pinned once a task, chat or stage owns external history.
    /// Credential replacement is separate and does not change this identity.
    fn has_external_work(&self) -> bool {
        std::iter::once(&self.current)
            .chain(self.other_goals.values())
            .any(GoalState::has_external_work)
    }

    fn route(&mut self, goal_id: &str) -> Result<()> {
        if self.goal_id.as_deref() == Some(goal_id) {
            return Ok(());
        }
        let next = self.other_goals.remove(goal_id).context("unknown goal")?;
        let old = std::mem::replace(&mut self.current, next);
        if let Some(id) = &old.goal_id {
            self.other_goals.insert(id.clone(), old);
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct HumanAcceptance {
    id: String,
    goal_id: String,
    status: String,
    definition: Criterion,
    actor: String,
    observed_at: String,
    source: SourceRef,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: String,
    pub goal: Option<Goal>,
    pub stage: Option<Stage>,
    pub dispatch: Option<StartEnvelope>,
    pub phase: Option<String>,
    pub binding: Option<EngineRef>,
    pub attention: Vec<Attention>,
    #[serde(default)]
    pub attention_history: Vec<Attention>,
    #[serde(default)]
    pub attention_stage_ids: BTreeMap<String, String>,
    pub event_count: usize,
    pub pending_writes: usize,
    #[serde(default)]
    pub prepared_guard: Option<PreparedStageGuard>,
    #[serde(default)]
    pub can_change_prepared: bool,
    #[serde(default)]
    pub requires_guarded_start: bool,
}

// Old runners only dispatch phases prepared/not_started. The edited variant
// is an intentional compatibility fence; it projects as prepared to new clients.
fn is_prepared(phase: &str) -> bool {
    matches!(phase, "prepared" | "prepared_edited")
}

fn stage_runtime_blocker(a: &Attention) -> bool {
    a.kind == "blocker"
        && (matches!(
            a.message.as_str(),
            "engine needs attention"
                | "t3_command_accepted_waiting_for_correlated_turn"
                | "t3_turn_correlation_unknown"
        ) || a.message.starts_with("observation unavailable: "))
}
fn stage_lifecycle_attention(a: &Attention) -> bool {
    stage_runtime_blocker(a)
        || matches!(
            (a.kind.as_str(), a.message.as_str()),
            (
                "decision",
                "outcome saved; goal criteria remain unmet" | "result review recorded"
            ) | (
                "final",
                "goal criteria passed"
                    | "goal criteria passed after explicit human acceptance"
                    | "result review recorded"
            )
        )
}

/// The typed packet remains lossless frontmatter; the body is for reviewing
/// what the engine will receive, without transport encodings or duplicated JSON.
fn context_body(packet: &ContextPacket) -> String {
    let mut body = format!(
        "\n# Prepared stage context\n\n{}\n\n## Next step\n\n{}\n",
        packet.goal, packet.next_step
    );
    for (heading, items) in [
        ("Decisions", &packet.decisions),
        ("Constraints", &packet.constraints),
    ] {
        if !items.is_empty() {
            body.push_str(&format!("\n## {heading}\n\n"));
            for item in items {
                body.push_str(&format!("- {item}\n"));
            }
        }
    }
    if !packet.sources.is_empty() {
        body.push_str("\n## Sources\n\n");
        for source in &packet.sources {
            body.push_str(&format!("- [{}]({})", source.uri, source.uri));
            if let Some(revision) = &source.revision {
                body.push_str(&format!(" — {revision}"));
            }
            body.push('\n');
        }
    }
    if let Some(messages) = packet
        .extra
        .get("conversation")
        .and_then(|c| c.get("messages"))
        .and_then(Value::as_array)
    {
        body.push_str("\n## Discussion\n");
        for message in messages {
            if let Some(text) = message["text"].as_str() {
                body.push_str(&format!(
                    "\n### {}\n\n{text}\n",
                    message["role"].as_str().unwrap_or("Message")
                ));
            }
        }
    }
    if let Some(conversation) = packet.extra.get("conversation") {
        if let Some(partial) = conversation["partial"]
            .as_str()
            .filter(|text| !text.is_empty())
        {
            body.push_str(&format!(
                "\n### Partial assistant response ({})\n\n{partial}\n",
                conversation["status"].as_str().unwrap_or("incomplete")
            ));
        }
    }
    if let Some(previous) = packet
        .extra
        .get("previous_result")
        .filter(|value| !value.is_null())
    {
        body.push_str(&format!(
            "\n## Previous result\n\n{}\n\nRecord: {}\n",
            previous["summary"].as_str().unwrap_or("Retained result"),
            packet.previous_result_id.as_deref().unwrap_or("unknown")
        ));
    }
    if let Some(excerpts) = packet
        .extra
        .get("source_excerpts")
        .and_then(Value::as_array)
    {
        body.push_str("\n## Frozen source contents\n");
        for excerpt in excerpts {
            if let Some(text) = excerpt["text"].as_str() {
                body.push_str(&format!(
                    "\n### {}\n\n{text}\n",
                    excerpt["path"].as_str().unwrap_or("Source")
                ));
            }
        }
    }
    body
}

pub struct Runner {
    route_recovery_required: bool,
    #[cfg(test)]
    t3_transition_fault: std::cell::Cell<Option<u8>>,
    #[cfg(test)]
    discussion_fault: Option<u8>,
    #[cfg(test)]
    suggestions_fault: Option<u8>,
    #[cfg(test)]
    suggestions_persist_fault: std::cell::Cell<Option<u8>>,
    proposal_feed_issue: Option<String>,
    proposal_store: Option<crate::proposals::Store>,
    proposal_draft_issue: Option<String>,
    #[cfg(test)]
    proposal_draft_fault: Option<proposal_drafts::Fault>,
    #[cfg(test)]
    proposal_adoption_fault: Option<proposal_adoption::Fault>,
    #[cfg(test)]
    proposal_inbox_adoption_fault: Option<proposal_inbox_adoption::Fault>,
    #[cfg(test)]
    proposal_feed_fault: Option<proposal_feed::Fault>,
    #[cfg(test)]
    proposal_feed_fault_after: usize,
    source: SourceStore,
    root: PathBuf,
    managed: bool,
    inbox_planning_enabled: bool,
    state_dir: PathBuf,
    state: State,
    _process_lock: File,
    #[cfg(test)]
    interrupt_after_write: Option<usize>,
    #[cfg(test)]
    interrupt_after_inbox_intent: bool,
    #[cfg(test)]
    interrupt_after_plan_intent: bool,
}
#[derive(Debug)]
pub(crate) struct PreparedChangeNotRecorded(String);
impl std::fmt::Display for PreparedChangeNotRecorded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PreparedChangeNotRecorded {}
impl Runner {
    pub fn open(config: RunnerConfig) -> Result<Self> {
        uuid(&config.brain_id)?;
        ensure!(
            !config.records_dir.is_empty()
                && config
                    .records_dir
                    .split('/')
                    .all(|p| !p.is_empty() && p != "." && p != "..")
                && !config.records_dir.contains('\\'),
            "invalid records directory"
        );
        ensure!(
            config.root.join(&config.records_dir).is_dir(),
            "configured records directory must exist"
        );
        let root = fs::canonicalize(&config.root)?;
        let state_dir = fs::canonicalize(&config.operational_dir)?;
        ensure!(
            !state_dir.starts_with(&root) && !root.starts_with(&state_dir),
            "operational directory must be outside brain"
        );
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(state_dir.join("runner.lock"))?;
        lock.try_lock()
            .context("another runner owns this operational directory")?;
        let source_dir = state_dir.join("source");
        fs::create_dir_all(&source_dir)?;
        let source = SourceStore::open_with_suggestions_control(
            &config.brain_id,
            &root,
            &source_dir,
            config.boundary,
        )?;
        let state_path = state_dir.join("state.json");
        let state = if state_path.exists() {
            let state: State = decode_state(&fs::read(&state_path)?)
                .context("invalid journal; manual recovery required")?;
            ensure!(
                state.brain_id == config.brain_id && state.records_dir == config.records_dir,
                "runtime identity mismatch"
            );
            state
        } else {
            ensure!(
                !fs::read_dir(root.join(&config.records_dir))?
                    .any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().starts_with("goal-"))),
                "owned goals exist without operational state; recover, do not redispatch"
            );
            State {
                brain_id: config.brain_id,
                records_dir: config.records_dir,
                ..State::default()
            }
        };
        let mut runner = Self {
            route_recovery_required: false,
            #[cfg(test)]
            t3_transition_fault: std::cell::Cell::new(None),
            #[cfg(test)]
            suggestions_fault: None,
            #[cfg(test)]
            discussion_fault: None,
            #[cfg(test)]
            suggestions_persist_fault: std::cell::Cell::new(None),
            proposal_feed_issue: None,
            proposal_store: None,
            proposal_draft_issue: None,
            #[cfg(test)]
            proposal_draft_fault: None,
            #[cfg(test)]
            proposal_adoption_fault: None,
            #[cfg(test)]
            proposal_inbox_adoption_fault: None,
            #[cfg(test)]
            proposal_feed_fault: None,
            #[cfg(test)]
            proposal_feed_fault_after: 0,
            source,
            root,
            managed: config.boundary == WriteBoundary::Managed,
            inbox_planning_enabled: inbox_plan::NEW_PLANS_ENABLED,
            state_dir,
            state,
            _process_lock: lock,
            #[cfg(test)]
            interrupt_after_write: None,
            #[cfg(test)]
            interrupt_after_inbox_intent: false,
            #[cfg(test)]
            interrupt_after_plan_intent: false,
        };
        runner.check_inbox_receipt_inventory()?;
        runner.check_attention_receipt_inventory()?;
        runner.check_plan_inventory()?;
        runner.check_maestro_inventory()?;
        runner.recover_suggestions_control()?;
        runner.recover_proposal_enrollment()?;
        runner.open_proposal_drafts()?;
        runner.open_proposal_adoption()?;
        runner.open_proposal_inbox_adoption()?;
        // No adapter is called during reconstruction. An interrupted start is
        // uncertain until the adapter provides reconciliation evidence.
        if runner.state.primary_goal_id.is_none() {
            runner.state.primary_goal_id = runner.state.goal_id.clone();
        }
        for slot in
            std::iter::once(&mut runner.state.current).chain(runner.state.other_goals.values_mut())
        {
            if let Some(d) = &mut slot.dispatch {
                if d.phase == "submitting" || d.phase == "running" {
                    d.phase = "indeterminate".into();
                }
            }
        }
        runner.persist()?;
        runner.ensure_attention_enrollment()?;
        runner.ensure_plan_enrollment()?;
        runner.flush_writes()?;
        Ok(runner)
    }
    // On an unsuccessful mutation discard speculative in-memory changes and
    // recover the last durable journal. Persisted pending writes remain pending;
    // this is not a rollback of already committed source bytes.
    fn mutation<T>(&mut self, action: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        ensure!(
            !self.route_recovery_required,
            "route journal recovery required before mutation"
        );
        match action(self) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.state = decode_state(&fs::read(self.state_dir.join("state.json"))?)?;
                Err(error)
            }
        }
    }
    /// Route one request by immutable ownership, never by a desktop selection.
    /// Read-only routing does not write the journal or invoke an adapter.
    pub fn with_goal<T>(
        &mut self,
        goal_id: &str,
        action: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let previous = self.state.goal_id.clone();
        self.state.route(goal_id)?;
        let result = action(self);
        if let Some(previous) = previous {
            self.state.route(&previous)?;
        }
        result
    }
    pub(crate) fn application_owner(&self, collection: &str, id: &str) -> Option<String> {
        std::iter::once(&self.state.current)
            .chain(self.state.other_goals.values())
            .find(|slot| {
                slot.application
                    .get(collection)
                    .and_then(|v| v.get(id))
                    .is_some()
            })
            .and_then(|slot| slot.goal_id.clone())
    }
    pub fn goal_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self
            .state
            .goal_id
            .iter()
            .cloned()
            .chain(self.state.other_goals.keys().cloned())
            .collect();
        ids.sort_by_key(|id| (Some(id) != self.state.primary_goal_id.as_ref(), id.clone()));
        ids
    }
    pub fn goals(&self) -> Result<Vec<Goal>> {
        self.goal_ids()
            .iter()
            .map(|id| {
                let (mut goal, _) = self.record::<Goal>("goal", id)?;
                let state = if self.state.goal_id.as_ref() == Some(id) {
                    Some(&self.state.current)
                } else {
                    self.state.other_goals.get(id)
                };
                if goal.status == "completed"
                    && !state.is_some_and(|state| self.goal_completion_valid(&goal, state))
                {
                    goal.status = "blocked".into();
                }
                Ok(goal)
            })
            .collect()
    }
    pub fn stages(&self) -> Result<Vec<Stage>> {
        let Some(id) = &self.state.goal_id else {
            return Ok(vec![]);
        };
        let goal = self.record::<Goal>("goal", id)?.0;
        goal.stage_ids
            .iter()
            .map(|id| self.record::<Stage>("stage", id).map(|r| r.0))
            .collect()
    }
    pub fn create_goal(&mut self, goal: Goal, body: String) -> Result<Snapshot> {
        ensure!(
            !goal.extra.contains_key("origin_inbox"),
            "retained inbox origin can only be created by inbox_plan"
        );
        self.mutation(|r| {
            if r.state.goal_id.is_none() {
                return r.create_goal_inner(goal, body);
            }
            ensure!(
                !r.goal_ids().contains(&goal.id),
                "goal identity already exists"
            );
            let id = goal.id.clone();
            r.state.other_goals.insert(
                id.clone(),
                GoalState {
                    goal_id: Some(id.clone()),
                    ..GoalState::default()
                },
            );
            r.with_goal(&id, |r| {
                r.state.goal_id = None;
                r.create_goal_inner(goal, body)
            })
        })
    }
    pub fn prepare_stage(
        &mut self,
        stage: Stage,
        packet: ContextPacket,
        operation_id: String,
        target: BTreeMap<String, Value>,
    ) -> Result<Snapshot> {
        let goal_id = stage.goal_id.clone();
        self.with_goal(&goal_id, |r| {
            r.mutation(|r| r.prepare_stage_inner(stage, packet, operation_id, target))
        })
    }
    pub fn start(&mut self, adapter: &mut dyn Adapter) -> Result<Snapshot> {
        self.start_expected(adapter, None)
    }
    pub fn start_expected(
        &mut self,
        adapter: &mut dyn Adapter,
        expected: Option<PreparedStageGuard>,
    ) -> Result<Snapshot> {
        self.mutation(|r| {
            r.flush_writes()?;
            if let Some(expected) = expected {
                r.check_prepared_guard(&expected)?;
            } else {
                ensure!(
                    !r.requires_guarded_start(),
                    "this goal has prepared-stage changes; Start requires an exact expected guard"
                );
            }
            r.start_inner(adapter)
        })
    }
    pub fn change_prepared(
        &mut self,
        request: PreparedChangeRequest,
    ) -> Result<PreparedChangeReceipt> {
        let goal_id = request.goal_id.clone();
        let operation_id = request.operation_id.clone();
        let result = self.with_goal(&goal_id, |r| {
            r.mutation(|r| r.change_prepared_inner(request))
        });
        result.map_err(|error| {
            // Certify rejection from durable state, never speculative memory.
            // If disk recovery itself fails, leave the outcome uncertain. The
            // process owner and service mutex exclude another writer here.
            let persisted = fs::read(self.state_dir.join("state.json"))
                .ok()
                .and_then(|bytes| decode_state(&bytes).ok());
            if persisted.is_some_and(|state| !state.prepared_changes.contains_key(&operation_id)) {
                let message = error.to_string();
                error.context(PreparedChangeNotRecorded(message))
            } else {
                error
            }
        })
    }
    fn requires_guarded_start(&self) -> bool {
        self.state
            .dispatch
            .as_ref()
            .is_some_and(|d| d.phase == "prepared_edited")
            || self
                .state
                .prepared_changes
                .values()
                .any(|change| Some(&change.request.goal_id) == self.state.goal_id.as_ref())
    }
    fn prepared_guard(&self) -> Result<Option<PreparedStageGuard>> {
        self.state
            .dispatch
            .as_ref()
            .map(|d| {
                Ok(PreparedStageGuard {
                    stage_id: d.envelope.stage_id.clone(),
                    stage_revision: self
                        .record::<Stage>("stage", &d.envelope.stage_id)?
                        .1
                        .revision,
                    context_id: d.envelope.context_id.clone(),
                    context_revision: d.envelope.context_revision.clone(),
                })
            })
            .transpose()
    }
    fn check_prepared_guard(&self, expected: &PreparedStageGuard) -> Result<()> {
        ensure!(
            self.prepared_guard()?.as_ref() == Some(expected),
            "prepared stage changed; refresh and review before retrying"
        );
        ensure!(
            self.record::<ContextPacket>("context", &expected.context_id)?
                .1
                .revision
                == expected.context_revision,
            "prepared context bytes changed; recover its original revision before execution"
        );
        Ok(())
    }
    fn change_prepared_inner(
        &mut self,
        request: PreparedChangeRequest,
    ) -> Result<PreparedChangeReceipt> {
        self.flush_writes()?;
        uuid(&request.operation_id)?;
        if let Some(old) = self.state.prepared_changes.get(&request.operation_id) {
            ensure!(
                old.request == request,
                "prepared change operation identity reused with different input"
            );
            return Ok(old.receipt.clone());
        }
        ensure!(
            self.state.goal_id.as_ref() == Some(&request.goal_id),
            "prepared change goal mismatch"
        );
        self.check_prepared_guard(&request.expected)?;
        let mut old_dispatch = self.state.dispatch.clone().context("no prepared stage")?;
        ensure!(
            is_prepared(&old_dispatch.phase) && old_dispatch.binding.is_none(),
            "only a never-dispatched prepared stage can be revised or discarded"
        );
        let (mut old_stage, _) = self.record::<Stage>("stage", &request.expected.stage_id)?;
        ensure!(
            old_stage.status == "ready" && old_stage.result_ids.is_empty(),
            "stage is not an undispatched preparation"
        );
        let mut receipt = PreparedChangeReceipt {
            operation_id: request.operation_id.clone(),
            goal_id: request.goal_id.clone(),
            action: "discard".into(),
            previous: request.expected.clone(),
            replacement: None,
        };
        old_stage.status = "cancelled".into();
        old_stage.extra.insert(
            "prepared_change_operation_id".into(),
            Value::String(request.operation_id.clone()),
        );
        match &request.change {
            PreparedChange::Discard => {
                old_stage.extra.insert(
                    "prepared_disposition".into(),
                    Value::String("discarded".into()),
                );
                self.queue("stage", &old_stage.id, &old_stage, None)?;
                self.state.dispatch.as_mut().unwrap().phase = "discarded".into();
            }
            PreparedChange::Revise { next_step } => {
                ensure!(
                    !next_step.trim().is_empty(),
                    "revised next step must not be empty"
                );
                let (mut goal, _) = self.record::<Goal>("goal", &request.goal_id)?;
                let retained = self
                    .state
                    .retained_goal
                    .as_ref()
                    .context("missing retained goal")?;
                let (metadata, _) = parse_document(retained)?;
                let original_goal: Goal =
                    serde_yaml::from_value(serde_yaml::Value::Mapping(metadata))?;
                ensure!(
                    goal.criteria == original_goal.criteria,
                    "goal criteria changed; discard and prepare a new context instead"
                );
                let namespace = Uuid::parse_str(&request.operation_id)?;
                let stage_id = Uuid::new_v5(&namespace, b"prepared-stage").to_string();
                let context_id = Uuid::new_v5(&namespace, b"prepared-context").to_string();
                let dispatch_id = Uuid::new_v5(&namespace, b"prepared-dispatch").to_string();
                for (kind, id) in [("stage", &stage_id), ("context", &context_id)] {
                    ensure!(
                        !self.root.join(self.path(kind, id)).try_exists()?,
                        "replacement identity already exists"
                    );
                }
                let mut packet = old_dispatch.envelope.packet.clone();
                packet.id = context_id.clone();
                packet.stage_id = stage_id.clone();
                packet.next_step = next_step.clone();
                let mut stage = old_stage.clone();
                stage.id = stage_id.clone();
                stage.context_id = context_id.clone();
                stage.status = "ready".into();
                stage.extra.remove("prepared_change_operation_id");
                stage.extra.insert(
                    "supersedes_stage_id".into(),
                    Value::String(old_stage.id.clone()),
                );
                old_stage.extra.insert(
                    "prepared_disposition".into(),
                    Value::String("superseded".into()),
                );
                old_stage.extra.insert(
                    "superseded_by_stage_id".into(),
                    Value::String(stage_id.clone()),
                );
                // Preserve the exact old context file and dispatch envelope. Only
                // the retired stage's lifecycle projection changes.
                self.queue("stage", &old_stage.id, &old_stage, None)?;
                let context_revision = self.queue(
                    "context",
                    &context_id,
                    &packet,
                    Some(&context_body(&packet)),
                )?;
                let stage_revision =
                    self.queue("stage", &stage_id, &stage, Some("\n# Execution stage\n"))?;
                goal.stage_ids.push(stage_id.clone());
                self.queue("goal", &goal.id, &goal, None)?;
                old_dispatch.phase = "superseded".into();
                let mut replacement = old_dispatch.clone();
                replacement.phase = "prepared_edited".into();
                replacement.envelope.operation_id = dispatch_id;
                replacement.envelope.stage_id = stage_id.clone();
                replacement.envelope.context_id = context_id.clone();
                replacement.envelope.context_revision = context_revision.clone();
                replacement.envelope.packet = packet;
                let history = StageHistory {
                    dispatch: old_dispatch,
                    retained_goal: self.state.retained_goal.clone(),
                    human_acceptances: self.state.human_acceptances.clone(),
                };
                self.state
                    .previous_stages
                    .insert(old_stage.id.clone(), history);
                if stage.engine == "t3" {
                    self.t3_pin_operation(
                        &replacement.envelope.operation_id,
                        &replacement.envelope.target,
                    )?;
                }
                self.state.dispatch = Some(replacement);
                self.state.stage_id = Some(stage_id.clone());
                receipt.action = "revise".into();
                receipt.replacement = Some(PreparedStageGuard {
                    stage_id,
                    stage_revision,
                    context_id,
                    context_revision,
                });
            }
        }
        self.state.prepared_changes.insert(
            request.operation_id.clone(),
            PreparedChangeOutcome {
                request,
                receipt: receipt.clone(),
            },
        );
        self.persist()?;
        self.flush_writes()?;
        Ok(receipt)
    }
    pub fn reconcile(&mut self, adapter: &mut dyn Adapter) -> Result<Snapshot> {
        self.mutation(|r| r.reconcile_inner(adapter))
    }
    pub fn poll(&mut self, adapter: &mut dyn Adapter) -> Result<Snapshot> {
        self.mutation(|r| r.poll_inner(adapter))
    }
    pub fn ingest(&mut self, event: EngineEvent) -> Result<Snapshot> {
        let owner = self.goal_ids().into_iter().find(|id| {
            let slot = if self.state.goal_id.as_ref() == Some(id) {
                &self.state.current
            } else {
                &self.state.other_goals[id]
            };
            slot.dispatch
                .as_ref()
                .is_some_and(|d| d.envelope.operation_id == event.operation_id)
                || slot
                    .previous_stages
                    .values()
                    .any(|s| s.dispatch.envelope.operation_id == event.operation_id)
        });
        if let Some(owner) = owner {
            self.with_goal(&owner, |r| r.mutation(|r| r.ingest_inner(event)))
        } else {
            self.mutation(|r| r.ingest_inner(event))
        }
    }
    pub fn accept_human(
        &mut self,
        criterion_id: String,
        actor: String,
        observed_at: String,
        source: SourceRef,
    ) -> Result<Snapshot> {
        self.mutation(|r| r.accept_human_inner(criterion_id, actor, observed_at, source))
    }
    pub(crate) fn path(&self, kind: &str, id: &str) -> String {
        format!("{}/{}-{}.md", self.state.records_dir, kind, id)
    }
    pub(crate) fn read_preview_source(&self, path: &str, max_bytes: u64) -> Result<SourceSnapshot> {
        Ok(self.source.read_bounded(path, max_bytes)?)
    }
    pub fn workspace_identity(&self) -> Value {
        serde_json::json!({"brain_id": self.state.brain_id, "root": self.root,
            "records_dir": self.state.records_dir, "managed": self.managed})
    }
    pub(crate) fn root(&self) -> &std::path::Path {
        &self.root
    }
    pub(crate) fn application_state(&self) -> Value {
        self.state.application.clone()
    }
    pub(crate) fn managed(&self) -> bool {
        self.managed
    }
    pub(crate) fn source_list(&self) -> Result<Vec<Value>> {
        fn walk(r: &Runner, dir: &std::path::Path, items: &mut Vec<Value>) -> Result<()> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let ty = entry.file_type()?;
                if ty.is_symlink() || entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                if ty.is_dir() {
                    walk(r, &entry.path(), items)?;
                } else if entry.path().extension().is_some_and(|e| e == "md") {
                    let path = entry
                        .path()
                        .strip_prefix(&r.root)?
                        .to_string_lossy()
                        .replace('\\', "/");
                    if let Ok(source) = r.read_source(&path) {
                        let bytes = STANDARD.decode(&source.content_base64)?;
                        let text = String::from_utf8_lossy(&bytes);
                        let body = parse_document(&source)
                            .map(|(_, body)| body)
                            .unwrap_or_else(|_| text.into_owned());
                        let title = body
                            .lines()
                            .find_map(|line| line.strip_prefix("# "))
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                entry
                                    .path()
                                    .file_stem()
                                    .unwrap()
                                    .to_string_lossy()
                                    .into_owned()
                            });
                        items.push(serde_json::json!({"title":title, "path":path}));
                    }
                }
            }
            Ok(())
        }
        let mut items = Vec::new();
        walk(self, &self.root, &mut items)?;
        items.sort_by_key(|v| v["path"].as_str().unwrap().to_owned());
        Ok(items)
    }
    // One journal transaction owns both application intent and queued canonical writes.
    #[cfg(test)]
    pub(crate) fn interrupt_next_source_projection(&mut self) {
        self.interrupt_after_write = Some(1);
    }
    pub(crate) fn checkpoint_application(
        &mut self,
        state: Value,
        record: Option<(&str, &str, Value, String)>,
        task_ref: Option<Value>,
    ) -> Result<()> {
        self.mutation(|r| {
            r.flush_writes()?;
            if let Some((kind, id, value, body)) = record {
                uuid(id)?;
                r.queue(kind, id, &value, Some(&body))?;
            }
            if let Some(task) = task_ref {
                let id = r.state.goal_id.clone().context("no goal")?;
                let (mut goal, _) = r.record::<Goal>("goal", &id)?;
                goal.task_ref = Some(task);
                r.queue("goal", &id, &goal, None)?;
            }
            r.state.application = state;
            r.persist()?;
            r.flush_writes()
        })
    }
    /// The bounded canonical receipt projection must commit and flush before dispatch.
    pub(crate) fn checkpoint_discussion(
        &mut self,
        state: Value,
        id: &str,
        record: Value,
        body: String,
        expected_revision: Option<String>,
    ) -> Result<()> {
        self.checkpoint_discussion_operation(state, id, record, body, expected_revision, None)
    }
    pub(crate) fn checkpoint_discussion_operation(
        &mut self,
        state: Value,
        id: &str,
        record: Value,
        body: String,
        expected_revision: Option<String>,
        operation: Option<&str>,
    ) -> Result<()> {
        self.mutation(|r| {
            r.flush_writes()?;
            uuid(id)?;
            r.queue_with_supplied_operation(
                "conversation",
                id,
                &record,
                Some(&body),
                Some(expected_revision.as_deref()),
                operation,
            )?;
            r.state.application = state;
            #[cfg(test)]
            if r.discussion_fault.take_if(|phase| *phase == 1).is_some() {
                anyhow::bail!("injected discussion failure before persist");
            }
            r.persist()?;
            #[cfg(test)]
            if r.discussion_fault.take_if(|phase| *phase == 2).is_some() {
                anyhow::bail!("injected discussion failure after persist before canonical flush");
            }
            r.flush_writes()
        })
    }
    pub(crate) fn discussion_pending_write(&self, operation: &str) -> Result<Option<&SourceWrite>> {
        let mut matches = self
            .state
            .pending_writes
            .iter()
            .filter(|w| w.operation_id == operation);
        let found = matches.next();
        ensure!(matches.next().is_none(), "duplicate pending operation UUID");
        Ok(found)
    }
    pub(crate) fn discussion_recovery_record(
        &self,
        operation: &str,
        max_bytes: u64,
    ) -> std::result::Result<tessera_core::source::RecoveryRecord, tessera_core::source::SourceError>
    {
        self.source.recovery_record_bounded(operation, max_bytes)
    }
    pub fn evaluate_result(
        &mut self,
        result_id: String,
        mut evaluation: CriterionEvaluation,
    ) -> Result<Snapshot> {
        self.mutation(|r| {
            r.flush_writes()?;
            ensure!(
                ["passed", "failed", "unverified"].contains(&evaluation.status.as_str()),
                "invalid evaluation status"
            );
            ensure!(
                !evaluation.evaluated_by.trim().is_empty(),
                "reviewer is required"
            );
            valid_time(&evaluation.evaluated_at)?;
            let goal_id = r.state.goal_id.clone().context("no goal")?;
            let stage_id = r.state.stage_id.clone().context("no stage")?;
            let (mut goal, _) = r.record::<Goal>("goal", &goal_id)?;
            let (mut stage, _) = r.record::<Stage>("stage", &stage_id)?;
            ensure!(
                stage.result_ids.last() == Some(&result_id),
                "review requires current stage result"
            );
            let (mut result, _) = r.record::<ResultRecord>("result", &result_id)?;
            ensure!(
                result.goal_id == goal_id && result.stage_id == stage_id,
                "result identity mismatch"
            );
            let criterion = goal
                .criteria
                .iter()
                .find(|c| c.id == evaluation.criterion_id)
                .context("unknown criterion")?;
            ensure!(
                !criterion.requires_human,
                "human criterion needs explicit human acceptance"
            );
            let retained = r
                .state
                .retained_goal
                .as_ref()
                .context("missing retained goal")?;
            let (mapping, _) = parse_document(retained)?;
            let original: Goal = serde_yaml::from_value(serde_yaml::Value::Mapping(mapping))?;
            ensure!(
                original.criteria.contains(criterion),
                "criterion changed since dispatch"
            );
            evaluation.goal_revision = retained.revision.clone();
            ensure!(
                !evaluation.evidence_ids.is_empty(),
                "select saved evidence to review"
            );
            for id in &evaluation.evidence_ids {
                let evidence = result
                    .outcome
                    .evidence
                    .iter_mut()
                    .find(|e| &e.id == id)
                    .context("unknown saved evidence")?;
                ensure!(
                    evidence.kind != "human_acceptance" && !evidence.source.uri.is_empty(),
                    "invalid evidence for this review"
                );
                evidence.status = evaluation.status.clone();
            }
            result
                .outcome
                .criterion_evaluations
                .retain(|e| e.criterion_id != evaluation.criterion_id);
            result.outcome.criterion_evaluations.push(evaluation);
            let complete =
                stage.status != "cancelled" && r.criteria_pass(&goal, &result.outcome)?;
            result.outcome.verification = if complete { "verified" } else { "partial" }.into();
            r.queue("result", &result_id, &result, Some(&result_body(&result)))?;
            if stage.status != "cancelled" {
                stage.status = if complete {
                    "completed"
                } else {
                    "outcome_ready"
                }
                .into();
                goal.status = if complete { "completed" } else { "active" }.into();
                r.queue("stage", &stage_id, &stage, None)?;
                r.queue("goal", &goal_id, &goal, None)?;
            }
            r.attention(
                if complete { "final" } else { "decision" },
                "result review recorded",
            );
            r.persist()?;
            r.flush_writes()?;
            r.snapshot()
        })
    }
    fn persist(&self) -> Result<()> {
        ensure!(
            !self.route_recovery_required,
            "route journal recovery required before persistence"
        );
        let mut temp = tempfile::NamedTempFile::new_in(&self.state_dir)?;
        let mut persisted = self.state.clone();
        if let Some(primary) = persisted.primary_goal_id.clone() {
            persisted.route(&primary)?;
        }
        if persisted.t3_routes.is_some() {
            serde_json::to_writer(
                &mut temp,
                &serde_json::json!({"schema":"tessera-runtime/routes-v2","state":persisted}),
            )?;
        } else {
            serde_json::to_writer(&mut temp, &persisted)?;
        }
        temp.flush()?;
        temp.as_file().sync_all()?;
        temp.persist(self.state_dir.join("state.json"))?;
        #[cfg(test)]
        if let Some(control) = &self.state.suggestions {
            let phase = if control.has_refusals() {
                Some(1)
            } else if control.has_final_receipt() {
                Some(2)
            } else {
                None
            };
            if phase.is_some() && self.suggestions_persist_fault.get() == phase {
                self.suggestions_persist_fault.set(None);
                anyhow::bail!("injected suggestions error after journal rename");
            }
        }
        File::open(&self.state_dir)?.sync_all()?;
        Ok(())
    }
    fn flush_writes(&mut self) -> Result<()> {
        while let Some(write) = self.state.pending_writes.first().cloned() {
            let receipt = self
                .source
                .write(write.clone())
                .context("pending Markdown write needs recovery; do not discard its operation")?;
            #[cfg(test)]
            if let Some(remaining) = &mut self.interrupt_after_write {
                *remaining -= 1;
                if *remaining == 0 {
                    self.interrupt_after_write = None;
                    anyhow::bail!("injected crash after source write before projection receipt");
                }
            }
            self.finalize_proposal_write(&write, &receipt)?;
            self.finalize_inbox_write(&write, &receipt)?;
            self.finalize_plan_write(&write, &receipt)?;
            self.finalize_attention_write(&write, &receipt)?;
            self.state.pending_writes.remove(0);
            self.persist()?;
        }
        self.pump_proposal_feed();
        Ok(())
    }
    fn queue_create_only(
        &mut self,
        kind: &str,
        id: &str,
        record: &impl Serialize,
        body: &str,
    ) -> Result<(String, String)> {
        let bytes = format!("---\n{}---\n{}", serde_yaml::to_string(record)?, body).into_bytes();
        let revision = format!("sha256:{:x}", Sha256::digest(&bytes));
        let operation_id = Uuid::new_v4().to_string();
        self.state.pending_writes.push(SourceWrite {
            schema: SCHEMA.into(),
            operation_id: operation_id.clone(),
            brain_id: self.state.brain_id.clone(),
            path: self.path(kind, id),
            expected_revision: None,
            content_base64: STANDARD.encode(bytes),
        });
        Ok((revision, operation_id))
    }
    fn queue(
        &mut self,
        kind: &str,
        id: &str,
        value: &impl Serialize,
        body: Option<&str>,
    ) -> Result<String> {
        self.queue_with_operation(kind, id, value, body)
            .map(|v| v.0)
    }
    fn queue_with_operation(
        &mut self,
        kind: &str,
        id: &str,
        value: &impl Serialize,
        body: Option<&str>,
    ) -> Result<(String, String)> {
        self.queue_with_operation_checked(kind, id, value, body, None)
    }
    fn queue_with_operation_checked(
        &mut self,
        kind: &str,
        id: &str,
        value: &impl Serialize,
        body: Option<&str>,
        discussion_revision: Option<Option<&str>>,
    ) -> Result<(String, String)> {
        self.queue_with_supplied_operation(kind, id, value, body, discussion_revision, None)
    }
    fn queue_with_supplied_operation(
        &mut self,
        kind: &str,
        id: &str,
        value: &impl Serialize,
        body: Option<&str>,
        discussion_revision: Option<Option<&str>>,
        supplied_operation: Option<&str>,
    ) -> Result<(String, String)> {
        if let Some(operation) = supplied_operation {
            uuid(operation)?;
            ensure!(
                kind == "conversation" && discussion_revision.is_some(),
                "caller operation UUID is only supported for initial Discussion projection"
            );
        }
        let path = self.path(kind, id);
        let read = if discussion_revision.is_some() {
            self.source
                .read_bounded(&path, crate::discussion_context::MAX_CONVERSATION)
        } else {
            self.source.read(&path)
        };
        let previous = match read {
            Ok(s) => Some(s),
            Err(e) if e.code == tessera_core::source::ErrorCode::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(expected) = discussion_revision {
            ensure!(
                previous.as_ref().map(|s| s.revision.as_str()) == expected,
                "chat_context_changed: conversation changed before checkpoint"
            );
        }
        let (mut metadata, existing_body) = if let Some(s) = &previous {
            parse_document(s)?
        } else {
            (serde_yaml::Mapping::new(), String::new())
        };
        let serialized = serde_yaml::to_value(value)?;
        for (k, v) in serialized
            .as_mapping()
            .context("record must be a mapping")?
        {
            metadata.insert(k.clone(), v.clone());
        }
        for (key, value) in [
            ("schema", SCHEMA),
            ("record_type", kind),
            ("brain_id", self.state.brain_id.as_str()),
        ] {
            metadata.insert(
                serde_yaml::Value::String(key.into()),
                serde_yaml::Value::String(value.into()),
            );
        }
        let bytes = format!(
            "---\n{}---\n{}",
            serde_yaml::to_string(&metadata)?,
            body.unwrap_or(&existing_body)
        )
        .into_bytes();
        if discussion_revision.is_some() {
            ensure!(bytes.len() <= crate::discussion_context::MAX_CANDIDATE,
                "chat_context_limit: conversation projection exceeds safe output reserve; start a new conversation");
        }
        let revision = format!("sha256:{:x}", Sha256::digest(&bytes));
        let operation_id = supplied_operation
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.state.pending_writes.push(SourceWrite {
            schema: SCHEMA.into(),
            operation_id: operation_id.clone(),
            brain_id: self.state.brain_id.clone(),
            path,
            expected_revision: previous.map(|s| s.revision),
            content_base64: STANDARD.encode(bytes),
        });
        Ok((revision, operation_id))
    }
    fn record<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<(T, SourceSnapshot)> {
        let s = self.source.read(&self.path(kind, id))?;
        let (metadata, _) = parse_document(&s)?;
        let value = serde_yaml::Value::Mapping(metadata);
        ensure!(
            value["schema"].as_str() == Some(SCHEMA)
                && value["brain_id"].as_str() == Some(&self.state.brain_id)
                && value["record_type"].as_str() == Some(kind)
                && value["id"].as_str() == Some(id),
            "record identity mismatch"
        );
        Ok((serde_yaml::from_value(value)?, s))
    }
    fn create_goal_inner(&mut self, mut goal: Goal, body: String) -> Result<Snapshot> {
        self.flush_writes()?;
        uuid(&goal.id)?;
        ensure!(
            !self.root.join(self.path("goal", &goal.id)).try_exists()?,
            "goal record already exists; recover its identity"
        );
        ensure!(
            self.state.goal_id.is_none(),
            "this POC runner already owns a goal"
        );
        ensure!(
            !goal.title.trim().is_empty() && !goal.criteria.is_empty(),
            "goal needs title and outcome criteria"
        );
        let mut ids = std::collections::BTreeSet::new();
        ensure!(
            goal.criteria.iter().all(|c| !c.id.is_empty()
                && !c.description.trim().is_empty()
                && ids.insert(c.id.clone())),
            "criteria must have unique IDs and descriptions"
        );
        goal.status = "active".into();
        goal.stage_ids.clear();
        self.queue("goal", &goal.id, &goal, Some(&body))?;
        if self.state.primary_goal_id.is_none() {
            self.state.primary_goal_id = Some(goal.id.clone());
        }
        self.state.goal_id = Some(goal.id);
        self.persist()?;
        self.flush_writes()?;
        self.snapshot()
    }
    fn prepare_stage_inner(
        &mut self,
        mut stage: Stage,
        packet: ContextPacket,
        operation_id: String,
        target: BTreeMap<String, Value>,
    ) -> Result<Snapshot> {
        self.flush_writes()?;
        uuid(&stage.id)?;
        uuid(&packet.id)?;
        uuid(&operation_id)?;
        for (kind, id) in [("stage", &stage.id), ("context", &packet.id)] {
            ensure!(
                !self.root.join(self.path(kind, id)).try_exists()?,
                "record identity already exists"
            );
        }
        ensure!(
            !std::iter::once(&self.state.current)
                .chain(self.state.other_goals.values())
                .any(|slot| slot
                    .dispatch
                    .as_ref()
                    .is_some_and(|d| d.envelope.operation_id == operation_id)
                    || slot
                        .previous_stages
                        .values()
                        .any(|s| s.dispatch.envelope.operation_id == operation_id)),
            "dispatch operation identity already exists"
        );
        if let Some(previous) = self.state.dispatch.clone() {
            ensure!(
                ["outcome_ready", "cancelled", "discarded"].contains(&previous.phase.as_str()),
                "a stage is already active for this goal"
            );
            let old = self
                .record::<Stage>("stage", &previous.envelope.stage_id)?
                .0;
            if previous.phase == "discarded" {
                ensure!(
                    packet.previous_result_id == previous.envelope.packet.previous_result_id,
                    "replacement must preserve the discarded preparation's prior-result ancestry"
                );
            } else {
                let result_id = old
                    .result_ids
                    .last()
                    .context("follow-up requires a retained prior result")?;
                ensure!(
                    packet.previous_result_id.as_ref() == Some(result_id),
                    "follow-up must explicitly reference the preceding result"
                );
            }
            ensure!(
                !self.state.previous_stages.contains_key(&stage.id) && stage.id != old.id,
                "stage identity already exists"
            );
            let history = StageHistory {
                dispatch: previous,
                retained_goal: self.state.retained_goal.clone(),
                human_acceptances: self.state.human_acceptances.clone(),
            };
            // Old journals lack attention ownership. Attribute only known stage
            // lifecycle messages; unrelated legacy decisions remain current.
            let legacy_ids: Vec<_> = self
                .state
                .attention
                .iter()
                .filter(|a| stage_lifecycle_attention(a))
                .map(|a| a.id.clone())
                .collect();
            for id in legacy_ids {
                self.state
                    .attention_stage_ids
                    .entry(id)
                    .or_insert_with(|| old.id.clone());
            }
            self.state.previous_stages.insert(old.id, history);
            self.state.human_acceptances.clear();
        } else {
            ensure!(
                packet.previous_result_id.is_none(),
                "initial stage has no previous result"
            );
        }
        let goal_id = self.state.goal_id.clone().context("create a goal first")?;
        let (mut goal, goal_source) = self.record::<Goal>("goal", &goal_id)?;
        ensure!(
            stage.goal_id == goal_id
                && packet.goal_id == goal_id
                && packet.stage_id == stage.id
                && stage.context_id == packet.id,
            "stage/context goal identity mismatch"
        );
        ensure!(
            packet.goal_revision == goal_source.revision,
            "context goal revision is stale"
        );
        ensure!(
            !stage.engine.is_empty()
                && !stage.criterion_ids.is_empty()
                && stage
                    .criterion_ids
                    .iter()
                    .all(|id| goal.criteria.iter().any(|c| &c.id == id)),
            "invalid stage criteria or engine"
        );
        ensure!(
            !packet.goal.trim().is_empty() && !packet.next_step.trim().is_empty(),
            "context needs goal and next step"
        );
        stage.status = "ready".into();
        stage.result_ids.clear();
        let context_revision =
            self.queue("context", &packet.id, &packet, Some(&context_body(&packet)))?;
        self.queue("stage", &stage.id, &stage, Some("\n# Execution stage\n"))?;
        goal.stage_ids.push(stage.id.clone());
        goal.status = "active".into();
        self.queue("goal", &goal.id, &goal, None)?;
        self.state.retained_goal = Some(goal_source);
        self.state.stage_id = Some(stage.id.clone());
        if stage.engine == "t3" {
            self.t3_pin_operation(&operation_id, &target)?;
        }
        self.state.dispatch = Some(Dispatch {
            envelope: StartEnvelope {
                schema: SCHEMA.into(),
                operation_id,
                goal_id,
                stage_id: stage.id,
                context_id: packet.id.clone(),
                context_revision,
                packet,
                target,
            },
            phase: if self.requires_guarded_start() {
                "prepared_edited"
            } else {
                "prepared"
            }
            .into(),
            binding: None,
            sequences: BTreeMap::new(),
            cursors: BTreeMap::new(),
        });
        self.persist()?;
        self.flush_writes()?;
        self.snapshot()
    }
    /// The backend mutex and this runner's process owner serialize record
    /// transactions. Drain their projections before acquiring SourceStore's
    /// writer lock for the coherent saved-knowledge snapshot.
    pub fn export_exact(
        &mut self,
        destination: &std::path::Path,
    ) -> Result<tessera_core::export::ExportReceipt> {
        self.flush_writes()?;
        self.source.export_exact(destination)
    }

    pub fn read_source(&self, path: &str) -> Result<SourceSnapshot> {
        Ok(self.source.read(path)?)
    }
    pub fn write_source(
        &mut self,
        request: SourceWrite,
    ) -> Result<tessera_core::source::WriteReceipt> {
        self.flush_writes()?;
        Ok(self.source.write(request)?)
    }
    pub fn write_source_with_base(
        &mut self,
        request: SourceWrite,
        base: Option<SourceSnapshot>,
    ) -> Result<tessera_core::source::WriteReceipt> {
        self.flush_writes()?;
        Ok(self.source.write_with_base(request, base)?)
    }
    pub fn source_conflict(
        &self,
        brain_id: &str,
        path: &str,
        conflict_id: &str,
    ) -> Result<tessera_core::source::ConflictView> {
        Ok(self.source.conflict(brain_id, path, conflict_id)?)
    }
    pub fn result(&self, id: &str) -> Result<ResultRecord> {
        uuid(id)?;
        Ok(self.record::<ResultRecord>("result", id)?.0)
    }
    pub fn goal_source(&self) -> Result<SourceSnapshot> {
        let id = self.state.goal_id.as_deref().context("no goal")?;
        Ok(self.record::<Goal>("goal", id)?.1)
    }
    fn has_correlated_terminal_result(&self, snapshot: &Snapshot) -> bool {
        let (Some(goal), Some(stage), Some(dispatch), Some(binding)) = (
            &snapshot.goal,
            &snapshot.stage,
            &snapshot.dispatch,
            &snapshot.binding,
        ) else {
            return false;
        };
        if stage.engine != "t3"
            || binding.engine != "t3"
            || !snapshot
                .phase
                .as_deref()
                .is_some_and(|phase| ["outcome_ready", "cancelled"].contains(&phase))
            || !["outcome_ready", "completed", "cancelled"].contains(&stage.status.as_str())
            || stage.goal_id != goal.id
            || dispatch.goal_id != goal.id
            || dispatch.stage_id != stage.id
            || binding.instance_id.trim().is_empty()
            || binding
                .thread_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
            || binding
                .turn_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
        {
            return false;
        }
        stage.result_ids.last().is_some_and(|id| {
            self.result(id).is_ok_and(|result| {
                result.id == *id
                    && result.goal_id == goal.id
                    && result.stage_id == stage.id
                    && result.operation_id == dispatch.operation_id
                    && result.engine_ref == *binding
                    && ["succeeded", "failed", "cancelled"]
                        .contains(&result.outcome.outcome.as_str())
            })
        })
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        let mut snapshot = Snapshot {
            schema: SCHEMA.into(),
            goal: self
                .state
                .goal_id
                .as_deref()
                .map(|id| self.record::<Goal>("goal", id).map(|p| p.0))
                .transpose()?,
            stage: self
                .state
                .stage_id
                .as_deref()
                .map(|id| self.record::<Stage>("stage", id).map(|p| p.0))
                .transpose()?,
            dispatch: self.state.dispatch.as_ref().map(|d| d.envelope.clone()),
            phase: self.state.dispatch.as_ref().map(|d| {
                if d.phase == "prepared_edited" {
                    "prepared".into()
                } else {
                    d.phase.clone()
                }
            }),
            binding: self.state.dispatch.as_ref().and_then(|d| d.binding.clone()),
            attention: self.state.attention.clone(),
            attention_history: self.state.attention.clone(),
            attention_stage_ids: self.state.attention_stage_ids.clone(),
            event_count: self.state.events.len(),
            pending_writes: self.state.pending_writes.len(),
            prepared_guard: self.prepared_guard()?,
            requires_guarded_start: self.requires_guarded_start(),
            can_change_prepared: self
                .state
                .dispatch
                .as_ref()
                .is_some_and(|d| is_prepared(&d.phase) && d.binding.is_none()),
        };
        if let Some(goal) = &mut snapshot.goal {
            if goal.status == "completed" {
                let valid = self.goal_completion_valid(goal, &self.state.current);
                if !valid {
                    goal.status = "blocked".into();
                    snapshot.attention.push(Attention {id:format!("criteria-invalidated-{}",goal.id),kind:"decision".into(),message:"Recorded completion no longer satisfies current criteria; retained evidence is preserved for re-evaluation".into()});
                }
            }
        }
        // Attention is the current action list, not the durable history. Filter
        // only our criterion transition messages, after validating completion
        // against current definitions. Reopened goals must not advertise an old
        // success; unrelated decisions and blockers remain visible.
        let complete = snapshot
            .goal
            .as_ref()
            .is_some_and(|g| g.status == "completed");
        let outcome_saved = snapshot
            .stage
            .as_ref()
            .is_some_and(|s| !s.result_ids.is_empty());
        // Catchup failure is obsolete only after a correlated terminal receipt
        // for this exact dispatch. A result ID alone can be stale or mismatched.
        let catchup_resolved = snapshot.attention.iter().any(|attention| {
            attention.kind == "blocker" && attention.message == "t3_snapshot_changed_during_catchup"
        }) && self.has_correlated_terminal_result(&snapshot);
        snapshot.attention.retain(|attention| {
            if self
                .state
                .attention_stage_ids
                .get(&attention.id)
                .is_some_and(|id| Some(id) != self.state.stage_id.as_ref())
            {
                return false;
            }
            if catchup_resolved
                && attention.kind == "blocker"
                && attention.message == "t3_snapshot_changed_during_catchup"
                && self.state.attention_stage_ids.get(&attention.id) == self.state.stage_id.as_ref()
            {
                return false;
            }
            if outcome_saved && stage_runtime_blocker(attention) {
                return false;
            }
            match (attention.kind.as_str(), attention.message.as_str()) {
                (
                    "decision",
                    "outcome saved; goal criteria remain unmet" | "result review recorded",
                ) => !complete,
                (
                    "final",
                    "goal criteria passed"
                    | "goal criteria passed after explicit human acceptance"
                    | "result review recorded",
                ) => complete,
                _ => true,
            }
        });
        Ok(snapshot)
    }
    fn stage_status(&mut self, status: &str) -> Result<bool> {
        let id = self.state.stage_id.clone().context("no stage")?;
        let (mut stage, _) = self.record::<Stage>("stage", &id)?;
        // Never turn an accepted outcome back into progress.
        if ["outcome_ready", "completed", "cancelled"].contains(&stage.status.as_str()) {
            return Ok(false);
        }
        stage.status = status.into();
        self.queue("stage", &id, &stage, None)?;
        Ok(true)
    }
    fn attention(&mut self, kind: &str, message: impl Into<String>) {
        let message = message.into();
        if self.state.attention.iter().any(|a| {
            a.kind == kind
                && a.message == message
                && self.state.attention_stage_ids.get(&a.id) == self.state.stage_id.as_ref()
        }) {
            return;
        }
        let id = Uuid::new_v4().to_string();
        if let Some(stage_id) = self.state.stage_id.clone() {
            self.state.attention_stage_ids.insert(id.clone(), stage_id);
        }
        self.state.attention.push(Attention {
            id,
            kind: kind.into(),
            message,
        });
    }
    fn start_inner(&mut self, adapter: &mut dyn Adapter) -> Result<Snapshot> {
        self.flush_writes()?;
        let d = self
            .state
            .dispatch
            .as_ref()
            .context("prepare a stage first")?;
        ensure!(
            is_prepared(&d.phase) || d.phase == "not_started",
            "start cannot be retried without proven not_started; reconcile original operation"
        );
        let engine = self
            .record::<Stage>("stage", &d.envelope.stage_id)?
            .0
            .engine;
        ensure!(
            adapter.capabilities().engine == engine,
            "adapter engine mismatch"
        );
        let envelope = d.envelope.clone();
        self.state.dispatch.as_mut().unwrap().phase = "submitting".into();
        self.stage_status("submitting")?;
        self.persist()?;
        self.flush_writes()?;
        let reply = adapter
            .start(&envelope)
            .unwrap_or_else(|e| StartReply::Indeterminate {
                reason: e.to_string(),
            });
        self.apply_start(reply)?;
        self.snapshot()
    }
    fn apply_start(&mut self, reply: StartReply) -> Result<()> {
        if self
            .state
            .dispatch
            .as_ref()
            .is_some_and(|d| ["outcome_ready", "cancelled"].contains(&d.phase.as_str()))
        {
            return Ok(());
        }
        match reply {
            StartReply::Accepted { binding } => {
                self.validate_binding(&binding)?;
                let d = self.state.dispatch.as_mut().unwrap();
                d.binding = Some(binding);
                d.phase = "running".into();
                self.stage_status("running")?;
            }
            StartReply::Rejected { reason } => {
                self.state.dispatch.as_mut().unwrap().phase = "not_started".into();
                self.stage_status("blocked")?;
                self.attention("blocker", reason);
            }
            StartReply::Indeterminate { reason } => {
                self.state.dispatch.as_mut().unwrap().phase = "indeterminate".into();
                self.stage_status("blocked")?;
                self.attention("blocker", reason);
            }
        }
        self.persist()?;
        self.flush_writes()
    }
    fn validate_binding(&self, binding: &EngineRef) -> Result<()> {
        let d = self.state.dispatch.as_ref().context("no dispatch")?;
        let (stage, _) = self.record::<Stage>("stage", &d.envelope.stage_id)?;
        ensure!(
            binding.engine == stage.engine && !binding.instance_id.is_empty(),
            "invalid engine binding"
        );
        if stage.engine == "t3" {
            ensure!(
                binding.thread_id.as_ref().is_some_and(|s| !s.is_empty())
                    && binding.turn_id.as_ref().is_some_and(|s| !s.is_empty()),
                "T3 binding needs exact thread and turn"
            );
        }
        if let Some(existing) = &d.binding {
            ensure!(
                binding == existing,
                "reconciliation cannot silently switch execution identity"
            );
        }
        Ok(())
    }
    fn reconcile_inner(&mut self, adapter: &mut dyn Adapter) -> Result<Snapshot> {
        self.flush_writes()?;
        let d = self.state.dispatch.as_ref().context("no dispatch")?;
        ensure!(
            !is_prepared(&d.phase) && d.phase != "discarded",
            "undispatched or discarded preparation has no provider operation to reconcile"
        );
        let reply = adapter
            .reconcile(&d.envelope, d.binding.as_ref())
            .unwrap_or_else(|e| ReconcileReply::Unknown {
                reason: e.to_string(),
            });
        match reply {
            ReconcileReply::Running { binding, evidence } => {
                ensure!(
                    !evidence.trim().is_empty(),
                    "reconciliation requires evidence"
                );
                self.apply_start(StartReply::Accepted { binding })?;
            }
            ReconcileReply::OutcomeAvailable {
                binding,
                events,
                evidence,
            } => {
                ensure!(
                    !evidence.trim().is_empty(),
                    "reconciliation requires evidence"
                );
                self.apply_start(StartReply::Accepted { binding })?;
                for e in events {
                    self.ingest(e)?;
                }
            }
            ReconcileReply::NotStarted { evidence } => {
                ensure!(
                    !evidence.trim().is_empty(),
                    "not_started requires provider evidence"
                );
                ensure!(
                    self.state.dispatch.as_ref().unwrap().binding.is_none(),
                    "known binding cannot be discarded as not_started"
                );
                self.apply_start(StartReply::Rejected { reason: evidence })?;
            }
            ReconcileReply::Unknown { reason } => {
                self.apply_start(StartReply::Indeterminate { reason })?
            }
        }
        self.snapshot()
    }
    fn poll_inner(&mut self, adapter: &mut dyn Adapter) -> Result<Snapshot> {
        self.flush_writes()?;
        let d = self.state.dispatch.as_ref().context("no dispatch")?;
        let binding = d
            .binding
            .clone()
            .context("unbound execution requires reconciliation")?;
        match adapter.observe(&binding, &d.cursors) {
            Ok(events) => {
                for event in events {
                    self.ingest(event)?;
                }
            }
            Err(e) => {
                self.attention("blocker", format!("observation unavailable: {e}"));
                self.persist()?;
            }
        }
        self.snapshot()
    }
    fn ingest_inner(&mut self, event: EngineEvent) -> Result<Snapshot> {
        self.flush_writes()?;
        ensure!(
            !event.event_id.is_empty() && !event.stream_id.is_empty(),
            "event requires scoped identity"
        );
        valid_time(&event.observed_at)?;
        let d = self.state.dispatch.as_ref().context("no dispatch")?;
        let existing = self.state.events.iter().position(|e| {
            e.event.engine_ref == event.engine_ref
                && e.event.stream_id == event.stream_id
                && e.event.event_id == event.event_id
        });
        if let Some(index) = existing {
            let old = &self.state.events[index];
            ensure!(
                old.event == event,
                "event identity reused with different payload"
            );
            if old.projected || old.reason != "unbound" {
                return self.snapshot();
            }
        }
        // A predecessor's late/duplicate events remain historical. Never project
        // them onto its successor or rewrite an immutable terminal receipt.
        if self
            .state
            .previous_stages
            .values()
            .any(|s| s.dispatch.envelope.operation_id == event.operation_id)
        {
            self.state.events.push(EventIntake {
                event,
                projected: false,
                reason: "retained_predecessor".into(),
            });
            self.persist()?;
            return self.snapshot();
        }
        let correlated = d.envelope.operation_id == event.operation_id
            && d.binding.as_ref() == Some(&event.engine_ref);
        let ordered = event.sequence.is_some_and(|seq| {
            d.sequences
                .get(&event.stream_id)
                .is_none_or(|old| seq > *old)
        });
        let reason = if !correlated {
            "unbound"
        } else if !ordered {
            "stale_or_unordered"
        } else {
            "accepted"
        };
        let before = self.state.clone();
        let intake = EventIntake {
            event: event.clone(),
            projected: correlated && ordered,
            reason: reason.into(),
        };
        if let Some(index) = existing {
            self.state.events[index] = intake;
        } else {
            self.state.events.push(intake);
        }
        // Intake evidence is durable even when state projection is rejected.
        if !correlated || !ordered {
            self.persist()?;
            return self.snapshot();
        }
        let projection = (|| -> Result<()> {
            match &event.payload {
                EventPayload::Status { state } => {
                    ensure!(
                        ["running", "blocked", "cancelled"].contains(&state.as_str()),
                        "invalid status event"
                    );
                    let applied = self.stage_status(state)?;
                    if applied && state == "cancelled" {
                        self.state.dispatch.as_mut().unwrap().phase = "cancelled".into();
                    }
                    if applied && state == "blocked" {
                        self.attention("blocker", "engine needs attention");
                    }
                }
                EventPayload::Attention { message } => self.attention("decision", message.clone()),
                EventPayload::Outcome(outcome) => self.save_outcome(&event, outcome.clone())?,
            }
            Ok(())
        })();
        if let Err(error) = projection {
            self.state = before;
            return Err(error);
        }
        let d = self.state.dispatch.as_mut().unwrap();
        d.sequences
            .insert(event.stream_id.clone(), event.sequence.unwrap());
        if let Some(cursor) = event.cursor {
            d.cursors.insert(event.stream_id, cursor);
        }
        self.persist()?;
        self.flush_writes()?;
        self.snapshot()
    }
    fn save_outcome(&mut self, event: &EngineEvent, mut outcome: Outcome) -> Result<()> {
        ensure!(
            ["succeeded", "failed", "cancelled", "unknown"].contains(&outcome.outcome.as_str()),
            "invalid outcome"
        );
        let d = self.state.dispatch.as_ref().unwrap();
        let (goal_id, stage_id, operation_id) = (
            d.envelope.goal_id.clone(),
            d.envelope.stage_id.clone(),
            d.envelope.operation_id.clone(),
        );
        let (mut goal, _) = self.record::<Goal>("goal", &goal_id)?;
        let (mut stage, _) = self.record::<Stage>("stage", &stage_id)?;
        if !stage.result_ids.is_empty() {
            return Ok(());
        }
        let complete = stage.status != "cancelled" && self.criteria_pass(&goal, &outcome)?;
        // Verification is evaluated from evidence, never accepted from the engine label.
        outcome.verification = if complete {
            "verified"
        } else if outcome.evidence.iter().any(|e| e.status == "failed") {
            "failed"
        } else {
            "unverified"
        }
        .into();
        let result = ResultRecord {
            id: Uuid::new_v4().to_string(),
            goal_id: goal_id.clone(),
            stage_id: stage_id.clone(),
            operation_id,
            engine_ref: event.engine_ref.clone(),
            received_at: event.observed_at.clone(),
            outcome,
        };
        let (_, result_source) =
            self.queue_with_operation("result", &result.id, &result, Some(&result_body(&result)))?;
        stage.result_ids.push(result.id.clone());
        if stage.status != "cancelled" {
            stage.status = if complete {
                "completed"
            } else {
                "outcome_ready"
            }
            .into();
        }
        if complete {
            goal.status = "completed".into();
        }
        let (_, stage_source) = self.queue_with_operation("stage", &stage_id, &stage, None)?;
        let (_, goal_source) = self.queue_with_operation("goal", &goal_id, &goal, None)?;
        self.stage_proposal_candidate(
            crate::proposals::TriggerKind::Result,
            &result.id,
            Some(goal_id.clone()),
            &result.received_at,
            &[result_source, stage_source, goal_source],
        )?;
        self.state.dispatch.as_mut().unwrap().phase = if stage.status == "cancelled" {
            "cancelled"
        } else {
            "outcome_ready"
        }
        .into();
        self.attention(
            if complete { "final" } else { "decision" },
            if complete {
                "goal criteria passed"
            } else {
                "outcome saved; goal criteria remain unmet"
            },
        );
        Ok(())
    }
    fn criteria_pass(&self, goal: &Goal, outcome: &Outcome) -> Result<bool> {
        self.criteria_pass_in_state(goal, outcome, &self.state.current)
    }
    fn criteria_pass_in_state(
        &self,
        goal: &Goal,
        outcome: &Outcome,
        state: &GoalState,
    ) -> Result<bool> {
        let mut ids = std::collections::BTreeSet::new();
        if goal.criteria.is_empty()
            || !goal.criteria.iter().all(|c| {
                !c.id.is_empty() && !c.description.trim().is_empty() && ids.insert(c.id.clone())
            })
        {
            return Ok(false);
        }
        if outcome.outcome != "succeeded" {
            return Ok(false);
        }
        let retained = state
            .retained_goal
            .as_ref()
            .context("missing retained goal snapshot")?;
        let (mapping, _) = parse_document(retained)?;
        let original: Goal = serde_yaml::from_value(serde_yaml::Value::Mapping(mapping))?;
        for criterion in &goal.criteria {
            if !original.criteria.contains(criterion) {
                return Ok(false);
            }
            if criterion.requires_human {
                let Some(acceptance) = state.human_acceptances.get(&criterion.id) else {
                    return Ok(false);
                };
                let (canonical, _) = self.record::<HumanAcceptance>("evidence", &acceptance.id)?;
                if canonical.goal_id != goal.id
                    || canonical.definition != *criterion
                    || canonical.status != "accepted"
                    || canonical.actor.trim().is_empty()
                    || canonical.source.uri.is_empty()
                    || valid_time(&canonical.observed_at).is_err()
                {
                    return Ok(false);
                }
                continue;
            }
            let Some(evaluation) = outcome.criterion_evaluations.iter().find(|e| {
                e.criterion_id == criterion.id
                    && e.goal_revision == retained.revision
                    && e.status == "passed"
            }) else {
                return Ok(false);
            };
            if evaluation.evaluated_by.trim().is_empty()
                || valid_time(&evaluation.evaluated_at).is_err()
                || evaluation.evidence_ids.is_empty()
            {
                return Ok(false);
            }
            if !evaluation.evidence_ids.iter().all(|id| {
                outcome.evidence.iter().any(|e| {
                    &e.id == id
                        && e.status == "passed"
                        && e.kind != "human_acceptance"
                        && !e.source.uri.is_empty()
                        && valid_time(&e.observed_at).is_ok()
                })
            }) {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Only an explicit operator-facing action calls this. Engine events cannot
    /// manufacture it; the actor/time/source are supplied by the actual receipt.
    fn accept_human_inner(
        &mut self,
        criterion_id: String,
        actor: String,
        observed_at: String,
        source: SourceRef,
    ) -> Result<Snapshot> {
        self.flush_writes()?;
        ensure!(
            !actor.trim().is_empty() && !source.uri.is_empty(),
            "human receipt needs actor/source"
        );
        valid_time(&observed_at)?;
        let goal_id = self.state.goal_id.as_ref().context("no goal")?;
        let (goal, _) = self.record::<Goal>("goal", goal_id)?;
        let criterion = goal
            .criteria
            .iter()
            .find(|c| c.id == criterion_id && c.requires_human)
            .context("no human criterion")?
            .clone();
        let acceptance = HumanAcceptance {
            id: Uuid::new_v4().to_string(),
            goal_id: goal.id.clone(),
            status: "accepted".into(),
            definition: criterion,
            actor,
            observed_at,
            source,
        };
        self.queue(
            "evidence",
            &acceptance.id,
            &acceptance,
            Some("\n# Explicit human acceptance\n"),
        )?;
        self.state
            .human_acceptances
            .insert(criterion_id.clone(), acceptance.clone());
        self.persist()?;
        self.flush_writes()?;
        // Re-evaluate the existing saved outcome; no engine redispatch and no
        // invented second engine outcome is necessary for actual human review.
        if let Some(stage_id) = self.state.stage_id.clone() {
            let (mut stage, _) = self.record::<Stage>("stage", &stage_id)?;
            if let Some(result_id) = stage.result_ids.last().cloned() {
                let (mut result, _) = self.record::<ResultRecord>("result", &result_id)?;
                let source = self.source.read(&self.path("evidence", &acceptance.id))?;
                result.outcome.evidence.push(Evidence {
                    id: acceptance.id.clone(),
                    kind: "human_acceptance".into(),
                    source: SourceRef {
                        uri: format!("brain://{}/{}", self.state.brain_id, source.path),
                        revision: Some(source.revision),
                        locator: None,
                    },
                    description: acceptance.definition.description.clone(),
                    observed_at: acceptance.observed_at.clone(),
                    status: "passed".into(),
                });
                result
                    .outcome
                    .criterion_evaluations
                    .retain(|e| e.criterion_id != criterion_id);
                result
                    .outcome
                    .criterion_evaluations
                    .push(CriterionEvaluation {
                        criterion_id,
                        goal_revision: self
                            .state
                            .retained_goal
                            .as_ref()
                            .context("missing goal snapshot")?
                            .revision
                            .clone(),
                        status: "passed".into(),
                        evidence_ids: vec![acceptance.id],
                        evaluated_by: acceptance.actor,
                        evaluated_at: acceptance.observed_at,
                    });
                let complete =
                    stage.status != "cancelled" && self.criteria_pass(&goal, &result.outcome)?;
                result.outcome.verification = if complete { "verified" } else { "partial" }.into();
                self.queue("result", &result_id, &result, Some(&result_body(&result)))?;
                if complete {
                    let mut goal = goal;
                    goal.status = "completed".into();
                    stage.status = "completed".into();
                    self.queue("stage", &stage_id, &stage, None)?;
                    self.queue("goal", &goal.id, &goal, None)?;
                    self.attention(
                        "final",
                        "goal criteria passed after explicit human acceptance",
                    );
                }
                self.persist()?;
                self.flush_writes()?;
            }
        }
        self.snapshot()
    }
    pub(crate) fn operational_root(&self) -> &std::path::Path {
        &self.state_dir
    }
    pub(crate) fn has_active_provider_work(&self) -> bool {
        self.state.has_active_provider_work()
    }
    pub(crate) fn has_external_work(&self) -> bool {
        self.state.has_external_work()
    }
    pub(crate) fn current_has_external_work(&self) -> bool {
        self.state.current.has_external_work()
    }

    pub fn active_engine(&self) -> Option<String> {
        let d = self.state.dispatch.as_ref()?;
        if !["running", "submitting", "indeterminate"].contains(&d.phase.as_str()) {
            return None;
        }
        self.record::<Stage>("stage", &d.envelope.stage_id)
            .ok()
            .map(|p| p.0.engine)
    }
}
fn result_body(result: &ResultRecord) -> String {
    let mut body = format!(
        "\n# Stage result and evidence\n\n{}\n\nVerification: {}\n\n## Evidence\n\n",
        result.outcome.summary, result.outcome.verification
    );
    for e in &result.outcome.evidence {
        body.push_str(&format!(
            "- [{}]({}) — {} ({})\n",
            e.id, e.source.uri, e.description, e.status
        ));
    }
    body.push_str("\n## Sources\n\n");
    for s in &result.outcome.sources {
        body.push_str(&format!("- [{}]({})\n", s.uri, s.uri));
    }
    body
}
pub(crate) fn parse_document(snapshot: &SourceSnapshot) -> Result<(serde_yaml::Mapping, String)> {
    let bytes = STANDARD.decode(&snapshot.content_base64)?;
    let text = String::from_utf8(bytes).context("record is not lossless UTF-8; preserve source")?;
    let front = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"))
        .context("record needs YAML frontmatter")?;
    let mut offset = 0;
    for line in front.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let body = front[offset + line.len()..].to_owned();
            return Ok((serde_yaml::from_str(&front[..offset])?, body));
        }
        offset += line.len();
    }
    anyhow::bail!("unterminated frontmatter")
}

fn uuid(id: &str) -> Result<()> {
    ensure!(
        Uuid::parse_str(id)?.to_string() == id,
        "ID must be canonical UUID"
    );
    Ok(())
}
fn valid_time(value: &str) -> Result<()> {
    let t = time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)?;
    ensure!(t.offset().is_utc(), "timestamp must be UTC");
    Ok(())
}

/// Decode a journal without opening a Runner, taking a lock, recovering or writing.
pub(crate) fn recovery_facts(
    bytes: &[u8],
    saved: &crate::application::ApplicationConfig,
    candidate: &crate::application::ApplicationConfig,
) -> Result<(bool, bool, bool, bool)> {
    let state: State = decode_state(bytes)?;
    ensure!(
        !state.brain_id.is_empty() && !state.records_dir.is_empty(),
        "missing journal identity"
    );
    let mut retained = state.has_external_work();
    let mut active = state.has_active_provider_work();
    let expected = crate::application::Application::provider_identity(saved);
    let proposed = crate::application::Application::provider_identity(candidate);
    let mut consistent = true;
    let mut allowed = state
        .maestro_journal
        .guard_config(candidate.maestro.as_ref())
        .is_ok();
    for slot in std::iter::once(&state.current).chain(state.other_goals.values()) {
        let identity =
            crate::application::Application::retained_provider_identity(&slot.application)?;
        // Empty unbound goals have no continuity to prove. A goal with external
        // history needs its original routing pin; historical model changes are
        // supported and do not invalidate that routing identity.
        let slot_retained = slot.dispatch.is_some()
            || !slot.previous_stages.is_empty()
            || slot.application.get("task").is_some_and(|v| !v.is_null())
            || ["mutations", "conversations"].iter().any(|key| {
                slot.application
                    .get(key)
                    .and_then(Value::as_object)
                    .is_some_and(|v| !v.is_empty())
            });
        if slot_retained && identity.is_null() {
            consistent = false;
        }
        if !identity.is_null()
            && crate::application::Application::routing_identity(&identity)
                != crate::application::Application::routing_identity(&expected)
        {
            consistent = false;
        }
        for dispatch in slot
            .dispatch
            .iter()
            .chain(slot.previous_stages.values().map(|s| &s.dispatch))
        {
            retained = true;
            active |= !["outcome_ready", "cancelled"].contains(&dispatch.phase.as_str());
            let target = &dispatch.envelope.target;
            let t3 = dispatch.binding.as_ref().is_some_and(|b| b.engine == "t3")
                || target.contains_key("environment_id")
                || target.contains_key("project_id");
            if t3 {
                let saved_t3 = saved.t3.as_ref();
                consistent &= saved_t3.is_some_and(|s| {
                    target.get("environment_id").and_then(Value::as_str)
                        == Some(s.environment_id.as_str())
                        && target.get("project_id").and_then(Value::as_str)
                            == Some(s.project_id.as_str())
                        && dispatch
                            .binding
                            .as_ref()
                            .is_none_or(|b| b.engine == "t3" && b.instance_id == s.environment_id)
                });
            }
        }
        allowed &= crate::application::Application::target_change_allowed(
            &identity, &proposed, retained, active,
        );
    }
    Ok((retained, active, consistent, allowed))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u32) -> String {
        format!("01000000-0000-4000-8000-{n:012}")
    }
    #[test]
    fn context_review_includes_interrupted_response_visible_to_engine() {
        let packet: ContextPacket = serde_json::from_value(serde_json::json!({
            "id":"context", "goal_id":"goal", "stage_id":"stage", "goal_revision":"revision",
            "goal":"Plan supplies", "decisions":[], "constraints":[], "sources":[],
            "previous_result_id":null, "next_step":"Review before execution",
            "conversation":{"messages":[], "partial":"Buy one marker; tape price is still unresolved.",
                "status":"interrupted", "source_snapshots":[{"content_base64":"hidden transport"}]}
        })).unwrap();
        let body = context_body(&packet);
        assert!(body.contains("Partial assistant response (interrupted)"));
        assert!(body.contains("Buy one marker; tape price is still unresolved."));
        assert!(!body.contains("content_base64"));
    }

    #[test]
    fn crash_after_result_before_projection_recovers_one_result_and_completion() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        fs::create_dir(temp.path().join("runtime")).unwrap();
        let config = || RunnerConfig {
            brain_id: id(1),
            root: temp.path().join("brain"),
            operational_dir: temp.path().join("runtime"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let mut r = Runner::open(config()).unwrap();
        r.create_goal(
            Goal {
                id: id(2),
                title: "Test".into(),
                status: "draft".into(),
                criteria: vec![Criterion {
                    id: "C1".into(),
                    description: "Proof".into(),
                    requires_human: false,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "\n# Goal\n".into(),
        )
        .unwrap();
        let goal_revision = r.goal_source().unwrap().revision;
        r.prepare_stage(
            Stage {
                id: id(3),
                goal_id: id(2),
                engine: "t3".into(),
                status: "ready".into(),
                criterion_ids: vec!["C1".into()],
                context_id: id(4),
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            ContextPacket {
                id: id(4),
                goal_id: id(2),
                stage_id: id(3),
                goal_revision: goal_revision.clone(),
                goal: "Test".into(),
                decisions: vec![],
                constraints: vec![],
                sources: vec![],
                previous_result_id: None,
                next_step: "Do stage".into(),
                extra: BTreeMap::new(),
            },
            id(5),
            BTreeMap::new(),
        )
        .unwrap();
        let binding = EngineRef {
            engine: "t3".into(),
            instance_id: "fixture".into(),
            thread_id: Some("thread".into()),
            turn_id: Some("turn".into()),
            task_id: None,
        };
        r.apply_start(StartReply::Accepted {
            binding: binding.clone(),
        })
        .unwrap();
        let when = "2026-09-05T12:00:00Z";
        let event = EngineEvent {
            operation_id: id(5),
            engine_ref: binding,
            event_id: "finished".into(),
            stream_id: "thread/turn".into(),
            sequence: Some(1),
            cursor: Some("1".into()),
            observed_at: when.into(),
            payload: EventPayload::Outcome(Outcome {
                outcome: "succeeded".into(),
                summary: "Done with proof".into(),
                sources: vec![],
                evidence: vec![Evidence {
                    id: "E1".into(),
                    kind: "artifact".into(),
                    source: SourceRef {
                        uri: "fixture://proof".into(),
                        revision: None,
                        locator: None,
                    },
                    description: "Proof".into(),
                    observed_at: when.into(),
                    status: "passed".into(),
                }],
                verification: "unverified".into(),
                criterion_evaluations: vec![CriterionEvaluation {
                    criterion_id: "C1".into(),
                    goal_revision,
                    status: "passed".into(),
                    evidence_ids: vec!["E1".into()],
                    evaluated_by: "test verifier".into(),
                    evaluated_at: when.into(),
                }],
            }),
        };
        r.interrupt_after_write = Some(1);
        assert!(r
            .ingest(event.clone())
            .unwrap_err()
            .to_string()
            .contains("injected crash"));
        // Positive controls: the result exists, but stage/goal projection has not
        // happened. Its source operation and all later writes are still queued.
        assert_eq!(r.state.pending_writes.len(), 3);
        let files = || {
            fs::read_dir(temp.path().join("brain/records"))
                .unwrap()
                .filter(|e| {
                    e.as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("result-")
                })
                .count()
        };
        assert_eq!(files(), 1);
        assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "active");
        drop(r);
        let mut r = Runner::open(config()).unwrap();
        assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "completed");
        assert_eq!(r.state.pending_writes.len(), 0);
        assert_eq!(files(), 1);
        let result_id = r.snapshot().unwrap().stage.unwrap().result_ids[0].clone();
        assert_eq!(
            r.ingest(event).unwrap().stage.unwrap().result_ids,
            vec![result_id]
        );
        assert_eq!(files(), 1);
    }
    #[test]
    fn proposal_feed_result_waits_for_every_source_batch_checkpoint() {
        for crash_after in 1..=3 {
            let temp = tempfile::tempdir().unwrap();
            fs::create_dir_all(temp.path().join("brain/records")).unwrap();
            fs::create_dir(temp.path().join("runtime")).unwrap();
            let config = || RunnerConfig {
                brain_id: id(1),
                root: temp.path().join("brain"),
                operational_dir: temp.path().join("runtime"),
                records_dir: "records".into(),
                boundary: WriteBoundary::Managed,
            };
            let mut r = Runner::open(config()).unwrap();
            r.create_goal(
                Goal {
                    id: id(2),
                    title: "Test".into(),
                    status: "draft".into(),
                    criteria: vec![Criterion {
                        id: "C1".into(),
                        description: "Proof".into(),
                        requires_human: false,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: BTreeMap::new(),
                },
                "\n# Goal\n".into(),
            )
            .unwrap();
            let goal_revision = r.goal_source().unwrap().revision;
            r.prepare_stage(
                Stage {
                    id: id(3),
                    goal_id: id(2),
                    engine: "t3".into(),
                    status: "ready".into(),
                    criterion_ids: vec!["C1".into()],
                    context_id: id(4),
                    result_ids: vec![],
                    extra: BTreeMap::new(),
                },
                ContextPacket {
                    id: id(4),
                    goal_id: id(2),
                    stage_id: id(3),
                    goal_revision: goal_revision.clone(),
                    goal: "Test".into(),
                    decisions: vec![],
                    constraints: vec![],
                    sources: vec![],
                    previous_result_id: None,
                    next_step: "Do stage".into(),
                    extra: BTreeMap::new(),
                },
                id(5),
                BTreeMap::new(),
            )
            .unwrap();
            let binding = EngineRef {
                engine: "t3".into(),
                instance_id: "fixture".into(),
                thread_id: Some("thread".into()),
                turn_id: Some("turn".into()),
                task_id: None,
            };
            r.apply_start(StartReply::Accepted {
                binding: binding.clone(),
            })
            .unwrap();
            let when = "2026-09-05T12:00:00Z";
            let event = EngineEvent {
                operation_id: id(5),
                engine_ref: binding,
                event_id: "finished".into(),
                stream_id: "thread/turn".into(),
                sequence: Some(1),
                cursor: Some("1".into()),
                observed_at: when.into(),
                payload: EventPayload::Outcome(Outcome {
                    outcome: "succeeded".into(),
                    summary: "Done with proof".into(),
                    sources: vec![],
                    evidence: vec![Evidence {
                        id: "E1".into(),
                        kind: "artifact".into(),
                        source: SourceRef {
                            uri: "fixture://proof".into(),
                            revision: None,
                            locator: None,
                        },
                        description: "Proof".into(),
                        observed_at: when.into(),
                        status: "passed".into(),
                    }],
                    verification: "unverified".into(),
                    criterion_evaluations: vec![CriterionEvaluation {
                        criterion_id: "C1".into(),
                        goal_revision,
                        status: "passed".into(),
                        evidence_ids: vec!["E1".into()],
                        evaluated_by: "test verifier".into(),
                        evaluated_at: when.into(),
                    }],
                }),
            };
            r.enroll_proposal_feed(1).unwrap();
            r.interrupt_after_write = Some(crash_after);
            assert!(r
                .ingest(event.clone())
                .unwrap_err()
                .to_string()
                .contains("injected crash"));
            // Positive controls: the result exists, but stage/goal projection has not
            // happened. Its source operation and all later writes are still queued.
            assert_eq!(r.state.pending_writes.len(), 4 - crash_after);
            assert_eq!(
                serde_json::to_value(&r.state).unwrap()["proposal_feed"]["published"],
                0
            );
            let files = || {
                fs::read_dir(temp.path().join("brain/records"))
                    .unwrap()
                    .filter(|e| {
                        e.as_ref()
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with("result-")
                    })
                    .count()
            };
            assert_eq!(files(), 1);
            if crash_after < 3 {
                assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "active");
            }
            drop(r);
            let mut r = Runner::open(config()).unwrap();
            assert_eq!(r.snapshot().unwrap().goal.unwrap().status, "completed");
            assert_eq!(r.state.pending_writes.len(), 0);
            assert_eq!(
                serde_json::to_value(&r.state).unwrap()["proposal_feed"]["acknowledged"],
                1
            );
            assert_eq!(files(), 1);
            let result_id = r.snapshot().unwrap().stage.unwrap().result_ids[0].clone();
            assert_eq!(
                r.ingest(event).unwrap().stage.unwrap().result_ids,
                vec![result_id]
            );
            assert_eq!(files(), 1);
        }
    }
    #[test]
    fn prepared_change_recovery_finishes_every_partial_markdown_projection_once() {
        for (discard, last_write) in [(false, 4), (true, 1)] {
            for fail_after in 1..=last_write {
                let temp = tempfile::tempdir().unwrap();
                fs::create_dir_all(temp.path().join("brain/records")).unwrap();
                fs::create_dir(temp.path().join("runtime")).unwrap();
                let config = || RunnerConfig {
                    brain_id: id(1),
                    root: temp.path().join("brain"),
                    operational_dir: temp.path().join("runtime"),
                    records_dir: "records".into(),
                    boundary: WriteBoundary::Managed,
                };
                let mut r = Runner::open(config()).unwrap();
                let goal: Goal = serde_json::from_value(serde_json::json!({
                    "id":id(2),"title":"Crash recovery","status":"active",
                    "criteria":[{"id":"C1","description":"Keep context","requires_human":false}],
                    "stage_ids":[],"task_ref":null
                }))
                .unwrap();
                r.create_goal(goal, "\n# Goal\n".into()).unwrap();
                let stage: Stage = serde_json::from_value(serde_json::json!({
                    "id":id(3),"goal_id":id(2),"engine":"t3","status":"ready",
                    "criterion_ids":["C1"],"context_id":id(4),"result_ids":[]
                }))
                .unwrap();
                let packet: ContextPacket = serde_json::from_value(serde_json::json!({
                    "id":id(4),"goal_id":id(2),"stage_id":id(3),"goal_revision":r.goal_source().unwrap().revision,
                    "goal":"Crash recovery","decisions":[],"constraints":[],"sources":[],
                    "previous_result_id":null,"next_step":"Original"
                })).unwrap();
                r.prepare_stage(stage, packet, id(5), BTreeMap::new())
                    .unwrap();
                let original = r.read_source(&r.path("context", &id(4))).unwrap();
                let request = PreparedChangeRequest {
                    operation_id: id(98),
                    goal_id: id(2),
                    expected: r.snapshot().unwrap().prepared_guard.unwrap(),
                    change: if discard {
                        PreparedChange::Discard
                    } else {
                        PreparedChange::Revise {
                            next_step: "Complete corrected instruction".into(),
                        }
                    },
                };
                r.interrupt_after_write = Some(fail_after);
                let error = r.change_prepared(request.clone()).unwrap_err();
                assert!(error.to_string().contains("injected crash"));
                assert!(error.downcast_ref::<PreparedChangeNotRecorded>().is_none());
                drop(r);
                let mut r = Runner::open(config()).unwrap();
                let receipt = r.change_prepared(request.clone()).unwrap();
                assert!(r.state.pending_writes.is_empty());
                assert_eq!(r.read_source(&original.path).unwrap(), original);
                assert_eq!(r.stages().unwrap().len(), if discard { 1 } else { 2 });
                assert_eq!(
                    r.snapshot().unwrap().phase.as_deref(),
                    Some(if discard { "discarded" } else { "prepared" })
                );
                drop(r);
                assert_eq!(
                    Runner::open(config())
                        .unwrap()
                        .change_prepared(request)
                        .unwrap(),
                    receipt
                );
            }
        }
    }
}

mod proposal_adopt;

#[cfg(test)]
#[path = "runtime/discussion_context_tests.rs"]
mod discussion_context_tests;

#[cfg(test)]
#[path = "runtime/discussion_send_tests.rs"]
mod discussion_send_tests;
