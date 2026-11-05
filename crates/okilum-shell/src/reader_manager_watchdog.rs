//! Deadlines for the embedded Manager. No WebKit or wall-clock dependency.
use std::time::{Duration, Instant};

pub(super) const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const PAGE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    Capability,
    Page,
}

#[derive(Default)]
pub(super) struct Watchdog {
    pending: Option<(u64, Stage, Instant)>,
}

impl Watchdog {
    pub(super) fn capability(&mut self, generation: u64, now: Instant) {
        self.pending = Some((generation, Stage::Capability, now + CAPABILITY_TIMEOUT));
    }

    /// Start at load_url, before WebKit emits Started (which means didCommit).
    /// Redirects must not keep extending a stalled navigation's deadline.
    pub(super) fn page(&mut self, generation: u64, now: Instant) -> bool {
        if matches!(self.pending, Some((g, Stage::Page, _)) if g == generation) {
            return false;
        }
        self.pending = Some((generation, Stage::Page, now + PAGE_TIMEOUT));
        true
    }

    /// The preflight's late Finished callback cannot complete the T3 load.
    pub(super) fn finished(&mut self, generation: u64, page: &str) -> bool {
        if !matches!(self.pending, Some((g, Stage::Page, _)) if g == generation)
            || !url::Url::parse(page).is_ok_and(|u| matches!(u.scheme(), "https" | "http"))
        {
            return false;
        }
        self.pending = None;
        true
    }

    pub(super) fn expire(&mut self, generation: u64, now: Instant) -> Option<Stage> {
        let (g, stage, deadline) = self.pending?;
        if g != generation || now < deadline {
            return None;
        }
        self.pending = None;
        Some(stage)
    }
}

/// Diagnostics keep the route, never credentials, queries or URL fragments.
pub(super) fn diagnostic_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return "<invalid URL>".into();
    };
    if !matches!(url.scheme(), "https" | "http" | "about") {
        return format!("{}:<redacted>", url.scheme());
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn missing_capability_expires_once_at_five_seconds() {
        let now = Instant::now();
        let mut watch = Watchdog::default();
        watch.capability(1, now);
        assert_eq!(
            watch.expire(1, now + CAPABILITY_TIMEOUT - Duration::from_millis(1)),
            None
        );
        assert_eq!(
            watch.expire(1, now + CAPABILITY_TIMEOUT),
            Some(Stage::Capability)
        );
        assert_eq!(watch.expire(1, now + PAGE_TIMEOUT), None);
    }

    #[test]
    fn supported_engine_gets_twenty_seconds_from_navigation_request() {
        let now = Instant::now();
        let mut watch = Watchdog::default();
        watch.capability(1, now);
        let navigation = now + Duration::from_secs(4);
        assert!(watch.page(1, navigation));
        assert_eq!(watch.expire(1, now + CAPABILITY_TIMEOUT), None);
        assert!(!watch.finished(1, "about:blank"));
        assert!(!watch.page(1, navigation + Duration::from_secs(10)));
        assert_eq!(
            watch.expire(1, navigation + PAGE_TIMEOUT - Duration::from_millis(1)),
            None
        );
        assert_eq!(
            watch.expire(1, navigation + PAGE_TIMEOUT),
            Some(Stage::Page)
        );
    }

    #[test]
    fn finished_document_cancels_timeout_and_later_navigation_is_watched() {
        let now = Instant::now();
        let mut watch = Watchdog::default();
        watch.page(1, now);
        assert!(watch.finished(1, "https://t3.example/env/thread"));
        assert_eq!(watch.expire(1, now + PAGE_TIMEOUT), None);
        assert!(watch.page(1, now + PAGE_TIMEOUT));
        assert_eq!(watch.expire(1, now + PAGE_TIMEOUT * 2), Some(Stage::Page));
    }

    #[test]
    fn old_callbacks_and_timers_cannot_complete_or_expire_a_retry() {
        let now = Instant::now();
        let mut watch = Watchdog::default();
        watch.page(1, now);
        watch.capability(2, now);
        assert!(!watch.finished(1, "https://t3.example/env/thread"));
        assert!(!watch.finished(2, "https://t3.example/env/thread"));
        assert_eq!(watch.expire(1, now + PAGE_TIMEOUT), None);
        assert_eq!(
            watch.expire(2, now + CAPABILITY_TIMEOUT),
            Some(Stage::Capability)
        );
        watch.capability(3, now);
        watch = Watchdog::default(); // Closing or rejecting an unsupported engine.
        assert_eq!(watch.expire(3, now + PAGE_TIMEOUT), None);
    }

    #[test]
    fn diagnostics_strip_tokens_credentials_and_non_web_payloads() {
        assert_eq!(
            diagnostic_url("https://user:secret@t3.example/env/thread?token=secret#secret"),
            "https://t3.example/env/thread"
        );
        assert_eq!(diagnostic_url("about:blank"), "about:blank");
        assert_eq!(diagnostic_url("data:text/html,secret"), "data:<redacted>");
        assert_eq!(diagnostic_url("invalid secret"), "<invalid URL>");
    }
}
