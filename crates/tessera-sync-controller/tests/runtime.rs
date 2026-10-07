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
    let resumed = reopened.enable(choice, &[])?;
    assert_eq!(resumed.identity.as_ref(), Some(&identity));
    ready()?;
    assert_eq!(
        fs::read(identity.config_file.with_file_name("cert.pem"))?,
        certificate
    );
    reopened.disable()?;
    assert!(identity.connect().is_err());
    Ok(())
}
