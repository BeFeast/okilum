#![cfg(target_os = "linux")]
use anyhow::{ensure, Context, Result};
use std::{
    fs, net::TcpListener, os::unix::fs::PermissionsExt, path::PathBuf, process::Command,
    time::Duration,
};
use tessera_sync::Syncthing;
use tessera_sync_controller::lifecycle::{Lifecycle, ManagedInstance, Systemd};

/// Creates a fresh identity and loopback-only config. Never reads a user daemon.
#[test]
#[ignore = "requires CT141 sandbox, user systemd and pinned Syncthing client"]
fn real_service_enable_disable_reenable_and_identity() -> Result<()> {
    let binary = PathBuf::from(
        std::env::var_os("TESSERA_SYNC_CLIENT").context("set isolated pinned client binary")?,
    );
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    fs::create_dir(&home)?;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
    let output = Command::new(&binary)
        .args(["generate", "--home"])
        .arg(&home)
        .output()?;
    ensure!(output.status.success(), "fixture generation failed");
    let rest_guard = TcpListener::bind("127.0.0.1:0")?;
    let listen_guard = TcpListener::bind("127.0.0.1:0")?;
    let rest = rest_guard.local_addr()?;
    let listen = listen_guard.local_addr()?;
    let output = Command::new("python3").arg("-c").arg(r#"
import json,sys,xml.etree.ElementTree as E
from pathlib import Path
home,rest,listen=sys.argv[1:];home=Path(home)
t=E.parse(home/'config.xml');r=t.getroot()
for f in list(r.findall('folder')):r.remove(f)
r.find('gui/address').text=rest;o=r.find('options')
for k,v in {'globalAnnounceEnabled':'false','localAnnounceEnabled':'false','natEnabled':'false','relaysEnabled':'false','startBrowser':'false','autoUpgradeIntervalH':'0','urAccepted':'-1','crashReportingEnabled':'false'}.items():
 e=o.find(k)
 if e is None:e=E.SubElement(o,k)
 e.text=v
for e in list(o.findall('listenAddress')):o.remove(e)
E.SubElement(o,'listenAddress').text='tcp://'+listen
t.write(home/'config.xml')
for n in ['config.xml','cert.pem','key.pem']:(home/n).chmod(0o600)
print(json.dumps([r.find('device').attrib['id'],r.find('gui/apikey').text]))
"#).arg(&home).arg(rest.to_string()).arg(listen.to_string()).output()?;
    ensure!(
        output.status.success(),
        "offline fixture configuration failed"
    );
    let (id, key): (String, String) = serde_json::from_slice(&output.stdout)?;
    let api = Syncthing::connect(rest, &key)?;
    let units =
        PathBuf::from(std::env::var_os("HOME").context("user home")?).join(".config/systemd/user");
    let state = root.path().join("controller");
    let controller = Lifecycle::new(state.clone(), units.clone(), Systemd);
    assert!(!controller.desired_enabled()?);
    controller.disable()?;
    assert!(!state.exists());
    assert!(api.identity().is_err());
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
    let instance = ManagedInstance {
        home: home.clone(),
        executable: binary,
    };
    let name = controller.enable(instance.clone())?;
    let wait_ready = || -> Result<()> {
        for _ in 0..100 {
            if api.verify_identity(&id).is_ok() {
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
    assert_eq!(api.version()?["version"], "v2.1.6");
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
