//! Deadline-bounded wire I/O on an already connected private client. Not yet a
//! Transport: authenticated discovery/open and supervisor wiring remain separate.
use super::{
    windows_endpoint::{PrivateClient, PrivatePipe},
    windows_peer::{PeerEnd, ProcessPeer},
    Scope, MAX_FRAME,
};
use std::{
    io::{self, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{HRESULT, PCWSTR},
    Win32::{
        Foundation::{ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Storage::FileSystem::{ReadFile, WriteFile},
        System::{
            Pipes::ConnectNamedPipe,
            Threading::{CreateEventW, WaitForSingleObject},
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        },
    },
};

#[cfg(test)]
#[derive(Default)]
struct Evidence {
    pending: AtomicUsize,
    cancelled: AtomicUsize,
}
const MAX_WORKERS: usize = 8;
static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Permit {
    fn acquire() -> io::Result<Self> {
        WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_WORKERS).then_some(n + 1)
            })
            .map(|_| Self)
            .map_err(|_| io::Error::other("pipe I/O worker limit reached"))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
fn expired() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "supervisor I/O deadline elapsed")
}
fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(expired)
}
fn native(api: &str, error: windows::core::Error) -> io::Error {
    io::Error::other(format!("{api}: {error}"))
}

enum Operation {
    Verify,
    Accept,
    Read(usize),
    Write(Vec<u8>),
}
struct Reply {
    bytes: Vec<u8>,
    count: usize,
}
struct Request {
    operation: Operation,
    reply: mpsc::SyncSender<io::Result<Reply>>,
}

enum Endpoint {
    Client(PrivateClient),
    Server {
        pipe: PrivatePipe,
        peer: ProcessPeer,
    },
}
impl Endpoint {
    fn handle(&self) -> HANDLE {
        match self {
            Self::Client(p) => HANDLE(p.as_raw_handle()),
            Self::Server { pipe, .. } => HANDLE(pipe.as_raw_handle()),
        }
    }
    fn verify(&self, before_accept: bool) -> io::Result<()> {
        match self {
            Self::Client(p) => p.verify(),
            Self::Server { pipe, peer } => pipe.verify().and_then(|()| {
                if before_accept {
                    Ok(())
                } else {
                    peer.verify_pipe(pipe, PeerEnd::Client)
                }
            }),
        }
        .map_err(io::Error::other)
    }
}

/// One owned server connection. No reaccept, disconnect/reuse, or permissive
/// peer discovery: the expected client comes from trusted captured-process setup.
pub struct ServerIo(ClientIo);
impl ServerIo {
    pub fn accept(pipe: PrivatePipe, peer: ProcessPeer, deadline: Instant) -> io::Result<Self> {
        let mut io = ClientIo::start(Endpoint::Server { pipe, peer }, deadline)?;
        io.request(Operation::Accept)?;
        Ok(Self(io))
    }
}
impl Read for ServerIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}
impl Write for ServerIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Exclusive client ownership and one immutable deadline for every fragment.
/// After any error the connection is poisoned; there is no retry/reset API.
/// Drop never joins a possibly delayed kernel cancellation on the caller thread.
pub struct ClientIo {
    requests: mpsc::SyncSender<Request>,
    deadline: Instant,
    failed: bool,
    #[cfg(test)]
    completed: mpsc::Receiver<()>,
    #[cfg(test)]
    evidence: std::sync::Arc<Evidence>,
}
impl ClientIo {
    pub fn new(client: PrivateClient, deadline: Instant) -> io::Result<Self> {
        Self::start(Endpoint::Client(client), deadline)
    }
    /// Open and verify on the admitted worker. The caller's wait shares the I/O
    /// deadline; a stalled synchronous open retains its worker admission slot.
    /// Thread creation itself is still synchronous and outside this guarantee.
    pub fn connect(scope: Scope, peer: ProcessPeer, deadline: Instant) -> io::Result<Self> {
        let mut io = Self::start_with(
            move || {
                PrivateClient::connect(&scope, peer)
                    .map(Endpoint::Client)
                    .map_err(io::Error::other)
            },
            deadline,
        )?;
        io.request(Operation::Verify)?;
        Ok(io)
    }
    fn start(endpoint: Endpoint, deadline: Instant) -> io::Result<Self> {
        Self::start_with(move || Ok(endpoint), deadline)
    }
    fn start_with(
        factory: impl FnOnce() -> io::Result<Endpoint> + Send + 'static,
        deadline: Instant,
    ) -> io::Result<Self> {
        let budget = remaining(deadline)?;
        if budget > Duration::from_secs(30) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "I/O budget exceeds 30 seconds",
            ));
        }
        let permit = Permit::acquire()?;
        #[cfg(test)]
        let evidence = std::sync::Arc::new(Evidence::default());
        #[cfg(test)]
        let worker_evidence = evidence.clone();
        #[cfg(test)]
        let (done, completed) = mpsc::sync_channel(1);
        let (requests, receiver) = mpsc::sync_channel::<Request>(1);
        std::thread::Builder::new()
            .name("sync-pipe-io".into())
            .spawn(move || {
                let _permit = permit;
                let endpoint = remaining(deadline).and_then(|_| factory());
                while let Ok(wait) = remaining(deadline) {
                    let Ok(request) = receiver.recv_timeout(wait) else {
                        break;
                    };
                    // Queries are synchronous; caller waiting remains bounded even
                    // if a query stalls. Admission includes such retained workers.
                    let accepting = matches!(request.operation, Operation::Accept);
                    let verifying = matches!(request.operation, Operation::Verify);
                    let result = endpoint
                        .as_ref()
                        .map_err(|error| io::Error::new(error.kind(), error.to_string()))
                        .and_then(|endpoint| {
                            endpoint.verify(accepting)?;
                            remaining(deadline)?;
                            Ok(endpoint)
                        })
                        .and_then(|endpoint| {
                            if verifying {
                                return Ok(Reply {
                                    bytes: Vec::new(),
                                    count: 0,
                                });
                            }
                            perform_inner(
                                endpoint.handle(),
                                request.operation,
                                deadline,
                                #[cfg(test)]
                                Some(&worker_evidence),
                            )
                            .and_then(|reply| {
                                if accepting {
                                    endpoint.verify(false)?;
                                }
                                remaining(deadline)?;
                                Ok(reply)
                            })
                        });
                    let stop = result.is_err();
                    let _ = request.reply.send(result);
                    if stop {
                        break;
                    }
                }
                drop(endpoint);
                drop(_permit);
                #[cfg(test)]
                let _ = done.send(());
            })?;
        Ok(Self {
            requests,
            deadline,
            failed: false,
            #[cfg(test)]
            completed,
            #[cfg(test)]
            evidence,
        })
    }
    fn request(&mut self, operation: Operation) -> io::Result<Reply> {
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pipe I/O is poisoned",
            ));
        }
        let result = (|| {
            remaining(self.deadline)?;
            let (reply, response) = mpsc::sync_channel(1);
            self.requests
                .try_send(Request { operation, reply })
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "pipe worker unavailable")
                })?;
            let response =
                response
                    .recv_timeout(remaining(self.deadline)?)
                    .map_err(|e| match e {
                        mpsc::RecvTimeoutError::Timeout => expired(),
                        mpsc::RecvTimeoutError::Disconnected => {
                            io::Error::new(io::ErrorKind::BrokenPipe, "pipe worker exited")
                        }
                    })?;
            // Never report a late successful completion as an in-budget reply.
            remaining(self.deadline)?;
            response
        })();
        self.failed |= result.is_err();
        result
    }
}
impl Read for ClientIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let reply = self.request(Operation::Read(bytes.len().min(MAX_FRAME)))?;
        bytes[..reply.count].copy_from_slice(&reply.bytes[..reply.count]);
        Ok(reply.count)
    }
}
impl Write for ClientIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        Ok(self
            .request(Operation::Write(
                bytes[..bytes.len().min(MAX_FRAME)].to_vec(),
            ))?
            .count)
    }
    fn flush(&mut self) -> io::Result<()> {
        // No userspace write buffer. FlushFileBuffers can wait for a malicious
        // reader and is intentionally not used as an acknowledgement of delivery.
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pipe I/O is poisoned",
            ));
        }
        remaining(self.deadline).map(|_| ())
    }
}

#[cfg(test)]
fn perform(pipe: HANDLE, operation: Operation, deadline: Instant) -> io::Result<Reply> {
    perform_inner(pipe, operation, deadline, None)
}
fn perform_inner(
    pipe: HANDLE,
    operation: Operation,
    deadline: Instant,
    #[cfg(test)] evidence: Option<&Evidence>,
) -> io::Result<Reply> {
    remaining(deadline)?;
    let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .map_err(|e| native("CreateEventW", e))?;
    let _event = unsafe { OwnedHandle::from_raw_handle(event.0) };
    let mut overlap = OVERLAPPED {
        hEvent: event,
        ..Default::default()
    };
    let accepting = matches!(operation, Operation::Accept);
    let read = matches!(operation, Operation::Read(_));
    let mut bytes = match operation {
        Operation::Verify => return Err(io::Error::other("verification is not a wire operation")),
        Operation::Accept => Vec::new(),
        Operation::Read(n) => vec![0; n],
        Operation::Write(bytes) => bytes,
    };
    let submitted = unsafe {
        if accepting {
            ConnectNamedPipe(pipe, Some(&mut overlap))
        } else if read {
            ReadFile(pipe, Some(&mut bytes), None, Some(&mut overlap))
        } else {
            WriteFile(pipe, Some(&bytes), None, Some(&mut overlap))
        }
    };
    if let Err(error) = submitted {
        if accepting && error.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) {
            remaining(deadline)?;
            return Ok(Reply { bytes, count: 0 });
        }
        if error.code() != HRESULT::from_win32(ERROR_IO_PENDING.0) {
            return Err(native(
                if accepting {
                    "ConnectNamedPipe"
                } else if read {
                    "ReadFile"
                } else {
                    "WriteFile"
                },
                error,
            ));
        }
        #[cfg(test)]
        if let Some(evidence) = evidence {
            evidence.pending.fetch_add(1, Ordering::Relaxed);
        }
    }
    // From submission until terminal completion, no early return may release
    // bytes/OVERLAPPED/event/pipe. Cancellation alone is not completion.
    let mut count = 0;
    let wait = match remaining(deadline) {
        Ok(left) => unsafe {
            WaitForSingleObject(
                event,
                left.as_millis().max(1).min(u32::MAX as u128 - 1) as u32,
            )
        },
        Err(_) => WAIT_TIMEOUT,
    };
    if wait != WAIT_OBJECT_0 {
        unsafe {
            let _ = CancelIoEx(pipe, Some(&overlap));
            // May outlive caller deadline; this worker keeps all resources and
            // its admission permit until Windows confirms terminal completion.
            let completion = GetOverlappedResult(pipe, &overlap, &mut count, true);
            #[cfg(test)]
            if let (Some(evidence), Err(error)) = (evidence, &completion) {
                if error.code()
                    == HRESULT::from_win32(windows::Win32::Foundation::ERROR_OPERATION_ABORTED.0)
                {
                    evidence.cancelled.fetch_add(1, Ordering::Relaxed);
                }
            }
            let _ = completion;
        }
        return Err(if wait == WAIT_TIMEOUT {
            expired()
        } else {
            io::Error::other("pipe event wait failed")
        });
    }
    unsafe { GetOverlappedResult(pipe, &overlap, &mut count, true) }
        .map_err(|e| native("GetOverlappedResult", e))?;
    remaining(deadline)?;
    // ConnectNamedPipe has no byte transfer; GetOverlappedResult does not define
    // its byte count. Only ReadFile/WriteFile completion lengths are meaningful.
    if accepting {
        return Ok(Reply { bytes, count: 0 });
    }
    if count as usize > bytes.len() {
        return Err(io::Error::other("invalid pipe completion length"));
    }
    Ok(Reply {
        bytes,
        count: count as usize,
    })
}

#[cfg(test)]
mod tests;
