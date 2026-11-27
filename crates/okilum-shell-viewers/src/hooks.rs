//! App-registered callbacks for viewer modules that need app-level services
//! (toasts, fonts, file actions, HEIC image sources, and macOS Quick Look
//! thumbnail FFI) without depending on `Reader` (#3238, epic #3241).
use gpui::{App, Global, ImageSource, SharedString, Window};
use std::path::{Path, PathBuf};

pub type CodeFontFn = fn(&App) -> SharedString;
pub type NoteFontFn = fn(&App) -> SharedString;
pub type ToastErrorFn = fn(String, &mut Window, &mut App);
pub type ToastTransientFn = fn(String, &mut Window, &mut App);
pub type ToastDuckdbErrorFn = fn(String, &mut Window, &mut App);
pub type OpenFileFn = fn(&Path, &str, &mut Window, &mut App);
pub type ImageSourceFn = fn(PathBuf) -> ImageSource;

/// Platform-provided native thumbnail handle (implemented by macOS Quick Look
/// FFI in `okilum-shell`; absent on other platforms).
pub trait ThumbnailRequest: Send {
    fn poll(&self) -> anyhow::Result<Option<Vec<u8>>>;
}

pub type StartThumbnailFn = fn(&Path) -> anyhow::Result<Box<dyn ThumbnailRequest>>;

pub struct ViewerHooks {
    pub code_font: CodeFontFn,
    pub note_font: NoteFontFn,
    pub toast_error: ToastErrorFn,
    pub toast_transient: ToastTransientFn,
    pub toast_duckdb_error: ToastDuckdbErrorFn,
    pub open_file: OpenFileFn,
    pub image_source: ImageSourceFn,
    pub start_thumbnail: Option<StartThumbnailFn>,
}

impl Global for ViewerHooks {}

/// Register app-level hooks once at startup (and in `test_init`).
pub fn install(hooks: ViewerHooks, cx: &mut App) {
    cx.set_global(hooks);
}

pub(crate) fn code_font(cx: &App) -> SharedString {
    if let Some(hooks) = cx.try_global::<ViewerHooks>() {
        return (hooks.code_font)(cx);
    }
    SharedString::from("monospace")
}

pub(crate) fn note_font(cx: &App) -> SharedString {
    if let Some(hooks) = cx.try_global::<ViewerHooks>() {
        return (hooks.note_font)(cx);
    }
    SharedString::from("sans-serif")
}

pub(crate) fn toast_error(message: impl Into<String>, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<ViewerHooks>() {
        (hooks.toast_error)(message.into(), window, cx);
    }
}

pub(crate) fn toast_transient(message: impl Into<String>, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<ViewerHooks>() {
        (hooks.toast_transient)(message.into(), window, cx);
    }
}

pub(crate) fn toast_duckdb_error(message: impl Into<String>, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<ViewerHooks>() {
        (hooks.toast_duckdb_error)(message.into(), window, cx);
    }
}

pub(crate) fn open_file(root: &Path, rel: &str, window: &mut Window, cx: &mut App) {
    if let Some(hooks) = cx.try_global::<ViewerHooks>() {
        (hooks.open_file)(root, rel, window, cx);
    }
}

pub(crate) fn image_source(path: PathBuf, cx: &App) -> ImageSource {
    if let Some(hooks) = cx.try_global::<ViewerHooks>() {
        return (hooks.image_source)(path);
    }
    ImageSource::from(path)
}
