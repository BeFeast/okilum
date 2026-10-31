//! T3 protocol v2 transport for the Linux native client (#1787).
//!
//! A subscription stays open across chunks. Run blocking operations on a
//! background executor. The caller owns the projection and commits each chunk
//! before its ACK; reconnect uses only that committed projection's cursor.
//! No worker, timer, automatic sign-in, command retry, or projection is hidden
//! here. The Linux UI will adopt this boundary with the projection slice;
//! the existing MCP gateway and macOS webview keep their own credentials.

mod auth;
mod manager;
mod protocol;
pub use manager::ManagerThread;
pub mod shell;

#[cfg(test)]
mod tests;

use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

use crate::Environment;

pub use auth::NativeToken;
pub use protocol::ThreadUpdate;

/// Transport errors deliberately exclude HTTP bodies, socket URLs and peer
/// error payloads: each may echo a pairing token, bearer or ticket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamError {
    PairingRequired,
    Expired,
    WrongOrigin,
    MissingReadPermission,
    Http(u16),
    Unreachable,
    Protocol(&'static str),
    IncompatibleProtocol,
    Closed,
    Rejected,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PairingRequired => write!(f, "Pair the native T3 client with this server; MCP sign-in cannot authorize WebSocket access"),
            Self::Expired => write!(f, "The native T3 session has expired; pair again"),
            Self::WrongOrigin => write!(f, "The native T3 session belongs to another server"),
            Self::MissingReadPermission => write!(f, "The native T3 session needs orchestration:read permission"),
            Self::Http(status) => write!(f, "T3 returned HTTP {status}"),
            Self::Unreachable => write!(f, "The T3 stream is unreachable"),
            Self::Protocol(detail) => write!(f, "Unexpected T3 stream response: {detail}"),
            Self::IncompatibleProtocol => write!(f, "This T3 server does not support orchestration protocol v2"),
            Self::Closed => write!(f, "The T3 stream closed; reconnect with the retained projection"),
            Self::Rejected => write!(f, "T3 refused the thread subscription; open the thread in T3"),
        }
    }
}

impl std::error::Error for StreamError {}
pub type StreamResult<T> = Result<T, StreamError>;

/// Belongs to a retained projection, not a durable delivery/replay ledger.
/// Discard it when discarding that projection. Its private fields prevent a
/// cursor from being constructed from an uncommitted or unrelated snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeCursor {
    origin: String,
    thread_id: String,
    sequence: u64,
}

impl ResumeCursor {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// Origin-bound native bearer, distinct from the MCP OAuth audience.
pub struct NativeClient {
    environment: Environment,
    token: NativeToken,
    http: reqwest::blocking::Client,
    timeout: Duration,
    access: crate::NetworkAccess,
}

impl NativeClient {
    pub fn history(
        &self,
        thread_id: &str,
        cursor: &str,
    ) -> StreamResult<crate::projection::HistoryPage> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        self.token.validate(&self.environment, crate::unix_now())?;
        auth::history(
            &self.http,
            &self.environment,
            &self.token,
            thread_id,
            cursor,
        )
    }
    /// `timeout` bounds HTTP, connect, socket I/O and heartbeat waits. DNS
    /// resolution uses the platform resolver. Never call on the UI thread.
    pub fn new(
        environment: Environment,
        token: NativeToken,
        timeout: Duration,
    ) -> StreamResult<Self> {
        token.validate(&environment, crate::unix_now())?;
        if timeout.is_zero() {
            return Err(StreamError::Protocol("zero network timeout"));
        }
        let http = auth::http(timeout)?;
        Ok(Self {
            environment,
            token,
            http,
            timeout,
            access: crate::NetworkAccess::current(),
        })
    }

    pub fn with_access(mut self, access: crate::NetworkAccess) -> Self {
        self.access = access;
        self
    }

    /// Whether the app's module lease still permits this client to connect.
    pub fn access_allowed(&self) -> bool {
        self.access.allowed()
    }

    pub fn can_operate(&self) -> bool {
        self.access.allowed()
            && self.token.can_operate()
            && self
                .token
                .validate(&self.environment, crate::unix_now())
                .is_ok()
    }

    /// One bounded attempt on a dedicated RPC socket. The live subscription is
    /// never interrupted by command traffic. No implicit retry or MCP fallback.
    pub fn dispatch(&self, command: &crate::actions::Command) -> StreamResult<u64> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        self.token.validate(&self.environment, crate::unix_now())?;
        if !self.token.can_operate() {
            return Err(StreamError::Protocol(
                "native session needs orchestration:operate permission",
            ));
        }
        let payload = command.wire()?;
        let (socket, cancel) = self.open_socket()?;
        let mut rpc = ThreadSubscription {
            socket,
            cancel,
            origin: self.environment.origin(),
            thread_id: command.thread_id.clone(),
            cursor: None,
            closed: false,
            access: self.access.clone(),
        };
        rpc.send(json!({"_tag":"Request","id":"1","tag":"orchestration.dispatchCommand","payload":payload,"headers":[]}))?;
        let deadline = Instant::now() + self.timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(StreamError::Closed);
            }
            let value: Value = match rpc.socket.read().map_err(|_| StreamError::Closed)? {
                Message::Text(text) => serde_json::from_str(&text)
                    .map_err(|_| StreamError::Protocol("invalid command response"))?,
                Message::Ping(_) => {
                    rpc.socket.flush().map_err(|_| StreamError::Closed)?;
                    continue;
                }
                Message::Pong(_) => continue,
                _ => return Err(StreamError::Closed),
            };
            match value["_tag"].as_str() {
                Some("Ping") => rpc.send(json!({"_tag":"Pong"}))?,
                Some("Pong") => {}
                Some("Exit") if value["requestId"] == "1" => {
                    if value["exit"]["_tag"] != "Success" {
                        return Err(StreamError::Rejected);
                    }
                    return value["exit"]["value"]["sequence"]
                        .as_u64()
                        .filter(|v| *v <= 9_007_199_254_740_991)
                        .ok_or(StreamError::Protocol("invalid command acknowledgement"));
                }
                _ => return Err(StreamError::Protocol("unexpected command response")),
            }
        }
    }

    fn open_socket(&self) -> StreamResult<(WebSocket<MaybeTlsStream<TcpStream>>, TcpStream)> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        let ticket = auth::ticket(&self.http, &self.environment, &self.token)?;
        let mut url = self.environment.endpoint("/ws");
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|_| StreamError::Protocol("socket scheme"))?;
        url.query_pairs_mut()
            .append_pair("orchestrationProtocol", "2")
            .append_pair("wsTicket", &ticket);
        let host = self.environment.host();
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        let addresses = (host.as_str(), url.port_or_known_default().unwrap_or(80))
            .to_socket_addrs()
            .map_err(|_| StreamError::Unreachable)?;
        let deadline = Instant::now() + self.timeout;
        let mut connected = None;
        for address in addresses {
            if !self.access.allowed() {
                return Err(StreamError::Closed);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            if let Ok(stream) = TcpStream::connect_timeout(&address, remaining) {
                connected = Some(stream);
                break;
            }
        }
        let stream = connected.ok_or(StreamError::Unreachable)?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|_| StreamError::Unreachable)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|_| StreamError::Unreachable)?;
        let cancel = stream.try_clone().map_err(|_| StreamError::Unreachable)?;
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        // subscribeShell includes the server directory before client-side project
        // filtering. Real servers can exceed tungstenite's 16 MiB frame default;
        // retain its 64 MiB message budget while admitting one unfragmented frame.
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_frame_size(Some(64 * 1024 * 1024));
        let (socket, _) =
            tungstenite::client_tls_with_config(url.as_str(), stream, Some(config), None).map_err(
                |error| match error {
                    tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response))
                        if response.status().as_u16() == 426 =>
                    {
                        StreamError::IncompatibleProtocol
                    }
                    tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response))
                        if response.status().as_u16() == 401 =>
                    {
                        StreamError::PairingRequired
                    }
                    tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)) => {
                        StreamError::Http(response.status().as_u16())
                    }
                    _ => StreamError::Unreachable,
                },
            )?;
        Ok((socket, cancel))
    }

    /// One explicit connection attempt. Every reconnect obtains a new ticket;
    /// neither a refused native session nor a network error starts OAuth.
    pub fn subscribe(
        &self,
        thread_id: &str,
        resume: Option<ResumeCursor>,
    ) -> StreamResult<ThreadSubscription> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        self.token.validate(&self.environment, crate::unix_now())?;
        if thread_id.trim().is_empty() {
            return Err(StreamError::Protocol("empty thread id"));
        }
        if resume.as_ref().is_some_and(|cursor| {
            cursor.origin != self.environment.origin() || cursor.thread_id != thread_id
        }) {
            return Err(StreamError::Protocol(
                "resume cursor belongs to another thread or server",
            ));
        }
        let (socket, cancel) = self.open_socket()?;
        let mut subscription = ThreadSubscription {
            socket,
            cancel,
            origin: self.environment.origin(),
            thread_id: thread_id.into(),
            cursor: resume,
            closed: false,
            access: self.access.clone(),
        };
        let mut payload = json!({"threadId":thread_id,"requestCompletionMarker":true,"acceptBoundedSnapshot":true});
        if let Some(cursor) = &subscription.cursor {
            payload["afterSequence"] = cursor.sequence.into();
        }
        subscription.send(json!({"_tag":"Request","id":"1","tag":"orchestration.subscribeThread","payload":payload,"headers":[]}))?;
        Ok(subscription)
    }
}

/// A cancellation handle can wake a blocked background receive immediately.
/// Cancellation does not dispatch `run.interrupt` or stop the remote agent.
pub struct CancelSubscription(TcpStream);

impl CancelSubscription {
    pub fn cancel(&self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

pub struct ThreadSubscription {
    access: crate::NetworkAccess,
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    cancel: TcpStream,
    origin: String,
    thread_id: String,
    cursor: Option<ResumeCursor>,
    closed: bool,
}

impl ThreadSubscription {
    pub fn cancellation(&self) -> StreamResult<CancelSubscription> {
        self.cancel
            .try_clone()
            .map(CancelSubscription)
            .map_err(|_| StreamError::Unreachable)
    }

    pub fn cursor(&self) -> Option<ResumeCursor> {
        self.cursor.clone()
    }

    /// Receives one chunk, preserving event boundaries and unknown event
    /// payloads for the next slice's reducer. `apply` must atomically accept
    /// the whole batch (or retain its previous projection on error).
    ///
    /// A successful apply commits the cursor even if writing the ACK fails:
    /// that projection can resume safely. A rejected batch closes the socket
    /// without acknowledging or advancing it. No unbounded queue is allocated.
    pub fn receive(
        &mut self,
        apply: impl FnOnce(&[ThreadUpdate]) -> StreamResult<()>,
    ) -> StreamResult<()> {
        if self.closed {
            return Err(StreamError::Closed);
        }
        let result = self.receive_inner(apply);
        if result.is_err() {
            self.closed = true;
            let _ = self.cancel.shutdown(Shutdown::Both);
        }
        result
    }

    fn receive_inner(
        &mut self,
        apply: impl FnOnce(&[ThreadUpdate]) -> StreamResult<()>,
    ) -> StreamResult<()> {
        let value = self.read_chunk()?;
        let (updates, sequence) = protocol::decode_chunk(
            &value,
            &self.thread_id,
            self.cursor.as_ref().map(|cursor| cursor.sequence),
        )?;
        apply(&updates)?;
        self.cursor = sequence.map(|sequence| ResumeCursor {
            origin: self.origin.clone(),
            thread_id: self.thread_id.clone(),
            sequence,
        });
        self.send(json!({"_tag":"Ack","requestId":"1"}))
    }

    fn read_chunk(&mut self) -> StreamResult<Value> {
        // Effect RPC clients initiate keepalives (server replies with Pong).
        self.send(json!({"_tag":"Ping"}))?;
        let mut awaiting_pong = true;
        loop {
            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) && !awaiting_pong =>
                {
                    self.send(json!({"_tag":"Ping"}))?;
                    awaiting_pong = true;
                    continue;
                }
                Err(_) => return Err(StreamError::Closed),
            };
            let value: Value = match message {
                Message::Text(text) => serde_json::from_str(&text)
                    .map_err(|_| StreamError::Protocol("invalid JSON"))?,
                Message::Ping(_) => {
                    self.socket.flush().map_err(|_| StreamError::Closed)?;
                    continue;
                }
                Message::Pong(_) => continue,
                Message::Close(_) => return Err(StreamError::Closed),
                _ => return Err(StreamError::Protocol("unsupported frame")),
            };
            match value["_tag"].as_str() {
                Some("Pong") => {
                    awaiting_pong = false;
                    continue;
                }
                Some("Ping") => {
                    self.send(json!({"_tag":"Pong"}))?;
                    continue;
                }
                Some("Exit") if value["requestId"] == "1" => {
                    return Err(if value["exit"]["_tag"] == "Success" {
                        StreamError::Closed
                    } else {
                        StreamError::Rejected
                    })
                }
                Some("Chunk") if value["requestId"] == "1" => {
                    return Ok(value);
                }
                _ => return Err(StreamError::Protocol("unexpected RPC response")),
            }
        }
    }

    fn send(&mut self, value: Value) -> StreamResult<()> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        self.socket
            .send(Message::Text(value.to_string().into()))
            .map_err(|_| StreamError::Closed)
    }

    /// Call off the UI thread; `cancellation().cancel()` is the immediate
    /// alternative when a background receiver owns this subscription.
    pub fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            let _ = self.send(json!({"_tag":"Interrupt","requestId":"1","interruptors":[]}));
            let _ = self.socket.close(None);
            let _ = self.cancel.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for ThreadSubscription {
    fn drop(&mut self) {
        // Ownership can end on the UI thread. Never flush protocol frames or
        // perform a close handshake here; graceful close is explicit. Closing
        // the TCP stream cancels the RPC without stopping the remote agent.
        let _ = self.cancel.shutdown(Shutdown::Both);
    }
}
