//! Restart of an account Sync helper that still runs a replaced bundle's image
//! (#2931). An in-place app update (Sparkle, a manual copy, the Sprint) swaps
//! `Okilum.app` while the launchd agent keeps running the old executable.
//!
//! The decision is evidence-based and never signals on uncertainty:
//! - the process is launchd's current PID for the account helper job and
//!   belongs to this user;
//! - none of its file mappings is the current bundle's helper file (device and
//!   inode), although every region was enumerated: a process started from the
//!   current bundle always maps its own executable;
//! - it maps an executable that is unlinked or named like the helper.
//!
//! Only that exact process (PID, UID and start time, revalidated immediately
//! before the signal) receives SIGTERM, the restart path of `restart_managed`:
//! its handler flushes the owned Sync runtime and exits so launchd starts the
//! helper from the new bundle. Never SIGKILL: an orphaned runtime is refused by
//! the helper lease. A helper that does not exit in time is left running.
use anyhow::{ensure, Result};
use std::time::{Duration, Instant};

/// File name of the helper executable inside `Contents/MacOS`.
pub const HELPER_NAME: &str = "okilum-sync-supervisor";
/// How long the old helper gets to flush its runtime and exit after SIGTERM.
pub const EXIT_WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(100);

/// A live process, identified the way the Sprint does: PID, owner and start
/// time together. A PID alone may be reused by an unrelated process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub uid: u32,
    /// Start time as seconds and microseconds.
    pub started: (u64, u64),
}

/// Device and inode of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId {
    pub device: u64,
    pub inode: u64,
}

/// One file-backed region of a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub file: FileId,
    /// The file has no remaining directory entry (its bundle was deleted).
    pub unlinked: bool,
    /// Last path component as the kernel reports it; empty when unknown.
    pub name: String,
}

/// Which helper image a process runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Image {
    /// It maps the current bundle's helper file.
    Current,
    /// It provably runs another helper image.
    Replaced,
    /// Not established; never acted upon.
    Unknown(String),
}

/// `complete` is true only when the region walk reached the end of the map.
pub fn classify(mappings: &[Mapping], complete: bool, current: FileId) -> Image {
    if mappings.iter().any(|mapping| mapping.file == current) {
        return Image::Current;
    }
    if !complete {
        return Image::Unknown("process memory map was not fully enumerated".into());
    }
    if mappings
        .iter()
        .any(|mapping| mapping.unlinked || mapping.name == HELPER_NAME)
    {
        Image::Replaced
    } else {
        Image::Unknown("no replaced helper image is mapped".into())
    }
}

/// Result of `launchctl kickstart` for the helper job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kick {
    Started,
    /// Exit status and launchctl's own message.
    Failed(String),
    /// No answer in time; launchd still restarts the job by itself.
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// launchd runs no helper; it starts one from the current bundle.
    NotRunning,
    /// The helper already runs the current bundle's image.
    Current,
    /// Not provably replaced, or its identity changed while it was inspected.
    Unknown(String),
    /// The old helper is gone. `signalled` is false when it exited before the
    /// signal (for example after `prepare` stopped its runtime).
    Restarted {
        old: Process,
        signalled: bool,
        kick: Kick,
    },
    /// Still running `EXIT_WAIT` after SIGTERM; left alone, no SIGKILL.
    LeftRunning { old: Process },
}

/// Native observations and effects. The production adapter is `Launchd`.
pub trait HelperOs {
    /// launchd's current PID for the account helper job; None when none runs.
    fn launchd_pid(&mut self) -> Result<Option<u32>>;
    /// The live, non-zombie process with this PID; None when it is gone.
    fn process(&mut self, pid: u32) -> Result<Option<Process>>;
    fn image(&mut self, pid: u32) -> Result<Image>;
    /// SIGTERM to this PID. Implementations never send SIGKILL.
    fn terminate(&mut self, pid: u32) -> Result<()>;
    fn kickstart(&mut self) -> Kick;
    fn sleep(&mut self, duration: Duration);
}

/// Restart the account helper only when it provably runs a replaced image.
///
/// `prepare` runs once the helper is classified as replaced and before any
/// signal: the caller verifies the installed bundle and binding there and
/// stops the authenticated runtime of a helper too old to clean up on SIGTERM.
/// An error from `prepare` sends no signal.
pub fn restart_replaced<O: HelperOs>(
    os: &mut O,
    uid: u32,
    wait: Duration,
    prepare: impl FnOnce() -> Result<()>,
) -> Result<Outcome> {
    let Some(pid) = os.launchd_pid()? else {
        return Ok(Outcome::NotRunning);
    };
    let Some(old) = os.process(pid)? else {
        return Ok(Outcome::NotRunning);
    };
    ensure!(
        old.uid == uid,
        "Sync helper process belongs to another user"
    );
    match os.image(pid)? {
        Image::Current => return Ok(Outcome::Current),
        Image::Unknown(reason) => return Ok(Outcome::Unknown(reason)),
        Image::Replaced => {}
    }
    // The image was read from this PID; it must still be the same process.
    if os.process(pid)? != Some(old) {
        return Ok(Outcome::Unknown(
            "Sync helper changed while it was inspected".into(),
        ));
    }
    prepare()?;
    // Revalidated immediately before the signal: launchd's job, the same
    // process and still the replaced image. A newer helper is never signalled.
    let signalled = if os.process(pid)? != Some(old) {
        false
    } else if os.launchd_pid()? == Some(pid) && os.image(pid)? == Image::Replaced {
        // The image read may race with an exit and PID reuse; check once more.
        if os.process(pid)? != Some(old) {
            false
        } else {
            os.terminate(pid)?;
            true
        }
    } else {
        return Ok(Outcome::Unknown(
            "Sync helper is no longer launchd's replaced helper; not signalled".into(),
        ));
    };
    let deadline = Instant::now() + wait;
    while os.process(pid)? == Some(old) {
        if Instant::now() >= deadline {
            return Ok(Outcome::LeftRunning { old });
        }
        os.sleep(POLL);
    }
    let kick = os.kickstart();
    Ok(Outcome::Restarted {
        old,
        signalled,
        kick,
    })
}

/// The production adapter: launchd's job `gui/<uid>/com.befeast.okilum.sync`
/// and the current bundle's helper file.
#[cfg(target_os = "macos")]
pub struct Launchd {
    pub uid: u32,
    pub current: std::path::PathBuf,
}

#[cfg(target_os = "macos")]
impl HelperOs for Launchd {
    fn launchd_pid(&mut self) -> Result<Option<u32>> {
        super::health::account_helper_pid()
    }
    fn process(&mut self, pid: u32) -> Result<Option<Process>> {
        native::process(pid)
    }
    fn image(&mut self, pid: u32) -> Result<Image> {
        native::image(pid, &self.current)
    }
    fn terminate(&mut self, pid: u32) -> Result<()> {
        native::terminate(pid)
    }
    fn kickstart(&mut self) -> Kick {
        native::kickstart(&format!("gui/{}/{}", self.uid, super::health::LABEL))
    }
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration)
    }
}

#[cfg(target_os = "macos")]
pub mod native {
    use super::{classify, FileId, Image, Kick, Mapping, Process};
    use anyhow::{bail, ensure, Context, Result};
    use std::{
        io::Read,
        path::Path,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    /// `PROC_PIDREGIONPATHINFO` from `<sys/proc_info.h>`; not in the libc crate.
    const PROC_PIDREGIONPATHINFO: libc::c_int = 8;
    /// Darwin `SZOMB`.
    const ZOMBIE: u32 = 5;
    /// A helper maps a few hundred regions; this only bounds a pathological walk.
    const MAX_REGIONS: usize = 200_000;

    /// `struct proc_regioninfo`; only the address range is read.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    struct RegionInfo {
        protection: u32,
        max_protection: u32,
        inheritance: u32,
        flags: u32,
        offset: u64,
        behavior: u32,
        user_wired_count: u32,
        user_tag: u32,
        pages_resident: u32,
        pages_shared_now_private: u32,
        pages_swapped_out: u32,
        pages_dirtied: u32,
        ref_count: u32,
        shadow_depth: u32,
        share_mode: u32,
        private_pages_resident: u32,
        shared_pages_resident: u32,
        obj_id: u32,
        depth: u32,
        address: u64,
        size: u64,
    }
    /// `struct proc_regionwithpathinfo`.
    #[repr(C)]
    struct RegionWithPath {
        region: RegionInfo,
        vnode: libc::vnode_info_path,
    }
    const _: () = assert!(std::mem::size_of::<RegionInfo>() == 96);
    const _: () = assert!(std::mem::size_of::<RegionWithPath>() == 96 + 1176);

    pub fn process(pid: u32) -> Result<Option<Process>> {
        ensure!(pid > 0 && pid <= i32::MAX as u32, "invalid process ID");
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if read <= 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            bail!("process observation unavailable: {error}");
        }
        ensure!(read == size, "process observation incomplete");
        if info.pbi_pid != pid || info.pbi_status == ZOMBIE {
            return Ok(None);
        }
        Ok(Some(Process {
            pid,
            uid: info.pbi_uid,
            started: (info.pbi_start_tvsec, info.pbi_start_tvusec),
        }))
    }

    pub fn file_id(path: &Path) -> Result<FileId> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("current Sync helper unavailable: {}", path.display()))?;
        // `st_dev` is a 32-bit dev_t, as is the kernel's `vst_dev`.
        Ok(FileId {
            device: u64::from(metadata.dev() as u32),
            inode: metadata.ino(),
        })
    }

    /// Every file-backed region of `pid`, and whether the walk reached the end.
    pub fn mappings(pid: u32) -> Result<(Vec<Mapping>, bool)> {
        ensure!(pid > 0 && pid <= i32::MAX as u32, "invalid process ID");
        let mut mappings: Vec<Mapping> = Vec::new();
        let mut address = 0u64;
        for _ in 0..MAX_REGIONS {
            let mut info: RegionWithPath = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<RegionWithPath>() as i32;
            let read = unsafe {
                libc::proc_pidinfo(
                    pid as i32,
                    PROC_PIDREGIONPATHINFO,
                    address,
                    (&mut info as *mut RegionWithPath).cast(),
                    size,
                )
            };
            if read <= 0 {
                let error = std::io::Error::last_os_error();
                // The kernel answers EINVAL when no region lies at or after
                // `address`: the end of the map.
                if error.raw_os_error() == Some(libc::EINVAL) && address > 0 {
                    return Ok((mappings, true));
                }
                bail!("process memory map unavailable: {error}");
            }
            ensure!(read == size, "process memory map entry incomplete");
            let stat = &info.vnode.vip_vi.vi_stat;
            if stat.vst_ino != 0 {
                // libc splits `char vip_path[MAXPATHLEN]` into 32 rows of 32;
                // read it as the kernel wrote it, one byte per C char.
                let raw = &info.vnode.vip_path;
                let raw = unsafe {
                    std::slice::from_raw_parts(
                        raw.as_ptr().cast::<u8>(),
                        std::mem::size_of_val(raw),
                    )
                };
                let bytes = &raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())];
                let name = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|path| path.rsplit('/').next())
                    .unwrap_or("")
                    .to_owned();
                let mapping = Mapping {
                    file: FileId {
                        device: u64::from(stat.vst_dev),
                        inode: stat.vst_ino,
                    },
                    unlinked: stat.vst_nlink == 0,
                    name,
                };
                if !mappings.contains(&mapping) {
                    mappings.push(mapping);
                }
            }
            let next = info
                .region
                .address
                .checked_add(info.region.size)
                .context("process memory map overflow")?;
            ensure!(next > address, "process memory map did not advance");
            address = next;
        }
        Ok((mappings, false))
    }

    pub fn image(pid: u32, current: &Path) -> Result<Image> {
        let current = file_id(current)?;
        let (mappings, complete) = mappings(pid)?;
        Ok(classify(&mappings, complete, current))
    }

    pub fn terminate(pid: u32) -> Result<()> {
        ensure!(pid > 1 && pid <= i32::MAX as u32, "invalid process ID");
        if unsafe { libc::kill(pid as i32, libc::SIGTERM) } != 0 {
            let error = std::io::Error::last_os_error();
            // Exited between the revalidation and the signal: nothing to stop.
            if error.raw_os_error() != Some(libc::ESRCH) {
                bail!("SIGTERM to Sync helper {pid} failed: {error}");
            }
        }
        Ok(())
    }

    /// launchctl can hang after the service has already started, so only our
    /// CLI child is bounded; its real exit status and message are reported.
    pub fn kickstart(target: &str) -> Kick {
        run_bounded(
            Command::new("/bin/launchctl").args(["kickstart", target]),
            Duration::from_secs(5),
        )
    }

    pub(super) fn run_bounded(command: &mut Command, budget: Duration) -> Kick {
        let spawned = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => return Kick::Failed(format!("cannot run launchctl: {error}")),
        };
        let deadline = Instant::now() + budget;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Kick::Started,
                Ok(Some(status)) => {
                    let mut message = String::new();
                    if let Some(stderr) = child.stderr.take() {
                        let _ = stderr.take(4096).read_to_string(&mut message);
                    }
                    return Kick::Failed(format!("{status}: {}", message.trim()));
                }
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Kick::TimedOut;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Kick::Failed(format!("launchctl wait failed: {error}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
