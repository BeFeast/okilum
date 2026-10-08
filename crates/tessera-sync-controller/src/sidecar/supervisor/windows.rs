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
                AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject, TerminateJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{
                CreateProcessW, ResumeThread, TerminateProcess, WaitForSingleObject,
                CREATE_NO_WINDOW, CREATE_SUSPENDED, PROCESS_INFORMATION, STARTUPINFOW,
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
        })
    }
    pub fn running(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(raw(&self.process), 0) } {
            WAIT_OBJECT_0 => Ok(false),
            WAIT_TIMEOUT => Ok(true),
            _ => Err(windows::core::Error::from_win32().into()),
        }
    }
    /// Confirms the entire job is empty, not merely that the main PID exited.
    /// On timeout the caller must keep removal pending; Drop still closes the job.
    pub fn stop(&self, timeout: Duration) -> Result<()> {
        unsafe {
            TerminateJobObject(raw(&self.job), 0)?;
        }
        let deadline = Instant::now() + timeout;
        loop {
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
            if account.ActiveProcesses == 0 && !self.running()? {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "owned process tree has not exited"
            );
            std::thread::sleep(Duration::from_millis(20));
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
                std::thread::sleep(std::time::Duration::from_secs(60));
                return;
            }
            let data = args.windows(2).find(|a| a[0] == "--data").unwrap()[1].clone();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("child").spawn().unwrap();
            std::fs::write(std::path::Path::new(&data).join("child.pid"), child.id().to_string()).unwrap();
            let _ = child.wait();
        }
    "#;

    #[test]
    fn native_job_stop_and_drop_terminate_confirmed_descendant() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("fixture.rs");
        let executable = root.path().join("fixture.exe");
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
}
