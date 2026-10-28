//! Explicit desktop Sync operations. Importing this crate never discovers or starts a daemon.
#[cfg(target_os = "linux")]
pub mod lifecycle;

#[cfg(target_os = "linux")]
pub mod daemon;
pub mod pairing;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod private;

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(target_os = "linux")]
pub mod conflicts;
pub mod sidecar;
