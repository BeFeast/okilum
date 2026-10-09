//! Live same-user processes running a given executable, for the "left the group"
//! check. Read-only. An unreadable process is skipped (it is not ours to see); a
//! failure of the scan itself is an error, never "none found".
use anyhow::Result;
use std::path::Path;

/// Platform start time of `pid`, comparable only with other values from here.
pub fn started(pid: u32) -> Result<u64> {
    imp::started(pid)
}
/// Pids of live processes of `uid` whose image is exactly `image` and whose start
/// time is not earlier than `since`.
pub fn live_copies(image: &Path, uid: u32, since: u64) -> Result<Vec<u32>> {
    imp::live_copies(image, uid, since)
}

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{Context, Result};
    use std::{os::unix::fs::MetadataExt, path::Path};

    pub fn started(pid: u32) -> Result<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        // Field 22 (clock ticks since boot). comm may contain spaces and ')'.
        let after = stat.rsplit_once(')').context("malformed stat")?.1;
        after
            .split_whitespace()
            .nth(19)
            .context("short stat")?
            .parse()
            .context("bad start time")
    }
    pub fn live_copies(image: &Path, uid: u32, since: u64) -> Result<Vec<u32>> {
        let mut found = vec![];
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            if meta.uid() != uid {
                continue;
            }
            let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
                continue;
            };
            if exe != image {
                continue;
            }
            // Gone between the listing and the read: no longer live.
            let Ok(start) = started(pid) else { continue };
            if start >= since {
                found.push(pid);
            }
        }
        Ok(found)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use anyhow::{ensure, Result};
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

    fn info(pid: u32) -> Option<libc::proc_bsdinfo> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        (read == size).then_some(info)
    }
    fn start_of(info: &libc::proc_bsdinfo) -> u64 {
        info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec
    }
    pub fn started(pid: u32) -> Result<u64> {
        let info = info(pid).ok_or_else(|| anyhow::anyhow!("process {pid} not readable"))?;
        Ok(start_of(&info))
    }
    pub fn live_copies(image: &Path, uid: u32, since: u64) -> Result<Vec<u32>> {
        let capacity = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        ensure!(capacity > 0, "cannot list processes");
        let mut pids = vec![0 as libc::pid_t; capacity as usize * 2];
        let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        ensure!(count > 0, "cannot list processes");
        let mut found = vec![];
        for &pid in &pids[..count as usize] {
            if pid <= 0 {
                continue;
            }
            let Some(info) = info(pid as u32) else {
                continue;
            };
            if info.pbi_uid != uid || start_of(&info) < since {
                continue;
            }
            let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
            let length =
                unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
            if length > 0 && Path::new(OsStr::from_bytes(&path[..length as usize])) == image {
                found.push(pid as u32);
            }
        }
        Ok(found)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use anyhow::Result;
    use std::path::Path;
    pub fn started(_: u32) -> Result<u64> {
        anyhow::bail!("process scan is not implemented for this platform")
    }
    pub fn live_copies(_: &Path, _: u32, _: u64) -> Result<Vec<u32>> {
        anyhow::bail!("process scan is not implemented for this platform")
    }
}
