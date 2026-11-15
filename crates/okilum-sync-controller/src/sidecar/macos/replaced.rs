//! Restart of an account Sync helper that still runs a replaced bundle's image
//! (#2931). Sparkle swaps `Okilum.app` in place while the launchd agent keeps
//! running the old executable.
//!
//! Like the Sprint (#2925, `scripts/dev/okilum-sprint.sh`), the target is
//! captured BEFORE the replacement and only that identity is acted on after it:
//! - `capture`, run by the old app right before it hands the install to
//!   Sparkle: launchd's current PID for the account helper job, owned by this
//!   user, that maps exactly this bundle's `Contents/MacOS/okilum-sync-supervisor`
//!   (device and inode). A helper of any other bundle is never captured.
//! - `restart_captured`, run by the next app launch: the same PID, UID and start
//!   time, no longer mapping the file now at that path (complete region walk).
//!
//! The owned Sync runtime is stopped gracefully first and its exit confirmed
//! within a bounded wait; the helper's own SIGTERM cleanup would SIGKILL a slow
//! runtime. If it is still running, nothing is signalled and the next launch
//! retries. Only then does the captured helper, revalidated immediately before
//! the signal, receive SIGTERM (the restart path of `restart_managed`), so
//! launchd starts it from the new bundle. Never SIGKILL: an orphaned runtime is
//! refused by the helper lease. A helper that does not exit in time is left.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// File name of the helper executable inside `Contents/MacOS`.
pub const HELPER_NAME: &str = "okilum-sync-supervisor";
/// How long the old helper gets to exit after SIGTERM.
pub const EXIT_WAIT: Duration = Duration::from_secs(15);
/// How long the runtime gets to finish a graceful shutdown before the restart
/// is postponed to the next launch.
pub const RUNTIME_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

/// A live process, identified the way the Sprint does: PID, owner and start
/// time together. A PID alone may be reused by an unrelated process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub pid: u32,
    pub uid: u32,
    /// Start time as seconds and microseconds.
    pub started: (u64, u64),
}

/// What the old app records before the update replaces its bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// The exact helper path inside the bundle that is about to be replaced.
    pub executable: String,
    pub helper: Process,
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
}

/// Which file a process runs, relative to the helper file at the bundle path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Image {
    /// It maps the file at the helper path.
    Current,
    /// Every region was read and none maps the file at the helper path.
    Other,
    /// Not established; never acted upon.
    Unknown(String),
}

/// `complete` is true only when the region walk reached the end of the map.
pub fn classify(mappings: &[Mapping], complete: bool, current: FileId) -> Image {
    if mappings.iter().any(|mapping| mapping.file == current) {
        Image::Current
    } else if complete {
        Image::Other
    } else {
        Image::Unknown("process memory map was not fully enumerated".into())
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

/// What `restart_captured` did. `Gone`, `Current` and `Restarted` settle the
/// snapshot; every other outcome keeps it for the next launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The captured helper no longer exists; launchd runs the new bundle's.
    Gone,
    /// The captured helper still maps the file at its path: nothing replaced it.
    Current,
    /// Not established, or its identity changed while it was inspected.
    Unknown(String),
    /// The runtime did not finish its graceful shutdown within the wait.
    RuntimeRunning { old: Process },
    /// The old helper is gone. `signalled` is false when it exited before the
    /// signal (for example after its runtime stopped).
    Restarted {
        old: Process,
        signalled: bool,
        kick: Kick,
    },
    /// Still running `EXIT_WAIT` after SIGTERM; left alone, no SIGKILL.
    LeftRunning { old: Process },
}

impl Outcome {
    pub fn settled(&self) -> bool {
        matches!(self, Self::Gone | Self::Current | Self::Restarted { .. })
    }
}

/// Native observations and effects. The production adapter is `Launchd`.
pub trait HelperOs {
    /// launchd's current PID for the account helper job; None when none runs.
    fn launchd_pid(&mut self) -> Result<Option<u32>>;
    /// The live, non-zombie process with this PID; None when it is gone.
    fn process(&mut self, pid: u32) -> Result<Option<Process>>;
    /// Live, non-zombie direct children of `pid`.
    fn children(&mut self, pid: u32) -> Result<Vec<Process>>;
    /// The process's image relative to the file at the bundle's helper path.
    fn image(&mut self, pid: u32) -> Result<Image>;
    /// SIGTERM to this PID. Implementations never send SIGKILL.
    fn terminate(&mut self, pid: u32) -> Result<()>;
    fn kickstart(&mut self) -> Kick;
    fn sleep(&mut self, duration: Duration);
}

/// Before the bundle is replaced: launchd's helper, only when it runs exactly
/// the helper file at this bundle's path. Read-only.
pub fn capture<O: HelperOs>(os: &mut O, uid: u32) -> Result<Option<Process>> {
    let Some(pid) = os.launchd_pid()? else {
        return Ok(None);
    };
    let Some(helper) = os.process(pid)? else {
        return Ok(None);
    };
    ensure!(
        helper.uid == uid,
        "Sync helper process belongs to another user"
    );
    if os.image(pid)? != Image::Current {
        return Ok(None);
    }
    // The image was read from this PID; it must still be launchd's same process.
    if os.process(pid)? != Some(helper) || os.launchd_pid()? != Some(pid) {
        return Ok(None);
    }
    Ok(Some(helper))
}

#[derive(Clone, Copy, Debug)]
pub struct Waits {
    pub runtime: Duration,
    pub exit: Duration,
}
impl Default for Waits {
    fn default() -> Self {
        Self {
            runtime: RUNTIME_WAIT,
            exit: EXIT_WAIT,
        }
    }
}

fn any_alive<O: HelperOs>(os: &mut O, processes: &[Process]) -> Result<bool> {
    for process in processes {
        if os.process(process.pid)? == Some(*process) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// After the replacement: restart the captured helper `old` only.
///
/// `prepare` runs once `old` is still alive and no longer runs the file at its
/// bundle path, before any effect: the caller verifies the installed bundle and
/// binding there. `stop_runtime` asks the helper's runtime to shut down
/// gracefully; its exit is then awaited for `waits.runtime`. An error from
/// either sends no signal.
pub fn restart_captured<O: HelperOs, S>(
    os: &mut O,
    uid: u32,
    old: Process,
    waits: Waits,
    prepare: impl FnOnce() -> Result<S>,
    stop_runtime: impl FnOnce(&S) -> Result<()>,
) -> Result<Outcome> {
    ensure!(
        old.uid == uid,
        "captured Sync helper belongs to another user"
    );
    let pid = old.pid;
    if os.process(pid)? != Some(old) {
        return Ok(Outcome::Gone);
    }
    match os.image(pid)? {
        Image::Current => return Ok(Outcome::Current),
        Image::Unknown(reason) => return Ok(Outcome::Unknown(reason)),
        Image::Other => {}
    }
    // The image was read from this PID; it must still be the same process.
    if os.process(pid)? != Some(old) {
        return Ok(Outcome::Gone);
    }
    let prepared = prepare()?;
    let runtime = os.children(pid)?;
    if !runtime.is_empty() {
        stop_runtime(&prepared)?;
        let deadline = Instant::now() + waits.runtime;
        while any_alive(os, &runtime)? {
            if Instant::now() >= deadline {
                return Ok(Outcome::RuntimeRunning { old });
            }
            os.sleep(POLL);
        }
    }
    // Revalidated immediately before the signal: launchd's job, the same
    // process, still not the bundle's file, no runtime left to interrupt.
    let signalled = if os.process(pid)? != Some(old) {
        false
    } else if os.launchd_pid()? == Some(pid)
        && os.image(pid)? == Image::Other
        && os.children(pid)?.is_empty()
    {
        // The reads above may race with an exit and PID reuse; check once more.
        if os.process(pid)? != Some(old) {
            false
        } else {
            os.terminate(pid)?;
            true
        }
    } else {
        return Ok(Outcome::Unknown(
            "captured Sync helper changed before the signal; not signalled".into(),
        ));
    };
    let deadline = Instant::now() + waits.exit;
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
/// and the helper file at this bundle's path.
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
    fn children(&mut self, pid: u32) -> Result<Vec<Process>> {
        native::children(pid)
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

    /// The live, non-zombie process and its parent PID.
    fn observe(pid: u32) -> Result<Option<(Process, u32)>> {
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
        Ok(Some((
            Process {
                pid,
                uid: info.pbi_uid,
                started: (info.pbi_start_tvsec, info.pbi_start_tvusec),
            },
            info.pbi_ppid,
        )))
    }

    pub fn process(pid: u32) -> Result<Option<Process>> {
        Ok(observe(pid)?.map(|(process, _)| process))
    }

    /// Live direct children of `pid`, each confirmed by its own parent PID.
    pub fn children(pid: u32) -> Result<Vec<Process>> {
        ensure!(pid > 0 && pid <= i32::MAX as u32, "invalid process ID");
        let mut pids: Vec<libc::pid_t> = vec![0; 4096];
        let capacity = std::mem::size_of_val(pids.as_slice()) as i32;
        let count =
            unsafe { libc::proc_listchildpids(pid as i32, pids.as_mut_ptr().cast(), capacity) };
        if count < 0 {
            bail!(
                "child process listing unavailable: {}",
                std::io::Error::last_os_error()
            );
        }
        // libproc reports a PID count; reading at most the buffer is correct
        // even if it reported bytes, since unused entries stay zero.
        let listed = (count as usize).min(pids.len());
        ensure!(listed < pids.len(), "child process listing incomplete");
        let mut children = Vec::new();
        for &child in &pids[..listed] {
            if child <= 0 {
                continue;
            }
            if let Some((process, parent)) = observe(child as u32)? {
                if parent == pid && !children.contains(&process) {
                    children.push(process);
                }
            }
        }
        Ok(children)
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
                let mapping = Mapping {
                    file: FileId {
                        device: u64::from(stat.vst_dev),
                        inode: stat.vst_ino,
                    },
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
