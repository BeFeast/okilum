//! Read-only `kubectl logs` connector. Credentials and context come from kubectl's
//! existing environment; this module never edits kubeconfig or cluster resources.
use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::Duration;

const CHUNK_BYTES: usize = 16 * 1024;
const QUEUED_CHUNKS: usize = 16;

#[derive(Clone, Debug)]
pub struct Request {
    /// None uses the existing current context, without changing it.
    pub context: Option<String>,
    pub namespace: String,
    pub pod: String,
    pub container: String,
    pub previous: bool,
    pub follow: bool,
    /// Initial history is bounded by lines; individual lines still stream in chunks.
    pub tail_lines: u32,
}

impl Request {
    pub fn args(&self) -> io::Result<Vec<String>> {
        for (name, value) in [
            ("namespace", self.namespace.as_str()),
            ("pod", self.pod.as_str()),
            ("container", self.container.as_str()),
        ] {
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
                || !value.as_bytes()[0].is_ascii_alphanumeric()
                || !value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Explicit Kubernetes {name} is required (DNS name)"),
                ));
            }
        }
        if self.tail_lines > 100_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Kubernetes history is limited to 100000 lines",
            ));
        }
        let mut args = Vec::new();
        if let Some(context) = &self.context {
            if context.trim().is_empty() || context.chars().any(char::is_control) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid Kubernetes context",
                ));
            }
            args.push(format!("--context={context}"));
        }
        args.extend([
            format!("--namespace={}", self.namespace),
            "logs".into(),
            format!("pod/{}", self.pod),
            format!("--container={}", self.container),
            format!("--tail={}", self.tail_lines),
            format!("--previous={}", self.previous),
            format!("--follow={}", self.follow),
            "--ignore-errors=false".into(),
        ]);
        Ok(args)
    }

    /// Start only after the user explicitly selects the target. No discovery,
    /// credential creation, shell interpolation, retries or cluster mutations.
    pub fn open(&self) -> io::Result<Session> {
        let mut command = Command::new("kubectl");
        command.args(self.args()?);
        Session::spawn(command)
    }
}

#[derive(Debug)]
pub enum Event {
    /// Exact log bytes. Chunk boundaries are not record boundaries.
    Data(Vec<u8>),
    /// Diagnostic bytes, kept separate from log records, also bounded chunks.
    Diagnostic(Vec<u8>),
    ReadError(io::Error),
    /// Delivered after both pipes drain, including nonzero kubectl failures.
    Exited(ExitStatus),
}

/// A bounded stream, suitable for a bounded Log view adapter. At most 256 KiB
/// is queued, plus one 16 KiB chunk per pipe. Consumers must bound retained data.
/// Dropping or cancelling kills and reaps the kubectl process, even if pipes stall.
pub struct Session {
    child: Option<Child>,
    events: Option<Receiver<Event>>,
}

impl Session {
    fn spawn(mut command: Command) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (sender, receiver) = mpsc::sync_channel(QUEUED_CHUNKS);
        pipe(
            child.stdout.take().expect("piped stdout"),
            sender.clone(),
            false,
        );
        pipe(child.stderr.take().expect("piped stderr"), sender, true);
        Ok(Self {
            child: Some(child),
            events: Some(receiver),
        })
    }

    /// None means no update yet (or a cancelled/completed session). Pipe reads
    /// never block the caller; kubectl failures remain visible as Exited events.
    pub fn next(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
        let Some(events) = &self.events else {
            return Ok(None);
        };
        match events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                // Pipes may close before the process exits. Never block the UI.
                let status = match self.child.as_mut() {
                    Some(child) => child.try_wait()?,
                    None => return Ok(None),
                };
                if let Some(status) = status {
                    self.child.take();
                    self.events.take();
                    Ok(Some(Event::Exited(status)))
                } else {
                    Ok(None)
                }
            }
        }
    }

    pub fn cancel(&mut self) -> io::Result<()> {
        // Release blocked pipe senders before terminating the process.
        self.events.take();
        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                if let Err(error) = child.kill() {
                    // It may have exited between try_wait and kill.
                    if child.try_wait()?.is_none() {
                        return Err(error);
                    }
                }
            }
            child.wait()?;
            self.child.take();
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

fn pipe(mut input: impl Read + Send + 'static, sender: SyncSender<Event>, diagnostic: bool) {
    std::thread::spawn(move || loop {
        let mut bytes = vec![0; CHUNK_BYTES];
        match input.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => {
                bytes.truncate(count);
                let event = if diagnostic {
                    Event::Diagnostic(bytes)
                } else {
                    Event::Data(bytes)
                };
                if sender.send(event).is_err() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = sender.send(Event::ReadError(error));
                break;
            }
        }
    });
}

#[cfg(test)]
#[path = "kubernetes_tests.rs"]
mod tests;
