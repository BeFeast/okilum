extern "C" {
    fn tessera_updater_start();
    fn tessera_updater_available() -> bool;
    fn tessera_updater_check();
    fn tessera_updater_beta() -> bool;
    fn tessera_updater_set_beta(enabled: bool);
}

pub(super) fn start() {
    unsafe { tessera_updater_start() }
}
pub(super) fn available() -> bool {
    unsafe { tessera_updater_available() }
}
pub(super) fn check() {
    unsafe { tessera_updater_check() }
}
pub(super) fn beta() -> bool {
    unsafe { tessera_updater_beta() }
}
pub(super) fn set_beta(enabled: bool) {
    unsafe { tessera_updater_set_beta(enabled) }
}
