//! Native peer identity primitive, not a complete IPC transport. The caller must
//! obtain the expected process handle through trusted launch/signature ownership
//! validation, and retain the connected pipe through the subsequent exchange.
use crate::sidecar::windows::security::{current_sid, process_sid};
use anyhow::{ensure, Result};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use windows::Win32::{
    Foundation::{HANDLE, WAIT_TIMEOUT},
    Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE},
    System::{
        Pipes::{
            GetNamedPipeClientProcessId, GetNamedPipeInfo, GetNamedPipeServerProcessId,
            NAMED_PIPE_MODE, PIPE_SERVER_END,
        },
        Threading::{GetProcessId, WaitForSingleObject},
    },
};

/// Which peer is expected at the other end of this connected local pipe.
#[derive(Clone, Copy, Debug)]
pub enum PeerEnd {
    Server,
    Client,
}

/// Keeps the trusted process object alive. No PID-based process open, pathname
/// trust, wire-provided identity, or default signature verifier is supplied here.
pub struct ProcessPeer {
    process: OwnedHandle,
    pid: u32,
    owner_sid: String,
}
impl ProcessPeer {
    /// Requires QUERY_LIMITED_INFORMATION and SYNCHRONIZE on an already verified
    /// process. This does NOT verify its signature, installation or runtime path.
    pub fn from_verified_process(process: OwnedHandle, owner_sid: &str) -> Result<Self> {
        ensure!(
            owner_sid == current_sid()?,
            "peer owner is not current user"
        );
        let raw = HANDLE(process.as_raw_handle());
        let pid = unsafe { GetProcessId(raw) };
        ensure!(pid != 0, "expected process has no identity");
        let peer = Self {
            process,
            pid,
            owner_sid: owner_sid.into(),
        };
        peer.verify_process()?;
        Ok(peer)
    }

    fn verify_process(&self) -> Result<()> {
        let process = HANDLE(self.process.as_raw_handle());
        ensure!(
            unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT,
            "expected peer is exited or cannot be queried"
        );
        ensure!(
            process_sid(process)? == self.owner_sid,
            "peer process owner changed"
        );
        Ok(())
    }

    /// Check both local endpoint direction and the kernel-reported remote PID.
    /// Holding this guard and the pipe is mandatory through the whole exchange;
    /// an I/O failure still denies success if the peer exits after this check.
    /// Private DACL, local-only endpoint construction, deadline and generation
    /// discovery are separate transport duties, not established by this method.
    pub fn verify_pipe(&self, pipe: &impl AsRawHandle, peer_end: PeerEnd) -> Result<()> {
        self.verify_process()?;
        let pipe = HANDLE(pipe.as_raw_handle());
        ensure!(
            unsafe { GetFileType(pipe) } == FILE_TYPE_PIPE,
            "expected a pipe handle"
        );
        let mut flags = NAMED_PIPE_MODE::default();
        unsafe {
            GetNamedPipeInfo(pipe, Some(&mut flags), None, None, None)?;
        }
        let local_is_server = flags.0 & PIPE_SERVER_END.0 != 0;
        ensure!(
            local_is_server == matches!(peer_end, PeerEnd::Client),
            "wrong pipe endpoint direction"
        );
        let mut peer_pid = 0;
        unsafe {
            match peer_end {
                PeerEnd::Server => GetNamedPipeServerProcessId(pipe, &mut peer_pid)?,
                PeerEnd::Client => GetNamedPipeClientProcessId(pipe, &mut peer_pid)?,
            }
        }
        ensure!(
            peer_pid == self.pid,
            "pipe peer is not the captured process"
        );
        // Recheck liveness after the PID query rather than trusting a stale PID.
        self.verify_process()
    }
}

#[cfg(test)]
mod tests;
