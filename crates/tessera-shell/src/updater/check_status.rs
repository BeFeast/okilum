//! Process-local manual update feedback; preferences remain with the platform updater.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CheckStatus {
    #[default]
    Idle,
    Checking,
    UpToDate,
    NewerVersion,
    NewerOsRequired,
    OlderOsRequired,
    UnsupportedHardware,
    NoCompatibleUpdate,
    Failed,
    Available,
    Busy,
}
impl CheckStatus {
    pub(super) fn from_native(value: u32) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Checking,
            2 => Self::UpToDate,
            3 => Self::NewerVersion,
            4 => Self::NewerOsRequired,
            5 => Self::OlderOsRequired,
            6 => Self::UnsupportedHardware,
            7 => Self::NoCompatibleUpdate,
            8 => Self::Failed,
            9 => Self::Available,
            10 => Self::Busy,
            _ => Self::Failed,
        }
    }
    pub(crate) fn message(self) -> Option<&'static str> {
        match self {
            Self::Idle => None,
            Self::Checking => Some("Checking for updates…"),
            Self::UpToDate => Some("Tessera is up to date."),
            Self::NewerVersion => Some("You’re running a newer version than the latest release."),
            Self::NewerOsRequired => Some("The latest update requires a newer version of macOS."),
            Self::OlderOsRequired => {
                Some("The latest update doesn’t support this version of macOS.")
            }
            Self::UnsupportedHardware => Some("The latest update requires an Apple silicon Mac."),
            Self::NoCompatibleUpdate => Some("No compatible update is available."),
            Self::Failed => Some("Couldn’t check for updates. Try again."),
            Self::Available => Some("An update is available. Continue in the update window."),
            Self::Busy => Some("Another update is in progress."),
        }
    }
    pub(crate) fn checking(self) -> bool {
        matches!(self, Self::Checking | Self::Busy)
    }
    #[cfg(target_os = "macos")]
    pub(super) fn tracking(self) -> bool {
        self.checking() || self == Self::Available
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incompatible_and_unknown_results_never_claim_up_to_date_or_disable_retry() {
        for code in [4, 5, 6, 7, 8, 11, u32::MAX] {
            let state = CheckStatus::from_native(code);
            assert_ne!(state, CheckStatus::UpToDate);
            assert!(!state.checking());
            assert!(state.message().is_some());
        }
        assert_eq!(CheckStatus::from_native(2), CheckStatus::UpToDate);
        assert!(CheckStatus::from_native(1).checking());
        assert!(CheckStatus::from_native(10).checking());
    }
}
