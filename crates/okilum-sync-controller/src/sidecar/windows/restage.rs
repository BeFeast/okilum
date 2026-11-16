//! Approved post-update helper refresh. No Velopack mutation or PID/task kill.
use super::{bootstrap, preparation, TaskScheduler};
use crate::sidecar::{
    store::{restage::Host, WindowsStore},
    Binding, Platform, Registration,
};
use anyhow::{ensure, Context, Result};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type Native = TaskScheduler<super::native::NativeTasks<super::guard::Guard>>;
struct Installed {
    state: PathBuf,
    sources: preparation::Sources,
    store: WindowsStore,
}
impl Host for Installed {
    type Native = Native;
    fn platform(&mut self, binding: &Binding) -> Result<Native> {
        let prepared = preparation::prepare(
            &self.state,
            &self.sources,
            Instant::now() + Duration::from_secs(30),
        )?;
        let prepared = preparation::for_binding(prepared, binding)?;
        let guard =
            super::guard::Guard::new(&prepared, &self.sources, self.store.clone(), &self.state)?;
        Ok(TaskScheduler(super::native::NativeTasks::connect(guard)?))
    }
    fn select_helper(&mut self, binding: &Binding, hash: &str) -> Result<()> {
        preparation::restage_helper(&self.state, &self.sources, binding, hash)
    }
    fn ready(&mut self, binding: &Binding, deadline: Instant) -> Result<()> {
        let mut native = self.platform(binding)?;
        bootstrap::wait_for_native_ready(&self.store, binding, deadline, || {
            Ok(native.inspect(binding)? == Registration::Running)
        })?;
        bootstrap::managed_source_identity(&self.state)?;
        Ok(())
    }
    fn cleanup(&mut self, old: &Binding) -> Result<()> {
        let helper = Path::new(&old.supervisor);
        if helper.try_exists()? {
            let mut permissions = std::fs::metadata(helper)?.permissions();
            permissions.set_readonly(false);
            std::fs::set_permissions(helper, permissions)?;
            std::fs::remove_file(helper)?;
        }
        let parent = helper.parent().context("old helper parent missing")?;
        if parent.try_exists()? {
            std::fs::remove_dir(parent)?;
        }
        Ok(())
    }
}

/// Nonfatal app-start work. An existing marker is resumed even while Disabled;
/// an ordinary disabled account never starts a new replacement.
pub fn refresh_current_app() -> Result<bool> {
    ensure!(
        okilum_sync::NetworkAccess::current().allowed(),
        "Sync is disabled"
    );
    let state = bootstrap::active_state_directory()?;
    if !state.join("sidecar.json").try_exists()? {
        return Ok(false);
    }
    refresh(state, preparation::Sources::installed()?)
}

/// Resume an unfinished plan, settle a cancelled one, or begin a new one.
pub(super) fn refresh(state: PathBuf, sources: preparation::Sources) -> Result<bool> {
    let store = WindowsStore::open_existing(state.to_str().context("Sync path is not Unicode")?)?;
    // Separate from prepare.lock: preparation takes that lock for each phase.
    let operation = crate::private::managed_lock(&state.join("restage.lock"), true)?;
    operation
        .try_lock()
        .context("helper refresh already in progress")?;
    let hash = crate::managed_launch::digest(&sources.helper)?;
    let target = state
        .join("helper")
        .join(&hash)
        .join("okilum-sync-supervisor.exe");
    let mut host = Installed {
        state,
        sources,
        store: store.clone(),
    };
    store.restage_helper(
        &mut host,
        target
            .to_str()
            .context("helper path is not Unicode")?
            .into(),
        hash,
        Instant::now() + Duration::from_secs(120),
    )
}
