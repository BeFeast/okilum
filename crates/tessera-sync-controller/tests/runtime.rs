#![cfg(target_os = "linux")]
use anyhow::{ensure, Context, Result};
use std::{fs, net::TcpListener, path::PathBuf, time::Duration};
use tessera_sync_controller::{
    daemon::Preparation,
    runtime::{Runtime, Selection},
};

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
        executable: PathBuf::from(
            std::env::var_os("TESSERA_SYNC_CLIENT").context("pinned binary")?,
        ),
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
        tessera_sync_controller::enrollment::Enrollment::new(root.path().join("enrollment"));
    enrollment.begin(&service, &identity.device_id, "Isolated removal fixture")?;
    enrollment.poll(&service)?;
    let folder = tessera_sync_controller::folder::FolderController::new(root.path().join("folder"));
    let outcome =
        tessera_sync_controller::removal::remove(&reopened, &enrollment, &folder, &service);
    assert!(outcome.local_stopped && !outcome.revoked && !outcome.complete());
    assert_eq!(service.calls.get(), 1);
    assert!(identity.connect().is_err());
    assert!(reopened.snapshot()?.unwrap().removed);
    assert!(reopened.enable(choice, &[]).is_err());
    assert!(!reopened.reconcile()?.unwrap().desired_enabled);
    service.offline.set(false);
    let outcome =
        tessera_sync_controller::removal::remove(&reopened, &enrollment, &folder, &service);
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
    identity: tessera_sync_controller::daemon::DaemonIdentity,
    vault: uuid::Uuid,
    offline: std::cell::Cell<bool>,
    calls: std::cell::Cell<usize>,
}
impl tessera_sync_controller::enrollment::PairingService for OfflineRemoval {
    fn origin(&self) -> &str {
        "https://isolated-removal.invalid"
    }
    fn start(
        &self,
        s: &tessera_sync_controller::pairing::Session,
    ) -> Result<tessera_sync_controller::pairing::Approval> {
        use tessera_sync_controller::pairing::*;
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
        s: &tessera_sync_controller::pairing::Session,
    ) -> Result<Option<tessera_sync_controller::pairing::Registration>> {
        Ok(Some(self.status(s)?.registration))
    }
    fn status(
        &self,
        s: &tessera_sync_controller::pairing::Session,
    ) -> Result<tessera_sync_controller::pairing::Status> {
        use tessera_sync_controller::pairing::*;
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
        _: &tessera_sync_controller::pairing::Session,
    ) -> Result<tessera_sync_controller::pairing::State> {
        self.calls.set(self.calls.get() + 1);
        ensure!(
            self.identity.connect().is_err(),
            "remote revoke ran before owned service stop"
        );
        ensure!(!self.offline.get(), "simulated service outage");
        Ok(tessera_sync_controller::pairing::State::Revoked)
    }
}
