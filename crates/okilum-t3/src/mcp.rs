//! [`ManagerGateway`] over T3's MCP endpoint: MCP Streamable HTTP, JSON-RPC
//! 2.0, protocol 2025-06-18, with the OAuth bearer token from [`crate::oauth`].

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::environment::Environment;
use crate::gateway::{GatewayError, GatewayResult, GatewayStatus, ManagerGateway};
use crate::token::{StoredToken, TokenFreshness};
use crate::types::{
    Capabilities, ModelSelection, OutgoingMessage, Project, SendReceipt, ThreadConfiguration,
    ThreadPage, ThreadQuery, ThreadSummary,
};
use crate::unix_now;

const PROTOCOL_VERSION: &str = "2025-06-18";
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const PAGE_SIZE: u64 = 100;
/// A paginated listing stops here even if the server keeps offering pages.
const MAX_PAGES: usize = 50;

pub struct McpGateway {
    env: Environment,
    http: Client,
    access: crate::NetworkAccess,
    token: Mutex<Option<StoredToken>>,
    /// `None` until `initialize` succeeded; then the session id, if T3 issued one.
    session: Mutex<Option<Session>>,
    /// The last failure that says something about the connection, and the
    /// token it was seen with. A new token clears it.
    observed: Mutex<Option<(String, GatewayStatus)>>,
    next_id: AtomicU64,
}

// Discovery is bound to the initialized session and credential. A new pairing
// or a dropped session gets a fresh catalog; polling reuses it.
#[derive(Clone)]
struct Session {
    id: Option<String>,
    tools: BTreeSet<String>,
}
const REQUIRED_TOOLS: &[&str] = &["t3_environment_read", "t3_thread_read", "t3_thread_send"];
impl Session {
    fn check_required(&self) -> GatewayResult<()> {
        for &tool in REQUIRED_TOOLS {
            if !self.tools.contains(tool) {
                return Err(GatewayError::Compatibility { tool: tool.into() });
            }
        }
        Ok(())
    }
}

/// Why one HTTP round trip failed.
enum Failure {
    /// T3 no longer knows the MCP session (404 with a session id): start a new one.
    SessionGone,
    Gateway(GatewayError),
}

impl From<GatewayError> for Failure {
    fn from(error: GatewayError) -> Self {
        Failure::Gateway(error)
    }
}

impl McpGateway {
    pub fn new(env: Environment, token: Option<StoredToken>) -> anyhow::Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("okilum/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            // A bearer token must not follow a redirect to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            env,
            http,
            access: crate::NetworkAccess::current(),
            token: Mutex::new(token),
            session: Mutex::new(None),
            observed: Mutex::new(None),
            next_id: AtomicU64::new(1),
        })
    }

    pub fn with_access(mut self, access: crate::NetworkAccess) -> Self {
        self.access = access;
        self
    }

    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// Replaces the token after (re)connecting, or forgets it with `None`.
    pub fn set_token(&self, token: Option<StoredToken>) {
        *self.token.lock().unwrap() = token;
        *self.session.lock().unwrap() = None;
        *self.observed.lock().unwrap() = None;
    }

    /// Verify the saved credential with initialize + tools/list, without
    /// invoking a feature or starting browser authorization.
    pub fn connect(&self) -> GatewayResult<()> {
        if !self.access.allowed() {
            return Err(GatewayError::NotConnected);
        }
        let token = self.current_token()?;
        let result = self
            .ensure_session(&token)
            .and_then(|session| session.check_required());
        self.observe(&token, &result);
        result
    }

    fn observe(&self, token: &StoredToken, result: &GatewayResult<impl Sized>) {
        let observed = match result {
            Err(GatewayError::Revoked | GatewayError::InvalidCredential) => {
                Some(GatewayStatus::Revoked)
            }
            Err(GatewayError::Unreachable(detail)) => Some(GatewayStatus::Unreachable {
                detail: detail.clone(),
            }),
            _ => None,
        };
        *self.observed.lock().unwrap() =
            observed.map(|status| (token.access_token.clone(), status));
    }

    fn current_token(&self) -> GatewayResult<StoredToken> {
        let token = self
            .token
            .lock()
            .unwrap()
            .clone()
            .ok_or(GatewayError::NotConnected)?;
        if token.freshness(unix_now()) == TokenFreshness::Expired {
            return Err(GatewayError::Expired);
        }
        Ok(token)
    }

    fn call<T: DeserializeOwned>(&self, tool: &str, arguments: Value) -> GatewayResult<T> {
        let value = self.call_value(tool, arguments)?;
        serde_json::from_value(value)
            .map_err(|error| GatewayError::Protocol(format!("{tool} result: {error}")))
    }

    fn call_value(&self, tool: &str, arguments: Value) -> GatewayResult<Value> {
        let token = self.current_token()?;
        let params = json!({ "name": tool, "arguments": arguments });
        let result = self.with_session(&token, |session| {
            session.check_required()?;
            if !session.tools.contains(tool) {
                return Err(GatewayError::Unsupported { tool: tool.into() }.into());
            }
            self.request(&token, session.id.as_deref(), "tools/call", params.clone())
        });
        self.observe(&token, &result);
        tool_result(result?)
    }

    /// Runs `exchange` in an initialised session, starting a new session
    /// once if T3 has dropped the old one.
    fn with_session(
        &self,
        token: &StoredToken,
        exchange: impl Fn(&Session) -> Result<Value, Failure>,
    ) -> GatewayResult<Value> {
        for _ in 0..2 {
            let session = self.ensure_session(token)?;
            match exchange(&session) {
                Ok(value) => return Ok(value),
                Err(Failure::Gateway(error)) => return Err(error),
                Err(Failure::SessionGone) => *self.session.lock().unwrap() = None,
            }
        }
        Err(GatewayError::Protocol(
            "T3 dropped the MCP session twice in a row".into(),
        ))
    }

    fn ensure_session(&self, token: &StoredToken) -> GatewayResult<Session> {
        let mut session = self.session.lock().unwrap();
        if let Some(id) = session.as_ref() {
            return Ok(id.clone());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "okilum", "version": env!("CARGO_PKG_VERSION") },
            },
        });
        let (reply, session_id) = self
            .post(token, None, false, &body)
            .map_err(gateway_error)?;
        rpc_result(reply, id, "initialize")?;
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        self.post(token, session_id.as_deref(), true, &note)
            .map_err(gateway_error)?;
        let tools = self.discover_tools(token, session_id.as_deref())?;
        let connected = Session {
            id: session_id,
            tools,
        };
        *session = Some(connected.clone());
        Ok(connected)
    }

    fn discover_tools(
        &self,
        token: &StoredToken,
        session: Option<&str>,
    ) -> GatewayResult<BTreeSet<String>> {
        #[derive(Deserialize)]
        struct Tool {
            name: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Tools {
            tools: Vec<Tool>,
            next_cursor: Option<String>,
        }
        let mut names = BTreeSet::new();
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        for _ in 0..MAX_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
            let reply = self
                .request(token, session, "tools/list", params)
                .map_err(gateway_error)?;
            let page: Tools = serde_json::from_value(reply)
                .map_err(|e| GatewayError::Protocol(format!("tools/list: {e}")))?;
            names.extend(page.tools.into_iter().map(|tool| tool.name));
            match page.next_cursor {
                Some(next) => {
                    if !seen.insert(next.clone()) {
                        return Err(GatewayError::Protocol(
                            "tools/list cursor did not advance".into(),
                        ));
                    }
                    cursor = Some(next);
                }
                None => return Ok(names),
            }
        }
        Err(GatewayError::Protocol(
            "tools/list exceeds the discovery page limit".into(),
        ))
    }

    fn request(
        &self,
        token: &StoredToken,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, Failure> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let (reply, _) = self.post(token, session, true, &body)?;
        Ok(rpc_result(reply, id, method)?)
    }

    /// One POST to `/mcp`. Returns the JSON-RPC message answering `body`
    /// (none for a notification) and the session id T3 sent back.
    fn post(
        &self,
        token: &StoredToken,
        session: Option<&str>,
        initialized: bool,
        body: &Value,
    ) -> Result<(Option<Value>, Option<String>), Failure> {
        if !self.access.allowed() {
            return Err(GatewayError::NotConnected.into());
        }
        let mut request = self
            .http
            .post(self.env.mcp_url())
            .bearer_auth(&token.access_token)
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if initialized {
            request = request.header(PROTOCOL_HEADER, PROTOCOL_VERSION);
        }
        if let Some(session) = session {
            request = request.header(SESSION_HEADER, session);
        }
        let response = request.send().map_err(|error| {
            GatewayError::Unreachable(format!("{:#}", anyhow::Error::new(error.without_url())))
        })?;
        let status = response.status();
        let session_id = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .or_else(|| session.map(str::to_owned));
        let event_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        match status {
            StatusCode::UNAUTHORIZED => {
                let rejected_challenge = response
                    .headers()
                    .get_all(WWW_AUTHENTICATE)
                    .iter()
                    .filter_map(|value| value.to_str().ok())
                    .any(invalid_credential_challenge);
                let body = response.text().unwrap_or_default();
                let rejected = serde_json::from_str::<Value>(&body)
                    .ok()
                    .is_some_and(|body| {
                        ["error", "code"].iter().any(|key| {
                            body.get(key).and_then(Value::as_str) == Some("invalid_mcp_credential")
                        })
                    })
                    || rejected_challenge;
                return Err(if rejected {
                    GatewayError::InvalidCredential
                } else {
                    GatewayError::Revoked
                }
                .into());
            }
            StatusCode::NOT_FOUND if session.is_some() => return Err(Failure::SessionGone),
            StatusCode::FORBIDDEN => {
                return Err(GatewayError::Tool {
                    code: "forbidden".into(),
                    message: snippet(&response.text().unwrap_or_default()),
                }
                .into())
            }
            status if !status.is_success() => {
                return Err(GatewayError::Protocol(format!("HTTP {status}")).into())
            }
            _ => {}
        }
        let Some(id) = body.get("id") else {
            return Ok((None, session_id));
        };
        let reply = if event_stream {
            reply_from_events(BufReader::new(response), id)?
        } else {
            let text = response.text().map_err(|error| {
                GatewayError::Unreachable(format!("{:#}", anyhow::Error::new(error.without_url())))
            })?;
            serde_json::from_str(&text).ok()
        };
        let reply = reply.ok_or_else(|| {
            GatewayError::Protocol("response ended without a matching JSON-RPC reply".into())
        })?;
        Ok((Some(reply), session_id))
    }

    fn paginate<T: DeserializeOwned>(
        &self,
        tool: &str,
        arguments: Value,
        field: &str,
    ) -> GatewayResult<Vec<T>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            next_cursor: Option<u64>,
            #[serde(flatten)]
            rest: serde_json::Map<String, Value>,
        }
        let mut all = Vec::new();
        let mut cursor = 0;
        for _ in 0..MAX_PAGES {
            let mut args = arguments.clone();
            args["cursor"] = cursor.into();
            args["limit"] = PAGE_SIZE.into();
            let page: Page = self.call(tool, args)?;
            let items = page
                .rest
                .get(field)
                .cloned()
                .unwrap_or(Value::Array(Vec::new()));
            let items: Vec<T> = serde_json::from_value(items)
                .map_err(|error| GatewayError::Protocol(format!("{tool} {field}: {error}")))?;
            all.extend(items);
            match page.next_cursor {
                Some(next) if next > cursor => cursor = next,
                None => return Ok(all),
                Some(_) => {
                    return Err(GatewayError::Protocol(format!(
                        "{tool}: pagination cursor did not advance"
                    )))
                }
            }
        }
        Err(GatewayError::Protocol(format!(
            "{tool}: listing exceeds {MAX_PAGES} pages"
        )))
    }
}

impl ManagerGateway for McpGateway {
    fn start_manager(&self, project_id: &str, title: &str, message: &str) -> GatewayResult<String> {
        let result = self.call_value(
            "t3_thread_launch",
            json!({
                "projectId": project_id, "title": title, "message": message,
                "workspaceStrategy": {"type": "root"}
            }),
        )?;
        if result.get("projectId").and_then(Value::as_str) != Some(project_id) {
            return Err(GatewayError::Protocol(
                "Created manager project is unconfirmed; do not start another".into(),
            ));
        }
        result
            .get("threadId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                GatewayError::Protocol(
                    "Manager launch receipt has no thread id; do not retry".into(),
                )
            })
    }

    fn server_identity(&self) -> GatewayResult<crate::ServerIdentity> {
        self.call("t3_environment_read", json!({}))
    }

    fn token_expires_at(&self) -> Option<u64> {
        self.token
            .lock()
            .unwrap()
            .as_ref()
            .map(|token| token.expires_at)
    }

    fn status(&self) -> GatewayStatus {
        let Some(token) = self.token.lock().unwrap().clone() else {
            return GatewayStatus::NotConnected;
        };
        if token.freshness(unix_now()) == TokenFreshness::Expired {
            return GatewayStatus::Expired;
        }
        if let Some((seen_with, status)) = self.observed.lock().unwrap().as_ref() {
            if *seen_with == token.access_token {
                return status.clone();
            }
        }
        match token.freshness(unix_now()) {
            TokenFreshness::Valid => GatewayStatus::Connected {
                expires_at: token.expires_at,
            },
            TokenFreshness::ExpiresSoon => GatewayStatus::ExpiresSoon {
                expires_at: token.expires_at,
            },
            TokenFreshness::Expired => GatewayStatus::Expired,
        }
    }

    fn list_projects(&self) -> GatewayResult<Vec<Project>> {
        self.paginate("t3_project_list", json!({}), "projects")
    }

    fn read_project(&self, project_id: &str) -> GatewayResult<Project> {
        self.call("t3_project_read", json!({ "projectId": project_id }))
    }

    fn set_project_icon(
        &self,
        project_id: &str,
        icon: Option<&crate::projects::icon::ProjectIcon>,
    ) -> GatewayResult<Project> {
        // `null` clears the override; omitted fields stay as they are.
        let args = json!({ "projectId": project_id, "projectIcon": icon });
        self.call_value("t3_project_update", args)?;
        let project = self.read_project(project_id)?;
        if project.project_icon.as_ref() != icon {
            return Err(GatewayError::Protocol(
                "T3 did not keep the project icon".into(),
            ));
        }
        Ok(project)
    }

    /// `t3_project_create`; the project is read back from T3's list by
    /// its workspace root, so the tool's own result shape does not matter.
    fn create_project(&self, title: &str, workspace_root: &str) -> GatewayResult<Project> {
        let args = json!({ "title": title, "workspaceRoot": workspace_root });
        self.call_value("t3_project_create", args)?;
        let root = workspace_root.trim_end_matches('/');
        self.list_projects()?
            .into_iter()
            .find(|project| project.workspace_root.trim_end_matches('/') == root)
            .ok_or_else(|| {
                GatewayError::Protocol(format!("T3 does not list a project at {workspace_root}"))
            })
    }

    fn list_threads(&self, project_id: &str) -> GatewayResult<Vec<ThreadSummary>> {
        let args = json!({ "projectId": project_id, "includeSubagents": true });
        self.paginate("t3_thread_list", args, "threads")
    }

    fn read_thread(&self, query: &ThreadQuery) -> GatewayResult<ThreadPage> {
        self.call("t3_thread_read", query.arguments())
    }

    fn send_message(&self, message: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        self.call("t3_thread_send", message.arguments())
    }

    fn pending_requests(&self, thread_id: &str) -> GatewayResult<Vec<String>> {
        let pending = self.call_value(
            "t3_pending_request",
            json!({ "action": "list", "threadId": thread_id }),
        )?;
        // The consolidated tool promises ids, not a count or question bodies.
        // Keep the previous requestIds response and tolerate the equivalent
        // bare-array/ids projections. Unknown shapes remain visibly unavailable.
        let ids = pending
            .get("requestIds")
            .or_else(|| pending.get("ids"))
            .unwrap_or(&pending);
        serde_json::from_value(ids.clone())
            .map_err(|error| GatewayError::Protocol(format!("t3_pending_request ids: {error}")))
    }

    fn capabilities(&self) -> GatewayResult<Capabilities> {
        self.call("orchestrator_capabilities", json!({}))
    }

    fn thread_configuration(&self, thread_id: &str) -> GatewayResult<ThreadConfiguration> {
        self.call("t3_thread_configure", json!({ "threadId": thread_id }))
    }

    fn configure_model(
        &self,
        thread_id: &str,
        selection: &ModelSelection,
    ) -> GatewayResult<ThreadConfiguration> {
        let args = json!({ "threadId": thread_id, "modelSelection": selection });
        self.call_value("t3_thread_configure", args)?;
        self.thread_configuration(thread_id)
    }
}

// Challenge parameters belong to their authentication scheme. Commas inside
// quoted values are data, so a realm cannot manufacture an error parameter.
fn invalid_credential_challenge(header: &str) -> bool {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (index, ch) in header.char_indices() {
        if escaped {
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch == ',' && !quoted {
            parts.push(header[start..index].trim());
            start = index + 1;
        }
    }
    if quoted || escaped {
        return false;
    }
    parts.push(header[start..].trim());
    let mut bearer = false;
    for part in parts {
        let boundary = part
            .find(|ch: char| ch.is_ascii_whitespace() || ch == '=')
            .unwrap_or(part.len());
        let (name, tail) = part.split_at(boundary);
        let tail = tail.trim_start();
        let parameter = if tail.starts_with('=') {
            part
        } else {
            bearer = name.eq_ignore_ascii_case("Bearer");
            tail
        };
        if bearer {
            if let Some((name, value)) = parameter.split_once('=') {
                let value = value.trim();
                let value = value
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(value);
                if name.trim().eq_ignore_ascii_case("error") && value == "invalid_mcp_credential" {
                    return true;
                }
            }
        }
    }
    false
}

fn gateway_error(failure: Failure) -> GatewayError {
    match failure {
        Failure::Gateway(error) => error,
        Failure::SessionGone => GatewayError::Protocol("T3 dropped the MCP session".into()),
    }
}

/// The `result` of a JSON-RPC reply, or its `error` as a protocol failure.
fn rpc_result(reply: Option<Value>, id: u64, method: &str) -> GatewayResult<Value> {
    let reply = reply.ok_or_else(|| GatewayError::Protocol(format!("{method}: empty reply")))?;
    if reply.get("id") != Some(&Value::from(id)) {
        return Err(GatewayError::Protocol(format!(
            "{method}: reply for another request"
        )));
    }
    if let Some(error) = reply.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        return Err(GatewayError::Protocol(format!("{method}: {message}")));
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| GatewayError::Protocol(format!("{method}: reply without a result")))
}

/// The structured payload of a `tools/call` result, or T3's failure.
fn tool_result(result: Value) -> GatewayResult<Value> {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        })
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str);
    let payload = result
        .get("structuredContent")
        .filter(|value| !value.is_null())
        .cloned()
        .or_else(|| text.and_then(|text| serde_json::from_str(text).ok()));
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let failure = payload.as_ref().map(|payload| {
            payload
                .get("error")
                .or_else(|| payload.get("failure"))
                .unwrap_or(payload)
        });
        let field = |name: &str| {
            failure
                .and_then(|failure| failure.get(name))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        return Err(GatewayError::Tool {
            code: field("code").unwrap_or_else(|| "error".into()),
            message: field("message")
                .or(text.map(str::to_owned))
                .unwrap_or_default(),
        });
    }
    payload.ok_or_else(|| GatewayError::Protocol("tool result without content".into()))
}

/// The JSON-RPC message with `id` in a `text/event-stream` body.
fn reply_from_events(mut body: impl BufRead, id: &Value) -> GatewayResult<Option<Value>> {
    let mut data = Vec::new();
    loop {
        let mut line = String::new();
        let read = body
            .read_line(&mut line)
            .map_err(|error| GatewayError::Unreachable(error.to_string()))?;
        if read == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if let Ok(message) = serde_json::from_str::<Value>(&data.join("\n")) {
                if message.get("id") == Some(id) {
                    return Ok(Some(message));
                }
            }
            data.clear();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
    }
}

fn snippet(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &trimmed[..cut]),
        None => trimmed.to_owned(),
    }
}

#[cfg(test)]
mod tests;
