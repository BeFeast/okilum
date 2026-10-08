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
            canonical(&super::resolve_task_accounts(
                &unsafe { task.Xml()? }.to_string(),
                super::security::resolve_account_sid
            )?)? == canonical(definition)?,
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
            definition: super::resolve_task_accounts(
                &unsafe { task.Xml()? }.to_string(),
                super::security::resolve_account_sid,
            )?,
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
        // The scheduler's default security owner can be the token's primary
        // owner (for example Administrators), not its user SID. Later ownership
        // checks intentionally require the user SID, so bind it explicitly.
        // O: alone leaves the scheduler's default access rules unchanged.
        let owner = super::security::current_sid()?;
        let security = VARIANT::from(BSTR::from(format!("O:{owner}")));
        unsafe {
            self.folder.RegisterTask(
                &BSTR::from(name),
                &BSTR::from(definition),
                TASK_CREATE.0,
                &empty,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &security,
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

#[cfg(test)]
mod tests {
    use super::*;

    // Registration-only transport test. Execution authentication is deliberately
    // unavailable; this must not be mistaken for supervisor lifecycle acceptance.
    struct NoExecution;
    impl TaskGuard for NoExecution {
        type ProcessHandles = ();
        fn verify_payload(&mut self, _: &Binding) -> Result<()> {
            anyhow::bail!("registration fixture cannot execute")
        }
        fn running(&mut self, _: &str) -> Result<bool> {
            Ok(false)
        }
        fn capture_owned_processes(&mut self, _: &str) -> Result<()> {
            anyhow::bail!("registration fixture cannot execute")
        }
        fn await_exit(&mut self, _: ()) -> Result<()> {
            anyhow::bail!("registration fixture cannot execute")
        }
    }
    struct FixtureTask {
        api: NativeTasks<NoExecution>,
        name: String,
        created: bool,
    }
    impl Drop for FixtureTask {
        fn drop(&mut self) {
            if self.created {
                // Only the random task successfully created by this test. Also
                // runs during panic so a failing assertion leaves no logon task.
                unsafe {
                    let _ = self.api.folder.DeleteTask(&BSTR::from(&self.name), 0);
                }
            }
        }
    }

    fn print_registration(fixture: &FixtureTask, expected: &str, label: &str) -> Result<()> {
        let task = fixture
            .api
            .task(&fixture.name)?
            .ok_or_else(|| anyhow::anyhow!("diagnostic task missing"))?;
        unsafe {
            let sddl = task.GetSecurityDescriptor(1)?.to_string();
            let actual = super::super::security::descriptor_owner_sid(&sddl)?;
            let mut principal = BSTR::new();
            task.Definition()?.Principal()?.UserId(&mut principal)?;
            let principal = principal.to_string();
            let state = task.State()?.0;
            eprintln!("{label}: process_sid={expected}; owner_sid={actual}; owner_sddl={sddl}; principal={principal}; scheduler_state={state}");
        }
        Ok(())
    }

    #[test]
    fn native_task_registration_collision_and_owned_removal() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut fixture = FixtureTask {
            api: NativeTasks::connect(NoExecution)?,
            name: format!("Tessera-Sync-{}", uuid::Uuid::new_v4()),
            created: false,
        };
        let owner = fixture.api.current_sid()?;
        let binding = Binding {
            instance: uuid::Uuid::parse_str(fixture.name.strip_prefix("Tessera-Sync-").unwrap())?,
            installation: uuid::Uuid::new_v4(),
            owner: owner.clone(),
            supervisor: root
                .path()
                .join("never-executed.exe")
                .to_string_lossy()
                .into_owned(),
            state_directory: root.path().to_string_lossy().into_owned(),
            device_identity: "native-registration-fixture".into(),
        };
        // Disable the disposable registration: no logon/retry process may start.
        let xml = super::super::definition(&binding)?
            .replace("<Settings>", "<Settings><Enabled>false</Enabled>");
        ensure!(
            fixture.api.read(&fixture.name)?.is_none(),
            "fixture name collision"
        );
        // Observe the scheduler defaults in the same runner/token before the
        // explicit-owner registration. This disabled fixture never executes.
        let empty = VARIANT::default();
        unsafe {
            fixture.api.folder.RegisterTask(
                &BSTR::from(&fixture.name),
                &BSTR::from(&xml),
                TASK_CREATE.0,
                &empty,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            )?;
        }
        fixture.created = true;
        print_registration(&fixture, &owner, "scheduler-default")?;
        unsafe {
            fixture
                .api
                .folder
                .DeleteTask(&BSTR::from(&fixture.name), 0)?;
        }
        fixture.created = false;
        fixture.api.create(&fixture.name, &xml)?;
        fixture.created = true;
        print_registration(&fixture, &owner, "explicit-owner")?;
        let task = fixture
            .api
            .read(&fixture.name)?
            .ok_or_else(|| anyhow::anyhow!("created task missing"))?;
        if canonical(&task.definition)? != canonical(&xml)? {
            eprintln!("scheduler XML differs after declaration normalization; expected={xml}; returned={}", task.definition);
        }
        ensure!(
            task.owner_sid == owner,
            "task owner mismatch: expected {owner}, got {}",
            task.owner_sid
        );
        ensure!(!task.running, "disabled fixture unexpectedly running");
        ensure!(
            fixture.api.create(&fixture.name, &xml).is_err(),
            "TASK_CREATE overwrote existing registration"
        );
        let reread = fixture.api.read(&fixture.name)?.unwrap();
        ensure!(
            reread.definition == task.definition,
            "collision changed task"
        );
        ensure!(
            fixture
                .api
                .delete_owned(&fixture.name, &xml, "S-1-5-18")
                .is_err(),
            "foreign-owner removal accepted"
        );
        let changed = xml.replace("native-registration-fixture", "changed-registration");
        ensure!(
            fixture
                .api
                .delete_owned(&fixture.name, &changed, &owner)
                .is_err(),
            "changed-definition removal accepted"
        );
        ensure!(
            fixture.api.read(&fixture.name)?.is_some(),
            "rejected removal deleted task"
        );
        fixture.api.delete_owned(&fixture.name, &xml, &owner)?;
        fixture.created = false;
        ensure!(
            fixture.api.read(&fixture.name)?.is_none(),
            "owned removal left task behind"
        );
        Ok(())
    }
}
