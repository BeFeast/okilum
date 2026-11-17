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
    fn ready(&mut self, native: &mut Native, binding: &Binding, deadline: Instant) -> Result<()> {
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
    refresh_with(state, sources, |installed| installed)
}

fn refresh_with<H: Host>(
    state: PathBuf,
    sources: preparation::Sources,
    host: impl FnOnce(Installed) -> H,
) -> Result<bool> {
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
    let target = target
        .to_str()
        .context("helper path is not Unicode")?
        .into();
    let mut host = host(Installed {
        state,
        sources,
        store: store.clone(),
    });
    store.restage_helper(
        &mut host,
        target,
        hash,
        Instant::now() + Duration::from_secs(120),
    )
}

/// Native control: the helper the plan armed its Stop for is no longer the
/// live generation (as after a scheduler restart between reading its scope and
/// committing the Stop). The real guard, Task Scheduler and authenticated Stop
/// must re-arm for the live generation; nothing is killed.
#[cfg(test)]
pub(super) fn refresh_after_generation_change() -> Result<bool> {
    use crate::sidecar::{authority::StopToken, supervisor::ipc::Scope};
    struct Stale<P>(P, bool);
    impl<P: Platform> Platform for Stale<P> {
        fn inspect(&mut self, b: &Binding) -> Result<Registration> {
            self.0.inspect(b)
        }
        fn verify_payload(&mut self, b: &Binding) -> Result<()> {
            self.0.verify_payload(b)
        }
        fn register(&mut self, b: &Binding) -> Result<()> {
            self.0.register(b)
        }
        fn start(&mut self, b: &Binding) -> Result<()> {
            self.0.start(b)
        }
        fn stop(&mut self, b: &Binding) -> Result<()> {
            self.0.stop(b)
        }
        fn supervisor_scope(&mut self, b: &Binding) -> Result<Option<Scope>> {
            let mut scope = self.0.supervisor_scope(b)?;
            if std::mem::take(&mut self.1) {
                let scope = scope.as_mut().context("helper not running for control")?;
                scope.generation = uuid::Uuid::new_v4();
            }
            Ok(scope)
        }
        fn stop_supervisor(&mut self, b: &Binding, token: &StopToken) -> Result<()> {
            self.0.stop_supervisor(b, token)
        }
        fn unregister(&mut self, b: &Binding) -> Result<()> {
            self.0.unregister(b)
        }
    }
    struct Once(Installed, bool);
    impl Host for Once {
        type Native = Stale<Native>;
        fn platform(&mut self, b: &Binding) -> Result<Self::Native> {
            Ok(Stale(self.0.platform(b)?, std::mem::take(&mut self.1)))
        }
        fn select_helper(&mut self, b: &Binding, hash: &str) -> Result<()> {
            self.0.select_helper(b, hash)
        }
        fn ready(
            &mut self,
            native: &mut Self::Native,
            b: &Binding,
            deadline: Instant,
        ) -> Result<()> {
            self.0.ready(&mut native.0, b, deadline)
        }
        fn cleanup(&mut self, old: &Binding) -> Result<()> {
            self.0.cleanup(old)
        }
    }
    refresh_with(
        bootstrap::active_state_directory()?,
        preparation::Sources::installed()?,
        |installed| Once(installed, true),
    )
}
