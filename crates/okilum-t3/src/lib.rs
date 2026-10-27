//! Okilum as a remote control for an area manager that runs in T3 Code (#1415).
//!
//! The manager and its executors stay in T3 on the server. Okilum talks to
//! T3's external MCP endpoint (`/mcp`) as an OAuth client and never runs an
//! agent itself. [`ManagerGateway`] is the transport-agnostic surface the UI
//! uses; [`McpGateway`] is its first implementation. [`SessionGateway`]
//! reads projects and writes icons with the Manager panel's T3 web session,
//! and [`PairedGateway`] joins the two for one saved connection.
//!
//! The access token is a secret: it lives in the OS keychain (the shell writes
//! [`StoredToken::to_secret`] under [`Environment::credential_url`]), never in
//! a file or in the vault, and its `Debug` output is redacted.

pub mod chat;
mod environment;
pub mod executors;
mod gateway;
pub mod launch;
pub mod manifest;
mod mcp;
pub mod models;
pub mod oauth;
pub mod projects;
pub mod reconnect;
mod session;
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
    is_http_link, Capabilities, ModelInfo, ModelOption, ModelSelection, OutgoingMessage, Project,
    ProviderCapability, RepositoryIdentity, SendMode, SendReceipt, ServerIdentity,
    ThreadConfiguration, ThreadDetail, ThreadPage, ThreadQuery, ThreadSummary, TimelineItem,
};

/// Seconds since the Unix epoch, the clock tokens are compared against.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}
