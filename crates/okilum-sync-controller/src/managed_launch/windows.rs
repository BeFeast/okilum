//! Captured Job Object launch and kernel process/REST ownership on Windows.
use super::*;
use ::windows::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_FILES, HANDLE, WAIT_TIMEOUT},
    NetworkManagement::IpHelper::*,
    Networking::WinSock::{AF_INET, AF_INET6},
    System::{Diagnostics::ToolHelp::*, Threading::*},
};
use std::net::IpAddr;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

struct Starting {
    reference: Reference,
    binding: Binding,
    gate: Arc<LockFile>,
    folders: FencedFolders,
}

/// Retained by the supervisor for its entire owned-tree lifetime. An older
/// unregistered supervisor can keep serving existing pairings but cannot grant
/// first-merge publication authority.
pub struct NativeLaunch {
    _process: LockFile,
    starting: Option<Starting>,
}
impl NativeLaunch {
    pub fn if_registered(config: &Path, executable: &Path) -> Result<Option<Self>> {
        let canonical_home = config
            .parent()
            .context("native config home missing")?
            .canonicalize()?;
        let home = canonical_home.as_path();
        let pinned = native_launch_reference(home.parent().context("native state missing")?)?;
        if !directory(home)?.try_exists()? {
            ensure!(
                pinned.is_none(),
                "native launch registry missing; recovery required"
            );
            return Ok(None);
        }
        let reference = existing(home)?;
        if let Some(pinned) = pinned {
            ensure!(
                pinned == reference,
                "native launch preparation reference changed"
            );
        }
        let process = lock_file(&reference.directory.join("process.lock"), false, false)?;
        let gate = reference.gate()?;
        reference.validate(&ManagedInstance {
            home: home.to_owned(),
            executable: executable.canonicalize()?,
        })?;
        let binding = reference.load()?;
        let folders = fence_folders(&reference, &binding)?;
        Ok(Some(Self {
            _process: process,
            starting: Some(Starting {
                reference,
                binding,
                gate,
                folders,
            }),
        }))
    }

    /// Call after spawning the captured child and before serving the supervisor
    /// channel. Any failure unwinds through the caller's owned-tree cleanup.
    pub fn ready(mut self, pid: u32, generation: Uuid) -> Result<Self> {
        let Starting {
            reference,
            binding,
            gate,
            folders,
        } = self
            .starting
            .take()
            .context("native launch already ready")?;
        let daemon = crate::daemon::managed_transport_identity(
            binding.home.parent().context("native state missing")?,
        )?;
        let address = daemon.rest_address()?;
        let mut receipt = Generation {
            id: generation,
            pid,
            start_time: process_start(pid)?,
            owner_pid: std::process::id(),
            owner_start_time: process_start(std::process::id())?,
            binding: binding.id,
            executable_hash: binding.executable_hash.clone(),
            rest_address: address,
            rest_start_time: None,
            paused_at_start: folders.paused_at_start,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            ensure!(
                process_parent(pid)? == std::process::id()
                    && process_start(pid)? == receipt.start_time
                    && identity(&process_image(pid)?)? == identity(&binding.executable)?,
                "native child generation changed during readiness"
            );
            if let Ok(api) = daemon.connect() {
                owns_rest(pid, address)?;
                receipt.rest_start_time = Some(
                    api.identity()?["startTime"]
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .context("native REST generation missing")?
                        .to_owned(),
                );
                for folder in &folders.closed {
                    ensure!(
                        api.folder(folder)?["paused"] == true,
                        "native startup fence failed"
                    );
                }
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "native runtime readiness timed out"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        private::write(
            &reference.directory.join("generation.json"),
            &serde_json::to_vec(&receipt)?,
        )?;
        drop(folders.leases);
        drop(gate);
        Ok(self)
    }
}

fn process(pid: u32) -> Result<OwnedHandle> {
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )?
    };
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    let raw = HANDLE(handle.as_raw_handle());
    ensure!(
        unsafe { WaitForSingleObject(raw, 0) } == WAIT_TIMEOUT,
        "native process has exited"
    );
    ensure!(
        crate::sidecar::windows::security::process_sid(raw)?
            == crate::sidecar::windows::security::current_sid()?,
        "native process belongs to another user"
    );
    Ok(handle)
}
fn process_start(pid: u32) -> Result<String> {
    let process = process(pid)?;
    Ok(
        crate::sidecar::supervisor::ipc::windows_discovery::start_time(HANDLE(
            process.as_raw_handle(),
        ))?
        .to_string(),
    )
}
fn process_image(pid: u32) -> Result<PathBuf> {
    let process = process(pid)?;
    Ok(PathBuf::from(
        crate::sidecar::supervisor::ipc::windows_discovery::image_path(HANDLE(
            process.as_raw_handle(),
        ))?,
    ))
}
fn process_parent(pid: u32) -> Result<u32> {
    // Retain the process object across the snapshot and verify its creation
    // time again. A reused PID never supplies a parent witness.
    let process = process(pid)?;
    let raw = HANDLE(process.as_raw_handle());
    let before = crate::sidecar::supervisor::ipc::windows_discovery::start_time(raw)?;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? };
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe {
        Process32FirstW(HANDLE(snapshot.as_raw_handle()), &mut entry)?;
    }
    loop {
        if entry.th32ProcessID == pid {
            ensure!(
                unsafe { WaitForSingleObject(raw, 0) } == WAIT_TIMEOUT
                    && crate::sidecar::supervisor::ipc::windows_discovery::start_time(raw)?
                        == before,
                "native process generation changed during parent inspection"
            );
            return Ok(entry.th32ParentProcessID);
        }
        match unsafe { Process32NextW(HANDLE(snapshot.as_raw_handle()), &mut entry) } {
            Ok(()) => (),
            Err(error)
                if error.code() == ::windows::core::HRESULT::from_win32(ERROR_NO_MORE_FILES.0) =>
            {
                break
            }
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("native process parent is unavailable")
}
fn owns_rest(pid: u32, address: SocketAddr) -> Result<()> {
    let captured = process(pid)?;
    let family = if address.is_ipv4() { AF_INET } else { AF_INET6 };
    let mut size = 0;
    ensure!(
        unsafe {
            GetExtendedTcpTable(
                None,
                &mut size,
                false,
                family.0 as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        } == ERROR_INSUFFICIENT_BUFFER.0,
        "native REST listener inventory unavailable"
    );
    ensure!(
        size <= 16 * 1024 * 1024,
        "native listener inventory exceeds bound"
    );
    let mut allocation = vec![0u64; (size as usize).div_ceil(8)];
    let capacity = allocation.len() * 8;
    let table = allocation.as_mut_ptr();
    ensure!(
        unsafe {
            GetExtendedTcpTable(
                Some(table.cast()),
                &mut size,
                false,
                family.0 as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        } == 0
            && size as usize <= capacity,
        "native REST listener inventory changed"
    );
    let mut matches = 0;
    if address.is_ipv4() {
        let table = table.cast::<MIB_TCPTABLE_OWNER_PID>();
        let count = unsafe { (*table).dwNumEntries } as usize;
        ensure!(
            count <= 65536
                && std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table)
                    + count * std::mem::size_of::<MIB_TCPROW_OWNER_PID>()
                    <= size as usize,
            "invalid listener inventory"
        );
        let rows = unsafe {
            std::slice::from_raw_parts(
                std::ptr::addr_of!((*table).table).cast::<MIB_TCPROW_OWNER_PID>(),
                count,
            )
        };
        for row in rows {
            if row.dwOwningPid == pid
                && row.dwState == 2
                && IpAddr::V4(std::net::Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()))
                    == address.ip()
                && u16::from_be(row.dwLocalPort as u16) == address.port()
            {
                matches += 1;
            }
        }
    } else {
        let table = table.cast::<MIB_TCP6TABLE_OWNER_PID>();
        let count = unsafe { (*table).dwNumEntries } as usize;
        ensure!(
            count <= 65536
                && std::mem::offset_of!(MIB_TCP6TABLE_OWNER_PID, table)
                    + count * std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>()
                    <= size as usize,
            "invalid listener inventory"
        );
        let rows = unsafe {
            std::slice::from_raw_parts(
                std::ptr::addr_of!((*table).table).cast::<MIB_TCP6ROW_OWNER_PID>(),
                count,
            )
        };
        for row in rows {
            if row.dwOwningPid == pid
                && row.dwState == 2
                && IpAddr::V6(std::net::Ipv6Addr::from(row.ucLocalAddr)) == address.ip()
                && u16::from_be(row.dwLocalPort as u16) == address.port()
            {
                matches += 1;
            }
        }
    }
    ensure!(
        matches == 1
            && unsafe { WaitForSingleObject(HANDLE(captured.as_raw_handle()), 0) } == WAIT_TIMEOUT,
        "REST listener is not owned by the admitted runtime generation"
    );
    Ok(())
}

pub fn live_generation(home: &Path) -> Result<Generation> {
    let reference = existing(home)?;
    let binding = reference.load()?;
    ensure!(binding.home == home, "native home binding changed");
    let lock = private::managed_lock(&reference.directory.join("process.lock"), false)?;
    ensure!(
        matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "native launch owner is absent"
    );
    let receipt: Generation = serde_json::from_slice(&private::read(
        &reference.directory.join("generation.json"),
    )?)?;
    ensure!(
        receipt.binding == binding.id
            && receipt.executable_hash == binding.executable_hash
            && process_start(receipt.pid)? == receipt.start_time
            && process_start(receipt.owner_pid)? == receipt.owner_start_time
            && process_parent(receipt.pid)? == receipt.owner_pid
            && identity(&process_image(receipt.pid)?)? == identity(&binding.executable)?
            && digest(&binding.executable)? == binding.executable_hash,
        "native process generation changed"
    );
    owns_rest(receipt.pid, receipt.rest_address)?;
    let daemon =
        crate::daemon::managed_transport_identity(home.parent().context("native state missing")?)?;
    let started = receipt
        .rest_start_time
        .as_ref()
        .context("native runtime not ready")?;
    ensure!(
        daemon.rest_address()? == receipt.rest_address
            && daemon.connect()?.identity()?["startTime"].as_str() == Some(started.as_str())
            && process_start(receipt.pid)? == receipt.start_time,
        "native REST generation changed"
    );
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_listener_and_process_proofs_require_live_kernel_owners() -> Result<()> {
        let pid = std::process::id();
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        owns_rest(pid, address)?;
        assert!(owns_rest(process_parent(pid)?, address).is_err());
        assert_eq!(
            identity(&process_image(pid)?)?,
            identity(&std::env::current_exe()?)?
        );
        assert_eq!(process_start(pid)?, process_start(pid)?);
        assert!(process_start(u32::MAX).is_err());
        drop(listener);
        assert!(owns_rest(pid, address).is_err());
        let listener = std::net::TcpListener::bind("[::1]:0")?;
        owns_rest(pid, listener.local_addr()?)?;
        Ok(())
    }
}
