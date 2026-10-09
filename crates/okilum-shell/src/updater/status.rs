//! The latest update check's outcome, shown inline next to the check button
//! in Settings and About (#995). Nothing to do never opens a modal.
// Linux has no in-app updater: only the settings harness reports outcomes.
#![cfg_attr(
    not(any(
        target_os = "macos",
        windows,
        test,
        all(target_os = "linux", feature = "settings-ui-harness")
    )),
    allow(dead_code)
)]
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckStatus {
    Idle,
    Checking,
    UpToDate(SystemTime),
    /// A newer version on the selected channel; installing is a separate action.
    Available(String),
    /// Windows: downloaded and waiting for a restart.
    Ready,
    /// A plain fact, e.g. nothing published on the channel yet.
    Note(String),
    Failed(String),
}

static STATUS: Mutex<(u64, CheckStatus)> = Mutex::new((0, CheckStatus::Idle));

pub(crate) fn set(status: CheckStatus) {
    let mut current = STATUS.lock().unwrap();
    current.0 += 1;
    current.1 = status;
}

pub(crate) fn get() -> CheckStatus {
    STATUS.lock().unwrap().1.clone()
}

/// Changes whenever the status does; the UI polls it to refresh.
pub(crate) fn generation() -> u64 {
    STATUS.lock().unwrap().0
}

/// The inline line for a status, or None while no check has run.
pub(crate) fn text(status: &CheckStatus, now: SystemTime) -> Option<String> {
    Some(match status {
        CheckStatus::Idle => return None,
        CheckStatus::Checking => "Checking for updates…".into(),
        CheckStatus::UpToDate(at) => {
            let ago = now.duration_since(*at).unwrap_or_default();
            format!("Up to date · checked {}", elapsed(ago))
        }
        CheckStatus::Available(version) => format!("Okilum {version} is available"),
        CheckStatus::Ready => "Update downloaded · restart to install".into(),
        CheckStatus::Note(note) => note.clone(),
        CheckStatus::Failed(error) => format!("Couldn’t check for updates: {error}"),
    })
}

fn elapsed(ago: Duration) -> String {
    match ago.as_secs() {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", ago.as_secs() / 60),
        secs => format!("{} h ago", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn lines_name_each_outcome_and_idle_shows_nothing() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let at = |secs| now - Duration::from_secs(secs);
        assert_eq!(text(&CheckStatus::Idle, now), None);
        for (status, line) in [
            (CheckStatus::Checking, "Checking for updates…"),
            (
                CheckStatus::UpToDate(at(5)),
                "Up to date · checked just now",
            ),
            (
                CheckStatus::UpToDate(at(600)),
                "Up to date · checked 10 min ago",
            ),
            (
                CheckStatus::UpToDate(at(7200)),
                "Up to date · checked 2 h ago",
            ),
            (
                CheckStatus::Available("0.1.9999".into()),
                "Okilum 0.1.9999 is available",
            ),
            (CheckStatus::Ready, "Update downloaded · restart to install"),
            (
                CheckStatus::Failed("offline".into()),
                "Couldn’t check for updates: offline",
            ),
        ] {
            assert_eq!(text(&status, now).as_deref(), Some(line));
        }
        // A clock that moved backwards still reads as just now.
        assert_eq!(
            text(&CheckStatus::UpToDate(now + Duration::from_secs(30)), now).as_deref(),
            Some("Up to date · checked just now")
        );
    }

    #[test]
    fn every_change_advances_the_generation() {
        let before = generation();
        set(CheckStatus::Checking);
        set(CheckStatus::Checking);
        assert_eq!(generation(), before + 2);
        assert_eq!(get(), CheckStatus::Checking);
        set(CheckStatus::Idle);
    }
}
