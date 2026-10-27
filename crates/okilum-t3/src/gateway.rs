use crate::types::{
    Capabilities, ModelSelection, OutgoingMessage, Project, SendReceipt, ThreadConfiguration,
    ThreadPage, ThreadQuery, ThreadSummary,
};

/// What the UI shows about the connection to an environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatewayStatus {
    /// No token: the user has not connected this environment yet.
    NotConnected,
    Connected {
        expires_at: u64,
    },
    ExpiresSoon {
        expires_at: u64,
    },
    /// The token's lifetime is over; reconnect.
    Expired,
    /// The server refused the token (revoked on the server).
    Revoked,
    /// The last request did not reach the server.
    Unreachable {
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatewayError {
    NotConnected,
    /// No T3 session from the Manager panel's sign-in for this server.
    NotPaired {
        origin: String,
    },
    Expired,
    Revoked,
    Unreachable(String),
    /// T3 refused the operation; `code` is T3's failure code, for example
    /// `capability_denied` or `thread_not_found`.
    Tool {
        code: String,
        message: String,
    },
    /// A required Manager read/send capability is missing from tools/list.
    Compatibility {
        tool: String,
    },
    /// Only the feature using this optional tool is unavailable.
    Unsupported {
        tool: String,
    },
    /// The server answered something this client does not understand.
    Protocol(String),
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayError::NotConnected => write!(f, "not connected to T3"),
            GatewayError::NotPaired { origin } => write!(
                f,
                "Okilum is not signed in to T3 at {origin}: open the Manager panel and sign in there"
            ),
            GatewayError::Expired => write!(f, "the T3 token has expired; reconnect"),
            GatewayError::Revoked => write!(f, "T3 refused the token; it was revoked"),
            GatewayError::Unreachable(detail) => write!(f, "T3 is unreachable: {detail}"),
            GatewayError::Tool { code, message } => write!(f, "T3 refused ({code}): {message}"),
            GatewayError::Compatibility { tool } => write!(f, "This T3 server is incompatible: required tool '{tool}' is missing"),
            GatewayError::Unsupported { tool } => write!(f, "This feature is unavailable: T3 does not provide '{tool}'"),
            GatewayError::Protocol(detail) => write!(f, "unexpected answer from T3: {detail}"),
        }
    }
}

impl std::error::Error for GatewayError {}

pub type GatewayResult<T> = Result<T, GatewayError>;

/// The operations the Project tab needs from the server that runs an area's
/// manager. Calls block; run them off the UI thread.
pub trait ManagerGateway: Send + Sync {
    /// Explicit user action only. This launch is NOT idempotent; callers must
    /// persist intent before calling and retain uncertain outcomes.
    fn start_manager(&self, project_id: &str, title: &str, message: &str) -> GatewayResult<String> {
        let _ = (project_id, title, message);
        Err(GatewayError::Protocol(
            "This T3 connection cannot start managers".into(),
        ))
    }

    fn status(&self) -> GatewayStatus;

    fn server_identity(&self) -> GatewayResult<crate::ServerIdentity>;

    fn list_projects(&self) -> GatewayResult<Vec<Project>>;

    /// One project, its `projectIcon` and `faviconPath` included.
    fn read_project(&self, project_id: &str) -> GatewayResult<Project> {
        let _ = project_id;
        Err(GatewayError::Protocol(
            "This T3 connection cannot read projects".into(),
        ))
    }

    /// Sets T3's `projectIcon`, or clears it with `None`, through T3's
    /// project service (the update T3's own icon dialog makes). Returns the
    /// project read back, which is what the UI shows.
    fn set_project_icon(
        &self,
        project_id: &str,
        icon: Option<&crate::projects::icon::ProjectIcon>,
    ) -> GatewayResult<Project> {
        let _ = (project_id, icon);
        Err(GatewayError::Protocol(
            "This T3 connection cannot change project icons".into(),
        ))
    }

    /// Registers `workspace_root` (a directory on the T3 host) as a new T3
    /// project named `title` (#1475): T3's own `project.create`. The root
    /// must exist; T3 refuses a root it already has. Returns the project.
    fn create_project(&self, title: &str, workspace_root: &str) -> GatewayResult<Project> {
        let _ = (title, workspace_root);
        Err(GatewayError::Protocol(
            "This T3 connection cannot create projects".into(),
        ))
    }

    /// All threads of a project, subagents included.
    fn list_threads(&self, project_id: &str) -> GatewayResult<Vec<ThreadSummary>>;

    fn read_thread(&self, query: &ThreadQuery) -> GatewayResult<ThreadPage>;

    /// Idempotent per `message.client_request_id`.
    fn send_message(&self, message: &OutgoingMessage) -> GatewayResult<SendReceipt>;

    /// Ids of the thread's pending user questions (not approvals).
    fn pending_requests(&self, thread_id: &str) -> GatewayResult<Vec<String>>;

    /// The live provider and model catalog, and this client's runtime ceiling.
    fn capabilities(&self) -> GatewayResult<Capabilities>;

    fn thread_configuration(&self, thread_id: &str) -> GatewayResult<ThreadConfiguration>;

    /// Sets the thread's model and returns the configuration read back from
    /// the server, which is what the UI shows.
    fn configure_model(
        &self,
        thread_id: &str,
        selection: &ModelSelection,
    ) -> GatewayResult<ThreadConfiguration>;
}
