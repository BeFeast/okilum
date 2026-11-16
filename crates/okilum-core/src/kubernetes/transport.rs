//! One bounded kubectl invocation: argv only, no shell, stdin closed, the
//! inherited environment (PATH, KUBECONFIG, HOME, exec credential plugins)
//! unchanged. The direct child is always reaped. On Unix the child leads its own
//! process group, so cancellation, the deadline and the output limit also
//! signal credential-helper descendants that are still in that group.
use std::ffi::OsString;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One kubectl resolution for every Kubernetes connector (defined next to the
/// log connector, which predates this module). Nothing here rewrites PATH.
pub use crate::log::kubernetes::{kubectl_command, KUBECTL};

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
    /// Only the direct kubectl child was killed (Windows): credential-helper
    /// descendants are not tracked.
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
        Self {
            command: Box::new(kubectl_command),
        }
    }
}

impl Kubectl {
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

enum Message {
    Out(Vec<u8>),
    Err(Vec<u8>),
    /// The pipe closed; `true` for stderr.
    Eof(bool),
    ReadError(bool, io::Error),
}

fn push_stderr(raw: &mut Raw, bytes: &[u8]) {
    let room = MAX_STDERR - raw.stderr.len();
    if bytes.len() > room {
        raw.stderr_truncated = true;
    }
    raw.stderr
        .extend_from_slice(&bytes[..bytes.len().min(room)]);
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
            Err(error) => {
                let _ = sender.send(Message::ReadError(err, error));
                break;
            }
        }
    });
}

/// The spawned child plus the guarantee that it is reaped, even on panic.
struct Owned {
    child: Option<Child>,
}

impl Owned {
    /// Ends the tree while kubectl is still unreaped, so the group id cannot
    /// have been reused, then reaps the direct child.
    fn terminate(&mut self) -> io::Result<Cleanup> {
        let Some(child) = self.child.as_mut() else {
            return Ok(Cleanup::NotNeeded);
        };
        let cleanup = if child.try_wait()?.is_none() {
            kill_tree(child)
        } else {
            Cleanup::NotNeeded
        };
        child.wait()?;
        self.child.take();
        Ok(cleanup)
    }
}

/// Unix: the child was spawned as a process-group leader and is still unreaped,
/// so its group id cannot have been reused yet.
#[cfg(unix)]
fn kill_tree(child: &mut Child) -> Cleanup {
    use rustix::io::Errno;
    use rustix::process::{kill_process_group, Pid, Signal};
    let group = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
    match group.map(|group| kill_process_group(group, Signal::KILL)) {
        Some(Ok(())) | Some(Err(Errno::SRCH)) => {}
        _ => {
            let _ = child.kill();
        }
    }
    Cleanup::ProcessGroup
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
    let (sender, receiver) = mpsc::sync_channel(8);
    reader(stdout, sender.clone(), false);
    reader(stderr, sender, true);

    let mut raw = Raw {
        stdout: Vec::new(),
        stderr: Vec::new(),
        stderr_truncated: false,
        end: End::Exited { code: None },
        cleanup: Cleanup::NotNeeded,
    };
    // Stderr may be held open by a credential-helper descendant that outlives
    // kubectl, so completion is: stdout drained and kubectl exited.
    let (mut stdout_open, mut readers) = (true, 2);
    let abnormal = loop {
        if cancel.is_cancelled() {
            break Some(End::Cancelled);
        }
        let elapsed = started.elapsed();
        if elapsed >= limits.deadline {
            break Some(End::Deadline);
        }
        if !stdout_open {
            let child = owned.child.as_mut().expect("unreaped child");
            if let Some(status) = child.try_wait()? {
                owned.child.take();
                // Collect stderr that is already written, without waiting on it.
                while let Ok(message) = receiver.try_recv() {
                    if let Message::Err(bytes) = message {
                        push_stderr(&mut raw, &bytes);
                    }
                }
                raw.end = End::Exited {
                    code: status.code(),
                };
                break None;
            }
        }
        let wait = POLL.min(limits.deadline - elapsed);
        if readers == 0 {
            std::thread::sleep(wait);
            continue;
        }
        match receiver.recv_timeout(wait) {
            Ok(Message::Out(bytes)) => {
                if raw.stdout.len() + bytes.len() > limits.max_stdout {
                    break Some(End::StdoutLimit);
                }
                raw.stdout.extend_from_slice(&bytes);
            }
            Ok(Message::Err(bytes)) => push_stderr(&mut raw, &bytes),
            Ok(Message::Eof(err)) => {
                readers -= 1;
                stdout_open &= err;
            }
            Ok(Message::ReadError(err, error)) => {
                readers -= 1;
                stdout_open &= err;
                push_stderr(
                    &mut raw,
                    format!("\n[okilum: pipe read error: {error}]").as_bytes(),
                );
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                readers = 0;
                stdout_open = false;
            }
        }
    };
    if let Some(end) = abnormal {
        // Release blocked readers first; an escaped descendant may keep a pipe.
        drop(receiver);
        raw.cleanup = owned.terminate()?;
        raw.end = end;
    }
    Ok(raw)
}
