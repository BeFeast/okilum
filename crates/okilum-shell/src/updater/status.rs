//! The latest update check's outcome, shown inline next to the check button
//! in Settings and About (#995). Nothing to do never opens a modal.
// Each platform constructs a different subset: Ready only on Windows,
// none outside the settings harness on Linux.
#![allow(dead_code)]
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckStatus {
    Idle,
    Checking,
    Downloading(Option<u8>),
    Preparing,
    Restarting,
    /// Sparkle is waiting for application termination; another guarded attempt is available.
    RestartRetry(String),
    Relaunched,
    UpToDate(SystemTime),
    /// A newer version on the selected channel; installing is a separate action.
    Available(String),
    /// Windows: downloaded and waiting for a restart.
    Ready,
    /// A plain fact, e.g. nothing published on the channel yet.
    Note(String),
    Failed(String),
}

#[derive(Debug)]
pub(crate) struct UpdateState {
    generation: u64,
    pub status: CheckStatus,
}
impl UpdateState {
    pub const fn new() -> Self {
        Self {
            generation: 0,
            status: CheckStatus::Idle,
        }
    }
    pub fn set(&mut self, status: CheckStatus) {
        self.generation += 1;
        self.status = status;
    }
    pub fn begin_restart(&mut self) -> bool {
        if !matches!(
            self.status,
            CheckStatus::Ready | CheckStatus::RestartRetry(_)
        ) {
            return false;
        }
        self.set(CheckStatus::Restarting);
        true
    }
    pub fn relaunched(&mut self) -> bool {
        if self.status != CheckStatus::Restarting {
            return false;
        }
        self.set(CheckStatus::Relaunched);
        true
    }
}
static STATUS: Mutex<UpdateState> = Mutex::new(UpdateState::new());

pub(crate) fn set(status: CheckStatus) {
    STATUS.lock().unwrap().set(status);
}
pub(crate) fn get() -> CheckStatus {
    STATUS.lock().unwrap().status.clone()
}
pub(crate) fn begin_restart() -> bool {
    STATUS.lock().unwrap().begin_restart()
}
pub(crate) fn relaunched() -> bool {
    STATUS.lock().unwrap().relaunched()
}
/// Changes whenever the status does; the UI polls it to refresh.
pub(crate) fn generation() -> u64 {
    STATUS.lock().unwrap().generation
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
        CheckStatus::Downloading(Some(percent)) => format!("Downloading update · {percent}%"),
        CheckStatus::Downloading(None) => "Downloading update…".into(),
        CheckStatus::Preparing => "Preparing update…".into(),
        CheckStatus::Restarting => "Restarting to install update…".into(),
        CheckStatus::RestartRetry(reason) => {
            if reason.is_empty() {
                "Waiting for Okilum to quit. You can try Restart now again.".into()
            } else {
                reason.clone()
            }
        }
        CheckStatus::Relaunched => "Update installed".into(),
        CheckStatus::Ready => "Update ready to install".into(),
        CheckStatus::Note(note) => note.clone(),
        CheckStatus::Failed(error) => format!("Couldn’t check for updates: {error}"),
    })
}

pub(crate) fn in_flight() -> bool {
    matches!(
        get(),
        CheckStatus::Checking
            | CheckStatus::Downloading(_)
            | CheckStatus::Preparing
            | CheckStatus::Restarting
    )
}

/// Visible progress and actions share this projection on every platform.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProgressView {
    pub percent: Option<u8>,
    pub restart: bool,
}
pub(crate) fn progress(status: &CheckStatus) -> Option<ProgressView> {
    match status {
        CheckStatus::Downloading(percent) => Some(ProgressView {
            percent: percent.map(|p| p.min(100)),
            restart: false,
        }),
        CheckStatus::Checking | CheckStatus::Preparing | CheckStatus::Restarting => {
            Some(ProgressView {
                percent: None,
                restart: false,
            })
        }
        CheckStatus::Ready | CheckStatus::RestartRetry(_) => Some(ProgressView {
            percent: Some(100),
            restart: true,
        }),
        _ => None,
    }
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
            (CheckStatus::Ready, "Update ready to install"),
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

#[cfg(test)]
mod progress_tests {
    use super::*;
    #[test]
    fn refused_restart_restores_progress_and_requires_another_explicit_action() {
        let mut state = UpdateState::new();
        state.set(CheckStatus::Ready);
        assert!(state.begin_restart());
        state.set(CheckStatus::RestartRetry(String::new()));
        assert_eq!(
            progress(&state.status),
            Some(ProgressView {
                percent: Some(100),
                restart: true
            })
        );
        assert!(text(&state.status, SystemTime::now())
            .unwrap()
            .contains("try Restart now again"));
        assert!(
            !state.relaunched(),
            "remaining open is not a completed installation"
        );
        assert!(state.begin_restart());
        assert!(
            !state.begin_restart(),
            "a double click cannot authorize another attempt"
        );
        state.set(CheckStatus::Note("Update cancelled".into()));
        assert!(!state.begin_restart());
        assert_eq!(progress(&state.status), None);
    }

    #[test]
    fn idle_download_prepare_ready_restart_and_relaunch_projection() {
        let mut state = UpdateState::new();
        assert!(!state.begin_restart());
        assert!(!state.relaunched());
        assert_eq!(progress(&state.status), None);
        for value in [0, 25, 99, 100] {
            state.set(CheckStatus::Downloading(Some(value)));
            assert!(!state.begin_restart());
            let status = state.status.clone();
            assert_eq!(
                progress(&status),
                Some(ProgressView {
                    percent: Some(value),
                    restart: false
                })
            );
            assert!(text(&status, SystemTime::now())
                .unwrap()
                .contains(&format!("{value}%")));
        }
        assert_eq!(
            progress(&CheckStatus::Preparing),
            Some(ProgressView {
                percent: None,
                restart: false
            })
        );
        assert_eq!(
            progress(&CheckStatus::Ready),
            Some(ProgressView {
                percent: Some(100),
                restart: true
            })
        );
        assert_eq!(
            progress(&CheckStatus::Restarting),
            Some(ProgressView {
                percent: None,
                restart: false
            })
        );
        state.set(CheckStatus::Preparing);
        assert!(!state.begin_restart());
        state.set(CheckStatus::Ready);
        assert!(state.begin_restart());
        assert!(
            !state.begin_restart(),
            "one explicit restart per ready package"
        );
        assert!(
            state.relaunched(),
            "the backend confirms relaunch after restart"
        );
        assert!(!state.relaunched());
        assert_eq!(progress(&state.status), None);
        assert_eq!(progress(&CheckStatus::Failed("offline".into())), None);
    }
}
