#![cfg(target_os = "linux")]
use anyhow::{ensure, Context, Result};
use std::{fs, net::TcpListener, path::PathBuf, process::Command, time::Duration};
use tessera_sync_controller::daemon::{discover, prepare, Preparation};
use tessera_sync_controller::lifecycle::{Lifecycle, Systemd};

/// Creates a fresh identity and loopback-only config. Never reads a user daemon.
#[test]
#[ignore = "requires CT141 sandbox, user systemd and pinned Syncthing client"]
fn real_service_enable_disable_reenable_and_identity() -> Result<()> {
    let binary = PathBuf::from(
        std::env::var_os("TESSERA_SYNC_CLIENT").context("set isolated pinned client binary")?,
    );
    let root = tempfile::tempdir()?;
    let rest_guard = TcpListener::bind("127.0.0.1:0")?;
    let listen_guard = TcpListener::bind("127.0.0.1:0")?;
    let rest = rest_guard.local_addr()?;
    let listen = listen_guard.local_addr()?;
    let preparation = Preparation {
        executable: binary.clone(),
        rest_address: rest,
        listen_address: listen,
    };
    let prepared_state = root.path().join("prepared");
    let (instance, identity) = prepare(&prepared_state, &preparation)?;
    let home = instance.home.clone();
    let id = identity.device_id.clone();
    let (_, again) = prepare(&prepared_state, &preparation)?;
    assert_eq!(again.device_id, id);
    fs::remove_file(prepared_state.join("prepared.json"))?;
    let (_, resumed) = prepare(&prepared_state, &preparation)?;
    assert_eq!(resumed.device_id, id);
    let mut changed = preparation.clone();
    changed.listen_address = rest;
    assert!(prepare(&prepared_state, &changed).is_err());
    let units =
        PathBuf::from(std::env::var_os("HOME").context("user home")?).join(".config/systemd/user");
    let state = root.path().join("controller");
    let controller = Lifecycle::new(state.clone(), units.clone(), Systemd);
    assert!(!controller.desired_enabled()?);
    controller.disable()?;
    assert!(!state.exists());

    let certificate = fs::read(home.join("cert.pem"))?;
    drop(rest_guard);
    drop(listen_guard);
    // RAII cleanup runs even on assertion failure after enable.
    struct Cleanup<'a>(&'a Lifecycle<Systemd>);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.disable();
        }
    }
    let _cleanup = Cleanup(&controller);
    let name = controller.enable(instance.clone())?;
    let wait_ready = || -> Result<()> {
        for _ in 0..100 {
            if identity.connect().is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        anyhow::bail!("enabled fixture did not become ready")
    };
    wait_ready()?;
    let property = |property: &str| -> Result<String> {
        let output = Command::new("systemctl")
            .args(["--user", "show", &name, "--property", property, "--value"])
            .output()?;
        ensure!(output.status.success(), "systemd property probe failed");
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    assert_eq!(property("UnitFileState")?, "enabled");
    assert_eq!(property("ActiveState")?, "active");
    let api = identity.connect()?;
    assert_eq!(api.version()?["version"], "v2.1.6");
    let before = fs::read(&identity.config_file)?;
    let discovery = discover(&[identity.config_file.clone(), identity.config_file.clone()]);
    assert_eq!(discovery.candidates.len(), 1);
    assert!(discovery.unavailable.is_empty());
    assert_eq!(
        discovery.select(&identity.config_file)?.identity.device_id,
        id
    );
    assert_eq!(fs::read(&identity.config_file)?, before);
    let incomplete = discover(&[
        identity.config_file.clone(),
        root.path().join("missing.xml"),
    ]);
    assert!(incomplete.select(&identity.config_file).is_err());
    let config = api.config()?;
    assert_eq!(config["gui"]["address"], rest.to_string());
    assert_eq!(
        config["options"]["listenAddresses"],
        serde_json::json!([format!("tcp://{listen}")])
    );
    controller.disable()?;
    assert_eq!(property("UnitFileState")?, "");
    assert_eq!(property("ActiveState")?, "inactive");
    assert!(!units.join(&name).exists() && api.identity().is_err());
    assert_eq!(controller.enable(instance)?, name);
    wait_ready()?;
    assert_eq!(property("UnitFileState")?, "enabled");
    assert_eq!(property("ActiveState")?, "active");
    assert_eq!(fs::read(home.join("cert.pem"))?, certificate);
    controller.disable()?;
    let reopened = Lifecycle::new(state, units.clone(), Systemd);
    reopened.reconcile()?;
    assert_eq!(property("UnitFileState")?, "");
    assert_eq!(property("ActiveState")?, "inactive");
    assert!(!units.join(&name).exists());
    Ok(())
}
