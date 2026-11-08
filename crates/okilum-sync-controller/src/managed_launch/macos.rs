//! The native supervisor retains the process lock; Darwin kernel witnesses bind
//! its exact child and authenticated REST listener to that launch generation.
use super::*;
use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

struct Starting {
    reference: Reference,
    binding: Binding,
    gate: Arc<File>,
    folders: FencedFolders,
}

/// Retained by the supervisor for its entire owned-tree lifetime. An older
/// unregistered supervisor can keep serving existing pairings but cannot grant
/// first-merge publication authority.
pub struct NativeLaunch {
    _process: File,
    starting: Option<Starting>,
}
impl NativeLaunch {
    pub fn if_registered(config: &Path, executable: &Path) -> Result<Option<Self>> {
        let home = config.parent().context("native config home missing")?;
        if !directory(home)?.try_exists()? {
            return Ok(None);
        }
        let reference = existing(home)?;
        let process = lock_file(&reference.directory.join("process.lock"), false, false)?;
        let gate = reference.gate()?;
        reference.validate(&ManagedInstance {
            home: home.to_owned(),
            executable: executable.to_owned(),
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

fn info(pid: u32) -> Result<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as libc::c_int;
    let count = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    ensure!(
        count == size && info.pbi_pid == pid && info.pbi_uid == rustix::process::geteuid().as_raw(),
        "native process identity unavailable"
    );
    Ok(info)
}
fn process_start(pid: u32) -> Result<String> {
    let info = info(pid)?;
    Ok(format!(
        "{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}
fn process_parent(pid: u32) -> Result<u32> {
    Ok(info(pid)?.pbi_ppid)
}
fn process_image(pid: u32) -> Result<PathBuf> {
    let mut bytes = vec![0; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let count = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            bytes.as_mut_ptr().cast(),
            bytes.len() as u32,
        )
    };
    ensure!(
        count > 0 && (count as usize) < bytes.len(),
        "native process executable unavailable"
    );
    Ok(PathBuf::from(OsStr::from_bytes(&bytes[..count as usize])))
}
unsafe extern "C" {
    fn okilum_macos_owned_listener(
        pid: libc::c_int,
        family: libc::c_int,
        address: *const u8,
        port: u16,
    ) -> libc::c_int;
}
fn owns_rest(pid: u32, address: SocketAddr) -> Result<()> {
    let (family, bytes): (_, Vec<u8>) = match address.ip() {
        std::net::IpAddr::V4(ip) => (libc::AF_INET, ip.octets().to_vec()),
        std::net::IpAddr::V6(ip) => (libc::AF_INET6, ip.octets().to_vec()),
    };
    ensure!(
        unsafe {
            okilum_macos_owned_listener(pid as libc::c_int, family, bytes.as_ptr(), address.port())
        } == 1,
        "native child does not own its authenticated REST listener"
    );
    Ok(())
}

pub fn live_generation(home: &Path) -> Result<Generation> {
    let reference = existing(home)?;
    let binding = reference.load()?;
    ensure!(binding.home == home, "native home binding changed");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(reference.directory.join("process.lock"))?;
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
    use std::net::TcpListener;

    #[test]
    fn darwin_listener_proof_requires_the_exact_owner_and_endpoint() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        owns_rest(std::process::id(), address)?;
        assert!(owns_rest(unsafe { libc::getppid() } as u32, address).is_err());
        drop(listener);
        assert!(owns_rest(std::process::id(), address).is_err());
        Ok(())
    }

    #[test]
    fn darwin_process_witness_uses_kernel_birth_parent_and_executable() -> Result<()> {
        let pid = std::process::id();
        assert_eq!(process_parent(pid)?, unsafe { libc::getppid() } as u32);
        assert_eq!(process_start(pid)?, process_start(pid)?);
        assert_eq!(
            identity(&process_image(pid)?)?,
            identity(&std::env::current_exe()?)?
        );
        assert!(process_start(u32::MAX).is_err());
        Ok(())
    }
}
