//! App-level re-exports and Reader/Settings-integration tests for
//! `okilum_shell_sync::reader_settings_sync` (#3240, epic #3241).
//! Standalone Sync settings, CLI, runtime, and unit tests live in
//! `crates/okilum-shell-sync`.

#[cfg(target_os = "linux")]
pub(crate) use okilum_shell_sync::reader_settings_sync::managed_service_directory;
#[cfg(target_os = "macos")]
pub(crate) use okilum_shell_sync::reader_settings_sync::native_signing_policy;
pub(crate) use okilum_shell_sync::reader_settings_sync::{
    active_controller, presentation, serial_actions, serve_paired, shared, suspend, SyncSettings,
    ADMISSION,
};
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) use okilum_shell_sync::reader_settings_sync::{
    capture_helper_before_update, restart_replaced_helper,
};

#[cfg(unix)]
pub(crate) mod cli {
    pub(crate) fn entry() -> Option<i32> {
        okilum_shell_sync::reader_settings_sync::cli::entry(|directory| {
            crate::reader_features::enable_saved(directory, crate::reader_features::Feature::Sync)
        })
    }
}

#[cfg(all(test, unix))]
#[path = "reader_settings_sync/reader_tests.rs"]
mod tests;
