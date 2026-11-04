//! Conservative fresh-install inventory. Observations never grant lifecycle ownership.
use anyhow::{ensure, Context, Result};
use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Bounded, output-limited read-only/offline commands. Never runs a shell or PATH lookup.
pub(super) fn output(command: &mut Command, deadline: Instant, limit: usize) -> Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    ensure!(Instant::now() < deadline, "operation deadline elapsed");
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        command
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut stdout = child.0.stdout.take().context("missing command output")?;
    let fd = stdout.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "cannot bound command output"
    );
    let mut bytes = Vec::new();
    loop {
        ensure!(Instant::now() < deadline, "operation deadline elapsed");
        let mut buffer = [0u8; 8192];
        let eof = match stdout.read(&mut buffer) {
            Ok(0) => true,
            Ok(n) => {
                ensure!(
                    bytes.len() + n <= limit,
                    "command output inventory incomplete"
                );
                bytes.extend_from_slice(&buffer[..n]);
                false
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                false
            }
            Err(e) => return Err(e.into()),
        };
        if let Some(status) = child.0.try_wait()? {
            if eof {
                ensure!(status.success(), "inventory/offline command failed");
                return Ok(bytes);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn reject_sync(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes)?.to_ascii_lowercase();
    ensure!(!text.contains("syncthing") && !text.contains("okilum-sync-supervisor") && !text.contains("com.befeast.okilum.sync"), "existing Sync process/service requires explicit selection or recovery; no second daemon will start");
    Ok(())
}

#[cfg(target_os = "macos")]
fn processes(deadline: Instant) -> Result<()> {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};
    let size = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    ensure!(
        size > 0 && size < 1_000_000,
        "process inventory unavailable"
    );
    let mut pids = vec![0 as libc::pid_t; size as usize * 2 + 128];
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<libc::pid_t>()) as i32,
        )
    };
    ensure!(
        count > 0 && (count as usize) < pids.len(),
        "process inventory truncated"
    );
    let mut self_seen = false;
    for &pid in &pids[..count as usize] {
        ensure!(Instant::now() < deadline, "inventory deadline elapsed");
        if pid <= 0 {
            continue;
        }
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if read != size {
            // A disappearing process is harmless. A live process this account may not
            // signal (EPERM) belongs to another account, e.g. a protected endpoint
            // security agent on a managed Mac (#2589); it cannot be this account's Sync
            // daemon. Only an unreadable process of this account remains uncertainty.
            let alive = unsafe { libc::kill(pid, 0) };
            let errno = std::io::Error::last_os_error().raw_os_error();
            ensure!(
                alive == -1 && matches!(errno, Some(libc::ESRCH) | Some(libc::EPERM)),
                "live process inventory unreadable"
            );
            continue;
        }
        let name: Vec<u8> = info
            .pbi_name
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        reject_sync(&name)?;
        if info.pbi_uid != unsafe { libc::geteuid() } {
            continue;
        }
        let mut bytes = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe { libc::proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
        if n <= 0 {
            // Managed Macs hide the image path of some account processes (#2589). Their
            // name already passed reject_sync; only the positive control needs a path.
            ensure!(
                pid != std::process::id() as i32,
                "account process image inventory unreadable"
            );
            continue;
        }
        let path = &bytes[..n as usize];
        reject_sync(path)?;
        if pid == std::process::id() as i32 {
            ensure!(
                Path::new(OsStr::from_bytes(path)) == std::env::current_exe()?,
                "process inventory positive control failed"
            );
            self_seen = true;
        }
    }
    ensure!(
        self_seen,
        "process inventory did not detect its positive control"
    );
    Ok(())
}

/// Call immediately before fresh preparation and again before registration. No mutation.
#[cfg(target_os = "macos")]
pub fn require_fresh(deadline: Instant) -> Result<()> {
    processes(deadline)?;
    let uid = unsafe { libc::geteuid() };
    for domain in [
        "system".to_owned(),
        format!("user/{uid}"),
        format!("gui/{uid}"),
    ] {
        let bytes = output(
            Command::new("/bin/launchctl").args(["print", &domain]),
            deadline,
            16 * 1024 * 1024,
        )?;
        let text = std::str::from_utf8(&bytes)?;
        ensure!(
            text.starts_with(&format!("{domain} = {{"))
                && text.contains("services = {")
                && text.contains("type ="),
            "launchd inventory positive control failed"
        );
        reject_sync(&bytes)?;
    }
    // Unloaded agents can become active later. Inspect persisted definitions too.
    let home = super::account_state_directory()?
        .ancestors()
        .nth(4)
        .context("account home unavailable")?
        .to_path_buf();
    let account_agents = home.join("Library/LaunchAgents");
    for directory in [
        account_agents.clone(),
        "/Library/LaunchAgents".into(),
        "/Library/LaunchDaemons".into(),
    ] {
        let system = directory != account_agents;
        match std::fs::read_dir(directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(entries) => {
                for entry in entries {
                    let path = entry?.path();
                    if path.extension().is_none_or(|x| x != "plist") {
                        continue;
                    }
                    let xml = match output(
                        Command::new("/usr/bin/plutil")
                            .args(["-convert", "xml1", "-o", "-"])
                            .arg(&path),
                        deadline,
                        1024 * 1024,
                    ) {
                        Ok(xml) => xml,
                        // A managed Mac keeps some system definitions unreadable to the
                        // account (#2589). This account cannot have installed them, so
                        // they cannot be its Sync service; account agents stay strict.
                        Err(_) if system && std::fs::File::open(&path).is_err() => continue,
                        Err(e) => return Err(e),
                    };
                    ensure!(
                        xml.starts_with(b"<?xml") && xml.windows(6).any(|x| x == b"<plist"),
                        "service definition inventory incomplete"
                    );
                    reject_sync(&xml)?;
                }
            }
        }
    }
    for path in [
        home.join("Library/Application Support/Syncthing/config.xml"),
        home.join(".config/syncthing/config.xml"),
        home.join(".local/state/syncthing/config.xml"),
    ] {
        ensure!(
            !path.try_exists()?,
            "existing Syncthing configuration requires explicit selection"
        );
        ensure!(
            std::fs::symlink_metadata(&path)
                .err()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "configuration inventory uncertain"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_positive_and_negative_controls() {
        assert!(reject_sync(b"/Applications/Syncthing.app/Contents/MacOS/syncthing").is_err());
        assert!(reject_sync(b"com.befeast.okilum.sync = { }").is_err());
        assert!(reject_sync(b"com.apple.finder = { }").is_ok());
        assert!(reject_sync(&[255]).is_err());
    }
    #[test]
    fn bounded_command_detects_output_failure_and_deadline() -> Result<()> {
        let soon = || Instant::now() + Duration::from_secs(2);
        assert_eq!(
            output(
                Command::new("/bin/echo").arg("positive-control"),
                soon(),
                1024
            )?,
            b"positive-control\n"
        );
        assert!(output(Command::new("/bin/echo").arg("positive-control"), soon(), 1).is_err());
        assert!(output(&mut Command::new("/usr/bin/false"), soon(), 1024).is_err());
        assert!(output(&mut Command::new("/bin/echo"), Instant::now(), 1024).is_err());
        Ok(())
    }
}
