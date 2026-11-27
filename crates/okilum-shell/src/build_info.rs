//! What this binary was built with, for QA tooling (#1043). A binary compiled
//! in the same cargo invocation as an example gets the dev-dependency feature
//! gpui `test-support` (resolver 2), which changes runtime behaviour (leak
//! detection, test hooks) and made native checks fail falsely.

/// Whether GPUI was compiled with its `test-support` feature.
///
/// GPUI defines `HitboxId::placeholder()` only with that feature. An inherent
/// associated function shadows a trait one of the same name, so the call
/// below resolves to GPUI's when it exists and to the fallback otherwise; the
/// return type then tells which.
pub(crate) fn gpui_test_support() -> bool {
    use gpui::HitboxId;

    struct Absent;
    #[allow(dead_code)] // Unused whenever GPUI's own function exists.
    trait Fallback {
        fn placeholder() -> Absent;
    }
    impl Fallback for HitboxId {
        fn placeholder() -> Absent {
            Absent
        }
    }
    trait Probe {
        const PRESENT: bool;
    }
    impl Probe for HitboxId {
        const PRESENT: bool = true;
    }
    impl Probe for Absent {
        const PRESENT: bool = false;
    }
    fn present<T: Probe>(_: T) -> bool {
        T::PRESENT
    }
    present(HitboxId::placeholder())
}

/// One line per fact, `key: value`, for `okilum --build-info`. `package:`
/// appears only when the installed package differs from the build (#1319).
pub(crate) fn report() -> String {
    #[cfg(all(unix, feature = "brain"))]
    let shell_brain_test_support = okilum_shell_brain::TEST_SUPPORT;
    #[cfg(not(all(unix, feature = "brain")))]
    let shell_brain_test_support = false;
    format!(
        "version: {}\n{}build: {}\nsource: {}\ngpui-test-support: {}\nshell-kit-test-support: {}\nshell-platform-test-support: {}\nshell-text-test-support: {}\nshell-viewers-test-support: {}\nshell-logs-test-support: {}\nshell-project-test-support: {}\nshell-brain-test-support: {}\nappimage-gui-acceptance: {}\ninstall: {}\nchannel: {}\n",
        env!("OKILUM_RELEASE_VERSION"),
        crate::version_info::package()
            .map(|package| format!("package: {package}\n"))
            .unwrap_or_default(),
        env!("OKILUM_BUILD_VERSION"),
        env!("OKILUM_SOURCE_COMMIT"),
        if gpui_test_support() { "yes" } else { "no" },
        if okilum_shell_kit::TEST_SUPPORT { "yes" } else { "no" },
        if okilum_shell_platform::TEST_SUPPORT { "yes" } else { "no" },
        if okilum_shell_text::TEST_SUPPORT { "yes" } else { "no" },
        if okilum_shell_viewers::TEST_SUPPORT { "yes" } else { "no" },
        if okilum_shell_logs::TEST_SUPPORT { "yes" } else { "no" },
        if okilum_shell_project::TEST_SUPPORT { "yes" } else { "no" },
        if shell_brain_test_support { "yes" } else { "no" },
        if cfg!(all(target_os = "linux", feature = "appimage-acceptance-harness")) { "yes" } else { "no" },
        crate::version_info::install_kind(),
        crate::updater::channel()
    )
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    /// Unit tests always build GPUI with test-support (dev-dependency), so
    /// the probe must see it here: the positive control for the detector.
    #[test]
    fn tests_see_gpui_test_support() {
        assert!(super::gpui_test_support());
        assert!(super::report().contains("gpui-test-support: yes"));
        // Same for the kit, platform, text, project and brain crates' test seams (#3233, #3234, #3235, #3236, #3237).
        assert!(super::report().contains("shell-kit-test-support: yes"));
        assert!(super::report().contains("shell-platform-test-support: yes"));
        assert!(super::report().contains("shell-text-test-support: yes"));
        assert!(super::report().contains("shell-viewers-test-support: yes"));
        assert!(super::report().contains("shell-logs-test-support: yes"));
        assert!(super::report().contains("shell-project-test-support: yes"));
        #[cfg(all(unix, feature = "brain"))]
        assert!(super::report().contains("shell-brain-test-support: yes"));
    }
}
