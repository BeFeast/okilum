// Hosted fixture links the same final coordinator as the GPUI/Rust adapter.
// The fixture editor is a real mutable file; C callbacks provide its protection
// and persistence operations instead of constructing a GPUI window.
#[path = "../../../crates/okilum-shell/src/updater/install_guard.rs"]
mod install_guard;

extern "C" {
    fn fixture_protect_current_editor() -> bool;
    fn fixture_persist_ui_state() -> bool;
    fn okilum_updater_finish_restart() -> bool;
    fn okilum_updater_refuse_restart();
}

#[no_mangle]
pub extern "C" fn fixture_finish_verified_install() -> bool {
    install_guard::finish(
        &mut (),
        |_| unsafe { fixture_protect_current_editor().then_some(()).ok_or(()) },
        |_| unsafe { fixture_persist_ui_state().then_some(()).ok_or(()) },
        |_| unsafe { okilum_updater_finish_restart().then_some(()).ok_or(()) },
        |_| unsafe { okilum_updater_refuse_restart() },
    )
    .is_ok()
}

// Use the same status/action projection as About, without a GPUI window.
#[path = "../../../crates/okilum-shell/src/updater/status.rs"]
mod status;

#[no_mangle]
pub extern "C" fn fixture_record_report(kind: i32) {
    use status::CheckStatus;
    match kind {
        4 => status::set(CheckStatus::Preparing),
        5 => status::set(CheckStatus::Ready),
        6 => {
            status::begin_restart();
        }
        7 => status::set(CheckStatus::Note("Update cancelled".into())),
        10 => status::set(CheckStatus::RestartRetry(String::new())),
        _ => {}
    }
}

#[no_mangle]
pub extern "C" fn fixture_about_retry_enabled() -> bool {
    let status = status::get();
    matches!(status, status::CheckStatus::RestartRetry(_))
        && status::progress(&status).is_some_and(|view| view.restart && view.percent == Some(100))
}
