//! Explicit Settings runtime selection. Merely opening Settings can read the
//! saved choice; it never prepares a daemon or registers a service.
use crate::{
    daemon::{discover, prepare, DaemonIdentity, Preparation},
    lifecycle::{Lifecycle, Systemd},
    private,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Selection {
    Managed(Preparation),
    Reuse(DaemonIdentity),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub selection: Selection,
    pub desired_enabled: bool,
    #[serde(default)]
    pub removed: bool,
    pub identity: Option<DaemonIdentity>,
}
pub struct Runtime {
    state: PathBuf,
    units: PathBuf,
}
impl Runtime {
    pub fn new(state: PathBuf, units: PathBuf) -> Self {
        Self { state, units }
    }
    pub fn snapshot(&self) -> Result<Option<Snapshot>> {
        let file = self.state.join("runtime.json");
        if !file.try_exists()? {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&private::read(&file)?)?))
    }
    /// Explicit Enable only. The complete current inventory is supplied by the
    /// desktop discovery task; unavailable candidates block new selection.
    pub fn enable(&self, selection: Selection, configs: &[PathBuf]) -> Result<Snapshot> {
        ensure!(
            self.state.is_absolute() && self.units.is_absolute(),
            "absolute runtime paths required"
        );
        let _lock = private::lock(&self.state)?;
        let mut saved = if let Some(saved) = self.snapshot()? {
            ensure!(
                saved.selection == selection,
                "runtime selection changed; remove the existing enrollment first"
            );
            saved
        } else {
            let inventory = discover(configs);
            inventory.folders()?;
            if let Selection::Reuse(identity) = &selection {
                ensure!(
                    inventory.select(&identity.config_file)?.identity == *identity,
                    "reuse identity changed"
                );
                identity.connect()?;
            }
            Snapshot {
                selection,
                desired_enabled: false,
                removed: false,
                identity: None,
            }
        };
        ensure!(
            !saved.removed,
            "runtime was removed; fresh enrollment required"
        );
        saved.desired_enabled = true;
        self.save(&saved)?;
        self.enable_locked(saved)
    }
    /// Retry only after the user has previously enabled this runtime. Disabled
    /// intent is reconciled as stop/unregister, never as an implicit re-enable.
    pub fn reconcile(&self) -> Result<Option<Snapshot>> {
        if self.snapshot()?.is_none() {
            return Ok(None);
        }
        let _lock = private::lock(&self.state)?;
        let saved = self.snapshot()?.context("runtime journal disappeared")?;
        if saved.desired_enabled && !saved.removed {
            Ok(Some(self.enable_locked(saved)?))
        } else {
            if matches!(saved.selection, Selection::Managed(_)) {
                self.lifecycle().disable()?;
            }
            Ok(Some(saved))
        }
    }
    /// Reuse never enters the lifecycle path, including when its daemon is
    /// offline. Disabling Okilum's integration does not stop external sync.
    pub fn disable(&self) -> Result<Option<Snapshot>> {
        if self.snapshot()?.is_none() {
            return Ok(None);
        }
        let _lock = private::lock(&self.state)?;
        let mut saved = self.snapshot()?.context("runtime journal disappeared")?;
        saved.desired_enabled = false;
        self.save(&saved)?;
        if matches!(saved.selection, Selection::Managed(_)) {
            self.lifecycle().disable()?;
        }
        Ok(Some(saved))
    }
    /// Durable removal is terminal for this runtime identity. Unlike Disable,
    /// later Enable cannot silently resurrect its old folder/grant configuration.
    pub fn request_remove(&self) -> Result<()> {
        if self.snapshot()?.is_none() {
            return Ok(());
        }
        let _lock = private::lock(&self.state)?;
        let mut saved = self.snapshot()?.context("runtime journal disappeared")?;
        saved.desired_enabled = false;
        saved.removed = true;
        self.save(&saved)
    }
    fn enable_locked(&self, mut saved: Snapshot) -> Result<Snapshot> {
        let identity = match &saved.selection {
            Selection::Managed(request) => {
                let (instance, identity) = prepare(&self.state.join("prepared"), request)?;
                if let Some(expected) = &saved.identity {
                    ensure!(*expected == identity, "prepared identity changed");
                }
                // Bind identity before service registration/start. REST readiness
                // is separately checked by the desktop before beginning pairing.
                saved.identity = Some(identity.clone());
                self.save(&saved)?;
                self.lifecycle().enable(instance)?;
                identity
            }
            Selection::Reuse(identity) => {
                identity.connect()?;
                identity.clone()
            }
        };
        saved.identity = Some(identity);
        self.save(&saved)?;
        Ok(saved)
    }
    fn lifecycle(&self) -> Lifecycle<Systemd> {
        Lifecycle::new(self.state.join("service"), self.units.clone(), Systemd)
    }
    fn save(&self, saved: &Snapshot) -> Result<()> {
        private::write(
            &self.state.join("runtime.json"),
            &serde_json::to_vec(saved)?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unopened_and_disabled_runtime_has_no_effects() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("runtime");
        let units = root.path().join("units");
        let runtime = Runtime::new(state.clone(), units.clone());
        assert!(runtime.snapshot()?.is_none());
        assert!(runtime.disable()?.is_none());
        assert!(runtime.reconcile()?.is_none());
        assert!(!state.exists() && !units.exists());
        Ok(())
    }
}
