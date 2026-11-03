//! Minimal backend-owned AI Brain POC. Provider transports plug into Adapter;
//! Markdown remains durable product data, not a copy of an execution database.
//! The source-store runtime is Unix-only, matching okilum-core::source.
//! Chat, Todoist and transport-neutral values remain available on Windows.
pub mod chat;
#[cfg(unix)]
mod discussion_context;
#[cfg(unix)]
mod discussion_send;
#[cfg(unix)]
pub mod export;
#[cfg(unix)]
mod runtime;
#[cfg(unix)]
pub mod service;
pub mod todoist;
pub mod types;
#[cfg(unix)]
pub use runtime::{Runner, RunnerConfig, Snapshot};
pub use types::*;

#[cfg(unix)]
pub mod application;
#[cfg(unix)]
pub mod t3;

#[cfg(unix)]
pub mod preview;

#[cfg(unix)]
pub mod settings;

#[cfg(unix)]
pub mod context_export;
#[cfg(unix)]
mod context_jobs;

#[cfg(unix)]
pub mod context;
#[cfg(unix)]
pub mod incoming_references;
#[cfg(unix)]
pub mod retrieval;

#[cfg(unix)]
pub mod inbox;

#[cfg(unix)]
pub mod attention;

#[cfg(unix)]
pub mod connector;

#[cfg(unix)]
pub mod inbox_plan;

#[cfg(unix)]
pub mod maestro;
#[cfg(unix)]
pub mod maestro_control;
#[cfg(unix)]
pub mod maestro_links;
#[cfg(unix)]
pub mod maestro_operations;

// Durable proposal provider and receipt storage.
#[allow(dead_code)]
#[cfg(unix)]
mod proposals;

#[cfg(unix)]
pub mod proposal;

#[cfg(unix)]
pub mod suggestions;

#[cfg(unix)]
pub mod t3_recovery;

#[cfg(unix)]
pub mod t3_routes;

#[cfg(unix)]
pub mod t3_compat;
