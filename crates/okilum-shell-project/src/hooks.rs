//! App-registered callbacks for project modules that need app-level services
//! (Settings -> T3 navigation, Agents module gating, toasts, code font,
//! diagnostics tracing, and test gateway probes) without depending on `Reader`
//! (#3237, epic #3241).
use gpui::{App, Global, SharedString, Window};
use okilum_t3::NetworkAccess;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

pub type ShowT3Fn = fn(&mut App);
pub type AgentsEnabledFn = fn(&App) -> bool;
pub type AgentsAccessFn = fn(&App) -> NetworkAccess;
pub type SetAgentsEnabledFn = fn(bool, &mut App);
pub type CodeFontFn = fn(&App) -> SharedString;
pub type ToastErrorFn = fn(String, &mut Window, &mut App);
pub type ToastTransientFn = fn(String, &mut Window, &mut App);
pub type DiagnosticsEventFn = Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync>;
pub type DiagnosticsTraceFn = fn(&App) -> Option<DiagnosticsEventFn>;
pub type RecordManagerGatewayFn = fn();

pub struct ProjectHooks {
    pub show_t3: ShowT3Fn,
    pub agents_enabled: AgentsEnabledFn,
    pub agents_access: AgentsAccessFn,
    pub set_agents_enabled: SetAgentsEnabledFn,
    pub code_font: CodeFontFn,
    pub toast_error: ToastErrorFn,
    pub toast_transient: ToastTransientFn,
    pub diagnostics_trace: DiagnosticsTraceFn,
}

impl Global for ProjectHooks {}

static RECORD_MANAGER_GATEWAY: RwLock<Option<RecordManagerGatewayFn>> = RwLock::new(None);
static TEST_GATEWAY_HOOK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Register app-level hooks once at startup (and in `test_init`).
pub fn install(hooks: ProjectHooks, cx: &mut App) {
    cx.set_global(hooks);
}

/// Register the `t3_probe::record_manager_gateway` observer for tests in the
/// app crate.
pub fn install_manager_gateway_probe(probe: RecordManagerGatewayFn) {
    if let Ok(mut slot) = RECORD_MANAGER_GATEWAY.write() {
        *slot = Some(probe);
        TEST_GATEWAY_HOOK_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_manager_gateway() {
    if TEST_GATEWAY_HOOK_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    if let Ok(slot) = RECORD_MANAGER_GATEWAY.read() {
        if let Some(f) = *slot {
            f();
        }
    }
}

pub(crate) fn show_t3(cx: &mut App) {
    if let Some(hooks) = cx.try_global::<ProjectHooks>() {
        let f = hooks.show_t3;
        f(cx);
    }
}

pub(crate) fn code_font(cx: &App) -> SharedString {
    if let Some(hooks) = cx.try_global::<ProjectHooks>() {
        let f = hooks.code_font;
        return f(cx);
    }
    SharedString::from("monospace")
}

pub(crate) fn toast_error(message: impl Into<String>, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<ProjectHooks>() {
        let f = hooks.toast_error;
        f(message.into(), window, cx);
    }
}

pub(crate) fn toast_transient(message: impl Into<String>, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<ProjectHooks>() {
        let f = hooks.toast_transient;
        f(message.into(), window, cx);
    }
}

pub(crate) fn diagnostics_trace(cx: &App) -> Option<DiagnosticsEventFn> {
    if let Some(hooks) = cx.try_global::<ProjectHooks>() {
        let f = hooks.diagnostics_trace;
        return f(cx);
    }
    None
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
struct LocalAgents {
    enabled: bool,
    permit: NetworkAccess,
}

#[cfg(any(test, feature = "test-support"))]
impl Global for LocalAgents {}

pub(crate) fn agents_enabled(cx: &App) -> bool {
    if let Some(hooks) = cx.try_global::<ProjectHooks>() {
        let f = hooks.agents_enabled;
        return f(cx);
    }
    #[cfg(any(test, feature = "test-support"))]
    if let Some(local) = cx.try_global::<LocalAgents>() {
        return local.enabled;
    }
    false
}

pub(crate) fn agents_access(cx: &App) -> NetworkAccess {
    if let Some(hooks) = cx.try_global::<ProjectHooks>() {
        let f = hooks.agents_access;
        return f(cx);
    }
    #[cfg(any(test, feature = "test-support"))]
    if let Some(local) = cx.try_global::<LocalAgents>() {
        return local.permit.clone();
    }
    NetworkAccess::new(false)
}

pub(crate) fn set_agents_enabled(enabled: bool, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<ProjectHooks>() {
        let f = hooks.set_agents_enabled;
        f(enabled, cx);
        return;
    }
    #[cfg(any(test, feature = "test-support"))]
    {
        if let Some(local) = cx.try_global::<LocalAgents>() {
            local.permit.revoke();
        }
        cx.set_global(LocalAgents {
            enabled,
            permit: NetworkAccess::new(enabled),
        });
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = (enabled, cx);
}
