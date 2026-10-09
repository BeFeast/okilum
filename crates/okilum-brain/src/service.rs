//! Versioned local JSON-lines service. Listener and adapter driver belong to the
//! backend process, not a desktop socket. This POC binds loopback only; no service
//! installation or remote exposure is performed here.
use crate::application::{Application, ApplicationConfig};
use crate::*;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub type Adapters = BTreeMap<String, Box<dyn Adapter>>;
struct Backend {
    runner: Runner,
    adapters: Adapters,
    app: Application,
    exports: crate::export::ExportDownloads,
    index: Option<Arc<crate::retrieval::BrainIndex>>,
    index_error: Option<String>,
    context_jobs: crate::context_jobs::Jobs,
    todoist_picker: todoist_picker::State,
}
#[derive(Deserialize)]
#[serde(try_from = "Value")]
struct Request {
    schema: String,
    id: Value,
    expected_workspace: Option<Value>,
    #[serde(flatten)]
    command: Command,
}
impl TryFrom<Value> for Request {
    type Error = String;
    fn try_from(value: Value) -> std::result::Result<Self, Self::Error> {
        let op = value.get("op").and_then(Value::as_str);
        if matches!(
            op,
            Some("t3_target_get" | "t3_target_prepare" | "t3_target_adopt")
        ) {
            if value["schema"] != "ai-brain/workspace-v1"
                || !value["expected_workspace"].is_object()
                || value["id"].as_str().is_none_or(str::is_empty)
            {
                return Err(
                    "T3 target operations require exact workspace guard and request id".into(),
                );
            }
            let payload = match op {
                Some("t3_target_prepare") => Some("candidate"),
                Some("t3_target_adopt") => Some("request"),
                _ => None,
            };
            if value.as_object().is_none_or(|v| {
                v.keys().any(|k| {
                    !["schema", "id", "expected_workspace", "op"].contains(&k.as_str())
                        && Some(k.as_str()) != payload
                        && !(op == Some("t3_target_prepare") && k == "compatibility_manifest")
                })
            }) {
                return Err("Unexpected T3 target operation fields".into());
            }
            if op == Some("t3_target_adopt") {
                serde_json::from_value::<crate::t3_routes::AdoptRequest>(value["request"].clone())
                    .map_err(|e| e.to_string())?;
            }
            if op == Some("t3_target_prepare") {
                serde_json::from_value::<crate::application::T3Settings>(
                    value["candidate"].clone(),
                )
                .map_err(|e| e.to_string())?;
            }
        }
        if op == Some("source_backlinks") {
            if value["schema"] != "ai-brain/workspace-v1"
                || !value["expected_workspace"].is_object()
                || value["id"].as_str().is_none_or(str::is_empty)
            {
                return Err(
                    "Incoming references require exact workspace guard and transport id".into(),
                );
            }
            let mut payload = value.clone();
            let fields = payload
                .as_object_mut()
                .ok_or("invalid incoming references request")?;
            for key in ["schema", "id", "expected_workspace", "op"] {
                fields.remove(key);
            }
            serde_json::from_value::<crate::incoming_references::Request>(payload)
                .map_err(|e| e.to_string())?;
        }
        if (op == Some("todoist_inbox_list")
            || (op == Some("task_link")
                && value.get("picker_session_id").is_some_and(|v| !v.is_null())))
            && (value.get("schema").and_then(Value::as_str) != Some("ai-brain/workspace-v1")
                || value.get("expected_workspace").is_none_or(Value::is_null))
        {
            return Err("Todoist picker requires exact workspace guard".into());
        }
        if matches!(op, Some("chat_send" | "chat_send_get")) {
            if value.get("schema").and_then(Value::as_str) != Some("ai-brain/workspace-v1")
                || value.get("expected_workspace").is_none_or(Value::is_null)
                || value
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(
                    "correlated Discussion requires an exact workspace guard and transport id"
                        .into(),
                );
            }
            let mut payload = value.clone();
            let fields = payload
                .as_object_mut()
                .ok_or("invalid Discussion request")?;
            for key in ["schema", "id", "expected_workspace", "op"] {
                fields.remove(key);
            }
            if op == Some("chat_send") {
                serde_json::from_value::<crate::discussion_send::SendRequest>(payload)
                    .map_err(|e| e.to_string())?;
            } else {
                serde_json::from_value::<crate::discussion_send::Lookup>(payload)
                    .map_err(|e| e.to_string())?;
            }
        }
        if matches!(op, Some("suggestions_get" | "suggestions_set")) {
            if value.get("schema").and_then(Value::as_str) != Some("ai-brain/workspace-v1")
                || value.get("expected_workspace").is_none_or(Value::is_null)
            {
                return Err("suggestions controls require exact workspace guard".into());
            }
            let mut payload = value.clone();
            let fields = payload
                .as_object_mut()
                .ok_or("invalid suggestions request")?;
            for key in ["schema", "id", "expected_workspace", "op"] {
                fields.remove(key);
            }
            if op == Some("suggestions_set") {
                serde_json::from_value::<crate::suggestions::SetRequest>(payload)
                    .map_err(|e| e.to_string())?;
            } else if !fields.is_empty() {
                return Err("unexpected suggestions_get payload".into());
            }
        }
        if matches!(
            op,
            Some(
                "proposal_list"
                    | "proposal_get"
                    | "proposal_disposition"
                    | "proposal_retry"
                    | "proposal_adopt"
            )
        ) {
            if value.get("schema").and_then(Value::as_str) != Some("ai-brain/workspace-v1")
                || value.get("expected_workspace").is_none_or(Value::is_null)
            {
                return Err("proposal commands require exact workspace guard".into());
            }
            let mut payload = value.clone();
            let fields = payload.as_object_mut().ok_or("invalid proposal request")?;
            for key in ["schema", "id", "expected_workspace", "op"] {
                fields.remove(key);
            }
            match op {
                Some("proposal_list") => {
                    serde_json::from_value::<crate::proposal::ListRequest>(payload)
                        .map_err(|e| e.to_string())?;
                }
                Some("proposal_adopt") => {
                    serde_json::from_value::<crate::proposal::AdoptRequest>(payload)
                        .map_err(|e| e.to_string())?;
                }
                Some("proposal_retry") => {
                    serde_json::from_value::<crate::proposal::RetryRequest>(payload)
                        .map_err(|e| e.to_string())?;
                }
                Some("proposal_get") => {
                    serde_json::from_value::<crate::proposal::Lookup>(payload)
                        .map_err(|e| e.to_string())?;
                }
                _ => {
                    serde_json::from_value::<crate::proposal::Request>(payload)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        if matches!(
            op,
            Some(
                "maestro_link"
                    | "maestro_unlink"
                    | "maestro_operation_get"
                    | "maestro_operation_abandon"
                    | "maestro_approval_review"
                    | "maestro_approval_decision"
                    | "maestro_approval_reconcile"
            )
        ) {
            // Serde's outer flatten otherwise ignores fields unknown to the inner
            // request. Validate the command payload separately at the wire boundary.
            let mut payload = value.clone();
            let fields = payload.as_object_mut().ok_or("invalid Maestro request")?;
            for key in ["schema", "id", "expected_workspace", "op"] {
                fields.remove(key);
            }
            match op {
                Some("maestro_link") => {
                    serde_json::from_value::<crate::maestro_links::LinkRequest>(payload)
                        .map_err(|error| error.to_string())?;
                }
                Some("maestro_unlink") => {
                    serde_json::from_value::<crate::maestro_links::UnlinkRequest>(payload)
                        .map_err(|error| error.to_string())?;
                }
                Some("maestro_approval_review") => {
                    serde_json::from_value::<crate::maestro_control::ReviewRequest>(payload)
                        .map_err(|e| e.to_string())?;
                }
                Some("maestro_approval_decision") => {
                    serde_json::from_value::<crate::maestro_control::Request>(payload)
                        .map_err(|e| e.to_string())?;
                }
                Some("maestro_operation_get" | "maestro_approval_reconcile") => {
                    serde_json::from_value::<crate::maestro_operations::Lookup>(payload)
                        .map_err(|error| error.to_string())?;
                }
                _ => {
                    serde_json::from_value::<crate::maestro_operations::Request>(payload)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        if value.get("op").and_then(Value::as_str) == Some("inbox_plan") {
            let allowed = [
                "schema",
                "id",
                "expected_workspace",
                "op",
                "operation_id",
                "capture_id",
                "expected_capture_revision",
                "title",
                "criteria",
                "source",
            ];
            if value
                .as_object()
                .is_none_or(|fields| fields.keys().any(|key| !allowed.contains(&key.as_str())))
            {
                return Err("inbox_plan contains an unsupported field".into());
            }
        }
        if matches!(
            op,
            Some("discussion_decision_get" | "discussion_decision_save")
        ) {
            if value["schema"] != "ai-brain/workspace-v1"
                || !value["expected_workspace"].is_object()
            {
                return Err("Decision save requires the exact workspace guard".into());
            }
            let allowed = [
                "schema",
                "id",
                "expected_workspace",
                "op",
                "goal_id",
                "conversation_id",
                "turn_id",
                "expected_actor_id",
                "request",
            ];
            if value
                .as_object()
                .is_none_or(|v| v.keys().any(|k| !allowed.contains(&k.as_str())))
                || (op == Some("discussion_decision_get") && value.get("request").is_some())
            {
                return Err("Unexpected decision command fields".into());
            }
            if op == Some("discussion_decision_save") {
                let fields = [
                    "schema",
                    "brain_id",
                    "operation_id",
                    "path",
                    "expected_revision",
                    "content_base64",
                ];
                if value["request"].as_object().is_none_or(|v| {
                    v.len() != fields.len() || v.keys().any(|k| !fields.contains(&k.as_str()))
                }) || !value["request"]["expected_revision"].is_null()
                {
                    return Err("Decision Save requires an exact create-only source request".into());
                }
            }
        }
        if matches!(
            op,
            Some("discussion_decision_reuse_get" | "discussion_decision_reuse_write")
        ) {
            if value["schema"] != "ai-brain/workspace-v1"
                || !value["expected_workspace"].is_object()
            {
                return Err("Reuse settings require exact workspace guard".into());
            }
            let allowed = if op == Some("discussion_decision_reuse_write") {
                &[
                    "schema",
                    "id",
                    "expected_workspace",
                    "op",
                    "goal_id",
                    "decision_id",
                    "request",
                    "base",
                ][..]
            } else {
                &[
                    "schema",
                    "id",
                    "expected_workspace",
                    "op",
                    "goal_id",
                    "decision_id",
                    "operation_id",
                ][..]
            };
            if value
                .as_object()
                .is_none_or(|v| v.keys().any(|k| !allowed.contains(&k.as_str())))
            {
                return Err("Unexpected reuse command field".into());
            }
            if op == Some("discussion_decision_reuse_write") {
                for (name, fields) in [
                    (
                        "request",
                        &[
                            "schema",
                            "brain_id",
                            "operation_id",
                            "path",
                            "expected_revision",
                            "content_base64",
                        ][..],
                    ),
                    (
                        "base",
                        &[
                            "schema",
                            "brain_id",
                            "path",
                            "revision",
                            "content_base64",
                            "media_type",
                        ][..],
                    ),
                ] {
                    if value[name].as_object().is_none_or(|v| {
                        v.len() != fields.len() || v.keys().any(|k| !fields.contains(&k.as_str()))
                    }) {
                        return Err("Reuse Save requires exact source/base shape".into());
                    }
                }
            }
        }
        #[derive(Deserialize)]
        struct Wire {
            schema: String,
            id: Value,
            expected_workspace: Option<Value>,
            #[serde(flatten)]
            command: Command,
        }
        if matches!(
            value["op"].as_str(),
            Some("goal_criteria_get" | "goal_criteria_write")
        ) && (value["schema"] != "ai-brain/workspace-v1"
            || !value["expected_workspace"].is_object())
        {
            return Err("Outcome criteria editing requires the exact workspace guard".into());
        }
        if matches!(op, Some("goal_criteria_get" | "goal_criteria_write")) {
            let allowed = if op == Some("goal_criteria_write") {
                &[
                    "schema",
                    "id",
                    "expected_workspace",
                    "op",
                    "goal_id",
                    "request",
                    "base",
                ][..]
            } else {
                &["schema", "id", "expected_workspace", "op", "goal_id"][..]
            };
            if value
                .as_object()
                .is_none_or(|v| v.keys().any(|k| !allowed.contains(&k.as_str())))
            {
                return Err("Unexpected criteria command fields".into());
            }
        }
        let wire: Wire = serde_json::from_value(value).map_err(|e| e.to_string())?;
        Ok(Self {
            schema: wire.schema,
            id: wire.id,
            expected_workspace: wire.expected_workspace,
            command: wire.command,
        })
    }
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Command {
    SuggestionsGet,
    SuggestionsSet {
        #[serde(flatten)]
        request: crate::suggestions::SetRequest,
    },
    Snapshot {
        goal_id: Option<String>,
    },
    WorkspaceAttention,
    ProposalList {
        #[serde(flatten)]
        request: crate::proposal::ListRequest,
    },
    ProposalGet {
        #[serde(flatten)]
        request: crate::proposal::Lookup,
    },
    ProposalDisposition {
        #[serde(flatten)]
        request: crate::proposal::Request,
    },
    ProposalAdopt {
        #[serde(flatten)]
        request: crate::proposal::AdoptRequest,
    },
    ProposalRetry {
        #[serde(flatten)]
        request: crate::proposal::RetryRequest,
    },
    MaestroApprovalReview {
        #[serde(flatten)]
        request: crate::maestro_control::ReviewRequest,
    },
    MaestroApprovalDecision {
        #[serde(flatten)]
        request: crate::maestro_control::Request,
    },
    MaestroApprovalReconcile {
        #[serde(flatten)]
        request: crate::maestro_operations::Lookup,
    },
    MaestroDiscover,
    MaestroOperationGet {
        #[serde(flatten)]
        request: crate::maestro_operations::Lookup,
    },
    MaestroOperationAbandon {
        #[serde(flatten)]
        request: crate::maestro_operations::Request,
    },
    MaestroLink {
        #[serde(flatten)]
        request: crate::maestro_links::LinkRequest,
    },
    MaestroGet {
        goal_id: String,
    },
    MaestroUnlink {
        #[serde(flatten)]
        request: crate::maestro_links::UnlinkRequest,
    },
    AttentionList {
        channel: Option<String>,
        limit: Option<usize>,
        cursor: Option<String>,
    },
    AttentionGet {
        goal_id: String,
        attention_id: String,
        revision: Option<String>,
        channel: Option<String>,
    },
    AttentionReply {
        #[serde(flatten)]
        request: crate::attention::Reply,
    },
    AttentionAck {
        #[serde(flatten)]
        request: crate::attention::Mutation,
    },
    InboxPlan {
        #[serde(flatten)]
        request: crate::inbox_plan::Request,
    },
    InboxCapture {
        #[serde(flatten)]
        request: crate::inbox::CaptureRequest,
    },
    InboxList {
        limit: Option<usize>,
        cursor: Option<String>,
    },
    InboxGet {
        capture_id: String,
    },
    SourceBacklinks {
        #[serde(flatten)]
        request: crate::incoming_references::Request,
    },
    BrainIndexStatus,
    BrainIndexRebuild,
    BrainSearch {
        #[serde(flatten)]
        request: crate::retrieval::SearchRequest,
    },
    Capabilities,
    ConnectorsGet,
    T3TargetGet,
    T3TargetPrepare {
        candidate: crate::application::T3Settings,
        #[serde(default)]
        compatibility_manifest: Option<crate::t3_compat::Manifest>,
    },
    T3TargetAdopt {
        request: crate::t3_routes::AdoptRequest,
    },
    ConnectorsSave {
        config: ApplicationConfig,
    },
    ConnectorsReconnect,
    ConnectorsCheck {
        config: ApplicationConfig,
    },
    ExportPrepare,
    ExportChunk {
        export_id: String,
        offset: u64,
    },
    ExportRelease {
        export_id: String,
    },
    SourceList,
    SourcePreview {
        path: String,
        content_base64: Option<String>,
    },
    ChatSend {
        #[serde(flatten)]
        request: crate::discussion_send::SendRequest,
    },
    ChatSendGet {
        #[serde(flatten)]
        request: crate::discussion_send::Lookup,
    },
    ChatStart {
        goal_id: String,
        message: String,
        source_paths: Vec<String>,
        conversation_id: Option<String>,
    },
    ChatGet {
        conversation_id: String,
        context_turn_id: Option<String>,
    },
    TaskCreate {
        goal_id: String,
        operation_id: String,
        content: String,
    },
    TodoistInboxList {
        goal_id: String,
        session_id: Option<String>,
    },
    TaskLink {
        goal_id: String,
        task_id: String,
        picker_session_id: Option<String>,
    },
    TaskRefresh {
        goal_id: String,
    },
    TaskReconcile {
        operation_id: String,
    },
    GoalSelection {
        goal_id: String,
        source_paths: Vec<String>,
        conversation_id: Option<String>,
    },
    ContextExportStart {
        goal_id: String,
        packet_id: String,
        packet_revision: String,
    },
    ContextExportGet {
        goal_id: String,
        job_id: String,
    },
    ContextExportCancel {
        goal_id: String,
        job_id: String,
    },
    ContextExportPrepare {
        goal_id: String,
        job_id: String,
    },
    GoalContextBrief {
        goal_id: String,
    },
    ContextGet {
        goal_id: String,
        packet_id: Option<String>,
    },
    ContextPrepare {
        goal_id: String,
        query: String,
        scope: crate::retrieval::SearchScope,
        citations: Vec<crate::retrieval::Citation>,
        #[serde(default)]
        pinned_citation_ids: Vec<String>,
    },
    ContextRevise {
        goal_id: String,
        packet_id: String,
        expected_revision: String,
        text: String,
    },
    StagePrepare {
        reviewed_packet: Option<crate::context::ReviewedPacketRef>,
        previous_result_id: Option<String>,
        goal_id: String,
        conversation_id: Option<String>,
        source_paths: Vec<String>,
        criterion_ids: Vec<String>,
        next_step: String,
    },
    StageRevise {
        goal_id: String,
        operation_id: String,
        expected: PreparedStageGuard,
        next_step: String,
    },
    StageDiscard {
        goal_id: String,
        operation_id: String,
        expected: PreparedStageGuard,
    },
    CriterionEvaluate {
        goal_id: Option<String>,
        result_id: String,
        criterion_id: String,
        evidence_ids: Vec<String>,
        status: String,
        evaluated_by: String,
        evaluated_at: String,
    },
    SourceRead {
        path: String,
    },
    SourceWrite {
        request: okilum_core::source::SourceWrite,
        base: Option<okilum_core::source::SourceSnapshot>,
    },
    SourceConflict {
        brain_id: String,
        path: String,
        conflict_id: String,
    },
    Result {
        result_id: String,
    },
    GoalSource {
        goal_id: Option<String>,
    },
    DiscussionDecisionGet {
        goal_id: String,
        conversation_id: String,
        turn_id: String,
        expected_actor_id: String,
    },
    DiscussionDecisionSave {
        goal_id: String,
        conversation_id: String,
        turn_id: String,
        expected_actor_id: String,
        request: okilum_core::source::SourceWrite,
    },
    DiscussionDecisionReuseGet {
        goal_id: String,
        decision_id: String,
        operation_id: Option<String>,
    },
    DiscussionDecisionReuseWrite {
        goal_id: String,
        decision_id: String,
        request: okilum_core::source::SourceWrite,
        base: okilum_core::source::SourceSnapshot,
    },
    GoalCriteriaGet {
        goal_id: String,
    },
    GoalCriteriaWrite {
        goal_id: String,
        request: okilum_core::source::SourceWrite,
        base: okilum_core::source::SourceSnapshot,
    },
    CreateGoal {
        goal: Goal,
        body: String,
    },
    PrepareStage {
        stage: Stage,
        packet: ContextPacket,
        operation_id: String,
        target: BTreeMap<String, Value>,
    },
    Start {
        goal_id: Option<String>,
        expected: Option<PreparedStageGuard>,
    },
    Reconcile {
        goal_id: Option<String>,
    },
    Poll {
        goal_id: Option<String>,
    },
    Ingest {
        event: EngineEvent,
    },
    AcceptHuman {
        goal_id: Option<String>,
        criterion_id: String,
        actor: String,
        observed_at: String,
        source: SourceRef,
    },
}
impl Backend {
    fn suggestions_provider(&self) -> crate::suggestions::Provider {
        let saved = crate::settings::path(self.runner.operational_root()).is_file();
        let available = saved && self.app.settings.chat.is_some() && self.app.chat.is_some();
        crate::suggestions::Provider {
            available,
            model: self.app.settings.chat.as_ref().map(|c| c.model.clone()),
            message: if available {
                "Saved Chat provider configured; reachability is not tested."
            } else {
                "Save a valid Chat provider and credential reference in Connections."
            }
            .into(),
        }
    }
    fn handle(&mut self, command: Command) -> Result<Value> {
        if matches!(command, Command::ConnectorsGet) {
            return Ok(
                json!({"config":self.app.settings,"states":self.app.connection_states,"saved":crate::settings::path(self.runner.operational_root()).is_file()}),
            );
        }
        if matches!(command, Command::WorkspaceAttention) {
            return self.app.workspace_attention(&mut self.runner);
        }
        match &command {
            Command::ChatSendGet { request } => {
                return Ok(serde_json::to_value(crate::discussion_send::lookup(
                    &self.runner,
                    request,
                )?)?);
            }
            Command::SuggestionsGet => {
                return Ok(serde_json::to_value(
                    self.runner
                        .suggestions_status(self.suggestions_provider())?,
                )?);
            }
            Command::SuggestionsSet { request } => {
                let provider = self.suggestions_provider();
                let result = self.runner.suggestions_set(
                    request.clone(),
                    self.app.local_actor(),
                    provider.available,
                );
                let (receipt, reason) = match result {
                    Ok(receipt) => (Some(receipt), None),
                    Err(error) => match error.downcast_ref::<crate::suggestions::Refusal>() {
                        Some(reason) => (None, Some(*reason)),
                        None => return Err(error),
                    },
                };
                return Ok(serde_json::to_value(crate::suggestions::Outcome {
                    schema: "tessera-suggestions-outcome/v1".into(),
                    status: if receipt.is_some() {
                        "committed"
                    } else {
                        "not_applied"
                    }
                    .into(),
                    workspace: self.runner.workspace_identity(),
                    request: request.clone(),
                    receipt,
                    reason,
                })?);
            }
            Command::MaestroOperationGet { request } => {
                return Ok(serde_json::to_value(self.runner.maestro_operation_get(
                    &request.goal_id,
                    &request.operation_id,
                )?)?);
            }
            Command::MaestroOperationAbandon { request } => {
                let result = self
                    .runner
                    .maestro_operation_reject(
                        request,
                        "abandoned",
                        "Local Maestro operation was explicitly abandoned",
                    )
                    .and_then(|disposition| Ok(serde_json::to_value(disposition)?));
                return result.map_err(|error| self.runner.maestro_operation_error(request, error));
            }
            Command::MaestroGet { goal_id } => return self.runner.maestro_link_view(goal_id),
            Command::MaestroUnlink { request } => {
                return self.runner.maestro_unlink(request.clone())
            }
            _ => {}
        }
        match &command {
            Command::AttentionList {
                channel,
                limit,
                cursor,
            } => {
                let items = self.app.attention_items(&mut self.runner)?;
                return Ok(serde_json::to_value(self.runner.attention_list(
                    items,
                    self.app.local_actor(),
                    channel.as_deref(),
                    *limit,
                    cursor.as_deref(),
                )?)?);
            }
            Command::AttentionGet {
                goal_id,
                attention_id,
                revision,
                channel,
            } => {
                let items = self.app.attention_items(&mut self.runner)?;
                return self.runner.attention_get(
                    items,
                    self.app.local_actor(),
                    channel.as_deref(),
                    goal_id,
                    attention_id,
                    revision.as_deref(),
                );
            }
            Command::AttentionReply { request } => {
                let app = &self.app;
                return Ok(serde_json::to_value(self.runner.attention_mutate(
                    request.mutation.clone(),
                    "save_decision",
                    Some(&request.text),
                    app.local_actor(),
                    |r| app.attention_items(r),
                )?)?);
            }
            Command::AttentionAck { request } => {
                let app = &self.app;
                return Ok(serde_json::to_value(self.runner.attention_mutate(
                    request.clone(),
                    "ack_seen",
                    None,
                    app.local_actor(),
                    |r| app.attention_items(r),
                )?)?);
            }
            Command::ProposalList { request } => {
                return Ok(serde_json::to_value(
                    self.runner.proposal_list(request.clone())?,
                )?)
            }
            Command::ProposalGet { request } => {
                return Ok(serde_json::to_value(
                    self.runner.proposal_get(request.clone())?,
                )?)
            }
            Command::ProposalDisposition { request } => {
                return Ok(serde_json::to_value(
                    self.runner
                        .proposal_disposition_outcome(request.clone(), self.app.local_actor())?,
                )?)
            }
            Command::ProposalAdopt { request } => {
                return Ok(serde_json::to_value(
                    self.runner
                        .proposal_adopt(request.clone(), self.app.local_actor())?,
                )?);
            }
            Command::ProposalRetry { request } => {
                return Ok(serde_json::to_value(self.runner.proposal_retry(
                    request.clone(),
                    self.app.local_actor(),
                    self.app.settings.chat.as_ref(),
                )?)?);
            }
            Command::InboxPlan { request } => {
                return Ok(serde_json::to_value(
                    self.runner
                        .inbox_plan(request.clone(), self.app.local_actor())?,
                )?);
            }
            Command::InboxCapture { request } => {
                return Ok(serde_json::to_value(
                    self.runner
                        .inbox_capture(request.clone(), self.app.local_actor())?,
                )?)
            }
            Command::InboxList { limit, cursor } => {
                return Ok(serde_json::to_value(
                    self.runner.inbox_list(*limit, cursor.as_deref())?,
                )?)
            }
            Command::InboxGet { capture_id } => {
                return Ok(serde_json::to_value(self.runner.inbox_get(capture_id)?)?)
            }
            Command::GoalContextBrief { goal_id } => {
                return self
                    .runner
                    .with_goal(goal_id, |r| r.goal_context_brief(goal_id))
            }
            Command::ContextGet { goal_id, packet_id } => {
                let mut value = self.runner.with_goal(goal_id, |r| {
                    self.app.context_get(r, goal_id, packet_id.as_deref())
                })?;
                if let Some(id) = value["packet"]["id"].as_str() {
                    value["export_job"] =
                        self.context_jobs.latest(goal_id, id).unwrap_or(Value::Null);
                }
                return Ok(value);
            }
            Command::ContextExportGet { goal_id, job_id }
            | Command::ContextExportPrepare { goal_id, job_id } => {
                let job = self.context_jobs.get(goal_id, job_id)?.clone();
                if job.status == "complete" {
                    if let Err(error) = self.runner.with_goal(goal_id, |r| {
                        crate::context::require_reviewed(r, goal_id, &job.packet).map(|_| ())
                    }) {
                        self.context_jobs
                            .stale(goal_id, job_id, error.to_string())?;
                    }
                }
                let job = self.context_jobs.get(goal_id, job_id)?;
                if matches!(command, Command::ContextExportGet { .. }) {
                    return Ok(job.view());
                }
                ensure!(
                    job.status == "complete",
                    "context export is not ready; inspect its job status"
                );
                let package = job.package.as_ref().context("export package missing")?;
                return Ok(serde_json::to_value(self.exports.prepare(|path| {
                    crate::context_export::write_archive(package, path)
                })?)?);
            }
            Command::ContextExportCancel { goal_id, job_id } => {
                return self.context_jobs.cancel(goal_id, job_id)
            }
            _ => {}
        }
        // Export is workspace-wide and never routes, selects or executes a goal.
        match command {
            Command::ExportPrepare => {
                return Ok(serde_json::to_value(
                    self.exports
                        .prepare(|path| self.runner.export_exact(path))?,
                )?)
            }
            Command::ExportChunk { export_id, offset } => {
                return Ok(serde_json::to_value(
                    self.exports.chunk(&export_id, offset)?,
                )?)
            }
            Command::ExportRelease { export_id } => {
                self.exports.release(&export_id)?;
                return Ok(json!({"released":true}));
            }
            _ => {}
        }
        let goal_id = match &command {
            Command::Snapshot { goal_id }
            | Command::GoalSource { goal_id }
            | Command::Start { goal_id, .. }
            | Command::Reconcile { goal_id }
            | Command::Poll { goal_id }
            | Command::AcceptHuman { goal_id, .. }
            | Command::CriterionEvaluate { goal_id, .. } => goal_id.clone(),
            Command::TaskCreate { goal_id, .. }
            | Command::TaskLink { goal_id, .. }
            | Command::TaskRefresh { goal_id }
            | Command::StagePrepare { goal_id, .. }
            | Command::GoalContextBrief { goal_id }
            | Command::DiscussionDecisionGet { goal_id, .. }
            | Command::DiscussionDecisionSave { goal_id, .. }
            | Command::DiscussionDecisionReuseGet { goal_id, .. }
            | Command::DiscussionDecisionReuseWrite { goal_id, .. }
            | Command::GoalCriteriaGet { goal_id }
            | Command::GoalCriteriaWrite { goal_id, .. }
            | Command::ContextGet { goal_id, .. }
            | Command::ContextPrepare { goal_id, .. }
            | Command::ContextRevise { goal_id, .. }
            | Command::StageRevise { goal_id, .. }
            | Command::StageDiscard { goal_id, .. }
            | Command::GoalSelection { goal_id, .. } => Some(goal_id.clone()),
            Command::PrepareStage { stage, .. } => Some(stage.goal_id.clone()),
            Command::TaskReconcile { operation_id } => {
                self.runner.application_owner("mutations", operation_id)
            }
            Command::ChatGet {
                conversation_id, ..
            } => self
                .runner
                .application_owner("conversations", conversation_id),
            _ => None,
        };
        if let Command::TaskCreate {
            operation_id,
            goal_id,
            ..
        } = &command
        {
            ensure!(
                self.runner
                    .application_owner("mutations", operation_id)
                    .is_none_or(|owner| owner == *goal_id),
                "task operation belongs to another goal"
            );
        }
        let Self {
            runner,
            adapters,
            app,
            ..
        } = self;
        if let Some(goal_id) = goal_id {
            runner.with_goal(&goal_id, |runner| {
                Self::handle_inner(runner, adapters, app, command)
            })
        } else {
            Self::handle_inner(runner, adapters, app, command)
        }
    }
    fn adapter_call(
        runner: &mut Runner,
        adapters: &mut Adapters,
        app: &Application,
        operation: &str,
        expected: Option<PreparedStageGuard>,
    ) -> Result<Value> {
        let engine = runner.snapshot()?.stage.context("no stage")?.engine;
        let mut cold = false;
        let key = if engine == "t3" {
            let operation_id = runner
                .snapshot()?
                .dispatch
                .context("no dispatch")?
                .operation_id;
            if let Some(settings) = runner.t3_operation_settings(&operation_id)? {
                let key = format!(
                    "t3:{}:{}",
                    crate::t3_routes::digest(&settings),
                    operation_id
                );
                if !adapters.contains_key(&key) {
                    let mut adapter =
                        crate::t3_routes::adapter(&settings, runner.operational_root())?;
                    if let Some(proof) = runner.t3_operation_proof(&operation_id) {
                        adapter.retain_compatibility_proof(proof);
                    }
                    adapters.insert(key.clone(), Box::new(adapter));
                    cold = true;
                }
                key
            } else {
                app.guard_legacy_t3_route(runner)?;
                engine.clone()
            }
        } else {
            engine.clone()
        };
        let adapter = adapters
            .get_mut(&key)
            .context("adapter is not configured; no execution dispatched")?;
        if operation == "start" {
            app.validate_reviewed_dispatch(runner)?;
        }
        let snapshot = match operation {
            "start" => runner.start_expected(adapter.as_mut(), expected)?,
            "reconcile" => runner.reconcile(adapter.as_mut())?,
            "poll" if cold => runner.reconcile(adapter.as_mut())?,
            _ => runner.poll(adapter.as_mut())?,
        };
        let _ = snapshot;
        app.snapshot(runner)
    }
    fn handle_inner(
        runner: &mut Runner,
        adapters: &mut Adapters,
        app: &Application,
        command: Command,
    ) -> Result<Value> {
        let snapshot = match command {
            Command::Snapshot { .. } => return app.snapshot(runner),
            Command::Capabilities => {
                let mut capabilities = app.capabilities(runner);
                capabilities["source_backlinks"] = json!(true);
                return Ok(capabilities);
            }
            Command::SuggestionsGet
            | Command::SuggestionsSet { .. }
            | Command::ProposalList { .. }
            | Command::ProposalGet { .. }
            | Command::ProposalDisposition { .. }
            | Command::ProposalAdopt { .. }
            | Command::ProposalRetry { .. }
            | Command::AttentionList { .. }
            | Command::AttentionGet { .. }
            | Command::AttentionReply { .. }
            | Command::AttentionAck { .. }
            | Command::InboxPlan { .. }
            | Command::InboxCapture { .. }
            | Command::InboxList { .. }
            | Command::InboxGet { .. }
            | Command::WorkspaceAttention => {
                unreachable!("workspace read handled before goal routing")
            }
            Command::SourceBacklinks { .. }
            | Command::BrainIndexStatus
            | Command::BrainIndexRebuild
            | Command::BrainSearch { .. } => {
                unreachable!("retrieval handled outside the service owner lock")
            }
            Command::ConnectorsGet
            | Command::ConnectorsSave { .. }
            | Command::ConnectorsReconnect
            | Command::ConnectorsCheck { .. }
            | Command::T3TargetGet
            | Command::T3TargetPrepare { .. }
            | Command::T3TargetAdopt { .. }
            | Command::MaestroDiscover
            | Command::MaestroApprovalReview { .. }
            | Command::MaestroApprovalDecision { .. }
            | Command::MaestroApprovalReconcile { .. }
            | Command::MaestroLink { .. }
            | Command::MaestroGet { .. }
            | Command::MaestroOperationGet { .. }
            | Command::MaestroOperationAbandon { .. }
            | Command::MaestroUnlink { .. } => {
                unreachable!("connector settings handled before goal routing")
            }
            Command::ExportPrepare
            | Command::ExportChunk { .. }
            | Command::ExportRelease { .. } => {
                unreachable!("workspace export handled before goal routing")
            }
            Command::SourcePreview {
                path,
                content_base64,
            } => return crate::preview::source_preview(runner, &path, content_base64.as_deref()),
            Command::SourceList => return Ok(json!({"sources":runner.source_list()?})),
            Command::ContextExportStart { .. }
            | Command::ContextExportGet { .. }
            | Command::ContextExportCancel { .. }
            | Command::ContextExportPrepare { .. } => {
                anyhow::bail!("context export requires backend job dispatch")
            }
            Command::ChatStart { .. } | Command::ChatSend { .. } | Command::ChatSendGet { .. } => {
                anyhow::bail!("chat requires backend-owned job dispatch")
            }
            Command::ChatGet {
                conversation_id,
                context_turn_id,
            } => return app.chat_get_context(runner, &conversation_id, context_turn_id.as_deref()),
            Command::TaskCreate {
                goal_id,
                operation_id,
                content,
            } => return app.task_create(runner, goal_id, operation_id, content),
            Command::TaskLink { .. } | Command::TodoistInboxList { .. } => {
                anyhow::bail!("Task provider reads must use outside-lock dispatch")
            }
            Command::TaskRefresh { goal_id } => return app.task_refresh(runner, goal_id),
            Command::TaskReconcile { operation_id } => {
                return app.task_reconcile(runner, operation_id)
            }
            Command::GoalSelection {
                source_paths,
                conversation_id,
                ..
            } => return app.select_sources(runner, source_paths, conversation_id),
            Command::GoalContextBrief { goal_id } => return runner.goal_context_brief(&goal_id),
            Command::ContextGet { goal_id, packet_id } => {
                return app.context_get(runner, &goal_id, packet_id.as_deref())
            }
            Command::ContextPrepare {
                goal_id,
                query,
                scope,
                citations,
                pinned_citation_ids,
            } => {
                return app.context_prepare(
                    runner,
                    goal_id,
                    query,
                    scope,
                    citations,
                    pinned_citation_ids,
                )
            }
            Command::ContextRevise {
                goal_id,
                packet_id,
                expected_revision,
                text,
            } => return app.context_revise(runner, goal_id, packet_id, expected_revision, text),
            Command::StagePrepare {
                reviewed_packet,
                previous_result_id,
                goal_id,
                conversation_id,
                source_paths,
                criterion_ids,
                next_step,
            } => {
                if let Some(reference) = reviewed_packet {
                    app.stage_prepare_reviewed(
                        runner,
                        goal_id,
                        criterion_ids,
                        reference,
                        previous_result_id,
                    )?;
                    return app.snapshot(runner);
                }
                app.stage_prepare_with_previous(
                    runner,
                    goal_id,
                    conversation_id,
                    source_paths,
                    criterion_ids,
                    next_step,
                    previous_result_id,
                )?;
                return app.snapshot(runner);
            }
            Command::StageRevise {
                goal_id,
                operation_id,
                expected,
                next_step,
            } => {
                ensure!(runner.snapshot()?.dispatch.as_ref().is_none_or(|d|!d.packet.extra.contains_key("reviewed_packet")),"Review the context packet, then discard and prepare its new revision; direct stage guidance edits cannot replace a reviewed packet");
                let receipt = runner.change_prepared(PreparedChangeRequest {
                    goal_id,
                    operation_id,
                    expected,
                    change: PreparedChange::Revise { next_step },
                })?;
                let mut snapshot = app.snapshot(runner)?;
                snapshot["prepared_change"] = serde_json::to_value(receipt)?;
                return Ok(snapshot);
            }
            Command::StageDiscard {
                goal_id,
                operation_id,
                expected,
            } => {
                let receipt = runner.change_prepared(PreparedChangeRequest {
                    goal_id,
                    operation_id,
                    expected,
                    change: PreparedChange::Discard,
                })?;
                let mut snapshot = app.snapshot(runner)?;
                snapshot["prepared_change"] = serde_json::to_value(receipt)?;
                return Ok(snapshot);
            }
            Command::CriterionEvaluate {
                goal_id: _,
                result_id,
                criterion_id,
                evidence_ids,
                status,
                evaluated_by,
                evaluated_at,
            } => runner.evaluate_result(
                result_id,
                CriterionEvaluation {
                    criterion_id,
                    goal_revision: String::new(),
                    status,
                    evidence_ids,
                    evaluated_by,
                    evaluated_at,
                },
            )?,
            Command::SourceRead { path } => {
                return Ok(serde_json::to_value(runner.read_source(&path)?)?)
            }
            Command::SourceWrite { request, base } => {
                return Ok(serde_json::to_value(
                    runner.write_source_with_base(request, base)?,
                )?)
            }
            Command::SourceConflict {
                brain_id,
                path,
                conflict_id,
            } => {
                return Ok(serde_json::to_value(runner.source_conflict(
                    &brain_id,
                    &path,
                    &conflict_id,
                )?)?)
            }
            Command::Result { result_id } => {
                return Ok(serde_json::to_value(runner.result(&result_id)?)?)
            }
            Command::GoalSource { .. } => return Ok(serde_json::to_value(runner.goal_source()?)?),
            Command::DiscussionDecisionGet {
                goal_id,
                conversation_id,
                turn_id,
                expected_actor_id,
            } => {
                let key = crate::runtime::discussion_decision::Key {
                    goal_id,
                    conversation_id,
                    turn_id,
                    expected_actor_id,
                };
                return runner.discussion_decision_get(&key, app.local_actor());
            }
            Command::DiscussionDecisionSave {
                goal_id,
                conversation_id,
                turn_id,
                expected_actor_id,
                request,
            } => {
                let key = crate::runtime::discussion_decision::Key {
                    goal_id,
                    conversation_id,
                    turn_id,
                    expected_actor_id,
                };
                return runner.discussion_decision_save(&key, request, app.local_actor());
            }
            Command::DiscussionDecisionReuseGet {
                goal_id,
                decision_id,
                operation_id,
            } => {
                return runner.discussion_decision_reuse_get(
                    &goal_id,
                    &decision_id,
                    operation_id.as_deref(),
                    app.local_actor(),
                )
            }
            Command::DiscussionDecisionReuseWrite {
                goal_id,
                decision_id,
                request,
                base,
            } => {
                return Ok(serde_json::to_value(
                    runner.discussion_decision_reuse_write(
                        &goal_id,
                        &decision_id,
                        request,
                        base,
                        app.local_actor(),
                    )?,
                )?)
            }
            Command::GoalCriteriaGet { goal_id } => return runner.goal_criteria_get(&goal_id),
            Command::GoalCriteriaWrite {
                goal_id,
                request,
                base,
            } => {
                return Ok(serde_json::to_value(
                    runner.goal_criteria_write(&goal_id, request, base)?,
                )?)
            }
            Command::CreateGoal { goal, body } => {
                let id = goal.id.clone();
                runner.create_goal(goal, body)?;
                return runner.with_goal(&id, |runner| app.snapshot(runner));
            }
            Command::PrepareStage {
                stage,
                packet,
                operation_id,
                target,
            } => {
                if stage.engine == "t3" {
                    app.bind_fresh_t3_origin(runner)?;
                }
                runner.prepare_stage(stage, packet, operation_id, target)?
            }
            Command::Start { expected, .. } => {
                return Self::adapter_call(runner, adapters, app, "start", expected)
            }
            Command::Reconcile { .. } => {
                return Self::adapter_call(runner, adapters, app, "reconcile", None)
            }
            Command::Poll { .. } => return Self::adapter_call(runner, adapters, app, "poll", None),
            Command::Ingest { event } => runner.ingest(event)?,
            Command::AcceptHuman {
                goal_id: _,
                criterion_id,
                actor,
                observed_at,
                source,
            } => runner.accept_human(criterion_id, actor, observed_at, source)?,
        };
        let _ = snapshot;
        app.snapshot(runner)
    }
}

pub fn serve(listener: TcpListener, runner: Runner, adapters: Adapters) -> Result<()> {
    serve_application(listener, runner, adapters, Application::unconfigured())
}
pub fn serve_application(
    listener: TcpListener,
    runner: Runner,
    adapters: Adapters,
    app: Application,
) -> Result<()> {
    serve_application_with_connector(listener, runner, adapters, app, None)
}
fn serve_application_with_connector(
    listener: TcpListener,
    mut runner: Runner,
    adapters: Adapters,
    app: Application,
    connector: Option<crate::connector::Listener>,
) -> Result<()> {
    for goal_id in runner.goal_ids() {
        runner.with_goal(&goal_id, |r| app.recover(r))?;
    }
    ensure!(
        listener.local_addr()?.ip().is_loopback(),
        "POC listener must be loopback-only"
    );
    let identity = runner.workspace_identity();
    let index = crate::retrieval::BrainIndex::start(
        identity["brain_id"].as_str().unwrap().to_owned(),
        runner.root().to_owned(),
        identity["records_dir"].as_str().unwrap().to_owned(),
        runner.operational_root().to_owned(),
        runner.managed(),
    );
    let (index, index_error) = match index {
        Ok(index) => (Some(index), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let context_jobs = crate::context_jobs::Jobs::open(runner.operational_root())?;
    let backend = Arc::new(Mutex::new(Backend {
        runner,
        adapters,
        app,
        exports: crate::export::ExportDownloads::default(),
        index,
        index_error,
        context_jobs,
        todoist_picker: Default::default(),
    }));
    if let Some(connector) = connector {
        connector_service::start(connector, backend.clone());
    }
    start_proposal_generation(backend.clone());
    let observer = backend.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(crate::maestro::POLL_SECONDS));
        let captured = {
            let Ok(b) = observer.lock() else { return };
            if b.runner.maestro_has_links() {
                b.app
                    .settings
                    .maestro
                    .clone()
                    .map(|c| (c, b.runner.maestro_links_token()))
            } else {
                None
            }
        };
        let Some((config, links_token)) = captured else {
            continue;
        };
        // Network and credential resolution happen outside the backend owner mutex.
        let result = crate::maestro::Client::new(config.clone()).and_then(|c| c.discover());
        let Ok(mut b) = observer.lock() else { return };
        if b.app.settings.maestro.as_ref() != Some(&config)
            || b.runner.maestro_links_token() != links_token
        {
            continue;
        }
        let applied = match result {
            Ok(d) => b.runner.maestro_observe(&d),
            Err(_) => b.runner.maestro_failed(&config.identity(), false),
        };
        if applied.is_err() {
            let _ = b.runner.maestro_failed(&config.identity(), true);
            eprintln!("Maestro observation could not be committed; retained evidence preserved");
        }
    });
    let driver = backend.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let Ok(mut b) = driver.lock() else { return };
        for goal_id in b.runner.goal_ids() {
            let Backend {
                runner,
                adapters,
                app,
                ..
            } = &mut *b;
            let result = runner.with_goal(&goal_id, |runner| {
                let Some(engine) = runner.active_engine() else {
                    return Ok(Value::Null);
                };
                if engine != "t3" && !adapters.contains_key(&engine) {
                    return Ok(Value::Null);
                }
                let phase = runner.snapshot()?.phase;
                let op = if phase.as_deref() == Some("running") {
                    "poll"
                } else {
                    "reconcile"
                };
                Backend::adapter_call(runner, adapters, app, op, None)
            });
            if let Err(e) = result {
                eprintln!("brain driver ({goal_id}): {e}");
            }
        }
    });
    for stream in listener.incoming() {
        let stream = stream?;
        let backend = backend.clone();
        std::thread::spawn(move || {
            let _ = connection(stream, backend);
        });
    }
    Ok(())
}
fn target_outcome(
    workspace: Value,
    request: &crate::t3_routes::AdoptRequest,
    receipt: Option<crate::t3_routes::Receipt>,
    reason: Option<&str>,
) -> Value {
    json!({"schema":"tessera-t3-target-outcome/v1","workspace":workspace,"request":request,
        "status":if receipt.is_some(){"committed"}else{"not_applied"},"receipt":receipt,"reason":reason})
}
fn dispatch_t3_target(backend: &Arc<Mutex<Backend>>, command: &Command) -> Result<Value> {
    dispatch_t3_target_with(backend, command, crate::t3_routes::discover)
}
fn dispatch_t3_target_with(
    backend: &Arc<Mutex<Backend>>,
    command: &Command,
    discover: impl FnOnce(&crate::application::T3Settings) -> Result<Value>,
) -> Result<Value> {
    let manifest = match command {
        Command::T3TargetPrepare {
            compatibility_manifest,
            ..
        } => compatibility_manifest.clone(),
        Command::T3TargetAdopt { request } => request.review.compatibility_manifest.clone(),
        _ => None,
    };
    let (candidate, config, before) = {
        let owner = backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
        if matches!(command, Command::T3TargetGet) {
            return Ok(owner.runner.t3_route_view(&owner.app.settings));
        }
        let candidate = match command {
            Command::T3TargetPrepare { candidate, .. } => candidate.clone(),
            Command::T3TargetAdopt { request } => {
                match owner.runner.t3_transition_receipt(request) {
                    Ok(Some(receipt)) => {
                        return Ok(target_outcome(
                            owner.runner.workspace_identity(),
                            request,
                            Some(receipt),
                            None,
                        ))
                    }
                    Err(error) if error.to_string() == "transition_operation_conflict" => {
                        return Ok(target_outcome(
                            owner.runner.workspace_identity(),
                            request,
                            None,
                            Some("transition_operation_conflict"),
                        ));
                    }
                    Err(error) => return Err(error),
                    _ => {}
                }
                request.review.candidate.clone()
            }
            _ => unreachable!(),
        };
        let mut config = owner.app.settings.clone();
        config.t3 = Some(candidate.clone());
        crate::settings::validate_references(&config, &owner.runner)?;
        let config = owner.app.settings.clone();
        let before = owner.runner.t3_review_with_manifest(
            &config,
            candidate.clone(),
            &Err(anyhow::anyhow!("not yet observed")),
            manifest.clone(),
        );
        if let Command::T3TargetAdopt { request } = command {
            if before.guard != request.review.guard {
                return Ok(target_outcome(
                    owner.runner.workspace_identity(),
                    request,
                    None,
                    Some("prepared_selection_stale"),
                ));
            }
        }
        (candidate, config, before)
    };
    let observed = discover(&candidate);
    let mut owner = backend
        .lock()
        .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
    // Another identical request may have committed while discovery was outside
    // the owner lock. Receipt lookup precedes every stale/not-applied decision.
    if let Command::T3TargetAdopt { request } = command {
        match owner.runner.t3_transition_receipt(request) {
            Ok(Some(receipt)) => {
                return Ok(target_outcome(
                    owner.runner.workspace_identity(),
                    request,
                    Some(receipt),
                    None,
                ))
            }
            Err(error) if error.to_string() == "transition_operation_conflict" => {
                return Ok(target_outcome(
                    owner.runner.workspace_identity(),
                    request,
                    None,
                    Some("transition_operation_conflict"),
                ))
            }
            Err(error) => return Err(error),
            _ => {}
        }
    }
    let current =
        owner
            .runner
            .t3_review_with_manifest(&owner.app.settings, candidate, &observed, manifest);
    let stable = current.guard == before.guard
        && serde_json::to_value(&config)? == serde_json::to_value(&owner.app.settings)?;
    match command {
        Command::T3TargetPrepare { .. } => {
            let mut review = current;
            if !stable {
                review.ready = false;
                review.blockers.push(crate::t3_routes::Blocker {
                    code: "snapshot_changed".into(),
                    operation_id: None,
                });
            }
            Ok(serde_json::to_value(review)?)
        }
        Command::T3TargetAdopt { request } => {
            if !stable {
                return Ok(target_outcome(
                    owner.runner.workspace_identity(),
                    request,
                    None,
                    Some("snapshot_changed"),
                ));
            }
            let config = owner.app.settings.clone();
            let result = owner.runner.t3_adopt(&config, request.clone(), &observed);
            match result {
                Ok(receipt) => Ok(target_outcome(
                    owner.runner.workspace_identity(),
                    request,
                    Some(receipt),
                    None,
                )),
                Err(_) => {
                    // mutation() reloads durable state. A postcommit failure must
                    // return the recorded receipt, never certify not_applied.
                    match owner.runner.t3_transition_receipt(request)? {
                        Some(receipt) => Ok(target_outcome(
                            owner.runner.workspace_identity(),
                            request,
                            Some(receipt),
                            None,
                        )),
                        None => Ok(target_outcome(
                            owner.runner.workspace_identity(),
                            request,
                            None,
                            Some("adoption_not_applied"),
                        )),
                    }
                }
            }
        }
        _ => unreachable!(),
    }
}

fn dispatch_maestro_decision(backend: &Arc<Mutex<Backend>>, command: &Command) -> Result<Value> {
    use crate::maestro_operations::Request as Operation;
    let reconcile = matches!(command, Command::MaestroApprovalReconcile { .. });
    let request = {
        let owner = backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
        match command {
            Command::MaestroApprovalDecision { request } => request.clone(),
            Command::MaestroApprovalReconcile { request } => {
                let d = owner
                    .runner
                    .maestro_operation_get(&request.goal_id, &request.operation_id)?;
                ensure!(
                    d.kind.as_deref() == Some("approval_decision"),
                    "No retained approval decision"
                );
                serde_json::from_value(d.request.context("Missing retained decision")?)?
            }
            _ => unreachable!(),
        }
    };
    let operation = Operation::ApprovalDecision(Box::new(request.clone()));
    let mut begin_attempted = false;
    let result = (|| -> Result<Value> {
        request.validate()?;
        let (config, retained) = {
            let owner = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            if let Some(receipt) = owner.runner.maestro_operation_replay(&operation)? {
                return Ok(receipt);
            }
            let config = owner
                .app
                .settings
                .maestro
                .clone()
                .context("Original Maestro connection is unavailable")?;
            ensure!(
                config.identity() == request.instance,
                "Original Maestro instance is unavailable"
            );
            if !reconcile {
                owner.runner.maestro_decision_scope(&request, &config)?;
            } else {
                owner.runner.maestro_control_link(
                    &request.goal_id,
                    &request.expected_link_id,
                    false,
                )?;
            }
            let retained = owner
                .runner
                .maestro_operation_get(&request.goal_id, &request.operation_id)?
                .status
                == "pending";
            (config, retained)
        };
        let client = crate::maestro::Client::new(config.clone())?;
        let view = client.guarded_view(&request.review.expected)?;
        if let Some(receipt) = view
            .decision_receipt
            .as_ref()
            .filter(|r| retained && r.matches(&request))
        {
            let mut owner = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            return owner
                .runner
                .maestro_decision_commit(&request, receipt, &view.status);
        }
        if reconcile {
            let owner = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            return Ok(serde_json::to_value(owner.runner.maestro_operation_get(
                &request.goal_id,
                &request.operation_id,
            )?)?);
        }
        ensure!(
            view.supported
                && view.status == "pending"
                && view.review.as_ref() == Some(&request.review),
            "Current guarded review differs or is unavailable; original request remains retained"
        );
        {
            let mut owner = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            ensure!(
                owner.app.settings.maestro.as_ref() == Some(&config),
                "Maestro connection changed before send"
            );
            owner.runner.maestro_decision_scope(&request, &config)?;
            begin_attempted = true;
            if let Some(receipt) = owner.runner.maestro_operation_begin(&operation)? {
                return Ok(receipt);
            }
        }
        let response = client.guarded_decide(&request)?;
        let mut owner = backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
        owner.runner.maestro_decision_commit(
            &request,
            &response.receipt,
            &response.execution_status,
        )
    })();
    result.map_err(|error| {
        backend
            .lock()
            .map(|mut owner| {
                if !reconcile && !begin_attempted {
                    // This request has not crossed this process's send boundary; a
                    // concurrent or previous pending intention prevents rejection.
                    let _ = owner
                        .runner
                        .maestro_decision_refuse_unsent(&request, &error.to_string());
                }
                owner
                    .runner
                    .maestro_operation_error(&operation, anyhow::anyhow!(error.to_string()))
            })
            .unwrap_or(error)
    })
}

fn dispatch(
    backend: &Arc<Mutex<Backend>>,
    command: Command,
    expected_workspace: Option<Value>,
) -> Result<Value> {
    // Workspace identity is immutable for this backend. Validate under the same
    // owner lock before either synchronous commands or background chat dispatch.
    if let Some(expected) = expected_workspace {
        let locked = backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
        ensure!(
            expected == locked.runner.workspace_identity(),
            "Workspace identity changed. Reconnect the selected brain."
        );
    }
    match command {
        Command::TodoistInboxList {
            goal_id,
            session_id,
        } => return todoist_picker::list(backend, goal_id, session_id),
        Command::TaskLink {
            goal_id,
            task_id,
            picker_session_id,
        } => return todoist_picker::link(backend, goal_id, task_id, picker_session_id),
        _ => {}
    }
    if matches!(
        command,
        Command::BrainIndexStatus
            | Command::BrainIndexRebuild
            | Command::BrainSearch { .. }
            | Command::SourceBacklinks { .. }
    ) {
        let (index, failure) = {
            let locked = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            if let Command::BrainSearch { request } = &command {
                ensure!(
                    locked.runner.goal_ids().contains(&request.scope.goal_id),
                    "unknown target goal"
                );
            }
            if let Command::SourceBacklinks { request } = &command {
                ensure!(
                    locked.runner.goal_ids().contains(&request.scope.goal_id),
                    "unknown target goal"
                );
            }
            (locked.index.clone(), locked.index_error.clone())
        };
        let Some(index) = index else {
            if matches!(command, Command::BrainIndexStatus) {
                return Ok(json!({"status":"unavailable","error":failure}));
            }
            return Err(crate::retrieval::error(
                "index_unavailable",
                failure.unwrap_or_else(|| "Brain index unavailable".into()),
            ));
        };
        return match command {
            Command::BrainIndexStatus => Ok(serde_json::to_value(index.status())?),
            Command::BrainIndexRebuild => Ok(serde_json::to_value(index.rebuild()?)?),
            Command::BrainSearch { request } => Ok(serde_json::to_value(index.search(request)?)?),
            Command::SourceBacklinks { request } => {
                Ok(serde_json::to_value(index.source_backlinks(request)?)?)
            }
            _ => unreachable!(),
        };
    }
    if let Command::ContextExportStart {
        goal_id,
        packet_id,
        packet_revision,
    } = &command
    {
        let goal_id = goal_id.clone();
        let reference = crate::context::ReviewedPacketRef {
            id: packet_id.clone(),
            revision: packet_revision.clone(),
        };
        let (job, config, input, cancel) = {
            let mut locked = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            let config = locked
                .app
                .chat
                .clone()
                .context("AI conversation provider is not configured")?;
            let input = locked.runner.with_goal(&goal_id, |runner| {
                crate::context_jobs::capture(runner, &goal_id, &reference)
            })?;
            let (job, cancel) = locked
                .context_jobs
                .start(goal_id.clone(), reference.clone())?;
            (job, config, input, cancel)
        };
        let response = job.view();
        let shared = backend.clone();
        std::thread::spawn(move || {
            let generated = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(crate::context_export::generate(
                    config,
                    input,
                    cancel,
                    |_| {},
                )),
                Err(_) => crate::context_export::Generation::Error {
                    code: "export_runtime_unavailable".into(),
                },
            };
            if let Ok(mut locked) = shared.lock() {
                let fresh = locked.runner.with_goal(&goal_id, |runner| {
                    crate::context::require_reviewed(runner, &goal_id, &reference).map(|_| ())
                });
                if let Err(error) =
                    locked
                        .context_jobs
                        .finish(&goal_id, &job.job_id, generated, fresh)
                {
                    eprintln!("context export persistence failed: {error}");
                }
            }
        });
        return Ok(response);
    }
    if let Command::MaestroApprovalReview { ref request } = command {
        let (config, link) = {
            let owner = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            let link = owner.runner.maestro_control_link(
                &request.goal_id,
                &request.expected_link_id,
                true,
            )?;
            let config = owner
                .app
                .settings
                .maestro
                .clone()
                .context("Maestro is not configured")?;
            ensure!(
                link.instance == config.identity(),
                "Linked provider identity changed"
            );
            (config, link)
        };
        let expected = crate::maestro_control::Expected {
            version: "v1".into(),
            project_id: link.project_id.clone(),
            project_name: link.project_name.clone(),
            project_repo: link.repo.clone(),
            approval_id: request.approval_id.clone(),
            created_at: String::new(),
            decision_revision: String::new(),
        };
        let mut view = crate::maestro::Client::new(config.clone())?.guarded_view(&expected)?;
        let owner = backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
        ensure!(
            owner.app.settings.maestro.as_ref() == Some(&config)
                && owner
                    .runner
                    .maestro_control_link(&request.goal_id, &request.expected_link_id, true)?
                    .instance
                    == link.instance,
            "Review scope changed"
        );
        view.supported = view.supported
            && crate::maestro_control::NEW_DECISIONS_ENABLED
            && view.status == "pending"
            && view
                .review
                .as_ref()
                .is_some_and(|r| r.target.issue == link.issue_number);
        return Ok(
            json!({"goal_id":request.goal_id,"link_id":link.id,"instance":link.instance,"view":view}),
        );
    }
    if matches!(
        command,
        Command::MaestroApprovalDecision { .. } | Command::MaestroApprovalReconcile { .. }
    ) {
        return dispatch_maestro_decision(backend, &command);
    }
    if matches!(
        command,
        Command::MaestroDiscover | Command::MaestroLink { .. }
    ) {
        let operation = match &command {
            Command::MaestroLink { request } => {
                Some(crate::maestro_operations::Request::Link(request.clone()))
            }
            _ => None,
        };
        let attempt = (|| -> Result<Value> {
            let (config, link_history) = {
                let mut owner = backend
                    .lock()
                    .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
                if let Some(request) = &operation {
                    if let Some(receipt) = owner.runner.maestro_operation_begin(request)? {
                        return Ok(receipt);
                    }
                    if !crate::maestro_links::NEW_LINKS_ENABLED {
                        return owner.runner.maestro_reject_result(
                            request,
                            "new_links_disabled",
                            "New Maestro links are disabled by this maintenance build",
                        );
                    }
                }
                let config = owner
                    .app
                    .settings
                    .maestro
                    .clone()
                    .context("Maestro observation is not configured")?;
                let history = operation
                    .as_ref()
                    .map(|request| owner.runner.maestro_goal_links_token(request.goal_id()));
                (config, history)
            };
            // The intention is durable before this GET; no owner lock is retained.
            let discovery = crate::maestro::Client::new(config.clone())?.discover()?;
            let mut owner = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            if let Some(request) = &operation {
                // A concurrent commit or durable rejection always wins over this late GET.
                if let Some(receipt) = owner.runner.maestro_operation_replay(request)? {
                    return Ok(receipt);
                }
                if owner.app.settings.maestro.as_ref() != Some(&config) {
                    return owner.runner.maestro_reject_result(
                        request,
                        "configuration_changed",
                        "Maestro configuration changed during observation",
                    );
                }
                if link_history != Some(owner.runner.maestro_goal_links_token(request.goal_id())) {
                    return owner.runner.maestro_reject_result(request, "link_history_changed", "Maestro link history changed during observation; rediscover before linking");
                }
            } else {
                ensure!(
                    owner.app.settings.maestro.as_ref() == Some(&config),
                    "Maestro configuration changed during observation"
                );
            }
            match command {
                Command::MaestroDiscover => Ok(crate::maestro_links::choices(&discovery)),
                Command::MaestroLink { request } => owner.runner.maestro_link(request, &discovery),
                _ => unreachable!(),
            }
        })();
        return match (attempt, operation) {
            (Err(error), Some(request)) => {
                let owner = backend
                    .lock()
                    .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
                Err(owner.runner.maestro_operation_error(&request, error))
            }
            (result, _) => result,
        };
    }
    if matches!(
        &command,
        Command::T3TargetGet | Command::T3TargetPrepare { .. } | Command::T3TargetAdopt { .. }
    ) {
        return dispatch_t3_target(backend, &command);
    }
    if let Command::ConnectorsCheck { config } = &command {
        {
            let locked = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            crate::settings::validate_references(config, &locked.runner)?;
        }
        return Ok(crate::settings::check(config));
    }
    if matches!(
        command,
        Command::ConnectorsSave { .. } | Command::ConnectorsReconnect
    ) {
        let (mut candidate, adapters, operational, account, previous_config, original_account) = {
            let mut locked = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            let operational = locked.runner.operational_root().to_path_buf();
            let saved = crate::settings::load(&operational)?;
            let account = saved.as_ref().and_then(|(_, account)| account.clone());
            let config = match command {
                Command::ConnectorsSave { config } => config,
                _ => saved
                    .map(|s| s.0)
                    .unwrap_or_else(|| locked.app.settings.clone()),
            };
            crate::settings::validate_references(&config, &locked.runner)?;
            let previous_config = serde_json::to_value(&locked.app.settings)?;
            let original_account = if account.is_none()
                && locked.runner.has_external_work()
                && locked.app.settings.todoist.is_some()
            {
                locked.app.account_adapter()
            } else {
                None
            };
            if account.is_none()
                && locked.runner.has_external_work()
                && locked.app.settings.todoist.is_some()
            {
                ensure!(original_account.is_some(),"Cannot verify the previous Todoist account. Restore its original credential before reconnecting existing tasks.");
            }
            let (app, adapters) =
                Application::configure_runtime(config, &operational, &mut locked.runner, false)?;
            (
                app,
                adapters,
                operational,
                account,
                previous_config,
                original_account,
            )
        };
        let previous_account = account.clone();
        // Provider reads never hold the runner lock; unrelated source access and
        // ongoing engine recovery remain available during auth/discovery failures.
        let account = if let Some(original) = original_account {
            Some(original.account_id().context("Cannot verify the previous Todoist account; restore its original credential first.")?)
        } else {
            account
        };
        let account = crate::settings::authenticate(&mut candidate, account);
        let mut locked = backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
        ensure!(
            serde_json::to_value(&locked.app.settings)? == previous_config,
            "Connection settings changed while reconnecting. Reload settings before retrying."
        );
        ensure!(
            crate::settings::load(&operational)?.and_then(|(_, pin)| pin) == previous_account,
            "Connection account identity changed while reconnecting. Reconnect the saved account again."
        );
        candidate.guard_target(&locked.runner)?;
        crate::settings::save(&operational, &candidate.settings, account)?;
        candidate.bind_target(&mut locked.runner)?;
        locked.todoist_picker.invalidate();
        locked.app = candidate;
        locked.adapters = adapters;
        return Ok(
            json!({"config":locked.app.settings,"states":locked.app.connection_states,"saved":true}),
        );
    }
    if let Command::ChatSend { request } = command {
        let goal_id = request.goal_id.clone();
        let admission = {
            let mut owner = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            let Backend { runner, app, .. } = &mut *owner;
            crate::discussion_send::send(app, runner, request)?
        };
        return match admission {
            crate::discussion_send::Admission::ExistingOperation(receipt) => {
                Ok(serde_json::to_value(receipt)?)
            }
            crate::discussion_send::Admission::NewlyCommitted(prepared, receipt) => {
                start_chat(backend.clone(), goal_id, *prepared);
                Ok(serde_json::to_value(receipt)?)
            }
        };
    }
    if let Command::ChatStart {
        goal_id,
        message,
        source_paths,
        conversation_id,
    } = command
    {
        let crate::discussion_context::Prepared {
            id,
            turn_id,
            config,
            body,
            request: prepared_request,
        } = {
            let mut locked = backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend unavailable"))?;
            let Backend { runner, app, .. } = &mut *locked;
            runner.with_goal(&goal_id, |runner| {
                app.chat_start_prepared(
                    runner,
                    goal_id.clone(),
                    message,
                    source_paths,
                    conversation_id,
                )
            })?
        };
        start_chat(
            backend.clone(),
            goal_id,
            crate::discussion_context::Prepared {
                id: id.clone(),
                turn_id: turn_id.clone(),
                config,
                body,
                request: prepared_request,
            },
        );
        Ok(json!({"conversation_id":id,"turn_id":turn_id,"status":"running"}))
    } else {
        backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend unavailable"))?
            .handle(command)
    }
}
fn start_chat(
    backend: Arc<Mutex<Backend>>,
    goal_id: String,
    prepared: crate::discussion_context::Prepared,
) {
    let crate::discussion_context::Prepared {
        id, config, body, ..
    } = prepared;
    let shared = backend.clone();
    let job_id = id.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let result: Result<()> = match runtime {
            Ok(runtime) => runtime.block_on(async {
                let client = chat::ChatClient::new(config).map_err(anyhow::Error::msg)?;
                let (cancel, receive) = tokio::sync::watch::channel(false);
                client
                    .run_prepared(body, receive, |event| {
                        let persisted = shared
                            .lock()
                            .map_err(|_| anyhow::anyhow!("backend unavailable"))
                            .and_then(|mut b| {
                                b.runner.with_goal(&goal_id, |runner| {
                                    Application::chat_event(runner, &job_id, event)
                                })
                            });
                        if persisted.is_err() {
                            let _ = cancel.send(true);
                        }
                    })
                    .await;
                Ok(())
            }),
            Err(_) => Err(anyhow::anyhow!("chat runtime unavailable")),
        };
        if result.is_err() {
            if let Ok(mut b) = shared.lock() {
                let _ = b.runner.with_goal(&goal_id, |runner| {
                    Application::chat_event(
                        runner,
                        &job_id,
                        chat::ChatEvent::Error {
                            text: String::new(),
                            code: "chat_initialization_failed".into(),
                        },
                    )
                });
            }
        }
    });
}

/// One worker per backend; a cancelled local future ends before another attempt starts.
fn start_proposal_generation(backend: Arc<Mutex<Backend>>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let job = {
                let Ok(mut b) = backend.lock() else { return };
                let settings = b.app.settings.chat.clone();
                if b.runner.suggestions_controlled() && !b.suggestions_provider().available {
                    continue;
                }
                match b.runner.prepare_proposal_generation(settings) {
                    Ok(job) => job,
                    Err(_) => {
                        let _ = b.runner.recover_generation_without_worker();
                        eprintln!("Proposal generation retains pending state for recovery");
                        None
                    }
                }
            };
            let Some(job) = job else { continue };
            // Credentials and network never hold the Runner lock.
            let config = crate::application::credential(&job.settings.api_key_env).map(|api_key| {
                chat::ChatConfig {
                    base_url: job.settings.base_url.clone(),
                    model: job.settings.model.clone(),
                    api_key,
                    idle_timeout: Duration::from_secs(30),
                }
            });
            let shared = &backend;
            let retained = &job;
            let result = match config.and_then(|c| chat::ChatClient::new(c).map_err(anyhow::Error::msg)) {
                Err(_) => Err(crate::proposal::Failure::ProviderUnavailable),
                Ok(client) => tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map(|runtime| runtime.block_on(async move {
                    let (_cancel, receiver) = tokio::sync::watch::channel(false);
                    let request = client.run_frozen(retained.body.clone(), 32 * 1024, Duration::from_secs(120), receiver);
                    tokio::pin!(request);
                    let mut poll = tokio::time::interval(Duration::from_millis(100));
                    loop {
                        tokio::select! {
                            biased;
                            _ = poll.tick() => {
                                let current = shared.lock().ok().and_then(|mut b| {
                                    let settings = b.app.settings.chat.clone();
                                    b.runner.proposal_generation_current(&retained.id, retained.goal.as_deref(), settings.as_ref()).ok()
                                }).unwrap_or(false);
                                if !current { break Err(crate::proposal::Failure::SourceChanged); }
                            },
                            event = &mut request => break match event {
                                chat::ChatEvent::Complete { text } => Ok(text),
                                chat::ChatEvent::Error { code, .. } if code == "total_timeout" => Err(crate::proposal::Failure::ProviderTimeout),
                                chat::ChatEvent::Interrupted { .. } => Err(crate::proposal::Failure::Interrupted),
                                _ => Err(crate::proposal::Failure::ProviderFailed),
                            }
                        }
                    }
                }))
                    .unwrap_or(Err(crate::proposal::Failure::ProviderUnavailable)),
            };
            // The per-attempt runtime and client are now dropped too: hyper's
            // spawned connection tasks cannot remain suspended between block_on
            // calls. Local transport terminates before publication/next slot.
            let Ok(mut b) = backend.lock() else { return };
            let settings = b.app.settings.chat.clone();
            if b.runner
                .finish_proposal_generation(&job.id, job.goal.as_deref(), settings.as_ref(), result)
                .is_err()
            {
                let _ = b.runner.recover_generation_without_worker();
                eprintln!("Proposal result requires retained projection recovery");
            }
        }
    });
}

fn connection(mut stream: TcpStream, backend: Arc<Mutex<Backend>>) -> Result<()> {
    let mut input = BufReader::new(stream.try_clone()?);
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request)
                if request.schema == SCHEMA
                    || (request.schema == "ai-brain/workspace-v1"
                        && request.expected_workspace.is_some()) =>
            {
                let result = dispatch(&backend, request.command, request.expected_workspace);
                match result {
                    Ok(data) => {
                        json!({"schema":request.schema,"id":request.id,"ok":true,"data":data})
                    }
                    Err(e) => {
                        json!({"schema":request.schema,"id":request.id,"ok":false,"error":error_value(&e)})
                    }
                }
            }
            Ok(request) => {
                json!({"schema":SCHEMA,"id":request.id,"ok":false,"error":{"code":"unsupported_schema","message":"unsupported schema"}})
            }
            Err(_) => {
                json!({"schema":SCHEMA,"id":null,"ok":false,"error":{"code":"invalid_request","message":"invalid request"}})
            }
        };
        writeln!(stream, "{response}")?;
        stream.flush()?;
    }
}

/// Standalone cored opt-in entrypoint. Existing stdio and MCP paths are untouched.
pub fn run_cli(args: &[String]) -> Result<()> {
    let mut values = BTreeMap::new();
    let mut managed = false;
    let mut iter = args.iter();
    while let Some(key) = iter.next() {
        if key == "--managed-brain" {
            managed = true;
            continue;
        }
        ensure!(
            [
                "--brain-id",
                "--vault",
                "--operational-dir",
                "--records-dir",
                "--listen",
                "--config",
                "--connector-config"
            ]
            .contains(&key.as_str()),
            "unknown brain option {key}"
        );
        values.insert(
            key.as_str(),
            iter.next().context("option needs a value")?.as_str(),
        );
    }
    let required = |name| {
        values
            .get(name)
            .copied()
            .with_context(|| format!("{name} is required"))
    };
    let config = RunnerConfig {
        brain_id: required("--brain-id")?.into(),
        root: required("--vault")?.into(),
        operational_dir: required("--operational-dir")?.into(),
        records_dir: required("--records-dir")?.into(),
        boundary: if managed {
            okilum_core::source::WriteBoundary::Managed
        } else {
            okilum_core::source::WriteBoundary::Unmanaged
        },
    };
    let address: std::net::SocketAddr = required("--listen")?.parse()?;
    ensure!(
        address.ip().is_loopback(),
        "POC listener must be loopback-only"
    );
    let operational = config.operational_dir.clone();
    let mut runner = Runner::open(config)?;
    let (app, adapters) = match crate::settings::load(&operational) {
        Ok(Some((config, account))) => {
            match crate::settings::apply(config.clone(), &operational, &mut runner, account) {
                Ok((app, adapters, _)) => (app, adapters),
                Err(error) => {
                    let mut app = Application::unconfigured();
                    app.settings = config;
                    app.connection_states.insert(
                        "settings".into(),
                        crate::settings::state(
                            "invalid_configuration",
                            &format!("{error}; restore the saved provider target in Connections."),
                        ),
                    );
                    (app, Adapters::new())
                }
            }
        }
        Ok(None) => {
            if let Some(path) = values.get("--config") {
                let config: ApplicationConfig = serde_json::from_slice(&std::fs::read(path)?)
                    .context("invalid application config")?;
                Application::configure(config, &operational, &mut runner)?
            } else {
                (Application::unconfigured(), Adapters::new())
            }
        }
        Err(_) => {
            let mut app = Application::unconfigured();
            app.connection_states.insert(
                "settings".into(),
                crate::settings::state(
                    "invalid_configuration",
                    "Saved connector settings cannot be read. Correct them in Workspace settings.",
                ),
            );
            (app, Adapters::new())
        }
    };
    let connector = values
        .get("--connector-config")
        .map(|path| {
            crate::connector::Listener::bind(
                std::path::Path::new(path),
                &runner.workspace_identity(),
            )
        })
        .transpose()?;
    let listener = TcpListener::bind(address)?;
    println!(
        "{}",
        json!({"schema":SCHEMA,"ready":true,"listen":listener.local_addr()?.to_string(),"adapters":adapters.keys().collect::<Vec<_>>()})
    );
    std::io::stdout().flush()?;
    serve_application_with_connector(listener, runner, adapters, app, connector)
}

fn error_value(error: &anyhow::Error) -> Value {
    if let Some(send) = error.downcast_ref::<crate::discussion_send::Error>() {
        return send.value();
    }
    if let Some(picker) = error.downcast_ref::<todoist_picker::Error>() {
        return json!({"code":picker.code,"message":picker.message});
    }
    if let Some(provider) = error.downcast_ref::<crate::todoist::TodoistError>() {
        return json!({"code":"todoist_provider_error","message":provider.to_string(),"todoist":provider});
    }
    if let Some(operation) = error.downcast_ref::<crate::maestro_operations::Error>() {
        return json!({"code":format!("maestro_operation_{}", operation.disposition.status),"message":operation.message,"maestro_operation":operation.disposition});
    }
    if let Some(connector) = error.downcast_ref::<crate::connector::Error>() {
        json!({"code":connector.code,"message":connector.message})
    } else if error
        .downcast_ref::<crate::runtime::PreparedChangeNotRecorded>()
        .is_some()
    {
        json!({"code":"runtime_error","message":error.to_string(),"prepared_change_recorded":false})
    } else if let Some(attention) = error.downcast_ref::<crate::attention::AttentionError>() {
        let mut value = json!({"code":attention.code,"message":attention.message});
        if let Some(current) = &attention.current {
            value["current"] = current.clone();
        }
        value
    } else if let Some(inbox) = error.downcast_ref::<crate::inbox::InboxError>() {
        json!({"code":inbox.code,"message":inbox.message})
    } else if let Some(retrieval) = error.downcast_ref::<crate::retrieval::RetrievalError>() {
        json!({"code":retrieval.code,"message":retrieval.message})
    } else if let Some(source) = error.downcast_ref::<okilum_core::source::SourceError>() {
        json!({"code":source.code,"message":source.message,"conflict":source.conflict})
    } else {
        json!({"code":"runtime_error","message":error.to_string()})
    }
}

#[cfg(test)]
#[path = "retrieval_service_tests.rs"]
mod retrieval_service_tests;

#[path = "connector_service.rs"]
mod connector_service;

#[cfg(test)]
mod maestro_network_tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn maestro_wire_contract_accepts_explicit_ownership_and_rejects_extra_link_fields() {
        let request = json!({
            "schema": SCHEMA, "id": "request-1", "expected_workspace": {},
            "op": "maestro_link",
            "operation_id": "01000000-0000-4000-8000-000000000001",
            "goal_id": "01000000-0000-4000-8000-000000000002",
            "project_id": "01000000-0000-4000-8000-000000000003",
            "project_name": "example", "repo": "example/project",
            "issue_number": 42, "selection_guard": "sha256:selection",
        });
        let decoded = serde_json::from_value::<Request>(request.clone()).unwrap();
        assert!(matches!(decoded.command, Command::MaestroLink { .. }));
        let mut extra = request.clone();
        extra["start_worker"] = json!(true);
        assert!(serde_json::from_value::<Request>(extra).is_err());
        let mut missing = request;
        missing.as_object_mut().unwrap().remove("goal_id");
        assert!(serde_json::from_value::<Request>(missing).is_err());
        let unlink = json!({
            "schema": SCHEMA, "id": "request-2", "expected_workspace": {},
            "op": "maestro_unlink",
            "operation_id": "01000000-0000-4000-8000-000000000004",
            "goal_id": "01000000-0000-4000-8000-000000000002",
            "expected_link_id": "01000000-0000-4000-8000-000000000005",
        });
        assert!(matches!(
            serde_json::from_value::<Request>(unlink).unwrap().command,
            Command::MaestroUnlink { .. }
        ));
    }
    #[test]
    fn maestro_operation_wire_contract_is_exact_and_rejects_unknown_fields() {
        let body = json!({"operation_id":"01000000-0000-4000-8000-000000000001", "goal_id":"01000000-0000-4000-8000-000000000002", "expected_link_id":"01000000-0000-4000-8000-000000000003"});
        let wire = json!({"schema":SCHEMA,"id":"abandon","op":"maestro_operation_abandon","kind":"unlink","request":body});
        let decoded = serde_json::from_value::<Request>(wire.clone()).unwrap();
        match decoded.command {
            Command::MaestroOperationAbandon { request } => assert_eq!(request.body(), body),
            _ => panic!("wrong command"),
        }
        for extra_inside in [false, true] {
            let mut extra = wire.clone();
            if extra_inside {
                extra["request"]["cancel_remote"] = json!(true);
            } else {
                extra["cancel_remote"] = json!(true);
            }
            assert!(serde_json::from_value::<Request>(extra).is_err());
        }
        let mut lookup = json!({"schema":SCHEMA,"id":"get","op":"maestro_operation_get","operation_id":body["operation_id"],"goal_id":body["goal_id"]});
        assert!(serde_json::from_value::<Request>(lookup.clone()).is_ok());
        lookup["kind"] = json!("unlink");
        assert!(serde_json::from_value::<Request>(lookup).is_err());
    }
    #[test]
    fn delayed_link_get_cannot_resurrect_work_after_concurrent_link_and_unlink() {
        held_link_get("history");
    }
    #[test]
    fn held_link_get_is_pending_then_abandoned_before_response() {
        held_link_get("abandon");
    }
    #[test]
    fn failed_link_get_stays_pending_and_query_abandon_work_offline() {
        held_link_get("failure");
    }
    #[test]
    fn held_link_get_configuration_refusal_is_durably_rejected() {
        held_link_get("config");
    }
    fn held_link_get(mode: &'static str) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        std::fs::create_dir(temp.path().join("runtime")).unwrap();
        let mut runner = Runner::open(RunnerConfig {
            brain_id: uuid::Uuid::new_v4().to_string(),
            root: temp.path().join("brain"),
            operational_dir: temp.path().join("runtime"),
            records_dir: "records".into(),
            boundary: okilum_core::source::WriteBoundary::Managed,
        })
        .unwrap();
        let goal = uuid::Uuid::new_v4().to_string();
        runner
            .create_goal(
                Goal {
                    id: goal.clone(),
                    title: "Delayed link fixture".into(),
                    status: "active".into(),
                    criteria: vec![Criterion {
                        id: "C1".into(),
                        description: "Evidence remains unverified".into(),
                        requires_human: false,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: BTreeMap::new(),
                },
                "# Fixture\n".into(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config = crate::maestro::tests::settings(&origin);
        let mut discovery = crate::maestro::tests::discovery();
        discovery.instance = config.identity();
        let project = &discovery.projects[0];
        let mut request = crate::maestro_links::LinkRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            goal_id: goal.clone(),
            selection_guard: crate::maestro::selection_guard(
                &discovery.instance,
                project,
                &project.issues[0],
            ),
            project_id: project.project_id.clone(),
            project_name: project.name.clone(),
            repo: project.repo.clone(),
            issue_number: 42,
        };
        let mut app = Application::unconfigured();
        app.settings.maestro = Some(config);
        let backend = Arc::new(Mutex::new(Backend {
            runner,
            adapters: Adapters::new(),
            app,
            exports: crate::export::ExportDownloads::default(),
            todoist_picker: Default::default(),
            index: None,
            index_error: None,
            context_jobs: crate::context_jobs::Jobs::open(&temp.path().join("runtime")).unwrap(),
        }));
        let (requested, wait_request) = std::sync::mpsc::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "GET /api/v1/fleet HTTP/1.1\r\n");
            requested.send(()).unwrap();
            wait_release.recv_timeout(Duration::from_secs(3)).unwrap();
            let mut body = crate::maestro::tests::raw();
            body["refreshed_at"] = json!(time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap());
            let body = serde_json::to_vec(&body).unwrap();
            let status = if mode == "failure" {
                "503 Unavailable"
            } else {
                "200 OK"
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let invoked = backend.clone();
        let delayed = request.clone();
        let operation = crate::maestro_operations::Request::Link(request.clone());
        let in_flight = std::thread::spawn(move || {
            dispatch(&invoked, Command::MaestroLink { request: delayed }, None)
        });
        wait_request.recv_timeout(Duration::from_secs(3)).unwrap();
        {
            let mut owner = backend
                .try_lock()
                .expect("held GET must not own backend mutex");
            let pending = owner
                .runner
                .maestro_operation_get(&goal, operation.operation_id())
                .unwrap();
            assert_eq!(pending.status, "pending");
            assert_eq!(pending.request, Some(operation.body()));
            if mode == "abandon" {
                let abandoned = owner
                    .handle(Command::MaestroOperationAbandon {
                        request: operation.clone(),
                    })
                    .unwrap();
                assert_eq!(abandoned["status"], "rejected");
                assert_eq!(abandoned["rejection"]["code"], "abandoned");
            } else if mode == "config" {
                owner.app.settings.maestro = None;
            } else if mode == "history" {
                request.operation_id = uuid::Uuid::new_v4().to_string();
                let receipt = owner.runner.maestro_link(request, &discovery).unwrap();
                owner
                    .runner
                    .maestro_unlink(crate::maestro_links::UnlinkRequest {
                        operation_id: uuid::Uuid::new_v4().to_string(),
                        goal_id: goal.clone(),
                        expected_link_id: receipt["link_id"].as_str().unwrap().into(),
                    })
                    .unwrap();
            }
        }
        release.send(()).unwrap();
        let error = in_flight.join().unwrap().unwrap_err();
        let value = error_value(&error);
        assert_eq!(
            value["maestro_operation"]["status"],
            if mode == "failure" {
                "pending"
            } else {
                "rejected"
            }
        );
        if mode != "failure" {
            assert_eq!(
                value["maestro_operation"]["rejection"]["code"],
                match mode {
                    "abandon" => "abandoned",
                    "config" => "configuration_changed",
                    _ => "link_history_changed",
                }
            );
        }
        server.join().unwrap();
        let mut owner = backend.lock().unwrap();
        let retained = owner.runner.maestro_link_view(&goal).unwrap();
        assert!(retained["link"].is_null());
        assert_eq!(
            retained["history"].as_array().unwrap().len(),
            usize::from(mode == "history")
        );
        // Provider listener is gone; neither query nor abandonment needs it.
        owner.app.settings.maestro = None;
        let disposition = owner
            .handle(Command::MaestroOperationGet {
                request: crate::maestro_operations::Lookup {
                    goal_id: goal,
                    operation_id: operation.operation_id().into(),
                },
            })
            .unwrap();
        assert_eq!(disposition, value["maestro_operation"]);
        let abandoned = owner
            .handle(Command::MaestroOperationAbandon {
                request: operation.clone(),
            })
            .unwrap();
        assert_eq!(abandoned["status"], "rejected");
        if mode == "failure" {
            assert_eq!(abandoned["rejection"]["code"], "abandoned");
        }
        let retry = owner
            .runner
            .maestro_link(
                match operation {
                    crate::maestro_operations::Request::Link(r) => r,
                    _ => unreachable!(),
                },
                &discovery,
            )
            .unwrap_err();
        assert_eq!(error_value(&retry)["maestro_operation"], abandoned);
    }
    #[test]
    fn maestro_get_wait_does_not_hold_owner_and_late_config_response_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        std::fs::create_dir(temp.path().join("runtime")).unwrap();
        let runner = Runner::open(RunnerConfig {
            brain_id: uuid::Uuid::new_v4().to_string(),
            root: temp.path().join("brain"),
            operational_dir: temp.path().join("runtime"),
            records_dir: "records".into(),
            boundary: okilum_core::source::WriteBoundary::Managed,
        })
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut app = Application::unconfigured();
        app.settings.maestro = Some(crate::maestro::tests::settings(&origin));
        let backend = Arc::new(Mutex::new(Backend {
            runner,
            adapters: Adapters::new(),
            app,
            exports: crate::export::ExportDownloads::default(),
            todoist_picker: Default::default(),
            index: None,
            index_error: None,
            context_jobs: crate::context_jobs::Jobs::open(&temp.path().join("runtime")).unwrap(),
        }));
        let (requested, wait_request) = std::sync::mpsc::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "GET /api/v1/fleet HTTP/1.1\r\n");
            requested.send(()).unwrap();
            wait_release.recv_timeout(Duration::from_secs(3)).unwrap();
            let mut body = crate::maestro::tests::raw();
            body["refreshed_at"] = json!(time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap());
            let body = serde_json::to_vec(&body).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let invoked = backend.clone();
        let request =
            std::thread::spawn(move || dispatch(&invoked, Command::MaestroDiscover, None));
        wait_request.recv_timeout(Duration::from_secs(3)).unwrap();
        {
            let mut owner = backend
                .try_lock()
                .expect("Network request must not retain backend owner lock");
            owner.app.settings.maestro = None;
        }
        release.send(()).unwrap();
        assert!(request
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("configuration changed"));
        server.join().unwrap();
    }
}

#[cfg(test)]
mod proposal_wire_tests {
    use super::*;
    #[test]
    fn proposal_adopt_wire_requires_exact_guard_and_typed_destination() {
        let source = crate::inbox::SourceIdentity {
            channel: "native".into(),
            instance_id: uuid::Uuid::new_v4().to_string(),
            account_id: "local".into(),
            actor_id: "operator".into(),
            chat_id: None,
            topic_id: None,
            message_id: uuid::Uuid::new_v4().to_string(),
            update_id: uuid::Uuid::new_v4().to_string(),
            uri: None,
        };
        let request = crate::proposal::AdoptRequest::Inbox(Box::new(
            crate::proposals::InboxAdoptionRequest {
                operation_id: source.message_id.clone(),
                proposal_id: "a".repeat(64),
                expected_revision: format!("sha256:{}", "b".repeat(64)),
                capture_id: uuid::Uuid::new_v4().to_string(),
                expected_capture_revision: format!("sha256:{}", "c".repeat(64)),
                title: "Edited proposal".into(),
                criteria: vec![],
                source,
            },
        ));
        let mut wire = serde_json::to_value(&request).unwrap();
        wire.as_object_mut().unwrap().extend(
            json!({
                "schema":"ai-brain/workspace-v1", "id":"adopt",
                "expected_workspace":{}, "op":"proposal_adopt"
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let parsed = serde_json::from_value::<Request>(wire.clone()).unwrap();
        assert!(
            matches!(parsed.command, Command::ProposalAdopt { request: actual } if actual == request)
        );
        for (field, value) in [
            ("unknown", json!(true)),
            ("destination", json!("arbitrary")),
            ("schema", json!(SCHEMA)),
            ("expected_workspace", Value::Null),
        ] {
            let mut invalid = wire.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }
    }
    #[test]
    fn proposal_retry_wire_accepts_exact_request_without_disposition() {
        let source = crate::inbox::SourceIdentity {
            channel: "native".into(),
            instance_id: uuid::Uuid::new_v4().to_string(),
            account_id: "local".into(),
            actor_id: "operator".into(),
            chat_id: None,
            topic_id: None,
            message_id: uuid::Uuid::new_v4().to_string(),
            update_id: uuid::Uuid::new_v4().to_string(),
            uri: None,
        };
        let request = crate::proposal::RetryRequest {
            operation_id: source.message_id.clone(),
            proposal_id: "a".repeat(64),
            goal_id: None,
            expected_revision: format!("sha256:{}", "b".repeat(64)),
            source,
        };
        let mut wire = serde_json::to_value(&request).unwrap();
        wire.as_object_mut().unwrap().extend(json!({"schema":"ai-brain/workspace-v1", "id":"retry", "expected_workspace":{}, "op":"proposal_retry"}).as_object().unwrap().clone());
        let parsed = serde_json::from_value::<Request>(wire.clone()).unwrap();
        assert!(
            matches!(parsed.command, Command::ProposalRetry { request: actual } if actual == request)
        );
        wire["disposition"] = "rejected".into();
        assert!(serde_json::from_value::<Request>(wire).is_err());
    }
    #[test]
    fn proposal_reads_require_guard_and_reject_unknown_fields_or_actions() {
        for (op, fields) in [
            (
                "proposal_list",
                json!({"goal_id":null,"limit":10,"cursor":null}),
            ),
            (
                "proposal_get",
                json!({"proposal_id":"a".repeat(64),"goal_id":null}),
            ),
        ] {
            let mut v = json!({"schema":"ai-brain/workspace-v1","id":"request","expected_workspace":{},"op":op});
            v.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            assert!(serde_json::from_value::<Request>(v.clone()).is_ok());
            let mut wrong = v.clone();
            wrong["unknown"] = true.into();
            assert!(serde_json::from_value::<Request>(wrong).is_err());
            let mut wrong = v.clone();
            wrong["schema"] = SCHEMA.into();
            assert!(serde_json::from_value::<Request>(wrong).is_err());
            v.as_object_mut().unwrap().remove("expected_workspace");
            assert!(serde_json::from_value::<Request>(v).is_err());
        }
        for op in ["proposal_retry", "proposal_adopt", "proposal_generate"] {
            assert!(serde_json::from_value::<Request>(json!({"schema":"ai-brain/workspace-v1","id":"request","expected_workspace":{},"op":op})).is_err());
        }
    }
}

#[cfg(test)]
#[path = "suggestions_service_tests.rs"]
mod suggestions_service_tests;

#[path = "todoist_picker.rs"]
mod todoist_picker;

#[cfg(test)]
mod maestro_control_network_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    fn setup(
        origin: &str,
    ) -> (
        tempfile::TempDir,
        Arc<Mutex<Backend>>,
        RunnerConfig,
        crate::maestro_control::Request,
    ) {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("brain/records")).unwrap();
        std::fs::create_dir(t.path().join("runtime")).unwrap();
        let config = RunnerConfig {
            brain_id: uuid::Uuid::new_v4().to_string(),
            root: t.path().join("brain"),
            operational_dir: t.path().join("runtime"),
            records_dir: "records".into(),
            boundary: okilum_core::source::WriteBoundary::Managed,
        };
        let mut runner = Runner::open(RunnerConfig {
            brain_id: config.brain_id.clone(),
            root: config.root.clone(),
            operational_dir: config.operational_dir.clone(),
            records_dir: config.records_dir.clone(),
            boundary: config.boundary,
        })
        .unwrap();
        let goal = uuid::Uuid::new_v4().to_string();
        runner
            .create_goal(
                Goal {
                    id: goal.clone(),
                    title: "Control fixture".into(),
                    status: "active".into(),
                    criteria: vec![Criterion {
                        id: "C1".into(),
                        description: "Human acceptance".into(),
                        requires_human: true,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: BTreeMap::new(),
                },
                "# Fixture".into(),
            )
            .unwrap();
        let settings = crate::maestro::tests::settings(origin);
        let mut d = crate::maestro::tests::discovery();
        d.instance = settings.identity();
        let p = &d.projects[0];
        let link = runner
            .maestro_link(
                crate::maestro_links::LinkRequest {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    goal_id: goal.clone(),
                    selection_guard: crate::maestro::selection_guard(&d.instance, p, &p.issues[0]),
                    project_id: p.project_id.clone(),
                    project_name: p.name.clone(),
                    repo: p.repo.clone(),
                    issue_number: 42,
                },
                &d,
            )
            .unwrap();
        let request=serde_json::from_value(json!({"operation_id":uuid::Uuid::new_v4().to_string(),"goal_id":goal,"expected_link_id":link["link_id"],"instance":settings.identity(),"review":{"expected":{"version":"v1","project_id":p.project_id,"project_name":p.name,"project_repo":p.repo,"approval_id":"approval-1","created_at":"2026-09-08T06:00:00Z","decision_revision":"v1:exact"},"decision_id":"decision-1","action":"merge_pr","target":{"issue":42,"pr":9,"head_sha":"a".repeat(40)},"summary":"Merge exact head","risk":"low","evidence":[]},"decision":"approved","actor":"operator","reason":"Exact review"})).unwrap();
        let mut app = Application::unconfigured();
        app.settings.maestro = Some(settings);
        let backend = Arc::new(Mutex::new(Backend {
            runner,
            app,
            adapters: Adapters::new(),
            exports: Default::default(),
            todoist_picker: Default::default(),
            index: None,
            index_error: None,
            context_jobs: crate::context_jobs::Jobs::open(&config.operational_dir).unwrap(),
        }));
        (t, backend, config, request)
    }
    fn read(stream: &std::net::TcpStream) -> (String, Value) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let route = line.clone();
        let mut len = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    len = value.trim().parse().unwrap();
                }
            }
        }
        let mut bytes = vec![0; len];
        reader.read_exact(&mut bytes).unwrap();
        (route, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
    fn fleet(request: &crate::maestro_control::Request, supported: bool, committed: bool) -> Value {
        let mut raw = crate::maestro::tests::raw();
        raw["projects"][0]["guarded_approvals"] = json!({"version":"v1","supported":supported,"actions":if supported {vec!["merge_pr"]}else{vec![]}});
        raw["approvals"] = json!([{"id":request.review.expected.approval_id,"project_name":request.review.expected.project_name,"project_repo":request.review.expected.project_repo,"action":"merge_pr","status":if committed {"execution_failed"}else{"pending"},"guarded_review":request.review,"decision_receipt":if committed {json!({"expected":request.review.expected,"decision":"approved","actor":"original-provider-actor","reason":"Original provider reason","at":"2026-09-08T06:01:00Z"})}else{Value::Null}}]);
        raw
    }
    fn respond(mut stream: std::net::TcpStream, value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        )
        .unwrap();
        stream.write_all(&bytes).unwrap();
    }
    #[test]
    fn lost_decision_reply_restart_unlink_and_disabled_capability_reconcile_only_get() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let (_t, backend, config, request) = setup(&origin);
        let copy = request.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let (route, _) = read(&stream);
            assert!(route.starts_with("GET /api/v1/fleet "));
            respond(stream, fleet(&copy, true, false));
            let (stream, _) = listener.accept().unwrap();
            let (route, body) = read(&stream);
            assert!(route
                .starts_with("POST /api/v1/fleet/approvals/approval-1/approve?project=fixture "));
            assert_eq!(body, copy.provider_body());
            drop(stream);
            let (stream, _) = listener.accept().unwrap();
            let (route, _) = read(&stream);
            assert!(route.starts_with("GET /api/v1/fleet "));
            respond(stream, fleet(&copy, false, true));
            1usize
        });
        let error = dispatch(
            &backend,
            Command::MaestroApprovalDecision {
                request: request.clone(),
            },
            None,
        )
        .unwrap_err();
        let d = error
            .downcast_ref::<crate::maestro_operations::Error>()
            .unwrap();
        assert_eq!(d.disposition.status, "pending");
        let app = {
            let mut owner = backend.lock().unwrap();
            owner
                .runner
                .maestro_unlink(crate::maestro_links::UnlinkRequest {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    goal_id: request.goal_id.clone(),
                    expected_link_id: request.expected_link_id.clone(),
                })
                .unwrap();
            owner.app.settings.maestro.clone().unwrap()
        };
        drop(backend);
        let runner = Runner::open(config).unwrap();
        let mut application = Application::unconfigured();
        application.settings.maestro = Some(app);
        let dir = _t.path().join("runtime");
        let backend = Arc::new(Mutex::new(Backend {
            runner,
            app: application,
            adapters: Adapters::new(),
            exports: Default::default(),
            todoist_picker: Default::default(),
            index: None,
            index_error: None,
            context_jobs: crate::context_jobs::Jobs::open(&dir).unwrap(),
        }));
        let result = dispatch(
            &backend,
            Command::MaestroApprovalReconcile {
                request: crate::maestro_operations::Lookup {
                    goal_id: request.goal_id.clone(),
                    operation_id: request.operation_id.clone(),
                },
            },
            None,
        )
        .unwrap();
        assert!(request.matches_local_receipt(&result));
        assert_eq!(
            result["decision_receipt"]["actor"],
            "original-provider-actor"
        );
        assert_eq!(result["execution_status"], "execution_failed");
        assert_eq!(server.join().unwrap(), 1);
        let replay = dispatch(
            &backend,
            Command::MaestroApprovalDecision {
                request: request.clone(),
            },
            None,
        )
        .unwrap();
        assert_eq!(replay, result);
    }
    #[test]
    fn explicit_request_after_capability_disappears_is_never_sent_tombstone() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let (_t, backend, config, request) = setup(&origin);
        let copy = request.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            assert!(read(&stream).0.starts_with("GET /api/v1/fleet "));
            respond(stream, fleet(&copy, false, false));
        });
        assert!(dispatch(
            &backend,
            Command::MaestroApprovalDecision {
                request: request.clone()
            },
            None
        )
        .is_err());
        server.join().unwrap();
        assert_eq!(
            backend
                .lock()
                .unwrap()
                .runner
                .maestro_operation_get(&request.goal_id, &request.operation_id)
                .unwrap()
                .status,
            "rejected"
        );
        let marker: Value = serde_json::from_slice(
            &std::fs::read(config.operational_dir.join("maestro-links-enrollment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marker[crate::maestro_control::ENROLLMENT_FIELD], 1);
        assert!(backend
            .lock()
            .unwrap()
            .runner
            .maestro_operation_begin(&crate::maestro_operations::Request::ApprovalDecision(
                Box::new(request)
            ))
            .is_err());
    }
    #[test]
    fn actual_go_handler_fixtures_compose_with_guarded_client() {
        let pending: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/maestro-control-go-v1/fleet-pending.json"
        ))
        .unwrap();
        let approved: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/maestro-control-go-v1/approve-response.json"
        ))
        .unwrap();
        let replay: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/maestro-control-go-v1/approve-replay-response.json"
        ))
        .unwrap();
        let readonly: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/maestro-control-go-v1/fleet-receipt-readonly.json"
        ))
        .unwrap();
        let review: crate::maestro_control::Review =
            serde_json::from_value(pending["approvals"][0]["guarded_review"].clone()).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let settings = crate::maestro::tests::settings(&origin);
        let request = crate::maestro_control::Request {
            operation_id: uuid::Uuid::new_v4().to_string(),
            goal_id: uuid::Uuid::new_v4().to_string(),
            expected_link_id: uuid::Uuid::new_v4().to_string(),
            instance: settings.identity(),
            review,
            decision: crate::maestro_control::Decision::Approved,
            actor: "another-client".into(),
            reason: "Same semantic decision".into(),
        };
        let copy = request.clone();
        let original = approved.clone();
        let server = std::thread::spawn(move || {
            for (expected_method, body) in [("GET", pending), ("POST", approved), ("GET", readonly)]
            {
                let (stream, _) = listener.accept().unwrap();
                let (route, input) = read(&stream);
                assert!(route.starts_with(expected_method));
                if expected_method == "POST" {
                    assert_eq!(input, copy.provider_body());
                }
                respond(stream, body);
            }
        });
        let client = crate::maestro::Client::new(settings).unwrap();
        let view = client.guarded_view(&request.review.expected).unwrap();
        assert!(view.supported);
        assert_eq!(view.review.as_ref(), Some(&request.review));
        let response = client.guarded_decide(&request).unwrap();
        assert!(response.receipt.matches(&request));
        assert_eq!(response.receipt.reason, "original reason");
        assert_eq!(
            serde_json::to_value(&response.receipt).unwrap(),
            original["receipt"]
        );
        assert_eq!(replay["receipt"], original["receipt"]);
        let view = client.guarded_view(&request.review.expected).unwrap();
        assert!(!view.supported);
        assert_eq!(view.status, "execution_skipped");
        assert!(view.decision_receipt.unwrap().matches(&request));
        server.join().unwrap();
    }
    #[test]
    fn missing_capability_review_alone_does_not_enroll() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let (_t, backend, config, request) = setup(&origin);
        let copy = request.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            assert!(read(&stream).0.starts_with("GET"));
            respond(stream, fleet(&copy, false, false));
        });
        let result = dispatch(
            &backend,
            Command::MaestroApprovalReview {
                request: crate::maestro_control::ReviewRequest {
                    goal_id: request.goal_id,
                    expected_link_id: request.expected_link_id,
                    approval_id: request.review.expected.approval_id,
                },
            },
            None,
        )
        .unwrap();
        assert_eq!(result["view"]["supported"], false);
        server.join().unwrap();
        let marker: Value = serde_json::from_slice(
            &std::fs::read(config.operational_dir.join("maestro-links-enrollment.json")).unwrap(),
        )
        .unwrap();
        assert!(marker
            .get(crate::maestro_control::ENROLLMENT_FIELD)
            .is_none());
    }
}

#[cfg(test)]
#[path = "discussion_send_service_tests.rs"]
mod discussion_send_service_tests;

#[cfg(test)]
mod criteria_wire_tests {
    use super::*;
    #[test]
    fn criteria_commands_require_workspace_and_exact_outer_shape() {
        let write = json!({"schema":"ai-brain/v1","operation_id":"02000000-0000-4000-8000-000000000008","brain_id":"02000000-0000-4000-8000-000000000001","path":"records/goal.md","expected_revision":"sha256:base","content_base64":""});
        let base = json!({"schema":"ai-brain/v1","brain_id":write["brain_id"],"path":write["path"],"revision":"sha256:base","content_base64":"","media_type":"text/markdown"});
        for op in ["goal_criteria_get", "goal_criteria_write"] {
            let mut wire = json!({"schema":"ai-brain/workspace-v1","id":"transport","expected_workspace":{},"op":op,"goal_id":"02000000-0000-4000-8000-000000000002"});
            if op == "goal_criteria_write" {
                wire["request"] = write.clone();
                wire["base"] = base.clone();
            }
            assert!(serde_json::from_value::<Request>(wire.clone()).is_ok());
            let mut invalid = wire.clone();
            invalid["schema"] = json!("ai-brain/v1");
            assert!(serde_json::from_value::<Request>(invalid).is_err());
            let mut invalid = wire.clone();
            invalid
                .as_object_mut()
                .unwrap()
                .remove("expected_workspace");
            assert!(serde_json::from_value::<Request>(invalid).is_err());
            wire["unknown"] = json!(true);
            assert!(serde_json::from_value::<Request>(wire).is_err());
        }
    }
    #[test]
    fn decision_commands_require_exact_workspace_create_only_shape() {
        for op in ["discussion_decision_get", "discussion_decision_save"] {
            let mut wire = json!({"schema":"ai-brain/workspace-v1","id":"transport","expected_workspace":{},"op":op,"goal_id":"goal","conversation_id":"conversation","turn_id":"turn","expected_actor_id":"actor"});
            if op == "discussion_decision_save" {
                wire["request"] = json!({"schema":"ai-brain/v1","operation_id":"op","brain_id":"brain","path":"target","expected_revision":null,"content_base64":""});
            }
            assert!(serde_json::from_value::<Request>(wire.clone()).is_ok());
            let mut bad = wire.clone();
            bad["schema"] = json!("ai-brain/v1");
            assert!(serde_json::from_value::<Request>(bad).is_err());
            let mut bad = wire.clone();
            bad["base"] = json!({});
            assert!(serde_json::from_value::<Request>(bad).is_err());
            if op == "discussion_decision_save" {
                let mut bad = wire.clone();
                bad["request"]
                    .as_object_mut()
                    .unwrap()
                    .remove("expected_revision");
                assert!(serde_json::from_value::<Request>(bad).is_err());
                let mut bad = wire.clone();
                bad["request"]["unknown"] = json!(true);
                assert!(serde_json::from_value::<Request>(bad).is_err());
                wire["request"]["expected_revision"] = json!("sha256:existing");
                assert!(serde_json::from_value::<Request>(wire).is_err());
            }
        }
    }
}

#[cfg(test)]
#[path = "t3_target_service_tests.rs"]
mod t3_target_tests;
