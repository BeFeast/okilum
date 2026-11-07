//! Carry one explicit Restart click through latest-offer discovery/download.
use super::status::CheckStatus;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub(super) struct RestartIntent(AtomicBool);

pub(super) enum Next {
    Wait,
    Download,
    Apply,
    Refuse(String),
}

impl RestartIntent {
    pub(super) const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Duplicate clicks share the already-running transaction.
    pub(super) fn request(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }

    pub(super) fn requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub(super) fn cancel(&self) {
        self.0.store(false, Ordering::Release);
    }

    pub(super) fn poll(&self, busy: bool, status: CheckStatus) -> Next {
        if busy || !self.requested() {
            return Next::Wait;
        }
        match status {
            CheckStatus::Ready => {
                self.0.store(false, Ordering::Release);
                Next::Apply
            }
            // An Ask-mode metadata check may have started before the click.
            CheckStatus::Available(_) => Next::Download,
            CheckStatus::Failed(reason)
            | CheckStatus::RestartRetry(reason)
            | CheckStatus::Note(reason) => {
                self.0.store(false, Ordering::Release);
                Next::Refuse(reason)
            }
            CheckStatus::UpToDate(_) => {
                self.0.store(false, Ordering::Release);
                Next::Refuse(
                    "No newer update is currently available. Nothing was installed.".into(),
                )
            }
            _ => Next::Wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_waits_for_latest_download_and_consumes_one_explicit_click() {
        let intent = RestartIntent::new();
        assert!(matches!(intent.poll(false, CheckStatus::Ready), Next::Wait));
        assert!(intent.request());
        assert!(!intent.request());
        assert!(matches!(intent.poll(true, CheckStatus::Ready), Next::Wait));
        assert!(matches!(
            intent.poll(false, CheckStatus::Checking),
            Next::Wait
        ));
        assert!(matches!(
            intent.poll(false, CheckStatus::Available("0.1.102".into())),
            Next::Download
        ));
        assert!(matches!(
            intent.poll(true, CheckStatus::Downloading(Some(75))),
            Next::Wait
        ));
        assert!(matches!(
            intent.poll(false, CheckStatus::Ready),
            Next::Apply
        ));
        assert!(!intent.requested());
        assert!(matches!(intent.poll(false, CheckStatus::Ready), Next::Wait));
    }

    #[test]
    fn failure_cancel_and_withdrawal_revoke_restart_even_if_a_package_later_becomes_ready() {
        let cancelled = RestartIntent::new();
        assert!(cancelled.request());
        cancelled.cancel();
        assert!(matches!(
            cancelled.poll(false, CheckStatus::Ready),
            Next::Wait
        ));
        for status in [
            CheckStatus::Failed("network failure".into()),
            CheckStatus::Note("Update cancelled. Nothing was installed.".into()),
            CheckStatus::RestartRetry("editor guard refused".into()),
            CheckStatus::UpToDate(std::time::SystemTime::now()),
        ] {
            let intent = RestartIntent::new();
            assert!(intent.request());
            let Next::Refuse(reason) = intent.poll(false, status) else {
                panic!("a failed/cancelled/withdrawn update must refuse restart");
            };
            assert!(
                !reason.is_empty(),
                "refusal must explain why nothing was installed"
            );
            assert!(!intent.requested());
            assert!(matches!(intent.poll(false, CheckStatus::Ready), Next::Wait));
            assert!(intent.request());
            assert!(matches!(
                intent.poll(false, CheckStatus::Ready),
                Next::Apply
            ));
        }
    }
}
