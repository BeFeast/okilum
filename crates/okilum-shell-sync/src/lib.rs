//! Desktop Sync settings, CLI, conflict resolution, and first-sync merge
//! preview below the okilum app crate (#3240, epic #3241). Nothing here knows
//! `Reader`; the app crate (`okilum-shell`, binary `okilum`) builds on it.
//!
//! Module names match their former paths in the app crate, so test names stay
//! the same (`reader_settings_sync::...`, `reader_sync_conflicts::...`,
//! `reader_sync_merge::...`).
#![allow(private_interfaces)]
#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

pub mod hooks;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub mod reader_settings_sync;
#[cfg(target_os = "linux")]
pub mod reader_sync_conflicts;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub mod reader_sync_merge;

/// Whether this build has the sync crate's `test-support` feature: true only
/// in test builds. `okilum --build-info` reports it (#1043, #3240).
pub const TEST_SUPPORT: bool = cfg!(feature = "test-support");

pub(crate) use okilum_shell_kit::{brand, reader_history, reader_sync_help, reader_ui_state};

pub(crate) mod reader_features {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Feature {
        Sync,
    }

    pub(crate) fn enabled(feature: Feature, cx: &gpui::App) -> bool {
        match feature {
            Feature::Sync => crate::hooks::sync_enabled(cx),
        }
    }

    pub(crate) fn access(feature: Feature, cx: &gpui::App) -> okilum_t3::NetworkAccess {
        match feature {
            Feature::Sync => crate::hooks::sync_access(cx),
        }
    }

    #[cfg(test)]
    pub(crate) fn install(_directory: &std::path::Path, cx: &mut gpui::App) {
        crate::hooks::set_local_sync_enabled(false, cx);
    }

    #[cfg(test)]
    pub(crate) fn set_enabled(feature: Feature, enabled: bool, cx: &mut gpui::App) {
        match feature {
            Feature::Sync => {
                crate::hooks::set_local_sync_enabled(enabled, cx);
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                if enabled {
                    crate::reader_settings_sync::serve_paired(cx);
                } else {
                    crate::reader_settings_sync::suspend(cx);
                }
            }
        }
    }
}

pub(crate) mod reader_diagnostics {
    pub(crate) struct TraceHandle(crate::hooks::DiagnosticsEventFn);

    impl TraceHandle {
        pub(crate) fn event(&self, phase: &'static str, details: serde_json::Value) {
            (self.0)(phase, details);
        }
    }

    pub(crate) fn trace(cx: &gpui::App) -> Option<TraceHandle> {
        crate::hooks::diagnostics_trace(cx).map(TraceHandle)
    }
}

pub(crate) mod reader_fonts {
    pub(crate) fn code(cx: &gpui::App) -> gpui::SharedString {
        crate::hooks::code_font(cx)
    }
}

pub(crate) mod reader_toast {
    pub(crate) fn error(message: impl Into<String>, window: &mut gpui::Window, cx: &mut gpui::App) {
        crate::hooks::toast_error(message, window, cx);
    }
}

pub(crate) mod reader_settings {
    pub(crate) fn show_sync(window: &mut gpui::Window, cx: &mut gpui::App) {
        crate::hooks::show_sync_settings(window, cx);
    }
}

pub(crate) mod reader_confirm {
    pub(crate) use crate::hooks::ConfirmBody as Body;

    pub(crate) fn confirm(
        window: &mut gpui::Window,
        cx: &mut gpui::App,
        title: &'static str,
        body: Body,
        confirm_label: &'static str,
    ) -> async_channel::Receiver<bool> {
        crate::hooks::confirm(window, cx, title, body, confirm_label)
    }
}

#[cfg(test)]
pub(crate) fn test_init(cx: &mut gpui::App) {
    okilum_shell_kit::test_support::init(cx);
    hooks::set_local_sync_enabled(true, cx);
}

/// What sync modules take from the root, as `use super::*` / `use crate::*` did
/// in the app.
#[allow(unused_imports)]
pub(crate) mod prelude {
    #[cfg(target_os = "linux")]
    pub(crate) use crate::reader_sync_conflicts;
    #[cfg(test)]
    pub(crate) use crate::test_init;
    pub(crate) use crate::{
        brand, reader_confirm, reader_diagnostics, reader_features, reader_fonts, reader_history,
        reader_settings, reader_sync_help, reader_toast, reader_ui_state,
    };
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub(crate) use crate::{reader_settings_sync, reader_sync_merge};
    pub(crate) use gpui::prelude::FluentBuilder as _;
    pub(crate) use gpui::*;
    pub(crate) use gpui_base::ElementExt as _;
    pub(crate) use gpui_component::{
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex, ActiveTheme as _, Disableable as _, IconName, Root, Selectable as _, Sizable as _,
        WindowExt as _,
    };
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use std::rc::Rc;
    pub(crate) use std::sync::Arc;
    pub(crate) use std::time::Duration;
}
