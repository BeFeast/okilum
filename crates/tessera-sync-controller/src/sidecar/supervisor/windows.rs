//! Owned Windows process tree. No PID-based kill or shell launch. Not wired into
//! a shipped supervisor until signature, state ownership and IPC gates are met.
use super::Launch;
use anyhow::{ensure, Result};
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    time::{Duration, Instant},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
                JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject, TerminateJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{
                CreateProcessW, GetProcessId, OpenProcess, ResumeThread, TerminateProcess,
                WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, PROCESS_INFORMATION,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, STARTUPINFOW,
            },
        },
    },
};
fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

// Until the primary thread is attached to our Job Object, every exit path must
// terminate the suspended child. Closing process/thread handles does not do so.
struct Suspended {
    process: Option<OwnedHandle>,
    thread: OwnedHandle,
}
impl Drop for Suspended {
    fn drop(&mut self) {
        if let Some(process) = &self.process {
            unsafe {
                let _ = TerminateProcess(raw(process), 1);
                let _ = WaitForSingleObject(raw(process), 5000);
            }
        }
    }
}

pub struct JobChild {
    // Closing the unnamed, non-inherited job kills every process still attached.
    job: OwnedHandle,
    process: OwnedHandle,
    // Keep process objects alive so their IDs cannot be reused between capture
    // and terminal completion. Never use these IDs as termination targets.
    descendants: Vec<OwnedHandle>,
}

const MAX_JOB_PROCESSES: usize = 256;
#[repr(C)]
struct ProcessList {
    assigned: u32,
    listed: u32,
    ids: [usize; MAX_JOB_PROCESSES],
}
impl JobChild {
    /// The caller must first verify the immutable staged payload signature/hash
    /// and private config/data ownership. Creation is an explicit Enable effect.
    pub fn spawn(launch: &Launch) -> Result<Self> {
        let mut command = launch.windows_command_line()?;
        let executable = wide(&launch.executable);
        let directory = wide(&launch.data);
        let job =
            unsafe { OwnedHandle::from_raw_handle(CreateJobObjectW(None, PCWSTR::null())?.0) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
        }
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR(executable.as_ptr()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_SUSPENDED | CREATE_NO_WINDOW,
                None,
                PCWSTR(directory.as_ptr()),
                &startup,
                &mut process,
            )?;
        }
        let mut suspended = Suspended {
            process: Some(unsafe { OwnedHandle::from_raw_handle(process.hProcess.0) }),
            thread: unsafe { OwnedHandle::from_raw_handle(process.hThread.0) },
        };
        unsafe {
            AssignProcessToJobObject(raw(&job), raw(suspended.process.as_ref().unwrap()))?;
            if ResumeThread(raw(&suspended.thread)) == u32::MAX {
                return Err(windows::core::Error::from_win32().into());
            }
        }
        Ok(Self {
            job,
            process: suspended.process.take().unwrap(),
            descendants: Vec::new(),
        })
    }
    /// A live descendant keeps the runtime running even after its root exits.
    /// Complete exit also requires signaled captured descendants. If a process
    /// disappeared before capture, status fails instead of certifying exit.
    pub fn running(&self) -> Result<bool> {
        let account = self.accounting()?;
        if account.ActiveProcesses != 0 || self.root_running()? {
            return Ok(true);
        }
        // Accounting can reach zero before process handles become signaled.
        // A vanished, uncaptured descendant cannot be certified after the fact.
        ensure!(
            account.TotalProcesses as usize == self.descendants.len() + 1,
            "owned job completion has uncaptured processes"
        );
        for process in &self.descendants {
            match unsafe { WaitForSingleObject(raw(process), 0) } {
                WAIT_OBJECT_0 => (),
                WAIT_TIMEOUT => return Ok(true),
                _ => return Err(windows::core::Error::from_win32().into()),
            }
        }
        Ok(false)
    }
    fn root_running(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(raw(&self.process), 0) } {
            WAIT_OBJECT_0 => Ok(false),
            WAIT_TIMEOUT => Ok(true),
            _ => Err(windows::core::Error::from_win32().into()),
        }
    }
    fn accounting(&self) -> Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
        let mut account = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(raw(&self.job)),
                JobObjectBasicAccountingInformation,
                &mut account as *mut _ as _,
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )?;
        }
        Ok(account)
    }
    fn capture_descendants(&mut self, deadline: Instant) -> Result<()> {
        let mut list = ProcessList {
            assigned: 0,
            listed: 0,
            ids: [0; MAX_JOB_PROCESSES],
        };
        unsafe {
            QueryInformationJobObject(
                Some(raw(&self.job)),
                JobObjectBasicProcessIdList,
                &mut list as *mut _ as _,
                size_of::<ProcessList>() as u32,
                None,
            )?;
        }
        ensure!(
            list.assigned == list.listed && list.listed as usize <= MAX_JOB_PROCESSES,
            "owned job process inventory incomplete"
        );
        let root = unsafe { GetProcessId(raw(&self.process)) } as usize;
        for id in &list.ids[..list.listed as usize] {
            ensure!(
                Instant::now() < deadline,
                "owned process stop deadline expired"
            );
            if *id == root
                || self
                    .descendants
                    .iter()
                    .any(|p| unsafe { GetProcessId(raw(p)) } as usize == *id)
            {
                continue;
            }
            ensure!(
                self.descendants.len() + 1 < MAX_JOB_PROCESSES,
                "owned job process limit exceeded"
            );
            let pid = u32::try_from(*id)?;
            let process = unsafe {
                OwnedHandle::from_raw_handle(
                    OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        false,
                        pid,
                    )?
                    .0,
                )
            };
            // The enumerated PID is not authority. Verify the captured process
            // object belongs to this unnamed, non-inherited job before retaining
            // it. An exit/reuse race fails, never adopts an unrelated process.
            let mut belongs = Default::default();
            unsafe {
                IsProcessInJob(raw(&process), Some(raw(&self.job)), &mut belongs)?;
            }
            ensure!(belongs.as_bool(), "captured process is not in owned job");
            self.descendants.push(process);
        }
        ensure!(
            self.accounting()?.TotalProcesses as usize == self.descendants.len() + 1,
            "owned job completion has uncaptured processes"
        );
        Ok(())
    }
    /// Convenience for callers without an existing operation budget. IPC/hook
    /// integration must use stop_until with its original absolute deadline.
    pub fn stop(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow::anyhow!("invalid owned process stop timeout"))?;
        self.stop_until(deadline)
    }
    /// Terminate and confirm the entire owned job within an existing budget.
    /// An expired budget refuses termination. An error after termination is not
    /// evidence of exit; retain pending intent and ownership for reconciliation.
    /// Native queries/termination are synchronous; this bounds polling, not an
    /// OS call that stalls. Drop still closes the kill-on-close job.
    pub fn stop_until(&mut self, deadline: Instant) -> Result<()> {
        ensure!(
            Instant::now() < deadline,
            "owned process stop deadline expired"
        );
        self.capture_descendants(deadline)?;
        ensure!(
            Instant::now() < deadline,
            "owned process stop deadline expired"
        );
        unsafe {
            TerminateJobObject(raw(&self.job), 0)?;
        }
        loop {
            ensure!(
                Instant::now() < deadline,
                "owned process stop deadline expired"
            );
            let running = self.running()?;
            let now = Instant::now();
            // A late successful query cannot convert an exhausted budget into
            // success or authorize unregister/removal in this exchange.
            ensure!(now < deadline, "owned process stop deadline expired");
            if !running {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20).min(deadline - now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

    // A real, disposable native process tree, using the fixed production Launch
    // argv. No shell, Syncthing install, task registration or personal state.
    const FIXTURE: &str = r#"
        fn main() {
            let args: Vec<_> = std::env::args().collect();
            if args.get(1).map(String::as_str) == Some("child") {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while std::time::Instant::now() < until {
                    if std::path::Path::new(&args[2]).join("release-child").exists() { break; }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                return;
            }
            let data = args.windows(2).find(|a| a[0] == "--data").unwrap()[1].clone();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("child").arg(&data).spawn().unwrap();
            std::fs::write(std::path::Path::new(&data).join("child.pid"), child.id().to_string()).unwrap();
            if std::path::Path::new(&data).join("exit-parent").exists() {
                return;
            }
            let _ = child.wait();
        }
    "#;

    fn compile_fixture(root: &std::path::Path) -> Result<std::path::PathBuf> {
        let source = root.join("fixture.rs");
        let executable = root.join("fixture.exe");
        fs::write(&source, FIXTURE)?;
        let compiled = Command::new("rustc")
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()?;
        ensure!(
            compiled.status.success(),
            "fixture compilation: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        Ok(executable)
    }

    #[test]
    fn native_job_stop_and_drop_terminate_confirmed_descendant() -> Result<()> {
        let root = tempfile::tempdir()?;
        let executable = compile_fixture(root.path())?;
        for stop_explicitly in [true, false] {
            let data = root
                .path()
                .join(if stop_explicitly { "stop" } else { "drop" });
            let config = root.path().join(if stop_explicitly {
                "config-stop"
            } else {
                "config-drop"
            });
            fs::create_dir(&data)?;
            fs::create_dir(&config)?;
            let mut job = JobChild::spawn(&Launch {
                executable: executable.to_string_lossy().into_owned(),
                config: config.to_string_lossy().into_owned(),
                data: data.to_string_lossy().into_owned(),
            })?;
            let deadline = Instant::now() + Duration::from_secs(15);
            let pid = loop {
                if let Ok(text) = fs::read_to_string(data.join("child.pid")) {
                    if let Ok(pid) = text.parse::<u32>() {
                        break pid;
                    }
                }
                ensure!(job.running()?, "fixture exited before creating descendant");
                ensure!(
                    Instant::now() < deadline,
                    "fixture descendant readiness timed out"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            let descendant = unsafe {
                OwnedHandle::from_raw_handle(OpenProcess(PROCESS_SYNCHRONIZE, false, pid)?.0)
            };
            let mut account = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            unsafe {
                QueryInformationJobObject(
                    Some(raw(&job.job)),
                    JobObjectBasicAccountingInformation,
                    &mut account as *mut _ as _,
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    None,
                )?;
                ensure!(
                    WaitForSingleObject(raw(&descendant), 0) == WAIT_TIMEOUT,
                    "descendant must be alive before termination"
                );
            }
            ensure!(
                account.ActiveProcesses >= 2,
                "positive control: job must contain parent and descendant"
            );
            if stop_explicitly {
                job.stop(Duration::from_secs(10))?;
                ensure!(!job.running()?, "parent still running after stop");
            }
            drop(job);
            ensure!(
                unsafe { WaitForSingleObject(raw(&descendant), 10000) } == WAIT_OBJECT_0,
                "descendant survived {}",
                if stop_explicitly { "stop" } else { "job close" }
            );
        }
        Ok(())
    }

    fn live_fixture(root: &std::path::Path, exit_parent: bool) -> Result<(JobChild, OwnedHandle)> {
        let executable = compile_fixture(root)?;
        let data = root.join("data");
        let config = root.join("config");
        fs::create_dir(&data)?;
        fs::create_dir(&config)?;
        if exit_parent {
            fs::write(data.join("exit-parent"), b"")?;
        }
        let job = JobChild::spawn(&Launch {
            executable: executable.to_string_lossy().into_owned(),
            config: config.to_string_lossy().into_owned(),
            data: data.to_string_lossy().into_owned(),
        })?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let pid = loop {
            if let Ok(text) = fs::read_to_string(data.join("child.pid")) {
                if let Ok(pid) = text.parse::<u32>() {
                    break pid;
                }
            }
            ensure!(Instant::now() < deadline, "descendant readiness timed out");
            std::thread::sleep(Duration::from_millis(20));
        };
        let descendant = unsafe {
            OwnedHandle::from_raw_handle(OpenProcess(PROCESS_SYNCHRONIZE, false, pid)?.0)
        };
        ensure!(
            unsafe { WaitForSingleObject(raw(&descendant), 0) } == WAIT_TIMEOUT,
            "positive control: descendant must be alive"
        );
        Ok((job, descendant))
    }

    #[test]
    fn native_job_reports_live_descendant_after_root_exit() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (mut job, descendant) = live_fixture(root.path(), true)?;
        ensure!(
            unsafe { WaitForSingleObject(raw(&job.process), 10000) } == WAIT_OBJECT_0,
            "fixture root did not exit"
        );
        ensure!(!job.root_running()?, "root must be exited");
        ensure!(
            job.accounting()?.ActiveProcesses >= 1,
            "descendant missing from owned job"
        );
        ensure!(job.running()?, "root exit must not hide a live descendant");
        job.stop_until(Instant::now() + Duration::from_secs(10))?;
        ensure!(!job.running()?, "job still running after confirmed stop");
        ensure!(
            unsafe { WaitForSingleObject(raw(&descendant), 0) } == WAIT_OBJECT_0,
            "descendant survived confirmed stop"
        );
        eprintln!("owned job: exited root with live descendant reported running; stop reaped both");
        Ok(())
    }

    #[test]
    fn native_job_expired_deadline_refuses_termination_with_stop_positive_control() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (mut job, descendant) = live_fixture(root.path(), false)?;
        let error = job.stop_until(Instant::now()).unwrap_err();
        ensure!(error.to_string().contains("deadline expired"), "{error:#}");
        ensure!(job.root_running()?, "expired stop terminated root");
        ensure!(
            job.accounting()?.ActiveProcesses >= 2,
            "expired stop changed owned tree"
        );
        ensure!(
            unsafe { WaitForSingleObject(raw(&descendant), 0) } == WAIT_TIMEOUT,
            "expired stop terminated descendant"
        );
        // The same job must respond to a valid budget: refusal above is not a
        // broken termination probe. No child is selected for termination by PID.
        job.stop_until(Instant::now() + Duration::from_secs(10))?;
        ensure!(!job.running()?, "positive stop did not empty job");
        ensure!(
            unsafe { WaitForSingleObject(raw(&descendant), 0) } == WAIT_OBJECT_0,
            "positive stop did not reap descendant"
        );
        eprintln!("owned job: expired budget preserved live tree; valid budget reaped same tree");
        Ok(())
    }

    #[test]
    fn native_job_uncaptured_exit_cannot_authorize_stopped() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (mut job, descendant) = live_fixture(root.path(), false)?;
        ensure!(
            job.running()?,
            "positive control: initial tree must be running"
        );
        fs::write(root.path().join("data/release-child"), b"")?;
        ensure!(
            unsafe { WaitForSingleObject(raw(&descendant), 10000) } == WAIT_OBJECT_0,
            "fixture descendant did not exit"
        );
        ensure!(
            unsafe { WaitForSingleObject(raw(&job.process), 10000) } == WAIT_OBJECT_0,
            "fixture root did not exit"
        );
        // Only the fixture owns the descendant handle. Production cannot use
        // an empty job to manufacture a terminal witness for an uncaptured exit.
        // Handle signaling and job accounting updates are not ordered. Wait
        // for the negative probe's empty-accounting precondition separately;
        // the production stop tests still require signaled handles on return.
        let ready = Instant::now() + Duration::from_secs(10);
        while job.accounting()?.ActiveProcesses != 0 {
            ensure!(Instant::now() < ready, "fixture job did not become empty");
            std::thread::sleep(Duration::from_millis(10));
        }
        let error = job.running().unwrap_err();
        ensure!(error.to_string().contains("uncaptured"), "{error:#}");
        ensure!(
            job.stop_until(Instant::now() + Duration::from_secs(10))
                .is_err(),
            "uncaptured historical process must not authorize successful stop"
        );
        eprintln!("owned job: uncaptured completed descendant refused; empty accounting is not terminal proof");
        Ok(())
    }
}
