//! SFTP transport runs off the UI thread; published history is leased and bounded.
use okilum_core::log::{
    sftp::{Follow, OpenSsh, Target, Transport, Update},
    stream::{Assembler, Snapshot},
};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

pub(super) struct Frame {
    pub data: Option<Arc<Snapshot>>,
    pub message: String,
    pub active: bool,
    pub error: bool,
}
pub(super) struct Worker(Arc<AtomicBool>);
impl Worker {
    pub fn start(
        target: Target,
        follow: bool,
        scope: String,
    ) -> (Self, async_channel::Receiver<Frame>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = cancel.clone();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            run(
                target,
                OpenSsh::default(),
                follow,
                scope,
                stopped,
                |frame| sender.send_blocking(frame).is_ok(),
            );
        });
        (Self(cancel), receiver)
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Cancellation is checked between *each* positional operation, including
/// consumed-prefix proof reads. It must not wait for a whole proof/poll to finish.
/// An in-flight OpenSSH exchange still has the connector's existing deadline.
struct Cancellable<T> {
    inner: T,
    cancel: Arc<AtomicBool>,
}
impl<T: Transport> Cancellable<T> {
    fn check(&mut self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            self.inner.disconnect();
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Session stopped",
            ))
        } else {
            Ok(())
        }
    }
}
impl<T: Transport> Transport for Cancellable<T> {
    fn open(&mut self, target: &Target) -> io::Result<()> {
        self.check()?;
        self.inner.open(target)
    }
    fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
        self.check()?;
        self.inner.read_at(offset, limit)
    }
    fn disconnect(&mut self) {
        self.inner.disconnect();
    }
}

/// Follow tick between EOF polls.
const FOLLOW_TICK: Duration = Duration::from_secs(1);
/// Matches the HTTPS adapter: 1/2/4/8 s backoff, the fifth consecutive failure
/// stops. Any successful poll (records, partial data or EOF) resets the budget.
const RECONNECT_ATTEMPTS: u32 = 5;
fn backoff(failures: u32) -> Option<Duration> {
    (failures < RECONNECT_ATTEMPTS).then(|| Duration::from_secs(1 << (failures - 1)))
}

/// Cancellable wait in short slices on the worker thread, never a UI timer.
fn wait(cancel: &AtomicBool, delay: Duration) {
    let deadline = std::time::Instant::now() + delay;
    while !cancel.load(Ordering::Acquire) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

fn run(
    target: Target,
    transport: impl Transport,
    follow: bool,
    scope: String,
    cancel: Arc<AtomicBool>,
    send: impl FnMut(Frame) -> bool,
) {
    let waiting = cancel.clone();
    run_paced(
        target,
        transport,
        follow,
        scope,
        cancel,
        send,
        move |delay| wait(&waiting, delay),
    );
}

/// A dropped connection after the session has read successfully reconnects
/// on the *same* `Follow`: it keeps the consumed-prefix proof, cursor and
/// unfinished record, so recovery re-verifies the prefix and appends only
/// unseen bytes (no duplicate or missing records). A failure before the first
/// successful read (host, trust or authentication) is reported immediately.
fn run_paced(
    target: Target,
    transport: impl Transport,
    follow: bool,
    scope: String,
    cancel: Arc<AtomicBool>,
    mut send: impl FnMut(Frame) -> bool,
    mut pause: impl FnMut(Duration),
) {
    let mut assembler = match Assembler::new("sftp-session.log".into(), scope) {
        Ok(assembler) => assembler,
        Err(error) => {
            let _ = send(Frame {
                data: None,
                message: error.to_string(),
                active: false,
                error: true,
            });
            return;
        }
    };
    let mut source = Follow::new(
        target,
        Cancellable {
            inner: transport,
            cancel: cancel.clone(),
        },
    );
    let mut data: Option<Arc<Snapshot>> = None;
    let mut connected = false;
    let mut failures = 0u32;
    let (message, error) = loop {
        if cancel.load(Ordering::Acquire) {
            break ("Stopped".into(), false);
        }
        let update = source.poll();
        if cancel.load(Ordering::Acquire) {
            break ("Stopped".into(), false);
        }
        let read = matches!(
            update,
            Update::Records(_) | Update::Pending | Update::CaughtUp
        );
        let recovered = read && failures > 0;
        if read {
            connected = true;
            failures = 0;
        }
        match update {
            Update::Records(bytes) => match assembler.append(&bytes) {
                Ok(snapshot) => {
                    if snapshot.is_some() { data = snapshot; }
                    if !send(Frame { data: data.clone(), message: "Reading SFTP log…".into(), active: true, error: false }) { return; }
                }
                Err(error) => break (error.to_string(), true),
            },
            Update::Pending => {
                if recovered && !send(Frame { data: None, message: "Reconnected · reading SFTP log…".into(), active: true, error: false }) { return; }
            },
            Update::CaughtUp => {
                let emitted = data.as_ref().map_or(0, |snapshot| snapshot.bytes().len() as u64);
                let pending = source.offset().saturating_sub(emitted);
                let suffix = if pending == 0 { String::new() } else { format!(" · {pending} trailing bytes without a complete line") };
                if !follow { break (format!("Loaded snapshot{suffix}"), false); }
                if !send(Frame { data: data.clone(), message: format!("Following SFTP log{suffix}"), active: true, error: false }) { return; }
                pause(FOLLOW_TICK);
            },
            Update::Disconnected(error) if !connected => break (format!("Disconnected: {error}. Start a new session to retry; loaded rows retained."), true),
            Update::Disconnected(error) => {
                failures += 1;
                let Some(delay) = backoff(failures) else {
                    break (format!("Disconnected: {error}. Connection failed {RECONNECT_ATTEMPTS} times in a row; loaded rows retained. Start a new session to retry."), true);
                };
                let message = format!(
                    "Disconnected: {error}. Reconnecting in {}s (retry {failures} of {}); loaded rows retained.",
                    delay.as_secs(),
                    RECONNECT_ATTEMPTS - 1,
                );
                // Rows stay as published; this frame only changes the status.
                if !send(Frame { data: None, message, active: true, error: true }) { return; }
                pause(delay);
            },
            Update::ContinuityLost => break ("File continuity lost (rotation, truncation or rewrite). Loaded rows retained; start a new session explicitly.".into(), true),
            Update::LimitReached => break ("SFTP history limit reached (8 MiB); loaded rows retained. Start a new session explicitly.".into(), true),
        }
    };
    drop(source); // Close the exact owned transport before reporting terminal state.
    let _ = send(Frame {
        data,
        message,
        active: false,
        error,
    });
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
