//! Opt-in AI Brain POC client. The backend owns work and persistence; closing
//! this view drops only its local TCP connection, never an engine operation.
pub(crate) mod app_quit;
mod attention_ui;
mod context_ui;
mod discussion_context;
mod discussion_decision_reuse_ui;
mod discussion_decision_ui;
mod discussion_note;
mod discussion_note_ui;
mod discussion_send_outbox;
mod discussion_send_ui;
pub(crate) use discussion_send_ui::OpenDiscussionOwner;
mod editor_recovery;
mod editor_ui;
mod goal_criteria_ui;
mod inbox_plan_ui;
mod inbox_ui;
mod incoming_references_ui;
mod maestro_ui;
mod merge_preview;
mod native_outbox;
mod new_conversation;
mod note_link;
mod note_link_ui;
mod open_link_ui;
mod proposal_adoption_outbox;
mod proposal_adoption_ui;
mod proposal_form_state;
mod proposal_outbox;
mod proposal_retry_outbox;
mod proposal_ui;
mod source_context;
mod source_find;
mod source_input;
mod source_navigation;
mod source_projection;
mod source_projection_ui;
pub(crate) mod source_trace;
pub(crate) mod suggestions_outbox;
pub(crate) mod t3_route_outbox;
mod todoist_picker;

use base64::{engine::general_purpose::STANDARD, Engine};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    text::{SelectionFormat, TextView},
    v_flex, ActiveTheme as _, Disableable as _, Sizable as _,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

const SCHEMA: &str = "ai-brain/v1";
const MAX_REPLY: u64 = 16 * 1024 * 1024;
const MAX_PREVIEW_REPLY: u64 = 128 * 1024 * 1024;

/// One request, one connection, no retry. Mutations have backend-persisted IDs;
/// a lost reply is recovered through reads/reconciliation, never re-created.
pub(crate) fn rpc(endpoint: SocketAddr, body: Value) -> Result<Value, String> {
    rpc_guarded(endpoint, body, None)
}

pub(crate) fn rpc_guarded(
    endpoint: SocketAddr,
    mut body: Value,
    workspace: Option<&Value>,
) -> Result<Value, String> {
    if let Some(workspace) = workspace {
        body["expected_workspace"] = workspace.clone();
    }
    if !endpoint.ip().is_loopback() {
        return Err("The AI Brain backend must use a local connection.".into());
    }
    let max_reply = if matches!(body["op"].as_str(), Some("source_preview" | "chat_get")) {
        MAX_PREVIEW_REPLY
    } else {
        MAX_REPLY
    };
    let id = Uuid::new_v4().to_string();
    let schema = if workspace.is_some() {
        "ai-brain/workspace-v1"
    } else {
        SCHEMA
    };
    body["schema"] = json!(schema);
    body["id"] = json!(id);
    let mut stream =
        TcpStream::connect_timeout(&endpoint, Duration::from_secs(3)).map_err(|_| {
            "Cannot connect to the AI Brain backend. Check that it is running.".to_string()
        })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(45)))
        .map_err(|_| "Cannot configure the backend connection.".to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "Cannot configure the backend connection.".to_string())?;
    let request =
        serde_json::to_vec(&body).map_err(|_| "Cannot prepare this request.".to_string())?;
    stream.write_all(&request).and_then(|_| stream.write_all(b"\n"))
        .map_err(|_| "Delivery is uncertain. Refresh to recover the saved state; the request was not retried.".to_string())?;
    let mut bytes = Vec::new();
    let reader = BufReader::new(stream);
    use std::io::Read as _;
    reader.take(max_reply + 1).read_until(b'\n', &mut bytes)
        .map_err(|_| "The connection ended before acknowledgement. Refresh to recover saved work; nothing was retried.".to_string())?;
    if bytes.len() as u64 > max_reply || bytes.last() != Some(&b'\n') {
        return Err("Incomplete backend reply. Refresh to recover saved work.".into());
    }
    let reply: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Unsupported backend reply.".to_string())?;
    if reply["schema"] != schema || reply["id"] != id {
        return Err("Backend reply does not match this request.".into());
    }
    if reply["ok"] != true {
        if matches!(body["op"].as_str(), Some("chat_send" | "chat_send_get")) {
            return Ok(json!({"_discussion_send_error": reply["error"]}));
        }
        // Only a matching server rejection establishes that a context-limit
        // refusal preceded persistence/dispatch. Transport text is never proof.
        if body["op"] == "chat_start"
            && reply["error"]["code"] == "runtime_error"
            && reply["error"]["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("chat_context_limit:"))
        {
            return Ok(json!({"_chat_context_refused": reply["error"]["message"]}));
        }
        if body["op"] == "todoist_inbox_list"
            || (body["op"] == "task_link" && body["picker_session_id"].is_string())
        {
            return Ok(json!({"_todoist_picker_error":reply["error"]}));
        }
        if text(&body["op"]).starts_with("proposal_") {
            return Ok(json!({"_proposal_error":reply["error"]}));
        }
        if text(&body["op"]).starts_with("maestro_") {
            return Ok(json!({"_maestro_error":reply["error"]}));
        }
        if body["op"] == "inbox_plan" {
            return Ok(json!({"_inbox_plan_error":reply["error"]}));
        }
        if text(&body["op"]).starts_with("attention_") {
            return Ok(json!({"_attention_error":reply["error"]}));
        }
        let message = reply["error"]["message"]
            .as_str()
            .unwrap_or("The backend rejected this action.");
        if matches!(body["op"].as_str(), Some("stage_revise" | "stage_discard"))
            && reply["error"]["prepared_change_recorded"] == false
        {
            // The owner checked durable operation history under its lock. Only
            // this explicit marker proves the change was never recorded.
            return Ok(json!({"prepared_change_rejected": {
                "goal_id":body["goal_id"], "operation_id":body["operation_id"], "message":message
            }}));
        }
        if reply["error"]["code"] == "conflict"
            || reply["error"].get("conflict").is_some_and(|v| !v.is_null())
        {
            if body["op"] == "discussion_decision_reuse_write"
                && reply["error"]["conflict"]["conflict_id"].is_string()
            {
                let conflict = rpc_guarded(
                    endpoint,
                    json!({"op":"discussion_decision_reuse_get","goal_id":body["goal_id"],"decision_id":body["decision_id"],"operation_id":reply["error"]["conflict"]["conflict_id"]}),
                    workspace,
                )?;
                return Ok(json!({"source_conflict":conflict}));
            }
            if matches!(
                body["op"].as_str(),
                Some("source_write" | "goal_criteria_write")
            ) && reply["error"]["conflict"]["conflict_id"].is_string()
            {
                // The write failed. Fetch preserved versions, never retry it or
                // flatten its structured identity into a disposable error string.
                let conflict = rpc_guarded(
                    endpoint,
                    json!({"op":"source_conflict",
                    "brain_id":body["request"]["brain_id"], "path":body["request"]["path"],
                    "conflict_id":reply["error"]["conflict"]["conflict_id"]}),
                    workspace,
                )?;
                return Ok(json!({"source_conflict":conflict}));
            }
            return Err(format!("{message} Your current draft is preserved."));
        }
        return Err(message.to_string());
    }
    Ok(reply["data"].clone())
}

fn decode_source(snapshot: &Value) -> Result<String, &'static str> {
    if snapshot["schema"] != SCHEMA
        || snapshot["path"].as_str().is_none()
        || snapshot["revision"].as_str().is_none()
        || snapshot["brain_id"].as_str().is_none()
    {
        return Err("Invalid source snapshot; editing is unavailable.");
    }
    let encoded = snapshot["content_base64"]
        .as_str()
        .ok_or("Invalid source bytes.")?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid source bytes.")?;
    String::from_utf8(bytes).map_err(|_| {
        "This note is not UTF-8. Its original bytes are preserved; text editing is unavailable."
    })
}
fn source_write(snapshot: &Value, edited: &str, operation: &str) -> Value {
    json!({"op":"source_write","request":{"schema":SCHEMA,"operation_id":operation,
        "brain_id":snapshot["brain_id"],"path":snapshot["path"],"expected_revision":snapshot["revision"],
        "content_base64":STANDARD.encode(edited.as_bytes())},"base":snapshot})
}

/// Only a byte-identical request against the same displayed revision may reuse
/// a lost acknowledgement's operation. Resolution and ordinary saves share it.
fn source_write_reusing(snapshot: &Value, edited: &str, pending: Option<&Value>) -> Value {
    if let Some(pending) = pending {
        let same_operation =
            source_write(snapshot, edited, &text(&pending["request"]["operation_id"]));
        if &same_operation == pending {
            return pending.clone();
        }
    }
    source_write(snapshot, edited, &uuid())
}

/// Remote attachments remain bytes: never interpret a backend path as a local
/// desktop file. Decoding/hash preparation happens on the background executor.
fn preview_images(data: &Value) -> Result<BTreeMap<String, Arc<Image>>, String> {
    let mut images = BTreeMap::new();
    for asset in array(&data["assets"]) {
        let url = text(&asset["url"]);
        if !url.starts_with("tessera-asset://") || images.contains_key(&url) {
            return Err("Invalid preview attachment identity.".into());
        }
        let format = ImageFormat::from_mime_type(&text(&asset["media_type"]))
            .ok_or_else(|| "Unsupported preview image format.".to_string())?;
        let bytes = STANDARD
            .decode(text(&asset["content_base64"]))
            .map_err(|_| "Invalid preview attachment bytes.".to_string())?;
        images.insert(url, Arc::new(Image::from_bytes(format, bytes)));
    }
    Ok(images)
}

fn preview_image(images: &BTreeMap<String, Arc<Image>>, url: &str) -> Option<super::MarkdownImage> {
    if let Some(image) = images.get(url) {
        Some(super::MarkdownImage::Source(ImageSource::from(
            image.clone(),
        )))
    } else if url.starts_with("https://") || url.starts_with("http://") {
        None // The reader's normal HTTP image handling remains available.
    } else {
        Some(super::MarkdownImage::Unavailable)
    }
}

fn array(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}
fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").to_string()
}
fn next_action(snapshot: &Value, conversation: &Value, capabilities: &Value) -> &'static str {
    if !snapshot["goal"].is_object() {
        return "Capture a thought and define what would make it done.";
    }
    if snapshot["goal"]["status"] == "completed" {
        return "Goal criteria passed. Review the saved result or capture another thought. Todoist remains the authority for the actual task.";
    }
    if snapshot["pending_task_operation_id"].is_string() {
        return "Recover the existing task operation before creating or linking another task.";
    }
    match snapshot["phase"].as_str().unwrap_or("") {
        "prepared" | "not_started" => "Review the prepared context, then start this stage in T3.",
        "indeterminate" => "Recover the original stage state; its start has not been confirmed.",
        "running" | "submitting" => {
            "The backend owns this stage. Open its T3 thread to inspect progress or intervene; closing this window does not stop it."
        }
        "outcome_ready" => {
            "The engine has returned. Review the saved result against the goal criteria, or prepare an explicit follow-up."
        }
        "discarded" => {
            "The prepared stage was discarded before dispatch. Its history is retained; prepare a replacement when ready."
        }
        "cancelled" => {
            "The stage was cancelled. Inspect its retained history before planning another step."
        }
        _ if conversation["status"] == "running" => {
            "The discussion is in progress. Its reply will be saved with this goal."
        }
        _ if matches!(
            conversation["status"].as_str(),
            Some("interrupted" | "failed" | "error")
        ) =>
        {
            "The conversation stopped. Review its saved messages before explicitly sending a new message."
        }
        _ if !snapshot["task"].is_object() && capabilities["chat"] == false => {
            "Configure the LLM connection to discuss this goal, or link its existing Todoist task."
        }
        _ if !snapshot["task"].is_object() => {
            "Include relevant sources and discuss the goal, then create or link its Todoist task."
        }
        _ if capabilities["t3"] == false => {
            "Configure the T3 connection, then describe the next stage and prepare its context."
        }
        _ => {
            "Describe the next stage, then prepare context from this goal, discussion and selected sources."
        }
    }
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("UTC timestamp")
}
fn uuid() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Clone, Copy, PartialEq)]
enum Surface {
    Conversation,
    Execution,
    Outcome,
    Details,
    Source,
    Context,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Collection {
    Inbox,
    Attention,
    Goals,
    Sources,
}

#[derive(Clone)]
enum PendingNavigation {
    Goal(String),
    GoalCriteria,
    DecisionReuse(String),
    Attention(Value),
    Source(String),
    SourceLink(String, Option<String>),
    SourceBack,
    Capture,
    DiscussionNote(discussion_note::Selection),
    NewNote(discussion_note::Target, SocketAddr),
    Collection(Collection),
    Surface(Surface),
    Conversation(String),
    Thought,
}
actions!(
    brain,
    [
        SearchBrain,
        CaptureThought,
        SaveDraft,
        KeepEditing,
        FindSource
    ]
);
pub(crate) fn bind_keys(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new(
        "cmd-f",
        FindSource,
        Some("TesseraWorkspace"),
    )]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new(
        "ctrl-f",
        FindSource,
        Some("TesseraWorkspace"),
    )]);
    cx.bind_keys([
        KeyBinding::new("ctrl-k", SearchBrain, Some("TesseraWorkspace")),
        KeyBinding::new("ctrl-s", SaveDraft, Some("TesseraWorkspace")),
        KeyBinding::new("n", CaptureThought, Some("TesseraWorkspace && !Input")),
        KeyBinding::new("escape", KeepEditing, Some("TesseraWorkspace")),
    ]);
}

/// A tiny local operation journal, outside the disposable search index. Records
/// are immutable; terminal markers belong to one exact operation, so another
/// desktop window cannot clear a newer request by acknowledging an older one.
struct PreparedJournal {
    root: std::path::PathBuf,
    workspace: Value,
}
impl PreparedJournal {
    fn for_workspace(workspace: &Value) -> Result<Self, String> {
        let brain = workspace["brain_id"]
            .as_str()
            .ok_or("Missing brain identity.")?;
        Uuid::parse_str(brain).map_err(|_| "Invalid brain identity.")?;
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".config")
            })
            .join("tessera/prepared-operations")
            .join(brain);
        Ok(Self {
            root,
            workspace: workspace.clone(),
        })
    }
    fn path(&self, request: &Value, suffix: &str) -> Result<std::path::PathBuf, String> {
        let id = request["operation_id"]
            .as_str()
            .ok_or("Missing operation identity.")?;
        let id = Uuid::parse_str(id).map_err(|_| "Invalid operation identity.")?;
        Ok(self.root.join(format!("{id}.{suffix}")))
    }
    fn record(&self, request: &Value) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&json!({"schema":"tessera-prepared-operation/v1","workspace":self.workspace,"request":request})).map_err(|e|e.to_string())
    }
    fn write_once(&self, request: &Value, suffix: &str) -> Result<(), String> {
        let path = self.path(request, suffix)?;
        let bytes = self.record(request)?;
        let mut missing = Vec::new();
        let mut ancestor = self.root.as_path();
        while !ancestor.exists() {
            missing.push(ancestor.to_path_buf());
            ancestor = ancestor
                .parent()
                .ok_or("Invalid operation journal directory.")?;
        }
        std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        // A synced file does not make a newly created directory's name durable.
        // Persist each newly created entry, from its existing ancestor downward.
        for directory in missing.iter().rev() {
            let parent = directory
                .parent()
                .ok_or("Invalid operation journal parent.")?;
            std::fs::File::open(parent)
                .and_then(|parent| parent.sync_all())
                .map_err(|e| e.to_string())?;
        }
        let temporary = self.root.join(format!(".operation-{}.tmp", uuid()));
        let result = (|| -> std::io::Result<()> {
            if suffix == "done"
                && std::fs::read(self.path(request, "json").map_err(std::io::Error::other)?)?
                    != bytes
            {
                return Err(std::io::Error::other("Operation journal identity mismatch"));
            }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            match std::fs::hard_link(&temporary, &path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if std::fs::read(&path)? != bytes {
                        return Err(std::io::Error::other("Operation journal identity mismatch"));
                    }
                }
                Err(error) => return Err(error),
            }
            std::fs::File::open(&self.root)?.sync_all()
        })();
        let _ = std::fs::remove_file(temporary);
        result.map_err(|e| format!("Cannot save prepared-stage recovery state: {e}"))
    }
    fn pending(&self) -> Result<BTreeMap<String, Value>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut paths = entries
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        paths.sort();
        let mut pending = BTreeMap::new();
        for path in paths {
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let entry: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if entry["workspace"] != self.workspace
                || entry["schema"] != "tessera-prepared-operation/v1"
            {
                return Err(
                    "Prepared-stage recovery belongs to a different workspace identity.".into(),
                );
            }
            let request = entry["request"].clone();
            if self.path(&request, "json")? != path {
                return Err("Prepared-stage recovery identity mismatch.".into());
            }
            match std::fs::read(self.path(&request, "done")?) {
                Ok(done) if done == bytes => continue,
                Ok(_) => return Err("Prepared-stage recovery acknowledgement mismatch.".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
            pending.entry(text(&request["goal_id"])).or_insert(request);
        }
        Ok(pending)
    }
}

#[derive(Clone)]
struct PreparedEdit {
    expected: Value,
    draft: Entity<TextareaState>,
    discard: bool,
}

pub struct BrainView {
    maestro_ui: maestro_ui::MaestroUi,
    context_ui: context_ui::ContextUi,
    discussion_context: discussion_context::DiscussionContextUi,
    new_conversation: new_conversation::NewConversationUi,
    discussion_send: discussion_send_ui::DiscussionSendUi,
    discussion_note: discussion_note_ui::DiscussionNoteUi,
    discussion_decision: Box<discussion_decision_ui::DecisionUi>,
    goal_criteria: goal_criteria_ui::GoalCriteriaUi,
    decision_reuse: discussion_decision_reuse_ui::ReuseUi,
    note_link: note_link_ui::NoteLinkUi,
    open_link: open_link_ui::OpenLinkUi,
    inbox_ui: inbox_ui::InboxUi,
    inbox_plan: inbox_plan_ui::InboxPlanUi,
    bound_attention: attention_ui::AttentionUi,
    proposals: proposal_ui::ProposalUi,
    adoption: proposal_adoption_ui::AdoptionUi,
    endpoint: SocketAddr,
    expected_workspace: Option<Value>,
    editor: editor_ui::EditorUi,
    snapshot: Value,
    workspace_attention: Value,
    attention_error: Option<String>,
    attention_target: Option<Value>,
    opened_attention: Option<Value>,
    selected_goal_id: Option<String>,
    pending_capture_id: Option<String>,
    pending_capture_fields: Option<(String, String)>,
    show_capture: bool,
    show_evidence: bool,
    show_history: bool,
    goal_drafts: BTreeMap<String, (String, String)>,
    capabilities: Value,
    conversation: Value,
    result: Value,
    source_list: Vec<Value>,
    selected_sources: BTreeSet<String>,
    source_snapshot: Option<Value>,
    source_conflict: Option<Value>,
    source_current: Entity<TextareaState>,
    source_base: Entity<TextareaState>,
    pending_source_write: Option<Value>,
    pending_source_read_draft: Option<String>,
    source_navigation: source_navigation::SourceNavigation,
    incoming_references: incoming_references_ui::IncomingReferencesUi,
    source_original: String,
    source_editable: bool,
    source_loading: bool,
    preview: Value,
    preview_images: BTreeMap<String, Arc<Image>>,
    attachment_preview: Option<(String, Arc<Image>)>,
    preview_generation: u64,
    preview_loading: bool,
    preview_error: Option<String>,
    link_candidates: Vec<Value>,
    title: Entity<InputState>,
    criteria: Entity<TextareaState>,
    needs_human: bool,
    compose: Entity<TextareaState>,
    source: source_input::SourceInput,
    source_projection: source_projection_ui::SourceProjectionUi,
    source_find: source_find::SourceFind,
    filter: Entity<InputState>,
    next_step: Entity<TextareaState>,
    todoist_picker: todoist_picker::PickerUi,
    surface: Surface,
    collection: Collection,
    embedded: bool,
    pending_navigation: Option<PendingNavigation>,
    navigation_after_source: bool,
    conversation_id: Option<String>,
    pending_task: Option<String>,
    prepared_journal: Option<PreparedJournal>,
    prepared_edits: BTreeMap<String, PreparedEdit>,
    pending_prepared_changes: BTreeMap<String, Value>,
    pending_message: Option<(String, usize)>,
    selected_evidence: BTreeSet<String>,
    busy: bool,
    poll_scheduled: bool,
    app_quit_token: Option<u64>,
    app_quit_ready: bool,
    poll_in_flight: bool,
    request_generation: u64,
    error: Option<String>,
    notice: String,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl BrainView {
    pub fn new(endpoint: SocketAddr, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_guarded(endpoint, None, window, cx)
    }
    pub fn new_guarded(
        endpoint: SocketAddr,
        expected_workspace: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if expected_workspace.is_some() {
            app_quit::register(cx.weak_entity(), window.window_handle(), cx);
            let weak = cx.weak_entity();
            window.on_window_should_close(cx, move |window, cx| {
                if !crate::reader_editor::save_window(window.window_handle(), cx) {
                    return false;
                }
                weak.update(cx, |this, cx| this.editor_request_close(window, cx))
                    .unwrap_or(true)
            });
        }
        let title =
            cx.new(|cx| InputState::new(window, cx).placeholder("What would you like to achieve?"));
        let criteria = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Observable outcome — one criterion per line")
        });
        let compose = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Clarify the goal, ask a question, or change direction…")
        });
        let source = source_input::SourceInput::new(expected_workspace.is_some(), window, cx);
        let source_current = cx.new(|cx| TextareaState::new(window, cx).rows(6).soft_wrap(false));
        let source_base = cx.new(|cx| TextareaState::new(window, cx).rows(4).soft_wrap(false));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search goals…"));
        let next_step = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("What should T3 do in this stage?")
        });
        let filter_sub = cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify());
        let source_sub = match &source {
            source_input::SourceInput::Legacy(source) => {
                cx.subscribe_in(source, window, |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.editor_input_changed();
                        this.schedule_preview(window, cx);
                        this.editor_protect(window, cx);
                    }
                    cx.notify();
                })
            }
            source_input::SourceInput::Managed(source) => cx.subscribe_in(
                source,
                window,
                |this, _, event: &gpui_component::input::projection::SourceMutation, window, cx| {
                    this.managed_source_changed(*event, window, cx);
                },
            ),
        };
        let clipboard_sub = source.managed().map(|source| {
            cx.subscribe(
                source,
                |this, _, event: &gpui_component::input::clipboard::ClipboardPasteEvent, cx| {
                    this.source_projection.clipboard_error = match event {
                        gpui_component::input::clipboard::ClipboardPasteEvent::Applied {
                            ..
                        } => None,
                        gpui_component::input::clipboard::ClipboardPasteEvent::Failed(error) => {
                            Some(format!("Paste unavailable: {error:?}"))
                        }
                    };
                    cx.notify();
                },
            )
        });
        let mut view = Self {
            context_ui: context_ui::ContextUi::default(),
            inbox_ui: inbox_ui::InboxUi::new(window, cx),
            inbox_plan: inbox_plan_ui::InboxPlanUi::new(window, cx),
            bound_attention: attention_ui::AttentionUi::new(window, cx),
            proposals: proposal_ui::ProposalUi::new(window, cx),
            adoption: proposal_adoption_ui::AdoptionUi::default(),
            endpoint,
            embedded: expected_workspace.is_some(),
            pending_navigation: None,
            navigation_after_source: false,
            expected_workspace,
            editor: editor_ui::EditorUi::default(),
            maestro_ui: maestro_ui::MaestroUi::default(),
            snapshot: Value::Null,
            workspace_attention: Value::Null,
            attention_error: None,
            attention_target: None,
            opened_attention: None,
            selected_goal_id: None,
            pending_capture_id: None,
            pending_capture_fields: None,
            show_capture: false,
            show_evidence: false,
            show_history: false,
            goal_drafts: BTreeMap::new(),
            capabilities: Value::Null,
            conversation: Value::Null,
            discussion_context: discussion_context::DiscussionContextUi::default(),
            new_conversation: new_conversation::NewConversationUi::default(),
            discussion_send: discussion_send_ui::DiscussionSendUi::default(),
            discussion_note: discussion_note_ui::DiscussionNoteUi::new(window, cx),
            discussion_decision: Default::default(),
            goal_criteria: Default::default(),
            decision_reuse: Default::default(),
            note_link: Default::default(),
            open_link: Default::default(),
            result: Value::Null,
            source_list: vec![],
            selected_sources: BTreeSet::new(),
            source_snapshot: None,
            source_conflict: None,
            source_current,
            source_base,
            pending_source_write: None,
            pending_source_read_draft: None,
            source_navigation: Default::default(),
            incoming_references: Default::default(),
            source_original: String::new(),
            source_editable: false,
            source_loading: false,
            preview: Value::Null,
            preview_images: BTreeMap::new(),
            attachment_preview: None,
            preview_generation: 0,
            preview_loading: false,
            preview_error: None,
            link_candidates: Vec::new(),
            title,
            criteria,
            needs_human: false,
            compose,
            source,
            source_projection: source_projection_ui::SourceProjectionUi::default(),
            source_find: Default::default(),
            filter,
            next_step,
            todoist_picker: todoist_picker::PickerUi::default(),
            surface: Surface::Conversation,
            collection: Collection::Goals,
            conversation_id: None,
            pending_task: None,
            prepared_journal: None,
            prepared_edits: BTreeMap::new(),
            pending_prepared_changes: BTreeMap::new(),
            pending_message: None,
            selected_evidence: BTreeSet::new(),
            busy: false,
            poll_scheduled: false,
            app_quit_token: None,
            app_quit_ready: false,
            poll_in_flight: false,
            request_generation: 0,
            error: None,
            notice: "Connecting…".into(),
            focus: cx.focus_handle(),
            _subscriptions: vec![
                filter_sub,
                source_sub,
                Self::heading_keyboard_cancel_subscription(cx),
            ]
            .into_iter()
            .chain(clipboard_sub)
            .collect(),
        };
        view.ensure_discussion_send();
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                this.editor_list(window, cx);
                this.batch(
                    vec![
                        json!({"op":"capabilities"}),
                        json!({"op":"snapshot","goal_id":this.initial_discussion_goal()}),
                        json!({"op":"source_list"}),
                    ],
                    window,
                    cx,
                )
            });
        })
        .detach();
        view
    }
    fn goal_id(&self) -> String {
        text(&self.snapshot["goal"]["id"])
    }
    pub(crate) fn dirty(&self, cx: &App) -> bool {
        self.decision_reuse.active
            || self.discussion_decision.active
            || self.discussion_note.active()
            || self.goal_criteria.active
            || self.source_dirty(cx)
    }
    fn source_dirty(&self, cx: &App) -> bool {
        self.editor_blocks_navigation()
            || (self.source_editable && self.source.value(cx).as_ref() != self.source_original)
    }
    fn running(&self) -> bool {
        self.context_ui.running(&self.goal_id())
            || self.conversation["status"] == "running"
            || ["submitting", "running", "indeterminate"]
                .contains(&self.snapshot["phase"].as_str().unwrap_or(""))
    }
    fn scoped_request(&self, mut request: Value) -> Value {
        if matches!(
            request["op"].as_str(),
            Some(
                "snapshot"
                    | "goal_source"
                    | "start"
                    | "reconcile"
                    | "poll"
                    | "accept_human"
                    | "criterion_evaluate"
            )
        ) && request.get("goal_id").is_none()
        {
            if let Some(id) = &self.selected_goal_id {
                request["goal_id"] = json!(id);
            }
        }
        request
    }
    fn batch(&mut self, requests: Vec<Value>, window: &mut Window, cx: &mut Context<Self>) {
        self.batch_inner(requests, false, window, cx);
    }
    fn batch_inner(
        &mut self,
        requests: Vec<Value>,
        background: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || (background && self.poll_in_flight) {
            return;
        }
        if self.pending_capture_id.is_some()
            && requests.iter().any(|request| {
                matches!(
                    request["op"].as_str(),
                    Some(
                        "chat_start"
                            | "task_create"
                            | "task_link"
                            | "stage_prepare"
                            | "stage_revise"
                            | "stage_discard"
                            | "start"
                            | "accept_human"
                            | "criterion_evaluate"
                    )
                )
            })
        {
            self.error=Some("Recover the pending capture with Refresh, or select an existing goal before sending more work.".into());
            cx.notify();
            return;
        }
        if !background && self.todoist_picker.settling() {
            self.reset_todoist_picker();
            self.notice = "Refresh the original goal to observe its pending link result.".into();
        }
        if !background
            && requests.iter().any(|request| {
                matches!(
                    request["op"].as_str(),
                    Some("task_create" | "task_link" | "task_reconcile")
                )
            })
        {
            self.reset_todoist_picker();
        }
        if background {
            self.poll_in_flight = true;
        } else {
            // User actions remain available during polling. Any older read may
            // complete, but cannot overwrite the state of the newer action.
            self.request_generation += 1;
            self.busy = true;
            self.sync_source_policy(cx);
            cx.notify();
        }
        let generation = self.request_generation;
        if requests.iter().any(|r| r["op"] == "source_read") {
            self.pending_source_read_draft = Some(self.source.value(cx).to_string());
        }
        let mut requests = requests;
        if self.capabilities["workspace_attention"] == true
            && requests.iter().any(|r| r["op"] == "snapshot")
            && !requests.iter().any(|r| r["op"] == "workspace_attention")
        {
            requests.push(json!({"op":"workspace_attention"}));
        }
        let requests = requests
            .into_iter()
            .map(|request| self.scoped_request(request))
            .collect::<Vec<_>>();
        let endpoint = self.endpoint;
        let expected_workspace = self.expected_workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let replies = cx
                .background_executor()
                .spawn(async move {
                    let mut replies = Vec::new();
                    for request in requests {
                        let operation = text(&request["op"]);
                        let origin_goal = request["goal_id"]
                            .as_str()
                            .or(request["scope"]["goal_id"].as_str())
                            .map(str::to_owned);
                        let client_request = request.clone();
                        let mut wire_request = request;
                        if operation == "brain_search" {
                            wire_request
                                .as_object_mut()
                                .unwrap()
                                .remove("_context_search_id");
                        }
                        if operation.starts_with("inbox_") {
                            if let Some(object) = wire_request.as_object_mut() {
                                object.remove("_inbox_workspace");
                                object.remove("_inbox_sequence");
                                object.remove("_inbox_plan_origin");
                                object.remove("_inbox_plan_goal_at_submit");
                            }
                        }
                        if operation.starts_with("attention_") {
                            if let Some(object) = wire_request.as_object_mut() {
                                object.remove("_attention_workspace");
                                object.remove("_attention_sequence");
                            }
                        }
                        if operation.starts_with("proposal_") {
                            if let Some(object) = wire_request.as_object_mut() {
                                object.remove("_proposal_workspace");
                                object.remove("_proposal_sequence");
                                object.remove("_proposal_pending");
                            }
                        }
                        if operation.starts_with("maestro_") {
                            if let Some(object) = wire_request.as_object_mut() {
                                object.remove("source");
                                object.remove("_maestro_workspace");
                                object.remove("_maestro_sequence");
                                object.remove("_maestro_goal");
                                object.remove("_maestro_original");
                            }
                        }
                        let result =
                            rpc_guarded(endpoint, wire_request, expected_workspace.as_ref()).map(
                                |mut data| {
                                    if (operation.starts_with("context_")
                                        || operation == "brain_search"
                                        || operation == "goal_context_brief")
                                        && data.is_object()
                                    {
                                        data["_client_context_goal_id"] = json!(origin_goal);
                                    }
                                    if operation == "brain_search" && data.is_object() {
                                        data["_client_search_request"] = client_request.clone();
                                    }
                                    if operation.starts_with("inbox_") && data.is_object() {
                                        data["_client_inbox_request"] = client_request.clone();
                                    }
                                    if operation.starts_with("attention_") && data.is_object() {
                                        data["_client_attention_request"] = client_request.clone();
                                    }
                                    if operation.starts_with("proposal_") && data.is_object() {
                                        data["_client_proposal_request"] = client_request.clone();
                                    }
                                    if operation.starts_with("maestro_") && data.is_object() {
                                        data["_client_maestro_request"] = client_request;
                                    }
                                    data
                                },
                            );
                        let failed = result.is_err();
                        replies.push((operation, result));
                        if failed {
                            break;
                        }
                    }
                    replies
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.finish_batch(replies, background, generation, window, cx) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
    /// Apply one completed batch. Polls never enter foreground loading state;
    /// unchanged replies do not invalidate the rendered view.
    fn finish_batch(
        &mut self,
        replies: Vec<(String, Result<Value, String>)>,
        background: bool,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if background {
            self.poll_in_flight = false;
            if self.request_generation != generation {
                self.schedule_poll(window, cx);
                return false;
            }
        } else {
            self.busy = false;
            self.sync_source_policy(cx);
        }
        let mut visible_changed = !background;
        let mut changed = false;
        let mut saved = false;
        let mut context_recheck = false;
        let mut got_snapshot = false;
        for (operation, result) in replies {
            match result {
                Err(error) => {
                    if operation == "goal_context_brief" {
                        self.context_brief_failed(error, window, cx);
                        visible_changed = true;
                        continue;
                    }
                    if operation == "proposal_adopt" {
                        self.adoption.error = Some(format!(
                            "{error} The exact adoption delivery remains retained."
                        ));
                        visible_changed = true;
                        continue;
                    }
                    if operation.starts_with("proposal_") {
                        self.proposals.error = Some(format!(
                            "{error} Any unconfirmed proposal delivery is retained."
                        ));
                        visible_changed = true;
                        continue;
                    }
                    if operation.starts_with("maestro_") {
                        self.maestro_ui.error = Some(format!("{error} Any unconfirmed link request is retained. Recover the same request."));
                        visible_changed = true;
                        continue;
                    }
                    if operation == "inbox_plan" {
                        self.inbox_plan.error = Some(format!(
                            "{error} The exact planning request and draft are retained. Use Recover goal creation."
                        ));
                        visible_changed = true;
                        continue;
                    }
                    if operation.starts_with("attention_") {
                        self.bound_attention.error = Some(format!(
                            "{error} The exact request and decision draft are retained. Use Recover delivery."
                        ));
                        visible_changed = true;
                        continue;
                    }
                    if operation.starts_with("inbox_") {
                        self.inbox_ui.error = Some(format!(
                            "{error} Your capture draft and any unconfirmed delivery are retained. Refresh the inbox or recover the same capture."
                        ));
                        visible_changed = true;
                        continue;
                    }
                    context_recheck |= matches!(
                        operation.as_str(),
                        "context_prepare" | "context_revise" | "context_export_start"
                    ) || (operation == "stage_prepare"
                        && self.capabilities["reviewed_context"] == true);
                    if matches!(
                        operation.as_str(),
                        "workspace_attention" | "snapshot" | "capabilities"
                    ) {
                        self.attention_error = Some(error.clone());
                    }
                    if operation == "snapshot" {
                        self.maestro_ui.backend_unavailable = true;
                        self.attention_target = None;
                    }
                    if operation == "source_read" {
                        self.editor_read_failed(window, cx);
                    }
                    visible_changed |= self.error.as_ref() != Some(&error);
                    self.error = Some(error);
                    if !background {
                        self.notice = "Action needs attention".into();
                        self.source_loading = false;
                        self.sync_source_policy(cx);
                    }
                    // Recover an uncertain mutation through a fresh read,
                    // never by repeating capture/chat/start with a new ID.
                    changed = matches!(
                        operation.as_str(),
                        "create_goal"
                            | "chat_start"
                            | "task_create"
                            | "task_link"
                            | "stage_prepare"
                            | "stage_revise"
                            | "stage_discard"
                            | "start"
                    );
                }
                Ok(data) => {
                    if operation == "snapshot" {
                        visible_changed |= self.maestro_ui.backend_unavailable;
                        self.maestro_ui.backend_unavailable = false;
                    }
                    if background {
                        got_snapshot |= operation == "snapshot";
                        let unchanged = match operation.as_str() {
                            "snapshot" => self.snapshot == data,
                            "chat_get" => self.conversation == data,
                            "result" => self.result == data,
                            "context_export_get" => {
                                self.context_ui.unchanged(&self.goal_id(), &data)
                            }
                            _ => false,
                        };
                        if unchanged {
                            continue;
                        }
                        visible_changed = true;
                    } else {
                        self.notice = "Connected".into();
                    }
                    if operation.starts_with("maestro_") {
                        self.finish_maestro(&operation, data, window, cx);
                        continue;
                    }
                    match operation.as_str() {
                        "capabilities" => {
                            if self.capabilities["todoist"] != data["todoist"]
                                || self.capabilities["todoist_inbox_picker"]
                                    != data["todoist_inbox_picker"]
                            {
                                self.reset_todoist_picker();
                            }
                            self.capabilities = data;
                            self.ensure_discussion_send();
                            self.ensure_maestro();
                            self.ensure_inbox(window, cx);
                            self.ensure_inbox_plan(window, cx);
                            self.ensure_adoption();
                            self.ensure_bound_attention(window, cx);
                            self.ensure_proposals();
                            if self.prepared_journal.is_none()
                                && self.capabilities["prepared_stage_edit"] == true
                            {
                                if let Some(identity) = &self.expected_workspace {
                                    match PreparedJournal::for_workspace(identity).and_then(
                                        |journal| {
                                            let pending = journal.pending()?;
                                            Ok((journal, pending))
                                        },
                                    ) {
                                        Ok((journal, pending)) => {
                                            self.prepared_journal = Some(journal);
                                            self.pending_prepared_changes = pending;
                                            for request in self
                                                .pending_prepared_changes
                                                .values()
                                                .cloned()
                                                .collect::<Vec<_>>()
                                            {
                                                self.restore_prepared_edit(&request, window, cx);
                                            }
                                        }
                                        Err(error) => self.error = Some(error),
                                    }
                                }
                            }
                        }
                        "workspace_attention" => {
                            self.workspace_attention = data;
                            if self.error == self.attention_error {
                                self.error = None;
                            }
                            self.attention_error = None;
                        }
                        "brain_search"
                        | "goal_context_brief"
                        | "brain_index_status"
                        | "context_prepare"
                        | "context_get"
                        | "context_revise"
                        | "context_export_start"
                        | "context_export_get"
                        | "context_export_cancel" => {
                            self.context_reply(&operation, data, window, cx)
                        }
                        "proposal_adopt" => self.adoption_reply(data, window, cx),
                        "proposal_list"
                        | "proposal_get"
                        | "proposal_disposition"
                        | "proposal_retry" => {
                            self.proposal_reply(&operation, data, window, cx);
                        }
                        "attention_list" | "attention_get" | "attention_reply"
                        | "attention_ack" => {
                            self.bound_attention_reply(&operation, data, window, cx);
                        }
                        "inbox_plan" => {
                            self.inbox_plan_reply(data, window, cx);
                        }
                        "inbox_capture" | "inbox_list" | "inbox_get" => {
                            self.inbox_reply(&operation, data, window, cx);
                        }
                        "source_list" => self.source_list = array(&data["sources"]),
                        "snapshot" | "create_goal" | "stage_prepare" | "stage_revise"
                        | "stage_discard" | "start" | "reconcile" | "criterion_evaluate"
                        | "accept_human" | "goal_selection" => {
                            if data["prepared_change_rejected"].is_object() {
                                let rejected = &data["prepared_change_rejected"];
                                let goal = text(&rejected["goal_id"]);
                                if self
                                    .pending_prepared_changes
                                    .get(&goal)
                                    .is_some_and(|request| {
                                        request["operation_id"] == rejected["operation_id"]
                                    })
                                {
                                    let request = self.pending_prepared_changes[&goal].clone();
                                    self.restore_prepared_edit(&request, window, cx);
                                    let terminal = self
                                        .prepared_journal
                                        .as_ref()
                                        .ok_or("Missing operation journal.".to_string())
                                        .and_then(|journal| journal.write_once(&request, "done"));
                                    if let Err(error) = terminal {
                                        self.error = Some(error);
                                        continue;
                                    }
                                    self.pending_prepared_changes.remove(&goal);
                                    self.reload_prepared_operations();
                                    self.error = Some(format!(
                                        "{} Your local draft is preserved.",
                                        text(&rejected["message"])
                                    ));
                                    changed = true;
                                }
                                continue;
                            }
                            if matches!(operation.as_str(), "stage_revise" | "stage_discard")
                                && !self.acknowledge_prepared_change(&data, window, cx)
                            {
                                continue;
                            }
                            let next_goal = data["goal"]["id"].as_str().map(str::to_string);
                            if self.pending_capture_id == next_goal {
                                self.pending_capture_id = None;
                                if let Some((title, criteria)) = self.pending_capture_fields.take()
                                {
                                    if self.title.read(cx).value().as_ref() == title
                                        && self.criteria.read(cx).value().as_ref() == criteria
                                    {
                                        self.title.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        self.criteria.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                    }
                                }
                            }
                            if self.snapshot["goal"]["id"] != data["goal"]["id"] {
                                if self.dirty(cx) {
                                    self.snapshot["goals"] = data["goals"].clone();
                                    self.attention_target = None;
                                    self.error=Some("The source changed while navigation was pending. Save or discard it before switching goals.".into());
                                    continue;
                                }
                                self.goal_drafts.insert(
                                    self.goal_id(),
                                    (
                                        self.compose.read(cx).value().to_string(),
                                        self.next_step.read(cx).value().to_string(),
                                    ),
                                );
                                self.opened_attention = None;
                                self.conversation = Value::Null;
                                self.reset_discussion_context();
                                self.result = Value::Null;
                                self.selected_evidence.clear();
                                self.show_evidence = false;
                                self.show_history = false;
                                self.pending_message = None;
                                self.source_snapshot = None;
                                self.source_original.clear();
                                self.source.reset("", window, cx);
                                self.source_conflict = None;
                                self.preview = Value::Null;
                                self.surface = Surface::Conversation;
                                let drafts = next_goal
                                    .as_ref()
                                    .and_then(|id| self.goal_drafts.get(id))
                                    .cloned()
                                    .unwrap_or_default();
                                self.compose
                                    .update(cx, |input, cx| input.set_value(drafts.0, window, cx));
                                self.next_step
                                    .update(cx, |input, cx| input.set_value(drafts.1, window, cx));
                            }
                            self.selected_goal_id = next_goal;
                            self.selected_sources = array(&data["selected_source_paths"])
                                .iter()
                                .filter_map(|p| p.as_str().map(str::to_string))
                                .collect();
                            self.conversation_id =
                                if self.new_conversation.drafting(&text(&data["goal"]["id"])) {
                                    None
                                } else {
                                    data["selected_conversation_id"]
                                        .as_str()
                                        .map(str::to_string)
                                };
                            if operation == "create_goal" {
                                self.show_capture = false;
                            }
                            if self.snapshot["goal"]["id"] != data["goal"]["id"]
                                || todoist_picker::task_identity(&self.snapshot)
                                    != todoist_picker::task_identity(&data)
                            {
                                self.reset_todoist_picker();
                            }
                            self.snapshot = data;
                            self.apply_attention_target();
                            got_snapshot = true;
                            self.pending_task = self.snapshot["pending_task_operation_id"]
                                .as_str()
                                .map(str::to_string);
                            if self.conversation_id.is_none()
                                && !self.new_conversation.drafting(&self.goal_id())
                            {
                                let conversations = array(&self.snapshot["conversations"]);
                                if conversations.len() == 1 {
                                    self.conversation_id =
                                        conversations[0]["id"].as_str().map(str::to_string);
                                }
                            }
                            self.restore_discussion_draft(window, cx);
                            changed = !matches!(operation.as_str(), "snapshot" | "goal_selection");
                        }
                        "chat_start" => {
                            if let Some(message) = data["_chat_context_refused"].as_str() {
                                self.pending_message = None;
                                self.new_conversation.pending.remove(&self.goal_id());
                                self.error = Some(message.to_string());
                                self.notice =
                                    "Message was not sent. Your draft is retained.".into();
                                continue;
                            }
                            if self.new_conversation.pending.contains(&self.goal_id()) {
                                let valid = data["conversation_id"].as_str().is_some_and(|id| {
                                    Uuid::parse_str(id).is_ok()
                                        && !array(&self.snapshot["conversations"])
                                            .iter()
                                            .any(|c| c["id"] == id)
                                });
                                if !valid {
                                    self.error = Some("New conversation acknowledgement is invalid. Inspect saved conversations; nothing was resent.".into());
                                    continue;
                                }
                                self.new_conversation.pending.remove(&self.goal_id());
                                self.new_conversation.drafts.remove(&self.goal_id());
                            }
                            self.conversation_id =
                                data["conversation_id"].as_str().map(str::to_string);
                            self.conversation["status"] = json!("running");
                            if self.pending_message.as_ref().is_some_and(|(submitted, _)| {
                                self.compose.read(cx).value().trim() == submitted
                            }) {
                                self.compose
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            }
                            self.pending_message = None;
                            changed = true;
                        }
                        "chat_get" => {
                            if self
                                .discussion_send
                                .expected_conversation
                                .as_ref()
                                .is_some_and(|expected| {
                                    self.conversation_id.as_ref() == Some(expected)
                                        && data["id"] != *expected
                                })
                            {
                                continue;
                            }
                            if self.new_conversation.drafting(&self.goal_id()) {
                                continue;
                            }
                            self.conversation = data;
                            self.reconcile_discussion_context();
                            if let Some((pending, previous_count)) =
                                self.pending_message.as_ref().filter(|_| {
                                    !self.new_conversation.pending.contains(&self.goal_id())
                                })
                            {
                                let messages = array(&self.conversation["messages"]);
                                let recorded = messages.len() > *previous_count
                                    && messages
                                        .iter()
                                        .rev()
                                        .find(|m| m["role"] == "user")
                                        .is_some_and(|m| m["text"].as_str() == Some(pending));
                                if recorded {
                                    if self.compose.read(cx).value().trim() == pending {
                                        self.compose.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                    }
                                    self.pending_message = None;
                                }
                            }
                            if let Some(error) = self.conversation["error"].as_str() {
                                self.error = Some(error.into());
                            }
                        }
                        "source_read" => {
                            let changed = self
                                .pending_source_read_draft
                                .take()
                                .is_some_and(|before| before != self.source.value(cx).as_ref());
                            if changed || self.editor_blocks_navigation() {
                                self.source_loading = false;
                                self.sync_source_policy(cx);
                                self.error=Some("The source draft changed while loading. Your draft is retained; save or discard it before navigating.".into());
                            } else {
                                self.load_source(data, window, cx);
                            }
                        }
                        "source_write" => {
                            self.pending_source_write = None;
                            if let Some(conflict) = data.get("source_conflict") {
                                self.show_source_conflict(conflict.clone(), window, cx);
                            } else {
                                self.source_conflict = None;
                                self.error = None;
                                self.notice = "Source saved".into();
                                saved = true;
                            }
                        }
                        "result" => {
                            self.result = data;
                        }
                        "task_create" | "task_link" | "task_refresh" | "task_reconcile" => {
                            if data["status"] == "indeterminate" {
                                self.error=Some("Task delivery is uncertain. Use Recover task to reconcile the original operation.".into());
                            } else if data["status"] == "rejected" {
                                self.pending_task = None;
                                self.error = Some(text(&data["error"]));
                            } else {
                                self.pending_task = None;
                            }
                            changed = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        if !background
            && self.navigation_after_source
            && !self.dirty(cx)
            && !self.source_loading
            && self.pending_navigation.is_some()
        {
            self.continue_navigation(window, cx);
            return true;
        }
        if context_recheck {
            self.batch(
                vec![json!({"op":"context_get","goal_id":self.goal_id()})],
                window,
                cx,
            );
        } else if saved {
            if let Some(source) = &self.source_snapshot {
                self.batch(
                    vec![
                        json!({"op":"source_read","path":source["path"]}),
                        json!({"op":"source_list"}),
                    ],
                    window,
                    cx,
                );
            }
        } else if changed {
            self.refresh(window, cx);
        } else if got_snapshot
            && (self.context_ui.running(&self.goal_id())
                || (self.capabilities["workspace_attention"] == true
                    && self.workspace_attention.is_null())
                || self.conversation_id.is_some()
                || !array(&self.snapshot["stage"]["result_ids"]).is_empty())
        {
            self.details_inner(background, window, cx);
        } else {
            self.schedule_poll(window, cx);
        }
        self.editor_flush(window, cx);
        self.resume_proposal_context(window, cx);
        visible_changed
    }
    pub(crate) fn busy(&self) -> bool {
        self.busy
    }
    pub(crate) fn refresh_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source_snapshot.is_some() {
            self.schedule_preview(window, cx);
        }
        if self.collection == Collection::Attention && self.capabilities["attention_read"] == true {
            self.refresh_bound_attention(false, window, cx);
            return;
        }
        if self.collection == Collection::Inbox && self.capabilities["inbox_read"] == true {
            self.refresh_inbox(false, window, cx);
            return;
        }
        let goal_id = self
            .pending_capture_id
            .clone()
            .or_else(|| self.selected_goal_id.clone());
        self.batch(
            vec![
                json!({"op":"capabilities"}),
                json!({"op":"snapshot","goal_id":goal_id}),
                json!({"op":"source_list"}),
            ],
            window,
            cx,
        );
    }
    pub(crate) fn refresh_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_todoist_picker();
        self.batch(
            vec![json!({"op":"capabilities"}), json!({"op":"snapshot"})],
            window,
            cx,
        );
    }
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let goal_id = self
            .pending_capture_id
            .clone()
            .or_else(|| self.selected_goal_id.clone());
        self.batch(vec![json!({"op":"snapshot","goal_id":goal_id})], window, cx);
    }
    fn details_inner(&mut self, background: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut requests = Vec::new();
        if let Some(request) = self.context_ui.poll(&self.goal_id()) {
            requests.push(request);
        }
        if self.capabilities["workspace_attention"] == true && self.workspace_attention.is_null() {
            requests.push(json!({"op":"workspace_attention"}));
        }
        if let Some(id) = &self.conversation_id {
            requests.push(json!({"op":"chat_get","conversation_id":id}));
        }
        if let Some(id) = array(&self.snapshot["stage"]["result_ids"]).last() {
            requests.push(json!({"op":"result","result_id":id}));
        }
        if requests.is_empty() {
            self.schedule_poll(window, cx);
        } else {
            self.batch_inner(requests, background, window, cx);
        }
    }
    fn attention_unknown(&self) -> bool {
        self.attention_error.is_some() || self.capabilities["workspace_attention"] != true
    }
    fn attention_polling(&self) -> bool {
        self.capabilities["workspace_attention"] == true
            && (self.collection == Collection::Attention
                || self.workspace_attention["running_goal_count"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0)
    }
    fn maestro_polling(&self) -> bool {
        let link = &self.snapshot["maestro"]["link"];
        link["active"] == true
            && !self.goal_id().is_empty()
            && link["goal_id"] == self.snapshot["goal"]["id"]
            && self.selected_goal_id.as_deref() == Some(self.goal_id().as_str())
    }
    fn needs_polling(&self) -> bool {
        self.running() || self.attention_polling() || self.maestro_polling()
    }
    fn schedule_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.poll_scheduled || !self.needs_polling() {
            return;
        }
        self.poll_scheduled = true;
        let interval = if self.running() { 1 } else { 5 };
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(interval))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.poll_scheduled = false;
                if !this.busy && this.needs_polling() {
                    this.batch_inner(vec![json!({"op":"snapshot"})], true, window, cx);
                }
            });
        })
        .detach();
    }
    fn select_goal(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_guard_navigation(PendingNavigation::Goal(id.clone()), cx) {
            return;
        }
        if self.busy
            || (self.pending_capture_id.is_none() && self.selected_goal_id.as_ref() == Some(&id))
        {
            return;
        }
        if self.dirty(cx) {
            self.pending_navigation = Some(PendingNavigation::Goal(id));
            self.surface = Surface::Source;
            self.show_capture = false;
            self.error = Some("Save or discard the source draft before switching goals.".into());
            cx.notify();
            return;
        }
        self.reset_todoist_picker();
        self.attention_target = None;
        self.opened_attention = None;
        let old = self.goal_id();
        self.goal_drafts.insert(
            old,
            (
                self.compose.read(cx).value().to_string(),
                self.next_step.read(cx).value().to_string(),
            ),
        );
        self.pending_capture_id = None;
        self.error = None;
        self.batch(vec![json!({"op":"snapshot","goal_id":id})], window, cx);
    }
    fn open_attention(&mut self, item: Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_guard_navigation(PendingNavigation::Attention(item.clone()), cx) {
            return;
        }
        if self.busy {
            return;
        }
        if self.dirty(cx) {
            self.pending_navigation = Some(PendingNavigation::Attention(item));
            self.surface = Surface::Source;
            self.show_capture = false;
            cx.notify();
            return;
        }
        let Some(goal_id) = item["goal_id"].as_str().map(str::to_owned) else {
            return;
        };
        self.reset_todoist_picker();
        self.attention_target = Some(item);
        self.pending_capture_id = None;
        self.show_capture = false;
        self.error = None;
        self.batch(vec![json!({"op":"snapshot","goal_id":goal_id})], window, cx);
    }
    fn apply_attention_target(&mut self) {
        let Some(item) = self.attention_target.take() else {
            return;
        };
        if item["goal_id"] != self.snapshot["goal"]["id"] {
            self.error =
                Some("Attention target could not be opened: the goal response changed.".into());
            return;
        }
        self.opened_attention = Some(item.clone());
        let current = array(&self.snapshot["attention"])
            .iter()
            .any(|a| a["id"] == item["attention_id"]);
        let same_stage =
            item["stage_id"].is_string() && item["stage_id"] == self.snapshot["stage"]["id"];
        self.show_history = !current || !same_stage;
        self.surface = if current && same_stage && item["kind"] == "final" {
            Surface::Outcome
        } else {
            Surface::Details
        };
        if !current {
            self.notice = "This attention item was resolved. Showing its goal and history.".into();
        }
    }
    fn persist_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.goal_id().is_empty() {
            return;
        }
        self.batch(
            vec![json!({"op":"goal_selection","goal_id":self.goal_id(),
            "source_paths":self.selected_sources,"conversation_id":self.conversation_id})],
            window,
            cx,
        );
    }
    fn capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_capture_id.is_some() {
            self.refresh(window, cx);
            return;
        }
        if self.dirty(cx) {
            self.error =
                Some("Save or discard the source draft before creating another goal.".into());
            cx.notify();
            return;
        }
        let title = self.title.read(cx).value().trim().to_string();
        let criteria:Vec<_>=self.criteria.read(cx).value().lines().map(str::trim).filter(|v|!v.is_empty())
            .map(|description|json!({"id":uuid(),"description":description,"requires_human":self.needs_human})).collect();
        if title.is_empty() || criteria.is_empty() {
            self.error = Some("Add a goal and at least one observable outcome.".into());
            cx.notify();
            return;
        }
        let id = uuid();
        self.goal_drafts.insert(
            self.goal_id(),
            (
                self.compose.read(cx).value().to_string(),
                self.next_step.read(cx).value().to_string(),
            ),
        );
        self.pending_capture_fields = Some((
            self.title.read(cx).value().to_string(),
            self.criteria.read(cx).value().to_string(),
        ));
        self.pending_capture_id = Some(id.clone());
        self.batch(vec![json!({"op":"create_goal","goal":{"id":id,"title":title,"status":"active","criteria":criteria,"stage_ids":[],"task_ref":null},"body":format!("\n# {title}\n")})],window,cx);
    }
    fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.discussion_send_enabled() {
            return;
        }
        if self.capabilities["discussion_send_recovery"] == true {
            self.send_correlated_discussion(window, cx);
            return;
        }
        let message = self.compose.read(cx).value().trim().to_string();
        if message.is_empty() {
            return;
        }
        if self.new_conversation.drafting(&self.goal_id()) {
            self.new_conversation.pending.insert(self.goal_id());
        }
        self.pending_message = Some((message.clone(), array(&self.conversation["messages"]).len()));
        self.batch(vec![json!({"op":"chat_start","goal_id":self.goal_id(),"message":message,"source_paths":self.selected_sources,"conversation_id":self.conversation_id})],window,cx);
    }
    fn open_source(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_guard_navigation(PendingNavigation::Source(path.clone()), cx) {
            return;
        }
        // Navigation must not enter loading state when batch cannot start.
        if self.busy {
            return;
        }
        if self.dirty(cx) {
            self.pending_navigation = Some(PendingNavigation::Source(path));
            self.surface = Surface::Source;
            self.show_capture = false;
            self.error =
                Some("Save or discard the current changes before opening another note.".into());
            cx.notify();
            return;
        }
        self.surface = Surface::Source;
        self.source_loading = true;
        self.sync_source_policy(cx);
        self.batch(vec![json!({"op":"source_read","path":path})], window, cx);
    }
    fn load_source(&mut self, data: Value, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate_source_find(cx);
        self.editor_detach();
        self.source_conflict = None;
        self.pending_source_write = None;
        self.source_loading = false;
        self.sync_source_policy(cx);
        self.source_editable = false;
        match decode_source(&data) {
            Ok(value) => {
                self.source_navigation_loaded(&text(&data["path"]));
                self.source_original = value.clone();
                self.source.reset(value, window, cx);
                // Runtime positive check against the actual native input: if a
                // vendor change normalizes BOM/newlines, do not allow a lossy save.
                self.source_editable = self.source.value(cx).as_ref() == self.source_original;
                if !self.source_editable {
                    self.error=Some("The editor changed this file's text representation. Original bytes are retained; editing is disabled.".into());
                }
                self.source_snapshot = Some(data);
            }
            Err(error) => {
                self.error = Some(error.into());
                self.source_snapshot = None;
                self.source_original.clear();
            }
        }
        self.preview = Value::Null;
        self.schedule_preview(window, cx);
    }
    fn show_source_conflict(
        &mut self,
        conflict: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.source_loading = false;
        self.sync_source_policy(cx);
        let Some(source) = &self.source_snapshot else {
            return;
        };
        if conflict["conflict"]["path"] != source["path"]
            || conflict["proposed"]["brain_id"] != source["brain_id"]
            || conflict["proposed"]["path"] != source["path"]
        {
            self.error =
                Some("Conflict reply does not match the open source. Draft retained.".into());
            return;
        }
        let displayed_base = self.editor_conflict_base(&conflict).clone();
        let current = decode_source(&conflict["current"]);
        self.source_current.update(cx, |input, cx| {
            input.set_value(
                current.unwrap_or_else(|_| {
                    "Current source is missing or cannot be edited as UTF-8.".into()
                }),
                window,
                cx,
            )
        });
        self.source_base.update(cx, |input, cx| {
            input.set_value(
                decode_source(&displayed_base)
                    .unwrap_or_else(|_| "No retained base is available.".into()),
                window,
                cx,
            )
        });
        self.source_conflict = Some(conflict);
        self.error = None;
        self.notice = "Source conflict — your draft is preserved".into();
    }

    fn resolve_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.source_loading || self.editor_blocks_navigation() {
            return;
        }
        let Some(conflict) = &self.source_conflict else {
            return;
        };
        if decode_source(&conflict["current"]).is_err() {
            return;
        }
        let request = source_write_reusing(
            &conflict["current"],
            &self.source.value(cx),
            self.pending_source_write.as_ref(),
        );
        if self.editor_enabled() {
            self.editor_save(request, window, cx);
            return;
        }
        self.pending_source_write = Some(request.clone());
        self.source_loading = true;
        self.sync_source_policy(cx);
        self.batch(vec![request], window, cx);
    }

    fn schedule_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.schedule_source_projection(window, cx);
        self.sync_source_policy(cx);
        self.preview_generation += 1;
        self.clear_open_link_preview();
        let preview_owner = self.open_link_preview_owner(cx);
        let generation = self.preview_generation;
        let Some(snapshot) = &self.source_snapshot else {
            self.preview = Value::Null;
            self.preview_images.clear();
            self.preview_loading = false;
            return;
        };
        let path = text(&snapshot["path"]);
        let content = self.source.value(cx).to_string();
        let endpoint = self.endpoint;
        let expected_workspace = self.expected_workspace.clone();
        self.preview_loading = true;
        self.preview_error = None;
        self.link_candidates.clear();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let current = this.update_in(cx, |this, _, _| this.preview_generation == generation).unwrap_or(false);
            if !current { return; }
            let reply = cx.background_executor().spawn(async move {
                let data = rpc_guarded(endpoint, json!({"op":"source_preview","path":path,"content_base64":STANDARD.encode(content.as_bytes())}), expected_workspace.as_ref())?;
                if data["path"] != path || !data["markdown"].is_string() {
                    return Err("Preview reply does not match the open source.".to_string());
                }
                let images = preview_images(&data)?;
                Ok((data, images))
            }).await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.preview_generation != generation { return; }
                this.preview_loading = false;
                match reply {
                    Ok((data, images)) => { this.accept_open_link_preview(preview_owner, generation, cx); this.preview = data; this.preview_images = images; }
                    Err(error) => { this.preview_error = Some(error); this.preview = Value::Null; this.preview_images.clear(); }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
    fn preview_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.attachment_preview = None;
        if self.preview_loading {
            return;
        }
        if url.starts_with("https://") || url.starts_with("http://") {
            cx.open_url(url);
            return;
        }
        if self.source.managed().is_some()
            && self.source_snapshot.as_ref().is_none_or(|source| {
                self.preview["path"] != source["path"]
                    || self.preview["revision"] != source["revision"]
                    || self.preview["preview_revision"]
                        != note_link::digest(self.source.value(cx).as_bytes())
            })
        {
            self.error =
                Some("The rendered preview is stale. Refresh it before opening this link.".into());
            cx.notify();
            return;
        }
        if self.preview["document_links_version"] != 1 {
            self.error = Some("This backend does not support consistent document links. Update the backend; source is unchanged.".into());
            cx.notify();
            return;
        }
        let matching: Vec<_> = array(&self.preview["links"])
            .into_iter()
            .filter(|link| link["url"] == url)
            .collect();
        if matching.windows(2).any(|rows| {
            rows[0]["status"] != rows[1]["status"]
                || rows[0]["candidates"] != rows[1]["candidates"]
                || rows[0]["heading"] != rows[1]["heading"]
        }) {
            self.error = Some("The preview has conflicting destinations for this link. Refresh it before navigating.".into());
            cx.notify();
            return;
        }
        let states = super::prepared_links::managed_states(
            &self.preview,
            !self.preview_loading && self.preview_error.is_none(),
        );
        if states
            .get(url)
            .is_some_and(|state| state.status.is_missing())
        {
            return;
        }
        if let Some(state) = states.get(url).filter(|state| {
            state.status == tessera_core::document_links::prepared::LinkStatus::Unknown
        }) {
            self.error = Some(state.reason.clone());
            cx.notify();
            return;
        }
        let link = matching.into_iter().next();
        match link.as_ref().and_then(|link| link["status"].as_str()) {
            Some("attachment") => {
                self.open_preview_attachment(link.as_ref().unwrap(), cx);
            }
            Some("resolved" | "resolved_heading") => {
                let candidates = array(&link.as_ref().unwrap()["candidates"]);
                if candidates.len() == 1 {
                    let heading = link.as_ref().unwrap()["heading"]
                        .as_str()
                        .map(str::to_owned);
                    self.open_source_link_at(text(&candidates[0]["path"]), heading, window, cx);
                } else {
                    self.error = Some("The note link does not have a unique destination.".into());
                }
            }
            Some("ambiguous" | "ambiguous_heading") => {
                let link = link.unwrap();
                self.link_candidates = array(&link["candidates"])
                    .into_iter()
                    .map(|mut c| {
                        c["heading"] = link["heading"].clone();
                        c
                    })
                    .collect();
            }
            _ => {
                self.error = Some(
                    link.as_ref()
                        .and_then(|l| l["reason"].as_str())
                        .unwrap_or("This document link has no readable supported destination.")
                        .into(),
                )
            }
        }
        cx.notify();
    }
    fn open_preview_attachment(&mut self, row: &Value, cx: &mut Context<Self>) {
        self.attachment_preview = None;
        let asset_url = text(&row["asset_url"]);
        let revision = text(&row["asset_revision"]);
        let image = self.preview_images.get(&asset_url).cloned();
        if self.preview["attachment_links_version"] != 1
            || revision
                .strip_prefix("sha256:")
                .is_none_or(|hash| asset_url != format!("tessera-asset://{hash}"))
        {
            self.error =
                Some("This image preview is unavailable. Refresh the note to try again.".into());
        } else if let Some(image) = image {
            let title = row["candidates"][0]["title"].as_str().unwrap_or("Image");
            self.attachment_preview = Some((title.to_owned(), image));
            self.error = None;
        } else {
            self.error =
                Some("This image preview is unavailable. Refresh the note to try again.".into());
        }
        cx.notify();
    }
    fn save_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_guarded_save() {
            return;
        }
        // A retained conflict requires the distinct inspect-and-resolve action;
        // ordinary Save must never silently use its newer current snapshot.
        if self.source_conflict.is_some() {
            return;
        }
        let Some(source) = &self.source_snapshot else {
            return;
        };
        let value = self.source.value(cx);
        let request = source_write_reusing(source, &value, self.pending_source_write.as_ref());
        if self.editor_enabled() {
            self.editor_save(request, window, cx);
            return;
        }
        self.source_loading = true;
        self.sync_source_policy(cx);
        self.pending_source_write = Some(request.clone());
        self.batch(vec![request], window, cx);
    }
    fn create_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let operation = uuid();
        self.pending_task = Some(operation.clone());
        self.batch(vec![json!({"op":"task_create","goal_id":self.goal_id(),"operation_id":operation,"content":self.snapshot["goal"]["title"]})],window,cx);
    }
    fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_step.read(cx).value().trim().to_string();
        if next.is_empty() {
            self.error = Some("Describe the stage before preparing its context.".into());
            cx.notify();
            return;
        }
        let criteria: Vec<_> = array(&self.snapshot["goal"]["criteria"])
            .iter()
            .map(|c| c["id"].clone())
            .collect();
        let previous_result_id = if self.snapshot["phase"] == "discarded" {
            self.snapshot["dispatch"]["packet"]["previous_result_id"].clone()
        } else {
            array(&self.snapshot["stage"]["result_ids"])
                .last()
                .cloned()
                .unwrap_or(Value::Null)
        };
        let reviewed = match self.reviewed_context(cx) {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error);
                self.open_context(window, cx);
                cx.notify();
                return;
            }
        };
        if !self.snapshot["task"].is_object() && reviewed.is_none() {
            self.error =
                Some("Create or link a task before preparing the selected-source workflow.".into());
            cx.notify();
            return;
        }
        self.batch(vec![json!({"op":"stage_prepare","previous_result_id":previous_result_id,"goal_id":self.goal_id(),"conversation_id":self.conversation_id,"source_paths":self.selected_sources,"criterion_ids":criteria,"next_step":next,"reviewed_packet":reviewed})],window,cx);
    }
    fn begin_prepared_edit(&mut self, discard: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !discard && self.snapshot["dispatch"]["packet"]["reviewed_packet"].is_object() {
            self.error=Some("This stage uses reviewed context. Discard the prepared stage, update Context, then prepare it again.".into());
            cx.notify();
            return;
        }
        if self.busy
            || self.snapshot["can_change_prepared"] != true
            || self.capabilities["prepared_stage_edit"] != true
            || !self.snapshot["prepared_guard"].is_object()
        {
            return;
        }
        let goal = self.goal_id();
        if self.pending_prepared_changes.contains_key(&goal) {
            return;
        }
        let next_step = text(&self.snapshot["dispatch"]["packet"]["next_step"]);
        self.prepared_edits
            .entry(goal)
            .or_insert_with(|| PreparedEdit {
                expected: self.snapshot["prepared_guard"].clone(),
                draft: cx.new(|cx| {
                    let mut state = TextareaState::new(window, cx).rows(4);
                    state.set_value(next_step, window, cx);
                    state
                }),
                discard,
            });
        cx.notify();
    }

    fn submit_prepared_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let goal = self.goal_id();
        let Some(edit) = self.prepared_edits.get(&goal) else {
            return;
        };
        if self.pending_prepared_changes.contains_key(&goal) {
            return;
        }
        if self.snapshot["can_change_prepared"] != true
            || self.snapshot["prepared_guard"] != edit.expected
        {
            self.error = Some("This prepared stage changed. Inspect the current context before using it for your draft.".into());
            cx.notify();
            return;
        }
        let mut request = json!({"op":if edit.discard {"stage_discard"} else {"stage_revise"},
            "goal_id":goal,"operation_id":uuid(),"expected":edit.expected});
        if !edit.discard {
            let next_step = edit.draft.read(cx).value().to_string();
            if next_step.trim().is_empty() {
                self.error = Some("Describe the revised stage before saving it.".into());
                cx.notify();
                return;
            }
            request["next_step"] = json!(next_step);
        }
        let Some(journal) = &self.prepared_journal else {
            self.error =
                Some("Reconnect the saved workspace before changing a prepared stage.".into());
            cx.notify();
            return;
        };
        if let Err(error) = journal.write_once(&request, "json") {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.pending_prepared_changes.insert(goal, request.clone());
        self.batch(vec![request], window, cx);
    }

    fn restore_prepared_edit(
        &mut self,
        request: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let goal = text(&request["goal_id"]);
        self.prepared_edits
            .entry(goal)
            .or_insert_with(|| PreparedEdit {
                expected: request["expected"].clone(),
                draft: cx.new(|cx| {
                    let mut state = TextareaState::new(window, cx).rows(4);
                    state.set_value(text(&request["next_step"]), window, cx);
                    state
                }),
                discard: request["op"] == "stage_discard",
            });
    }

    fn reload_prepared_operations(&mut self) {
        if let Some(journal) = &self.prepared_journal {
            match journal.pending() {
                Ok(pending) => self.pending_prepared_changes = pending,
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn acknowledge_prepared_change(
        &mut self,
        data: &Value,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let receipt = &data["prepared_change"];
        let goal = text(&receipt["goal_id"]);
        let Some(request) = self.pending_prepared_changes.get(&goal) else {
            self.error = Some("The prepared-stage reply has no matching local operation. Refresh to inspect the current stage.".into());
            return false;
        };
        let expected_action = if request["op"] == "stage_discard" {
            "discard"
        } else {
            "revise"
        };
        if receipt["operation_id"] != request["operation_id"]
            || receipt["previous"] != request["expected"]
            || receipt["action"] != expected_action
            || data["goal"]["id"] != goal
        {
            self.error = Some("The prepared-stage acknowledgement does not match the submitted operation. Retry the original request.".into());
            return false;
        }
        let Some(journal) = &self.prepared_journal else {
            return false;
        };
        if let Err(error) = journal.write_once(request, "done") {
            self.error = Some(error);
            return false;
        }
        let late_draft = self.prepared_edits.get(&goal).is_some_and(|edit| {
            !edit.discard && edit.draft.read(cx).value().as_ref() != text(&request["next_step"])
        });
        if late_draft {
            if let Some(edit) = self.prepared_edits.get_mut(&goal) {
                edit.expected = receipt["replacement"].clone();
            }
            self.notice = "Prepared stage updated; your later draft is still open.".into();
        } else {
            self.prepared_edits.remove(&goal);
        }
        self.pending_prepared_changes.remove(&goal);
        self.reload_prepared_operations();
        true
    }

    fn evaluate(
        &mut self,
        criterion: String,
        status: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_evidence.is_empty() {
            self.error = Some("Select the evidence that supports this criterion.".into());
            cx.notify();
            return;
        }
        self.batch(vec![json!({"op":"criterion_evaluate","result_id":self.result["id"],"criterion_id":criterion,"evidence_ids":self.selected_evidence,"status":status,"evaluated_by":self.capabilities["actor"],"evaluated_at":now()})],window,cx);
    }
    fn accept(&mut self, criterion: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.goal_completed() {
            return;
        }
        let path = text(&self.snapshot["source_paths"]["result"]);
        if path.is_empty() {
            self.error = Some("The saved result must be available before confirming it.".into());
            cx.notify();
            return;
        }
        let Some(source) = self.source_snapshot.as_ref().filter(|s| s["path"] == path) else {
            self.error = Some("Open the saved result and review it before confirming.".into());
            cx.notify();
            return;
        };
        if self.dirty(cx) || self.source_loading {
            self.error =
                Some("Review the saved result without unsaved changes before confirming.".into());
            cx.notify();
            return;
        }
        let uri = format!("brain://{}/{}", text(&source["brain_id"]), path);
        self.batch(vec![json!({"op":"accept_human","criterion_id":criterion,"actor":self.capabilities["actor"],"observed_at":now(),"source":{"uri":uri,"revision":source["revision"],"locator":criterion}})],window,cx);
    }
    fn goal_completed(&self) -> bool {
        // The backend validates this status against the current criterion
        // definitions. A historical verified result or completed stage alone
        // does not mean a reopened goal is still complete.
        self.snapshot["goal"]["status"] == "completed"
    }
    pub(crate) fn search_input(&self) -> Entity<InputState> {
        self.filter.clone()
    }
    pub(crate) fn collection(&self) -> Collection {
        self.collection
    }
    pub(crate) fn select_collection(
        &mut self,
        collection: Collection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.note_guard_navigation(PendingNavigation::Collection(collection), cx) {
            return;
        }
        self.proposals.leave();
        self.reset_todoist_picker();
        self.cancel_source_navigation();
        let changed = self.collection != collection;
        if changed {
            self.clear_open_link_preview();
        }
        self.collection = collection;
        if changed
            && collection == Collection::Sources
            && self.surface == Surface::Source
            && !self.show_capture
            && self.source.managed().is_some()
            && self.source_snapshot.is_some()
        {
            self.schedule_preview(window, cx);
        }
        if collection == Collection::Inbox {
            self.ensure_inbox(window, cx);
            self.show_capture = false;
            self.refresh_inbox(false, window, cx);
        }
        if collection == Collection::Attention && self.capabilities["attention_read"] == true {
            self.show_capture = false;
            self.refresh_bound_attention(false, window, cx);
        } else if collection == Collection::Attention
            && self.capabilities["workspace_attention"] == true
        {
            self.batch(vec![json!({"op":"workspace_attention"})], window, cx);
        }
        self.filter.update(cx, |input, cx| {
            input.set_placeholder(
                if collection == Collection::Inbox {
                    "Search inbox…"
                } else if collection == Collection::Sources {
                    "Search source notes…"
                } else {
                    "Search goals…"
                },
                window,
                cx,
            )
        });
        cx.notify();
    }
    pub(crate) fn begin_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_guard_navigation(PendingNavigation::Capture, cx) {
            return;
        }
        if self.busy {
            return;
        }
        if self.dirty(cx) {
            self.pending_navigation = Some(PendingNavigation::Capture);
            self.surface = Surface::Source;
            self.show_capture = false;
            cx.notify();
            return;
        }
        self.reset_todoist_picker();
        self.cancel_source_navigation();
        self.clear_open_link_preview();
        self.show_capture = true;
        self.title.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    pub(crate) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.read(cx).focus_handle(cx).focus(window, cx);
    }
    fn continue_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation_after_source = false;
        if self.dirty(cx) || self.busy {
            return;
        }
        self.error = None;
        match self.pending_navigation.take() {
            Some(PendingNavigation::Goal(id)) => self.select_goal(id, window, cx),
            Some(PendingNavigation::GoalCriteria) => self.open_goal_criteria(window, cx),
            Some(PendingNavigation::DecisionReuse(id)) => self.open_decision_reuse(id, window, cx),
            Some(PendingNavigation::Attention(item)) => self.open_attention(item, window, cx),
            Some(PendingNavigation::Source(path)) => self.open_source(path, window, cx),
            Some(PendingNavigation::SourceLink(path, heading)) => {
                self.open_source_link_at(path, heading, window, cx)
            }
            Some(PendingNavigation::SourceBack) => self.back_source(window, cx),
            Some(PendingNavigation::Capture) => self.begin_capture(window, cx),
            Some(PendingNavigation::DiscussionNote(selection)) => {
                self.prepare_discussion_note(selection, window, cx)
            }
            Some(PendingNavigation::NewNote(target, endpoint)) => {
                self.prepare_new_note(target, endpoint, window, cx)
            }
            Some(PendingNavigation::Collection(collection)) => {
                self.select_collection(collection, window, cx)
            }
            Some(PendingNavigation::Surface(surface)) => {
                self.note_navigate_surface(surface, window, cx)
            }
            Some(PendingNavigation::Conversation(id)) => {
                self.note_navigate_conversation(id, window, cx)
            }
            Some(PendingNavigation::Thought) => self.begin_thought(window, cx),
            None => {}
        }
    }
    fn save_and_continue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_closing()
            || self.busy
            || self.source_conflict.is_some()
            || self.editor_guarded_save()
        {
            return;
        }
        self.navigation_after_source = true;
        self.save_source(window, cx);
    }
    fn discard_and_continue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_closing() || self.busy {
            return;
        }
        self.navigation_after_source = true;
        self.editor_discard(window, cx);
    }

    fn navigation_prompt(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let colors = super::brand::palette(cx);
        let path = self
            .source_snapshot
            .as_ref()
            .map(|s| text(&s["path"]))
            .unwrap_or_default();
        v_flex().id("dirty-navigation").p_4().gap_2().border_b_1().border_color(colors.warning).bg(colors.surface_raised)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Unsaved source changes"))
            .child(div().text_sm().child(format!("{path} has an unsaved draft. Stay here, save it, or discard only this draft before continuing.")))
            .child(h_flex().gap_2().flex_wrap()
                .child(super::brand::button("navigation-stay","Keep editing",super::brand::ButtonKind::Secondary,cx).on_click(cx.listener(|this,_,_,cx| {this.pending_navigation=None;this.navigation_after_source=false;this.error=None;cx.notify();})))
                .child(super::brand::control("navigation-save",cx).primary().label("Save and continue").disabled(self.busy || self.editor_guarded_save() || self.source_conflict.is_some() || self.capabilities["source_write"] != true).on_click(cx.listener(|this,_,window,cx|this.save_and_continue(window,cx))))
                .child(super::brand::button("navigation-discard","Discard draft and continue",super::brand::ButtonKind::Danger,cx).disabled(self.busy).on_click(cx.listener(|this,_,window,cx|this.discard_and_continue(window,cx)))))
            .into_any_element()
    }
    fn sidebar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = super::brand::palette(cx);
        let title = match self.collection {
            Collection::Inbox => "Inbox",
            Collection::Attention => "Attention",
            Collection::Goals => "All goals",
            Collection::Sources => "Project brain",
        };
        let query = self.filter.read(cx).value().to_lowercase();
        let mut rows = v_flex()
            .id("brain-master-rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .gap_1();
        if self.collection == Collection::Inbox {
            rows = rows.child(self.inbox_sidebar(cx));
        } else if self.collection == Collection::Attention && self.use_bound_attention() {
            rows = rows
                .child(self.bound_attention_sidebar(cx))
                .child(self.proposals_sidebar(cx));
        } else if self.collection == Collection::Attention {
            let freshness = if let Some(error) = &self.attention_error {
                format!(
                    "Refresh failed: {error}. Saved overview may be stale; use Refresh to retry."
                )
            } else if self.capabilities.is_null() {
                "Connecting to workspace attention…".into()
            } else if self.capabilities["workspace_attention"] != true {
                "Workspace overview unavailable. Refresh the connection or update the backend; inspect All goals in the meantime.".into()
            } else if self.workspace_attention.is_null() {
                "Loading attention across all goals…".into()
            } else {
                format!(
                    "All {} goals · checked {} · backend state",
                    self.workspace_attention["goal_count"],
                    text(&self.workspace_attention["observed_at"])
                )
            };
            rows = rows.child(
                div()
                    .p_3()
                    .text_xs()
                    .text_color(if self.attention_error.is_some() {
                        colors.warning
                    } else {
                        colors.text_muted
                    })
                    .child(freshness),
            );
            let mut count = 0;
            for item in array(&self.workspace_attention["items"]) {
                let title = text(&item["goal_title"]);
                let message = text(&item["message"]);
                if !format!("{title} {message}").to_lowercase().contains(&query) {
                    continue;
                }
                count += 1;
                let key = format!(
                    "attention-{}-{}-{}",
                    text(&item["goal_id"]),
                    text(&item["attention_id"]),
                    text(&item["stage_id"])
                );
                let selected = item["goal_id"] == self.goal_id();
                rows = rows.child(
                    super::brand::control(SharedString::from(key.clone()), cx)
                        .debug_selector(move || key.clone())
                        .w_full()
                        .h_auto()
                        .py_3()
                        .justify_start()
                        .when(selected, |b| b.bg(colors.selected))
                        .child(
                            v_flex()
                                .flex_1()
                                .w_full()
                                .items_start()
                                .text_left()
                                .whitespace_normal()
                                .gap_1()
                                .min_w_0()
                                .child(div().w_full().font_weight(FontWeight::MEDIUM).child(title))
                                .child(div().text_xs().text_color(colors.text_muted).child(
                                    if item["kind"] == "final" {
                                        "Result".into()
                                    } else {
                                        text(&item["kind"])
                                    },
                                ))
                                .child(div().text_sm().child(message)),
                        )
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_attention(item.clone(), window, cx)
                        })),
                );
            }
            if count == 0 && self.workspace_attention.is_object() {
                rows = rows.child(div().p_3().text_sm().text_color(colors.text_muted).child(
                    if self.attention_unknown() {
                        "No items in the last saved overview; current attention is unknown."
                    } else if query.is_empty() {
                        "No current blockers, decisions or results across this workspace."
                    } else {
                        "No matching attention items."
                    },
                ));
            }
        } else if self.collection == Collection::Goals {
            let goals = array(&self.snapshot["goals"]);
            let mut count = 0;
            for goal in goals {
                let id = text(&goal["id"]);
                let title = text(&goal["title"]);
                if !title.to_lowercase().contains(&query) {
                    continue;
                }
                count += 1;
                let selected = id == self.goal_id();
                let status = text(&goal["status"]);
                rows = rows.child(
                    super::brand::control(SharedString::from(format!("select-goal-{id}")), cx)
                        .w_full()
                        .h_auto()
                        .py_3()
                        .justify_start()
                        .when(selected, |b| b.bg(colors.selected))
                        .child(
                            v_flex()
                                .flex_1()
                                .w_full()
                                .items_start()
                                .text_left()
                                .whitespace_normal()
                                .gap_1()
                                .min_w_0()
                                .child(div().w_full().font_weight(FontWeight::MEDIUM).child(title))
                                .child(div().text_xs().text_color(colors.text_muted).child(status)),
                        )
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_goal(id.clone(), window, cx)
                        })),
                );
            }
            if count == 0 {
                rows = rows.child(div().p_3().text_sm().text_color(colors.text_muted).child(
                    if self.snapshot.is_null() {
                        "Waiting for the backend. Saved content will appear after connection."
                    } else if query.is_empty() {
                        "Capture your first thought."
                    } else {
                        "No matching goals."
                    },
                ));
            }
        } else {
            rows = rows.child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .child(
                        super::brand::control("source-new-note", cx)
                            .label("New note")
                            .disabled(
                                self.busy
                                    || self.discussion_note.active()
                                    || self.discussion_decision.active
                                    || self.goal_id().is_empty()
                                    || !self.note_writable(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_new_note(window, cx)),
                            ),
                    )
                    .when(self.goal_id().is_empty(), |row| {
                        row.child(
                            div()
                                .text_sm()
                                .text_color(colors.text_muted)
                                .child("Select a goal to create a note."),
                        )
                    })
                    .when(!self.goal_id().is_empty() && !self.note_writable(), |row| {
                        row.child(
                            div()
                                .text_sm()
                                .text_color(colors.text_muted)
                                .child("Creating notes requires a writable managed workspace."),
                        )
                    }),
            );
            for item in self
                .source_list
                .iter()
                .filter(|item| {
                    text(&item["path"]).to_lowercase().contains(&query)
                        || text(&item["title"]).to_lowercase().contains(&query)
                })
                .take(100)
            {
                let path = text(&item["path"]);
                let open = path.clone();
                let included = self.selected_sources.contains(&path);
                let selected = self
                    .source_snapshot
                    .as_ref()
                    .is_some_and(|s| s["path"] == path);
                rows = rows.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .p_2()
                        .gap_1()
                        .rounded(px(8.))
                        .when(selected, |r| r.bg(colors.selected))
                        .child(
                            super::brand::control(SharedString::from(format!("source-{path}")), cx)
                                .ghost()
                                .justify_start()
                                .w_full()
                                .h_auto()
                                .accessibility_label(text(&item["title"]))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .text_left()
                                        .child(text(&item["title"])),
                                )
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_source(open.clone(), window, cx)
                                })),
                        )
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .whitespace_normal()
                                .font_family(super::brand::MONO_FONT)
                                .text_xs()
                                .text_color(colors.text_muted)
                                .child(path.clone()),
                        )
                        .child(
                            super::brand::control(
                                SharedString::from(format!("include-{path}")),
                                cx,
                            )
                            .ghost()
                            .label(if included {
                                "Included in goal"
                            } else {
                                "Include in goal"
                            })
                            .disabled(self.busy || self.goal_id().is_empty())
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    if !this.selected_sources.remove(&path) {
                                        this.selected_sources.insert(path.clone());
                                    }
                                    this.persist_selection(window, cx);
                                    cx.notify();
                                },
                            )),
                        ),
                );
            }
        }
        v_flex()
            .id("brain-master-list")
            .debug_selector(|| "brain-master-list".into())
            .w(px(if _window.viewport_size().width < px(1100.) {
                232.
            } else {
                272.
            }))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .bg(colors.surface)
            .border_r_1()
            .border_color(colors.border_subtle)
            .child(
                div()
                    .px_5()
                    .py_4()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when(!self.embedded, |p| p.child(Input::new(&self.filter)))
            .child(rows)
            .child(
                div()
                    .p_3()
                    .border_t_1()
                    .border_color(colors.border_subtle)
                    .text_xs()
                    .text_color(colors.text_muted)
                    .child(if self.collection == Collection::Inbox {
                        "Thoughts saved independently of goals"
                    } else if self.collection == Collection::Sources {
                        "Saved Markdown · select sources for this goal"
                    } else {
                        "Select a goal to keep its context together"
                    }),
            )
            .into_any_element()
    }
    fn capture_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex().id("brain-capture").flex_1().min_h_0().overflow_y_scroll().p_6().gap_4()
            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child("Create a goal"))
            .child("Give it an outcome you can recognise. You can discuss the plan before starting work.")
            .child(Input::new(&self.title).disabled(self.busy))
            .child(Textarea::new(&self.criteria).disabled(self.busy))
            .child(super::brand::control("human-toggle", cx).ghost().label(if self.needs_human { "Requires my confirmation: yes" } else { "Requires my confirmation: no" })
                .on_click(cx.listener(|this, _, _, cx| { this.needs_human = !this.needs_human; cx.notify(); })))
            .child(h_flex().gap_2().child(super::brand::control("capture", cx).primary().label("Capture goal")
                .disabled(self.busy || self.pending_capture_id.is_some()).on_click(cx.listener(|this, _, window, cx| this.capture(window, cx))))
                .child(super::brand::control("close-capture", cx).ghost().label("Keep for later").on_click(cx.listener(|this, _, _, cx| { this.show_capture = false; cx.notify(); }))))
            .into_any_element()
    }
    fn center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = super::brand::palette(cx);
        let mut tabs = h_flex()
            .gap_2()
            .flex_wrap()
            .border_b_1()
            .border_color(colors.border_subtle)
            .pb_2();
        for (id, label, surface) in [
            ("conversation-tab", "Discussion", Surface::Conversation),
            ("context-tab", "Context", Surface::Context),
            ("execution-tab", "Execution", Surface::Execution),
            ("outcome-tab", "Outcome", Surface::Outcome),
            ("details-tab", "Details", Surface::Details),
            ("source-tab", "Source & preview", Surface::Source),
        ] {
            tabs = tabs.child(
                super::brand::control(id, cx)
                    .ghost()
                    .label(label)
                    .when(self.surface == surface, |b| {
                        b.bg(colors.selected).text_color(colors.link)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.note_navigate_surface(surface, window, cx);
                    })),
            );
        }
        let center = v_flex()
            .id("brain-center")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .overflow_hidden()
            .p_6()
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text(&self.snapshot["goal"]["title"])),
                    )
                    .child(div().text_sm().text_color(colors.text_muted).child(format!(
                        "{} · {} discussion sources",
                        text(&self.snapshot["goal"]["status"]),
                        self.selected_sources.len()
                    ))),
            )
            .child(tabs);
        let content = match self.surface {
            Surface::Conversation => self.discussion(window, cx),
            Surface::Source => self.source_detail(window, cx),
            Surface::Context => self.context_panel(window, cx),
            _ => self.execution(window, cx),
        };
        center.child(content).into_any_element()
    }
    fn discussion(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme_snapshot = cx.theme().clone();
        let theme = &theme_snapshot;
        let colors = super::brand::palette(cx);
        let mut center = v_flex()
            .id("brain-discussion")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap_3();

        let conversations = array(&self.snapshot["conversations"]);
        center = center.child(self.new_conversation_controls(cx));
        if conversations.len() > 1
            || self.new_conversation.drafting(&self.goal_id())
            || self.new_conversation.pending.contains(&self.goal_id())
        {
            let mut choices = h_flex().gap_2().flex_wrap();
            for (index, conversation) in conversations.iter().enumerate() {
                let id = text(&conversation["id"]);
                let selected = self.conversation_id.as_ref() == Some(&id);
                choices = choices.child(
                    super::brand::control(("choose-conversation", index), cx)
                        .label(format!(
                            "Conversation {} · {}",
                            index + 1,
                            text(&conversation["status"])
                        ))
                        .small()
                        .disabled(self.busy)
                        .when(selected, |b| b.primary())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.note_navigate_conversation(id.clone(), window, cx);
                        })),
                );
            }
            center = center.child(choices);
        }
        let mut history = v_flex()
            .id("brain-transcript")
            .debug_selector(|| "brain-transcript".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_4();
        let origin = &self.snapshot["goal"]["origin_inbox"];
        if origin["schema"] == "ai-brain/inbox-origin-v1" {
            history = history.child(
                v_flex()
                    .id("goal-inbox-origin")
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .child("Original thought — operator input, unverified"),
                    )
                    .child(
                        TextView::markdown("goal-inbox-origin-text", text(&origin["text"]))
                            .selectable(true)
                            .style(super::reader_text_style(theme)),
                    )
                    .child(div().text_xs().text_color(colors.text_muted).child(format!(
                        "Pinned from {}. The saved Inbox thought stays unchanged.",
                        text(&origin["path"])
                    ))),
            );
        }
        let messages = array(&self.conversation["messages"]);
        if messages.is_empty() {
            history = history.child(div().text_color(theme.muted_foreground).child(
                if self.goal_id().is_empty() {
                    "Capture a goal, then discuss the outcome and next step."
                } else if self.capabilities["discussion_context"] == true {
                    "Discuss this goal and its next step. Available saved goal inputs are included with your question."
                } else {
                    "Include relevant notes, then discuss this goal and its next step."
                },
            ));
        }
        for (index, message) in messages.into_iter().enumerate() {
            let mut row = v_flex()
                .gap_1()
                .child(div().text_xs().font_weight(FontWeight::BOLD).child(
                    if message["role"] == "user" {
                        "You"
                    } else {
                        "Tessera"
                    },
                ))
                .child(
                    TextView::markdown(("brain-message", index), text(&message["text"]))
                        .selectable(true)
                        .style(super::reader_text_style(theme)),
                );
            if message["role"] == "user" {
                row = row.child(self.discussion_context_row(index, cx));
                if self.capabilities["discussion_decision_save"] == true {
                    row = row.child(
                        super::brand::control(("keep-user-decision", index), cx)
                            .small()
                            .label("Keep as goal decision")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_discussion_decision(index, window, cx)
                            })),
                    );
                }
            }
            if message["role"] == "assistant" && !text(&message["text"]).trim().is_empty() {
                row = row.child(
                    super::brand::control(("save-answer-note", index), cx)
                        .small()
                        .label("Save as note")
                        .disabled(
                            self.busy || self.discussion_note.active() || !self.note_writable(),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_discussion_note(index, window, cx)
                        })),
                );
            }
            history = history.child(row);
        }
        let partial = text(&self.conversation["partial"]);
        if !partial.is_empty() {
            history = history.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .child(if self.conversation["status"] == "running" {
                                "Tessera · in progress"
                            } else {
                                "Tessera · interrupted reply · nothing resent"
                            }),
                    )
                    .child(
                        TextView::markdown("brain-partial", partial)
                            .selectable(true)
                            .style(super::reader_text_style(theme)),
                    ),
            );
        }
        let status = text(&self.conversation["status"]);
        let composer = v_flex()
            .id("brain-composer")
            .debug_selector(|| "brain-composer".into())
            .flex_shrink_0()
            .gap_2()
            .p_3()
            .rounded(px(8.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if self.discussion_guarded() || self.new_conversation.pending.contains(&self.goal_id()) {
                        "Send unconfirmed — draft retained".into()
                    } else if self.new_conversation.drafting(&self.goal_id()) {
                        "Draft — not sent yet".into()
                    } else if status.is_empty() {
                        "Conversation is saved in the brain.".into()
                    } else {
                        format!("Conversation: {status}")
                    }),
            )
            .child(Textarea::new(&self.compose).disabled(self.busy))
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if self.capabilities["discussion_context"] == true {
                                format!(
                                    "Available saved goal inputs are included when you send · {} selected source(s)",
                                    self.selected_sources.len()
                                )
                            } else {
                                format!("{} selected source(s)", self.selected_sources.len())
                            }),
                    )
                    .child(
                        super::brand::control("send-message", cx)
                            .label("Send")
                            .primary()
                            .disabled(
                                !self.discussion_send_enabled(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send_message(window, cx)),
                            ),
                    ),
            );
        center = center
            .child(history)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_start()
                    .gap_3()
                    .flex_shrink_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .text_sm()
                            .text_color(colors.text_muted)
                            .child(next_action(
                                &self.snapshot,
                                &self.conversation,
                                &self.capabilities,
                            )),
                    )
                    .child(
                        super::brand::control("discussion-next-action", cx)
                            .flex_shrink_0()
                            .label(if self.result.is_object() {
                                "Review outcome"
                            } else {
                                "Task & execution"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.surface = if this.result.is_object() {
                                    Surface::Outcome
                                } else {
                                    Surface::Execution
                                };
                                cx.notify();
                            })),
                    ),
            )
            .child(composer);
        if self.capabilities["chat"] == false {
            center = center.child(
                div()
                    .text_sm()
                    .child("Conversation is unavailable until an LLM connection is configured."),
            );
        }
        center.into_any_element()
    }
    fn source_detail(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let recovery_panel = self.editor_enabled().then(|| self.editor_panel(cx));
        let merge_panel = self.editor_merge_panel(cx);
        let note_link_panel = self.note_link_panel(cx);
        let find_panel = self.source_find_panel(cx);
        let source_selection_panel = self.source_selection_context_panel(cx);
        let back_path = self.source_back_path().map(str::to_owned);
        let context_return = self.context_return_available(cx);
        let open_link_panel = self.open_link_panel(cx);
        let theme = cx.theme();
        let colors = super::brand::palette(cx);
        let mut center = v_flex()
            .id("brain-source_detail")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap_3();

        center = center.overflow_y_scroll();
        if let Some(panel) = recovery_panel {
            center = center.child(panel);
        }
        let path = self
            .source_snapshot
            .as_ref()
            .map(|s| text(&s["path"]))
            .unwrap_or_else(|| "Choose a source note".into());
        let dirty = self.dirty(cx);
        center = center.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .when(context_return, |row| {
                    row.child(
                        super::brand::control("context-return", cx)
                            .label("Return to Context")
                            .debug_selector(|| "context-return".into())
                            .disabled(
                                self.busy
                                    || self.source_loading
                                    || self.source_navigation_pending(),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.return_to_context(window, cx)
                            })),
                    )
                })
                .when_some(back_path, |row, path| {
                    row.child(
                        super::brand::control("source-back", cx)
                            .label("Back to previous note")
                            .tooltip(path)
                            .disabled(
                                self.busy
                                    || self.source_loading
                                    || self.source_navigation_pending(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.back_source(window, cx)),
                            ),
                    )
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .min_w_0()
                        .max_w_full()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_family(super::brand::MONO_FONT)
                        .text_sm()
                        .child(path),
                )
                .child(
                    div()
                        .text_xs()
                        .child(if dirty { "Unsaved" } else { "Saved" }),
                )
                .child(
                    super::brand::control("source-find", cx)
                        .label("Find")
                        .disabled(!self.source_find_enabled())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_source_find(window, cx)),
                        ),
                )
                .child(
                    super::brand::control("source-insert-link", cx)
                        .label("Insert note link")
                        .disabled(!self.note_link_enabled())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_note_link(window, cx)),
                        ),
                )
                .child(
                    super::brand::control("source-use-context", cx)
                        .label("Use in Context")
                        .disabled(self.busy || self.source_loading || self.editor_closing())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.use_source_in_context(window, cx)
                        })),
                )
                .child(
                    super::brand::control("source-use-selected-context", cx)
                        .label("Use selected lines in Context")
                        .disabled(self.busy || self.source_loading || self.editor_closing())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_source_selection_context(window, cx)
                        })),
                )
                .child(
                    super::brand::control("save-source", cx)
                        .label("Save")
                        .primary()
                        .disabled(
                            self.busy
                                || self.editor_closing()
                                || self.editor_guarded_save()
                                || !dirty
                                || self.source_conflict.is_some()
                                || self.capabilities["source_write"] != true,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save_source(window, cx);
                        })),
                )
                .child(
                    super::brand::control("discard-source", cx)
                        .label("Discard changes")
                        .disabled(
                            self.busy || self.source_loading || self.editor_closing() || !dirty,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.editor_discard(window, cx);
                        })),
                ),
        );
        if let Some(panel) = source_selection_panel {
            center = center.child(panel);
        }
        if let Some(panel) = note_link_panel {
            center = center.child(panel);
        }
        if let Some(conflict) = &self.source_conflict {
            let can_resolve = decode_source(&conflict["current"]).is_ok();
            center = center.child(v_flex().gap_2().p_4().rounded(px(8.)).bg(colors.surface_raised).border_1().border_color(colors.warning)
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Source changed — your draft is preserved"))
                        .child(div().text_sm().child("Compare the saved version with your draft. Saving the resolved draft replaces the displayed saved version and keeps the conflict evidence."))
                        .child(super::brand::control("resolve-source",cx).debug_selector(||"brain-resolve-source".into()).label("Save resolved draft").primary()
                            .disabled(self.busy || self.source_loading || self.editor_blocks_navigation() || !can_resolve || !self.source_editable).on_click(cx.listener(|this,_,window,cx|this.resolve_source(window,cx)))));
        }
        if let Some(panel) = merge_panel {
            center = center.child(panel);
        }
        let weak = cx.entity().downgrade();
        let images = self.preview_images.clone();
        let states = super::prepared_links::managed_states(
            &self.preview,
            !self.preview_loading
                && self.preview_error.is_none()
                && self.source_snapshot.as_ref().is_some_and(|source| {
                    self.preview["path"] == source["path"]
                        && self.preview["revision"] == source["revision"]
                        && self.preview["preview_revision"]
                            == note_link::digest(self.source.value(cx).as_bytes())
                }),
        );
        let preview = super::markdown_plugins(
            TextView::markdown("brain-source-preview", text(&self.preview["markdown"]))
                .selectable(true)
                .style(super::reader_text_style(theme)),
            Arc::new(move |url, _, window, cx| {
                if let Some(entity) = weak.upgrade() {
                    entity.update(cx, |this, cx| this.preview_link(url, window, cx));
                }
            }),
            Arc::new(move |url| preview_image(&images, url)),
            SelectionFormat::Plain,
        )
        .link_presentation(move |url| super::prepared_links::presentation(url, &states));
        let mut preview_panel = v_flex()
            .id("brain-preview")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        if self.preview_loading {
            preview_panel = preview_panel.child(div().text_xs().child("Updating preview…"));
        }
        if let Some(error) = &self.preview_error {
            preview_panel = preview_panel.child(
                div()
                    .text_sm()
                    .child(format!("Preview unavailable: {error}")),
            );
        }
        for (index, candidate) in self.link_candidates.iter().enumerate() {
            let path = text(&candidate["path"]);
            let heading = candidate["heading"].as_str().map(str::to_owned);
            preview_panel = preview_panel.child(
                super::brand::control(("preview-link-candidate", index), cx)
                    .label(format!("{} · {}", text(&candidate["title"]), path))
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_source_link_at(path.clone(), heading.clone(), window, cx)
                    })),
            );
        }
        if !self.link_candidates.is_empty() {
            preview_panel = preview_panel.child(
                div()
                    .text_xs()
                    .child("Choose a destination for this ambiguous note link."),
            );
        }
        if let Some((title, image)) = &self.attachment_preview {
            preview_panel = preview_panel.child(
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("preview-image-back")
                                    .ghost()
                                    .icon(gpui_component::IconName::ArrowLeft)
                                    .tooltip("Back to note")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.attachment_preview = None;
                                        cx.notify();
                                    })),
                            )
                            .child(div().text_sm().child(title.clone())),
                    )
                    .child(img(image.clone()).w_full().object_fit(ObjectFit::Contain)),
            );
        } else {
            preview_panel = preview_panel.child(preview);
        }
        let editor = v_flex()
            .when_some(open_link_panel, |panel, chooser| panel.child(chooser))
            .when(self.source.managed().is_some(), |panel| {
                panel.child(
                    super::brand::control("source-open-link", cx)
                        .debug_selector(|| "source-open-link".into())
                        .label("Open link at caret")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_link_at_caret(window, cx)),
                        ),
                )
            })
            .when_some(find_panel, |panel, find| panel.child(find))
            .when(self.source.managed().is_some(), |panel| {
                panel.child(
                    Button::new("managed-source-mode")
                        .disabled(self.source_find.active())
                        .label(if self.source_find.active() {
                            "Source · Find"
                        } else if self.source_projection.live {
                            "Live Preview"
                        } else {
                            "Source"
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_source_projection(window, cx)
                        })),
                )
            })
            .when_some(
                self.source_projection.clipboard_error.clone(),
                |panel, message| panel.child(div().text_sm().child(message)),
            )
            .flex_1()
            .min_w(px(270.))
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(if dirty {
                        "Your draft · unsaved"
                    } else {
                        "Markdown source"
                    }),
            )
            .child(
                div()
                    .relative()
                    .font_family(super::brand::MONO_FONT)
                    .text_size(px(13.))
                    .capture_any_mouse_down(cx.listener(|this, _, _, _| {
                        this.cancel_heading_position();
                    }))
                    .child(self.source.render(self.source_readonly()))
                    .when(self.source.managed().is_some(), |editor| {
                        editor.child(self.source_heading_paint_hook(cx))
                    }),
            );
        if self.source_conflict.is_some() {
            center = center
                .child(
                    h_flex()
                        .w_full()
                        .gap_4()
                        .flex_wrap()
                        .items_start()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(270.))
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Current saved version · read-only"),
                                )
                                .child(
                                    div()
                                        .font_family(super::brand::MONO_FONT)
                                        .text_size(px(13.))
                                        .child(
                                            Textarea::new(&self.source_current)
                                                .readonly(true)
                                                .h(px(300.)),
                                        ),
                                ),
                        )
                        .child(editor),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child("Base version · read-only"))
                        .child(
                            div()
                                .font_family(super::brand::MONO_FONT)
                                .text_size(px(13.))
                                .child(Textarea::new(&self.source_base).readonly(true).h(px(100.))),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Draft preview"),
                )
                .child(preview_panel.min_h(px(200.)));
        } else if self.source.managed().is_some() && self.source_projection.live {
            center = center.child(editor);
        } else {
            center = center.child(
                h_flex()
                    .w_full()
                    .gap_4()
                    .flex_wrap()
                    .items_start()
                    .child(editor)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(270.))
                            .gap_2()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(
                                if dirty {
                                    "Preview · unsaved draft"
                                } else {
                                    "Markdown preview"
                                },
                            ))
                            .child(preview_panel.min_h(px(300.))),
                    ),
            );
        }
        if let Some(panel) = self.incoming_references_panel(cx) {
            center = center.child(panel);
        }
        center.into_any_element()
    }
    fn prepared_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let goal = self.goal_id();
        let mut panel = v_flex().gap_2();
        if let Some(request) = self.pending_prepared_changes.get(&goal) {
            let retry = request.clone();
            panel = panel
                .child(div().text_sm().child("This change has not been acknowledged. Refresh to inspect the stage or retry the same saved operation."))
                .child(super::brand::control("retry-prepared-change", cx)
                    .debug_selector(|| "brain-retry-prepared-change".into())
                    .label("Retry saved change").disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.pending_prepared_changes.get(&this.goal_id()) == Some(&retry) {
                            this.batch(vec![retry.clone()], window, cx);
                        }
                    })));
        }
        if let Some(edit) = self.prepared_edits.get(&goal).cloned() {
            let pending = self.pending_prepared_changes.contains_key(&goal);
            let stale = edit.expected != self.snapshot["prepared_guard"];
            panel = panel.child(div().font_weight(FontWeight::BOLD).child(if edit.discard {
                "Discard this prepared stage?"
            } else {
                "Revise the prepared stage"
            }));
            if edit.discard {
                panel = panel.child(div().text_sm().child("Discard is allowed only before T3 dispatch. The stage and context remain in history, and you can prepare a replacement."));
            } else {
                panel = panel.child(div().text_sm().child("Change the next step. Sources, goal criteria and previous result stay as prepared."))
                    .child(Textarea::new(&edit.draft).disabled(self.busy || pending));
            }
            if stale {
                let current_guard = self.snapshot["prepared_guard"].clone();
                let current_goal = goal.clone();
                panel = panel.child(div().text_sm().child("The prepared stage changed while this draft was open. Inspect its current context before applying the draft to it."))
                    .child(super::brand::control("rebase-prepared-draft", cx)
                        .label("Use current stage for this draft")
                        .disabled(self.busy || pending || self.snapshot["can_change_prepared"] != true)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.goal_id() == current_goal && this.snapshot["prepared_guard"] == current_guard && this.snapshot["can_change_prepared"] == true {
                                if let Some(edit) = this.prepared_edits.get_mut(&this.goal_id()) {
                                    edit.expected = this.snapshot["prepared_guard"].clone();
                                }
                            }
                            cx.notify();
                        })));
            }
            panel = panel.child(
                h_flex()
                    .gap_2()
                    .child(
                        super::brand::control("apply-prepared-change", cx)
                            .debug_selector(|| "brain-apply-prepared-change".into())
                            .label(if edit.discard {
                                "Discard prepared stage"
                            } else {
                                "Save revised stage"
                            })
                            .disabled(
                                self.busy
                                    || pending
                                    || stale
                                    || self.snapshot["can_change_prepared"] != true,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit_prepared_change(window, cx)
                            })),
                    )
                    .child(
                        super::brand::control("cancel-prepared-edit", cx)
                            .label(if edit.discard {
                                "Keep stage"
                            } else {
                                "Cancel editing"
                            })
                            .disabled(self.busy || pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prepared_edits.remove(&this.goal_id());
                                cx.notify();
                            })),
                    ),
            );
        } else if self.capabilities["prepared_stage_edit"] == true
            && self.snapshot["can_change_prepared"] == true
        {
            panel = panel.child(
                h_flex()
                    .gap_2()
                    .child(
                        super::brand::control("revise-prepared-stage", cx)
                            .debug_selector(|| "brain-revise-prepared-stage".into())
                            .label("Revise stage")
                            .disabled(
                                self.busy
                                    || self.snapshot["dispatch"]["packet"]["reviewed_packet"]
                                        .is_object(),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_prepared_edit(false, window, cx)
                            })),
                    )
                    .child(
                        super::brand::control("discard-prepared-stage", cx)
                            .debug_selector(|| "brain-discard-prepared-stage".into())
                            .label("Discard stage")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_prepared_edit(true, window, cx)
                            })),
                    ),
            );
        }
        panel.into_any_element()
    }

    fn execution(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Build sibling panels sequentially: their large debug render frames
        // must not share the stack while composing the same element tree.
        let maestro = (self.surface == Surface::Execution).then(|| self.maestro_panel(cx));
        self.execution_panel(maestro, cx)
    }

    fn execution_panel(
        &mut self,
        maestro: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let goal = self.goal_id();
        let has_goal = !goal.is_empty();
        let mut panel = v_flex()
            .id("brain-execution")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .p_4()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child(match self.surface {
                        Surface::Outcome => "Saved outcome",
                        Surface::Details => "Goal details & history",
                        _ => "Task & execution",
                    }),
            );
        if self.surface == Surface::Details
            && has_goal
            && self.capabilities["goal_criteria_edit"] == true
        {
            panel = panel.child(
                super::brand::control("edit-goal-criteria", cx)
                    .label("Edit outcome criteria")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_goal_criteria(window, cx)),
                    ),
            );
        }
        if let Some(item) = self
            .opened_attention
            .as_ref()
            .filter(|item| item["goal_id"] == self.snapshot["goal"]["id"])
        {
            let current = array(&self.snapshot["attention"])
                .iter()
                .any(|a| a["id"] == item["attention_id"]);
            panel =
                panel.child(
                    div()
                        .p_3()
                        .border_1()
                        .border_color(theme.border)
                        .rounded_md()
                        .child(div().text_sm().child(text(&item["message"])))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            if current {
                                "Selected attention item · current"
                            } else {
                                "Selected attention item · resolved; retained for context"
                            },
                        )),
                );
            if let Some(stage) = array(&self.snapshot["stages"])
                .iter()
                .find(|stage| stage["id"] == item["stage_id"])
            {
                if let Some(root) = self.snapshot["source_paths"]["stage"]
                    .as_str()
                    .and_then(|p| std::path::Path::new(p).parent())
                {
                    let path = if let Some(result) = item["result_id"].as_str() {
                        root.join(format!("result-{result}.md"))
                    } else {
                        root.join(format!("context-{}.md", text(&stage["context_id"])))
                    };
                    let path = path.to_string_lossy().into_owned();
                    panel = panel.child(
                        super::brand::control("attention-open-context", cx)
                            .label(if item["result_id"].is_string() {
                                "Open this result"
                            } else {
                                "Open this stage context"
                            })
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_source(path.clone(), window, cx)
                            })),
                    );
                }
            }
        }
        panel = panel.child(div().text_sm().child(next_action(
            &self.snapshot,
            &self.conversation,
            &self.capabilities,
        )));
        if self.surface != Surface::Execution {
            panel = panel.child(
                div()
                    .mt_3()
                    .font_weight(FontWeight::BOLD)
                    .child("Attention & outcomes"),
            );
            let attention = array(&self.snapshot["attention"]);
            if attention.is_empty() {
                panel = panel.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No blockers or decisions."),
                );
            }
            for item in attention {
                panel = panel.child(
                    div()
                        .p_2()
                        .border_1()
                        .border_color(theme.border)
                        .rounded_md()
                        .text_sm()
                        .child(text(&item["message"])),
                );
            }
            if self.result.is_object() {
                panel = panel
                    .child(
                        div()
                            .mt_3()
                            .font_weight(FontWeight::BOLD)
                            .child("Saved outcome"),
                    )
                    .child(div().text_sm().child(format!(
                        "Engine: {} · verification: {}",
                        text(&self.result["outcome"]),
                        text(&self.result["verification"])
                    )))
                    .child(
                        TextView::markdown("brain-result", text(&self.result["summary"]))
                            .selectable(true)
                            .style(super::reader_text_style(theme)),
                    );
                if let Some(path) = self.snapshot["source_paths"]["result"].as_str() {
                    let path = path.to_string();
                    panel = panel.child(
                        super::brand::control("open-result-note", cx)
                            .debug_selector(|| "brain-open-result".into())
                            .label("Open saved result")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_source(path.clone(), window, cx)
                            })),
                    );
                }
                panel = panel.child(
                    super::brand::control("toggle-evidence", cx)
                        .debug_selector(|| "brain-toggle-evidence".into())
                        .label(if self.show_evidence {
                            "Hide evidence"
                        } else {
                            "Review evidence"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_evidence = !this.show_evidence;
                            cx.notify();
                        })),
                );
                if self.show_evidence {
                    for (index, evidence) in array(&self.result["evidence"]).into_iter().enumerate()
                    {
                        let id = text(&evidence["id"]);
                        let selected = self.selected_evidence.contains(&id);
                        panel = panel.child(
                            v_flex()
                                .gap_1()
                                .p_2()
                                .border_1()
                                .border_color(theme.border)
                                .rounded_md()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .child(text(&evidence["kind"])),
                                )
                                .child(
                                    TextView::markdown(
                                        ("brain-evidence", index),
                                        text(&evidence["description"]),
                                    )
                                    .selectable(true)
                                    .style(super::reader_text_style(theme)),
                                )
                                .child(
                                    super::brand::control(("evidence-select", index), cx)
                                        .debug_selector(|| "brain-evidence-select".into())
                                        .label(if selected {
                                            "Selected evidence"
                                        } else {
                                            "Use as evidence"
                                        })
                                        .small()
                                        .when(selected, |b| b.primary())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.selected_evidence.remove(&id) {
                                                this.selected_evidence.insert(id.clone());
                                            }
                                            cx.notify();
                                        })),
                                ),
                        );
                    }
                }
                for (index, criterion) in array(&self.snapshot["goal"]["criteria"])
                    .into_iter()
                    .enumerate()
                {
                    let id = text(&criterion["id"]);
                    let mut review = v_flex()
                        .gap_1()
                        .child(div().text_sm().child(text(&criterion["description"])));
                    if self.goal_completed() {
                        review = review.child(
                            div()
                                .debug_selector(|| "brain-review-completed".into())
                                .text_xs()
                                .child("Criterion passed — goal completed."),
                        );
                    } else if criterion["requires_human"] == true {
                        review = review
                        .child(div().text_xs().child(
                            "Open the saved result to review its exact source before confirming.",
                        ))
                        .child(
                            super::brand::control(("accept", index), cx)
                                .debug_selector(|| "brain-confirm-outcome".into())
                                .label("I confirm this outcome")
                                .small()
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.accept(id.clone(), window, cx)
                                })),
                        );
                    } else {
                        let fail_id = id.clone();
                        review = review.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    super::brand::control(("pass", index), cx)
                                        .label("Evidence passes")
                                        .small()
                                        .disabled(self.busy || self.selected_evidence.is_empty())
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.evaluate(id.clone(), "passed", window, cx)
                                        })),
                                )
                                .child(
                                    super::brand::control(("fail", index), cx)
                                        .label("Not met")
                                        .small()
                                        .disabled(self.busy)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.evaluate(fail_id.clone(), "failed", window, cx)
                                        })),
                                ),
                        );
                    }
                    panel = panel.child(review);
                }
            }
            if self.surface == Surface::Outcome {
                return panel.into_any_element();
            }
        }
        if self.surface == Surface::Execution {
            panel = panel.children(maestro);
            let task = &self.snapshot["task"];
            if task.is_object() {
                panel = panel
                    .child(div().child(text(&task["content"])))
                    .child(div().text_sm().child(format!(
                        "Todoist: {} · Task {} · Connector {}",
                        text(&task["status"]),
                        text(&task["task_id"]),
                        task["instance_id"].as_str().unwrap_or("Unavailable")
                    )))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                super::brand::control("open-task", cx)
                                    .label("Open task")
                                    .on_click({
                                        let url = text(&task["url"]);
                                        move |_, _, cx| {
                                            if url.starts_with("https://") {
                                                cx.open_url(&url);
                                            }
                                        }
                                    }),
                            )
                            .child(
                                super::brand::control("refresh-task", cx)
                                    .label("Refresh")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.batch(
                                        vec![json!({"op":"task_refresh","goal_id":this.goal_id()})],
                                        window,
                                        cx,
                                    )
                                    })),
                            ),
                    );
            } else {
                panel = panel.child(
                    super::brand::control("create-task", cx)
                        .label("Create Todoist task")
                        .disabled(
                            self.busy
                                || !has_goal
                                || self.capabilities["todoist"] != true
                                || self.pending_task.is_some()
                                || self.todoist_picker.linking(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.create_task(window, cx))),
                );
                if self.capabilities["todoist"] == false {
                    panel =
                        panel.child(div().text_xs().child(
                            "Open Workspace → Connections to configure or reconnect Todoist.",
                        ));
                }
            }
            panel = panel.child(self.todoist_picker_panel(cx));
            if let Some(operation) = self.pending_task.clone() {
                panel = panel.child(
                    super::brand::control("recover-task", cx)
                        .label("Recover task")
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.batch(
                                vec![json!({"op":"task_reconcile","operation_id":operation})],
                                window,
                                cx,
                            )
                        })),
                );
            }
        }
        let phase = text(&self.snapshot["phase"]);
        let replacement = phase == "discarded";
        let followup = ["outcome_ready", "cancelled"].contains(&phase.as_str())
            && !array(&self.snapshot["stage"]["result_ids"]).is_empty();
        panel = panel
            .child(div().mt_3().font_weight(FontWeight::BOLD).child("T3 stage"))
            .when(phase.is_empty() || followup || replacement, |panel| {
                panel.child(Textarea::new(&self.next_step).disabled(self.busy))
            })
            .child(
                super::brand::control("prepare-stage", cx)
                    .label(if followup {
                        "Prepare follow-up context"
                    } else {
                        "Prepare context"
                    })
                    .disabled(
                        self.busy
                            || !has_goal
                            || self.capabilities["t3"] != true
                            || (!self.snapshot["task"].is_object()
                                && !matches!(self.reviewed_context(cx), Ok(Some(_))))
                            || self.prepared_edits.contains_key(&goal)
                            || self.pending_prepared_changes.contains_key(&goal)
                            || (!phase.is_empty() && !followup && !replacement),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.prepare(window, cx))),
            )
            .child(div().text_sm().child(if phase.is_empty() {
                "No stage prepared".into()
            } else {
                format!("Stage: {phase}")
            }));
        panel = panel.child(
            super::brand::control("toggle-history", cx)
                .label(if self.show_history {
                    "Hide history"
                } else {
                    "History & details"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_history = !this.show_history;
                    cx.notify();
                })),
        );
        if self.show_history || self.surface == Surface::Details {
            for (index, stage) in array(&self.snapshot["stages"]).iter().enumerate() {
                if stage["id"] == self.snapshot["stage"]["id"] {
                    continue;
                }
                if let Some(root) = self.snapshot["source_paths"]["stage"]
                    .as_str()
                    .and_then(|p| std::path::Path::new(p).parent())
                {
                    for (kind, id) in std::iter::once(("context", stage["context_id"].clone()))
                        .chain(
                            array(&stage["result_ids"])
                                .into_iter()
                                .map(|id| ("result", id)),
                        )
                    {
                        let path = root
                            .join(format!("{kind}-{}.md", text(&id)))
                            .to_string_lossy()
                            .to_string();
                        panel = panel.child(
                            super::brand::control(
                                SharedString::from(format!("history-{kind}-{index}-{}", text(&id))),
                                cx,
                            )
                            .label(format!("Stage {} {kind}", index + 1))
                            .disabled(self.busy)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_source(path.clone(), window, cx)
                                },
                            )),
                        );
                    }
                }
            }
            panel = panel.child(div().text_xs().child(format!(
                "{} recorded engine events",
                self.snapshot["event_count"]
            )));
            let current_ids: BTreeSet<_> = array(&self.snapshot["attention"])
                .iter()
                .filter_map(|a| a["id"].as_str().map(str::to_string))
                .collect();
            for item in array(&self.snapshot["attention_history"]) {
                if !current_ids.contains(&text(&item["id"])) {
                    panel = panel.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("Earlier: {}", text(&item["message"]))),
                    );
                }
            }
        }
        if let Some(path) = self.snapshot["source_paths"]["stage"].as_str() {
            let path = std::path::Path::new(path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(""))
                .join(format!(
                    "context-{}.md",
                    text(&self.snapshot["dispatch"]["context_id"])
                ))
                .to_string_lossy()
                .into_owned();
            panel = panel.child(
                super::brand::control("open-stage-context", cx)
                    .label("Open prepared context")
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_source(path.clone(), window, cx)
                    })),
            );
        }
        panel = panel.child(self.prepared_controls(cx));
        if ["prepared", "not_started"].contains(&phase.as_str()) {
            let expected = self.snapshot["prepared_guard"].clone();
            let start_goal = goal.clone();
            let guarded = self.capabilities["guarded_start"] == true;
            panel = panel.child(
                super::brand::control("start-stage", cx)
                    .label("Start in T3")
                    .primary()
                    .disabled(self.busy || self.prepared_edits.contains_key(&goal)
                        || self.pending_prepared_changes.contains_key(&goal)
                        || (guarded && !expected.is_object()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.goal_id() != start_goal || this.prepared_edits.contains_key(&start_goal) || this.pending_prepared_changes.contains_key(&start_goal) || (guarded && this.snapshot["prepared_guard"] != expected) {
                            this.error = Some("The stage changed before Start. Inspect the current prepared context.".into());
                            cx.notify(); return;
                        }
                        let mut request = json!({"op":"start","goal_id":start_goal});
                        if guarded { request["expected"] = expected.clone(); }
                        this.batch(vec![request], window, cx)
                    })),
            );
        }
        if phase == "indeterminate" {
            panel = panel.child(
                super::brand::control("reconcile-stage", cx)
                    .label("Recover stage state")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.batch(vec![json!({"op":"reconcile"})], window, cx)
                    })),
            );
        }
        if let Some(url) = self.snapshot["thread_url"].as_str() {
            panel = panel.child(
                super::brand::control("open-thread", cx)
                    .label("Open T3 thread")
                    .on_click({
                        let url = url.to_string();
                        move |_, _, cx| {
                            if url.starts_with("https://") || url.starts_with("http://") {
                                cx.open_url(&url);
                            }
                        }
                    }),
            );
        }
        if self.capabilities["t3"] == false {
            panel = panel.child(
                div()
                    .text_xs()
                    .child("Open Workspace → Connections to configure or reconnect T3."),
            );
        }
        panel.into_any_element()
    }
}
impl Render for BrainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_source_navigation(cx);
        self.sync_open_link(window, cx);
        self.sync_source_find(window, cx);
        self.sync_source_policy(cx);
        self.sync_source_selection_context(window, cx);
        let colors = super::brand::palette(cx);
        let error = self.error.clone();
        let content = if self.decision_reuse.active {
            self.decision_reuse_panel(cx)
        } else if self.discussion_decision.active {
            self.decision_panel(cx)
        } else if self.goal_criteria.active {
            self.goal_criteria_panel(cx)
        } else if self.discussion_note.active() {
            self.discussion_note_panel(cx)
        } else if self.collection == Collection::Inbox {
            if self.inbox_plan.form {
                self.inbox_plan_panel(cx)
            } else {
                self.inbox_panel(cx)
            }
        } else if self.collection == Collection::Attention && self.use_bound_attention() {
            if self.proposals.active {
                self.proposal_panel(cx)
            } else {
                self.bound_attention_panel(cx)
            }
        } else if self.show_capture
            || (self.goal_id().is_empty() && self.surface != Surface::Source)
        {
            self.capture_form(cx)
        } else {
            self.center(window, cx)
        };
        let pending = self.pending_navigation.is_some();
        v_flex()
            .size_full()
            .min_h_0()
            .track_focus(&self.focus)
            .key_context("TesseraWorkspace")
            .on_action(
                cx.listener(|this, _: &FindSource, window, cx| this.open_source_find(window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Search, window, cx| {
                    if this.source_find_focus(window, cx) {
                        this.open_source_find(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    if this.source_find.active() && this.source_find_focus(window, cx) {
                        this.close_source_find(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(|this, _: &KeepEditing, window, cx| {
                if this.source_find.active() && this.source_find_focus(window, cx) {
                    this.close_source_find(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(
                cx.listener(|this, _: &source_trace::DumpManagedSourceTrace, _, cx| {
                    this.dump_source_trace(cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &SearchBrain, window, cx| this.focus_search(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &CaptureThought, window, cx| this.begin_thought(window, cx)),
            )
            .on_action(cx.listener(|this, _: &SaveDraft, window, cx| {
                if this.decision_reuse.active {
                    this.save_decision_reuse(window, cx);
                    return;
                }
                if this.discussion_decision.active {
                    this.decision_rpc(true, window, cx);
                    return;
                }
                if this.goal_criteria.active {
                    this.save_goal_criteria(window, cx);
                    return;
                }
                if this.discussion_note.active() {
                    this.save_discussion_note(window, cx);
                    return;
                }
                if this.surface == Surface::Source
                    && !this.editor_closing()
                    && this.dirty(cx)
                    && !this.busy
                    && this.source_conflict.is_none()
                    && this.capabilities["source_write"] == true
                {
                    this.save_source(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &KeepEditing, _, cx| {
                if this.decision_reuse.active {
                    this.reuse_keep_reviewing(cx);
                    return;
                }
                if this.discussion_decision.active {
                    this.decision_keep_reviewing(cx);
                    return;
                }
                if this.goal_criteria.active {
                    this.criteria_keep_editing(cx);
                    return;
                }
                if this.discussion_note.active() {
                    this.note_keep_editing(cx);
                    return;
                }
                if this.pending_navigation.is_some() {
                    this.pending_navigation = None;
                    this.navigation_after_source = false;
                    this.error = None;
                    cx.notify();
                }
            }))
            .bg(colors.canvas)
            .text_color(colors.text)
            .when(!self.embedded, |view| {
                view.child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .flex_shrink_0()
                        .border_b_1()
                        .border_color(colors.border_subtle)
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(colors.text_muted)
                                .child(if self.busy {
                                    "Working…".into()
                                } else {
                                    self.notice.clone()
                                }),
                        )
                        .child(
                            super::brand::control("new-goal", cx)
                                .ghost()
                                .label("New thought")
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.begin_thought(window, cx)
                                })),
                        )
                        .child(
                            super::brand::button(
                                "brain-refresh",
                                "Refresh",
                                super::brand::ButtonKind::Quiet,
                                cx,
                            )
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.refresh_workspace(window, cx)
                                }),
                            ),
                        ),
                )
            })
            .child(self.discussion_delivery_panel(cx))
            .when(pending, |view| view.child(self.navigation_prompt(cx)))
            .when_some(error, |view, error| {
                view.child(
                    h_flex()
                        .p_3()
                        .gap_3()
                        .bg(colors.surface_raised)
                        .border_b_1()
                        .border_color(colors.danger)
                        .child(div().flex_1().text_color(colors.danger).child(error))
                        .child(
                            super::brand::control("dismiss-brain-error", cx)
                                .ghost()
                                .label("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.error = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_start()
                    .child(self.sidebar(window, cx))
                    .child(content),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::{
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    fn source_snapshot(bytes: &[u8]) -> Value {
        json!({"schema":SCHEMA,"brain_id":"01000000-0000-4000-8000-000000000001","path":"notes/source.md",
            "revision":"sha256:fixture","content_base64":STANDARD.encode(bytes),"media_type":"text/markdown"})
    }

    fn attention_fixture() -> Value {
        json!({"goal_count":2,"running_goal_count":1,"observed_at":"2026-09-06T14:00:00Z","items":[
            {"attention_id":"a","goal_id":"one","goal_title":"First goal","stage_id":"s1","kind":"blocker","message":"First blocker"},
            {"attention_id":"b","goal_id":"two","goal_title":"Second goal","stage_id":"s2","kind":"final","message":"Second result","result_id":"r2"}
        ]})
    }

    #[gpui::test]
    fn workspace_attention_renders_other_goals_and_retains_stale_overview(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.busy = true;
            view.snapshot = json!({"goal":{"id":"one","title":"First goal"},"attention":[]});
            view.capabilities = json!({"workspace_attention":true});
            view.workspace_attention = attention_fixture();
            view.collection = Collection::Attention;
            view
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("attention-one-a-s1").is_some(),
            "positive control: selected goal item is rendered"
        );
        assert!(
            cx.debug_bounds("attention-two-b-s2").is_some(),
            "other goal result is rendered even though selected goal has no attention"
        );
        view.update_in(cx, |view, window, cx| {
            let original = view.workspace_attention.clone();
            view.finish_batch(
                vec![("workspace_attention".into(), Err("backend offline".into()))],
                true,
                0,
                window,
                cx,
            );
            assert_eq!(view.workspace_attention, original);
            assert_eq!(view.attention_error.as_deref(), Some("backend offline"));
            let mut newer = original.clone();
            newer["items"] = json!([]);
            view.request_generation = 2;
            view.finish_batch(
                vec![("workspace_attention".into(), Ok(newer.clone()))],
                true,
                1,
                window,
                cx,
            );
            assert_eq!(
                view.workspace_attention, original,
                "obsolete read cannot clear current items"
            );
            view.finish_batch(
                vec![("workspace_attention".into(), Ok(newer))],
                true,
                2,
                window,
                cx,
            );
            assert!(array(&view.workspace_attention["items"]).is_empty());
            assert!(view.attention_error.is_none());
            view.finish_batch(vec![("capabilities".into(),Err("connection offline".into()))],false,2,window,cx);
            assert!(view.attention_unknown(), "capabilities-first refresh failure must not claim the cached empty queue is current");
            let empty = view.workspace_attention.clone();
            assert!(array(&empty["items"]).is_empty());
            view.finish_batch(vec![("workspace_attention".into(),Ok(empty))],false,2,window,cx);
            assert!(!view.attention_unknown());
            view.capabilities["workspace_attention"] = json!(false);
            assert!(view.attention_unknown(), "downgraded backend cannot establish current empty attention");
            view.capabilities["workspace_attention"] = json!(true);
            view.collection = Collection::Goals;
            assert!(
                view.attention_polling(),
                "running other goal keeps the overview refreshed"
            );
            view.workspace_attention["running_goal_count"] = json!(0);
            assert!(
                !view.attention_polling(),
                "idle All goals view does not poll"
            );
        });
    }

    #[gpui::test]
    fn maestro_only_goal_polls_through_disconnect_until_unlinked(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.collection = Collection::Goals;
            view.selected_goal_id = Some("one".into());
            view.snapshot = json!({"goal":{"id":"one"},"maestro":{"link":{
                "goal_id":"one","active":true,"status":"observed"
            }}});
            assert!(!view.running() && !view.attention_polling());
            assert!(
                view.needs_polling(),
                "Maestro does not depend on a T3 stage"
            );
            view.snapshot["maestro"]["link"]["status"] = json!("disconnected");
            view.maestro_ui.backend_unavailable = true;
            assert!(view.needs_polling(), "Reconnect must remain observable");
            view.selected_goal_id = Some("two".into());
            assert!(
                !view.needs_polling(),
                "Old goal cannot enroll the new selection"
            );
            view.selected_goal_id = Some("one".into());
            view.snapshot["maestro"]["link"]["goal_id"] = json!("other");
            assert!(
                !view.needs_polling(),
                "Mismatched link is not observation authority"
            );
            view.snapshot["maestro"]["link"]["goal_id"] = json!("one");
            view.snapshot["maestro"]["link"]["active"] = json!(false);
            assert!(
                !view.needs_polling(),
                "Stopped history must not keep polling"
            );
            view.snapshot["maestro"]["link"] = Value::Null;
            assert!(!view.needs_polling());
            view
        });
    }

    #[gpui::test]
    fn attention_navigation_preserves_dirty_and_prepared_ownership_and_resolved_context(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"one"}});
            view.selected_goal_id = Some("one".into());
            let pending = json!({"op":"stage_revise","goal_id":"one","operation_id":"kept"});
            view.pending_prepared_changes
                .insert("one".into(), pending.clone());
            view.compose.update(cx, |input, cx| {
                input.set_value("Unsent first goal thought", window, cx)
            });
            view.source_original = "saved".into();
            view.source_editable = true;
            view.source.reset("unsaved", window, cx);
            let item = attention_fixture()["items"][1].clone();
            view.open_attention(item.clone(), window, cx);
            assert!(matches!(
                view.pending_navigation,
                Some(PendingNavigation::Attention(_))
            ));
            assert!(!view.busy, "dirty attention navigation sends no request");
            assert_eq!(view.goal_id(), "one");
            view.source.reset("saved", window, cx);
            view.continue_navigation(window, cx);
            assert!(view.busy);
            let second = json!({"goal":{"id":"two"},"stage":{"id":"s2"},"attention":[{"id":"b"}]});
            view.finish_batch(
                vec![("snapshot".into(), Ok(second.clone()))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert_eq!(view.goal_id(), "two");
            assert!(matches!(view.surface, Surface::Outcome));
            assert_eq!(view.goal_drafts["one"].0, "Unsent first goal thought");
            assert_eq!(view.pending_prepared_changes["one"], pending);
            assert_eq!(view.opened_attention.as_ref().unwrap()["result_id"], "r2");
            view.open_attention(item, window, cx);
            let mut resolved = second;
            resolved["attention"] = json!([]);
            resolved["stage"]["id"] = json!("replacement");
            view.finish_batch(
                vec![("snapshot".into(), Ok(resolved))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert!(matches!(view.surface, Surface::Details));
            assert!(
                view.show_history,
                "stale item opens history instead of attributing the old result to new stage"
            );
            assert_eq!(view.opened_attention.as_ref().unwrap()["stage_id"], "s2");
            view
        });
    }

    #[gpui::test]
    fn acknowledged_capture_clears_only_the_submitted_fields(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            for (id, newer) in [("one", false), ("two", true)] {
                view.pending_capture_id = Some(id.into());
                view.pending_capture_fields =
                    Some(("Submitted thought".into(), "Observed outcome".into()));
                view.title.update(cx, |input, cx| {
                    input.set_value(
                        if newer {
                            "Another unsent thought"
                        } else {
                            "Submitted thought"
                        },
                        window,
                        cx,
                    )
                });
                view.criteria.update(cx, |input, cx| {
                    input.set_value("Observed outcome", window, cx)
                });
                view.finish_batch(
                    vec![(
                        "snapshot".into(),
                        Ok(json!({"goal":{"id":id,"title":"Submitted thought","status":"active"}})),
                    )],
                    false,
                    view.request_generation,
                    window,
                    cx,
                );
                assert!(view.pending_capture_id.is_none());
                assert!(view.pending_capture_fields.is_none());
                assert_eq!(
                    view.title.read(cx).value().as_ref(),
                    if newer { "Another unsent thought" } else { "" }
                );
                assert_eq!(
                    view.criteria.read(cx).value().as_ref(),
                    if newer { "Observed outcome" } else { "" }
                );
            }
            view
        });
    }

    fn prepared_fixture(goal: &str, stage: &str) -> Value {
        json!({"goal":{"id":goal,"title":"Prepared goal"},"phase":"prepared",
            "can_change_prepared":true,
            "prepared_guard":{"stage_id":stage,"stage_revision":"sha256:stage","context_id":format!("context-{stage}"),"context_revision":"sha256:context"},
            "stage":{"id":stage,"result_ids":[]},
            "dispatch":{"packet":{"next_step":"Original prepared instruction"}}})
    }

    #[gpui::test]
    fn prepared_revision_retains_exact_retry_and_later_draft(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = prepared_fixture("one", "first");
            view.prepared_journal = Some(PreparedJournal {
                root: std::env::temp_dir().join(format!("tessera-prepared-test-{}", uuid())),
                workspace: json!({"brain_id":"fixture"}),
            });
            view.capabilities = json!({"prepared_stage_edit":true,"guarded_start":true});
            view.begin_prepared_edit(false, window, cx);
            let editor = view.prepared_edits["one"].draft.clone();
            editor.update(cx, |input, cx| input.set_value("Revised instruction", window, cx));
            view.submit_prepared_change(window, cx);
            let submitted = view.pending_prepared_changes["one"].clone();
            assert_eq!(submitted["next_step"], "Revised instruction");
            let journal = view.prepared_journal.as_ref().unwrap();
            let reopened = PreparedJournal { root:journal.root.clone(), workspace:journal.workspace.clone() };
            assert_eq!(reopened.pending().unwrap()["one"], submitted);
            let different_workspace = PreparedJournal { root:journal.root.clone(), workspace:json!({"brain_id":"other"}) };
            assert!(different_workspace.pending().is_err());
            assert_eq!(submitted["expected"], view.snapshot["prepared_guard"]);
            assert!(Uuid::parse_str(submitted["operation_id"].as_str().unwrap()).is_ok());
            view.finish_batch(vec![("stage_revise".into(), Err("Lost acknowledgement".into()))], false, view.request_generation, window, cx);
            assert_eq!(view.pending_prepared_changes["one"], submitted);
            view.busy = false;
            editor.update(cx, |input, cx| input.set_value("Later unsent instruction", window, cx));
            view.submit_prepared_change(window, cx);
            assert_eq!(view.pending_prepared_changes["one"], submitted, "must never allocate a second operation while one is pending");
            let mut acknowledgement = prepared_fixture("one", "second");
            acknowledgement["prepared_change"] = json!({"goal_id":"one","action":"revise","operation_id":submitted["operation_id"],"previous":submitted["expected"],"replacement":acknowledgement["prepared_guard"]});
            let mut wrong = acknowledgement.clone();
            wrong["prepared_change"]["operation_id"] = json!("another-operation");
            assert!(!view.acknowledge_prepared_change(&wrong, window, cx));
            assert_eq!(view.pending_prepared_changes["one"], submitted);
            assert!(view.acknowledge_prepared_change(&acknowledgement, window, cx));
            assert!(!view.pending_prepared_changes.contains_key("one"));
            assert_eq!(view.prepared_edits["one"].draft.read(cx).value(), "Later unsent instruction");
            assert_eq!(view.prepared_edits["one"].expected, acknowledgement["prepared_guard"]);
            view
        });
    }

    #[test]
    fn prepared_rejection_requires_explicit_durable_non_recording_proof() {
        for proof in [Value::Null, json!(true), json!(false)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = listener.local_addr().unwrap();
            let proof_for_server = proof.clone();
            let server = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                writeln!(reader.get_mut(), "{}", json!({"schema":SCHEMA,"id":request["id"],"ok":false,
                    "error":{"code":"runtime_error","message":"Stage changed","prepared_change_recorded":proof_for_server}})).unwrap();
            });
            let result = rpc(
                endpoint,
                json!({"op":"stage_revise","goal_id":"one","operation_id":"saved-operation"}),
            );
            if proof == false {
                let rejection = result.unwrap();
                assert_eq!(
                    rejection["prepared_change_rejected"]["operation_id"],
                    "saved-operation"
                );
            } else {
                assert!(
                    result.is_err(),
                    "an uncertain rejection must retain the original pending request"
                );
            }
            server.join().unwrap();
        }
    }

    #[gpui::test]
    fn prepared_edits_are_goal_owned_and_stale_drafts_do_not_mutate(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.prepared_journal = Some(PreparedJournal {
                root: std::env::temp_dir().join(format!("tessera-prepared-test-{}", uuid())),
                workspace: json!({"brain_id":"fixture"}),
            });
            view.capabilities = json!({"prepared_stage_edit":true});
            view.snapshot = prepared_fixture("one", "first");
            view.begin_prepared_edit(false, window, cx);
            let first = view.prepared_edits["one"].draft.clone();
            first.update(cx, |input, cx| input.set_value("Goal one's draft", window, cx));
            view.snapshot = prepared_fixture("two", "second");
            view.begin_prepared_edit(true, window, cx);
            assert!(view.prepared_edits["two"].discard);
            assert_eq!(first.read(cx).value(), "Goal one's draft");
            view.snapshot = prepared_fixture("one", "newer-first");
            view.submit_prepared_change(window, cx);
            assert!(view.pending_prepared_changes.is_empty());
            assert_eq!(first.read(cx).value(), "Goal one's draft");
            let never_recorded = json!({"op":"stage_revise","goal_id":"one","operation_id":uuid(),"expected":view.snapshot["prepared_guard"],"next_step":"Restored submitted instruction"});
            view.prepared_journal.as_ref().unwrap().write_once(&never_recorded, "json").unwrap();
            let rejected_id = never_recorded["operation_id"].clone();
            view.pending_prepared_changes.insert("one".into(), never_recorded);
            view.prepared_edits.remove("one");
            view.finish_batch(vec![("stage_revise".into(), Ok(json!({"prepared_change_rejected":{"goal_id":"one","operation_id":rejected_id,"message":"Stage changed"}})))], false, view.request_generation, window, cx);
            assert!(!view.pending_prepared_changes.contains_key("one"));
            assert_eq!(view.prepared_edits["one"].draft.read(cx).value(), "Restored submitted instruction");
            assert_eq!(view.prepared_edits["one"].expected, view.snapshot["prepared_guard"]);
            assert_eq!(first.read(cx).value(), "Goal one's draft");
            view.busy = false;
            view.snapshot = prepared_fixture("two", "second");
            view.submit_prepared_change(window, cx);
            let request = view.pending_prepared_changes["two"].clone();
            assert_eq!(request["op"], "stage_discard");
            assert!(request.get("next_step").is_none());
            let receipt = json!({"goal":{"id":"two"},"prepared_change":{"goal_id":"two","action":"discard","operation_id":request["operation_id"],"previous":request["expected"],"replacement":null}});
            assert!(view.acknowledge_prepared_change(&receipt, window, cx));
            assert!(!view.prepared_edits.contains_key("two"));
            assert_eq!(first.read(cx).value(), "Goal one's draft");
            view
        });
    }

    #[gpui::test]
    fn retained_composer_stays_below_independent_transcript_as_messages_grow(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view,cx)=cx.add_window_view(|window,cx| {
            let mut view=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);
            view.snapshot=json!({"goal":{"id":"one","title":"Fixture goal","status":"active"},"goals":[{"id":"one","title":"Fixture goal","status":"active"}]});
            view.selected_goal_id=Some("one".into());
            view.conversation=json!({"messages":[{"role":"assistant","text":"A short reply"}],"status":"complete"});
            view.compose.update(cx,|input,cx|input.set_value("Unsent personal draft",window,cx));
            view
        });
        cx.run_until_parked();
        let composer = cx
            .debug_bounds("brain-composer")
            .expect("composer is visible");
        let list = cx
            .debug_bounds("brain-master-list")
            .expect("master list is persistent");
        let transcript = cx
            .debug_bounds("brain-transcript")
            .expect("independent scroll region");
        assert!(transcript.bottom() <= composer.top());
        view.update(cx, |view, cx| {
            view.conversation["messages"] =
                json!([{"role":"assistant","text":"Longer retained response.\n\n".repeat(100)}]);
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("brain-composer"), Some(composer));
        assert_eq!(cx.debug_bounds("brain-master-list"), Some(list));
        view.update(cx, |view, cx| {
            assert_eq!(
                view.compose.read(cx).value().as_ref(),
                "Unsent personal draft"
            )
        });
    }

    #[gpui::test]
    fn reader_file_delivery_preserves_managed_dirty_recovery(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::reader_open::install(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-managed-open327-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("read.md");
        std::fs::write(&file, "# Read only\n").unwrap();
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"one","title":"One","status":"active"}});
            view.selected_goal_id = Some("one".into());
            view.load_source(source_snapshot(b"Original\r\n"), window, cx);
            view.source.reset("Unsaved draft\r\n", window, cx);
            view.pending_navigation = Some(PendingNavigation::Capture);
            view.pending_source_read_draft = Some("Recovery draft\r\n".into());
            let generation = view.request_generation;
            let source_entity = view.source.entity_id();
            crate::reader_open::dispatch_urls(
                vec![url::Url::from_file_path(&file).unwrap().into()],
                cx,
            );
            assert!(view.dirty(cx));
            assert_eq!(view.source.value(cx).as_ref(), "Unsaved draft\r\n");
            assert!(matches!(
                view.pending_navigation,
                Some(PendingNavigation::Capture)
            ));
            assert_eq!(
                view.pending_source_read_draft.as_deref(),
                Some("Recovery draft\r\n")
            );
            assert_eq!(view.request_generation, generation);
            assert_eq!(view.source.entity_id(), source_entity);
            assert_eq!(view.goal_id(), "one");
            view
        });
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn dirty_navigation_keeps_destination_and_late_draft_until_acknowledged(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"one","title":"One","status":"active"}});
            view.selected_goal_id = Some("one".into());
            let original = source_snapshot(b"Original\r\n");
            view.load_source(original.clone(), window, cx);
            view.source.reset("Draft\r\n", window, cx);
            view.begin_capture(window, cx);
            assert!(matches!(
                view.pending_navigation,
                Some(PendingNavigation::Capture)
            ));
            assert!(!view.show_capture);
            assert_eq!(view.goal_id(), "one");
            view.navigation_after_source = true;
            view.finish_batch(
                vec![("source_write".into(), Err("Save failed".into()))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert!(view.pending_navigation.is_some());
            assert_eq!(view.source.value(cx).as_ref(), "Draft\r\n");
            // An edit queued during explicit discard is still protected by the
            // existing source-read acknowledgement guard.
            view.pending_source_read_draft = Some("Draft\r\n".into());
            view.source.reset("Later draft\r\n", window, cx);
            view.finish_batch(
                vec![("source_read".into(), Ok(original.clone()))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert!(!view.show_capture);
            assert_eq!(view.source.value(cx).as_ref(), "Later draft\r\n");
            assert!(view.pending_navigation.is_some());
            // Positive control: only an acknowledged unchanged discard unlocks
            // the requested capture surface, without creating a goal itself.
            view.pending_source_read_draft = Some("Later draft\r\n".into());
            view.finish_batch(
                vec![("source_read".into(), Ok(original))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert!(view.show_capture);
            assert!(view.pending_navigation.is_none());
            assert_eq!(view.goal_id(), "one");
            assert!(!view.dirty(cx));
            view
        });
    }

    #[test]
    fn next_action_distinguishes_engine_outcome_from_goal_completion_and_recovery() {
        let mut snapshot = json!({"goal":{"id":"goal", "status":"active"},"phase":"outcome_ready"});
        assert!(
            next_action(&snapshot, &Value::Null, &Value::Null).contains("Review the saved result")
        );
        snapshot["goal"]["status"] = json!("completed");
        assert!(next_action(&snapshot, &Value::Null, &Value::Null)
            .contains("Todoist remains the authority"));
        snapshot["goal"]["status"] = json!("active");
        snapshot["phase"] = json!("indeterminate");
        assert!(next_action(&snapshot, &Value::Null, &Value::Null).contains("original stage"));
        snapshot["pending_task_operation_id"] = json!("pending-task");
        assert!(
            next_action(&snapshot, &Value::Null, &Value::Null).contains("existing task operation")
        );
    }

    #[gpui::test]
    fn goal_switch_restores_own_selection_and_blocks_dirty_source_navigation(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx| {
            let mut view=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);
            let first=json!({"goal":{"id":"first","title":"First thought"},"goals":[{"id":"first"},{"id":"second"}],
                "selected_source_paths":["first.md"],"selected_conversation_id":null});
            let second=json!({"goal":{"id":"second","title":"Second thought"},"goals":[{"id":"first"},{"id":"second"}],
                "selected_source_paths":["second.md"],"selected_conversation_id":null});
            view.finish_batch(vec![("snapshot".into(),Ok(first.clone()))],false,0,window,cx);
            assert_eq!(view.selected_sources,BTreeSet::from(["first.md".into()]));
            view.compose.update(cx,|input,cx|input.set_value("First unsent thought",window,cx));
            view.source_original="Saved source".into();
            view.source_editable=true;
            view.source.reset("Dirty source", window, cx);
            view.select_goal("second".into(),window,cx);
            assert_eq!(view.selected_goal_id.as_deref(),Some("first"));
            assert_eq!(view.source.value(cx).as_ref(),"Dirty source");
            assert!(!view.busy,"blocked navigation must not send a request");
            // Deliberate discard permits navigation; a late poll is fenced by
            // the generation change and cannot restore the old goal afterwards.
            view.source.reset("Saved source", window, cx);
            view.select_goal("second".into(),window,cx);
            assert!(view.busy);
            let generation=view.request_generation;
            view.finish_batch(vec![("snapshot".into(),Ok(second))],false,generation,window,cx);
            assert_eq!(view.goal_id(),"second");
            assert_eq!(view.selected_sources,BTreeSet::from(["second.md".into()]));
            assert_eq!(view.compose.read(cx).value().as_ref(),"");
            view.finish_batch(vec![("snapshot".into(),Ok(first.clone()))],true,generation-1,window,cx);
            assert_eq!(view.goal_id(),"second");
            view.select_goal("first".into(),window,cx);
            view.finish_batch(vec![("snapshot".into(),Ok(first))],false,view.request_generation,window,cx);
            assert_eq!(view.compose.read(cx).value().as_ref(),"First unsent thought");
            assert_eq!(view.selected_sources,BTreeSet::from(["first.md".into()]));
            view
        });
    }

    #[gpui::test]
    fn failed_selection_and_delayed_drafts_never_change_visible_goal_ownership(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            let first = json!({"goal":{"id":"first"},"selected_source_paths":[]});
            let second = json!({"goal":{"id":"second"},"selected_source_paths":[]});
            view.finish_batch(vec![("snapshot".into(), Ok(first))], false, 0, window, cx);
            view.select_goal("second".into(), window, cx);
            view.finish_batch(
                vec![("snapshot".into(), Err("unavailable".into()))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert_eq!(view.goal_id(), "first");
            assert_eq!(
                view.scoped_request(json!({"op":"start"}))["goal_id"],
                "first"
            );
            assert_eq!(
                view.scoped_request(json!({"op":"accept_human"}))["goal_id"],
                "first"
            );
            view.pending_capture_id = Some("uncertain-new-goal".into());
            view.batch(vec![json!({"op":"start"})], window, cx);
            assert!(
                !view.busy,
                "uncertain capture prevents new provider command"
            );
            assert_eq!(
                view.scoped_request(json!({"op":"start"}))["goal_id"],
                "first"
            );
            view.select_goal("second".into(), window, cx);
            // Positive delayed-edit probe: bypass render disabled state to show
            // a queued textarea event still cannot be dropped by the response.
            view.source_editable = true;
            view.source_original = "original".into();
            view.source.reset("late source edit", window, cx);
            view.compose.update(cx, |input, cx| {
                input.set_value("late compose edit", window, cx)
            });
            view.finish_batch(
                vec![("snapshot".into(), Ok(second.clone()))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert_eq!(view.goal_id(), "first");
            assert_eq!(view.source.value(cx).as_ref(), "late source edit");
            assert_eq!(view.compose.read(cx).value().as_ref(), "late compose edit");
            view.source.reset("original", window, cx);
            view.select_goal("second".into(), window, cx);
            view.compose.update(cx, |input, cx| {
                input.set_value("latest compose edit", window, cx)
            });
            view.finish_batch(
                vec![("snapshot".into(), Ok(second))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert_eq!(view.goal_id(), "second");
            assert_eq!(view.goal_drafts["first"].0, "latest compose edit");
            view
        });
    }

    #[gpui::test]
    fn delayed_source_read_preserves_new_edit_but_explicit_discard_can_reload(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            let first = source_snapshot(b"Saved first source");
            view.load_source(first.clone(), window, cx);
            view.open_source("notes/other.md".into(), window, cx);
            view.source.reset("Late first-source edit", window, cx);
            let mut other = source_snapshot(b"Second source");
            other["path"] = json!("notes/other.md");
            view.finish_batch(
                vec![("source_read".into(), Ok(other))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert_eq!(view.source_snapshot, Some(first.clone()));
            assert_eq!(view.source.value(cx).as_ref(), "Late first-source edit");
            assert!(!view.source_loading);
            // Same explicit discard path as the native control: loading a dirty
            // source is permitted only if no new edit arrived after the request.
            view.batch(
                vec![json!({"op":"source_read","path":first["path"]})],
                window,
                cx,
            );
            view.finish_batch(
                vec![("source_read".into(), Ok(first))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert_eq!(view.source.value(cx).as_ref(), "Saved first source");
            assert!(!view.dirty(cx));
            view
        });
    }

    #[gpui::test]
    fn completed_goal_replaces_confirmation_but_keeps_saved_result_access(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({
                "goal":{"id":"goal", "status":"active", "criteria":[{"id":"C1", "description":"Review outcome", "requires_human":true}]},
                "stage":{"status":"completed"}, "source_paths":{"result":"records/result.md"}
            });
            view.result = json!({"id":"result", "outcome":"succeeded", "verification":"verified", "summary":"Saved checklist", "evidence":[{"id":"evidence-1","kind":"artifact","description":"Saved proof"}]});
            view.surface = Surface::Outcome;
            view
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("brain-confirm-outcome").is_some(),
            "positive control: active goal renders confirmation despite historical verified result"
        );
        assert!(cx.debug_bounds("brain-open-result").is_some());
        assert!(cx.debug_bounds("brain-evidence-select").is_none());
        let toggle = cx.debug_bounds("brain-toggle-evidence").unwrap();
        cx.simulate_click(toggle.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("brain-evidence-select").is_some());
        view.update(cx, |view, cx| {
            view.snapshot["goal"]["status"] = json!("completed");
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("brain-confirm-outcome").is_none());
        assert!(cx.debug_bounds("brain-review-completed").is_some());
        assert!(cx.debug_bounds("brain-open-result").is_some());
        view.update(cx, |view, cx| {
            view.snapshot["goal"]["status"] = json!("blocked");
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("brain-confirm-outcome").is_some());
        assert!(cx.debug_bounds("brain-review-completed").is_none());
        assert!(cx.debug_bounds("brain-open-result").is_some());
    }

    #[gpui::test]
    fn busy_preview_navigation_preserves_source_and_loading_state(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            // A foreground action is in flight. Neither a preview link nor a chosen
            // candidate may claim to have started a source read behind it.
            view.busy = true;
            view.preview = json!({"links":[{
                "url":"tessera://open/notes/target.md", "status":"resolved",
                "candidates":[{"path":"notes/target.md"}]
            }]});
            view.preview_link("tessera://open/notes/target.md", window, cx);
            assert!(!view.source_loading);
            assert!(matches!(view.surface, Surface::Conversation));
            view.open_source("notes/target.md".into(), window, cx);
            assert!(!view.source_loading);
            assert!(view.source_snapshot.is_none());
            view
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn polling_keeps_controls_and_preview_stable_but_reveals_new_thread(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.notice = "Connected".into();
            view.snapshot = json!({"phase":"indeterminate","thread_url":null});
            view.preview = json!({"markdown":"Saved preview"});
            view.source_original = "Source draft".into();
            view.source.reset("Source draft", window, cx);
            // Exercise the same begin path as the timer, before its IO runs.
            view.batch_inner(vec![], true, window, cx);
            assert!(view.poll_in_flight);
            assert!(!view.busy, "automatic polling must not disable every action");
            let same = view.snapshot.clone();
            assert!(!view.finish_batch(vec![("snapshot".into(), Ok(same))], true, 0, window, cx),
                "an unchanged poll must not notify and rebuild the preview");
            assert_eq!(view.notice, "Connected");
            assert_eq!(view.preview["markdown"], "Saved preview");
            assert_eq!(view.source.value(cx).as_ref(), "Source draft");
            assert!(!view.source_loading);
            let linked = json!({"phase":"indeterminate", "thread_url":"http://127.0.0.1:23010/threads/existing"});
            assert!(view.finish_batch(vec![("snapshot".into(), Ok(linked))], true, 0, window, cx));
            assert_eq!(view.snapshot["phase"], "indeterminate");
            assert_eq!(view.snapshot["thread_url"], "http://127.0.0.1:23010/threads/existing");
            assert!(!view.busy);
            view
        });
    }

    #[gpui::test]
    fn foreground_action_supersedes_in_flight_poll(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.batch_inner(vec![], true, window, cx);
            let poll_generation = view.request_generation;
            view.open_source("notes/source.md".into(), window, cx);
            assert!(view.source_loading);
            assert!(matches!(view.surface, Surface::Source));
            assert!(
                view.busy,
                "the foreground action must start despite an in-flight poll"
            );
            assert!(view.request_generation > poll_generation);
            view.snapshot = json!({"phase":"completed","thread_url":"http://localhost/current"});
            view.source_loading = true;
            let before = view.snapshot.clone();
            assert!(!view.finish_batch(
                vec![("snapshot".into(), Ok(json!({"phase":"running"})))],
                true,
                poll_generation,
                window,
                cx
            ));
            assert_eq!(
                view.snapshot, before,
                "late polling must not restore stale state"
            );
            assert!(view.busy, "an old poll cannot finish the foreground action");
            assert!(view.source_loading);
            assert!(!view.finish_batch(
                vec![("snapshot".into(), Err("old failure".into()))],
                true,
                poll_generation,
                window,
                cx
            ));
            assert!(
                view.error.is_none(),
                "late polling errors must also be discarded"
            );
            view
        });
    }

    const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

    #[test]
    fn remote_preview_images_use_supplied_bytes_and_reject_desktop_paths() {
        let images = preview_images(&json!({"assets":[{
            "url":"tessera-asset://fixture", "media_type":"image/png", "content_base64":PIXEL
        }]}))
        .unwrap();
        assert_eq!(
            images["tessera-asset://fixture"].bytes,
            STANDARD.decode(PIXEL).unwrap()
        );
        assert!(matches!(
            preview_image(&images, "tessera-asset://fixture"),
            Some(super::super::MarkdownImage::Source(ImageSource::Image(_)))
        ));
        for url in [
            "file:///tmp/private.png",
            "/home/server/brain/pixel.png",
            "../pixel.png",
            "tessera-asset://unavailable",
        ] {
            assert!(matches!(
                preview_image(&images, url),
                Some(super::super::MarkdownImage::Unavailable)
            ));
        }
        assert!(preview_images(&json!({"assets":[{"url":"file:///tmp/private.png","media_type":"image/png","content_base64":PIXEL}]})).is_err());
    }

    struct RemotePreviewFixture {
        images: BTreeMap<String, Arc<Image>>,
        image_requests: Arc<Mutex<Vec<String>>>,
        links: Arc<Mutex<Vec<String>>>,
    }
    impl Render for RemotePreviewFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let images = self.images.clone();
            let image_requests = self.image_requests.clone();
            let links = self.links.clone();
            div().size_full().child(super::super::markdown_plugins(
                TextView::markdown("remote-preview-fixture",
                    "[Open target](tessera://open/notes/target.md)\n\n> [!note] Remote reference\n> [Nested target](tessera://open/notes/nested.md)\n>\n> ![Remote image](tessera-asset://fixture)\n> [Image neighbor](tessera://open/notes/image-neighbor.md)\n"),
                Arc::new(move |url,_,_,_| links.lock().unwrap().push(url.to_string())),
                Arc::new(move |url| {
                    image_requests.lock().unwrap().push(url.to_string());
                    preview_image(&images, url)
                }),
                SelectionFormat::Plain,
            ))
        }
    }
    #[gpui::test]
    fn shared_native_plugins_render_remote_image_inside_callout(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let image_requests = Arc::new(Mutex::new(Vec::new()));
        let links = Arc::new(Mutex::new(Vec::new()));
        let images = preview_images(&json!({"assets":[{
            "url":"tessera-asset://fixture", "media_type":"image/png", "content_base64":PIXEL
        }]}))
        .unwrap();
        let pixel = images["tessera-asset://fixture"].clone();
        let (_, cx) = cx.add_window_view(|_, _| RemotePreviewFixture {
            images,
            image_requests: image_requests.clone(),
            links: links.clone(),
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        assert!(
            image_requests
                .lock()
                .unwrap()
                .iter()
                .any(|url| url == "tessera-asset://fixture"),
            "nested callout must invoke the supplied byte image resolver"
        );
        cx.update(|window, cx| {
            pixel
                .to_image_data(cx.svg_renderer())
                .expect("valid native image bytes");
            assert!(
                pixel.clone().get_render_image(window, cx).is_some(),
                "supplied remote image bytes must decode into the native rendered image cache"
            );
        });
        // Real native link hit-testing exercises the callback shared by both
        // top-level content and recursively rendered callout bodies.
        cx.simulate_click(point(px(30.), px(10.)), Modifiers::default());
        cx.run_until_parked();
        assert!(links
            .lock()
            .unwrap()
            .iter()
            .any(|url| url == "tessera://open/notes/target.md"));
        for y in (44..132).step_by(4) {
            cx.simulate_click(point(px(40.), px(y as f32)), Modifiers::default());
        }
        cx.run_until_parked();
        assert!(
            links
                .lock()
                .unwrap()
                .iter()
                .any(|url| url == "tessera://open/notes/nested.md"),
            "the link inside the callout must use the shared navigation callback"
        );
        for y in (90..240).step_by(4) {
            cx.simulate_click(point(px(40.), px(y as f32)), Modifiers::default());
        }
        cx.run_until_parked();
        assert!(
            links
                .lock()
                .unwrap()
                .iter()
                .any(|url| url == "tessera://open/notes/image-neighbor.md"),
            "a link sharing the image paragraph must remain interactive"
        );
    }

    #[gpui::test]
    fn native_conflict_keeps_draft_and_inspects_newer_versions_without_discard(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let base = source_snapshot(b"Choice: blue\r\n[[note|alias]]");
        let mut current = source_snapshot(b"Choice: red\r\n[[note|alias]]");
        current["revision"] = json!("sha256:current");
        let proposal = "Choice: green\r\n[[note|alias]]";
        let conflict = json!({"conflict":{"conflict_id":"preserved-conflict", "path":base["path"]},
            "base":base, "current":current, "proposed":source_snapshot(proposal.as_bytes())});
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.load_source(base.clone(), window, cx);
            view.source.reset(proposal, window, cx);
            view.surface = Surface::Source;
            view.show_source_conflict(conflict.clone(), window, cx);
            assert_eq!(view.source.value(cx).as_ref(), proposal);
            assert_eq!(
                view.source_current.read(cx).value().as_ref(),
                "Choice: red\r\n[[note|alias]]"
            );
            assert_eq!(
                view.source_base.read(cx).value().as_ref(),
                "Choice: blue\r\n[[note|alias]]"
            );
            assert_eq!(view.source_snapshot, Some(base.clone()));
            view
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("brain-resolve-source").is_some(),
            "real native conflict UI exposes explicit resolution"
        );
        view.update_in(cx, |view, window, cx| {
            let mut raced = conflict.clone();
            raced["current"]["content_base64"] =
                json!(STANDARD.encode("Choice: yellow\r\n[[note|alias]]"));
            raced["current"]["revision"] = json!("sha256:newer");
            raced["conflict"]["conflict_id"] = json!("second-conflict");
            view.finish_batch(
                vec![("source_write".into(), Ok(json!({"source_conflict":raced})))],
                false,
                view.request_generation,
                window,
                cx,
            );
            assert_eq!(view.source.value(cx).as_ref(), proposal);
            assert_eq!(
                view.source_current.read(cx).value().as_ref(),
                "Choice: yellow\r\n[[note|alias]]"
            );
            assert!(view.dirty(cx));
            assert!(!view.source_loading);
            let mut committed = source_snapshot(proposal.as_bytes());
            committed["revision"] = json!("sha256:resolved");
            view.load_source(committed.clone(), window, cx);
            assert!(view.source_conflict.is_none());
            assert_eq!(view.source_snapshot, Some(committed));
            assert!(!view.dirty(cx));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("brain-resolve-source").is_none());
    }

    #[test]
    fn lost_resolution_ack_reuses_only_the_exact_displayed_revision_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let displayed = source_snapshot(b"Current saved text");
        let initial = source_write_reusing(&displayed, "Deliberate resolution", None);
        let expected = initial.clone();
        let server = thread::spawn(move || {
            for index in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let got: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(got["request"], expected["request"]);
                assert_eq!(got["base"], expected["base"]);
                if index == 0 {
                    continue;
                }
                writeln!(reader.get_mut(),"{}",json!({"schema":SCHEMA,"id":got["id"],"ok":true,"data":{"revision":"sha256:resolved"}})).unwrap();
            }
        });
        assert!(rpc(endpoint, initial.clone()).is_err());
        let retry = source_write_reusing(&displayed, "Deliberate resolution", Some(&initial));
        assert_eq!(retry, initial);
        assert_eq!(rpc(endpoint, retry).unwrap()["revision"], "sha256:resolved");
        server.join().unwrap();
        let mut newer = displayed.clone();
        newer["revision"] = json!("sha256:newer");
        assert_ne!(
            source_write_reusing(&newer, "Deliberate resolution", Some(&initial))["request"]
                ["operation_id"],
            initial["request"]["operation_id"]
        );
        assert_ne!(
            source_write_reusing(&displayed, "Edited again", Some(&initial))["request"]
                ["operation_id"],
            initial["request"]["operation_id"]
        );
    }

    #[test]
    fn source_boundary_preserves_bytes_revision_and_rejects_invalid_utf8() {
        let original = b"\xef\xbb\xbf---\r\ntype: Note\r\n---\n# Source\r\n[[links|alias]]\nlast";
        let snapshot = source_snapshot(original);
        let decoded = decode_source(&snapshot).unwrap();
        let operation = uuid();
        let request = source_write(&snapshot, &decoded, &operation);
        assert_eq!(
            STANDARD
                .decode(request["request"]["content_base64"].as_str().unwrap())
                .unwrap(),
            original
        );
        assert_eq!(request["request"]["expected_revision"], "sha256:fixture");
        assert_eq!(request["request"]["operation_id"], operation);
        assert!(decode_source(&source_snapshot(b"invalid\xff")).is_err());
        assert!(decode_source(&json!({})).is_err());
    }

    struct SourceInput(Entity<TextareaState>);
    impl Render for SourceInput {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.0.clone())
        }
    }
    #[gpui::test]
    fn native_textarea_keeps_bom_crlf_mixed_newlines_and_unchanged_regions(
        cx: &mut TestAppContext,
    ) {
        let mut input = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                gpui_component::init(cx);
                input = Some(cx.new(|cx| TextareaState::new(window, cx).rows(8).soft_wrap(false)));
                cx.new(|_| SourceInput(input.clone().unwrap()))
            })
            .unwrap()
        });
        let input = input.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        for original in [
            "---\ntype: Note\n---\n[[note|alias]]\n",
            "\u{feff}---\r\ntype: Note\r\n---\r\n# Heading\r\n",
            "---\r\ntype: Note\n---\r\n[[note]]\nlast",
        ] {
            cx.update(|window, cx| {
                input.update(cx, |state, cx| {
                    state.set_value(original.to_string(), window, cx);
                    assert_eq!(state.value().as_bytes(), original.as_bytes());
                    let end = original.encode_utf16().count();
                    state.replace_text_in_range(Some(end..end), "\nnew text", window, cx);
                    let expected = format!("{original}\nnew text");
                    assert_eq!(state.value().as_bytes(), expected.as_bytes());
                    let request = source_write(
                        &source_snapshot(original.as_bytes()),
                        &state.value(),
                        "source-operation",
                    );
                    assert_eq!(
                        STANDARD
                            .decode(request["request"]["content_base64"].as_str().unwrap())
                            .unwrap(),
                        expected.as_bytes()
                    );
                })
            });
        }
    }

    #[gpui::test]
    fn native_source_keyboard_navigation_preserves_line_endings(cx: &mut TestAppContext) {
        let mut input = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                gpui_component::init(cx);
                input = Some(cx.new(|cx| TextareaState::new(window, cx).rows(8).soft_wrap(false)));
                cx.new(|_| SourceInput(input.clone().unwrap()))
            })
            .unwrap()
        });
        let input = input.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        #[cfg(target_os = "macos")]
        let (document_end, document_start) = ("cmd-down", "cmd-down cmd-up");
        #[cfg(not(target_os = "macos"))]
        let (document_end, document_start) = ("ctrl-end", "ctrl-end ctrl-home");
        for original in [
            "\u{feff}first\r\nlast\r\n",
            "first\nlast\n",
            "first\r\nnext\nlast",
            "\r\nlast",
        ] {
            let newline = original.find('\n').unwrap();
            let line_end = if original.as_bytes()[newline - 1] == b'\r' {
                newline - 1
            } else {
                newline
            };
            for (keys, offset) in [
                ("end", line_end),
                (document_end, original.len()),
                (document_start, 0),
                ("end right", newline + 1),
                ("end right left", line_end),
            ] {
                cx.update(|window, cx| {
                    input.update(cx, |state, cx| {
                        state.set_value(original, window, cx);
                        state.focus(window, cx);
                    })
                });
                cx.simulate_keystrokes(keys);
                cx.update(|window, cx| {
                    input.update(cx, |state, cx| {
                        state.replace_text_in_range(None, "INSERT", window, cx);
                        let expected =
                            format!("{}INSERT{}", &original[..offset], &original[offset..]);
                        assert_eq!(state.value().as_ref(), expected, "{keys}: {original:?}");
                        let request = source_write(
                            &source_snapshot(original.as_bytes()),
                            &state.value(),
                            "keyboard-operation",
                        );
                        assert_eq!(
                            STANDARD
                                .decode(request["request"]["content_base64"].as_str().unwrap())
                                .unwrap(),
                            expected.as_bytes()
                        );
                    })
                });
            }
        }
    }

    #[test]
    fn lost_capture_reply_is_recovered_by_snapshot_without_repeating_mutation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let seen = observed.clone();
        let goal_id = uuid();
        let remote_goal = goal_id.clone();
        let server = thread::spawn(move || {
            for index in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                seen.lock().unwrap().push(text(&request["op"]));
                if index == 0 {
                    assert_eq!(request["op"], "create_goal");
                    assert_eq!(request["goal"]["id"], remote_goal);
                    continue;
                }
                assert_eq!(request["op"], "snapshot");
                let reply = json!({"schema":SCHEMA,"id":request["id"],"ok":true,"data":{"goal":{"id":remote_goal,"title":"Saved despite lost reply"},"pending_task_operation_id":"pending-task"}});
                writeln!(reader.get_mut(), "{reply}").unwrap();
            }
        });
        assert!(rpc(address, json!({"op":"create_goal","goal":{"id":goal_id}})).is_err());
        let recovered = rpc(address, json!({"op":"snapshot"})).unwrap();
        assert_eq!(recovered["goal"]["id"], goal_id);
        assert_eq!(recovered["pending_task_operation_id"], "pending-task");
        server.join().unwrap();
        assert_eq!(*observed.lock().unwrap(), vec!["create_goal", "snapshot"]);
    }

    #[test]
    fn unrelated_backend_reply_is_not_accepted() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            writeln!(
                reader.get_mut(),
                "{}",
                json!({"schema":SCHEMA,"id":"other-request","ok":true,"data":{}})
            )
            .unwrap();
        });
        assert!(rpc(address, json!({"op":"snapshot"}))
            .unwrap_err()
            .contains("does not match"));
        server.join().unwrap();
    }

    #[test]
    fn chat_reply_limit_accepts_maximum_escaped_output_with_context_overhead() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let workspace = json!({"brain_id":"fixture","root":"/isolated-fixture"});
        let expected_workspace = workspace.clone();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["op"], "chat_get");
            assert_eq!(request["expected_workspace"], expected_workspace);
            let payload = json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":{
                "messages":[{"role":"assistant","text":"\u{0001}".repeat(8*1024*1024)}],
                "context_snapshot":{"snapshot":{"inputs":[{"text":"x".repeat(8*1024*1024-1024)}]}},
                "turn_contexts":[{"retained_summary_overhead":"s".repeat(17*1024)}]
            }});
            let mut bytes = serde_json::to_vec(&payload).unwrap();
            bytes.push(b'\n');
            assert!(bytes.len() as u64 > MAX_REPLY && (bytes.len() as u64) < MAX_PREVIEW_REPLY);
            reader.get_mut().write_all(&bytes).unwrap();
            bytes.len()
        });
        let data = rpc_guarded(
            address,
            json!({"op":"chat_get","conversation_id":"fixture"}),
            Some(&workspace),
        )
        .unwrap();
        let assistant = data["messages"][0]["text"].as_str().unwrap();
        assert_eq!(assistant.len(), 8 * 1024 * 1024);
        assert!(assistant.bytes().all(|byte| byte == 1));
        assert_eq!(
            data["context_snapshot"]["snapshot"]["inputs"][0]["text"]
                .as_str()
                .unwrap()
                .len(),
            8 * 1024 * 1024 - 1024
        );
        assert!(server.join().unwrap() as u64 > MAX_REPLY);
    }

    // Stream a valid JSON reply past the reader's boundary without allocating
    // a second 128 MiB payload. The peer may close as soon as it rejects it.
    fn overbound_rpc_reply(operation: &str, payload_bytes: u64) -> (String, u64) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected_operation = operation.to_string();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["op"], expected_operation);
            let prefix = format!(
                "{{\"schema\":{},\"id\":{},\"ok\":true,\"data\":{{\"text\":\"",
                request["schema"], request["id"]
            );
            reader.get_mut().write_all(prefix.as_bytes()).unwrap();
            let mut sent = prefix.len() as u64;
            let chunk = [b'x'; 64 * 1024];
            let mut remaining = payload_bytes;
            while remaining > 0 {
                let count = remaining.min(chunk.len() as u64) as usize;
                match reader.get_mut().write(&chunk[..count]) {
                    Ok(0) => break,
                    Ok(count) => {
                        remaining -= count as u64;
                        sent += count as u64;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                        ) =>
                    {
                        break
                    }
                    Err(error) => panic!("unexpected fixture write failure: {error}"),
                }
            }
            if remaining == 0 {
                let _ = reader.get_mut().write_all(b"\"}}\n");
            }
            sent
        });
        let error = rpc_guarded(address, json!({"op":operation}), None).unwrap_err();
        (error, server.join().unwrap())
    }

    #[test]
    fn chat_reply_limit_rejects_over_128_mib_and_preserves_other_rpc_limit() {
        let (error, sent) = overbound_rpc_reply("chat_get", MAX_PREVIEW_REPLY + 1);
        assert!(error.contains("Incomplete backend reply"), "{error}");
        assert!(
            sent > MAX_PREVIEW_REPLY,
            "the peer actually supplied the overbound bytes"
        );
        let (error, sent) = overbound_rpc_reply("snapshot", MAX_REPLY + 1);
        assert!(error.contains("Incomplete backend reply"), "{error}");
        assert!(
            sent > MAX_REPLY,
            "the ordinary RPC still reaches its 16 MiB bound"
        );
    }
}
