//! Explicit desktop Sync operations. Importing this crate never discovers or starts a daemon.
#[cfg(target_os = "linux")]
pub mod lifecycle;

#[cfg(target_os = "linux")]
pub mod daemon;
pub mod pairing;
#[cfg(target_os = "linux")]
mod private;

#[cfg(target_os = "linux")]
pub mod enrollment;

#[cfg(target_os = "linux")]
pub mod folder;

#[cfg(target_os = "linux")]
pub mod runtime;

pub mod readiness;

#[cfg(target_os = "linux")]
pub mod presentation;

#[cfg(target_os = "linux")]
pub mod removal;

#[cfg(target_os = "linux")]
pub mod desktop;
