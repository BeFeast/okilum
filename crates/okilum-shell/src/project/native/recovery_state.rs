//! Pure recovery presentation and app-session error dismissal. Neither changes
//! transport authority, retained projections, drafts, or command records.

use okilum_t3::stream::StreamError;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ErrorOrigin {
    Connection,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryClass {
    Transient,
    Credentials,
    Incompatible,
    ReadPermission,
    WrongOrigin,
    Rejected,
    OtherHttp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryAction {
    Reconnect,
    PairAgain,
    OpenT3,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecoveryPresentation {
    pub summary: String,
    pub full_safe_text: String,
    pub class: RecoveryClass,
    pub action: RecoveryAction,
}

/// Preserve the existing worker retry policy. A terminal failure may still
/// offer an explicit recovery action; presentation never starts that action.
pub(super) fn is_transient(error: &StreamError) -> bool {
    matches!(
        error,
        StreamError::Closed | StreamError::Unreachable | StreamError::Http(408 | 429 | 500..=599)
    )
}

pub(super) fn presentation(error: &StreamError) -> RecoveryPresentation {
    let (class, action) = match error {
        StreamError::Closed | StreamError::Unreachable => {
            (RecoveryClass::Transient, RecoveryAction::Reconnect)
        }
        StreamError::Http(_) if is_transient(error) => {
            (RecoveryClass::Transient, RecoveryAction::Reconnect)
        }
        StreamError::PairingRequired | StreamError::Expired => {
            (RecoveryClass::Credentials, RecoveryAction::PairAgain)
        }
        StreamError::Protocol(_) | StreamError::IncompatibleProtocol => {
            (RecoveryClass::Incompatible, RecoveryAction::OpenT3)
        }
        StreamError::MissingReadPermission => {
            (RecoveryClass::ReadPermission, RecoveryAction::OpenT3)
        }
        StreamError::WrongOrigin => (RecoveryClass::WrongOrigin, RecoveryAction::OpenT3),
        StreamError::Rejected => (RecoveryClass::Rejected, RecoveryAction::OpenT3),
        StreamError::Http(_) => (RecoveryClass::OtherHttp, RecoveryAction::Reconnect),
    };
    let summary = match error {
        StreamError::Closed | StreamError::Unreachable => {
            "The connection to T3 was lost. Reconnecting…".to_owned()
        }
        StreamError::PairingRequired | StreamError::Expired => {
            "Sign in to T3 again to continue this conversation.".to_owned()
        }
        StreamError::WrongOrigin => {
            "This connection belongs to a different T3 server. Check the connection in Settings."
                .to_owned()
        }
        StreamError::MissingReadPermission => {
            "This T3 connection cannot read conversations. Check its access in T3.".to_owned()
        }
        StreamError::IncompatibleProtocol => {
            "This version of T3 cannot connect to Okilum. Open T3 to continue.".to_owned()
        }
        StreamError::Protocol(_) => {
            "Okilum could not read a response from T3. Open T3 to continue.".to_owned()
        }
        StreamError::Rejected => {
            "T3 did not allow this conversation to connect. Open it in T3 to continue.".to_owned()
        }
        StreamError::Http(status) if is_transient(error) => {
            format!("T3 is temporarily unavailable (HTTP {status}). Reconnecting…")
        }
        StreamError::Http(status) => {
            format!("T3 rejected the connection (HTTP {status}). Try reconnecting or open T3.")
        }
    };
    RecoveryPresentation {
        summary,
        // StreamError deliberately excludes HTTP bodies, URLs and peer error
        // payloads. Keep its full safe details, including protocol context.
        full_safe_text: error.to_string(),
        class,
        action,
    }
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
    fn connection_summaries_are_plain_while_exact_safe_details_remain_available() {
        for cause in [
            StreamError::PairingRequired,
            StreamError::Expired,
            StreamError::IncompatibleProtocol,
            StreamError::Protocol("invalid snapshot"),
            StreamError::MissingReadPermission,
            StreamError::WrongOrigin,
        ] {
            let shown = presentation(&cause);
            assert!(!shown.summary.contains("MCP"));
            assert!(!shown.summary.contains("WebSocket"));
            assert!(!shown.summary.contains("orchestration:"));
            assert!(!shown.summary.contains("protocol v2"));
            assert_eq!(shown.full_safe_text, cause.to_string());
        }
        assert!(presentation(&StreamError::Expired)
            .summary
            .contains("Sign in"));
        assert!(presentation(&StreamError::Http(503))
            .summary
            .contains("503"));
        assert_ne!(
            presentation(&StreamError::Protocol("invalid snapshot")).summary,
            presentation(&StreamError::IncompatibleProtocol).summary
        );
    }

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
    fn terminal_causes_keep_distinct_safe_details_and_explicit_recovery() {
        let cases = [
            (
                StreamError::PairingRequired,
                RecoveryClass::Credentials,
                RecoveryAction::PairAgain,
            ),
            (
                StreamError::Expired,
                RecoveryClass::Credentials,
                RecoveryAction::PairAgain,
            ),
            (
                StreamError::WrongOrigin,
                RecoveryClass::WrongOrigin,
                RecoveryAction::OpenT3,
            ),
            (
                StreamError::MissingReadPermission,
                RecoveryClass::ReadPermission,
                RecoveryAction::OpenT3,
            ),
            (
                StreamError::IncompatibleProtocol,
                RecoveryClass::Incompatible,
                RecoveryAction::OpenT3,
            ),
            (
                StreamError::Protocol("invalid snapshot"),
                RecoveryClass::Incompatible,
                RecoveryAction::OpenT3,
            ),
            (
                StreamError::Rejected,
                RecoveryClass::Rejected,
                RecoveryAction::OpenT3,
            ),
            (
                StreamError::Http(403),
                RecoveryClass::OtherHttp,
                RecoveryAction::Reconnect,
            ),
        ];
        let mut texts = HashSet::new();
        for (cause, class, action) in cases {
            let shown = presentation(&cause);
            assert_eq!(shown.class, class);
            assert_eq!(shown.action, action);
            assert_eq!(shown.full_safe_text, cause.to_string());
            assert!(texts.insert(shown.full_safe_text));
        }
    }

    #[test]
    fn transient_http_retains_status_in_full_text() {
        for cause in [
            StreamError::Closed,
            StreamError::Unreachable,
            StreamError::Http(503),
        ] {
            let shown = presentation(&cause);
            assert_eq!(shown.class, RecoveryClass::Transient);
            assert_eq!(shown.action, RecoveryAction::Reconnect);
            assert_eq!(shown.full_safe_text, cause.to_string());
        }
        assert!(presentation(&StreamError::Http(503))
            .full_safe_text
            .contains("503"));
    }

    #[test]
    fn dismissal_is_session_scoped_and_survives_reconstructed_view_keys() {
        let key = || {
            ErrorDismissKey::new(
                "https://a.test",
                "thread-a",
                ErrorOrigin::Connection,
                "Error",
            )
        };
        let mut session = SessionDismissals::default();
        assert!(!session.is_dismissed(&key()));
        assert!(session.dismiss(key()));
        assert!(!session.dismiss(key()));
        let clean_other_thread = ErrorDismissKey::new(
            "https://a.test",
            "thread-b",
            ErrorOrigin::Connection,
            "Error",
        );
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
            ErrorOrigin::Connection,
            text,
        ));
        for key in [
            ErrorDismissKey::new("https://b.test", "thread", ErrorOrigin::Connection, text),
            ErrorDismissKey::new(
                "https://a.test",
                "other-thread",
                ErrorOrigin::Connection,
                text,
            ),
            ErrorDismissKey::new("https://a.test", "thread", ErrorOrigin::History, text),
            ErrorDismissKey::new("https://a.test", "thread", ErrorOrigin::Action, text),
            ErrorDismissKey::new("https://a.test", "thread", ErrorOrigin::Setup, text),
            ErrorDismissKey::new(
                "https://a.test",
                "thread",
                ErrorOrigin::Connection,
                "Same preview\nSecond line\nThird line\nOther detail",
            ),
            ErrorDismissKey::new(
                "https://a.test",
                "thread",
                ErrorOrigin::Connection,
                &format!("{text}\n"),
            ),
            ErrorDismissKey::new(
                "https://a.test",
                "thread",
                ErrorOrigin::Connection,
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
            ErrorOrigin::Connection,
            "error",
        ));
        assert!(!session.is_dismissed(&ErrorDismissKey::new(
            "origin",
            "thread\0suffix",
            ErrorOrigin::Connection,
            "error"
        )));
    }
}
