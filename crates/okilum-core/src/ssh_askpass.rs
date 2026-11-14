//! OpenSSH `SSH_ASKPASS` bridge for remote Markdown links (#1610).
//!
//! System OpenSSH keeps every authentication decision: ssh-agent and keys are
//! tried first, `known_hosts` stays authoritative and a changed host key is
//! still refused. Only when OpenSSH itself needs an answer (key passphrase,
//! password, keyboard-interactive/2FA code, or trust for an unknown host) does
//! it run `SSH_ASKPASS`. That program is the Okilum binary in helper mode: it
//! forwards the prompt over a private Unix socket to the running app, which
//! shows its own dialog, and prints the answer for OpenSSH. Answers are never
//! stored, logged or reused across connections.
//!
//! Without an installed bridge (and on Windows, for now) commands keep
//! `BatchMode=yes`, so a connection that needs an answer fails visibly.
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Socket of the running app's bridge; set only on Okilum's own ssh children.
pub const SOCKET_ENV: &str = "OKILUM_SSH_ASKPASS_SOCKET";
/// Per-connection id, so a cancelled connection is not asked again.
pub const SESSION_ENV: &str = "OKILUM_SSH_ASKPASS_SESSION";
const PROMPT_LIMIT: usize = 8 * 1024;
const ANSWER_LIMIT: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Passphrase, password or one-time code; shown masked.
    Secret,
    /// Yes/no question, e.g. trusting an unknown host key.
    Confirm,
}
impl Kind {
    /// OpenSSH marks agent confirmations with `SSH_ASKPASS_PROMPT=confirm`; the
    /// unknown-host question carries no hint, only its `(yes/no` text.
    pub fn classify(prompt: &str, hint: Option<&str>) -> Self {
        if hint == Some("confirm") || prompt.contains("(yes/no") {
            Self::Confirm
        } else {
            Self::Secret
        }
    }
}

// Time spent waiting for the person to answer must not count against network
// deadlines. Accumulated nanoseconds of finished prompts, plus the start of
// the prompts still open (count > 0).
static FINISHED_NANOS: AtomicU64 = AtomicU64::new(0);
static OPEN: std::sync::Mutex<(usize, Option<Instant>)> = std::sync::Mutex::new((0, None));

/// Total time any askpass dialog has been open in this process.
pub fn prompt_time() -> Duration {
    let finished = Duration::from_nanos(FINISHED_NANOS.load(Ordering::Acquire));
    let open = OPEN
        .lock()
        .map(|open| open.1.map_or(Duration::ZERO, |since| since.elapsed()))
        .unwrap_or_default();
    finished + open
}
struct PromptClock;
impl PromptClock {
    fn start() -> Self {
        if let Ok(mut open) = OPEN.lock() {
            if open.0 == 0 {
                open.1 = Some(Instant::now());
            }
            open.0 += 1;
        }
        Self
    }
}
impl Drop for PromptClock {
    fn drop(&mut self) {
        if let Ok(mut open) = OPEN.lock() {
            open.0 -= 1;
            if open.0 == 0 {
                if let Some(since) = open.1.take() {
                    FINISHED_NANOS.fetch_add(since.elapsed().as_nanos() as u64, Ordering::AcqRel);
                }
            }
        }
    }
}

/// A network deadline that pauses while an askpass dialog is open.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    at: Instant,
    prompts: Duration,
}
impl Deadline {
    pub fn at(at: Instant) -> Self {
        Self {
            at,
            prompts: prompt_time(),
        }
    }
    pub fn after(budget: Duration) -> Self {
        Self::at(Instant::now() + budget)
    }
    pub fn remaining(&self) -> Duration {
        let paused = prompt_time().saturating_sub(self.prompts);
        (self.at + paused).saturating_duration_since(Instant::now())
    }
    pub fn expired(&self) -> bool {
        self.remaining().is_zero()
    }
}

#[cfg(unix)]
pub use unix::{configure, helper_entry, install, Request};

#[cfg(not(unix))]
pub fn configure(_: &mut std::process::Command) -> bool {
    false
}
#[cfg(not(unix))]
pub fn helper_entry() -> Option<i32> {
    None
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        collections::HashSet,
        io::{self, Read, Write},
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
        path::{Path, PathBuf},
        process::Command,
        sync::{mpsc, Arc, Mutex, OnceLock},
    };

    /// One question from OpenSSH. Answer it exactly once; dropping it cancels.
    pub struct Request {
        pub prompt: String,
        pub kind: Kind,
        reply: Option<mpsc::Sender<Option<String>>>,
    }
    impl Request {
        pub fn answer(mut self, answer: Option<String>) {
            if let Some(reply) = self.reply.take() {
                let _ = reply.send(answer);
            }
        }
    }
    impl Drop for Request {
        fn drop(&mut self) {
            if let Some(reply) = self.reply.take() {
                let _ = reply.send(None);
            }
        }
    }

    struct Bridge {
        socket: PathBuf,
        helper: PathBuf,
        _dir: tempfile::TempDir,
    }
    static BRIDGE: OnceLock<Bridge> = OnceLock::new();

    /// Start the app-wide bridge once. `helper` is the executable OpenSSH runs
    /// as `SSH_ASKPASS` (the current Okilum binary); `handler` must hand the
    /// request to the UI without blocking. Later calls are no-ops.
    pub fn install(
        helper: PathBuf,
        handler: impl Fn(Request) + Send + Sync + 'static,
    ) -> io::Result<()> {
        if BRIDGE.get().is_some() {
            return Ok(());
        }
        let (dir, listener) = listen()?;
        let socket = dir.path().join("askpass.sock");
        if BRIDGE
            .set(Bridge {
                socket,
                helper,
                _dir: dir,
            })
            .is_err()
        {
            return Ok(()); // Another thread won; its listener serves.
        }
        serve(listener, Arc::new(handler));
        Ok(())
    }

    fn listen() -> io::Result<(tempfile::TempDir, UnixListener)> {
        // sun_path is ~104 bytes on macOS; keep the directory short.
        let base = if cfg!(target_os = "macos") {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = tempfile::Builder::new()
            .prefix("okilum-askpass-")
            .tempdir_in(base)?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let listener = UnixListener::bind(dir.path().join("askpass.sock"))?;
        Ok((dir, listener))
    }

    fn serve(listener: UnixListener, handler: Arc<dyn Fn(Request) + Send + Sync>) {
        // One dialog at a time; a cancelled connection is not asked again.
        let state = Arc::new(Mutex::new(HashSet::<String>::new()));
        std::thread::Builder::new()
            .name("okilum-askpass".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let handler = handler.clone();
                    let state = state.clone();
                    let _ = std::thread::Builder::new()
                        .name("okilum-askpass-request".into())
                        .spawn(move || {
                            let _ = answer(stream, &*handler, &state);
                        });
                }
            })
            .ok();
    }

    fn answer(
        mut stream: UnixStream,
        handler: &(dyn Fn(Request) + Send + Sync),
        cancelled: &Mutex<HashSet<String>>,
    ) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let session = String::from_utf8(read_field(&mut stream, 64)?)
            .map_err(|_| invalid("askpass session is not UTF-8"))?;
        let mut hint = [0];
        stream.read_exact(&mut hint)?;
        let prompt = String::from_utf8_lossy(&read_field(&mut stream, PROMPT_LIMIT)?).into_owned();
        stream.set_read_timeout(None)?;
        // Hold the lock across the dialog: prompts are answered in order.
        let mut cancelled = cancelled.lock().map_err(|_| invalid("askpass poisoned"))?;
        let reply = if cancelled.contains(&session) {
            None
        } else {
            let (sender, receiver) = mpsc::channel();
            let _clock = PromptClock::start();
            handler(Request {
                kind: Kind::classify(&prompt, (hint[0] == 1).then_some("confirm")),
                prompt,
                reply: Some(sender),
            });
            receiver.recv().ok().flatten()
        };
        if reply.is_none() {
            if cancelled.len() > 256 {
                cancelled.clear();
            }
            cancelled.insert(session);
        }
        drop(cancelled);
        let mut out = Vec::new();
        match reply {
            Some(answer) if answer.len() <= ANSWER_LIMIT && !answer.contains(['\n', '\0']) => {
                out.push(1);
                out.extend_from_slice(&(answer.len() as u32).to_be_bytes());
                out.extend_from_slice(answer.as_bytes());
            }
            _ => out.push(0),
        }
        stream.write_all(&out)
    }

    fn read_field(stream: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
        let mut length = [0; 4];
        stream.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > limit {
            return Err(invalid("askpass field exceeds its limit"));
        }
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn invalid(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }

    /// Let OpenSSH ask through the bridge. Returns false (command unchanged)
    /// when no bridge is installed; the caller then keeps `BatchMode=yes`.
    pub fn configure(command: &mut Command) -> bool {
        let Some(bridge) = BRIDGE.get() else {
            return false;
        };
        command
            .env("SSH_ASKPASS", &bridge.helper)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(SOCKET_ENV, &bridge.socket)
            .env(SESSION_ENV, uuid::Uuid::new_v4().simple().to_string());
        true
    }

    /// Run before any app initialization. When OpenSSH started this process as
    /// `SSH_ASKPASS`, ask the app and print the answer; returns the exit code.
    pub fn helper_entry() -> Option<i32> {
        let socket = std::env::var_os(SOCKET_ENV)?;
        let prompt = std::env::args().nth(1).unwrap_or_default();
        let hint = std::env::var("SSH_ASKPASS_PROMPT").ok();
        if hint.as_deref() == Some("none") {
            // A notice (e.g. "touch your security key"): nothing to answer.
            return Some(0);
        }
        let session = std::env::var(SESSION_ENV).unwrap_or_default();
        let answer = ask(
            Path::new(&socket),
            &session,
            &prompt,
            hint.as_deref() == Some("confirm"),
        );
        Some(match answer {
            Ok(Some(answer)) => {
                let mut stdout = io::stdout().lock();
                if writeln!(stdout, "{answer}")
                    .and_then(|_| stdout.flush())
                    .is_ok()
                {
                    0
                } else {
                    1
                }
            }
            _ => 1,
        })
    }

    pub(super) fn ask(
        socket: &Path,
        session: &str,
        prompt: &str,
        confirm: bool,
    ) -> io::Result<Option<String>> {
        let mut stream = UnixStream::connect(socket)?;
        let mut out = Vec::new();
        let session = &session.as_bytes()[..session.len().min(64)];
        out.extend_from_slice(&(session.len() as u32).to_be_bytes());
        out.extend_from_slice(session);
        out.push(confirm as u8);
        let prompt = &prompt.as_bytes()[..prompt.len().min(PROMPT_LIMIT)];
        out.extend_from_slice(&(prompt.len() as u32).to_be_bytes());
        out.extend_from_slice(prompt);
        stream.write_all(&out)?;
        let mut status = [0];
        stream.read_exact(&mut status)?;
        if status[0] != 1 {
            return Ok(None);
        }
        let answer = read_field(&mut stream, ANSWER_LIMIT)?;
        String::from_utf8(answer)
            .map(Some)
            .map_err(|_| invalid("askpass answer is not UTF-8"))
    }

    #[cfg(test)]
    pub(super) fn serve_for_test(
        handler: impl Fn(Request) + Send + Sync + 'static,
    ) -> (tempfile::TempDir, PathBuf) {
        let (dir, listener) = listen().unwrap();
        let socket = dir.path().join("askpass.sock");
        serve(listener, Arc::new(handler));
        (dir, socket)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn classify_secret_and_host_key_prompts() {
        assert_eq!(
            Kind::classify("Enter passphrase for key '/home/u/.ssh/id_ed25519': ", None),
            Kind::Secret
        );
        assert_eq!(Kind::classify("u@host's password: ", None), Kind::Secret);
        assert_eq!(Kind::classify("Verification code: ", None), Kind::Secret);
        assert_eq!(
            Kind::classify(
                "The authenticity of host 'h (10.0.0.1)' can't be established.\nED25519 key fingerprint is SHA256:x.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ",
                None
            ),
            Kind::Confirm
        );
        assert_eq!(
            Kind::classify("Allow use of key?", Some("confirm")),
            Kind::Confirm
        );
    }

    #[test]
    fn answers_reach_the_helper_and_cancel_is_remembered_per_session() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let (_dir, socket) = unix::serve_for_test(move |request| {
            counter.fetch_add(1, Ordering::SeqCst);
            let answer = (request.prompt == "password: ").then(|| "s3cret".to_owned());
            assert_eq!(request.kind, Kind::Secret);
            request.answer(answer);
        });
        assert_eq!(
            unix::ask(&socket, "a", "password: ", false)
                .unwrap()
                .as_deref(),
            Some("s3cret")
        );
        assert_eq!(unix::ask(&socket, "b", "code: ", false).unwrap(), None);
        // OpenSSH retries after an empty answer; the cancelled session is not
        // shown another dialog, while other sessions still are.
        assert_eq!(unix::ask(&socket, "b", "password: ", false).unwrap(), None);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(
            unix::ask(&socket, "c", "password: ", false)
                .unwrap()
                .as_deref(),
            Some("s3cret")
        );
        assert_eq!(asked.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn dropped_request_cancels_and_multiline_answers_are_refused() {
        let (_dir, socket) = unix::serve_for_test(|request| {
            if request.prompt == "drop" {
                drop(request);
            } else {
                request.answer(Some("a\nProxyCommand x".into()));
            }
        });
        assert_eq!(unix::ask(&socket, "d", "drop", false).unwrap(), None);
        assert_eq!(unix::ask(&socket, "e", "inject", false).unwrap(), None);
    }

    #[test]
    fn socket_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _socket) = unix::serve_for_test(|request| request.answer(None));
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn deadline_pauses_while_a_prompt_is_open() {
        let deadline = Deadline::after(Duration::from_millis(50));
        let clock = PromptClock::start();
        std::thread::sleep(Duration::from_millis(80));
        assert!(!deadline.expired(), "time in a dialog must not count");
        drop(clock);
        std::thread::sleep(Duration::from_millis(80));
        assert!(deadline.expired());
    }
}
