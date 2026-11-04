//! Explicit desktop Sync operations. Importing this crate never discovers or starts a daemon.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod admission;
#[cfg(target_os = "linux")]
pub mod lifecycle;
#[cfg(target_os = "linux")]
pub mod managed_launch;
pub mod merge_preview;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod daemon;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod direct;
pub mod pairing;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod private;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod enrollment;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod folder;

#[cfg(target_os = "linux")]
pub mod runtime;

pub mod readiness;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod presentation;

#[cfg(target_os = "linux")]
pub mod removal;

#[cfg(target_os = "linux")]
pub mod desktop;

#[cfg(target_os = "linux")]
pub mod conflicts;
pub mod sidecar;

#[cfg(target_os = "linux")]
pub mod linux_bootstrap;

#[cfg(target_os = "linux")]
pub mod network_policy;
