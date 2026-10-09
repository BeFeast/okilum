//! Connect-then-verify identification of the supervisor (design:
//! docs/sync-sidecar-discovery.md). Given an already connected private pipe, find
//! the server process the kernel reports, open it at once, and verify on that
//! handle: same owner as the binding, the executable in `Binding.supervisor`, the
//! start time the supervisor published in its hint (a reused PID started later),
//! and the signature policy. The PID is never trusted by itself; the caller retains
//! the returned peer and pipe, and `ProcessPeer::verify_pipe` rechecks liveness
//! and the PID on the still-connected pipe.
use super::windows_peer::ProcessPeer;
use crate::sidecar::{
    store::Hint,
    windows::security::{current_sid, process_sid},
    Binding,
};
use anyhow::{ensure, Context, Result};
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
};
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::{FILETIME, HANDLE},
        System::{
            Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
            Threading::{
                GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        },
    },
};

/// Signature policy for the supervisor image, injected and never defaulted: an
/// Authenticode signer pin in releases. It receives the path the OS reports for
/// the verified process.
pub trait ImagePolicy {
    fn verify_image(&self, image: &Path) -> Result<()>;
}

pub(crate) fn image_path(process: HANDLE) -> Result<String> {
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )?;
    }
    Ok(String::from_utf16(&buffer[..length as usize])?)
}
/// Process creation time as a 64-bit FILETIME, the unit the hint carries.
pub fn start_time(process: HANDLE) -> Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user)?;
    }
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}
/// Windows paths compare case-insensitively; anything that differs after this
/// normalisation is a different file for our purposes (fail closed).
fn same_path(a: &str, b: &str) -> bool {
    let normal = |p: &str| p.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    normal(a) == normal(b)
}

/// The supervisor's side: identify the client (the app) from the connected pipe. The
/// client's executable is not known in advance and is not path-pinned; the injected
/// policy decides which signed program is accepted. Same user, then the policy.
pub fn identify_client(
    pipe: &impl AsRawHandle,
    owner: &str,
    policy: &dyn ImagePolicy,
) -> Result<ProcessPeer> {
    let mut pid = 0;
    unsafe {
        GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid)?;
    }
    ensure!(pid != 0, "pipe reports no client process");
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    }
    .context("cannot open the pipe's client process")?;
    let process = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let handle = HANDLE(process.as_raw_handle());
    ensure!(
        owner == current_sid()?,
        "the binding's owner is not the running user"
    );
    ensure!(
        process_sid(handle)? == owner,
        "client process belongs to another user"
    );
    let image = image_path(handle)?;
    policy
        .verify_image(Path::new(&image))
        .context("client executable failed the signature policy")?;
    ProcessPeer::from_verified_process(process, owner)
}

pub fn identify_server(
    pipe: &impl AsRawHandle,
    binding: &Binding,
    hint: &Hint,
    policy: &dyn ImagePolicy,
) -> Result<ProcessPeer> {
    let mut pid = 0;
    unsafe {
        GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid)?;
    }
    ensure!(pid != 0, "pipe reports no server process");
    // Open at once. A PID alone proves nothing; the checks below bind this handle.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    }
    .context("cannot open the pipe's server process")?;
    let process = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let handle = HANDLE(process.as_raw_handle());
    ensure!(
        binding.owner == current_sid()?,
        "the binding's owner is not the running user"
    );
    ensure!(
        process_sid(handle)? == binding.owner,
        "server process belongs to another user"
    );
    let image = image_path(handle)?;
    ensure!(
        same_path(&image, &binding.supervisor),
        "server process runs a different executable"
    );
    ensure!(
        start_time(handle)? == hint.started(),
        "server process start time differs from the published hint"
    );
    policy
        .verify_image(Path::new(&image))
        .context("server executable failed the signature policy")?;
    ProcessPeer::from_verified_process(process, &binding.owner)
}

#[cfg(test)]
mod tests;
