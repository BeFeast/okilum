//! Minimal backend-owned AI Brain POC. Provider transports plug into Adapter;
//! Markdown remains durable product data, not a copy of an execution database.
pub mod chat;
mod discussion_context;
mod discussion_send;
pub mod export;
mod runtime;
pub mod service;
pub mod todoist;
pub mod types;
pub use runtime::{Runner, RunnerConfig, Snapshot};
pub use types::*;

pub mod application;
pub mod t3;

pub mod preview;

pub mod settings;

pub mod context_export;
mod context_jobs;

pub mod context;
pub mod incoming_references;
pub mod retrieval;

pub mod inbox;

pub mod attention;

pub mod connector;

pub mod inbox_plan;

pub mod maestro;
pub mod maestro_control;
pub mod maestro_links;
pub mod maestro_operations;

// Durable proposal provider and receipt storage.
#[allow(dead_code)]
mod proposals;

pub mod proposal;

pub mod suggestions;

pub mod t3_recovery;

pub mod t3_routes;

pub mod t3_compat;
