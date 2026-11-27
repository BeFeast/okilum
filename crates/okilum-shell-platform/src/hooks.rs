//! App-registered callbacks for platform modules that need app-level services
//! (error toasts, vault path checking, dirty-note save before Open-in, shortcut
//! labels, release version) without depending on `Reader` or `env!` (#3234, epic #3241).
use gpui::{Action, App, Global, SharedString, Window};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

type ToastErrorFn = fn(String, &mut Window, &mut App);
type CheckedPathFn = fn(&Path, &str) -> anyhow::Result<PathBuf>;
type PrepareOpenNoteFn = fn(&Path, &str, &mut App) -> anyhow::Result<Option<usize>>;
type ShortcutFn = fn(&dyn Action, &App) -> Option<String>;
type HintFn = fn(&str, &dyn Action, &App) -> SharedString;

static RELEASE_VERSION: OnceLock<&'static str> = OnceLock::new();

struct PlatformHooks {
    toast_error: ToastErrorFn,
    checked_path: CheckedPathFn,
    prepare_open_note: PrepareOpenNoteFn,
    shortcut: ShortcutFn,
    hint: HintFn,
}

impl Global for PlatformHooks {}

/// Register app-level hooks once at startup (and in `test_init`).
pub fn install(
    release_version: &'static str,
    toast_error: ToastErrorFn,
    checked_path: CheckedPathFn,
    prepare_open_note: PrepareOpenNoteFn,
    shortcut: ShortcutFn,
    hint: HintFn,
    cx: &mut App,
) {
    let _ = RELEASE_VERSION.set(release_version);
    cx.set_global(PlatformHooks {
        toast_error,
        checked_path,
        prepare_open_note,
        shortcut,
        hint,
    });
}

pub(crate) fn release_version() -> &'static str {
    RELEASE_VERSION.get().copied().unwrap_or("0.0.0-dev")
}

pub(crate) fn toast_error(message: String, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<PlatformHooks>() {
        let f = hooks.toast_error;
        f(message, window, cx);
    }
}

pub(crate) fn checked_path(root: &Path, rel: &str, cx: &App) -> anyhow::Result<PathBuf> {
    if let Some(hooks) = cx.try_global::<PlatformHooks>() {
        let f = hooks.checked_path;
        return f(root, rel);
    }
    Ok(root.join(rel))
}

pub(crate) fn prepare_open_note(
    root: &Path,
    rel: &str,
    cx: &mut App,
) -> anyhow::Result<Option<usize>> {
    if let Some(hooks) = cx.try_global::<PlatformHooks>() {
        let f = hooks.prepare_open_note;
        return f(root, rel, cx);
    }
    Ok(None)
}

pub(crate) fn shortcut(action: &dyn Action, cx: &App) -> Option<String> {
    cx.try_global::<PlatformHooks>()
        .and_then(|hooks| (hooks.shortcut)(action, cx))
}

pub(crate) fn hint(label: &str, action: &dyn Action, cx: &App) -> SharedString {
    cx.try_global::<PlatformHooks>()
        .map(|hooks| (hooks.hint)(label, action, cx))
        .unwrap_or_else(|| label.to_owned().into())
}
