//! Signed Task Scheduler adapter. Unknown native state stays recovery pending;
//! the production guard never falls back to scheduler termination or PID kills.
use super::{
    connection,
    native::TaskGuard,
    preparation::{Prepared, Sources},
    signature::AuthenticodePolicy,
};
use crate::sidecar::{
    authority::StopToken,
    discovery::{self, Discovered, WindowsConnector},
    store::{Selection, WindowsStore},
    supervisor::ipc::{windows_discovery::ImagePolicy, Scope},
    Binding,
};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Read,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

pub(super) struct Guard {
    binding: Binding,
    state: PathBuf,
    app: PathBuf,
    helper_hash: String,
    selection: Selection,
    store: WindowsStore,
    policy: Arc<AuthenticodePolicy>,
    stopped: bool,
}
impl Guard {
    pub fn new(
        prepared: &Prepared,
        sources: &Sources,
        store: WindowsStore,
        state: &Path,
    ) -> Result<Self> {
        let selection = store
            .read_selection(Instant::now() + Duration::from_secs(5))?
            .context("prepared runtime selection missing")?;
        ensure!(
            selection.digest() == prepared.runtime_hash,
            "runtime selection changed after preparation"
        );
        Ok(Self {
            binding: prepared.binding.clone(),
            state: state.to_owned(),
            app: sources.app.clone(),
            helper_hash: prepared.helper_hash.clone(),
            selection,
            store,
            policy: Arc::new(sources.policy.clone()),
            stopped: false,
        })
    }
    fn require_task(&self, name: &str) -> Result<()> {
        ensure!(
            name == super::task_name(&self.binding),
            "native task differs from its prepared identity"
        );
        Ok(())
    }
    fn observed(&self) -> Result<Option<Scope>> {
        if self.stopped {
            return Ok(None);
        }
        match discovery::discover_detailed(
            &self.store,
            &self.binding,
            &WindowsConnector::new(self.policy.clone()),
            Duration::from_secs(3),
        ) {
            Discovered::Live(scope) => Ok(Some(scope)),
            Discovered::Absent => Ok(None),
            Discovered::Unreachable(reason) => {
                anyhow::bail!("supervisor recovery pending: {reason}")
            }
        }
    }
}
/// No ancestor opens here: Controller may hold WindowsStore's exclusive state
/// directory transaction. Its root handle pins that boundary; the immutable
/// selected files are opened without write/delete sharing for the complete read.
fn retained_hash(path: &Path) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    okilum_write_authority::filesystem_identity::from_file(&file)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 100_000_000,
        "unsafe retained supervisor executable"
    );
    let mut hasher = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hasher.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
impl TaskGuard for Guard {
    type ProcessHandles = ();
    fn verify_payload(&mut self, binding: &Binding) -> Result<()> {
        ensure!(
            binding == &self.binding && binding.owner == super::security::current_sid()?,
            "prepared task binding changed"
        );
        self.policy.verify_image(&self.app)?;
        self.policy.verify_image(Path::new(&binding.supervisor))?;
        ensure!(
            retained_hash(Path::new(&binding.supervisor))? == self.helper_hash,
            "retained supervisor executable changed"
        );
        let runtime = self.selection.verify_file(&self.state)?;
        ensure!(
            super::payload::verify(&runtime, &self.policy)? == self.selection.digest(),
            "retained runtime changed"
        );
        Ok(())
    }
    fn running(&mut self, name: &str) -> Result<bool> {
        self.require_task(name)?;
        Ok(self.observed()?.is_some())
    }
    fn capture_owned_processes(&mut self, name: &str) -> Result<()> {
        self.require_task(name)?;
        anyhow::bail!("authenticated owned-tree Stop is required; scheduler fallback remains recovery pending")
    }
    fn await_exit(&mut self, _: ()) -> Result<()> {
        anyhow::bail!("no captured authenticated Stop can certify this exit")
    }
    fn supervisor_scope(&mut self, binding: &Binding) -> Result<Option<Scope>> {
        ensure!(binding == &self.binding, "supervisor scope binding changed");
        self.observed()
    }
    fn stop_supervisor(&mut self, binding: &Binding, token: &StopToken) -> Result<()> {
        ensure!(binding == &self.binding, "supervisor stop binding changed");
        connection::stop_and_wait(
            &self.store,
            binding,
            token,
            self.policy.clone(),
            Instant::now() + Duration::from_secs(20),
        )?;
        self.stopped = true;
        Ok(())
    }
}
