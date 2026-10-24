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
