//! Explicit desktop Sync operations. Importing this crate never discovers or starts a daemon.
#[cfg(target_os = "linux")]
pub mod lifecycle;
