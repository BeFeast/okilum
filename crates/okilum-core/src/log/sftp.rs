//! Read-only SFTP v3 over the user's OpenSSH configuration (#1390).
//!
//! This adapter deliberately stops on a changed consumed prefix: SFTP v3 has no
//! portable inode identity and cannot recover an unobserved rotated-away file.
//! Callers must display `ContinuityLost`, rather than imply gap-free recovery.
//!
//! A follow keeps one SFTP connection while it stays healthy. Growth is read
//! with an overlap of already consumed bytes, so an in-place truncation or
//! rewrite at the cursor is noticed in the same request. At EOF the path is
//! reopened on that connection (rotation check), and after a reopen or a new
//! connection the retained proof window (the last 64 KiB plus any unfinished
//! record) is verified before more bytes are accepted.
use crate::ssh_askpass::{self, Clock, Deadline};
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

pub mod directory;

const CHUNK: usize = 32 * 1024;
const PACKET_LIMIT: usize = CHUNK + 4096;
/// Consumed bytes re-verified after a reopen or reconnect.
const PROOF: usize = 64 * 1024;
/// Consumed bytes re-read in front of each growth read while attached.
const OVERLAP: usize = 4 * 1024;
/// An unfinished record (no newline yet) is retained up to this size; beyond
/// it the follow stops explicitly instead of evicting or skipping bytes.
pub const RECORD_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    host: String,
    user: Option<String>,
    port: Option<u16>,
    path: Vec<u8>,
    /// Remote Markdown (#1610) lets OpenSSH ask for a passphrase, password or
    /// unknown-host trust through the app's askpass bridge. Log following
    /// reconnects on every poll and stays non-interactive.
    interactive: bool,
}
impl Target {
    /// Accept an SSH config alias and absolute path. Authentication stays in
    /// OpenSSH/ssh-agent; passwords, URL userinfo and URL options are rejected.
    pub fn parse(url: &str) -> io::Result<Self> {
        let mut target = Self::parse_login(url)?;
        target.interactive = false;
        if target.user.is_some() || target.port.is_some() {
            return Err(invalid(
                "Use an SSH config host alias without credentials or URL options",
            ));
        }
        Ok(target)
    }
    /// `sftp://[user@]host[:port]/path` for explicit remote Markdown links.
    /// The host may be an SSH config alias; a URL password is always refused.
    /// The resulting commands may ask through the askpass bridge.
    pub fn parse_login(url: &str) -> io::Result<Self> {
        let (authority, path) = url
            .strip_prefix("sftp://")
            .and_then(|rest| rest.split_once('/'))
            .ok_or_else(|| invalid("Expected sftp://host/path"))?;
        let refused = || {
            invalid(
                "Use an SSH config host alias or user@host:port, without a password or URL options",
            )
        };
        let (user, host) = match authority.rsplit_once('@') {
            Some((user, host)) => {
                if user.is_empty()
                    || user.len() > 64
                    || user.starts_with('-')
                    || !user
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
                {
                    return Err(refused());
                }
                (Some(user.to_owned()), host)
            }
            None => (None, authority),
        };
        let (host, port) = match host.split_once(':') {
            Some((host, port)) => {
                let port = (!port.is_empty()
                    && port.len() <= 5
                    && port.bytes().all(|b| b.is_ascii_digit()))
                .then(|| port.parse::<u16>().ok())
                .flatten()
                .filter(|port| *port != 0)
                .ok_or_else(refused)?;
                (host, Some(port))
            }
            None => (host, None),
        };
        if path.len() > 4096
            || host.len() > 253
            || host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
            || host.starts_with('-')
            || path.contains(['?', '#'])
        {
            return Err(refused());
        }
        let mut decoded = vec![b'/'];
        let mut bytes = path.bytes();
        while let Some(b) = bytes.next() {
            decoded.push(if b == b'%' {
                let hi = bytes
                    .next()
                    .and_then(hex)
                    .ok_or_else(|| invalid("Invalid path escape"))?;
                let lo = bytes
                    .next()
                    .and_then(hex)
                    .ok_or_else(|| invalid("Invalid path escape"))?;
                hi * 16 + lo
            } else {
                b
            });
        }
        if decoded.contains(&0) {
            return Err(invalid("NUL in remote path"));
        }
        Ok(Self {
            host: host.into(),
            user,
            port,
            path: decoded,
            interactive: true,
        })
    }
    /// The askpass session (if any) must live exactly as long as the child.
    fn command(&self, clock: &Arc<Clock>) -> (Command, Option<ssh_askpass::Session>) {
        let (mut command, session) = self.ssh_command(clock);
        command.args(["-s", "--", &self.host, "sftp"]);
        (command, session)
    }
    /// Trusted, fixed program only. Remote paths and content belong on stdin,
    /// never in shell arguments. Used only by explicit guarded Markdown Save.
    pub(crate) fn guarded_markdown_save_command(
        &self,
        program: &'static str,
        clock: &Arc<Clock>,
    ) -> (Command, Option<ssh_askpass::Session>) {
        let (mut command, session) = self.ssh_command(clock);
        command.args(["--", &self.host, program]);
        (command, session)
    }
    pub(crate) fn remote_path(&self) -> &[u8] {
        &self.path
    }
    fn ssh_command(&self, clock: &Arc<Clock>) -> (Command, Option<ssh_askpass::Session>) {
        let mut command = Command::new("ssh");
        command.arg("-T");
        // Without the askpass bridge OpenSSH must never wait for an answer.
        // With it, agent/keys still go first; only OpenSSH's own questions
        // reach the app, and a changed host key is still refused.
        let session = if self.interactive {
            ssh_askpass::configure(&mut command, clock)
        } else {
            None
        };
        if session.is_some() {
            command.args(["-oBatchMode=no", "-oStrictHostKeyChecking=ask"]);
        } else {
            command.args(["-oBatchMode=yes", "-oStrictHostKeyChecking=yes"]);
        }
        command.args([
            "-oConnectTimeout=10",
            "-oServerAliveInterval=5",
            "-oServerAliveCountMax=2",
        ]);
        if let Some(user) = &self.user {
            command.args(["-l", user]);
        }
        if let Some(port) = self.port {
            command.args(["-p", &port.to_string()]);
        }
        (command, session)
    }
}
fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|n| n as u8)
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}
fn take_u32(input: &mut &[u8]) -> io::Result<u32> {
    if input.len() < 4 {
        return Err(invalid("Short SFTP integer"));
    }
    let value = u32::from_be_bytes(input[..4].try_into().unwrap());
    *input = &input[4..];
    Ok(value)
}
fn take_string<'a>(input: &mut &'a [u8]) -> io::Result<&'a [u8]> {
    let len = take_u32(input)? as usize;
    if len > input.len() {
        return Err(invalid("Short SFTP string"));
    }
    let (value, rest) = input.split_at(len);
    *input = rest;
    Ok(value)
}
fn packet(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > PACKET_LIMIT {
        return Err(invalid("SFTP packet exceeds limit"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Injectable positional transport; offsets count bytes, never UTF-8 characters.
pub trait Transport {
    fn open(&mut self, target: &Target) -> io::Result<()>;
    fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>>;
    fn disconnect(&mut self);
    /// Open `target` again for reading, on the live connection when there is
    /// one, so a follow can notice rotation without a new SSH handshake.
    fn reopen(&mut self, target: &Target) -> io::Result<()> {
        self.open(target)
    }
    /// Size of the open file, when the server reports one.
    fn size(&mut self) -> io::Result<Option<u64>> {
        Ok(None)
    }
    /// Clock of this transport's askpass dialogs: operation deadlines pause
    /// while one of its own connections waits for the person, never for others.
    fn prompt_clock(&self) -> Option<Arc<Clock>> {
        None
    }
}
#[derive(Default)]
pub struct OpenSsh {
    session: Option<Session>,
    clock: Arc<Clock>,
    cancel: Option<Arc<AtomicBool>>,
}
impl OpenSsh {
    /// Setting `cancel` aborts a waiting request at once: the ssh process is
    /// killed, its askpass dialog closed and the worker released.
    pub fn cancellable(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel: Some(cancel),
            ..Self::default()
        }
    }
    pub(crate) fn spawn(&mut self, target: &Target) -> io::Result<Session> {
        let (mut command, askpass) = target.command(&self.clock);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::sync_channel(1);
        std::thread::spawn(move || loop {
            let result = packet(&mut stdout);
            let failed = result.is_err();
            if sender.send(result).is_err() || failed {
                break;
            }
        });
        Ok(Session {
            child,
            input,
            output,
            id: 0,
            handle: Vec::new(),
            clock: askpass.as_ref().map(|_| self.clock.clone()),
            cancel: self.cancel.clone(),
            _askpass: askpass,
        })
    }
}
pub(crate) struct Session {
    child: Child,
    input: ChildStdin,
    output: Receiver<io::Result<Vec<u8>>>,
    id: u32,
    handle: Vec<u8>,
    /// Present only for an interactive connection.
    clock: Option<Arc<Clock>>,
    cancel: Option<Arc<AtomicBool>>,
    /// Dropped after the child is killed (fields drop after `Drop::drop`).
    _askpass: Option<ssh_askpass::Session>,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Session {
    pub(crate) fn exchange(&mut self, payload: &[u8]) -> io::Result<Vec<u8>> {
        self.input
            .write_all(&(payload.len() as u32).to_be_bytes())?;
        self.input.write_all(payload)?;
        self.input.flush()?;
        // The first response waits for authentication; time spent in this
        // connection's own askpass dialog does not count against the deadline.
        let deadline = Deadline::after(Duration::from_secs(15), self.clock.clone());
        loop {
            if self
                .cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::Acquire))
            {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Remote connection cancelled",
                ));
            }
            match self
                .output
                .recv_timeout(deadline.remaining().min(Duration::from_millis(50)))
            {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) if !deadline.expired() => continue,
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "SFTP response deadline",
                    ))
                }
            }
        }
    }
    fn request(&mut self, kind: u8, payload: &[u8]) -> io::Result<(u8, Vec<u8>)> {
        self.id = self
            .id
            .checked_add(1)
            .ok_or_else(|| invalid("SFTP request ID exhausted"))?;
        let mut request = vec![kind];
        request.extend_from_slice(&self.id.to_be_bytes());
        request.extend_from_slice(payload);
        decode_response(&self.exchange(&request)?, self.id)
    }
}
fn decode_response(response: &[u8], id: u32) -> io::Result<(u8, Vec<u8>)> {
    let (&kind, mut body) = response
        .split_first()
        .ok_or_else(|| invalid("Empty SFTP response"))?;
    if take_u32(&mut body)? != id {
        return Err(invalid("Unexpected SFTP response ID"));
    }
    Ok((kind, body.to_vec()))
}
fn decode_data(kind: u8, mut body: &[u8], limit: usize) -> io::Result<Vec<u8>> {
    match kind {
        103 => {
            let bytes = take_string(&mut body)?;
            if bytes.is_empty() || bytes.len() > limit || !body.is_empty() {
                return Err(invalid("Invalid SFTP DATA length"));
            }
            Ok(bytes.to_vec())
        }
        101 if take_u32(&mut body)? == 1 => Ok(Vec::new()), // SSH_FX_EOF
        _ => Err(invalid("SFTP read failed")),
    }
}
fn open_handle(session: &mut Session, target: &Target) -> io::Result<()> {
    let mut request = Vec::new();
    string(&mut request, &target.path);
    request.extend_from_slice(&1u32.to_be_bytes()); // SSH_FXF_READ only
    request.extend_from_slice(&0u32.to_be_bytes()); // no attributes
    let (kind, body) = session.request(3, &request)?;
    if kind != 102 {
        return Err(invalid("SFTP read-only open failed"));
    }
    session.handle = take_string(&mut body.as_slice())?.to_vec();
    Ok(())
}
fn close_handle(session: &mut Session) -> io::Result<()> {
    let mut request = Vec::new();
    string(&mut request, &std::mem::take(&mut session.handle));
    let (kind, body) = session.request(4, &request)?;
    if kind != 101 || take_u32(&mut body.as_slice())? != 0 {
        return Err(invalid("SFTP close failed"));
    }
    Ok(())
}
/// SSH_FXP_ATTRS: the size is optional and comes first after the flags.
fn decode_size(kind: u8, mut body: &[u8]) -> io::Result<Option<u64>> {
    if kind != 105 {
        return Err(invalid("SFTP stat failed"));
    }
    if take_u32(&mut body)? & 1 == 0 {
        return Ok(None);
    }
    let size = body.get(..8).ok_or_else(|| invalid("Short SFTP size"))?;
    Ok(Some(u64::from_be_bytes(size.try_into().unwrap())))
}
impl Transport for OpenSsh {
    fn open(&mut self, target: &Target) -> io::Result<()> {
        self.disconnect();
        let mut session = self.spawn(target)?;
        let version = session.exchange(&[1, 0, 0, 0, 3])?;
        if version.len() < 5 || version[0] != 2 || version[1..5] != 3u32.to_be_bytes() {
            return Err(invalid("SFTP v3 required"));
        }
        open_handle(&mut session, target)?;
        self.session = Some(session);
        Ok(())
    }
    fn reopen(&mut self, target: &Target) -> io::Result<()> {
        let Some(session) = self.session.as_mut() else {
            return self.open(target);
        };
        let result = close_handle(session).and_then(|()| open_handle(session, target));
        if result.is_err() {
            self.disconnect();
        }
        result
    }
    fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| invalid("SFTP disconnected"))?;
        let mut request = Vec::new();
        string(&mut request, &session.handle);
        request.extend_from_slice(&offset.to_be_bytes());
        let limit = limit.min(CHUNK);
        request.extend_from_slice(&(limit as u32).to_be_bytes());
        let (kind, body) = session.request(5, &request)?;
        decode_data(kind, &body, limit)
    }

    fn size(&mut self) -> io::Result<Option<u64>> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| invalid("SFTP disconnected"))?;
        let mut request = Vec::new();
        string(&mut request, &session.handle);
        let (kind, body) = session.request(8, &request)?; // SSH_FXP_FSTAT
        decode_size(kind, &body)
    }
    fn disconnect(&mut self) {
        self.session = None;
    }
    fn prompt_clock(&self) -> Option<Arc<Clock>> {
        Some(self.clock.clone())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// Complete newline-terminated records only; partial UTF-8 stays pending.
    Records(Vec<u8>),
    CaughtUp,
    /// Read bytes without completing a record; more reads may still be needed.
    Pending,
    Disconnected(String),
    /// A rotation, truncation or rewrite invalidated the consumed prefix.
    /// Existing rows must be retained; restarting is an explicit caller action.
    ContinuityLost,
    LimitReached,
}
/// One read-only following session. Call `poll` on a background worker with
/// caller-owned retry/backoff. A failed request never advances its cursor.
pub struct Follow<T: Transport> {
    target: Target,
    transport: T,
    /// Bytes consumed from the start of the file, partial records included.
    offset: u64,
    /// Consumed bytes ending at `offset`: at least the last `PROOF` bytes and
    /// the whole unfinished record, which starts at `emitted`.
    window: Vec<u8>,
    emitted: usize,
    terminal: bool,
    /// The transport holds a live connection.
    connected: bool,
    /// The open handle is known to continue the consumed bytes.
    attached: bool,
    /// Start this many bytes before the end, at a line boundary (first open).
    tail: Option<u64>,
    started_at: u64,
    /// Bytes up to the first newline after a tail start belong to a line
    /// whose beginning was not read; they are consumed but never emitted.
    skip_line: bool,
}
impl<T: Transport> Follow<T> {
    pub fn new(target: Target, transport: T) -> Self {
        Self {
            target,
            transport,
            offset: 0,
            window: Vec::new(),
            emitted: 0,
            terminal: false,
            connected: false,
            attached: false,
            tail: None,
            started_at: 0,
            skip_line: false,
        }
    }
    /// Begin at the first full line of the last `bytes` of the file instead of
    /// its start, when the server reports a larger size on the first open.
    pub fn tail(mut self, bytes: u64) -> Self {
        self.tail = Some(bytes);
        self
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    /// File offset where reading began; 0 when the whole file is read.
    pub fn started_at(&self) -> u64 {
        self.started_at
    }
    /// Consumed bytes after the last emitted record (no newline yet).
    pub fn unfinished(&self) -> usize {
        self.window.len() - self.emitted
    }
    pub fn poll(&mut self) -> Update {
        if self.terminal {
            return Update::ContinuityLost;
        }
        if self.window.len() - self.emitted >= RECORD_LIMIT {
            self.close();
            return Update::LimitReached;
        }
        match self.read_poll() {
            Ok(Update::ContinuityLost) => {
                self.terminal = true;
                self.close();
                Update::ContinuityLost
            }
            Ok(update) => update,
            Err(error) => {
                self.close();
                Update::Disconnected(error.to_string())
            }
        }
    }
    fn close(&mut self) {
        self.transport.disconnect();
        self.connected = false;
        self.attached = false;
    }
    fn read_poll(&mut self) -> io::Result<Update> {
        let overlap = if self.attached {
            self.window.len().min(OVERLAP)
        } else {
            if self.connected {
                self.transport.reopen(&self.target)?;
            } else {
                self.transport.open(&self.target)?;
                self.connected = true;
            }
            if !self.prove()? {
                return Ok(Update::ContinuityLost);
            }
            if let Some(tail) = self.tail {
                let size = self.transport.size()?;
                self.tail = None;
                if let Some(size) = size.filter(|size| *size > tail) {
                    // One byte early: if it is a newline, the first line
                    // read is complete and only that byte is skipped.
                    self.offset = size - tail - 1;
                    self.started_at = self.offset + 1;
                    self.skip_line = true;
                }
            }
            self.attached = true;
            0
        };
        let unfinished = self.window.len() - self.emitted;
        let limit = CHUNK.min(overlap + RECORD_LIMIT - unfinished);
        let mut bytes = self
            .transport
            .read_at(self.offset - overlap as u64, limit)?;
        if bytes.len() > limit {
            return Err(invalid("SFTP read exceeds requested length"));
        }
        let back = self.window.len() - overlap;
        let seen = bytes.len().min(overlap);
        if bytes[..seen] != self.window[back..back + seen] {
            return Ok(Update::ContinuityLost);
        }
        if bytes.len() < overlap {
            // Truncated at the cursor, or a short read: reopen and prove the
            // whole window now (once; that pass reads with no overlap).
            self.attached = false;
            return self.read_poll();
        }
        if bytes.len() == overlap {
            // EOF: the next poll reopens the path and proves the window,
            // which also notices a rotated-away file.
            self.attached = false;
            return Ok(Update::CaughtUp);
        }
        bytes.drain(..overlap);
        self.offset += bytes.len() as u64;
        self.window.extend_from_slice(&bytes);
        if self.skip_line {
            match self.window[self.emitted..].iter().position(|b| *b == b'\n') {
                Some(n) => {
                    self.emitted += n + 1;
                    self.skip_line = false;
                }
                None => self.emitted = self.window.len(),
            }
        }
        let end = self.window[self.emitted..]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(self.emitted, |n| self.emitted + n + 1);
        let records = self.window[self.emitted..end].to_vec();
        self.emitted = end;
        // Keep only the proof window and the unfinished record.
        let trim = self.emitted.min(self.window.len().saturating_sub(PROOF));
        self.window.drain(..trim);
        self.emitted -= trim;
        Ok(if records.is_empty() {
            Update::Pending
        } else {
            Update::Records(records)
        })
    }
    /// Re-read the retained window at its offset; short reads are allowed.
    fn prove(&mut self) -> io::Result<bool> {
        let start = self.offset - self.window.len() as u64;
        let mut verified = 0;
        while verified < self.window.len() {
            let limit = CHUNK.min(self.window.len() - verified);
            let bytes = self.transport.read_at(start + verified as u64, limit)?;
            if bytes.is_empty()
                || bytes.len() > limit
                || bytes != self.window[verified..verified + bytes.len()]
            {
                return Ok(false);
            }
            verified += bytes.len();
        }
        Ok(true)
    }
}
impl<T: Transport> Drop for Follow<T> {
    fn drop(&mut self) {
        self.transport.disconnect();
    }
}

#[cfg(test)]
#[path = "sftp/tests.rs"]
mod tests;
