//! Read-only SFTP v3 over the user's OpenSSH configuration (#1390).
//!
//! This adapter deliberately stops on a changed consumed prefix: SFTP v3 has no
//! portable inode identity and cannot recover an unobserved rotated-away file.
//! Callers must display `ContinuityLost`, rather than imply gap-free recovery.
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

pub mod directory;

const CHUNK: usize = 32 * 1024;
const PACKET_LIMIT: usize = CHUNK + 4096;
/// Retained continuity proof is bounded; reaching this limit requires a new session.
pub const HISTORY_LIMIT: usize = 8 * 1024 * 1024;

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
    fn command(&self) -> Command {
        let mut command = self.ssh_command();
        command.args(["-s", "--", &self.host, "sftp"]);
        command
    }
    /// Trusted, fixed program only. Remote paths and content belong on stdin,
    /// never in shell arguments. Used only by explicit guarded Markdown Save.
    pub(crate) fn guarded_markdown_save_command(&self, program: &'static str) -> Command {
        let mut command = self.ssh_command();
        command.args(["--", &self.host, program]);
        command
    }
    pub(crate) fn remote_path(&self) -> &[u8] {
        &self.path
    }
    fn ssh_command(&self) -> Command {
        let mut command = Command::new("ssh");
        command.arg("-T");
        // Without the askpass bridge OpenSSH must never wait for an answer.
        // With it, agent/keys still go first; only OpenSSH's own questions
        // reach the app, and a changed host key is still refused.
        if self.interactive && crate::ssh_askpass::configure(&mut command) {
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
        command
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
}
#[derive(Default)]
pub struct OpenSsh {
    session: Option<Session>,
}
struct Session {
    child: Child,
    input: ChildStdin,
    output: Receiver<io::Result<Vec<u8>>>,
    id: u32,
    handle: Vec<u8>,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Session {
    fn exchange(&mut self, payload: &[u8]) -> io::Result<Vec<u8>> {
        self.input
            .write_all(&(payload.len() as u32).to_be_bytes())?;
        self.input.write_all(payload)?;
        self.input.flush()?;
        // The first response waits for authentication; time spent in an
        // askpass dialog does not count against the response deadline.
        let deadline = crate::ssh_askpass::Deadline::after(Duration::from_secs(15));
        loop {
            match self
                .output
                .recv_timeout(deadline.remaining().min(Duration::from_millis(250)))
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
impl Transport for OpenSsh {
    fn open(&mut self, target: &Target) -> io::Result<()> {
        self.disconnect();
        let mut child = target
            .command()
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
        let mut session = Session {
            child,
            input,
            output,
            id: 0,
            handle: Vec::new(),
        };
        let version = session.exchange(&[1, 0, 0, 0, 3])?;
        if version.len() < 5 || version[0] != 2 || version[1..5] != 3u32.to_be_bytes() {
            return Err(invalid("SFTP v3 required"));
        }
        let mut request = Vec::new();
        string(&mut request, &target.path);
        request.extend_from_slice(&1u32.to_be_bytes()); // SSH_FXF_READ only
        request.extend_from_slice(&0u32.to_be_bytes()); // no attributes
        let (kind, body) = session.request(3, &request)?;
        if kind != 102 {
            return Err(invalid("SFTP read-only open failed"));
        }
        session.handle = take_string(&mut body.as_slice())?.to_vec();
        self.session = Some(session);
        Ok(())
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

    fn disconnect(&mut self) {
        self.session = None;
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
/// One bounded, read-only following session. Call `poll` on a background worker
/// with caller-owned retry/backoff. A failed request never advances its cursor.
pub struct Follow<T: Transport> {
    target: Target,
    transport: T,
    consumed: Vec<u8>,
    emitted: usize,
    terminal: bool,
}
impl<T: Transport> Follow<T> {
    pub fn new(target: Target, transport: T) -> Self {
        Self {
            target,
            transport,
            consumed: Vec::new(),
            emitted: 0,
            terminal: false,
        }
    }
    pub fn offset(&self) -> u64 {
        self.consumed.len() as u64
    }
    pub fn poll(&mut self) -> Update {
        if self.terminal {
            return Update::ContinuityLost;
        }
        if self.consumed.len() == HISTORY_LIMIT {
            return Update::LimitReached;
        }
        // Reopen and prove the entire prefix, including partial records, before
        // accepting growth. Sampling guards cannot prove reconnect continuity.
        let result = self.read_poll();
        self.transport.disconnect();
        match result {
            Ok(update) => update,
            Err(error) => Update::Disconnected(error.to_string()),
        }
    }
    fn read_poll(&mut self) -> io::Result<Update> {
        self.transport.open(&self.target)?;
        let mut verified = 0;
        while verified < self.consumed.len() {
            let limit = CHUNK.min(self.consumed.len() - verified);
            let bytes = self.transport.read_at(verified as u64, limit)?;
            if bytes.is_empty()
                || bytes.len() > limit
                || bytes != self.consumed[verified..verified + bytes.len()]
            {
                self.terminal = true;
                return Ok(Update::ContinuityLost);
            }
            verified += bytes.len();
        }
        let limit = CHUNK.min(HISTORY_LIMIT - self.consumed.len());
        let bytes = self.transport.read_at(self.offset(), limit)?;
        if bytes.len() > limit {
            return Err(invalid("SFTP read exceeds requested length"));
        }
        self.consumed.extend_from_slice(&bytes);
        let end = self.consumed[self.emitted..]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(self.emitted, |n| self.emitted + n + 1);
        if end == self.emitted {
            return Ok(if bytes.is_empty() {
                Update::CaughtUp
            } else {
                Update::Pending
            });
        }
        let records = self.consumed[self.emitted..end].to_vec();
        self.emitted = end;
        Ok(Update::Records(records))
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
