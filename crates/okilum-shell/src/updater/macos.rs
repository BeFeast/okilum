#[path = "install_guard.rs"]
mod install_guard;
use super::status::{self, CheckStatus};
use std::ffi::{c_char, c_int, CStr};

extern "C" {
    fn okilum_updater_start();
    fn okilum_updater_set_report(report: extern "C" fn(c_int, *const c_char));
    fn okilum_updater_set_automatic_admission(admit: extern "C" fn(bool) -> bool);
    fn okilum_updater_available() -> bool;
    fn okilum_updater_check(download: bool, foreground: bool) -> bool;
    fn okilum_updater_set_mode(mode: c_int);
    fn okilum_updater_can_cancel() -> bool;
    fn okilum_updater_cancel();
    fn okilum_updater_restart() -> bool;
    fn okilum_updater_finish_restart() -> bool;
    fn okilum_updater_ready() -> bool;
    fn okilum_updater_guard_requested() -> bool;
    fn okilum_updater_restart_relaunches() -> bool;
    fn okilum_updater_refuse_restart();
    fn okilum_updater_beta() -> bool;
    fn okilum_updater_set_beta(enabled: bool);
    #[cfg(test)]
    fn okilum_updater_forget_beta();
}

/// Sparkle calls this on the main thread with a probe's outcome.
extern "C" fn report(kind: c_int, text: *const c_char) {
    if kind == 12 {
        super::run_check(super::Trigger::Focus);
        return;
    }
    let text = if text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    };
    super::restart_feedback::report(kind, &text);
    if kind == 13 {
        return;
    }
    if kind == 5 && !text.is_empty() {
        // The ready package's version keys the single update notice (#2679).
        *READY_PACKAGE.lock().unwrap() = Some(text.clone());
    }
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
        7 => CheckStatus::Note("Update cancelled. Nothing was installed.".into()),
        9 => CheckStatus::Checking,
        10 => CheckStatus::RestartRetry(text),
        _ => CheckStatus::Failed(text),
    });
}

extern "C" fn admit_automatic(automatic: bool) -> bool {
    super::admit_sparkle_check(automatic)
}

pub(super) fn start() {
    unsafe {
        okilum_updater_set_report(report);
        okilum_updater_set_automatic_admission(admit_automatic);
        configure_mode();
        okilum_updater_start()
    }
}
pub(super) fn available() -> bool {
    unsafe { okilum_updater_available() }
}
pub(super) fn check(download: bool, foreground: bool) -> bool {
    // A repeated check must not replace download progress or a ready action.
    if unsafe { okilum_updater_check(download, foreground) } {
        status::set(CheckStatus::Checking);
        true
    } else {
        false
    }
}
pub(super) fn configure_mode() {
    unsafe {
        okilum_updater_set_mode(match super::mode() {
            super::Mode::Background => 0,
            super::Mode::Ask => 1,
            super::Mode::Off => 2,
        })
    }
}
pub(super) fn beta() -> bool {
    unsafe { okilum_updater_beta() }
}
pub(super) fn set_beta(enabled: bool) {
    unsafe { okilum_updater_set_beta(enabled) }
}

pub(super) fn ready() -> bool {
    unsafe { okilum_updater_ready() }
}

static READY_PACKAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The version Sparkle last reported ready; a newer one replaces the notice.
pub(super) fn ready_package() -> Option<String> {
    READY_PACKAGE.lock().unwrap().clone()
}
pub(super) fn restart(_cx: &mut gpui::App) {
    super::restart_feedback::attempt();
    // Freshness is asynchronous. The final save guards run after it completes,
    // through finish_requested_restart, even for Sparkle's own Restart button.
    // A click on a notice older than Sparkle's current state keeps its
    // consent: the bridge installs the newest ready update, downloading it
    // first if needed (#2679). Busy and failed clicks are reported in the same
    // single notice. Never discard a refusal without feedback.
    unsafe { okilum_updater_restart() };
}

pub(super) fn finish_requested_restart(cx: &mut gpui::App) {
    if !unsafe { okilum_updater_guard_requested() } {
        return;
    }
    let result = protect_verified_install(
        cx,
        unsafe { okilum_updater_restart_relaunches() },
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
        // Shown inside the single update notice, not as a second toast.
        super::restart_feedback::refused(format!("Could not restart to update: {error}. Save or close the affected note, then try again."));
    }
}

/// Both AppKit Quit and Restart reach this boundary only after freshness. The
/// callbacks isolate the irreversible continuation; editor protection is real
/// and always reads the current GPUI editor state before committing either path.
pub(crate) fn protect_verified_install(
    cx: &mut gpui::App,
    relaunch: bool,
    commit: impl FnOnce() -> anyhow::Result<()>,
    refuse: impl FnOnce(),
) -> anyhow::Result<()> {
    #[cfg(test)]
    let directory = cx
        .try_global::<crate::reader_history::TestSessionDirectory>()
        .map(|directory| Ok(directory.0.clone()))
        .unwrap_or_else(crate::reader_history::state_directory);
    #[cfg(not(test))]
    let directory = crate::reader_history::state_directory();
    install_guard::finish(
        cx,
        crate::reader_update_restart::protect_editors,
        |cx| {
            crate::reader_ui_state::prepare_for_restart(cx)?;
            if relaunch {
                let directory = directory
                    .as_ref()
                    .map_err(|error| anyhow::anyhow!("{error:#}"))?;
                crate::reader_update_restart::persist(directory, cx)?;
            }
            Ok(())
        },
        |_| commit(),
        |_| {
            if let Ok(directory) = &directory {
                crate::reader_update_restart::clear(directory);
            }
            refuse();
        },
    )
}

pub(super) fn can_cancel() -> bool {
    unsafe { okilum_updater_can_cancel() }
}
pub(super) fn cancel() {
    unsafe { okilum_updater_cancel() }
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
