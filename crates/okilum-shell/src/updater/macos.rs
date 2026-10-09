use super::status::{self, CheckStatus};
use std::ffi::{c_char, c_int, CStr};

extern "C" {
    fn okilum_updater_start();
    fn okilum_updater_set_report(report: extern "C" fn(c_int, *const c_char));
    fn okilum_updater_available() -> bool;
    fn okilum_updater_check();
    fn okilum_updater_install();
    fn okilum_updater_beta() -> bool;
    fn okilum_updater_set_beta(enabled: bool);
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
    status::set(match kind {
        0 => CheckStatus::UpToDate(std::time::SystemTime::now()),
        1 => CheckStatus::Available(text),
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
    status::set(CheckStatus::Checking);
    unsafe { okilum_updater_check() }
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
