//! Backend application journal and explicitly configured provider wiring.
//! Durable application intent shares Runner's single transaction/source owner.
use crate::{chat::*, t3::*, todoist::*, *};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_core::source::SourceSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, time::Duration};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConfig {
    #[serde(default)]
    pub maestro: Option<crate::maestro::Settings>,
    pub actor: String,
    pub chat: Option<ChatSettings>,
    pub todoist: Option<TodoistSettings>,
    pub t3: Option<T3Settings>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSettings {
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoistSettings {
    pub base_url: String,
    pub instance_id: String,
    pub token_env: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct T3Settings {
    pub base_url: String,
    pub token_env: String,
    pub environment_id: String,
    pub project_id: String,
    pub model_instance_id: String,
    pub model: String,
    pub runtime_mode: String,
    pub interaction_mode: String,
}
pub(crate) fn credential(name: &str) -> Result<String> {
    if let Some(path) = name.strip_prefix("file:") {
        ensure!(
            Path::new(path).is_absolute(),
            "credential file reference must be absolute"
        );
        let value =
            std::fs::read_to_string(path).context("configured credential file is unavailable")?;
        let value = value.trim().to_string();
        ensure!(
            !value.is_empty() && !value.contains(['\n', '\r']),
            "credential file must contain one nonempty token"
        );
        return Ok(value);
    }
    let name = name.strip_prefix("env:").unwrap_or(name);
    ensure!(
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "invalid credential environment variable reference"
    );
    let value =
        std::env::var(name).context("configured credential environment variable is unavailable")?;
    ensure!(!value.trim().is_empty(), "configured credential is empty");
    Ok(value)
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub text: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub goal_id: String,
    pub path: String,
    pub status: String,
    pub messages: Vec<Message>,
    pub partial: String,
    pub error: Option<String>,
    pub sources: Vec<SourceSnapshot>,
    pub request: ChatRequest,
}
struct DiscussionCandidate {
    state: Value,
    record: Value,
    projection: String,
    revision: Option<String>,
    prepared: crate::discussion_context::Prepared,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Journal {
    conversations: BTreeMap<String, Conversation>,
    #[serde(default)]
    selected_source_paths: Vec<String>,
    #[serde(default)]
    selected_conversation_id: Option<String>,
    #[serde(default)]
    reviewed_packet_id: Option<String>,
    mutations: BTreeMap<String, TaskIntent>,
    task: Option<TaskState>,
    #[serde(default)]
    task_generation: u64,
    #[serde(default)]
    provider_identity: Value,
}
#[derive(Clone, Serialize, Deserialize)]
struct TaskIntent {
    #[serde(default)]
    generation: u64,
    prepared: PreparedMutation,
    outcome: Option<MutationOutcome>,
}
#[derive(Clone, Serialize, Deserialize)]
struct TaskState {
    id: String,
    binding: TaskBinding,
    content: String,
    status: String,
    observation: Option<TaskObservation>,
}
impl TaskState {
    fn view(&self) -> Value {
        json!({"task_id":self.binding.external_id,"provider":self.binding.provider,"instance_id":self.binding.instance_id,"goal_id":self.binding.goal_id,"content":self.content,"status":self.status,"url":format!("https://app.todoist.com/app/task/{}", self.binding.external_id)})
    }
}
pub struct Application {
    pub chat: Option<ChatConfig>,
    todoist: Option<Todoist>,
    t3_target: Option<BTreeMap<String, Value>>,
    actor: String,
    pub(crate) settings: ApplicationConfig,
    pub(crate) connection_states: BTreeMap<String, Value>,
}
impl Application {
    pub fn unconfigured() -> Self {
        Self {
            chat: None,
            todoist: None,
            t3_target: None,
            actor: "local operator".into(),
            settings: ApplicationConfig {
                maestro: None,
                actor: "local operator".into(),
                chat: None,
                todoist: None,
                t3: None,
            },
            connection_states: BTreeMap::new(),
        }
    }
    pub fn configure(
        config: ApplicationConfig,
        operational: &Path,
        runner: &mut Runner,
    ) -> Result<(Self, crate::service::Adapters)> {
        Self::configure_runtime(config, operational, runner, true)
    }
    pub(crate) fn configure_runtime(
        config: ApplicationConfig,
        operational: &Path,
        runner: &mut Runner,
        recover: bool,
    ) -> Result<(Self, crate::service::Adapters)> {
        ensure!(
            !config.actor.trim().is_empty(),
            "configured actor is required"
        );
        runner.maestro_guard_config(config.maestro.as_ref())?;
        runner.t3_settings_guard(config.t3.as_ref())?;
        if let Some(c) = &config.maestro {
            c.validate()?;
        }
        let settings = config.clone();
        let mut connection_states = BTreeMap::new();
        if config.maestro.is_some() {
            connection_states.insert("maestro".into(), crate::settings::state("configured", "Maestro observation configured; use Check connection to discover existing work."));
        }
        let identity = Self::provider_identity(&config);
        let mut state = Self::journal(runner)?;
        ensure!(
            Self::target_change_allowed(&state.provider_identity, &identity, runner.has_external_work(), runner.has_active_provider_work()),
            "Provider target change is blocked by existing work. Restore the account/project; model changes require all chats and stages to finish."
        );
        let chat = config.chat.as_ref().and_then(|c| {
            let result = (|| {
                let runtime = ChatConfig {
                    api_key: credential(&c.api_key_env)?,
                    base_url: c.base_url.clone(),
                    model: c.model.clone(),
                    idle_timeout: Duration::from_secs(30),
                };
                ChatClient::new(runtime.clone()).map_err(anyhow::Error::msg)?;
                Ok::<_, anyhow::Error>(runtime)
            })();
            connection_states.insert("chat".into(), crate::settings::configuration_state(&result));
            result.ok()
        });
        let todoist = config.todoist.as_ref().and_then(|c| {
            let result = (|| {
                Ok::<_, anyhow::Error>(Todoist::new(TodoistConfig::new(
                    c.instance_id.clone(),
                    c.base_url.clone(),
                    credential(&c.token_env)?,
                ))?)
            })();
            connection_states.insert(
                "todoist".into(),
                crate::settings::configuration_state(&result),
            );
            result.ok()
        });
        let mut adapters = crate::service::Adapters::new();
        let mut target = None;
        if let Some(c) = &config.t3 {
            let result = (|| {
                let receipt_dir = std::fs::canonicalize(operational)?.join("t3-receipts");
                std::fs::create_dir_all(&receipt_dir)?;
                T3Adapter::new(T3Config {
                    base_url: c.base_url.clone(),
                    bearer_token: credential(&c.token_env)?,
                    environment_id: c.environment_id.clone(),
                    project_id: c.project_id.clone(),
                    model_instance_id: c.model_instance_id.clone(),
                    model: c.model.clone(),
                    runtime_mode: c.runtime_mode.clone(),
                    interaction_mode: c.interaction_mode.clone(),
                    timeout: Duration::from_secs(30),
                    receipt_dir,
                })
            })();
            connection_states.insert("t3".into(), crate::settings::configuration_state(&result));
            if let Ok(adapter) = result {
                target = Some(BTreeMap::from([
                    ("environment_id".into(), json!(c.environment_id)),
                    ("project_id".into(), json!(c.project_id)),
                ]));
                adapters.insert("t3".into(), Box::new(adapter) as Box<dyn Adapter>);
            }
        }
        if recover {
            Self::bind_provider_identity(&mut state, identity, runner);
            runner.checkpoint_application(serde_json::to_value(state)?, None, None)?;
        }
        let app = Self {
            chat,
            todoist,
            t3_target: target,
            actor: config.actor,
            settings,
            connection_states,
        };
        if recover {
            app.recover(runner)?;
        }
        Ok((app, adapters))
    }
    pub(crate) fn provider_identity(config: &ApplicationConfig) -> Value {
        json!({"chat":config.chat.as_ref().map(|c| json!({"base_url":c.base_url,"model":c.model})),"todoist":config.todoist.as_ref().map(|c| json!({"base_url":c.base_url,"instance_id":c.instance_id})),"t3":config.t3.as_ref().map(|c| json!({"base_url":c.base_url,"environment_id":c.environment_id,"project_id":c.project_id,"model_instance_id":c.model_instance_id,"model":c.model,"runtime_mode":c.runtime_mode,"interaction_mode":c.interaction_mode}))})
    }
    pub(crate) fn routing_identity(identity: &Value) -> Value {
        let mut routing = identity.clone();
        for (provider, fields) in [
            ("chat", &["model"][..]),
            (
                "t3",
                &[
                    "model",
                    "model_instance_id",
                    "runtime_mode",
                    "interaction_mode",
                ][..],
            ),
        ] {
            if let Some(object) = routing[provider].as_object_mut() {
                for field in fields {
                    object.remove(*field);
                }
            }
        }
        routing
    }
    pub(crate) fn guard_target(&self, runner: &Runner) -> Result<()> {
        let state = Self::journal(runner)?;
        runner.maestro_guard_config(self.settings.maestro.as_ref())?;
        runner.t3_settings_guard(self.settings.t3.as_ref())?;
        ensure!(Self::target_change_allowed(&state.provider_identity, &Self::provider_identity(&self.settings), runner.has_external_work(), runner.has_active_provider_work()),
            "Provider target change is blocked by existing work. Restore the account/project; model changes require all chats and stages to finish.");
        Ok(())
    }
    /// Shared admission predicate; callers must also enforce the Maestro guard.
    pub(crate) fn target_change_allowed(
        old: &Value,
        new: &Value,
        retained: bool,
        active: bool,
    ) -> bool {
        old.is_null()
            || old == new
            || !retained
            || (Self::routing_identity(old) == Self::routing_identity(new) && !active)
    }
    pub(crate) fn retained_provider_identity(value: &Value) -> Result<Value> {
        if value.is_null() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_value::<Journal>(value.clone())?.provider_identity)
    }
    // Configuration describes future connections, not the origin of retained work.
    // In particular, a missing historical pin must survive startup and reconnect.
    fn bind_provider_identity(state: &mut Journal, mut identity: Value, runner: &Runner) {
        if runner.current_has_external_work() {
            if state.provider_identity.is_null() {
                return;
            }
            if state.provider_identity["t3"].is_null() {
                // Preserve absent versus explicit null as well as the unknown
                // origin, without preventing admitted updates to other fields.
                if let Some(fields) = identity.as_object_mut() {
                    match state.provider_identity.get("t3") {
                        Some(original) => {
                            fields.insert("t3".into(), original.clone());
                        }
                        None => {
                            fields.remove("t3");
                        }
                    }
                }
            }
        }
        state.provider_identity = identity;
    }
    pub(crate) fn bind_target(&self, runner: &mut Runner) -> Result<()> {
        self.guard_target(runner)?;
        let mut state = Self::journal(runner)?;
        Self::bind_provider_identity(&mut state, Self::provider_identity(&self.settings), runner);
        runner.checkpoint_application(serde_json::to_value(state)?, None, None)
    }
    fn legacy_t3_route_is_known(&self, identity: &Value) -> bool {
        let retained = &Self::routing_identity(identity)["t3"];
        let configured = &Self::routing_identity(&Self::provider_identity(&self.settings))["t3"];
        !retained.is_null() && retained == configured
    }
    fn journal_for_new_provider_work(&self, runner: &Runner) -> Result<Journal> {
        let mut state = Self::journal(runner)?;
        // Alternate goal constructors (including Inbox) need the same first-work
        // boundary. Commit this initial pin with the provider intent, not on read.
        if self.settings.t3.is_some()
            && !runner.t3_has_generations()
            && !runner.current_has_external_work()
        {
            self.guard_target(runner)?;
            Self::bind_provider_identity(
                &mut state,
                Self::provider_identity(&self.settings),
                runner,
            );
        }
        Ok(state)
    }
    pub(crate) fn bind_fresh_t3_origin(&self, runner: &mut Runner) -> Result<()> {
        // Establish provenance before the first dispatch exists, never from an
        // existing envelope. New generations carry their own operation identity.
        if self.settings.t3.is_some()
            && !runner.t3_has_generations()
            && !runner.current_has_external_work()
        {
            self.bind_target(runner)?;
        }
        Ok(())
    }
    pub(crate) fn guard_legacy_t3_route(&self, runner: &Runner) -> Result<()> {
        // Explicit in-process adapters without connector settings keep their
        // existing boundary. A configured legacy connector needs a retained pin.
        ensure!(
            self.settings.t3.is_none()
                || self.legacy_t3_route_is_known(&Self::journal(runner)?.provider_identity),
            "historical_origin_unavailable"
        );
        Ok(())
    }
    pub(crate) fn account_adapter(&self) -> Option<Todoist> {
        self.todoist.clone()
    }
    pub(crate) fn disable_todoist(&mut self, state: Value) {
        self.todoist = None;
        self.connection_states.insert("todoist".into(), state);
    }
    pub(crate) fn todoist_account(&self) -> Result<String> {
        self.todoist
            .as_ref()
            .context("Todoist credentials are unavailable")?
            .account_id()
            .map_err(anyhow::Error::from)
    }
    fn journal(runner: &Runner) -> Result<Journal> {
        let value = runner.application_state();
        if value.is_null() {
            Ok(Journal::default())
        } else {
            Ok(serde_json::from_value(value)?)
        }
    }
    pub(crate) fn local_actor(&self) -> &str {
        &self.actor
    }
    pub fn capabilities(&self, runner: &Runner) -> Value {
        json!({"discussion_decision_reuse":runner.managed(),"discussion_decision_save":runner.managed(),"goal_criteria_edit":runner.managed(),"discussion_send_recovery":true,"chat":self.chat.is_some(),"todoist":self.todoist.is_some(),"todoist_inbox_picker":true,"t3":self.t3_target.is_some() || runner.t3_has_generations(),"maestro_observation":self.settings.maestro.is_some(),"maestro_link":self.settings.maestro.is_some()&&runner.maestro_links_writable(),"maestro_control":self.settings.maestro.is_some()&&runner.managed(),"maestro_approval_send":self.settings.maestro.is_some()&&runner.managed()&&crate::maestro_control::NEW_DECISIONS_ENABLED,"source_write":runner.managed(),"actor":self.actor,"workspace":runner.workspace_identity(),"workspace_guard":true,"prepared_stage_edit":true,"guarded_start":true,"workspace_attention":true,"proposal_read":runner.proposals_readable(),"proposal_retry":runner.proposals_retry_writable(),"proposal_adopt":runner.proposals_adopt_writable(),"proposal_disposition":runner.proposals_writable(),"proposal_disposition_terminal":runner.proposals_writable(),"inbox_read":true,"inbox_capture":runner.inbox_writable(),"inbox_plan":runner.inbox_plan_writable(),"attention_read":runner.attention_readable(),"attention_reply":runner.attention_writable(),"attention_ack":runner.attention_writable(),"brain_retrieval":true,"reviewed_context":true,"goal_context_brief":true,"discussion_context":true,"ai_context_export":self.chat.is_some(),"connections":self.connection_states})
    }
    // Keep selected-goal and workspace attention on the same filtering path.
    // A retained T3 binding alone does not resolve a correlation blocker.
    fn current_attention(snapshot: &Snapshot) -> Vec<&Attention> {
        let outcome_saved = snapshot
            .stage
            .as_ref()
            .is_some_and(|stage| stage.engine == "t3" && !stage.result_ids.is_empty());
        snapshot
            .attention
            .iter()
            .filter(|attention| {
                !(outcome_saved
                    && attention.kind == "blocker"
                    && matches!(
                        attention.message.as_str(),
                        "t3_command_accepted_waiting_for_correlated_turn"
                            | "t3_turn_correlation_unknown"
                    ))
            })
            .collect()
    }

    /// Read the configured brain's current attention without persisting selection,
    /// polling providers, refreshing tasks, or exposing historical alerts as current.
    /// The service holds its owner mutex for the entire projection.
    pub(crate) fn attention_items(&self, runner: &mut Runner) -> Result<Vec<Value>> {
        self.workspace_attention(runner)?["items"]
            .as_array()
            .cloned()
            .context("attention projection unavailable")
    }
    pub fn workspace_attention(&self, runner: &mut Runner) -> Result<Value> {
        let goal_ids = runner.goal_ids();
        let mut items = Vec::new();
        let mut running_goal_count = 0;
        for goal_id in &goal_ids {
            runner.with_goal(goal_id, |runner| {
                let snapshot = runner.snapshot()?;
                let goal = snapshot
                    .goal
                    .as_ref()
                    .context("goal missing from workspace")?;
                if snapshot.phase.as_deref().is_some_and(|phase| {
                    ["running", "submitting", "indeterminate"].contains(&phase)
                }) {
                    running_goal_count += 1;
                }
                for attention in Self::current_attention(&snapshot) {
                    let stage_id = snapshot.attention_stage_ids.get(&attention.id);
                    let current_stage_id = snapshot.stage.as_ref().map(|stage| &stage.id);
                    // Legacy and goal-wide alerts have no recorded stage owner.
                    // Never attach those to a new stage or its result by inference.
                    let result_id = snapshot
                        .stage
                        .as_ref()
                        .filter(|stage| Some(&stage.id) == stage_id)
                        .and_then(|stage| stage.result_ids.last());
                    items.push(json!({
                        "attention_id": attention.id,
                        "goal_id": goal.id,
                        "goal_title": goal.title,
                        "goal_status": goal.status,
                        "stage_id": stage_id,
                        "current_stage_id": current_stage_id,
                        "result_id": result_id,
                        "kind": attention.kind,
                        "message": attention.message
                    }));
                }
                for mut item in runner.maestro_attention(goal_id) {
                    item["goal_title"] = json!(goal.title);
                    item["goal_status"] = json!(goal.status);
                    items.push(item);
                }
                Ok(())
            })?;
        }
        Ok(json!({
            "schema": SCHEMA,
            "workspace": runner.workspace_identity(),
            "observed_at": time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)?,
            "goal_count": goal_ids.len(),
            "running_goal_count": running_goal_count,
            "items": items
        }))
    }

    pub fn snapshot(&self, runner: &Runner) -> Result<Value> {
        let snap = runner.snapshot()?;
        let state = Self::journal(runner)?;
        let mut result = serde_json::to_value(&snap)?;
        result["goals"] = json!(runner.goals()?);
        result["stages"] = json!(runner.stages()?);
        result["selected_source_paths"] = json!(state.selected_source_paths);
        result["selected_conversation_id"] = json!(state.selected_conversation_id);
        result["attention"] = json!(Self::current_attention(&snap));
        result["conversations"] = json!(state
            .conversations
            .values()
            .map(|c| json!({"id":c.id,"path":c.path,"status":c.status}))
            .collect::<Vec<_>>());
        result["pending_task_operation_id"] = state
            .mutations
            .iter()
            .find(|(_, m)| {
                matches!(
                    m.outcome,
                    None | Some(MutationOutcome::Indeterminate { .. })
                )
            })
            .map(|(id, _)| json!(id))
            .unwrap_or(Value::Null);
        result["task"] = state
            .task
            .as_ref()
            .map(TaskState::view)
            .unwrap_or(Value::Null);
        // Opening a dispatched thread does not require a correlated provider
        // turn. This URL identifies the persisted attempt, not proof it ran.
        let thread_id = snap
            .binding
            .as_ref()
            .and_then(|b| b.thread_id.clone())
            .or_else(|| {
                if snap
                    .stage
                    .as_ref()
                    .is_some_and(|stage| stage.engine == "t3")
                    && snap
                        .phase
                        .as_deref()
                        .is_some_and(|phase| !["prepared", "not_started"].contains(&phase))
                {
                    snap.dispatch
                        .as_ref()
                        .and_then(|d| T3Adapter::thread_id(d).ok())
                } else {
                    None
                }
            });
        let operation = snap.dispatch.as_ref().map(|d| d.operation_id.as_str());
        let route = operation
            .map(|id| runner.t3_operation_settings(id))
            .transpose();
        let settings = match route {
            Ok(Some(Some(settings))) => Some(settings),
            Ok(_) => self
                .settings
                .t3
                .clone()
                .filter(|_| self.legacy_t3_route_is_known(&state.provider_identity)),
            Err(_) => None,
        };
        result["historical_route_status"] = json!(if settings.is_some() {
            "known_route"
        } else {
            "historical_origin_unavailable"
        });
        result["thread_url"] = match (settings, thread_id) {
            (Some(settings), Some(id)) => {
                let mut url = reqwest::Url::parse(&settings.base_url)?;
                url.path_segments_mut()
                    .map_err(|_| anyhow::anyhow!("invalid T3 thread URL"))?
                    .pop_if_empty()
                    .push(&settings.environment_id)
                    .push(&id);
                json!(url.as_str())
            }
            _ => Value::Null,
        };
        result["maestro"] = snap
            .goal
            .as_ref()
            .map(|g| runner.maestro_link_view(&g.id))
            .transpose()?
            .unwrap_or(Value::Null);
        result["source_paths"] = json!({"goal":snap.goal.as_ref().map(|g|runner.path("goal",&g.id)),"stage":snap.stage.as_ref().map(|s|runner.path("stage",&s.id)),"result":snap.stage.as_ref().and_then(|s|s.result_ids.last()).map(|id|runner.path("result",id))});
        Ok(result)
    }
    pub fn select_sources(
        &self,
        runner: &mut Runner,
        paths: Vec<String>,
        conversation_id: Option<String>,
    ) -> Result<Value> {
        Self::sources(runner, &paths)?;
        let mut state = Self::journal(runner)?;
        if let Some(id) = &conversation_id {
            ensure!(
                state.conversations.contains_key(id),
                "conversation belongs to another goal"
            );
        }
        state.selected_source_paths = paths;
        state.selected_conversation_id = conversation_id;
        runner.checkpoint_application(serde_json::to_value(state)?, None, None)?;
        self.snapshot(runner)
    }
    fn check_goal(runner: &Runner, goal_id: &str) -> Result<Goal> {
        let goal = runner.snapshot()?.goal.context("create a goal first")?;
        ensure!(goal.id == goal_id, "goal identity mismatch");
        Ok(goal)
    }
    fn sources(runner: &Runner, paths: &[String]) -> Result<Vec<SourceSnapshot>> {
        let mut seen = std::collections::BTreeSet::new();
        paths
            .iter()
            .filter(|p| seen.insert(*p))
            .map(|p| runner.read_source(p))
            .collect()
    }
    fn source_refs(sources: &[SourceSnapshot]) -> Vec<SourceRef> {
        sources
            .iter()
            .map(|s| SourceRef {
                uri: format!("brain://{}/{}", s.brain_id, s.path),
                revision: Some(s.revision.clone()),
                locator: None,
            })
            .collect()
    }
    fn conversation_body(c: &Conversation) -> String {
        let mut body = format!("\n# Goal conversation\n\nStatus: {}\n", c.status);
        for m in &c.messages {
            body.push_str(&format!("\n## {}\n\n{}\n", m.role, m.text));
        }
        if !c.partial.is_empty() {
            body.push_str(&format!("\n## Assistant (partial)\n\n{}\n", c.partial));
        }
        body
    }
    fn save_conversation(runner: &mut Runner, mut state: Journal, c: Conversation) -> Result<()> {
        let record = serde_json::to_value(&c)?;
        let body = Self::conversation_body(&c);
        let id = c.id.clone();
        state.conversations.insert(id.clone(), c);
        runner.checkpoint_application(
            serde_json::to_value(state)?,
            Some(("conversation", &id, record, body)),
            None,
        )
    }
    pub fn recover(&self, runner: &mut Runner) -> Result<()> {
        let ids: Vec<_> = Self::journal(runner)?
            .conversations
            .values()
            .filter(|c| c.status == "running")
            .map(|c| c.id.clone())
            .collect();
        for id in ids {
            let state = Self::journal(runner)?;
            let mut c = state.conversations[&id].clone();
            c.status = "interrupted".into();
            c.error = Some("backend_restarted".into());
            Self::save_conversation(runner, state, c)?;
        }
        Ok(())
    }
    pub fn chat_start(
        &self,
        runner: &mut Runner,
        goal_id: String,
        message: String,
        paths: Vec<String>,
        conversation_id: Option<String>,
    ) -> Result<(String, ChatConfig, ChatRequest)> {
        let prepared =
            self.chat_start_prepared(runner, goal_id, message, paths, conversation_id)?;
        Ok((prepared.id, prepared.config, prepared.request))
    }
    pub(crate) fn chat_start_prepared(
        &self,
        runner: &mut Runner,
        goal_id: String,
        message: String,
        paths: Vec<String>,
        conversation_id: Option<String>,
    ) -> Result<crate::discussion_context::Prepared> {
        let candidate =
            self.prepare_discussion(runner, goal_id, message, paths, conversation_id, None)?;
        runner.checkpoint_discussion(
            candidate.state,
            &candidate.prepared.id,
            candidate.record,
            candidate.projection,
            candidate.revision,
        )?;
        Ok(candidate.prepared)
    }
    pub(crate) fn chat_send_prepared(
        &self,
        runner: &mut Runner,
        request: &crate::discussion_send::SendRequest,
    ) -> Result<crate::discussion_context::Prepared> {
        let candidate = self
            .prepare_discussion(
                runner,
                request.goal_id.clone(),
                request.message.clone(),
                request.source_paths.clone(),
                request.conversation_id.clone(),
                Some(request),
            )
            .map_err(|e| {
                crate::discussion_send::Error::rejected(
                    runner.workspace_identity()["brain_id"].as_str().unwrap(),
                    request.key(),
                    e,
                )
            })?;
        // From this boundary onwards errors retain the client's pending guard,
        // even if a subsequent durability step cannot confirm publication.
        runner
            .checkpoint_discussion_operation(
                candidate.state,
                &candidate.prepared.id,
                candidate.record,
                candidate.projection,
                candidate.revision,
                Some(&request.operation_id),
            )
            .map_err(crate::discussion_send::Error::recovery)?;
        Ok(candidate.prepared)
    }
    fn prepare_discussion(
        &self,
        runner: &Runner,
        goal_id: String,
        message: String,
        paths: Vec<String>,
        conversation_id: Option<String>,
        operation: Option<&crate::discussion_send::SendRequest>,
    ) -> Result<DiscussionCandidate> {
        let config = self.chat.clone().context("chat is not configured")?;
        let goal = Self::check_goal(runner, &goal_id)?;
        ensure!(!message.trim().is_empty(), "message is required");
        let mut state = self.journal_for_new_provider_work(runner)?;
        let sources = crate::discussion_context::manual_sources(runner, &paths)?;
        let new = conversation_id.is_none();
        let mut c = if let Some(id) = conversation_id {
            state
                .conversations
                .get(&id)
                .context("unknown conversation")?
                .clone()
        } else {
            let id = Uuid::new_v4().to_string();
            Conversation {
                path: runner.path("conversation", &id),
                id,
                goal_id: goal_id.clone(),
                status: "complete".into(),
                messages: vec![],
                partial: String::new(),
                error: None,
                sources: vec![],
                request: ChatRequest {
                    messages: vec![],
                    context: ChatContext {
                        goal: String::new(),
                        decisions: vec![],
                        constraints: vec![],
                        sources: vec![],
                        previous_result: None,
                        next_step: String::new(),
                    },
                },
            }
        };
        ensure!(
            c.goal_id == goal_id && c.status != "running",
            "conversation belongs to another goal or is already running"
        );
        let (mut envelope, revision) = crate::discussion_context::load(runner, &c, new)?;
        if !c.partial.is_empty() {
            c.messages.push(Message {
                role: "assistant".into(),
                text: format!("[Previous response interrupted]\n{}", c.partial),
            });
        }
        c.partial.clear();
        c.error = None;
        c.status = "running".into();
        c.sources = sources;
        c.messages.push(Message {
            role: "user".into(),
            text: message,
        });
        let (turn, request, body) = crate::discussion_context::prepare(
            runner,
            &goal,
            &c,
            &self.actor,
            &config,
            &c.sources,
        )?;
        let turn_id = turn.turn_id.clone();
        let send_receipt = operation
            .map(|request| {
                crate::discussion_send::Receipt::new(
                    runner.workspace_identity()["brain_id"].as_str().unwrap(),
                    request,
                    &c.id,
                    &turn,
                )
            })
            .transpose()?;
        envelope.append(turn)?;
        c.request = request.clone();
        let id = c.id.clone();
        state.selected_conversation_id = Some(id.clone());
        state.selected_source_paths = paths;
        let mut record = serde_json::to_value(&c)?;
        record["discussion_context"] = serde_json::to_value(envelope)?;
        if let Some(receipt) = send_receipt {
            record["discussion_send"] = serde_json::to_value(receipt)?;
        }
        let projection = Self::conversation_body(&c);
        state.conversations.insert(id.clone(), c);
        Ok(DiscussionCandidate {
            state: serde_json::to_value(state)?,
            record,
            projection,
            revision,
            prepared: crate::discussion_context::Prepared {
                id,
                turn_id,
                config,
                request,
                body,
            },
        })
    }
    pub fn chat_get(&self, runner: &Runner, id: &str) -> Result<Value> {
        self.chat_get_context(runner, id, None)
    }
    pub(crate) fn chat_get_context(
        &self,
        runner: &Runner,
        id: &str,
        turn_id: Option<&str>,
    ) -> Result<Value> {
        let state = Self::journal(runner)?;
        let c = state
            .conversations
            .get(id)
            .context("unknown conversation")?;
        let mut response = json!({"id":c.id,"goal_id":c.goal_id,"path":c.path,"status":c.status,"messages":c.messages,"partial":c.partial,"error":c.error});
        let context = crate::discussion_context::view(runner, c, turn_id)?;
        response
            .as_object_mut()
            .unwrap()
            .extend(context.as_object().unwrap().clone());
        Ok(response)
    }
    pub fn chat_event(runner: &mut Runner, id: &str, event: ChatEvent) -> Result<()> {
        let state = Self::journal(runner)?;
        let mut c = state
            .conversations
            .get(id)
            .context("unknown conversation")?
            .clone();
        ensure!(c.status == "running", "conversation no longer running");
        match event {
            ChatEvent::Delta { text } => c.partial.push_str(&text),
            ChatEvent::Complete { text } => {
                c.status = "complete".into();
                c.partial.clear();
                c.messages.push(Message {
                    role: "assistant".into(),
                    text,
                });
            }
            ChatEvent::Interrupted { text, reason } => {
                c.status = "interrupted".into();
                c.partial = text;
                c.error = Some(reason);
            }
            ChatEvent::Error { text, code } => {
                c.status = "error".into();
                c.partial = text;
                c.error = Some(code);
            }
        }
        Self::save_conversation(runner, state, c)
    }
    fn task_save(runner: &mut Runner, state: Journal) -> Result<()> {
        let record = state.task.as_ref().map(|t|(t.id.clone(),json!({"id":t.id,"goal_id":t.binding.goal_id,"binding":t.binding,"observed_status":t.status,"observation":t.observation}),format!("\n# {}\n\nTodoist status: {}\n\n[Open task]({})\n",t.content,t.status,t.view()["url"].as_str().unwrap())));
        let task_ref = state.task.as_ref().map(|t|json!({"provider":"todoist","instance_id":t.binding.instance_id,"external_id":t.binding.external_id,"observed_status":t.status,"observed_at":t.observation.as_ref().map(|o|o.observed_at.clone())}));
        runner.checkpoint_application(
            serde_json::to_value(state)?,
            record
                .as_ref()
                .map(|(id, v, b)| ("task", id.as_str(), v.clone(), b.clone())),
            task_ref,
        )
    }
    pub fn task_create(
        &self,
        runner: &mut Runner,
        goal_id: String,
        operation_id: String,
        content: String,
    ) -> Result<Value> {
        Self::check_goal(runner, &goal_id)?;
        ensure!(!content.trim().is_empty(), "task content is required");
        let provider = self.todoist.as_ref().context("Todoist is not configured")?;
        let prepared = provider.prepare(
            operation_id.clone(),
            goal_id.clone(),
            Mutation::Create {
                fields: TaskFields {
                    content: Some(content),
                    ..Default::default()
                },
                project_id: None,
                section_id: None,
            },
        )?;
        let mut state = self.journal_for_new_provider_work(runner)?;
        if let Some(old) = state.mutations.get(&operation_id) {
            ensure!(
                old.prepared == prepared,
                "operation UUID reused with changed task payload"
            );
        } else {
            state.task_generation = state
                .task_generation
                .checked_add(1)
                .context("task selection generation exhausted")?;
            state.mutations.insert(
                operation_id.clone(),
                TaskIntent {
                    generation: state.task_generation,
                    prepared,
                    outcome: None,
                },
            );
            Self::task_save(runner, state)?;
        }
        self.task_reconcile(runner, operation_id)
    }
    pub fn task_reconcile(&self, runner: &mut Runner, operation_id: String) -> Result<Value> {
        let provider = self.todoist.as_ref().context("Todoist is not configured")?;
        let mut state = Self::journal(runner)?;
        let intent = state
            .mutations
            .get(&operation_id)
            .context("unknown task operation")?
            .clone();
        let outcome = match intent.outcome {
            Some(
                ref
                terminal @ (MutationOutcome::Accepted { .. } | MutationOutcome::Rejected { .. }),
            ) => terminal.clone(),
            _ => provider.reconcile(&intent.prepared),
        };
        state.mutations.get_mut(&operation_id).unwrap().outcome = Some(outcome.clone());
        let superseded = intent.generation != state.task_generation;
        let (status, error) = match outcome {
            MutationOutcome::Accepted { .. } if superseded => ("accepted", Some("operation superseded by a newer task selection; receipt retained without relinking".into())),
            MutationOutcome::Accepted { receipt } => {
                if state
                    .task
                    .as_ref()
                    .is_none_or(|t| t.binding != receipt.binding)
                {
                    let content = match &intent.prepared.mutation {
                        Mutation::Create { fields, .. } => {
                            fields.content.clone().unwrap_or_default()
                        }
                        _ => String::new(),
                    };
                    state.task = Some(TaskState {
                        id: Uuid::new_v4().to_string(),
                        binding: receipt.binding,
                        content,
                        status: "unknown".into(),
                        observation: None,
                    });
                }
                ("accepted", None)
            }
            MutationOutcome::Rejected { error } => ("rejected", Some(error.to_string())),
            MutationOutcome::Indeterminate { error } => ("indeterminate", Some(error.to_string())),
        };
        // Retain provider receipt and identity before attempting any observation.
        Self::task_save(runner, state)?;
        if status == "accepted" && !superseded {
            return self.task_refresh(runner, intent.prepared.goal_id);
        }
        Ok(
            json!({"status":status,"task":Self::journal(runner)?.task.as_ref().map(TaskState::view),"error":error}),
        )
    }
    pub fn task_link(
        &self,
        runner: &mut Runner,
        goal_id: String,
        task_id: String,
    ) -> Result<Value> {
        Self::check_goal(runner, &goal_id)?;
        let observed = self
            .todoist
            .as_ref()
            .context("Todoist is not configured")?
            .associate(goal_id.clone(), task_id)?;
        self.task_link_observed(runner, goal_id, observed)
    }
    pub(crate) fn task_generation(runner: &Runner, goal_id: &str) -> Result<u64> {
        Self::check_goal(runner, goal_id)?;
        Ok(Self::journal(runner)?.task_generation)
    }
    /// Network-free commit shared by direct links and frozen picker selections.
    pub(crate) fn task_link_observed(
        &self,
        runner: &mut Runner,
        goal_id: String,
        observed: TaskObservation,
    ) -> Result<Value> {
        Self::check_goal(runner, &goal_id)?;
        ensure!(
            observed.binding.goal_id == goal_id,
            "task goal identity mismatch"
        );
        let mut state = self.journal_for_new_provider_work(runner)?;
        state.task_generation = state
            .task_generation
            .checked_add(1)
            .context("task selection generation exhausted")?;
        let task = TaskState {
            id: Uuid::new_v4().to_string(),
            binding: observed.binding.clone(),
            content: observed.task.content.clone(),
            status: if observed.task.is_deleted {
                "deleted"
            } else if observed.task.checked {
                "completed"
            } else {
                "open"
            }
            .into(),
            observation: Some(observed),
        };
        let view = task.view();
        state.task = Some(task);
        Self::task_save(runner, state)?;
        Ok(json!({"status":"accepted","task":view,"error":null}))
    }
    pub fn task_refresh(&self, runner: &mut Runner, goal_id: String) -> Result<Value> {
        Self::check_goal(runner, &goal_id)?;
        let mut state = Self::journal(runner)?;
        let task = state.task.as_mut().context("no linked task")?;
        ensure!(
            task.binding.goal_id == goal_id,
            "task goal identity mismatch"
        );
        let error = match self
            .todoist
            .as_ref()
            .context("Todoist is not configured")?
            .read(&task.binding)
        {
            Ok(o) => {
                task.content = o.task.content.clone();
                task.status = if o.task.is_deleted {
                    "deleted"
                } else if o.task.checked {
                    "completed"
                } else {
                    "open"
                }
                .into();
                task.observation = Some(o);
                None
            }
            Err(e) => {
                task.status = "unknown".into();
                Some(e.to_string())
            }
        };
        let view = task.view();
        Self::task_save(runner, state)?;
        Ok(
            json!({"status":if error.is_some(){"indeterminate"}else{"accepted"},"task":view,"error":error}),
        )
    }
    /// Patch one accepted pointer expectation without restoring an old journal.
    pub(crate) fn adopt_context_pointer(
        runner: &mut Runner,
        expected: Option<&str>,
        target: &str,
    ) -> Result<crate::proposals::PointerOutcome> {
        use crate::proposals::PointerOutcome;
        let mut state = runner.application_state();
        if state.is_null() {
            state = serde_json::to_value(Journal::default())?;
        }
        // Validate known state, but preserve unknown/unrelated fields verbatim.
        let journal: Journal = serde_json::from_value(state.clone())?;
        if journal.reviewed_packet_id.as_deref() == Some(target) {
            return Ok(PointerOutcome::AlreadySelected);
        }
        if journal.reviewed_packet_id.as_deref() != expected {
            return Ok(PointerOutcome::PreservedNewer);
        }
        state
            .as_object_mut()
            .context("application state is not an object")?
            .insert("reviewed_packet_id".into(), Value::String(target.into()));
        runner.checkpoint_application(state, None, None)?;
        Ok(PointerOutcome::Updated)
    }
    pub fn context_get(
        &self,
        runner: &Runner,
        goal_id: &str,
        packet_id: Option<&str>,
    ) -> Result<Value> {
        Self::check_goal(runner, goal_id)?;
        let journal = Self::journal(runner)?;
        let id = packet_id.or(journal.reviewed_packet_id.as_deref());
        let packet = id
            .map(|id| crate::context::read(runner, goal_id, id))
            .transpose()?;
        Ok(json!({"packet":packet}))
    }
    pub fn context_prepare(
        &self,
        runner: &mut Runner,
        goal_id: String,
        query: String,
        scope: crate::retrieval::SearchScope,
        citations: Vec<crate::retrieval::Citation>,
        pinned: Vec<String>,
    ) -> Result<Value> {
        let goal = Self::check_goal(runner, &goal_id)?;
        let packet = crate::context::create(runner, goal, query, scope, citations, pinned)?;
        let mut journal = Self::journal(runner)?;
        journal.reviewed_packet_id = Some(packet.id.clone());
        runner.checkpoint_application(
            serde_json::to_value(journal)?,
            Some((
                "reviewed-context",
                &packet.id,
                packet.metadata()?,
                packet.text.clone(),
            )),
            None,
        )?;
        self.context_get(runner, &goal_id, Some(&packet.id))
    }
    pub fn context_revise(
        &self,
        runner: &mut Runner,
        goal_id: String,
        packet_id: String,
        expected_revision: String,
        text: String,
    ) -> Result<Value> {
        Self::check_goal(runner, &goal_id)?;
        ensure!(!text.trim().is_empty(), "reviewed guidance is required");
        let mut packet = crate::context::read(runner, &goal_id, &packet_id)?;
        ensure!(
            packet.revision == expected_revision,
            "context revision changed; retain this draft and inspect the current packet"
        );
        crate::context::validate_fresh(runner, &packet)?;
        ensure!(
            text.len()
                + packet
                    .citations
                    .iter()
                    .map(|c| c.excerpt.len())
                    .sum::<usize>()
                <= crate::context::MAX_PACKET_BYTES,
            "reviewed context exceeds 64 KiB"
        );
        packet.text = text;
        packet.updated_at = crate::retrieval::now();
        packet.mark_reviewed()?;
        let mut meta = packet.metadata()?;
        meta["schema"] = json!(SCHEMA);
        meta["record_type"] = json!("reviewed-context");
        meta["brain_id"] = runner.workspace_identity()["brain_id"].clone();
        let body = format!("---\n{}---\n{}", serde_yaml::to_string(&meta)?, packet.text);
        runner.write_source(okilum_core::source::SourceWrite {
            schema: SCHEMA.into(),
            operation_id: Uuid::new_v4().to_string(),
            brain_id: runner.workspace_identity()["brain_id"]
                .as_str()
                .unwrap()
                .into(),
            path: runner.path("reviewed-context", &packet_id),
            expected_revision: Some(expected_revision),
            content_base64: STANDARD.encode(body.as_bytes()),
        })?;
        self.context_get(runner, &goal_id, Some(&packet_id))
    }

    pub fn validate_reviewed_dispatch(&self, runner: &Runner) -> Result<()> {
        if let Some(dispatch) = runner.snapshot()?.dispatch {
            if let Some(value) = dispatch.packet.extra.get("reviewed_packet") {
                let reference: crate::context::ReviewedPacketRef =
                    serde_json::from_value(value.clone())?;
                let (current, _) =
                    crate::context::require_reviewed(runner, &dispatch.packet.goal_id, &reference)?;
                ensure!(
                    serde_json::to_value(&current.citations)?
                        == dispatch.packet.extra["source_excerpts"],
                    "prepared citations differ from reviewed packet"
                );
                ensure!(current.text==dispatch.packet.next_step,"prepared guidance differs from the reviewed packet; discard and prepare its reviewed revision");
            }
        }
        Ok(())
    }

    fn preparation_target(&self, runner: &Runner) -> Result<BTreeMap<String, Value>> {
        if runner.t3_has_generations() {
            let settings = runner
                .t3_future_settings(None)
                .context("active T3 generation unavailable")?;
            Ok(BTreeMap::from([
                ("environment_id".into(), json!(settings.environment_id)),
                ("project_id".into(), json!(settings.project_id)),
            ]))
        } else {
            // Preserve the legacy configured-adapter boundary, including injected
            // adapters used by the transport-neutral application contract.
            self.t3_target.clone().context("T3 is not configured")
        }
    }
    pub fn stage_prepare_reviewed(
        &self,
        runner: &mut Runner,
        goal_id: String,
        criterion_ids: Vec<String>,
        reference: crate::context::ReviewedPacketRef,
        previous_result_id: Option<String>,
    ) -> Result<()> {
        let goal = Self::check_goal(runner, &goal_id)?;
        let (reviewed, _) = crate::context::require_reviewed(runner, &goal_id, &reference)?;
        let mut target = self.preparation_target(runner)?;
        target.insert("created_at".into(), json!(crate::retrieval::now()));
        let stage_id = Uuid::new_v4().to_string();
        let context_id = Uuid::new_v4().to_string();
        let packet = ContextPacket {
            id: context_id.clone(),
            goal_id: goal_id.clone(),
            stage_id: stage_id.clone(),
            goal_revision: runner.goal_source()?.revision,
            goal: goal.title,
            decisions: vec![],
            constraints: goal
                .criteria
                .iter()
                .map(|c| c.description.clone())
                .collect(),
            sources: reviewed
                .citations
                .iter()
                .map(|c| SourceRef {
                    uri: format!(
                        "brain://{}/{}",
                        runner.workspace_identity()["brain_id"].as_str().unwrap(),
                        c.path
                    ),
                    revision: Some(c.revision.clone()),
                    locator: Some(c.locator.clone()),
                })
                .collect(),
            previous_result_id,
            next_step: reviewed.text.clone(),
            extra: BTreeMap::from([
                (
                    "reviewed_packet".into(),
                    serde_json::to_value(reviewed.reference())?,
                ),
                ("source_excerpts".into(), json!(reviewed.citations)),
            ]),
        };
        // No full source snapshots, conversation transcript or previous-result
        // body is appended to the new reviewed path. The visible packet is the payload.
        self.bind_fresh_t3_origin(runner)?;
        runner.prepare_stage(
            Stage {
                id: stage_id,
                goal_id,
                engine: "t3".into(),
                status: "ready".into(),
                criterion_ids,
                context_id,
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            packet,
            Uuid::new_v4().to_string(),
            target,
        )?;
        Ok(())
    }

    pub fn stage_prepare(
        &self,
        runner: &mut Runner,
        goal_id: String,
        conversation_id: Option<String>,
        paths: Vec<String>,
        criterion_ids: Vec<String>,
        next_step: String,
    ) -> Result<()> {
        self.stage_prepare_with_previous(
            runner,
            goal_id,
            conversation_id,
            paths,
            criterion_ids,
            next_step,
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn stage_prepare_with_previous(
        &self,
        runner: &mut Runner,
        goal_id: String,
        conversation_id: Option<String>,
        paths: Vec<String>,
        criterion_ids: Vec<String>,
        next_step: String,
        previous_result_id: Option<String>,
    ) -> Result<()> {
        let goal = Self::check_goal(runner, &goal_id)?;
        let mut sources = Self::sources(runner, &paths)?;
        crate::inbox_plan::exclude_pinned_source(&goal, &mut sources)?;
        let state = Self::journal(runner)?;
        ensure!(state.reviewed_packet_id.is_none(),"A reviewed context packet exists; supply its exact reviewed_packet reference before preparation");
        let transcript = if let Some(id) = conversation_id {
            let c = state
                .conversations
                .get(&id)
                .context("unknown conversation")?;
            ensure!(
                c.goal_id == goal_id && c.status != "running",
                "conversation cannot be frozen for this goal"
            );
            json!({"messages":c.messages,"partial":c.partial,"status":c.status,"source_snapshots":c.sources})
        } else {
            Value::Null
        };
        let mut target = self.preparation_target(runner)?;
        target.insert(
            "created_at".into(),
            json!(time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)?),
        );
        let stage_id = Uuid::new_v4().to_string();
        let context_id = Uuid::new_v4().to_string();
        let excerpts: Vec<_> = sources.iter().map(|s|json!({"path":s.path,"revision":s.revision,"text":String::from_utf8_lossy(&STANDARD.decode(&s.content_base64).unwrap_or_default())})).collect();
        let original = crate::inbox_plan::operator_input(&goal)?;
        let packet = ContextPacket {
            id: context_id.clone(),
            goal_id: goal_id.clone(),
            stage_id: stage_id.clone(),
            goal_revision: runner.goal_source()?.revision,
            goal: goal.title,
            decisions: original.into_iter().collect(),
            constraints: goal
                .criteria
                .iter()
                .map(|c| c.description.clone())
                .collect(),
            sources: Self::source_refs(&sources),
            previous_result_id: previous_result_id.clone(),
            next_step,
            extra: BTreeMap::from([
                ("source_snapshots".into(), json!(sources)),
                ("source_excerpts".into(), json!(excerpts)),
                ("conversation".into(), transcript),
                (
                    "previous_result".into(),
                    previous_result_id
                        .as_ref()
                        .map(|id| runner.result(id))
                        .transpose()?
                        .map(|r| json!(r))
                        .unwrap_or(Value::Null),
                ),
            ]),
        };
        if goal.extra.contains_key("origin_inbox") {
            ensure!(
                serde_json::to_vec(&packet)?.len() <= crate::context::MAX_PACKET_BYTES,
                "original thought and selected context exceed64 KiB"
            );
        }
        self.bind_fresh_t3_origin(runner)?;
        runner.prepare_stage(
            Stage {
                id: stage_id,
                goal_id,
                engine: "t3".into(),
                status: "ready".into(),
                criterion_ids,
                context_id,
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            packet,
            Uuid::new_v4().to_string(),
            target,
        )?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "reviewed_context_tests.rs"]
mod reviewed_context_tests;
