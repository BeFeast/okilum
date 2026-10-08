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
