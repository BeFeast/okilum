//! Velopack lifecycle side of `okilum:` link registration (#1049); the
//! registry work and its tests are `okilum_core::link_registration`.

pub(crate) fn install() {
    if let Ok(executable) = std::env::current_exe() {
        okilum_core::link_registration::install(&executable);
    }
}

pub(crate) fn uninstall() {
    okilum_core::link_registration::uninstall();
}
