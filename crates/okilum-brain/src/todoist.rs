//! Minimal Todoist API v1 task connector. Todoist remains task authority.
//!
//! Mutations use documented Sync command UUID idempotency, not an assumed REST
//! `X-Request-Id` guarantee: https://developer.todoist.com/api/v1/#tag/Sync
//! The caller must durably retain PreparedMutation before execute, reject UUID
//! reuse with different input, and retain its receipt/binding before advancing.
//! Reconcile sends exactly that saved command, never a newly generated command.
//! No method retries automatically. Account access/reminder entitlements remain
//! unverified until a separately authorized live acceptance run.
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderValue, AUTHORIZATION, RETRY_AFTER};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::io::Read;
use std::time::Duration;
use uuid::Uuid;

const SCHEMA: &str = "ai-brain/v1";
const MAX_RESPONSE: u64 = 1024 * 1024;

/// Stable instance_id denotes one configured account; do not change the account
/// behind an existing ID. Token is intentionally neither Debug nor Serialize.
pub struct TodoistConfig {
    pub instance_id: String,
    /// Includes /api/v1, e.g. https://api.todoist.com/api/v1/.
    pub base_url: String,
    token: String,
    pub timeout: Duration,
}

impl TodoistConfig {
    pub fn new(instance_id: String, base_url: String, token: String) -> Self {
        Self {
            instance_id,
            base_url,
            token,
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidConfiguration,
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    RateLimited,
    Rejected,
    Http,
    Transport,
    Malformed,
}

/// Only allowlisted diagnostics are retained. Never include HTTP bodies, URLs,
/// headers or reqwest error strings, which may contain credentials or user data.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoistError {
    pub kind: ErrorKind,
    pub http_status: Option<u16>,
    pub retry_after_secs: Option<u64>,
}
impl fmt::Display for TodoistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Todoist {:?} (HTTP {:?})", self.kind, self.http_status)
    }
}
impl std::error::Error for TodoistError {}
fn err(kind: ErrorKind) -> TodoistError {
    TodoistError {
        kind,
        http_status: None,
        retry_after_secs: None,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Provider-native priority, passed through without UI priority conversion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// Provider parses human dates/recurrence, e.g. {string: "every Monday"}.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<DueInput>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DueInput {
    pub string: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mutation {
    Create {
        fields: TaskFields,
        project_id: Option<String>,
        section_id: Option<String>,
    },
    Update {
        task_id: String,
        fields: TaskFields,
    },
    /// Closes one occurrence. A recurring series is NOT marked complete.
    Close {
        task_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedMutation {
    pub schema: String,
    pub operation_id: String,
    pub goal_id: String,
    pub instance_id: String,
    pub mutation: Mutation,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskBinding {
    pub provider: String,
    pub instance_id: String,
    pub goal_id: String,
    pub external_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationEffect {
    Created,
    Updated,
    OccurrenceClosed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationReceipt {
    pub operation_id: String,
    pub binding: TaskBinding,
    pub effect: MutationEffect,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MutationOutcome {
    Accepted {
        receipt: MutationReceipt,
    },
    Rejected {
        error: TodoistError,
    },
    /// Retain the exact envelope; do not create another operation to retry.
    Indeterminate {
        error: TodoistError,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DueDate {
    pub date: String,
    pub is_recurring: bool,
    #[serde(default)]
    pub string: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub content: String,
    pub description: String,
    pub checked: bool,
    pub is_deleted: bool,
    pub labels: Vec<String>,
    pub priority: u8,
    pub due: Option<DueDate>,
    pub updated_at: Option<String>,
    pub completed_at: Option<String>,
}

/// A snapshot is an observation, not a second task store. A 404 from active-task
/// GET is NotFound, never proof of completion (it may mean deletion/no access).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskObservation {
    pub binding: TaskBinding,
    pub observed_at: String,
    pub task: Task,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct InboxAccount {
    pub id: String,
    pub inbox_project_id: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct InboxPage {
    pub results: Vec<Task>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct Todoist {
    instance_id: String,
    base: Url,
    authorization: HeaderValue,
    client: Client,
}

impl Todoist {
    pub fn new(config: TodoistConfig) -> Result<Self, TodoistError> {
        let mut base =
            Url::parse(&config.base_url).map_err(|_| err(ErrorKind::InvalidConfiguration))?;
        let local = base
            .host_str()
            .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "[::1]");
        if config.instance_id.is_empty()
            || config.token.is_empty()
            || config.timeout.is_zero()
            || (!base.username().is_empty() || base.password().is_some())
            || base.query().is_some()
            || base.fragment().is_some()
            || !(base.scheme() == "https" || (base.scheme() == "http" && local))
        {
            return Err(err(ErrorKind::InvalidConfiguration));
        }
        base.set_path(&format!("{}/", base.path().trim_end_matches('/')));
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", config.token))
            .map_err(|_| err(ErrorKind::InvalidConfiguration))?;
        authorization.set_sensitive(true);
        let client = Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| err(ErrorKind::InvalidConfiguration))?;
        Ok(Self {
            instance_id: config.instance_id,
            base,
            authorization,
            client,
        })
    }

    /// Authenticated identity from the exact captured adapter, bounded like task reads.
    fn user_info(&self) -> Result<Value, TodoistError> {
        let response = self
            .client
            .get(self.endpoint("user")?)
            .header(AUTHORIZATION, self.authorization.clone())
            .send()
            .map_err(|_| err(ErrorKind::Transport))?;
        response_json(response)
    }
    pub fn account_id(&self) -> Result<String, TodoistError> {
        let value = self.user_info()?;
        let id = value["id"]
            .as_str()
            .ok_or_else(|| err(ErrorKind::Malformed))?;
        valid_external_id(id).map_err(|_| err(ErrorKind::Malformed))?;
        Ok(id.to_owned())
    }
    /// Inbox classification is provider identity, never a localized project name.
    pub fn inbox_account(&self) -> Result<InboxAccount, TodoistError> {
        let value = self.user_info()?;
        let account: InboxAccount =
            serde_json::from_value(value).map_err(|_| err(ErrorKind::Malformed))?;
        valid_external_id(&account.id).map_err(|_| err(ErrorKind::Malformed))?;
        valid_external_id(&account.inbox_project_id).map_err(|_| err(ErrorKind::Malformed))?;
        Ok(account)
    }
    /// One explicit GET page; never follows cursors or retries automatically.
    pub fn inbox_page(
        &self,
        project_id: &str,
        cursor: Option<&str>,
    ) -> Result<InboxPage, TodoistError> {
        valid_external_id(project_id)?;
        if cursor.is_some_and(|c| c.is_empty() || c.len() > 4096) {
            return Err(err(ErrorKind::InvalidRequest));
        }
        let mut url = self.endpoint("tasks")?;
        url.query_pairs_mut()
            .append_pair("project_id", project_id)
            .append_pair("limit", "50");
        if let Some(cursor) = cursor {
            url.query_pairs_mut().append_pair("cursor", cursor);
        }
        let response = self
            .client
            .get(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .send()
            .map_err(|_| err(ErrorKind::Transport))?;
        let value = response_json(response)?;
        // Absence is malformed, not proof that the list is complete.
        if !value
            .get("next_cursor")
            .is_some_and(|v| v.is_null() || v.is_string())
        {
            return Err(err(ErrorKind::Malformed));
        }
        let page: InboxPage =
            serde_json::from_value(value).map_err(|_| err(ErrorKind::Malformed))?;
        if page.results.len() > 50
            || page
                .next_cursor
                .as_ref()
                .is_some_and(|c| c.is_empty() || c.len() > 4096)
            || page.results.iter().any(|task| {
                valid_external_id(&task.id).is_err()
                    || task.project_id != project_id
                    || task.checked
                    || task.is_deleted
            })
        {
            return Err(err(ErrorKind::Malformed));
        }
        Ok(page)
    }
    /// This produces no network I/O. Persist the returned exact value first.
    pub fn prepare(
        &self,
        operation_id: String,
        goal_id: String,
        mutation: Mutation,
    ) -> Result<PreparedMutation, TodoistError> {
        let command = PreparedMutation {
            schema: SCHEMA.into(),
            operation_id,
            goal_id,
            instance_id: self.instance_id.clone(),
            mutation,
        };
        self.validate(&command)?;
        Ok(command)
    }

    pub fn associate(
        &self,
        goal_id: String,
        external_id: String,
    ) -> Result<TaskObservation, TodoistError> {
        self.read(&TaskBinding {
            provider: "todoist".into(),
            instance_id: self.instance_id.clone(),
            goal_id,
            external_id,
        })
    }

    pub fn read(&self, binding: &TaskBinding) -> Result<TaskObservation, TodoistError> {
        if binding.provider != "todoist" || binding.instance_id != self.instance_id {
            return Err(err(ErrorKind::InvalidRequest));
        }
        valid_uuid(&binding.goal_id)?;
        valid_external_id(&binding.external_id)?;
        let mut url = self.endpoint("tasks")?;
        url.path_segments_mut()
            .map_err(|_| err(ErrorKind::InvalidConfiguration))?
            .push(&binding.external_id);
        let response = self
            .client
            .get(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .send()
            .map_err(|_| err(ErrorKind::Transport))?;
        let value = response_json(response)?;
        let task: Task = serde_json::from_value(value).map_err(|_| err(ErrorKind::Malformed))?;
        if task.id != binding.external_id {
            return Err(err(ErrorKind::Malformed));
        }
        let observed_at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| err(ErrorKind::Malformed))?;
        Ok(TaskObservation {
            binding: binding.clone(),
            observed_at,
            task,
        })
    }

    pub fn execute(&self, command: &PreparedMutation) -> MutationOutcome {
        if let Err(error) = self.validate(command) {
            return MutationOutcome::Rejected { error };
        }
        let wire = wire_command(command);
        let url = match self.endpoint("sync") {
            Ok(url) => url,
            Err(error) => return MutationOutcome::Rejected { error },
        };
        let response = self
            .client
            .post(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .form(&[("commands", Value::Array(vec![wire]).to_string())])
            .send();
        let value = match response
            .map_err(|_| err(ErrorKind::Transport))
            .and_then(response_json)
        {
            Ok(value) => value,
            Err(error) => return classify_mutation_error(error),
        };
        match value
            .get("sync_status")
            .and_then(|v| v.get(&command.operation_id))
        {
            Some(Value::String(status)) if status == "ok" => {}
            Some(Value::Object(error))
                if error.get("error_code").and_then(Value::as_i64).is_some() =>
            {
                let status = error
                    .get("http_code")
                    .and_then(Value::as_u64)
                    .and_then(|v| u16::try_from(v).ok());
                let failure = http_error(
                    status,
                    error
                        .get("error_extra")
                        .and_then(|v| v.get("retry_after"))
                        .and_then(Value::as_u64),
                );
                // An explicit command error is not a successful receipt. Server
                // uncertainty remains indeterminate even in an HTTP 200 body.
                return if status.is_some_and(|s| s >= 500 || s == 408) {
                    MutationOutcome::Indeterminate { error: failure }
                } else {
                    MutationOutcome::Rejected { error: failure }
                };
            }
            _ => {
                return MutationOutcome::Indeterminate {
                    error: err(ErrorKind::Malformed),
                }
            }
        }
        let (task_id, effect) = match &command.mutation {
            Mutation::Create { .. } => {
                let Some(id) = value
                    .get("temp_id_mapping")
                    .and_then(|m| m.get(&command.operation_id))
                    .and_then(Value::as_str)
                else {
                    return MutationOutcome::Indeterminate {
                        error: err(ErrorKind::Malformed),
                    };
                };
                if valid_external_id(id).is_err() {
                    return MutationOutcome::Indeterminate {
                        error: err(ErrorKind::Malformed),
                    };
                }
                (id.to_owned(), MutationEffect::Created)
            }
            Mutation::Update { task_id, .. } => (task_id.clone(), MutationEffect::Updated),
            Mutation::Close { task_id } => (task_id.clone(), MutationEffect::OccurrenceClosed),
        };
        MutationOutcome::Accepted {
            receipt: MutationReceipt {
                operation_id: command.operation_id.clone(),
                binding: TaskBinding {
                    provider: "todoist".into(),
                    instance_id: self.instance_id.clone(),
                    goal_id: command.goal_id.clone(),
                    external_id: task_id,
                },
                effect,
            },
        }
    }

    /// Explicit provider-idempotent replay of the saved command, including its
    /// UUID/temp ID. Missing mapping cannot be repaired by making a new task.
    pub fn reconcile(&self, saved: &PreparedMutation) -> MutationOutcome {
        self.execute(saved)
    }

    fn endpoint(&self, suffix: &str) -> Result<Url, TodoistError> {
        self.base
            .join(suffix)
            .map_err(|_| err(ErrorKind::InvalidConfiguration))
    }

    fn validate(&self, command: &PreparedMutation) -> Result<(), TodoistError> {
        if command.schema != SCHEMA || command.instance_id != self.instance_id {
            return Err(err(ErrorKind::InvalidRequest));
        }
        valid_uuid(&command.operation_id)?;
        valid_uuid(&command.goal_id)?;
        let fields = match &command.mutation {
            Mutation::Create {
                fields,
                project_id,
                section_id,
            } => {
                if fields.content.as_ref().is_none_or(|s| s.trim().is_empty()) {
                    return Err(err(ErrorKind::InvalidRequest));
                }
                for id in [project_id, section_id].into_iter().flatten() {
                    valid_external_id(id)?;
                }
                Some(fields)
            }
            Mutation::Update { fields, task_id } => {
                valid_external_id(task_id)?;
                Some(fields)
            }
            Mutation::Close { task_id } => {
                valid_external_id(task_id)?;
                None
            }
        };
        if let Some(fields) = fields {
            if fields.priority.is_some_and(|p| !(1..=4).contains(&p))
                || fields.content.as_ref().is_some_and(|s| s.trim().is_empty())
                || fields
                    .due
                    .as_ref()
                    .is_some_and(|d| d.string.trim().is_empty())
            {
                return Err(err(ErrorKind::InvalidRequest));
            }
        }
        Ok(())
    }
}

fn valid_uuid(id: &str) -> Result<(), TodoistError> {
    if Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        Ok(())
    } else {
        Err(err(ErrorKind::InvalidRequest))
    }
}
fn valid_external_id(id: &str) -> Result<(), TodoistError> {
    if id.is_empty() || id == "." || id == ".." || id.chars().any(|c| c.is_control()) {
        Err(err(ErrorKind::InvalidRequest))
    } else {
        Ok(())
    }
}
fn wire_command(command: &PreparedMutation) -> Value {
    let (kind, mut args) = match &command.mutation {
        Mutation::Create { fields, .. } => ("item_add", json!(fields)),
        Mutation::Update { fields, .. } => ("item_update", json!(fields)),
        Mutation::Close { .. } => ("item_close", json!({})),
    };
    let mut wire = json!({"type": kind, "uuid": command.operation_id});
    match &command.mutation {
        Mutation::Create {
            project_id,
            section_id,
            ..
        } => {
            // ID namespaces differ: this stable temporary resource ID and the
            // command UUID may share bytes without adding another identity.
            wire["temp_id"] = json!(command.operation_id);
            if let Some(id) = project_id {
                args["project_id"] = json!(id);
            }
            if let Some(id) = section_id {
                args["section_id"] = json!(id);
            }
        }
        Mutation::Update { task_id, .. } | Mutation::Close { task_id } => {
            args["id"] = json!(task_id)
        }
    }
    wire["args"] = args;
    wire
}

fn response_json(response: Response) -> Result<Value, TodoistError> {
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut bytes = Vec::new();
    if response
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Err(err(ErrorKind::Transport));
    }
    if !(200..300).contains(&status) {
        let body_retry = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("error_extra")?.get("retry_after")?.as_u64());
        return Err(http_error(Some(status), retry.or(body_retry)));
    }
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(err(ErrorKind::Malformed));
    }
    serde_json::from_slice(&bytes).map_err(|_| err(ErrorKind::Malformed))
}
fn http_error(status: Option<u16>, retry_after_secs: Option<u64>) -> TodoistError {
    let kind = match status {
        Some(401) => ErrorKind::Unauthorized,
        Some(403) => ErrorKind::Forbidden,
        Some(404) => ErrorKind::NotFound,
        Some(429) => ErrorKind::RateLimited,
        Some(400 | 422) | None => ErrorKind::Rejected,
        _ => ErrorKind::Http,
    };
    TodoistError {
        kind,
        http_status: status,
        retry_after_secs,
    }
}
fn classify_mutation_error(error: TodoistError) -> MutationOutcome {
    if matches!(error.http_status, Some(400 | 401 | 403 | 404 | 422 | 429)) {
        MutationOutcome::Rejected { error }
    } else {
        MutationOutcome::Indeterminate { error }
    }
}
