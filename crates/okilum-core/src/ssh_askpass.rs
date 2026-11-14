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
//! Every interactive ssh process is a registered [`Session`]. Its owner (the
//! transport or save worker) drops it when the process ends or is cancelled:
//! a pending dialog for it is then closed and answered with nothing, and any
//! later prompt carrying its id is refused. Time spent in a dialog pauses only
//! the deadlines that share that session's [`Clock`].
//!
//! Without an installed bridge (and on Windows, for now) commands keep
//! `BatchMode=yes`, so a connection that needs an answer fails visibly.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Socket of the running app's bridge; set only on Okilum's own ssh children.
pub const SOCKET_ENV: &str = "OKILUM_SSH_ASKPASS_SOCKET";
/// Per-process session id; prompts are accepted only for live sessions.
pub const SESSION_ENV: &str = "OKILUM_SSH_ASKPASS_SESSION";
#[cfg(unix)]
const PROMPT_LIMIT: usize = 8 * 1024;
#[cfg(unix)]
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

/// Time the person spent answering dialogs for the sessions of one operation.
/// Only deadlines holding this clock pause while such a dialog is open.
#[derive(Debug, Default)]
pub struct Clock {
    finished_nanos: AtomicU64,
    open: Mutex<(usize, Option<Instant>)>,
}
impl Clock {
    pub fn elapsed(&self) -> Duration {
        let finished = Duration::from_nanos(self.finished_nanos.load(Ordering::Acquire));
        let open = self
            .open
            .lock()
            .map(|open| open.1.map_or(Duration::ZERO, |since| since.elapsed()))
            .unwrap_or_default();
        finished + open
    }
    fn start(self: &Arc<Self>) -> Prompting {
        if let Ok(mut open) = self.open.lock() {
            if open.0 == 0 {
                open.1 = Some(Instant::now());
            }
            open.0 += 1;
        }
        Prompting(self.clone())
    }
}
struct Prompting(Arc<Clock>);
impl Drop for Prompting {
    fn drop(&mut self) {
        if let Ok(mut open) = self.0.open.lock() {
            open.0 -= 1;
            if open.0 == 0 {
                if let Some(since) = open.1.take() {
                    self.0
                        .finished_nanos
                        .fetch_add(since.elapsed().as_nanos() as u64, Ordering::AcqRel);
                }
            }
        }
    }
}

/// A network deadline. With a clock it pauses while a dialog of that clock's
/// sessions is open; without one (non-interactive targets) it never pauses.
#[derive(Clone, Debug)]
pub struct Deadline {
    at: Instant,
    clock: Option<Arc<Clock>>,
    base: Duration,
}
impl Deadline {
    pub fn at(at: Instant, clock: Option<Arc<Clock>>) -> Self {
        let base = clock
            .as_ref()
            .map_or(Duration::ZERO, |clock| clock.elapsed());
        Self { at, clock, base }
    }
    pub fn after(budget: Duration, clock: Option<Arc<Clock>>) -> Self {
        Self::at(Instant::now() + budget, clock)
    }
    pub fn remaining(&self) -> Duration {
        let paused = self.clock.as_ref().map_or(Duration::ZERO, |clock| {
            clock.elapsed().saturating_sub(self.base)
        });
        (self.at + paused).saturating_duration_since(Instant::now())
    }
    pub fn expired(&self) -> bool {
        self.remaining().is_zero()
    }
}

struct Entry {
    clock: Arc<Clock>,
    /// Set when the owner drops the session (process ended or cancelled).
    ended: Arc<AtomicBool>,
    /// Set when the person cancels; OpenSSH retries are not asked again.
    declined: AtomicBool,
}
static SESSIONS: Mutex<Vec<(String, Arc<Entry>)>> = Mutex::new(Vec::new());
fn entry(id: &str) -> Option<Arc<Entry>> {
    SESSIONS
        .lock()
        .ok()?
        .iter()
        .find(|(key, _)| key == id)
        .map(|(_, entry)| entry.clone())
}

/// Registration of one interactive ssh process. Keep it alive exactly as long
/// as that process; dropping it closes its dialog and refuses its prompts.
#[derive(Debug)]
pub struct Session {
    id: String,
    ended: Arc<AtomicBool>,
}
impl Session {
    fn register(clock: Arc<Clock>) -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let ended = Arc::new(AtomicBool::new(false));
        if let Ok(mut sessions) = SESSIONS.lock() {
            sessions.push((
                id.clone(),
                Arc::new(Entry {
                    clock,
                    ended: ended.clone(),
                    declined: AtomicBool::new(false),
                }),
            ));
        }
        Self { id, ended }
    }
    pub fn id(&self) -> &str {
        &self.id
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.ended.store(true, Ordering::Release);
        if let Ok(mut sessions) = SESSIONS.lock() {
            sessions.retain(|(id, _)| id != &self.id);
        }
    }
}

#[cfg(unix)]
pub use unix::{configure, helper_entry, install, Request};

#[cfg(not(unix))]
pub fn configure(_: &mut std::process::Command, _: &Arc<Clock>) -> Option<Session> {
    None
}
#[cfg(not(unix))]
pub fn helper_entry() -> Option<i32> {
    None
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        io::{self, Read, Write},
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
        path::{Path, PathBuf},
        process::Command,
        sync::{mpsc, OnceLock},
    };

    /// One question from OpenSSH. Answer it exactly once; dropping it cancels.
    /// The UI must close its dialog once [`Request::is_cancelled`] turns true.
    pub struct Request {
        pub prompt: String,
        pub kind: Kind,
        ended: Arc<AtomicBool>,
        reply: Option<mpsc::Sender<Option<String>>>,
    }
    impl Request {
        pub fn answer(mut self, answer: Option<String>) {
            if let Some(reply) = self.reply.take() {
                let _ = reply.send(answer);
            }
        }
        /// The ssh process this question belongs to was stopped or ended.
        pub fn is_cancelled(&self) -> bool {
            self.ended.load(Ordering::Acquire)
        }
        /// A request not backed by OpenSSH, for UI tests. Storing `true` in
        /// the returned flag behaves like the owner dropping its session.
        #[doc(hidden)]
        pub fn detached(
            prompt: &str,
            kind: Kind,
        ) -> (Self, mpsc::Receiver<Option<String>>, Arc<AtomicBool>) {
            let (sender, receiver) = mpsc::channel();
            let ended = Arc::new(AtomicBool::new(false));
            (
                Self {
                    prompt: prompt.into(),
                    kind,
                    ended: ended.clone(),
                    reply: Some(sender),
                },
                receiver,
                ended,
            )
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
        // One dialog at a time, in arrival order.
        let turn = Arc::new(Mutex::new(()));
        std::thread::Builder::new()
            .name("okilum-askpass".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let handler = handler.clone();
                    let turn = turn.clone();
                    let _ = std::thread::Builder::new()
                        .name("okilum-askpass-request".into())
                        .spawn(move || {
                            let _ = answer(stream, &*handler, &turn);
                        });
                }
            })
            .ok();
    }

    fn answer(
        mut stream: UnixStream,
        handler: &(dyn Fn(Request) + Send + Sync),
        turn: &Mutex<()>,
    ) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let session = String::from_utf8(read_field(&mut stream, 64)?)
            .map_err(|_| invalid("askpass session is not UTF-8"))?;
        let mut hint = [0];
        stream.read_exact(&mut hint)?;
        let prompt = String::from_utf8_lossy(&read_field(&mut stream, PROMPT_LIMIT)?).into_owned();
        let reply = ask_person(&session, prompt, hint[0] == 1, handler, turn);
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

    fn ask_person(
        session: &str,
        prompt: String,
        confirm: bool,
        handler: &(dyn Fn(Request) + Send + Sync),
        turn: &Mutex<()>,
    ) -> Option<String> {
        // Unknown (ended, forged or stale) sessions are never shown.
        let entry = entry(session)?;
        let _turn = turn.lock().ok()?;
        if entry.ended.load(Ordering::Acquire) || entry.declined.load(Ordering::Acquire) {
            return None;
        }
        let (sender, receiver) = mpsc::channel();
        let _prompting = entry.clock.start();
        handler(Request {
            kind: Kind::classify(&prompt, confirm.then_some("confirm")),
            prompt,
            ended: entry.ended.clone(),
            reply: Some(sender),
        });
        // A stopped session releases this worker (and the next prompt) at once.
        let reply = loop {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(reply) => break reply,
                Err(mpsc::RecvTimeoutError::Timeout) if !entry.ended.load(Ordering::Acquire) => {}
                Err(_) => break None,
            }
        };
        if reply.is_none() {
            entry.declined.store(true, Ordering::Release);
        }
        reply
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

    /// Let OpenSSH ask through the bridge for this one process. Returns `None`
    /// (command unchanged) when no bridge is installed; the caller then keeps
    /// `BatchMode=yes`. Keep the returned session alive with the process.
    pub fn configure(command: &mut Command, clock: &Arc<Clock>) -> Option<Session> {
        let bridge = BRIDGE.get()?;
        let session = Session::register(clock.clone());
        command
            .env("SSH_ASKPASS", &bridge.helper)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(SOCKET_ENV, &bridge.socket)
            .env(SESSION_ENV, session.id());
        Some(session)
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
pub(crate) fn session_for_test(clock: Arc<Clock>) -> Session {
    Session::register(clock)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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
    fn answers_reach_the_helper_and_decline_is_remembered_per_session() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let (_dir, socket) = unix::serve_for_test(move |request| {
            counter.fetch_add(1, Ordering::SeqCst);
            let answer = (request.prompt == "password: ").then(|| "s3cret".to_owned());
            assert_eq!(request.kind, Kind::Secret);
            request.answer(answer);
        });
        let clock = Arc::new(Clock::default());
        let (a, b, c) = (
            Session::register(clock.clone()),
            Session::register(clock.clone()),
            Session::register(clock.clone()),
        );
        assert_eq!(
            unix::ask(&socket, a.id(), "password: ", false)
                .unwrap()
                .as_deref(),
            Some("s3cret")
        );
        assert_eq!(unix::ask(&socket, b.id(), "code: ", false).unwrap(), None);
        // OpenSSH retries after an empty answer; the declined session is not
        // shown another dialog, while other sessions still are.
        assert_eq!(
            unix::ask(&socket, b.id(), "password: ", false).unwrap(),
            None
        );
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(
            unix::ask(&socket, c.id(), "password: ", false)
                .unwrap()
                .as_deref(),
            Some("s3cret")
        );
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        // Unknown and ended sessions never reach the person.
        assert_eq!(
            unix::ask(&socket, "forged", "password: ", false).unwrap(),
            None
        );
        let ended = a.id().to_owned();
        drop(a);
        assert_eq!(
            unix::ask(&socket, &ended, "password: ", false).unwrap(),
            None
        );
        assert_eq!(asked.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn dropping_the_session_closes_its_open_dialog_and_frees_the_next_prompt() {
        // The UI keeps the first request open (the person has not answered).
        let held = Arc::new(Mutex::new(Vec::new()));
        let keep = held.clone();
        let (_dir, socket) = unix::serve_for_test(move |request| {
            if request.prompt == "hold" {
                keep.lock().unwrap().push(request);
            } else {
                request.answer(Some("next".into()));
            }
        });
        let clock = Arc::new(Clock::default());
        let stopped = Session::register(clock.clone());
        let other = Session::register(Arc::new(Clock::default()));
        let id = stopped.id().to_owned();
        let pending = {
            let socket = socket.clone();
            std::thread::spawn(move || unix::ask(&socket, &id, "hold", false))
        };
        let started = Instant::now();
        while held.lock().unwrap().is_empty() {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!held.lock().unwrap()[0].is_cancelled());
        // Stop: the owner drops the session, as the transport does on cancel.
        drop(stopped);
        assert_eq!(pending.join().unwrap().unwrap(), None);
        assert!(
            held.lock().unwrap()[0].is_cancelled(),
            "UI closes its dialog"
        );
        assert_eq!(
            unix::ask(&socket, other.id(), "next", false)
                .unwrap()
                .as_deref(),
            Some("next")
        );
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
        let clock = Arc::new(Clock::default());
        let (d, e) = (
            Session::register(clock.clone()),
            Session::register(clock.clone()),
        );
        assert_eq!(unix::ask(&socket, d.id(), "drop", false).unwrap(), None);
        assert_eq!(unix::ask(&socket, e.id(), "inject", false).unwrap(), None);
    }

    #[test]
    fn socket_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _socket) = unix::serve_for_test(|request| request.answer(None));
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn a_dialog_pauses_only_deadlines_of_its_own_clock() {
        let mine = Arc::new(Clock::default());
        let other = Arc::new(Clock::default());
        let paused = Deadline::after(Duration::from_millis(50), Some(mine.clone()));
        let unrelated = Deadline::after(Duration::from_millis(50), Some(other));
        let batch = Deadline::after(Duration::from_millis(50), None);
        let prompting = mine.start();
        std::thread::sleep(Duration::from_millis(80));
        assert!(!paused.expired(), "time in its own dialog must not count");
        assert!(
            unrelated.expired(),
            "another session's dialog must not pause it"
        );
        assert!(batch.expired(), "non-interactive deadlines never pause");
        drop(prompting);
        std::thread::sleep(Duration::from_millis(80));
        assert!(paused.expired());
    }
}
