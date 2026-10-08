//! Native local Task Scheduler COM transport. No PowerShell, shell, password,
//! elevation, remote scheduler connection or replace-existing registration.
use super::{canonical, Binding, Task, TaskApi};
use anyhow::{ensure, Result};
use std::{marker::PhantomData, rc::Rc};
use windows::{
    core::BSTR,
    Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            TaskScheduler::{
                IRegisteredTask, ITaskFolder, ITaskService, TaskScheduler, TASK_CREATE,
                TASK_LOGON_INTERACTIVE_TOKEN, TASK_STATE_RUNNING,
            },
            Variant::VARIANT,
        },
    },
};

/// Security and process ownership are not inferred from task names. The actual
/// implementation must use the current process token, native security-descriptor
/// parsing and owned supervisor/Job Object handles. Native acceptance is required.
pub trait TaskGuard {
    type ProcessHandles;
    fn verify_payload(&mut self, binding: &Binding) -> Result<()>;
    fn running(&mut self, task_name: &str) -> Result<bool>;
    /// Capture authenticated supervisor/child handles BEFORE scheduler Stop.
    fn capture_owned_processes(&mut self, task_name: &str) -> Result<Self::ProcessHandles>;
    fn await_exit(&mut self, handles: Self::ProcessHandles) -> Result<()>;
}
struct Apartment(PhantomData<Rc<()>>);
impl Apartment {
    fn initialize() -> Result<Self> {
        // S_FALSE is also success and requires the matching CoUninitialize.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub struct NativeTasks<G> {
    // COM references must be released before the apartment, in field drop order.
    folder: ITaskFolder,
    _service: ITaskService,
    guard: G,
    _apartment: Apartment,
}
impl<G: TaskGuard> NativeTasks<G> {
    /// Call on a dedicated controller thread; the handle cannot cross threads.
    pub fn connect(guard: G) -> Result<Self> {
        let apartment = Apartment::initialize()?;
        let service: ITaskService =
            unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)? };
        let empty = VARIANT::default();
        unsafe {
            service.Connect(&empty, &empty, &empty, &empty)?;
        }
        let folder = unsafe { service.GetFolder(&BSTR::from("\\"))? };
        Ok(Self {
            folder,
            _service: service,
            guard,
            _apartment: apartment,
        })
    }
    fn task(&self, name: &str) -> Result<Option<IRegisteredTask>> {
        validate_name(name)?;
        match unsafe { self.folder.GetTask(&BSTR::from(name)) } {
            Ok(task) => Ok(Some(task)),
            Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
    fn owned(&mut self, name: &str, definition: &str, owner: &str) -> Result<IRegisteredTask> {
        ensure!(
            super::security::current_sid()? == owner,
            "task owner differs from current user"
        );
        let task = self
            .task(name)?
            .ok_or_else(|| anyhow::anyhow!("owned task disappeared"))?;
        let sddl = unsafe { task.GetSecurityDescriptor(1)? }.to_string(); // OWNER_SECURITY_INFORMATION
        ensure!(
            super::security::descriptor_owner_sid(&sddl)? == owner,
            "task security owner changed"
        );
        ensure!(
            canonical(&unsafe { task.Xml()? }.to_string())? == canonical(definition)?,
            "task definition changed"
        );
        Ok(task)
    }
}
fn validate_name(name: &str) -> Result<()> {
    let suffix = name
        .strip_prefix("Tessera-Sync-")
        .ok_or_else(|| anyhow::anyhow!("unexpected task name"))?;
    uuid::Uuid::parse_str(suffix)?;
    Ok(())
}
impl<G: TaskGuard> TaskApi for NativeTasks<G> {
    fn current_sid(&self) -> Result<String> {
        super::security::current_sid()
    }
    fn read(&mut self, name: &str) -> Result<Option<Task>> {
        let Some(task) = self.task(name)? else {
            return Ok(None);
        };
        let sddl = unsafe { task.GetSecurityDescriptor(1)? }.to_string();
        Ok(Some(Task {
            owner_sid: super::security::descriptor_owner_sid(&sddl)?,
            definition: unsafe { task.Xml()? }.to_string(),
            running: unsafe { task.State()? } == TASK_STATE_RUNNING && self.guard.running(name)?,
        }))
    }
    fn verify_payload(&mut self, binding: &Binding) -> Result<()> {
        self.guard.verify_payload(binding)
    }
    fn create(&mut self, name: &str, definition: &str) -> Result<()> {
        validate_name(name)?;
        // The typed adapter provides the restricted XML. TASK_CREATE preserves a
        // racing foreign task instead of overwriting it with UPDATE or FORCE.
        let empty = VARIANT::default();
        unsafe {
            self.folder.RegisterTask(
                &BSTR::from(name),
                &BSTR::from(definition),
                TASK_CREATE.0,
                &empty,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            )?;
        }
        Ok(())
    }
    fn start_owned(&mut self, name: &str, definition: &str, owner: &str) -> Result<()> {
        let task = self.owned(name, definition, owner)?;
        unsafe {
            task.Run(&VARIANT::default())?;
        }
        Ok(())
    }
    fn stop_owned(&mut self, name: &str, definition: &str, owner: &str) -> Result<()> {
        let task = self.owned(name, definition, owner)?;
        let handles = self.guard.capture_owned_processes(name)?;
        unsafe {
            task.Stop(0)?;
        }
        self.guard.await_exit(handles)
    }
    fn delete_owned(&mut self, name: &str, definition: &str, owner: &str) -> Result<()> {
        let task = self.owned(name, definition, owner)?;
        ensure!(
            unsafe { task.State()? } != TASK_STATE_RUNNING && !self.guard.running(name)?,
            "owned task still running"
        );
        // Task Scheduler has no compare-and-delete API. Recheck immediately before
        // deletion; same-user concurrent replacement remains a native race gate.
        self.owned(name, definition, owner)?;
        unsafe {
            self.folder.DeleteTask(&BSTR::from(name), 0)?;
        }
        Ok(())
    }
}
