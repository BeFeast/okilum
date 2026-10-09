extern "C" {
    fn okilum_updater_start();
    fn okilum_updater_available() -> bool;
    fn okilum_updater_check();
    fn okilum_updater_beta() -> bool;
    fn okilum_updater_set_beta(enabled: bool);
}

pub(super) fn start() {
    unsafe { okilum_updater_start() }
}
pub(super) fn available() -> bool {
    unsafe { okilum_updater_available() }
}
pub(super) fn check() {
    unsafe { okilum_updater_check() }
}
pub(super) fn beta() -> bool {
    unsafe { okilum_updater_beta() }
}
pub(super) fn set_beta(enabled: bool) {
    unsafe { okilum_updater_set_beta(enabled) }
}
