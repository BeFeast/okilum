//! Project Manager, Board, Records, T3 settings, Manager start, and external
//! terminal launcher below the okilum app crate (#3237, epic #3241). Nothing
//! here knows `Reader`; the app crate (`okilum-shell`, binary `okilum`) builds
//! on it.
//!
//! Module names match their former paths in the app crate, so test names stay
//! the same (`project::...`, `project_terminal::...`, `reader_t3_settings::...`,
//! `reader_manager_start::...`).
#![allow(private_interfaces)]
#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

pub mod hooks;
pub mod project;
pub mod project_terminal;
pub mod reader_manager_start;
pub mod reader_t3_settings;

/// Whether this build has the project crate's `test-support` feature: true
/// only in test builds. `okilum --build-info` reports it (#1043, #3237).
pub const TEST_SUPPORT: bool = cfg!(feature = "test-support");

pub(crate) use okilum_shell_kit::actions::{Dismiss, DismissManagerModel, OPEN_IN_TERMINAL_KEY};
pub(crate) use okilum_shell_kit::clock::now;
pub(crate) use okilum_shell_kit::{
    brand, contextual_help, reader_history, reader_layout, reader_ui_state,
};
pub(crate) use okilum_shell_platform::{note_paste, platform};
pub(crate) use okilum_shell_text::markdown_style::{markdown_plugins, reader_text_style};
pub(crate) use okilum_shell_text::{reader_code, source_presentation};

pub(crate) mod reader_settings {
    pub(crate) fn show_t3(cx: &mut gpui::App) {
        crate::hooks::show_t3(cx);
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

    pub(crate) fn transient(
        message: impl Into<String>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        crate::hooks::toast_transient(message, window, cx);
    }
}

pub(crate) mod reader_diagnostics {
    use gpui::App;
    #[cfg(test)]
    use std::sync::Arc;

    #[derive(Clone)]
    pub(crate) struct Trace {
        sink: crate::hooks::DiagnosticsEventFn,
    }

    impl Trace {
        pub(crate) fn event(&self, phase: &'static str, details: serde_json::Value) {
            (self.sink)(phase, details);
        }

        #[cfg(test)]
        pub(crate) fn new(directory: Option<std::path::PathBuf>, _session: Option<String>) -> Self {
            let path = directory.map(|dir| dir.join("reader-diagnostic.log"));
            let lock = Arc::new(std::sync::Mutex::new(()));
            Self {
                sink: Arc::new(move |phase, details| {
                    let Some(path) = &path else { return };
                    let _guard = lock.lock().unwrap();
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let line = serde_json::json!({
                        "phase": phase,
                        "details": details,
                    });
                    use std::io::Write as _;
                    if let Ok(mut file) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                    {
                        let _ = writeln!(file, "{line}");
                    }
                }),
            }
        }
    }

    #[cfg(test)]
    #[derive(Clone)]
    pub(crate) struct LaunchTrace(pub(crate) Trace);
    #[cfg(test)]
    impl gpui::Global for LaunchTrace {}

    pub(crate) fn trace(cx: &App) -> Option<Trace> {
        #[cfg(test)]
        if let Some(local) = cx.try_global::<LaunchTrace>() {
            return Some(local.0.clone());
        }
        crate::hooks::diagnostics_trace(cx).map(|sink| Trace { sink })
    }
}

pub(crate) mod reader_manager_links {
    /// An http(s) URL the system browser may open; anything else is refused.
    pub(crate) fn external_url(raw: &str) -> Option<url::Url> {
        url::Url::parse(raw)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
    }
}

pub(crate) mod reader_features {
    use gpui::App;
    use okilum_t3::NetworkAccess;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Feature {
        Agents,
    }

    pub(crate) fn enabled(_feature: Feature, cx: &App) -> bool {
        crate::hooks::agents_enabled(cx)
    }

    pub(crate) fn access(_feature: Feature, cx: &App) -> NetworkAccess {
        crate::hooks::agents_access(cx)
    }

    #[allow(dead_code)]
    pub(crate) fn set_enabled(_feature: Feature, enabled: bool, cx: &mut App) {
        crate::hooks::set_agents_enabled(enabled, cx);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn enable_test_modules(cx: &mut App) {
        crate::hooks::set_agents_enabled(true, cx);
    }
}

#[cfg(test)]
pub(crate) fn test_init(cx: &mut gpui::App) {
    okilum_shell_kit::test_support::init(cx);
    reader_features::enable_test_modules(cx);
}

#[cfg(test)]
pub(crate) fn bind_keys(cx: &mut gpui::App) {
    project_terminal::bind_keys(cx);
    cx.bind_keys([
        gpui::KeyBinding::new("escape", Dismiss, None),
        gpui::KeyBinding::new("escape", DismissManagerModel, None),
    ]);
}

/// What project modules take from the root, as `use super::*` did in the app.
#[allow(unused_imports)]
pub(crate) mod prelude {
    #[cfg(test)]
    pub(crate) use crate::{bind_keys, test_init};
    pub(crate) use crate::{
        brand, contextual_help, markdown_plugins, note_paste, now, platform, project,
        project_terminal, reader_code, reader_diagnostics, reader_features, reader_fonts,
        reader_history, reader_layout, reader_manager_links, reader_manager_start, reader_settings,
        reader_t3_settings, reader_text_style, reader_toast, reader_ui_state, source_presentation,
        Dismiss, DismissManagerModel, OPEN_IN_TERMINAL_KEY,
    };
    pub(crate) use gpui::prelude::FluentBuilder as _;
    pub(crate) use gpui::*;
    pub(crate) use gpui_component::menu::ContextMenuExt as _;
    pub(crate) use gpui_component::{
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        text::{
            markdown_ast, MarkdownNode, SelectionFormat, TextView, TextViewState, TextViewStyle,
        },
        v_flex, ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, Root,
        Selectable as _, Sizable as _, Theme, TitleBar, WindowExt as _,
    };
    pub(crate) use okilum_core::Vault;
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use std::rc::Rc;
    pub(crate) use std::sync::Arc;
    pub(crate) use std::time::Duration;
}
