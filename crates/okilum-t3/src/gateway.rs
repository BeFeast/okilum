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
    Expired,
    Revoked,
    Unreachable(String),
    /// T3 refused the operation; `code` is T3's failure code, for example
    /// `capability_denied` or `thread_not_found`.
    Tool {
        code: String,
        message: String,
    },
    /// The server answered something this client does not understand.
    Protocol(String),
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayError::NotConnected => write!(f, "not connected to T3"),
            GatewayError::Expired => write!(f, "the T3 token has expired; reconnect"),
            GatewayError::Revoked => write!(f, "T3 refused the token; it was revoked"),
            GatewayError::Unreachable(detail) => write!(f, "T3 is unreachable: {detail}"),
            GatewayError::Tool { code, message } => write!(f, "T3 refused ({code}): {message}"),
            GatewayError::Protocol(detail) => write!(f, "unexpected answer from T3: {detail}"),
        }
    }
}

impl std::error::Error for GatewayError {}

pub type GatewayResult<T> = Result<T, GatewayError>;

/// The operations the Project tab needs from the server that runs an area's
/// manager. Calls block; run them off the UI thread.
pub trait ManagerGateway: Send + Sync {
    fn status(&self) -> GatewayStatus;

    fn server_identity(&self) -> GatewayResult<crate::ServerIdentity>;

    fn list_projects(&self) -> GatewayResult<Vec<Project>>;

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
