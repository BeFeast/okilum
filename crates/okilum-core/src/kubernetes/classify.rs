//! Turns one bounded invocation into data or a typed failure. Successful empty
//! data is never a failure; failures never carry partial stdout as data.
use super::transport::{End, Raw};
use std::io;
use std::time::Duration;

/// Why a read produced no current observation. Kinds stay separate so the UI
/// can say "forbidden" without implying the cluster is down, and "sign in
/// again" without implying a permission problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The API server refused this verb/resource for the current identity.
    Forbidden {
        detail: String,
    },
    /// Credentials missing, rejected or expired (HTTP 401, failing helper).
    AuthExpired {
        detail: String,
    },
    /// A kubeconfig exec credential plugin needs a terminal, browser or MFA
    /// step that a GUI child process cannot complete.
    InteractiveLogin {
        detail: String,
    },
    /// The object or API group/version does not exist on this cluster.
    NotFound {
        detail: String,
    },
    /// No usable connection: kubectl missing, network, TLS, server overloaded.
    Unavailable {
        detail: String,
    },
    /// The kubeconfig cannot satisfy the request (unknown context, none set).
    Configuration {
        detail: String,
    },
    /// A request this adapter refuses before running kubectl.
    InvalidRequest {
        detail: String,
    },
    /// Exit 0 but not the JSON this adapter expects.
    Malformed {
        detail: String,
    },
    /// Stdout exceeded the byte bound; the process tree was ended.
    Oversized {
        limit_bytes: usize,
    },
    TimedOut {
        after: Duration,
    },
    Cancelled,
    /// Any other kubectl failure, with its exit code and bounded stderr.
    Failed {
        code: Option<i32>,
        detail: String,
    },
}

impl Failure {
    pub fn summary(&self) -> String {
        match self {
            Self::Forbidden { detail } => format!("Forbidden: {detail}"),
            Self::AuthExpired { detail } => format!("Credentials expired or rejected: {detail}"),
            Self::InteractiveLogin { detail } => {
                format!("Interactive sign-in (MFA/browser) required outside Okilum: {detail}")
            }
            Self::NotFound { detail } => format!("Not found: {detail}"),
            Self::Unavailable { detail } => format!("Cluster unavailable: {detail}"),
            Self::Configuration { detail } => format!("Kubeconfig: {detail}"),
            Self::InvalidRequest { detail } => detail.clone(),
            Self::Malformed { detail } => format!("Unexpected kubectl output: {detail}"),
            Self::Oversized { limit_bytes } => {
                format!("Response exceeded {limit_bytes} bytes; nothing from it was kept")
            }
            Self::TimedOut { after } => format!("No answer within {} s", after.as_secs()),
            Self::Cancelled => "Cancelled".into(),
            Self::Failed { code, detail } => match code {
                Some(code) => format!("kubectl exited with {code}: {detail}"),
                None => format!("kubectl was terminated: {detail}"),
            },
        }
    }

    pub(crate) fn invalid(detail: impl Into<String>) -> Self {
        Self::InvalidRequest {
            detail: detail.into(),
        }
    }
}

pub(crate) fn spawn_failure(error: &io::Error) -> Failure {
    let detail = if error.kind() == io::ErrorKind::NotFound {
        "kubectl was not found on the application's PATH".to_string()
    } else {
        format!("could not start kubectl: {error}")
    };
    Failure::Unavailable { detail }
}

/// Stdout of a successful run, or the failure it represents.
pub(crate) fn outcome(raw: Raw, limits: &super::transport::Limits) -> Result<Vec<u8>, Failure> {
    match raw.end {
        End::Cancelled => Err(Failure::Cancelled),
        End::Deadline => Err(Failure::TimedOut {
            after: limits.deadline,
        }),
        End::StdoutLimit => Err(Failure::Oversized {
            limit_bytes: limits.max_stdout,
        }),
        End::Exited { code: Some(0) } => Ok(raw.stdout),
        End::Exited { code } => Err(from_stderr(code, &raw.stderr)),
    }
}

/// Stderr as text with control characters (other than newlines) removed.
fn sanitized(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .trim()
        .chars()
        .map(|c| if c == '\n' || !c.is_control() { c } else { ' ' })
        .collect()
}

const DETAIL_HEAD: usize = 512;
const DETAIL_TAIL: usize = 1536;

/// Display text, bounded to its start and its end: kubectl's error is last.
fn bounded(text: String) -> String {
    if text.len() <= DETAIL_HEAD + DETAIL_TAIL {
        return text;
    }
    let mut head = DETAIL_HEAD;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - DETAIL_TAIL;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}\n…\n{}", &text[..head], &text[tail..])
}

/// kubectl's explicit API status reason, as in `Error from server (Forbidden):`
/// or `... to the server (Unauthorized)`. The last one wins: kubectl prints its
/// error after any warnings. Quoted resource names are never read as a reason.
fn server_reason(text: &str) -> Option<&str> {
    let mut found: Option<(usize, &str)> = None;
    for marker in ["from server (", "to the server ("] {
        for (at, _) in text.match_indices(marker) {
            let after = &text[at + marker.len()..];
            let Some(end) = after.find(')') else { continue };
            let reason = &after[..end];
            if !reason.is_empty()
                && reason.bytes().all(|b| b.is_ascii_alphabetic())
                && found.is_none_or(|(seen, _)| at > seen)
            {
                found = Some((at, reason));
            }
        }
    }
    found.map(|(_, reason)| reason)
}

/// Text with every double-quoted span removed, so object or context names such
/// as `"unauthorized"` or `"timeout-svc"` cannot look like an error kind.
fn unquoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quoted = false;
    for c in text.chars() {
        if c == '"' {
            quoted = !quoted;
            out.push(' ');
        } else if !quoted {
            out.push(c);
        }
    }
    out
}

pub(crate) fn from_stderr(code: Option<i32>, stderr: &[u8]) -> Failure {
    // Classify on all retained stderr; only the displayed detail is shortened.
    let text = sanitized(stderr);
    let reason = server_reason(&text).map(str::to_string);
    let lower = unquoted(&text).to_ascii_lowercase();
    let detail = bounded(text);
    if let Some(reason) = reason.as_deref() {
        return match reason {
            "Forbidden" => Failure::Forbidden { detail },
            "Unauthorized" => Failure::AuthExpired { detail },
            "NotFound" => Failure::NotFound { detail },
            "ServiceUnavailable" | "InternalError" | "Timeout" | "ServerTimeout"
            | "TooManyRequests" => Failure::Unavailable { detail },
            _ => Failure::Failed { code, detail },
        };
    }
    // Client-side failures carry no server reason: match kubectl's own
    // wording, outside quoted names, most specific first.
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    let credentials = has(&["getting credentials", "exec plugin", "credential plugin"]);
    if has(&[
        "context was not found",
        "current-context is not set",
        "invalid configuration",
        "no configuration has been provided",
    ]) {
        Failure::Configuration { detail }
    } else if credentials
        && has(&[
            "interactive",
            "not a terminal",
            "mfa",
            "multi-factor",
            "device code",
            "devicecode",
            "browser",
            "one-time",
            "otp",
        ])
    {
        Failure::InteractiveLogin { detail }
    } else if credentials
        || has(&[
            "must be logged in",
            "provide credentials",
            "token has expired",
            "token is expired",
            "expired token",
            "refresh token",
            "invalid_grant",
        ])
    {
        Failure::AuthExpired { detail }
    } else if has(&[
        "unable to connect",
        "connection refused",
        "no such host",
        "i/o timeout",
        "dial tcp",
        "tls handshake",
        "x509",
        "deadline exceeded",
        "unexpected eof",
        "connection reset",
        "network is unreachable",
    ]) {
        Failure::Unavailable { detail }
    } else {
        Failure::Failed { code, detail }
    }
}
