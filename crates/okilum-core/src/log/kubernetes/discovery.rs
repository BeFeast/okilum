//! Explicit, read-only discovery for the native investigation workspace (#3009).
//! No command strings, kubeconfig writes, implicit log sessions or automatic retries.
use super::{Event, Session};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

mod model;
pub use model::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Connection {
    /// None preserves kubectl's inherited KUBECONFIG/default merging rules.
    pub kubeconfig: Option<PathBuf>,
    pub context: String,
    pub cluster: String,
    pub server: String,
    pub user: Option<String>,
    pub default_namespace: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Query {
    /// Reads only the selected context's configuration, without --raw or secrets.
    Resolve {
        context: String,
        kubeconfig: Option<PathBuf>,
    },
    List {
        connection: Connection,
        namespace: String,
        kind: Kind,
    },
    Get {
        connection: Connection,
        namespace: String,
        kind: Kind,
        name: String,
    },
    /// Core Events selected by involvedObject.uid, never by a reused name alone.
    RelatedEvents {
        connection: Connection,
        namespace: String,
        uid: String,
    },
}

impl Query {
    pub fn args(&self) -> io::Result<Vec<OsString>> {
        let (context, kubeconfig) = match self {
            Self::Resolve {
                context,
                kubeconfig,
            } => (context, kubeconfig),
            Self::List { connection, .. }
            | Self::Get { connection, .. }
            | Self::RelatedEvents { connection, .. } => {
                (&connection.context, &connection.kubeconfig)
            }
        };
        token(context)?;
        let mut args = vec![OsString::from(format!("--context={context}"))];
        if let Some(path) = kubeconfig {
            if path.as_os_str().is_empty() {
                return Err(invalid("Empty kubeconfig path"));
            }
            let mut arg = OsString::from("--kubeconfig=");
            arg.push(path);
            args.push(arg);
        }
        if matches!(self, Self::Resolve { .. }) {
            args.extend(["config", "view", "--minify", "--output=json"].map(OsString::from));
            return Ok(args);
        }
        let (connection, namespace, kind, name) = match self {
            Self::List {
                connection,
                namespace,
                kind,
            } => (connection, namespace, *kind, None),
            Self::Get {
                connection,
                namespace,
                kind,
                name,
            } => {
                dns(name)?;
                (connection, namespace, *kind, Some(name))
            }
            Self::RelatedEvents {
                connection,
                namespace,
                uid,
            } => {
                if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    return Err(invalid("Invalid related object UID"));
                }
                (connection, namespace, Kind::Event, None)
            }
            Self::Resolve { .. } => unreachable!(),
        };
        token(&connection.cluster)?;
        token(&connection.server)?;
        if kind != Kind::Namespace {
            dns(namespace)?;
        }
        // Bind both context and cluster name. We never accept an arbitrary server override.
        args.push(format!("--cluster={}", connection.cluster).into());
        if kind != Kind::Namespace {
            args.push(format!("--namespace={namespace}").into());
        }
        args.extend(["get".into(), kind.resource().into()]);
        if let Some(name) = name {
            args.push(name.into());
        }
        if let Self::RelatedEvents { uid, .. } = self {
            args.push(format!("--field-selector=involvedObject.uid={uid}").into());
        }
        args.extend(
            ["--output=json", "--request-timeout=15s", "--chunk-size=200"].map(OsString::from),
        );
        Ok(args)
    }

    fn kind(&self) -> Option<Kind> {
        match self {
            Self::Resolve { .. } => None,
            Self::List { kind, .. } | Self::Get { kind, .. } => Some(*kind),
            Self::RelatedEvents { .. } => Some(Kind::Event),
        }
    }

    fn namespace(&self) -> Option<&str> {
        match self {
            Self::Resolve { .. } => None,
            Self::List { namespace, .. }
            | Self::Get { namespace, .. }
            | Self::RelatedEvents { namespace, .. } => Some(namespace),
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn token(value: &str) -> io::Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        Err(invalid(
            "Explicit Kubernetes connection identity is required",
        ))
    } else {
        Ok(())
    }
}
fn dns(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 253
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
    {
        Err(invalid("Explicit Kubernetes DNS name is required"))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureKind {
    InvalidRequest,
    Forbidden,
    Authentication,
    Unavailable,
    InvalidJson,
    Limit,
    Cancelled,
    Timeout,
    Io,
}
#[derive(Clone, Debug)]
pub struct Failure {
    pub kind: FailureKind,
    /// Bounded stderr, never interpreted as log rows. May contain sensitive local diagnostics.
    pub diagnostic: String,
    pub diagnostic_truncated: bool,
}
impl Failure {
    fn new(kind: FailureKind, diagnostic: impl Into<String>) -> Self {
        Self {
            kind,
            diagnostic: diagnostic.into(),
            diagnostic_truncated: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartialReason {
    ItemLimit,
    ContinueToken,
}
#[derive(Clone, Debug)]
pub enum Data {
    Connection(Connection),
    Resources {
        items: Vec<Resource>,
        resource_version: Option<String>,
        partial: Vec<PartialReason>,
    },
}
#[derive(Clone, Debug)]
pub struct Observation {
    /// Includes context/cluster/kubeconfig scope and the exact resource/query.
    pub source: Query,
    pub started_at: SystemTime,
    pub observed_at: SystemTime,
    pub data: Data,
    pub diagnostic: String,
    pub diagnostic_truncated: bool,
}
#[derive(Clone, Debug)]
pub struct Attempt {
    /// Monotonic read-start sequence, independent of wall-clock adjustments.
    pub sequence: u64,
    pub source: Query,
    pub finished_at: SystemTime,
    pub result: Result<Observation, Failure>,
}

/// One source's retained evidence; a failed refresh never becomes an empty success.
#[derive(Clone, Debug)]
pub struct SourceState {
    pub source: Query,
    pub retained: Option<Observation>,
    pub last_failure: Option<(SystemTime, Failure)>,
    last_sequence: Option<u64>,
}
impl SourceState {
    pub fn new(source: Query) -> Self {
        Self {
            source,
            retained: None,
            last_failure: None,
            last_sequence: None,
        }
    }
    /// Return false for another source or an out-of-order completion.
    pub fn apply(&mut self, attempt: Attempt) -> bool {
        if attempt.source != self.source {
            return false;
        }
        if self
            .last_sequence
            .is_some_and(|sequence| sequence >= attempt.sequence)
        {
            return false;
        }
        if let Ok(observation) = &attempt.result {
            if observation.source != self.source {
                return false;
            }
        }
        self.last_sequence = Some(attempt.sequence);
        match attempt.result {
            Ok(observation) => {
                self.retained = Some(observation);
                self.last_failure = None;
            }
            Err(failure) => self.last_failure = Some((attempt.finished_at, failure)),
        }
        true
    }
    pub fn is_stale(&self, now: SystemTime, max_age: Duration) -> bool {
        self.retained.as_ref().is_some_and(|o| {
            self.last_failure.is_some()
                || now
                    .duration_since(o.observed_at)
                    .is_ok_and(|age| age > max_age)
        })
    }
}

/// Fixed ceilings, shared by all discovery queries. No unbounded raw response escapes.
pub const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
pub const MAX_ITEMS: usize = 500;
const DEADLINE: Duration = Duration::from_secs(20);
static READ_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Trusted local executable selection for GUI launches; PATH also reaches exec plugins.
/// Defaults preserve the installed kubectl, KUBECONFIG and inherited environment.
#[derive(Clone, Debug)]
pub struct Kubectl {
    pub executable: PathBuf,
    pub search_path: Option<OsString>,
}
impl Default for Kubectl {
    fn default() -> Self {
        Self {
            executable: "kubectl".into(),
            search_path: None,
        }
    }
}
impl Kubectl {
    /// Call off the render thread. Cancellation is checked before spawn and every 20ms.
    /// Owns/kills/reaps the direct child via Session, including deadline/parse/limit exits.
    /// External credential-helper descendants are NOT owned; see the contract document.
    pub fn read(&self, source: Query, cancel: &AtomicBool) -> Attempt {
        let sequence = READ_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let started_at = SystemTime::now();
        let result = (|| {
            let args = source
                .args()
                .map_err(|e| Failure::new(FailureKind::InvalidRequest, e.to_string()))?;
            if cancel.load(Ordering::Acquire) {
                return Err(Failure::new(
                    FailureKind::Cancelled,
                    "Cancelled before kubectl spawn",
                ));
            }
            let mut command = Command::new(&self.executable);
            command.args(args);
            if let Some(path) = &self.search_path {
                command.env("PATH", path);
            }
            let session = Session::spawn(command)
                .map_err(|e| Failure::new(FailureKind::Unavailable, e.to_string()))?;
            collect(session, &source, started_at, cancel, DEADLINE)
        })();
        Attempt {
            sequence,
            source,
            finished_at: SystemTime::now(),
            result,
        }
    }
}

fn collect(
    mut session: Session,
    source: &Query,
    started_at: SystemTime,
    cancel: &AtomicBool,
    deadline: Duration,
) -> Result<Observation, Failure> {
    let start = Instant::now();
    let (mut bytes, mut diagnostic) = (Vec::new(), Vec::new());
    let mut diagnostic_truncated = false;
    let result = loop {
        if cancel.load(Ordering::Acquire) {
            break Err(Failure::new(FailureKind::Cancelled, "Discovery cancelled"));
        }
        if start.elapsed() >= deadline {
            break Err(Failure::new(
                FailureKind::Timeout,
                "Discovery deadline exceeded (auth may require interactive renewal)",
            ));
        }
        match session.next(Duration::from_millis(20)) {
            Ok(Some(Event::Data(chunk))) => {
                if chunk.len() > MAX_JSON_BYTES - bytes.len() {
                    break Err(Failure::new(
                        FailureKind::Limit,
                        "Discovery JSON byte limit exceeded",
                    ));
                }
                bytes.extend(chunk);
            }
            Ok(Some(Event::Diagnostic(chunk))) => {
                let available = MAX_DIAGNOSTIC_BYTES - diagnostic.len();
                diagnostic_truncated |= chunk.len() > available;
                diagnostic.extend(&chunk[..chunk.len().min(available)]);
            }
            Ok(Some(Event::Exited(status))) => {
                if status.success() {
                    break parse(source, started_at, &bytes).map(|mut observation| {
                        observation.diagnostic = String::from_utf8_lossy(&diagnostic).into_owned();
                        observation.diagnostic_truncated = diagnostic_truncated;
                        observation
                    });
                }
                let text = String::from_utf8_lossy(&diagnostic).into_owned();
                let mut failure = Failure::new(
                    classify(&bytes, &text),
                    if text.is_empty() {
                        format!("kubectl failed: {status}")
                    } else {
                        text
                    },
                );
                failure.diagnostic_truncated = diagnostic_truncated;
                break Err(failure);
            }
            Ok(Some(Event::ReadError(error))) | Err(error) => {
                break Err(Failure::new(FailureKind::Io, error.to_string()))
            }
            Ok(None) => (),
        }
    };
    // Don't suppress cleanup errors or present retained evidence as freshly complete.
    session
        .cancel()
        .map_err(|e| Failure::new(FailureKind::Io, format!("Could not reap kubectl: {e}")))?;
    result
}

fn classify(bytes: &[u8], diagnostic: &str) -> FailureKind {
    // kubectl commonly emits plain diagnostics; structured Status takes precedence.
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        match value.get("reason").and_then(|v| v.as_str()) {
            Some("Forbidden") => return FailureKind::Forbidden,
            Some("Unauthorized") => return FailureKind::Authentication,
            _ => (),
        }
    }
    let lower = diagnostic.to_ascii_lowercase();
    if [
        "unauthorized",
        "must be logged in",
        "credentials",
        "token has expired",
        "token is expired",
        "login",
        "authentication",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        FailureKind::Authentication
    } else if lower.contains("forbidden") {
        FailureKind::Forbidden
    } else {
        FailureKind::Unavailable
    }
}

#[cfg(test)]
mod tests;
