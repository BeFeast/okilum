//! App-registered callbacks for log and Kubernetes modules that need app-level
//! services (toasts and diagnostics trace) without depending on `Reader`
//! (#3239, epic #3241).
use gpui::{App, Global, Window};
use std::sync::Arc;

pub type ToastErrorFn = fn(String, &mut Window, &mut App);
pub type ToastTransientFn = fn(String, &mut Window, &mut App);
pub type DiagnosticsEventFn = Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync>;
pub type DiagnosticsTraceFn = fn(&App) -> Option<DiagnosticsEventFn>;

pub struct LogsHooks {
    pub release_version: &'static str,
    pub source_commit: &'static str,
    pub toast_error: ToastErrorFn,
    pub toast_transient: ToastTransientFn,
    pub diagnostics_trace: DiagnosticsTraceFn,
}

impl Global for LogsHooks {}

/// Register app-level hooks once at startup (and in `test_init`).
pub fn install(hooks: LogsHooks, cx: &mut App) {
    cx.set_global(hooks);
}

pub(crate) fn release_version(cx: &App) -> &'static str {
    cx.try_global::<LogsHooks>()
        .map(|hooks| hooks.release_version)
        .unwrap_or("0.0.0-test")
}

pub(crate) fn source_commit(cx: &App) -> &'static str {
    cx.try_global::<LogsHooks>()
        .map(|hooks| hooks.source_commit)
        .unwrap_or("test")
}

pub(crate) fn toast_error(message: impl Into<String>, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<LogsHooks>() {
        (hooks.toast_error)(message.into(), window, cx);
    }
}

pub(crate) fn toast_transient(message: impl Into<String>, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<LogsHooks>() {
        (hooks.toast_transient)(message.into(), window, cx);
    }
}

pub(crate) fn diagnostics_trace(cx: &App) -> Option<DiagnosticsEventFn> {
    cx.try_global::<LogsHooks>()
        .and_then(|hooks| (hooks.diagnostics_trace)(cx))
}
