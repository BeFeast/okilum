//! Okilum as a remote control for an area manager that runs in T3 Code (#1415).
//!
//! The manager and its executors stay in T3 on the server. Okilum talks to
//! T3's external MCP endpoint (`/mcp`) as an OAuth client and never runs an
//! agent itself. [`ManagerGateway`] is the transport-agnostic surface the UI
//! uses; [`McpGateway`] is its first implementation. [`SessionGateway`]
//! reads projects and writes icons with the separately paired native bearer,
//! and [`PairedGateway`] joins the two for one saved connection.
//!
//! The access token is a secret: it lives in the OS keychain (the shell writes
//! [`StoredToken::to_secret`] under [`Environment::credential_url`]), never in
//! a file or in the vault, and its `Debug` output is redacted.

pub use okilum_network_access::{
    scoped as scope_network, scoped_current as scope_current_network, NetworkAccess,
};
pub mod actions;
pub mod area_files;
pub mod board;
pub mod chat;
pub mod chat_drafts;
pub mod connection;
pub mod draft_handoff;
mod environment;
pub mod executors;
mod gateway;
pub mod launch;
pub mod manifest;
mod mcp;
pub mod message_types;
pub mod models;
pub mod oauth;
pub mod projection;
pub mod projects;
pub mod reconnect;
pub mod records;
pub mod rotation;
pub mod service_layout;
mod session;
pub mod stream;
mod token;
mod types;

#[cfg(test)]
mod testing;

pub use environment::Environment;
pub use gateway::{GatewayError, GatewayStatus, ManagerGateway};
pub use mcp::McpGateway;
pub use session::{
    NoSessionSource, PairedGateway, SessionCookie, SessionGateway, SessionSource, CONNECT_TIMEOUT,
};
pub use token::{StoredToken, TokenFreshness, EXPIRES_SOON_SECS};
pub use types::{
    is_http_link, Capabilities, ModelCapabilities, ModelInfo, ModelOption, ModelOptionDescriptor,
    ModelOptionValue, ModelSelection, OutgoingMessage, Project, ProviderCapability,
    RepositoryIdentity, SendMode, SendReceipt, ServerIdentity, ThreadConfiguration, ThreadDetail,
    ThreadPage, ThreadQuery, ThreadSummary, TimelineItem,
};

/// Seconds since the Unix epoch, the clock tokens are compared against.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

pub mod rules_refresh;
