#![cfg(target_os = "linux")]
use anyhow::{ensure, Context, Result};
use okilum_sync_controller::{
    daemon::Preparation,
    runtime::{Runtime, Selection},
};
use std::{fs, net::TcpListener, path::PathBuf, time::Duration};

#[test]
#[ignore = "requires isolated CT141 and pinned binary with user systemd"]
fn settings_runtime_enable_disable_and_restart() -> Result<()> {
    let root = tempfile::tempdir()?;
    let state = root.path().join("runtime");
    let units =
        PathBuf::from(std::env::var_os("HOME").context("home")?).join(".config/systemd/user");
    let runtime = Runtime::new(state.clone(), units.clone());
    assert!(runtime.snapshot()?.is_none());
    assert!(!state.exists());
    let rest = TcpListener::bind("127.0.0.1:0")?;
    let listen = TcpListener::bind("127.0.0.1:0")?;
    let choice = Selection::Managed(Preparation {
        executable: PathBuf::from(std::env::var_os("OKILUM_SYNC_CLIENT").context("pinned binary")?),
        rest_address: rest.local_addr()?,
        listen_address: listen.local_addr()?,
    });
    drop(rest);
    drop(listen);
    struct Cleanup<'a>(&'a Runtime);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.disable();
        }
    }
    let _cleanup = Cleanup(&runtime);
    let active = runtime.enable(choice.clone(), &[])?;
    let identity = active.identity.context("identity")?;
    let certificate = fs::read(identity.config_file.with_file_name("cert.pem"))?;
    let ready = || -> Result<()> {
        for _ in 0..100 {
            if identity.connect().is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        anyhow::bail!("runtime REST not ready")
    };
    ready()?;
    let reopened = Runtime::new(state.clone(), units);
    assert!(reopened.snapshot()?.unwrap().desired_enabled);
    reopened.disable()?;
    ensure!(
        identity.connect().is_err(),
        "owned daemon still reachable after Disable"
    );
    assert!(!reopened.reconcile()?.unwrap().desired_enabled);
    assert!(identity.connect().is_err());
    let resumed = reopened.enable(choice.clone(), &[])?;
    assert_eq!(resumed.identity.as_ref(), Some(&identity));
    ready()?;
    assert_eq!(
        fs::read(identity.config_file.with_file_name("cert.pem"))?,
        certificate
    );
    let service = OfflineRemoval {
        identity: identity.clone(),
        vault: uuid::Uuid::new_v4(),
        offline: std::cell::Cell::new(true),
        calls: std::cell::Cell::new(0),
    };
    let enrollment =
        okilum_sync_controller::enrollment::Enrollment::new(root.path().join("enrollment"));
    enrollment.begin(&service, &identity.device_id, "Isolated removal fixture")?;
    enrollment.poll(&service)?;
    let folder = okilum_sync_controller::folder::FolderController::new(root.path().join("folder"));
    let outcome =
        okilum_sync_controller::removal::remove(&reopened, &enrollment, &folder, &service);
    assert!(outcome.local_stopped && !outcome.revoked && !outcome.complete());
    assert_eq!(service.calls.get(), 1);
    assert!(identity.connect().is_err());
    assert!(reopened.snapshot()?.unwrap().removed);
    assert!(reopened.enable(choice, &[]).is_err());
    assert!(!reopened.reconcile()?.unwrap().desired_enabled);
    service.offline.set(false);
    let outcome =
        okilum_sync_controller::removal::remove(&reopened, &enrollment, &folder, &service);
    assert!(outcome.complete(), "{outcome:?}");
    assert_eq!(service.calls.get(), 2);
    assert!(identity.connect().is_err());
    assert_eq!(
        fs::read(identity.config_file.with_file_name("cert.pem"))?,
        certificate
    );
    Ok(())
}

struct OfflineRemoval {
    identity: okilum_sync_controller::daemon::DaemonIdentity,
    vault: uuid::Uuid,
    offline: std::cell::Cell<bool>,
    calls: std::cell::Cell<usize>,
}
impl okilum_sync_controller::enrollment::PairingService for OfflineRemoval {
    fn origin(&self) -> &str {
        "https://isolated-removal.invalid"
    }
    fn start(
        &self,
        s: &okilum_sync_controller::pairing::Session,
    ) -> Result<okilum_sync_controller::pairing::Approval> {
        use okilum_sync_controller::pairing::*;
        Ok(Approval {
            approval_url: "https://isolated-removal.invalid/approval".into(),
            request: Request {
                id: s.id(),
                device_id: s.device_id().into(),
                name: s.name().into(),
                code: "12345678".into(),
                state: "pending".into(),
                expires: i64::MAX,
            },
        })
    }
    fn exchange(
        &self,
        s: &okilum_sync_controller::pairing::Session,
    ) -> Result<Option<okilum_sync_controller::pairing::Registration>> {
        Ok(Some(self.status(s)?.registration))
    }
    fn status(
        &self,
        s: &okilum_sync_controller::pairing::Session,
    ) -> Result<okilum_sync_controller::pairing::Status> {
        use okilum_sync_controller::pairing::*;
        Ok(Status {
            registration: Registration {
                id: s.id(),
                vault: self.vault,
                device_id: s.device_id().into(),
                name: s.name().into(),
                state: State::HubReady,
                last_error: None,
            },
            descriptor: None,
        })
    }
    fn remove(
        &self,
        _: &okilum_sync_controller::pairing::Session,
    ) -> Result<okilum_sync_controller::pairing::State> {
        self.calls.set(self.calls.get() + 1);
        ensure!(
            self.identity.connect().is_err(),
            "remote revoke ran before owned service stop"
        );
        ensure!(!self.offline.get(), "simulated service outage");
        Ok(okilum_sync_controller::pairing::State::Revoked)
    }
}

#[test]
#[ignore = "requires isolated CT141 and pinned binary with user systemd"]
fn desktop_partial_enable_removal_and_fresh_setup_keep_separate_identities() -> Result<()> {
    use okilum_sync_controller::{
        desktop::{Desktop, Setup},
        pairing::Service,
    };
    let root = tempfile::tempdir()?;
    let home = PathBuf::from(std::env::var_os("HOME").context("home")?);
    let desktop = Desktop {
        state: root.path().join("sync"),
        config_home: home.join(".config"),
        home,
        state_home: root.path().join("state"),
    };
    let destination = root.path().join("vault");
    fs::create_dir(&destination)?;
    let rest = TcpListener::bind("127.0.0.1:0")?;
    let listen = TcpListener::bind("127.0.0.1:0")?;
    let choice = Selection::Managed(Preparation {
        executable: PathBuf::from(std::env::var_os("OKILUM_SYNC_CLIENT").context("pinned binary")?),
        rest_address: rest.local_addr()?,
        listen_address: listen.local_addr()?,
    });
    drop(rest);
    drop(listen);
    let service = Service::new("https://127.0.0.1:9", None)?;
    let setup = Setup {
        origin: service.origin().into(),
        name: "Desktop isolated fixture".into(),
        destination: destination.clone(),
        selection: choice,
    };
    struct Cleanup<'a>(&'a Desktop);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.disable();
        }
    }
    let _cleanup = Cleanup(&desktop);
    assert!(desktop.read()?.setup.is_none());
    assert!(!desktop.state.exists());
    desktop.enable(setup.clone(), &service)?;
    assert!(
        desktop.refresh(&service).is_err(),
        "unavailable HTTPS service cannot approve pairing"
    );
    let runtime = desktop
        .read()?
        .runtime
        .context("durable runtime after partial enable")?;
    assert!(runtime.desired_enabled);
    let identity = runtime.identity.context("identity")?;
    identity.connect()?; // Positive control: this action really started the fixture.
    let record = fs::read(desktop.state.join("setup.json"))?;
    let removed = desktop.remove(&service)?;
    assert!(removed.complete(), "no grant was exchanged: {removed:?}");
    assert!(identity.connect().is_err());
    assert!(desktop.read()?.setup.is_none());
    assert!(
        identity.config_file.exists(),
        "retired private journal retained"
    );
    assert!(destination.is_dir());
    desktop.enable(setup, &service)?;
    let fresh = desktop
        .read()?
        .runtime
        .context("fresh runtime")?
        .identity
        .context("fresh identity")?;
    assert_ne!(fresh.device_id, identity.device_id);
    assert_ne!(fs::read(desktop.state.join("setup.json"))?, record);
    desktop.disable()?;
    assert!(!desktop.read()?.runtime.unwrap().desired_enabled);
    assert!(fresh.connect().is_err());
    Ok(())
}
