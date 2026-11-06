//! Pure recovery presentation and app-session error dismissal. Neither changes
//! transport authority, retained projections, drafts, or command records.

use okilum_t3::stream::StreamError;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ErrorOrigin {
    History,
    Action,
    Setup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConnectionContext {
    Connect,
    Receive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConnectionFailure {
    pub cause: StreamError,
    pub context: ConnectionContext,
}

/// Preserve the existing worker retry policy. A terminal failure may still
/// offer an explicit recovery action; presentation never starts that action.
pub(super) fn is_transient(error: &StreamError) -> bool {
    matches!(
        error,
        StreamError::Closed | StreamError::Unreachable | StreamError::Http(408 | 429 | 500..=599)
    )
}

/// Tuple fields avoid collisions from separators in source identities or text.
/// The full already-sanitized text is intentionally neither trimmed nor clamped.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ErrorDismissKey {
    origin: String,
    thread: String,
    error_origin: ErrorOrigin,
    full_safe_text: String,
}

impl ErrorDismissKey {
    pub(super) fn new(
        origin: &str,
        thread: &str,
        error_origin: ErrorOrigin,
        full_safe_text: &str,
    ) -> Self {
        Self {
            origin: origin.to_owned(),
            thread: thread.to_owned(),
            error_origin,
            full_safe_text: full_safe_text.to_owned(),
        }
    }
}

/// App-owned presentation mask: survives view remounts and navigation, ends
/// with the app session, and never clears or acknowledges the canonical error.
#[derive(Default)]
pub(super) struct SessionDismissals {
    keys: HashSet<ErrorDismissKey>,
}

impl gpui::Global for SessionDismissals {}

impl SessionDismissals {
    pub(super) fn is_dismissed(&self, key: &ErrorDismissKey) -> bool {
        self.keys.contains(key)
    }

    /// The owner must notify its view after a newly inserted dismissal.
    pub(super) fn dismiss(&mut self, key: ErrorDismissKey) -> bool {
        self.keys.insert(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_retry_is_limited_to_existing_transient_causes() {
        for error in [StreamError::Closed, StreamError::Unreachable] {
            assert!(is_transient(&error));
        }
        for status in 100..=599 {
            assert_eq!(
                is_transient(&StreamError::Http(status)),
                matches!(status, 408 | 429 | 500..=599),
                "HTTP {status}"
            );
        }
        for error in [
            StreamError::PairingRequired,
            StreamError::Expired,
            StreamError::WrongOrigin,
            StreamError::MissingReadPermission,
            StreamError::Protocol("invalid snapshot"),
            StreamError::IncompatibleProtocol,
            StreamError::Rejected,
        ] {
            assert!(!is_transient(&error), "{error:?}");
        }
    }

    #[test]
    fn dismissal_is_session_scoped_and_survives_reconstructed_view_keys() {
        let key =
            || ErrorDismissKey::new("https://a.test", "thread-a", ErrorOrigin::Setup, "Error");
        let mut session = SessionDismissals::default();
        assert!(!session.is_dismissed(&key()));
        assert!(session.dismiss(key()));
        assert!(!session.dismiss(key()));
        let clean_other_thread =
            ErrorDismissKey::new("https://a.test", "thread-b", ErrorOrigin::Setup, "Error");
        assert!(!session.is_dismissed(&clean_other_thread));
        assert!(
            session.is_dismissed(&key()),
            "returning to the same conversation stays dismissed"
        );
        assert!(
            !SessionDismissals::default().is_dismissed(&key()),
            "a new app session has no dismissals"
        );
    }

    #[test]
    fn dismissal_identity_includes_origin_thread_slot_and_full_untrimmed_text() {
        let mut session = SessionDismissals::default();
        let text = "Same preview\nSecond line\nThird line\nHidden detail";
        session.dismiss(ErrorDismissKey::new(
            "https://a.test",
            "thread",
            ErrorOrigin::Setup,
            text,
        ));
        for key in [
            ErrorDismissKey::new("https://b.test", "thread", ErrorOrigin::Setup, text),
            ErrorDismissKey::new("https://a.test", "other-thread", ErrorOrigin::Setup, text),
            ErrorDismissKey::new("https://a.test", "thread", ErrorOrigin::History, text),
            ErrorDismissKey::new("https://a.test", "thread", ErrorOrigin::Action, text),
            ErrorDismissKey::new("https://a.test", "thread", ErrorOrigin::Action, text),
            ErrorDismissKey::new(
                "https://a.test",
                "thread",
                ErrorOrigin::Setup,
                "Same preview\nSecond line\nThird line\nOther detail",
            ),
            ErrorDismissKey::new(
                "https://a.test",
                "thread",
                ErrorOrigin::Setup,
                &format!("{text}\n"),
            ),
            ErrorDismissKey::new(
                "https://a.test",
                "thread",
                ErrorOrigin::Setup,
                &format!(" {text}"),
            ),
        ] {
            assert!(!session.is_dismissed(&key), "{key:?}");
        }
    }

    #[test]
    fn tuple_key_cannot_collide_via_separator_characters() {
        let mut session = SessionDismissals::default();
        session.dismiss(ErrorDismissKey::new(
            "origin\0thread",
            "suffix",
            ErrorOrigin::Setup,
            "error",
        ));
        assert!(!session.is_dismissed(&ErrorDismissKey::new(
            "origin",
            "thread\0suffix",
            ErrorOrigin::Setup,
            "error"
        )));
    }
}
