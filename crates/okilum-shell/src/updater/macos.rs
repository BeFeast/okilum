#[path = "install_guard.rs"]
mod install_guard;
use super::status::{self, CheckStatus};
use std::ffi::{c_char, c_int, CStr};

extern "C" {
    fn okilum_updater_start();
    fn okilum_updater_set_report(report: extern "C" fn(c_int, *const c_char));
    fn okilum_updater_available() -> bool;
    fn okilum_updater_check() -> bool;
    fn okilum_updater_install();
    fn okilum_updater_restart() -> bool;
    fn okilum_updater_finish_restart() -> bool;
    fn okilum_updater_ready() -> bool;
    fn okilum_updater_guard_requested() -> bool;
    fn okilum_updater_refuse_restart();
    fn okilum_updater_beta() -> bool;
    fn okilum_updater_set_beta(enabled: bool);
    #[cfg(test)]
    fn okilum_updater_forget_beta();
}

/// Sparkle calls this on the main thread with a probe's outcome.
extern "C" fn report(kind: c_int, text: *const c_char) {
    let text = if text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    };
    if kind == 6 {
        status::begin_restart();
        return;
    }
    if kind == 8 {
        status::relaunched();
        return;
    }
    status::set(match kind {
        0 => CheckStatus::UpToDate(std::time::SystemTime::now()),
        1 => CheckStatus::Available(text),
        3 => CheckStatus::Downloading(text.parse::<u8>().ok().map(|p| p.min(100))),
        4 => CheckStatus::Preparing,
        5 => CheckStatus::Ready,
        6 => CheckStatus::Restarting,
        7 => CheckStatus::Idle,
        _ => CheckStatus::Failed(text),
    });
}

pub(super) fn start() {
    unsafe {
        okilum_updater_set_report(report);
        okilum_updater_start()
    }
}
pub(super) fn available() -> bool {
    unsafe { okilum_updater_available() }
}
pub(super) fn check() {
    // A repeated check must not replace download progress or a ready action.
    if unsafe { okilum_updater_check() } {
        status::set(CheckStatus::Checking);
    }
}
pub(super) fn install() {
    unsafe { okilum_updater_install() }
}
pub(super) fn beta() -> bool {
    unsafe { okilum_updater_beta() }
}
pub(super) fn set_beta(enabled: bool) {
    unsafe { okilum_updater_set_beta(enabled) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1307: an install that never chose a channel follows stable; an
    /// explicit choice either way stays. Beta was the default, so a stable
    /// install took the next every-merge beta.
    #[test]
    fn channel_defaults_to_stable_and_keeps_an_explicit_choice() {
        unsafe { okilum_updater_forget_beta() };
        assert!(!beta(), "a fresh install follows stable");
        set_beta(true);
        assert!(beta(), "an explicit Beta choice stays");
        set_beta(false);
        assert!(!beta(), "an explicit Stable choice stays");
        unsafe { okilum_updater_forget_beta() };
        assert!(!beta());
    }
}

pub(super) fn ready() -> bool {
    unsafe { okilum_updater_ready() }
}
pub(super) fn restart(_cx: &mut gpui::App) {
    if !ready() {
        return;
    }
    // Freshness is asynchronous. The final save guards run after it completes,
    // through finish_requested_restart, even for Sparkle's own Restart button.
    unsafe { okilum_updater_restart() };
}

pub(super) fn finish_requested_restart(cx: &mut gpui::App) {
    if !unsafe { okilum_updater_guard_requested() } {
        return;
    }
    let result = protect_verified_install(
        cx,
        || {
            anyhow::ensure!(
                unsafe { okilum_updater_finish_restart() },
                "The update is no longer ready. Check for updates again."
            );
            Ok(())
        },
        || unsafe { okilum_updater_refuse_restart() },
    );
    if let Err(error) = result {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, cx| {
                crate::reader_toast::error(
                    format!("Could not restart to update: {error}"),
                    window,
                    cx,
                )
            });
        }
    }
}

/// Both AppKit Quit and Restart reach this boundary only after freshness. The
/// callbacks isolate the irreversible continuation; editor protection is real
/// and always reads the current GPUI editor state before committing either path.
pub(crate) fn protect_verified_install(
    cx: &mut gpui::App,
    commit: impl FnOnce() -> anyhow::Result<()>,
    refuse: impl FnOnce(),
) -> anyhow::Result<()> {
    install_guard::finish(
        cx,
        |cx| {
            anyhow::ensure!(
                crate::reader_editor::protect_all_for_quit(cx),
                "Your latest edits could not be protected. Okilum will stay open."
            );
            Ok(())
        },
        crate::reader_ui_state::prepare_for_restart,
        |_| commit(),
        |_| refuse(),
    )
}
