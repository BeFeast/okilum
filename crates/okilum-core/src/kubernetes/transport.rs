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
    /// kubectl exited on its own and stderr closed within the grace period;
    /// nothing was signalled. On Windows the Job Object handle still closes
    /// at return, which ends any process left in it.
    NotNeeded,
    /// Unix: the whole process group was killed while kubectl was unreaped
    /// (cancel, deadline, stdout limit, or stderr still held open after the
    /// grace period). A descendant that moved to another group or session is
    /// not reached.
    ProcessGroup,
    /// Windows: the Job Object holding kubectl and every descendant was
    /// terminated (same triggers as `ProcessGroup`).
    JobObject,
    /// Only the direct kubectl child was killed: the process group could not
    /// be signalled, or no Job Object could be created/assigned. Credential-
    /// helper descendants are not reached.
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

/// After kubectl exits, stderr gets this long to reach EOF. A descendant that
/// still holds it then (a credential helper, or anything it started) is ended
/// with the rest of the owned tree.
const STDERR_GRACE: Duration = Duration::from_millis(250);
/// Windows reader threads are joined for at most this long once the tree ended.
const READER_JOIN: Duration = Duration::from_secs(2);

/// The owned process tree: kubectl, its process group (Unix) or Job Object
/// (Windows), and the guarantee that both are ended and kubectl reaped, even
/// on panic.
struct Owned {
    child: Option<Child>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl Owned {
    /// End the whole tree, then reap kubectl. On Unix kubectl is never reaped
    /// before this (its exit is observed with `waitid(NOWAIT)`), so its
    /// process-group id is still reserved even when kubectl itself exited.
    fn terminate(&mut self) -> io::Result<(Cleanup, Option<i32>)> {
        let cleanup = self.kill_tree();
        let code = self.reap()?;
        Ok((cleanup, code))
    }

    fn reap(&mut self) -> io::Result<Option<i32>> {
        match self.child.take() {
            Some(mut child) => Ok(child.wait()?.code()),
            None => Ok(None),
        }
    }

    #[cfg(unix)]
    fn kill_tree(&mut self) -> Cleanup {
        use rustix::io::Errno;
        use rustix::process::{kill_process_group, Pid, Signal};
        let Some(child) = self.child.as_mut() else {
            return Cleanup::NotNeeded;
        };
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

    #[cfg(windows)]
    fn kill_tree(&mut self) -> Cleanup {
        if let Some(job) = &self.job {
            if job.terminate().is_ok() {
                return Cleanup::JobObject;
            }
        }
        match self.child.as_mut() {
            Some(child) => {
                let _ = child.kill();
                Cleanup::DirectChildOnly
            }
            None => Cleanup::NotNeeded,
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn kill_tree(&mut self) -> Cleanup {
        match self.child.as_mut() {
            Some(child) => {
                let _ = child.kill();
                Cleanup::DirectChildOnly
            }
            None => Cleanup::NotNeeded,
        }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.terminate();
        }
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

/// How the read loop ended.
#[derive(Clone, Copy)]
enum Outcome {
    Abnormal(End),
    /// kubectl exited with stdout at EOF; `stderr_closed` is false when the
    /// grace period ended with stderr still held open by a descendant.
    Exited {
        stderr_closed: bool,
    },
}

fn spawn(mut command: Command) -> io::Result<Owned> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
        Ok(Owned {
            child: Some(command.spawn()?),
        })
    }
    #[cfg(windows)]
    {
        // Created suspended so the Job Object owns kubectl before it can start
        // a credential helper; every later descendant is in the job too.
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let job = job::Job::new()
            .and_then(|job| job.assign(&child).map(|()| job))
            .ok();
        if let Err(error) = job::resume(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Owned {
            child: Some(child),
            job,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(Owned {
            child: Some(command.spawn()?),
        })
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
    let started = Instant::now();
    let mut owned = spawn(command)?;
    let child = owned.child.as_mut().expect("spawned child");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut raw = new_raw();
    let mut errors = Stderr::default();
    let (outcome, readers) = pump(
        &mut owned,
        stdout,
        stderr,
        &mut raw,
        &mut errors,
        limits,
        cancel,
        started,
    )?;
    let (cleanup, code) = match outcome {
        Outcome::Abnormal(end) => {
            raw.end = end;
            owned.terminate()?
        }
        Outcome::Exited {
            stderr_closed: true,
        } => (Cleanup::NotNeeded, owned.reap()?),
        Outcome::Exited {
            stderr_closed: false,
        } => owned.terminate()?,
    };
    if let Outcome::Exited { .. } = outcome {
        raw.end = End::Exited { code };
    }
    raw.cleanup = cleanup;
    join_readers(readers);
    drop(owned);
    errors.finish(&mut raw);
    Ok(raw)
}

#[cfg(unix)]
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

/// Unix: whether kubectl exited, without reaping it (`waitid` with NOWAIT), so
/// its process-group id stays reserved for a later group signal.
#[cfg(unix)]
fn leader_exited(owned: &Owned) -> io::Result<bool> {
    use rustix::process::{waitid, Pid, WaitId, WaitIdOptions};
    let Some(child) = owned.child.as_ref() else {
        return Ok(true);
    };
    let pid = i32::try_from(child.id())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::other("invalid kubectl process id"))?;
    Ok(waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )?
    .is_some())
}

/// Unix: both pipes are nonblocking and read on this thread. No reader thread
/// outlives the call, and both pipe ends close when it returns. Completion is
/// "stdout at EOF and kubectl exited", then stderr gets `STDERR_GRACE` to close;
/// the cancel and deadline checks stay in force throughout.
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
) -> io::Result<(Outcome, Readers)> {
    rustix::io::ioctl_fionbio(&stdout, true)?;
    rustix::io::ioctl_fionbio(&stderr, true)?;
    let mut buffer = vec![0; CHUNK];
    let (mut out_open, mut err_open) = (true, true);
    let mut grace_until: Option<Instant> = None;
    loop {
        if cancel.is_cancelled() {
            return Ok((Outcome::Abnormal(End::Cancelled), Vec::new()));
        }
        let elapsed = started.elapsed();
        if elapsed >= limits.deadline {
            return Ok((Outcome::Abnormal(End::Deadline), Vec::new()));
        }
        let mut progressed = false;
        if out_open {
            match read_some(&mut stdout, &mut buffer) {
                Ok(Some(0)) => out_open = false,
                Ok(Some(count)) => {
                    if raw.stdout.len() + count > limits.max_stdout {
                        return Ok((Outcome::Abnormal(End::StdoutLimit), Vec::new()));
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
        if !out_open && grace_until.is_none() && leader_exited(owned)? {
            grace_until = Some(Instant::now() + STDERR_GRACE);
        }
        if let Some(until) = grace_until {
            if !err_open {
                return Ok((
                    Outcome::Exited {
                        stderr_closed: true,
                    },
                    Vec::new(),
                ));
            }
            if Instant::now() >= until {
                return Ok((
                    Outcome::Exited {
                        stderr_closed: false,
                    },
                    Vec::new(),
                ));
            }
        }
        if !progressed {
            std::thread::sleep(POLL.min(limits.deadline - elapsed));
        }
    }
}

/// Test instrumentation: reader threads alive right now (non-Unix), so a
/// test can prove no reader outlives a read. Production has no such state.
#[cfg(all(test, not(unix)))]
pub(crate) static LIVE_READERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Counts one reader from before its spawn until its thread ends; a failed
/// spawn drops it at once, so no phantom count remains.
#[cfg(all(test, not(unix)))]
struct LiveReader;

#[cfg(all(test, not(unix)))]
impl LiveReader {
    fn new() -> Self {
        LIVE_READERS.fetch_add(1, Ordering::SeqCst);
        LiveReader
    }
}

#[cfg(all(test, not(unix)))]
impl Drop for LiveReader {
    fn drop(&mut self) {
        LIVE_READERS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Reader threads (Windows only; Unix reads on the calling thread, so the list
/// is empty there).
type Readers = Vec<std::thread::JoinHandle<()>>;

/// Windows: once the Job Object is terminated every write end is closed, so
/// the readers reach EOF; they are joined with a bound.
fn join_readers(readers: Readers) {
    let until = Instant::now() + READER_JOIN;
    for reader in readers {
        while !reader.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        if reader.is_finished() {
            let _ = reader.join();
        }
    }
}

/// Windows: blocking pipes, one reader thread each, owned by the Job Object
/// lifecycle (see `join_readers`).
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
) -> io::Result<(Outcome, Readers)> {
    use std::sync::mpsc::{self, RecvTimeoutError};
    enum Message {
        Out(Vec<u8>),
        Err(Vec<u8>),
        Eof(bool),
    }
    fn reader(
        mut input: impl Read + Send + 'static,
        sender: mpsc::SyncSender<Message>,
        err: bool,
    ) -> std::thread::JoinHandle<()> {
        #[cfg(test)]
        let live = LiveReader::new();
        std::thread::spawn(move || {
            #[cfg(test)]
            let _live = live;
            read_loop(&mut input, &sender, err)
        })
    }
    fn read_loop(input: &mut impl Read, sender: &mpsc::SyncSender<Message>, err: bool) {
        loop {
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
        }
    }
    let (sender, receiver) = mpsc::sync_channel(8);
    let readers = vec![
        reader(stdout, sender.clone(), false),
        reader(stderr, sender, true),
    ];
    let (mut out_open, mut err_open) = (true, true);
    let mut grace_until: Option<Instant> = None;
    loop {
        if cancel.is_cancelled() {
            return Ok((Outcome::Abnormal(End::Cancelled), readers));
        }
        let elapsed = started.elapsed();
        if elapsed >= limits.deadline {
            return Ok((Outcome::Abnormal(End::Deadline), readers));
        }
        if !out_open && grace_until.is_none() {
            let child = owned.child.as_mut().expect("unreaped child");
            if child.try_wait()?.is_some() {
                grace_until = Some(Instant::now() + STDERR_GRACE);
            }
        }
        if let Some(until) = grace_until {
            if !err_open {
                return Ok((
                    Outcome::Exited {
                        stderr_closed: true,
                    },
                    readers,
                ));
            }
            if Instant::now() >= until {
                return Ok((
                    Outcome::Exited {
                        stderr_closed: false,
                    },
                    readers,
                ));
            }
        }
        match receiver.recv_timeout(POLL.min(limits.deadline - elapsed)) {
            Ok(Message::Out(bytes)) => {
                if raw.stdout.len() + bytes.len() > limits.max_stdout {
                    return Ok((Outcome::Abnormal(End::StdoutLimit), readers));
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

/// Windows Job Object owning kubectl and every descendant. Closing the handle
/// (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) also ends whatever is left.
#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    pub(super) struct Job(HANDLE);

    impl Job {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: plain Win32 calls on a handle owned by this value.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(handle);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION
                        as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        pub(super) fn assign(&self, child: &Child) -> io::Result<()> {
            let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Resume the threads of a process created with `CREATE_SUSPENDED`.
    pub(super) fn resume(pid: u32) -> io::Result<()> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut resumed = 0;
        let mut more = unsafe { Thread32First(snapshot, &mut entry) };
        while more != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    if unsafe { ResumeThread(thread) } != u32::MAX {
                        resumed += 1;
                    }
                    unsafe { CloseHandle(thread) };
                }
            }
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            more = unsafe { Thread32Next(snapshot, &mut entry) };
        }
        unsafe { CloseHandle(snapshot) };
        if resumed == 0 {
            return Err(io::Error::other(
                "could not resume the suspended kubectl process",
            ));
        }
        Ok(())
    }
}
