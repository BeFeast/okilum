//! Structured log viewer, HTTP/remote/SFTP/Kubernetes/virtual log sources, and
//! Kubernetes workspace below the okilum app crate (#3239, epic #3241). Nothing
//! here knows `Reader`; the app crate (`okilum-shell`, binary `okilum`) builds
//! on it.
//!
//! Module names match their former paths in the app crate, so test names stay
//! the same (`reader_log::...`, `reader_log_sftp::...`,
//! `reader_log_kubernetes::...`, `kubernetes_workspace::...`).
#![allow(private_interfaces)]
#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

pub mod hooks;
pub mod kubernetes_workspace;
pub mod reader_log;
pub mod reader_log_kubernetes;
pub mod reader_log_sftp;

/// Whether this build has the logs crate's `test-support` feature: true only
/// in test builds. `okilum --build-info` reports it (#1043, #3239).
pub const TEST_SUPPORT: bool = cfg!(feature = "test-support");

pub(crate) use okilum_shell_kit::actions::{Dismiss, FindInNote, FindNext, FindPrev};
pub(crate) use okilum_shell_kit::{
    brand, count_label, reader_layout, reader_ui_state, ui_zoom, window_state,
};
pub(crate) use okilum_shell_platform::platform;
pub(crate) use okilum_shell_viewers::{
    reader_find_field, reader_icon_button, reader_segmented, reader_split, reader_status_line,
    reader_viewer_chrome, toolbar_text_width,
};

pub(crate) mod reader_shortcuts {
    pub(crate) use okilum_shell_viewers::shortcut_hint::hint;
}

pub(crate) mod reader_toast {
    pub(crate) fn error(message: impl Into<String>, window: &mut gpui::Window, cx: &mut gpui::App) {
        crate::hooks::toast_error(message, window, cx);
    }

    pub(crate) fn transient(
        message: impl Into<String>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        crate::hooks::toast_transient(message, window, cx);
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

#[cfg(test)]
pub(crate) fn test_init(cx: &mut gpui::App) {
    okilum_shell_kit::test_support::init(cx);
    hooks::install(
        hooks::LogsHooks {
            release_version: "0.0.0-test",
            source_commit: "test",
            toast_error: |_, _, _| {},
            toast_transient: |_, _, _| {},
            diagnostics_trace: |_| None,
        },
        cx,
    );
}

#[cfg(test)]
pub(crate) fn bind_keys(cx: &mut gpui::App) {
    reader_log::bind_keys(cx);
    kubernetes_workspace::bind_keys(cx);
    cx.bind_keys([
        gpui::KeyBinding::new("escape", Dismiss, None),
        gpui::KeyBinding::new("secondary-f", FindInNote, None),
        gpui::KeyBinding::new("secondary-g", FindNext, None),
        gpui::KeyBinding::new("secondary-shift-g", FindPrev, None),
    ]);
}

/// What log and Kubernetes modules take from the root, as `use super::*` /
/// `use crate::*` did in the app.
#[allow(unused_imports)]
pub(crate) mod prelude {
    #[cfg(test)]
    pub(crate) use crate::{bind_keys, test_init};
    pub(crate) use crate::{
        brand, count_label, kubernetes_workspace, platform, reader_diagnostics, reader_find_field,
        reader_icon_button, reader_layout, reader_log, reader_log_kubernetes, reader_log_sftp,
        reader_segmented, reader_shortcuts, reader_split, reader_status_line, reader_toast,
        reader_ui_state, reader_viewer_chrome, toolbar_text_width, ui_zoom, window_state, Dismiss,
        FindInNote, FindNext, FindPrev,
    };
    pub(crate) use gpui::prelude::FluentBuilder as _;
    pub(crate) use gpui::*;
    pub(crate) use gpui_base::ElementExt as _;
    pub(crate) use gpui_component::menu::ContextMenuExt as _;
    pub(crate) use gpui_component::{
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Root, Selectable as _,
        Sizable as _, Theme, TitleBar, WindowExt as _,
    };
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use std::rc::Rc;
    pub(crate) use std::sync::Arc;
    pub(crate) use std::time::Duration;
}
