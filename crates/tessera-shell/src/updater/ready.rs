//! Downloaded-package readiness, independent of GUI and platform SDK calls.
//! A feed result is not ready: only a verified pending package may be recorded.
#[derive(Default, Debug)]
pub(super) struct Ready {
    package: Option<String>,
    announced: Option<String>,
    restarting: bool,
}

impl Ready {
    pub(super) fn observe(&mut self, package: Option<String>) {
        if self.package != package {
            self.package = package;
            self.announced = None;
            self.restarting = false;
        }
    }

    pub(super) fn package(&self) -> Option<&str> {
        self.package.as_deref()
    }

    /// Call only after a Reader is available to present the notification.
    pub(super) fn take_announcement(&mut self) -> Option<String> {
        let package = self.package.as_ref()?;
        if self.announced.as_ref() == Some(package) {
            return None;
        }
        self.announced = Some(package.clone());
        Some(package.clone())
    }

    /// Reserve one explicit restart, tied to the package the user saw.
    pub(super) fn begin_restart(&mut self, expected: &str) -> bool {
        if self.restarting || self.package.as_deref() != Some(expected) {
            return false;
        }
        self.restarting = true;
        true
    }

    pub(super) fn restart_failed(&mut self) {
        self.restarting = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_survives_absent_window_and_dismissal_without_reannouncing() {
        let mut state = Ready::default();
        assert_eq!(state.take_announcement(), None);
        state.observe(Some("package-1".into()));
        state.observe(Some("package-1".into()));
        assert_eq!(state.take_announcement().as_deref(), Some("package-1"));
        assert_eq!(state.take_announcement(), None);
        assert_eq!(state.package(), Some("package-1"));
        state.observe(Some("package-2".into()));
        assert_eq!(state.take_announcement().as_deref(), Some("package-2"));
        assert!(!state.begin_restart("package-1"));
        assert!(state.begin_restart("package-2"));
    }

    #[test]
    fn restart_is_bound_to_observed_package_and_explicit_retry() {
        let mut state = Ready::default();
        state.observe(Some("package-1".into()));
        assert!(!state.begin_restart("stale-package"));
        assert!(state.begin_restart("package-1"));
        assert!(!state.begin_restart("package-1"));
        state.restart_failed();
        assert!(state.begin_restart("package-1"));
        state.observe(None);
        assert_eq!(state.package(), None);
        assert!(!state.begin_restart("package-1"));
    }
}

/// Preparation must finish before the external updater is armed or GPUI quits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestartStep {
    Arguments,
    ProtectEditors,
    PersistState,
    ArmUpdater,
    Quit,
}

pub(super) fn restart_transaction<E>(
    mut step: impl FnMut(RestartStep) -> Result<(), E>,
) -> Result<(), E> {
    for stage in [
        RestartStep::Arguments,
        RestartStep::ProtectEditors,
        RestartStep::PersistState,
        RestartStep::ArmUpdater,
        RestartStep::Quit,
    ] {
        step(stage)?;
    }
    Ok(())
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    #[test]
    fn restart_stops_at_each_failure_and_quits_only_after_arming() {
        let stages = [
            RestartStep::Arguments,
            RestartStep::ProtectEditors,
            RestartStep::PersistState,
            RestartStep::ArmUpdater,
            RestartStep::Quit,
        ];
        for fail in 0..4 {
            let mut calls = vec![];
            let result = restart_transaction(|stage| {
                calls.push(stage);
                if stage == stages[fail] {
                    Err(())
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            assert_eq!(calls, stages[..=fail]);
            assert!(!calls.contains(&RestartStep::Quit));
        }
        let mut calls = vec![];
        restart_transaction(|stage| {
            calls.push(stage);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(calls, stages);
    }
}
