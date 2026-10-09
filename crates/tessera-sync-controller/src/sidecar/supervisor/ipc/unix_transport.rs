//! Unix-domain transport for the macOS supervisor (design:
//! docs/sync-sidecar-discovery.md). A private socket in the owner-only state
//! directory named by generation, one exchange per connection, an absolute deadline
//! over every read and write, and a pluggable [`PeerCheck`] that must pass before any
//! byte is exchanged. Linux builds it too, so the logic is tested everywhere; only the
//! peer check differs per platform. Signature verification of the peer is a separate
//! macOS-only [`PeerCheck`], never implied by this transport.
use super::{Scope, Transport};
use crate::sidecar::Binding;
use anyhow::{ensure, Context, Result};
use std::{
    io::{self, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// `sun_path` is 104 bytes on macOS and 108 on Linux; stay below both with room.
const MAX_SOCKET_PATH: usize = 100;

/// Which end of the connection the checked peer is.
#[derive(Clone, Copy, Debug)]
pub enum PeerEnd {
    Server,
    Client,
}
/// Platform-specific authentication of the process on the other end of a connected
/// socket. It must use kernel-reported credentials of that connection, never wire
/// claims or a PID file.
pub trait PeerCheck {
    fn verify(&self, stream: &UnixStream, end: PeerEnd) -> Result<()>;
}

impl PeerCheck for Box<dyn PeerCheck> {
    fn verify(&self, stream: &UnixStream, end: PeerEnd) -> Result<()> {
        (**self).verify(stream, end)
    }
}

/// Effective uid of the peer as the kernel reports it for this connection.
pub fn peer_uid(stream: &UnixStream) -> Result<u32> {
    imp::peer_uid(stream)
}
/// Peer uid must equal the current effective uid: same user, nothing more. Not a
/// signature or executable check.
pub struct SameUser;
impl PeerCheck for SameUser {
    fn verify(&self, stream: &UnixStream, _: PeerEnd) -> Result<()> {
        ensure!(
            peer_uid(stream)? == rustix::process::geteuid().as_raw(),
            "socket peer belongs to another user"
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{ensure, Result};
    use std::{mem::size_of, os::fd::AsRawFd, os::unix::net::UnixStream};
    pub fn peer_uid(stream: &UnixStream) -> Result<u32> {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = size_of::<libc::ucred>() as libc::socklen_t;
        let status = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        ensure!(
            status == 0 && length as usize == size_of::<libc::ucred>(),
            "cannot read the socket peer credentials"
        );
        Ok(cred.uid)
    }
}
#[cfg(target_os = "macos")]
mod imp {
    use anyhow::{ensure, Result};
    use std::{mem::size_of, os::fd::AsRawFd, os::unix::net::UnixStream};
    /// `LOCAL_PEERCRED` (sys/un.h) at level `SOL_LOCAL` (0).
    const SOL_LOCAL: libc::c_int = 0;
    const LOCAL_PEERCRED: libc::c_int = 1;
    pub fn peer_uid(stream: &UnixStream) -> Result<u32> {
        let mut cred: libc::xucred = unsafe { std::mem::zeroed() };
        cred.cr_version = libc::XUCRED_VERSION;
        let mut length = size_of::<libc::xucred>() as libc::socklen_t;
        let status = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                SOL_LOCAL,
                LOCAL_PEERCRED,
                (&mut cred as *mut libc::xucred).cast(),
                &mut length,
            )
        };
        ensure!(
            status == 0 && cred.cr_version == libc::XUCRED_VERSION,
            "cannot read the socket peer credentials"
        );
        Ok(cred.cr_uid)
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use anyhow::Result;
    use std::os::unix::net::UnixStream;
    pub fn peer_uid(_: &UnixStream) -> Result<u32> {
        anyhow::bail!("peer credentials are not implemented for this platform")
    }
}

fn socket_path(directory: &Path, scope: &Scope) -> Result<PathBuf> {
    ensure!(
        !scope.installation.is_nil() && !scope.instance.is_nil() && !scope.generation.is_nil(),
        "nil transport scope"
    );
    // 16 hex digits of the random generation locate the socket; the generation itself
    // is verified by the reply, so a collision here only fails closed.
    let name = scope.generation.simple().to_string();
    let path = directory.join(format!("s-{}.sock", &name[..16]));
    ensure!(
        path.as_os_str().len() <= MAX_SOCKET_PATH,
        "state directory path is too long for a socket"
    );
    Ok(path)
}
/// The state directory is the access boundary: it must be a real directory (not a
/// link) owned by the current user with no group or other access.
fn check_private_directory(directory: &Path) -> Result<()> {
    ensure!(directory.is_absolute(), "absolute state directory required");
    let metadata = std::fs::symlink_metadata(directory)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.mode() & 0o077 == 0,
        "private user-owned state directory required"
    );
    Ok(())
}

/// One listening socket for one supervisor generation. Created only on explicit
/// request; a socket already at that path is a collision and is never unlinked,
/// adopted or replaced. Dropping removes only the path this value created.
pub struct UnixEndpoint {
    listener: UnixListener,
    path: PathBuf,
    scope: Scope,
}
impl UnixEndpoint {
    pub fn create(directory: &Path, scope: &Scope) -> Result<Self> {
        check_private_directory(directory)?;
        let path = socket_path(directory, scope)?;
        let listener = UnixListener::bind(&path).context("creating the private socket")?;
        // Own the node from the moment it exists, so every failure below removes it.
        let endpoint = Self {
            listener,
            path,
            scope: scope.clone(),
        };
        // The 0700 directory is the real boundary; tighten the node as well.
        std::fs::set_permissions(&endpoint.path, std::fs::Permissions::from_mode(0o600))?;
        endpoint.listener.set_nonblocking(true)?;
        Ok(endpoint)
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Wait for exactly one connection until the absolute deadline.
    pub fn accept(&self, deadline: Instant) -> Result<UnixStream> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    return Ok(stream);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    ensure!(!left.is_zero(), "no client connected before the deadline");
                    std::thread::sleep(left.min(Duration::from_millis(2)));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}
impl Drop for UnixEndpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One local connect attempt, no retry and no fallback path. Missing or busy
/// endpoints fail closed.
pub fn connect(directory: &Path, scope: &Scope) -> Result<UnixStream> {
    check_private_directory(directory)?;
    UnixStream::connect(socket_path(directory, scope)?).context("connecting to the supervisor")
}

/// Binding/scope-checked transport. Raw I/O is unavailable until `verify_peer`
/// succeeded, every read and write is bounded by one absolute deadline, and any error
/// poisons it.
pub struct UnixTransport<C> {
    stream: UnixStream,
    end: PeerEnd,
    binding: Binding,
    scope: Scope,
    deadline: Instant,
    check: C,
    verified: bool,
    failed: bool,
}
impl<C: PeerCheck> UnixTransport<C> {
    /// `end` is which side the *peer* is: a client checks `Server`, a server `Client`.
    pub fn new(
        stream: UnixStream,
        end: PeerEnd,
        binding: Binding,
        scope: Scope,
        deadline: Instant,
        check: C,
    ) -> Result<Self> {
        ensure!(
            !scope.installation.is_nil() && !scope.instance.is_nil() && !scope.generation.is_nil(),
            "nil transport scope"
        );
        ensure!(
            binding.installation == scope.installation && binding.instance == scope.instance,
            "prepared scope differs from binding"
        );
        ensure!(
            !binding.supervisor.is_empty()
                && !binding.state_directory.is_empty()
                && !binding.device_identity.is_empty(),
            "incomplete prepared binding"
        );
        Ok(Self {
            stream,
            end,
            binding,
            scope,
            deadline,
            check,
            verified: false,
            failed: false,
        })
    }
    fn ready(&mut self) -> io::Result<Duration> {
        if self.failed || !self.verified {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "transport is unverified or poisoned",
            ));
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "supervisor I/O deadline elapsed",
            ));
        }
        Ok(left)
    }
}
impl<C: PeerCheck> Transport for UnixTransport<C> {
    fn verify_peer(&mut self, binding: &Binding, scope: &Scope) -> Result<()> {
        let result = (|| {
            ensure!(!self.failed, "transport is poisoned");
            ensure!(
                binding == &self.binding,
                "prepared transport binding changed"
            );
            ensure!(scope == &self.scope, "prepared transport scope changed");
            self.check.verify(&self.stream, self.end)
        })();
        self.verified = result.is_ok();
        self.failed |= result.is_err();
        result
    }
}
impl<C: PeerCheck> Read for UnixTransport<C> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let result = self.ready().and_then(|left| {
            self.stream.set_read_timeout(Some(left))?;
            self.stream.read(bytes)
        });
        self.failed |= result.is_err();
        result
    }
}
impl<C: PeerCheck> Write for UnixTransport<C> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.ready().and_then(|left| {
            self.stream.set_write_timeout(Some(left))?;
            self.stream.write(bytes)
        });
        self.failed |= result.is_err();
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        let result = self.ready().and_then(|_| self.stream.flush());
        self.failed |= result.is_err();
        result
    }
}

#[cfg(test)]
mod tests;
