//! One explicitly selected, read-only kubectl session. No discovery or retries.
use okilum_core::log::{
    kubernetes::{Event, Request, Session},
    stream::{Assembler, Snapshot},
};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(100);
const DIAGNOSTIC_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Reading,
    Complete,
    Stopped,
    Failed(String),
}

pub(super) struct Update {
    pub snapshot: Option<Arc<Snapshot>>,
    pub status: Status,
    pub diagnostic: String,
    pub pending: usize,
}

pub(super) struct Worker(Arc<AtomicBool>);
impl Worker {
    pub fn start(request: Request, scope: String) -> (Self, async_channel::Receiver<Update>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = async_channel::bounded(1);
        let stopped = cancel.clone();
        std::thread::spawn(move || {
            if stopped.load(Ordering::Acquire) {
                return;
            }
            let path = format!(
                "{}/{}/{}.log",
                request.namespace, request.pod, request.container
            );
            let prepared = Assembler::new(path.into(), scope).and_then(|assembler| {
                anyhow::ensure!(
                    !stopped.load(Ordering::Acquire),
                    "Session cancelled before opening kubectl"
                );
                Ok((request.open()?, assembler))
            });
            match prepared {
                Ok((session, assembler)) => {
                    run(session, assembler, &stopped, |update, _| {
                        // Backpressure preserves the latest rows even if the UI
                        // pauses and no more transport data arrives afterwards.
                        // Dropping the window's receiver releases a blocked send.
                        sender.send_blocking(update).is_ok()
                    })
                }
                Err(error) => {
                    let _ = sender.send_blocking(Update {
                        snapshot: None,
                        status: Status::Failed(error.to_string()),
                        diagnostic: String::new(),
                        pending: 0,
                    });
                }
            }
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

enum Input {
    Data(Vec<u8>),
    Diagnostic(Vec<u8>),
    Exit(bool, String),
}
trait Source {
    fn next(&mut self) -> io::Result<Option<Input>>;
    fn cancel(&mut self) -> io::Result<()>;
}
impl Source for Session {
    fn next(&mut self) -> io::Result<Option<Input>> {
        Ok(match self.next(POLL)? {
            Some(Event::Data(data)) => Some(Input::Data(data)),
            Some(Event::Diagnostic(data)) => Some(Input::Diagnostic(data)),
            Some(Event::Exited(status)) => Some(Input::Exit(status.success(), status.to_string())),
            Some(Event::ReadError(error)) => return Err(error),
            None => None,
        })
    }
    fn cancel(&mut self) -> io::Result<()> {
        Session::cancel(self)
    }
}

fn run(
    mut source: impl Source,
    mut assembler: Assembler,
    cancel: &AtomicBool,
    mut send: impl FnMut(Update, bool) -> bool,
) {
    let mut snapshot = None;
    let mut diagnostic = Vec::new();
    let mut published = Instant::now();
    let mut dirty = false;
    let terminal = loop {
        if cancel.load(Ordering::Acquire) {
            break Status::Stopped;
        }
        match source.next() {
            Ok(Some(Input::Data(bytes))) => match assembler.append(&bytes) {
                Ok(next) => {
                    if next.is_some() {
                        snapshot = next;
                    }
                    dirty = true;
                }
                Err(error) => break Status::Failed(error.to_string()),
            },
            Ok(Some(Input::Diagnostic(bytes))) => {
                // Keep bounded stderr separately; it must never become a log row.
                let keep = bytes.len().min(DIAGNOSTIC_BYTES);
                diagnostic.extend_from_slice(&bytes[bytes.len() - keep..]);
                if diagnostic.len() > DIAGNOSTIC_BYTES {
                    diagnostic.drain(..diagnostic.len() - DIAGNOSTIC_BYTES);
                }
                dirty = true;
            }
            Ok(Some(Input::Exit(true, _))) => break Status::Complete,
            Ok(Some(Input::Exit(false, status))) => {
                break Status::Failed(format!("kubectl failed: {status}"));
            }
            Ok(None) => {}
            Err(error) => break Status::Failed(error.to_string()),
        }
        if dirty && published.elapsed() >= POLL {
            if !send(
                Update {
                    snapshot: snapshot.clone(),
                    status: Status::Reading,
                    diagnostic: String::from_utf8_lossy(&diagnostic).into_owned(),
                    pending: assembler.pending_bytes(),
                },
                false,
            ) {
                let _ = source.cancel();
                return;
            }
            dirty = false;
            published = Instant::now();
        }
    };
    let status = match source.cancel() {
        Ok(()) => terminal,
        Err(error) => Status::Failed(format!("Could not stop kubectl: {error}")),
    };
    let _ = send(
        Update {
            snapshot,
            status,
            diagnostic: String::from_utf8_lossy(&diagnostic).into_owned(),
            pending: assembler.pending_bytes(),
        },
        true,
    );
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
