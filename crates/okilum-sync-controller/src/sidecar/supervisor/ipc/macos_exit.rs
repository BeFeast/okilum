//! Observe exit of the authenticated supervisor process, never kill by PID.
//! The kernel process event is installed between two audit-token/code checks so
//! PID reuse during capture cannot turn another process into exit authority.
use super::{
    macos_peer::{peer_audit_token, SignedPeer},
    unix_transport::{PeerCheck, PeerEnd},
};
use anyhow::{ensure, Context, Result};
use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    time::Instant,
};

pub struct ExitWatch {
    queue: OwnedFd,
    pid: usize,
    exited: bool,
}
impl ExitWatch {
    /// Capture only from an established connection to the expected signed helper.
    /// No caller-supplied PID, image guess, hint PID, signal or service operation.
    pub fn capture(stream: &UnixStream, peer: &SignedPeer) -> Result<Self> {
        peer.verify(stream, PeerEnd::Server)?;
        let token = peer_audit_token(stream)?;
        // audit_token_t.val[5] is pid; val[7] is the PID version used by the
        // Security audit-token lookup. Retain/check the entire token, not only pid.
        let pid = u32::from_ne_bytes(token[20..24].try_into().unwrap());
        ensure!(
            pid > 0 && pid <= i32::MAX as u32,
            "invalid authenticated process id"
        );
        let descriptor = unsafe { libc::kqueue() };
        ensure!(
            descriptor >= 0,
            "cannot create process exit queue: {}",
            io::Error::last_os_error()
        );
        let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
        ensure!(
            unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
            "cannot make exit queue non-inheritable: {}",
            io::Error::last_os_error()
        );
        let event = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let registered = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        ensure!(
            registered == 0,
            "cannot capture process exit: {}",
            io::Error::last_os_error()
        );
        // The registration pins the kernel process object, but its numeric PID
        // was read before registering. Re-authenticate the original audit token
        // after registration. A dead/reused PID or changed image is a refusal.
        peer.verify(stream, PeerEnd::Server)?;
        ensure!(
            peer_audit_token(stream)? == token,
            "socket peer changed during exit capture"
        );
        Ok(Self {
            queue,
            pid: pid as usize,
            exited: false,
        })
    }

    /// True only after NOTE_EXIT for the captured process. False is an expired
    /// observation budget, not a claim that the process is alive or safe to kill.
    /// This does not certify child-tree exit; require an authenticated Stop reply
    /// separately and revalidate durable authority before unregistering.
    pub fn wait(&mut self, deadline: Instant) -> Result<bool> {
        if self.exited {
            return Ok(true);
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let timeout = libc::timespec {
                tv_sec: remaining
                    .as_secs()
                    .try_into()
                    .context("exit observation budget overflow")?,
                tv_nsec: remaining.subsec_nanos().into(),
            };
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let count = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &timeout,
                )
            };
            if count == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error).context("observing authenticated supervisor exit");
            }
            if count == 0 {
                return Ok(false);
            }
            ensure!(
                count == 1
                    && event.flags & libc::EV_ERROR == 0
                    && event.filter == libc::EVFILT_PROC
                    && event.ident == self.pid
                    && event.fflags & libc::NOTE_EXIT != 0,
                "unexpected process exit event"
            );
            self.exited = true;
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests;
