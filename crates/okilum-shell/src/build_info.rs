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

/// One line per fact, `key: value`, for `okilum --build-info`.
pub(crate) fn report() -> String {
    format!(
        "version: {}\nbuild: {}\ngpui-test-support: {}\n",
        env!("OKILUM_RELEASE_VERSION"),
        env!("OKILUM_BUILD_VERSION"),
        if gpui_test_support() { "yes" } else { "no" }
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
    }
}
