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

fn run(
    target: Target,
    transport: impl Transport,
    follow: bool,
    scope: String,
    cancel: Arc<AtomicBool>,
    mut send: impl FnMut(Frame) -> bool,
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
    let (message, error) = loop {
        if cancel.load(Ordering::Acquire) {
            break ("Stopped".into(), false);
        }
        let update = source.poll();
        if cancel.load(Ordering::Acquire) {
            break ("Stopped".into(), false);
        }
        match update {
            Update::Records(bytes) => match assembler.append(&bytes) {
                Ok(snapshot) => {
                    if snapshot.is_some() { data = snapshot; }
                    if !send(Frame { data: data.clone(), message: "Reading SFTP log…".into(), active: true, error: false }) { return; }
                }
                Err(error) => break (error.to_string(), true),
            },
            Update::Pending => {},
            Update::CaughtUp => {
                let emitted = data.as_ref().map_or(0, |snapshot| snapshot.bytes().len() as u64);
                let pending = source.offset().saturating_sub(emitted);
                let suffix = if pending == 0 { String::new() } else { format!(" · {pending} trailing bytes without a complete line") };
                if !follow { break (format!("Loaded snapshot{suffix}"), false); }
                if !send(Frame { data: data.clone(), message: format!("Following SFTP log{suffix}"), active: true, error: false }) { return; }
                // Short cancellable waits, never a timer on the render path.
                for _ in 0..10 {
                    if cancel.load(Ordering::Acquire) { break; }
                    std::thread::sleep(Duration::from_millis(100));
                }
            },
            Update::Disconnected(error) => break (format!("Disconnected: {error}. Start a new session to retry; loaded rows retained."), true),
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
