//! Desktop operation boundary. All work is blocking and belongs on the UI's
//! background executor. Constructing/reading it does not launch any process.
use crate::{
    daemon::{configuration_paths, Preparation},
    enrollment::{Enrollment, Snapshot as PairingSnapshot},
    folder::{FolderController, LocalStatus},
    pairing::{Approval, Service, State},
    private,
    removal::{self, Outcome},
    runtime::{Runtime, Selection, Snapshot as RuntimeSnapshot},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    pub origin: String,
    pub name: String,
    pub destination: PathBuf,
    pub selection: Selection,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub setup: Option<Setup>,
    pub runtime: Option<RuntimeSnapshot>,
    pub pairing: Option<PairingSnapshot>,
    pub last_connected_at: Option<u64>,
}
#[derive(Debug)]
pub struct Progress {
    pub snapshot: Snapshot,
    pub approval: Option<Approval>,
    pub folder: Option<LocalStatus>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    setup: Setup,
    generation: uuid::Uuid,
    retired: bool,
}
#[derive(Clone)]
pub struct Desktop {
    pub state: PathBuf,
    pub home: PathBuf,
    pub state_home: PathBuf,
    pub config_home: PathBuf,
}
impl Desktop {
    pub fn read(&self) -> Result<Snapshot> {
        let Some(record) = self.record()?.filter(|r| !r.retired) else {
            return Ok(Snapshot {
                setup: None,
                runtime: None,
                pairing: None,
                last_connected_at: None,
            });
        };
        Ok(Snapshot {
            setup: Some(record.setup),
            runtime: self.runtime()?.snapshot()?,
            pairing: self.enrollment()?.snapshot()?,
            last_connected_at: if self.folder()?.exists()? {
                self.folder()?.last_connected_at()?
            } else {
                None
            },
        })
    }
    pub fn configs(&self) -> Result<Vec<PathBuf>> {
        configuration_paths(&self.home, &self.state_home, &self.config_home)
    }
    pub fn managed_selection(&self) -> Result<Selection> {
        // Reserving sockets happens only for an explicit Enable operation. The
        // prepared daemon validates binding; a collision fails without fallback.
        let rest = std::net::TcpListener::bind("127.0.0.1:0")?;
        let listen = std::net::TcpListener::bind("127.0.0.1:0")?;
        Ok(Selection::Managed(Preparation {
            executable: "/usr/bin/syncthing".into(),
            rest_address: rest.local_addr()?,
            listen_address: listen.local_addr()?,
        }))
    }
    pub fn enable(&self, setup: Setup, service: &Service) -> Result<Snapshot> {
        ensure!(
            setup.origin == service.origin(),
            "service differs from explicit selection"
        );
        ensure!(
            setup.destination.is_absolute() && setup.destination.is_dir(),
            "choose a folder first"
        );
        ensure!(
            !setup.name.is_empty()
                && setup.name.len() <= 100
                && setup.name.trim() == setup.name
                && !setup.name.chars().any(char::is_control),
            "invalid computer name"
        );
        if self.read()?.setup.is_none() && matches!(setup.selection, Selection::Managed(_)) {
            ensure!(
                setup.destination.read_dir()?.next().is_none(),
                "unknown nonempty destination"
            );
        }
        ensure!(
            self.state.is_absolute() && self.config_home.is_absolute(),
            "absolute application paths required"
        );
        std::fs::create_dir_all(self.state.parent().context("state parent missing")?)?;
        let _lock = private::lock(&self.state)?;
        if let Some(saved) = self.read()?.setup {
            ensure!(
                saved == setup,
                "existing connection must be removed before changing setup"
            );
        } else {
            let record = Record {
                setup: setup.clone(),
                generation: uuid::Uuid::new_v4(),
                retired: false,
            };
            private::directory(&self.state.join(record.generation.to_string()))?;
            private::write(&self.setup_file(), &serde_json::to_vec(&record)?)?;
        }
        self.runtime()?.enable(setup.selection, &self.configs()?)?;
        self.read()
    }
    /// Explicit refresh or scheduled continuation of an enabled setup. Disabled
    /// and removed runtimes never reconnect, exchange grants, or promote folders.
    pub fn refresh(&self, service: &Service) -> Result<Progress> {
        if self.read()?.setup.is_none() {
            return Ok(Progress {
                snapshot: self.read()?,
                approval: None,
                folder: None,
            });
        }
        let _lock = private::lock(&self.state)?;
        let snapshot = self.read()?;
        let setup = snapshot.setup.as_ref().context("setup missing")?;
        ensure!(
            setup.origin == service.origin(),
            "service differs from saved origin"
        );
        let Some(runtime) = snapshot
            .runtime
            .as_ref()
            .filter(|s| s.desired_enabled && !s.removed)
        else {
            return Ok(Progress {
                snapshot,
                approval: None,
                folder: None,
            });
        };
        let identity = runtime
            .identity
            .as_ref()
            .context("runtime identity unavailable")?;
        // Service startup is asynchronous. This bounded wait runs off the UI thread.
        for attempt in 0..50 {
            match identity.connect() {
                Ok(_) => break,
                Err(error) if attempt == 49 => return Err(error),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        let enrollment = self.enrollment()?;
        let approval = if snapshot
            .pairing
            .as_ref()
            .is_none_or(|s| s.registration.is_none())
        {
            Some(enrollment.begin(service, &identity.device_id, &setup.name)?)
        } else {
            None
        };
        let paired = enrollment.poll(service)?;
        if paired.intent == crate::enrollment::Intent::Remove {
            let outcome = removal::remove(&self.runtime()?, &enrollment, &self.folder()?, service);
            if outcome.complete() {
                self.retire()?;
            }
            return Ok(Progress {
                snapshot: self.read()?,
                approval: None,
                folder: None,
            });
        }
        let folder = if paired
            .registration
            .as_ref()
            .is_some_and(|r| r.state == State::HubReady)
            && paired.intent == crate::enrollment::Intent::Pair
        {
            let controller = self.folder()?;
            let status =
                controller.enroll(identity, &self.configs()?, &paired, &setup.destination)?;
            if status.preparing
                && !status.reused
                && status.folder["paused"] == false
                && status.status["state"] == "idle"
                && status.status["needTotalItems"] == 0
                && status.status["receiveOnlyTotalItems"] == 0
            {
                controller.promote(&|| enrollment.readiness(service))?;
            }
            Some(controller.status()?)
        } else {
            None
        };
        Ok(Progress {
            snapshot: self.read()?,
            approval,
            folder,
        })
    }
    pub fn pause(&self, paused: bool) -> Result<LocalStatus> {
        let _lock = private::lock(&self.state)?;
        ensure!(
            self.runtime()?
                .snapshot()?
                .is_some_and(|s| s.desired_enabled && !s.removed),
            "Sync is not enabled"
        );
        self.folder()?.pause(paused)
    }
    pub fn disable(&self) -> Result<Snapshot> {
        if self.read()?.setup.is_none() {
            return self.read();
        }
        let _lock = private::lock(&self.state)?;
        self.runtime()?.disable()?;
        self.read()
    }
    pub fn remove(&self, service: &Service) -> Result<Outcome> {
        let _lock = private::lock(&self.state)?;
        let setup = self.read()?.setup.context("setup missing")?;
        ensure!(
            setup.origin == service.origin(),
            "service differs from saved origin"
        );
        let outcome = removal::remove(
            &self.runtime()?,
            &self.enrollment()?,
            &self.folder()?,
            service,
        );
        if outcome.complete() {
            self.retire()?;
        }
        Ok(outcome)
    }
    fn retire(&self) -> Result<()> {
        let mut record = self.record()?.context("setup missing")?;
        record.retired = true;
        private::write(&self.setup_file(), &serde_json::to_vec(&record)?)
    }
    fn record(&self) -> Result<Option<Record>> {
        if !self.setup_file().try_exists()? {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&private::read(
            &self.setup_file(),
        )?)?))
    }
    fn generation(&self) -> Result<PathBuf> {
        let record = self.record()?.context("setup missing")?;
        Ok(self.state.join(record.generation.to_string()))
    }
    fn setup_file(&self) -> PathBuf {
        self.state.join("setup.json")
    }
    fn runtime(&self) -> Result<Runtime> {
        Ok(Runtime::new(
            self.generation()?.join("runtime"),
            self.config_home.join("systemd/user"),
        ))
    }
    fn enrollment(&self) -> Result<Enrollment> {
        Ok(Enrollment::new(self.generation()?.join("enrollment")))
    }
    fn folder(&self) -> Result<FolderController> {
        Ok(FolderController::new(self.generation()?.join("folder")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opening_settings_and_refresh_before_enable_are_inert() -> Result<()> {
        let root = tempfile::tempdir()?;
        let desktop = Desktop {
            state: root.path().join("sync"),
            home: root.path().join("home"),
            state_home: root.path().join("state"),
            config_home: root.path().join("config"),
        };
        let service = Service::new("https://not-contacted.invalid", None)?;
        assert!(desktop.read()?.setup.is_none());
        assert!(desktop.refresh(&service)?.snapshot.runtime.is_none());
        assert!(desktop.disable()?.runtime.is_none());
        assert!(!desktop.state.exists());
        assert!(!desktop.config_home.exists());
        assert!(!desktop.state_home.exists());
        Ok(())
    }
    #[test]
    fn unknown_nonempty_managed_destination_is_rejected_before_setup_or_service() -> Result<()> {
        let root = tempfile::tempdir()?;
        let vault = root.path().join("vault");
        std::fs::create_dir(&vault)?;
        std::fs::write(vault.join("local.md"), "retain me")?;
        let desktop = Desktop {
            state: root.path().join("sync"),
            home: root.path().join("home"),
            state_home: root.path().join("state"),
            config_home: root.path().join("config"),
        };
        let service = Service::new("https://not-contacted.invalid", None)?;
        let setup = Setup {
            origin: service.origin().into(),
            name: "Fixture".into(),
            destination: vault.clone(),
            selection: Selection::Managed(Preparation {
                executable: "/unavailable/syncthing".into(),
                rest_address: "127.0.0.1:49100".parse()?,
                listen_address: "127.0.0.1:49101".parse()?,
            }),
        };
        assert!(desktop
            .enable(setup, &service)
            .unwrap_err()
            .to_string()
            .contains("unknown nonempty destination"));
        assert!(!desktop.state.exists() && !desktop.config_home.exists());
        assert_eq!(
            std::fs::read_to_string(vault.join("local.md"))?,
            "retain me"
        );
        Ok(())
    }
}
