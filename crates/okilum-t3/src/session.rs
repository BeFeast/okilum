//! [`ManagerGateway`] over T3's own web session: the browser-session cookie
//! the Manager panel's web view signed in with (#1627).
//!
//! The Manager panel is T3's web app in a web view. Pairing it ("Okilum
//! Sprint M4" in T3's client list) gives that web view a session cookie and
//! nothing else: no OAuth token reaches the keychain, and T3's `/mcp`
//! accepts only MCP client sessions. Project reads and icon writes therefore
//! go where T3's web app sends them, `GET /api/projects` and
//! `POST /api/projects/mutate` (`project.update`), with the same cookie.
//! Read-only Board thread summaries use its `GET /api/orchestration/shell`.
//! The cookie is read from the web view's store on the first call (the
//! panel need not be open) and again after T3 refuses it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE, COOKIE};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::environment::Environment;
use crate::gateway::{GatewayError, GatewayResult, GatewayStatus, ManagerGateway};
use crate::types::{
    Capabilities, ModelSelection, OutgoingMessage, Project, SendReceipt, ThreadConfiguration,
    ThreadPage, ThreadQuery, ThreadSummary,
};
mod threads;

/// How long connecting may take: reading the cookie store, then asking T3
/// which cookie is a live session.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// T3 names its session cookie `t3_session` or `t3_session_<instance>`.
const COOKIE_PREFIX: &str = "t3_session";

/// One cookie from the web view's store. The value is a session secret:
/// `Debug` does not show it.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionCookie {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for SessionCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCookie")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Where the paired session lives: on macOS the Manager web view's cookie
/// store. Called off the UI thread; must return within `timeout`.
pub trait SessionSource: Send + Sync {
    fn cookies(&self, host: &str, timeout: Duration) -> anyhow::Result<Vec<SessionCookie>>;
}

/// No paired web view on this platform.
pub struct NoSessionSource;

impl SessionSource for NoSessionSource {
    fn cookies(&self, _: &str, _: Duration) -> anyhow::Result<Vec<SessionCookie>> {
        Ok(Vec::new())
    }
}

pub struct SessionGateway {
    env: Environment,
    http: Client,
    source: Arc<dyn SessionSource>,
    /// The cookie T3 accepted last; `None` until the first call connects.
    active: Mutex<Option<SessionCookie>>,
    observed: Mutex<Option<GatewayStatus>>,
}

/// Why a request with one cookie failed.
enum Failure {
    /// 401: T3 does not (or no longer) know this session.
    Refused,
    Gateway(GatewayError),
}

impl SessionGateway {
    pub fn new(env: Environment, source: Arc<dyn SessionSource>) -> anyhow::Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("okilum/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(30))
            // A session cookie must not follow a redirect to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            env,
            http,
            source,
            active: Mutex::new(None),
            observed: Mutex::new(None),
        })
    }

    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// Whether Okilum holds a paired session for this server, connecting
    /// now if it has not yet. Errors as [`ManagerGateway`] calls report them.
    pub fn connect(&self) -> GatewayResult<()> {
        self.session().map(drop)
    }

    fn not_paired(&self) -> GatewayError {
        GatewayError::NotPaired {
            origin: self.env.origin(),
        }
    }

    fn observe(&self, result: &GatewayResult<impl Sized>) {
        let status = match result {
            Ok(_) => None,
            Err(GatewayError::NotPaired { .. }) => Some(GatewayStatus::NotConnected),
            Err(GatewayError::Revoked) => Some(GatewayStatus::Revoked),
            Err(GatewayError::Unreachable(detail)) => Some(GatewayStatus::Unreachable {
                detail: detail.clone(),
            }),
            Err(_) => return,
        };
        *self.observed.lock().unwrap() = status;
    }

    /// The accepted cookie, reading the store and asking T3 when none is.
    fn session(&self) -> GatewayResult<SessionCookie> {
        if let Some(cookie) = self.active.lock().unwrap().clone() {
            return Ok(cookie);
        }
        let host = self.env.host();
        let cookies = self
            .source
            .cookies(&host, CONNECT_TIMEOUT)
            .map_err(|error| {
                GatewayError::Unreachable(format!("the T3 session store: {error:#}"))
            })?;
        let mut candidates: Vec<SessionCookie> = cookies
            .into_iter()
            .filter(|cookie| {
                cookie.name == COOKIE_PREFIX
                    || cookie
                        .name
                        .strip_prefix(COOKIE_PREFIX)
                        .is_some_and(|rest| rest.starts_with('_'))
            })
            .filter(|cookie| !cookie.value.is_empty())
            .collect();
        // The instance cookie before the legacy one.
        candidates.sort_by_key(|cookie| cookie.name == COOKIE_PREFIX);
        let mut refused = false;
        for cookie in candidates {
            match self.authenticated(&cookie) {
                Ok(true) => {
                    *self.active.lock().unwrap() = Some(cookie.clone());
                    return Ok(cookie);
                }
                Ok(false) => refused = true,
                Err(error) => return Err(error),
            }
        }
        Err(if refused {
            GatewayError::Revoked
        } else {
            self.not_paired()
        })
    }

    /// T3's own word on a cookie: `GET /api/auth/session`.
    fn authenticated(&self, cookie: &SessionCookie) -> GatewayResult<bool> {
        #[derive(Deserialize)]
        struct State {
            authenticated: bool,
        }
        let response = self
            .http
            .get(self.env.endpoint("/api/auth/session"))
            .timeout(CONNECT_TIMEOUT)
            .header(ACCEPT, "application/json")
            .header(COOKIE, cookie_header(cookie))
            .send()
            .map_err(|error| GatewayError::Unreachable(error.without_url().to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(false);
        }
        let response = refusal(response).map_err(|failure| match failure {
            Failure::Refused => GatewayError::Revoked,
            Failure::Gateway(error) => error,
        })?;
        let state: State = response
            .json()
            .map_err(|error| GatewayError::Protocol(format!("/api/auth/session: {error}")))?;
        Ok(state.authenticated)
    }

    /// One request with the session; a refused cookie is read again from
    /// the store once (the panel may have signed in since).
    fn send(
        &self,
        request: impl Fn(&SessionCookie) -> reqwest::blocking::RequestBuilder,
    ) -> GatewayResult<Value> {
        let result = (|| {
            for _ in 0..2 {
                let cookie = self.session()?;
                let response = request(&cookie)
                    .header(ACCEPT, "application/json")
                    .header(COOKIE, cookie_header(&cookie))
                    .send()
                    .map_err(|error| GatewayError::Unreachable(error.without_url().to_string()))?;
                match refusal(response) {
                    Ok(response) => {
                        return response
                            .json()
                            .map_err(|error| GatewayError::Protocol(error.to_string()))
                    }
                    Err(Failure::Refused) => *self.active.lock().unwrap() = None,
                    Err(Failure::Gateway(error)) => return Err(error),
                }
            }
            Err(GatewayError::Revoked)
        })();
        self.observe(&result);
        result
    }

    fn projects(&self) -> GatewayResult<Vec<Project>> {
        #[derive(Deserialize)]
        struct Snapshot {
            projects: Vec<Value>,
        }
        let value = self.send(|_| self.http.get(self.env.endpoint("/api/projects")))?;
        let snapshot: Snapshot = serde_json::from_value(value)
            .map_err(|error| GatewayError::Protocol(format!("/api/projects: {error}")))?;
        snapshot
            .projects
            .into_iter()
            // A deleted project stays in T3's snapshot with `deletedAt`.
            .filter(|project| project.get("deletedAt").is_none_or(Value::is_null))
            .map(|project| {
                serde_json::from_value(project)
                    .map_err(|error| GatewayError::Protocol(format!("/api/projects: {error}")))
            })
            .collect()
    }
}

fn cookie_header(cookie: &SessionCookie) -> String {
    format!("{}={}", cookie.name, cookie.value)
}

/// A successful response, or what T3 said when it refused.
fn refusal(response: reqwest::blocking::Response) -> Result<reqwest::blocking::Response, Failure> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Failure::Refused);
    }
    let text = response.text().unwrap_or_default();
    Err(Failure::Gateway(t3_refusal(status.as_u16(), &text)))
}

/// T3's HTTP API errors are tagged JSON: `code` and a `reason`, a
/// `requiredScope` or a `message`.
fn t3_refusal(status: u16, text: &str) -> GatewayError {
    let body: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let field = |name: &str| body.get(name).and_then(Value::as_str).map(str::to_owned);
    let code = field("code")
        .or_else(|| field("_tag"))
        .unwrap_or_else(|| format!("http_{status}"));
    let message = field("message")
        .or_else(|| field("reason"))
        .or_else(|| field("requiredScope").map(|scope| format!("needs the {scope} scope")))
        .unwrap_or_else(|| {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                format!("HTTP {status}")
            } else {
                trimmed.chars().take(200).collect()
            }
        });
    GatewayError::Tool { code, message }
}

fn unsupported<T>(what: &str) -> GatewayResult<T> {
    Err(GatewayError::Protocol(format!(
        "the paired T3 session does not {what}"
    )))
}

impl ManagerGateway for SessionGateway {
    fn status(&self) -> GatewayStatus {
        if let Some(status) = self.observed.lock().unwrap().clone() {
            return status;
        }
        if self.active.lock().unwrap().is_some() {
            // The cookie's lifetime is T3's to know; it renews in the panel.
            GatewayStatus::Connected { expires_at: 0 }
        } else {
            GatewayStatus::NotConnected
        }
    }

    fn server_identity(&self) -> GatewayResult<crate::ServerIdentity> {
        crate::projects::browser_identity(&self.env)
            .map_err(|error| GatewayError::Unreachable(format!("{error:#}")))
    }

    fn list_projects(&self) -> GatewayResult<Vec<Project>> {
        self.projects()
    }

    fn read_project(&self, project_id: &str) -> GatewayResult<Project> {
        self.projects()?
            .into_iter()
            .find(|project| project.id == project_id)
            .ok_or_else(|| GatewayError::Tool {
                code: "project_not_found".into(),
                message: format!("T3 has no project {project_id}"),
            })
    }

    /// T3's web app's own icon save: `project.update` with `projectIcon`
    /// (`null` resets it); omitted fields stay as they are.
    fn set_project_icon(
        &self,
        project_id: &str,
        icon: Option<&crate::projects::icon::ProjectIcon>,
    ) -> GatewayResult<Project> {
        let command = json!({
            "type": "project.update",
            "commandId": uuid::Uuid::new_v4().to_string(),
            "projectId": project_id,
            "projectIcon": icon,
        });
        let url = self.env.endpoint("/api/projects/mutate");
        let value = self.send(|_| {
            self.http
                .post(url.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(command.to_string())
        })?;
        let project: Project = serde_json::from_value(value)
            .map_err(|error| GatewayError::Protocol(format!("/api/projects/mutate: {error}")))?;
        if project.id != project_id || project.project_icon.as_ref() != icon {
            return Err(GatewayError::Protocol(
                "T3 did not keep the project icon".into(),
            ));
        }
        Ok(project)
    }

    /// T3's web app's own «Add project»: `project.create` with a new
    /// project id. T3 answers with the project it registered.
    fn create_project(&self, title: &str, workspace_root: &str) -> GatewayResult<Project> {
        let project_id = uuid::Uuid::new_v4().to_string();
        let command = json!({
            "type": "project.create",
            "commandId": uuid::Uuid::new_v4().to_string(),
            "projectId": project_id,
            "title": title,
            "workspaceRoot": workspace_root,
        });
        let url = self.env.endpoint("/api/projects/mutate");
        let value = self.send(|_| {
            self.http
                .post(url.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(command.to_string())
        })?;
        let project: Project = serde_json::from_value(value)
            .map_err(|error| GatewayError::Protocol(format!("/api/projects/mutate: {error}")))?;
        if project.id != project_id {
            return Err(GatewayError::Protocol(
                "T3 answered the new project with another project".into(),
            ));
        }
        Ok(project)
    }

    fn list_threads(&self, project_id: &str) -> GatewayResult<Vec<ThreadSummary>> {
        let value = self.send(|_| self.http.get(self.env.endpoint("/api/orchestration/shell")))?;
        threads::read(value, project_id)
    }

    fn read_thread(&self, _: &ThreadQuery) -> GatewayResult<ThreadPage> {
        unsupported("read threads")
    }

    fn send_message(&self, _: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        unsupported("send messages")
    }

    fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
        unsupported("read pending requests")
    }

    fn capabilities(&self) -> GatewayResult<Capabilities> {
        unsupported("read capabilities")
    }

    fn thread_configuration(&self, _: &str) -> GatewayResult<ThreadConfiguration> {
        unsupported("read thread configuration")
    }

    fn configure_model(&self, _: &str, _: &ModelSelection) -> GatewayResult<ThreadConfiguration> {
        unsupported("configure models")
    }
}

/// The gateway the Projects page and the icon dialog use for one saved T3
/// connection: projects and icons over the paired web session (the Manager
/// panel's), including read-only thread summaries. Without a paired session an MCP token, if
/// the user connected one, still serves the icons.
pub struct PairedGateway {
    session: SessionGateway,
    mcp: Arc<dyn ManagerGateway>,
}

impl PairedGateway {
    pub fn new(session: SessionGateway, mcp: Arc<dyn ManagerGateway>) -> Self {
        Self { session, mcp }
    }

    fn mcp_ready(&self) -> bool {
        matches!(
            self.mcp.status(),
            GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. }
        )
    }

    /// The session's answer, or MCP's when there is no paired session.
    fn projects_call<T>(
        &self,
        session: impl FnOnce(&SessionGateway) -> GatewayResult<T>,
        mcp: impl FnOnce(&dyn ManagerGateway) -> GatewayResult<T>,
    ) -> GatewayResult<T> {
        match session(&self.session) {
            Err(GatewayError::NotPaired { .. }) if self.mcp_ready() => mcp(&*self.mcp),
            result => result,
        }
    }
}

impl ManagerGateway for PairedGateway {
    fn start_manager(&self, project_id: &str, title: &str, message: &str) -> GatewayResult<String> {
        self.mcp.start_manager(project_id, title, message)
    }

    fn status(&self) -> GatewayStatus {
        self.mcp.status()
    }

    fn server_identity(&self) -> GatewayResult<crate::ServerIdentity> {
        self.mcp.server_identity()
    }

    fn list_projects(&self) -> GatewayResult<Vec<Project>> {
        self.projects_call(|s| s.list_projects(), |m| m.list_projects())
    }

    fn read_project(&self, project_id: &str) -> GatewayResult<Project> {
        self.projects_call(
            |s| s.read_project(project_id),
            |m| m.read_project(project_id),
        )
    }

    fn set_project_icon(
        &self,
        project_id: &str,
        icon: Option<&crate::projects::icon::ProjectIcon>,
    ) -> GatewayResult<Project> {
        self.projects_call(
            |s| s.set_project_icon(project_id, icon),
            |m| m.set_project_icon(project_id, icon),
        )
    }

    fn create_project(&self, title: &str, workspace_root: &str) -> GatewayResult<Project> {
        self.projects_call(
            |s| s.create_project(title, workspace_root),
            |m| m.create_project(title, workspace_root),
        )
    }

    fn list_threads(&self, project_id: &str) -> GatewayResult<Vec<ThreadSummary>> {
        self.projects_call(
            |s| s.list_threads(project_id),
            |m| m.list_threads(project_id),
        )
    }

    fn read_thread(&self, query: &ThreadQuery) -> GatewayResult<ThreadPage> {
        self.mcp.read_thread(query)
    }

    fn send_message(&self, message: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        self.mcp.send_message(message)
    }

    fn pending_requests(&self, thread_id: &str) -> GatewayResult<Vec<String>> {
        self.mcp.pending_requests(thread_id)
    }

    fn capabilities(&self) -> GatewayResult<Capabilities> {
        self.mcp.capabilities()
    }

    fn thread_configuration(&self, thread_id: &str) -> GatewayResult<ThreadConfiguration> {
        self.mcp.thread_configuration(thread_id)
    }

    fn configure_model(
        &self,
        thread_id: &str,
        selection: &ModelSelection,
    ) -> GatewayResult<ThreadConfiguration> {
        self.mcp.configure_model(thread_id, selection)
    }
}

#[cfg(test)]
mod tests;
