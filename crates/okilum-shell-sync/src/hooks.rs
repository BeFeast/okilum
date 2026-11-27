//! App-registered callbacks for Sync modules that need app-level services
//! (Sync feature gating, updater channel, vault display names, open Reader
//! windows, Settings navigation, toasts, code font, and diagnostics tracing)
//! without depending on `Reader` (#3240, epic #3241).
use gpui::{App, Global, SharedString, Window};
use okilum_t3::NetworkAccess;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct OpenVault {
    pub vault_root: PathBuf,
    pub session_directory: Option<PathBuf>,
    pub single_file: bool,
}

pub struct ConfirmBody {
    pub intro: Vec<String>,
    pub items: Vec<String>,
    pub outro: Vec<String>,
}

pub type IsPortableWindowsFn = fn() -> bool;
pub type SyncEnabledFn = fn(&App) -> bool;
pub type SyncAccessFn = fn(&App) -> NetworkAccess;
pub type DiagnosticsEventFn = Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync>;
pub type DiagnosticsTraceFn = fn(&App) -> Option<DiagnosticsEventFn>;
pub type RememberSyncVaultNamesFn = fn(Vec<(PathBuf, String)>, &mut App);
pub type OpenVaultInNewWindowFn = fn(PathBuf, PathBuf, &mut App);
pub type OpenVaultsFn = fn(&App) -> Vec<OpenVault>;
pub type AnyDirtyReaderFn = fn(&Path, &App) -> bool;
pub type ShowSyncSettingsFn = fn(&mut Window, &mut App);
pub type CodeFontFn = fn(&App) -> SharedString;
pub type ToastErrorFn = fn(String, &mut Window, &mut App);
pub type ConfirmFn = fn(
    &mut Window,
    &mut App,
    &'static str,
    ConfirmBody,
    &'static str,
) -> async_channel::Receiver<bool>;

pub struct SyncHooks {
    pub is_portable_windows: IsPortableWindowsFn,
    pub sync_enabled: SyncEnabledFn,
    pub sync_access: SyncAccessFn,
    pub diagnostics_trace: DiagnosticsTraceFn,
    pub remember_sync_vault_names: RememberSyncVaultNamesFn,
    pub open_vault_in_new_window: OpenVaultInNewWindowFn,
    pub open_vaults: OpenVaultsFn,
    pub any_dirty_reader: AnyDirtyReaderFn,
    pub show_sync_settings: ShowSyncSettingsFn,
    pub code_font: CodeFontFn,
    pub toast_error: ToastErrorFn,
    pub confirm: ConfirmFn,
}

impl Global for SyncHooks {}

/// Register app-level hooks once at startup (and in `test_init`).
pub fn install(hooks: SyncHooks, cx: &mut App) {
    cx.set_global(hooks);
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
struct LocalSyncFeature {
    enabled: bool,
    permit: NetworkAccess,
}

#[cfg(any(test, feature = "test-support"))]
impl Global for LocalSyncFeature {}

#[cfg(any(test, feature = "test-support"))]
pub fn set_local_sync_enabled(enabled: bool, cx: &mut App) {
    if let Some(local) = cx.try_global::<LocalSyncFeature>() {
        local.permit.revoke();
    }
    cx.set_global(LocalSyncFeature {
        enabled,
        permit: NetworkAccess::new(enabled),
    });
}

#[cfg(not(any(test, feature = "test-support")))]
pub(crate) fn is_portable_windows(cx: &App) -> bool {
    cx.try_global::<SyncHooks>()
        .is_some_and(|hooks| (hooks.is_portable_windows)())
}

pub(crate) fn sync_enabled(cx: &App) -> bool {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.sync_enabled;
        return f(cx);
    }
    #[cfg(any(test, feature = "test-support"))]
    if let Some(local) = cx.try_global::<LocalSyncFeature>() {
        return local.enabled;
    }
    false
}

pub(crate) fn sync_access(cx: &App) -> NetworkAccess {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.sync_access;
        return f(cx);
    }
    #[cfg(any(test, feature = "test-support"))]
    if let Some(local) = cx.try_global::<LocalSyncFeature>() {
        return local.permit.clone();
    }
    NetworkAccess::new(false)
}

pub(crate) fn diagnostics_trace(cx: &App) -> Option<DiagnosticsEventFn> {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.diagnostics_trace;
        return f(cx);
    }
    None
}

pub(crate) fn remember_sync_vault_names(known: Vec<(PathBuf, String)>, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.remember_sync_vault_names;
        f(known, cx);
    }
}

pub(crate) fn open_vault_in_new_window(path: PathBuf, directory: PathBuf, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.open_vault_in_new_window;
        f(path, directory, cx);
    }
}

pub(crate) fn open_vaults(cx: &App) -> Vec<OpenVault> {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.open_vaults;
        return f(cx);
    }
    Vec::new()
}

pub(crate) fn any_dirty_reader(root: &Path, cx: &App) -> bool {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.any_dirty_reader;
        return f(root, cx);
    }
    false
}

pub(crate) fn show_sync_settings(window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.show_sync_settings;
        f(window, cx);
    }
}

pub(crate) fn code_font(cx: &App) -> SharedString {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.code_font;
        return f(cx);
    }
    SharedString::from("monospace")
}

pub(crate) fn toast_error(message: impl Into<String>, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.toast_error;
        f(message.into(), window, cx);
    }
}

pub(crate) fn confirm(
    window: &mut Window,
    cx: &mut App,
    title: &'static str,
    body: ConfirmBody,
    confirm_label: &'static str,
) -> async_channel::Receiver<bool> {
    if let Some(hooks) = cx.try_global::<SyncHooks>() {
        let f = hooks.confirm;
        return f(window, cx, title, body, confirm_label);
    }
    let (send, receive) = async_channel::bounded(1);
    let _ = send.try_send(false);
    receive
}
