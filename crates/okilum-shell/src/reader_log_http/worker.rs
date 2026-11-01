use super::model::Source;
use anyhow::Result;
use okilum_core::log::{
    http::{Error, HttpLog, Status},
    stream::{Assembler, Snapshot},
};
use std::{
    sync::{mpsc, Arc},
    time::Duration,
};

pub(super) struct Frame {
    pub data: Option<Arc<Snapshot>>,
    pub message: String,
    pub active: bool,
}
pub(super) trait Input: Send {
    fn poll(&mut self) -> Result<(Vec<u8>, Status), Error>;
}
impl Input for HttpLog {
    fn poll(&mut self) -> Result<(Vec<u8>, Status), Error> {
        let bytes = HttpLog::poll(self)?;
        Ok((bytes, self.status().clone()))
    }
}
pub(super) type Factory = Arc<dyn Fn(&str) -> Result<Box<dyn Input>> + Send + Sync>;
pub(super) fn factory() -> Factory {
    Arc::new(|url| Ok(Box::new(HttpLog::open(url)?)))
}

/// Owns one bounded-request worker. Stop/drop interrupts waiting and suppresses
/// replies; an already-running HTTPS request finishes within its 10s deadline.
pub(super) struct Worker {
    cancel: mpsc::Sender<()>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
    }
}
impl Worker {
    pub fn start(
        source: Source,
        factory: Factory,
    ) -> Result<(Self, async_channel::Receiver<Frame>)> {
        let (cancel, stop) = mpsc::channel();
        let (events, receiver) = async_channel::bounded(1);
        std::thread::Builder::new().name("https-log-follow".into()).spawn(move || {
            let scope = uuid::Uuid::new_v4().to_string();
            let setup = || -> Result<_> { Ok((factory(&source.url)?, Assembler::new(source.name.clone().into(), scope)?)) };
            let (mut input, mut assembler) = match setup() {
                Ok(value) => value,
                Err(error) => { let _ = events.send_blocking(Frame { data: None, message: format!("Stopped: {error:#}"), active: false }); return; }
            };
            loop {
                if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) { break; }
                let result = input.poll();
                if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) { break; }
                let (data, message, wait, active) = match result {
                    Ok((bytes, status)) => {
                        let data = match assembler.append(&bytes) {
                            Ok(data) => data,
                            Err(error) => {
                                let _ = events.send_blocking(Frame { data: None, message: format!("Stopped: {error:#}. Only previously loaded rows are shown."), active: false });
                                break;
                            }
                        };
                        let pending = assembler.pending_bytes();
                        match status {
                            Status::Connected => (data, format!("Connected · {} bytes awaiting newline", pending), Duration::from_millis(500), true),
                            Status::Disconnected { retry_after } => (data, format!("Disconnected · reconnecting in {}s · {} bytes awaiting newline", retry_after.as_secs(), pending), retry_after, true),
                            Status::Stopped(error) => (data, format!("Stopped: {error}"), Duration::ZERO, false),
                        }
                    }
                    Err(error) => (None, format!("Stopped: {error}"), Duration::ZERO, false),
                };
                if events.send_blocking(Frame { data, message, active }).is_err() || !active { break; }
                match stop.recv_timeout(wait) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
        })?;
        Ok((Self { cancel }, receiver))
    }
}
