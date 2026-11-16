//! One bounded kubectl invocation: argv only, no shell, stdin closed, the
//! inherited environment (PATH, KUBECONFIG, HOME, exec credential plugins)
//! unchanged. The direct child is always reaped. On Unix the child leads its own
//! process group, so cancellation, the deadline and the output limit also
//! signal credential-helper descendants that are still in that group.
use std::ffi::OsString;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One kubectl resolution for every Kubernetes connector (defined next to the
/// log connector, which predates this module). Nothing here rewrites PATH.
pub use crate::log::kubernetes::{kubectl_command, KubectlProgram, KUBECTL};

pub const DEFAULT_MAX_STDOUT: usize = 16 * 1024 * 1024;
pub const MAX_STDERR: usize = 16 * 1024;
const CHUNK: usize = 64 * 1024;
const POLL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Stdout beyond this stops the read and ends the process tree.
    pub max_stdout: usize,
    /// Wall-clock bound for the whole invocation, including credential plugins.
    pub deadline: Duration,
    /// Passed to kubectl as `--request-timeout` for API requests.
    pub request_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_stdout: DEFAULT_MAX_STDOUT,
            deadline: Duration::from_secs(45),
            request_timeout: Duration::from_secs(20),
        }
    }
}

/// Cooperative cancellation shared with whoever started the read.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Exited { code: Option<i32> },
    StdoutLimit,
    Deadline,
    Cancelled,
}

/// What happened to processes other than the direct child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cleanup {
    /// kubectl exited on its own; nothing was signalled. Descendants it left
    /// behind (for example a credential helper cache) are not ours to end.
    NotNeeded,
    /// Unix: the whole process group was killed while kubectl was still unreaped.
    /// A descendant that moved to another group or session is not reached.
    ProcessGroup,
    /// Only the direct kubectl child was killed: always on Windows, and on Unix
    /// when the process group could not be signalled. Credential-helper
    /// descendants are not reached.
    DirectChildOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raw {
    pub stdout: Vec<u8>,
    /// Bounded to `MAX_STDERR`; `stderr_truncated` says when more was dropped.
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
    pub end: End,
    pub cleanup: Cleanup,
}

/// The seam between collection and the process. Tests use synthetic transports;
/// production uses `Kubectl`.
pub trait Transport {
    fn run(&self, args: &[String], limits: Limits, cancel: &Cancel) -> io::Result<Raw>;
}

/// kubectl from PATH by default; `with_command` exists for synthetic fixtures.
pub struct Kubectl {
    command: Box<dyn Fn() -> Command + Send + Sync>,
}

impl Default for Kubectl {
    fn default() -> Self {
        Self::with_program(KubectlProgram::default())
    }
}

impl Kubectl {
    /// A configured kubectl executable and PATH, shared with the log connector.
    pub fn with_program(program: KubectlProgram) -> Self {
        Self {
            command: Box::new(move || program.command()),
        }
    }
    /// A different program (a local synthetic fixture) receiving the same argv.
    pub fn with_command(command: impl Fn() -> Command + Send + Sync + 'static) -> Self {
        Self {
            command: Box::new(command),
        }
    }
    pub fn program(&self) -> OsString {
        (self.command)().get_program().to_owned()
    }
}

impl Transport for Kubectl {
    fn run(&self, args: &[String], limits: Limits, cancel: &Cancel) -> io::Result<Raw> {
        run_process((self.command)(), args, limits, cancel)
    }
}

/// Keeps the first `STDERR_HEAD` bytes and the newest bytes after them, within
/// `MAX_STDERR` in total. kubectl prints warnings first and its error last, so
/// the final error line survives any amount of earlier noise.
const STDERR_HEAD: usize = 4 * 1024;
const STDERR_GAP: &[u8] = b"\n[...]\n";

#[derive(Default)]
struct Stderr {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    dropped: bool,
}

impl Stderr {
    fn push(&mut self, mut bytes: &[u8]) {
        let room = STDERR_HEAD - self.head.len();
        let take = bytes.len().min(room);
        self.head.extend_from_slice(&bytes[..take]);
        bytes = &bytes[take..];
        let cap = MAX_STDERR - STDERR_HEAD - STDERR_GAP.len();
        if bytes.len() >= cap {
            self.dropped |= !self.tail.is_empty() || bytes.len() > cap;
            self.tail.clear();
            bytes = &bytes[bytes.len() - cap..];
        } else {
            let excess = (self.tail.len() + bytes.len()).saturating_sub(cap);
            if excess > 0 {
                self.dropped = true;
                self.tail.drain(..excess);
            }
        }
        self.tail.extend(bytes);
    }

    fn finish(self, raw: &mut Raw) {
        raw.stderr = self.head;
        if self.dropped {
            raw.stderr.extend_from_slice(STDERR_GAP);
        }
        raw.stderr.extend(self.tail);
        raw.stderr_truncated = self.dropped;
    }
}

/// The spawned child plus the guarantee that it is reaped, even on panic.
struct Owned {
    child: Option<Child>,
}

impl Owned {
    /// Abnormal end (cancel, deadline, stdout limit, panic). kubectl has not
    /// been reaped on these paths, so on Unix its process-group id is still
    /// reserved and the group is signalled even if kubectl itself has already
    /// exited; then kubectl is reaped.
    fn terminate(&mut self) -> io::Result<Cleanup> {
        let Some(child) = self.child.as_mut() else {
            return Ok(Cleanup::NotNeeded);
        };
        let cleanup = kill_tree(child);
        child.wait()?;
        self.child.take();
        Ok(cleanup)
    }
}

#[cfg(unix)]
fn kill_tree(child: &mut Child) -> Cleanup {
    use rustix::io::Errno;
    use rustix::process::{kill_process_group, Pid, Signal};
    let group = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
    match group.map(|group| kill_process_group(group, Signal::KILL)) {
        Some(Ok(())) | Some(Err(Errno::SRCH)) => Cleanup::ProcessGroup,
        // The group could not be signalled: only the direct child is ended.
        _ => {
            let _ = child.kill();
            Cleanup::DirectChildOnly
        }
    }
}

#[cfg(not(unix))]
fn kill_tree(child: &mut Child) -> Cleanup {
    let _ = child.kill();
    Cleanup::DirectChildOnly
}

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn new_raw() -> Raw {
    Raw {
        stdout: Vec::new(),
        stderr: Vec::new(),
        stderr_truncated: false,
        end: End::Exited { code: None },
        cleanup: Cleanup::NotNeeded,
    }
}

pub(crate) fn run_process(
    mut command: Command,
    args: &[String],
    limits: Limits,
    cancel: &Cancel,
) -> io::Result<Raw> {
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let started = Instant::now();
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut owned = Owned { child: Some(child) };
    let mut raw = new_raw();
    let mut errors = Stderr::default();
    let abnormal = pump(
        &mut owned,
        stdout,
        stderr,
        &mut raw,
        &mut errors,
        limits,
        cancel,
        started,
    )?;
    if let Some(end) = abnormal {
        raw.cleanup = owned.terminate()?;
        raw.end = end;
    }
    errors.finish(&mut raw);
    Ok(raw)
}

fn read_some(input: &mut impl Read, buffer: &mut [u8]) -> io::Result<Option<usize>> {
    match input.read(buffer) {
        Ok(count) => Ok(Some(count)),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Unix: both pipes are nonblocking and read on this thread. No reader thread
/// outlives the call, and both pipe ends close when it returns, even when a
/// credential-helper descendant still holds the write end of stderr.
/// Completion is "stdout at EOF and kubectl exited"; everything kubectl wrote
/// to stderr is then already in the pipe and is drained without waiting.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn pump(
    owned: &mut Owned,
    mut stdout: std::process::ChildStdout,
    mut stderr: std::process::ChildStderr,
    raw: &mut Raw,
    errors: &mut Stderr,
    limits: Limits,
    cancel: &Cancel,
    started: Instant,
) -> io::Result<Option<End>> {
    rustix::io::ioctl_fionbio(&stdout, true)?;
    rustix::io::ioctl_fionbio(&stderr, true)?;
    let mut buffer = vec![0; CHUNK];
    let (mut out_open, mut err_open) = (true, true);
    loop {
        if cancel.is_cancelled() {
            return Ok(Some(End::Cancelled));
        }
        let elapsed = started.elapsed();
        if elapsed >= limits.deadline {
            return Ok(Some(End::Deadline));
        }
        let mut progressed = false;
        if out_open {
            match read_some(&mut stdout, &mut buffer) {
                Ok(Some(0)) => out_open = false,
                Ok(Some(count)) => {
                    if raw.stdout.len() + count > limits.max_stdout {
                        return Ok(Some(End::StdoutLimit));
                    }
                    raw.stdout.extend_from_slice(&buffer[..count]);
                    progressed = true;
                }
                Ok(None) => {}
                Err(error) => {
                    errors.push(format!("\n[okilum: stdout read error: {error}]").as_bytes());
                    out_open = false;
                }
            }
        }
        if err_open {
            match read_some(&mut stderr, &mut buffer) {
                Ok(Some(0)) => err_open = false,
                Ok(Some(count)) => {
                    errors.push(&buffer[..count]);
                    progressed = true;
                }
                Ok(None) => {}
                Err(_) => err_open = false,
            }
        }
        if !out_open {
            let child = owned.child.as_mut().expect("unreaped child");
            if let Some(status) = child.try_wait()? {
                owned.child.take();
                while err_open {
                    match read_some(&mut stderr, &mut buffer) {
                        Ok(Some(0)) | Ok(None) | Err(_) => err_open = false,
                        Ok(Some(count)) => errors.push(&buffer[..count]),
                    }
                }
                raw.end = End::Exited {
                    code: status.code(),
                };
                return Ok(None);
            }
        }
        if !progressed {
            std::thread::sleep(POLL.min(limits.deadline - elapsed));
        }
    }
}

/// Windows: blocking pipes, one reader thread each. After kubectl exits,
/// stderr gets a short grace period to reach EOF; a reader whose pipe is held
/// open by a descendant ends only when that descendant closes it.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
fn pump(
    owned: &mut Owned,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    raw: &mut Raw,
    errors: &mut Stderr,
    limits: Limits,
    cancel: &Cancel,
    started: Instant,
) -> io::Result<Option<End>> {
    use std::sync::mpsc::{self, RecvTimeoutError};
    enum Message {
        Out(Vec<u8>),
        Err(Vec<u8>),
        Eof(bool),
    }
    fn reader(mut input: impl Read + Send + 'static, sender: mpsc::SyncSender<Message>, err: bool) {
        std::thread::spawn(move || loop {
            let mut bytes = vec![0; CHUNK];
            match input.read(&mut bytes) {
                Ok(0) => {
                    let _ = sender.send(Message::Eof(err));
                    break;
                }
                Ok(count) => {
                    bytes.truncate(count);
                    let message = if err {
                        Message::Err(bytes)
                    } else {
                        Message::Out(bytes)
                    };
                    if sender.send(message).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    let _ = sender.send(Message::Eof(err));
                    break;
                }
            }
        });
    }
    let (sender, receiver) = mpsc::sync_channel(8);
    reader(stdout, sender.clone(), false);
    reader(stderr, sender, true);
    let (mut out_open, mut err_open) = (true, true);
    let mut exited: Option<(Option<i32>, Instant)> = None;
    loop {
        if exited.is_none() {
            if cancel.is_cancelled() {
                return Ok(Some(End::Cancelled));
            }
            if started.elapsed() >= limits.deadline {
                return Ok(Some(End::Deadline));
            }
        }
        if let Some((code, at)) = exited {
            if !err_open || at.elapsed() >= Duration::from_millis(250) {
                raw.end = End::Exited { code };
                return Ok(None);
            }
        } else if !out_open {
            let child = owned.child.as_mut().expect("unreaped child");
            if let Some(status) = child.try_wait()? {
                owned.child.take();
                exited = Some((status.code(), Instant::now()));
                continue;
            }
        }
        match receiver.recv_timeout(POLL) {
            Ok(Message::Out(bytes)) => {
                if raw.stdout.len() + bytes.len() > limits.max_stdout {
                    return Ok(Some(End::StdoutLimit));
                }
                raw.stdout.extend_from_slice(&bytes);
            }
            Ok(Message::Err(bytes)) => errors.push(&bytes),
            Ok(Message::Eof(err)) => {
                if err {
                    err_open = false;
                } else {
                    out_open = false;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                out_open = false;
                err_open = false;
            }
        }
    }
}
