#![cfg(unix)]
use anyhow::{Context, Result};
use okilum_inboxd::sync_hub::{Action, Config, Hub, Request};
use okilum_sync::Syncthing;
use serde_json::json;
use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use uuid::Uuid;
struct Daemon {
    child: Child,
    binary: PathBuf,
    home: PathBuf,
}
impl Daemon {
    fn start(binary: &Path, home: &Path) -> Result<Self> {
        let log = fs::File::create(home.join("daemon.log"))?;
        let child = Command::new(binary)
            .env_clear()
            .env("HOME", home)
            .env("STMONITORED", "1")
            .env("GOMAXPROCS", "1")
            .args([
                "serve",
                "--no-browser",
                "--no-restart",
                "--no-upgrade",
                "--home",
            ])
            .arg(home)
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        Ok(Self {
            child,
            binary: binary.into(),
            home: home.into(),
        })
    }
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn restart(&mut self) -> Result<()> {
        self.stop();
        let new = Self::start(&self.binary, &self.home)?;
        *self = new;
        Ok(())
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}
fn generated(binary: &Path, home: &Path, rest: u16, listen: u16) -> Result<(String, String)> {
    fs::create_dir(home)?;
    anyhow::ensure!(
        Command::new(binary)
            .env_clear()
            .env("HOME", home)
            .args(["generate", "--home"])
            .arg(home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "generate"
    );
    let result=Command::new("python3").arg("-c").arg(r#"
import sys,json,xml.etree.ElementTree as E
p,rest,listen=sys.argv[1:];t=E.parse(p);r=t.getroot()
for f in list(r.findall('folder')):r.remove(f)
g=r.find('gui');g.find('address').text='127.0.0.1:'+rest;o=r.find('options')
for k,v in {'globalAnnounceEnabled':'false','localAnnounceEnabled':'false','natEnabled':'false','relaysEnabled':'false','startBrowser':'false','autoUpgradeIntervalH':'0','urAccepted':'-1','crashReportingEnabled':'false'}.items():
 e=o.find(k)
 if e is None:e=E.SubElement(o,k)
 e.text=v
for e in list(o.findall('listenAddress')):o.remove(e)
E.SubElement(o,'listenAddress').text='tcp://127.0.0.1:'+listen
t.write(p)
print(json.dumps([r.find('device').attrib['id'],g.find('apikey').text]))
"#).arg(home.join("config.xml")).arg(rest.to_string()).arg(listen.to_string()).output()?;
    anyhow::ensure!(result.status.success(), "offline setup");
    Ok(serde_json::from_slice(&result.stdout)?)
}
fn ready(api: &Syncthing) {
    for _ in 0..100 {
        if api.identity().is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("fixture did not start")
}
#[test]
#[ignore = "requires unchanged pinned Syncthing 1.29.5; run on isolated development host"]
fn real_hub_scope_restart_offline_revocation_and_preservation() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let binary =
        PathBuf::from(std::env::var_os("OKILUM_SYNC_HUB").context("set pinned hub binary")?);
    let temp = tempfile::tempdir()?;
    let rest_guard = TcpListener::bind("127.0.0.1:0")?;
    let listen_guard = TcpListener::bind("127.0.0.1:0")?;
    let rest = rest_guard.local_addr()?;
    let listen = listen_guard.local_addr()?;
    let home = temp.path().join("hub");
    let (hub_id, key) = generated(&binary, &home, rest.port(), listen.port())?;
    let (device, _) = generated(&binary, &temp.path().join("new-client"), 1, 2)?;
    let (external, _) = generated(&binary, &temp.path().join("external-client"), 3, 4)?;
    drop(rest_guard);
    drop(listen_guard);
    let mut daemon = Daemon::start(&binary, &home)?;
    let api = Syncthing::connect(rest, &key)?;
    ready(&api);
    let vault = temp.path().join("fixture");
    let other = temp.path().join("unrelated");
    fs::create_dir(&vault)?;
    fs::create_dir(&other)?;
    api.add_device(&json!({"deviceID":external,"name":"External sentinel","addresses":["tcp://127.0.0.1:23456"],"introducer":true,"autoAcceptFolders":false}))?;
    api.add_paused_folder(&json!({"id":"fixture","path":vault,"paused":true,"devices":[{"deviceID":hub_id},{"deviceID":external}]}))?;
    api.add_paused_folder(&json!({"id":"unrelated","path":other,"paused":true,"devices":[{"deviceID":hub_id},{"deviceID":external}]}))?;
    let before = api.config()?;
    let original_folder = api.folder("fixture")?;
    let unrelated = api.folder("unrelated")?;
    let external_before = api.device(&external)?;
    let state = temp.path().join("adapter");
    fs::create_dir(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    let keyfile = temp.path().join("rest-key");
    fs::write(&keyfile, &key)?;
    fs::set_permissions(&keyfile, fs::Permissions::from_mode(0o600))?;
    let config = Config {
        owner_id: Uuid::new_v4(),
        vault_id: Uuid::new_v4(),
        folder_id: "fixture".into(),
        folder_path: vault,
        hub_device_id: hub_id,
        rest_address: rest,
        rest_key_file: keyfile,
        data_dir: state.clone(),
        socket: state.join("adapter.sock"),
    };
    let mut adapter = Hub::open(config.clone())?;
    let request = Request {
        owner_id: config.owner_id,
        vault_id: config.vault_id,
        registration_id: Uuid::new_v4(),
        device_id: device.clone(),
        action: Action::Add,
    };
    let mut wrong = request.clone();
    wrong.owner_id = Uuid::new_v4();
    assert!(adapter.apply(&wrong).is_err());
    wrong = request.clone();
    wrong.vault_id = Uuid::new_v4();
    assert!(adapter.apply(&wrong).is_err());
    assert_eq!(api.folder("fixture")?, original_folder);
    let mut existing = request.clone();
    existing.registration_id = Uuid::new_v4();
    existing.device_id = external.clone();
    assert!(adapter.apply(&existing).is_err());
    existing.action = Action::Remove;
    assert_eq!(adapter.apply(&existing)?.state, "revoked");
    assert_eq!(
        api.folder("fixture")?,
        original_folder,
        "Remove before owned Add must preserve external share"
    );
    // Simulate interruption after durable reservation and device creation, before share creation.
    drop(adapter);
    let db = rusqlite::Connection::open(state.join("hub.db"))?;
    db.execute(
        "INSERT INTO registrations VALUES(?1,?2,0,1)",
        rusqlite::params![request.registration_id.to_string(), device],
    )?;
    drop(db);
    api.add_device(&json!({"deviceID":device,"name":"Okilum desktop","addresses":["dynamic"],"introducer":false,"skipIntroductionRemovals":true,"autoAcceptFolders":false}))?;
    let mut adapter = Hub::open(config.clone())?;
    assert_eq!(adapter.apply(&request)?.state, "hub_ready");
    assert_eq!(adapter.apply(&request)?.state, "hub_ready");
    let mut observation_request = request.clone();
    observation_request.action = Action::Readiness;
    // An unready/paused runner may return unavailable, never fake an empty
    // successful observation. Unpause the synthetic folder for live sampling.
    api.patch_folder("fixture", &json!({"paused":false}))?;
    fs::write(
        config.folder_path.join("readiness.md"),
        "synthetic index generation",
    )?;
    api.scan("fixture")?;
    let first = adapter
        .apply(&observation_request)?
        .observation
        .context("observation")?;
    assert_eq!(
        first["registration_id"],
        request.registration_id.to_string()
    );
    assert_eq!(first["device_id"], device);
    assert_eq!(first["connected"], false);
    assert!(first["hub"]["sequence"].as_u64().is_some_and(|s| s > 0));
    let mut substituted = observation_request.clone();
    substituted.owner_id = Uuid::new_v4();
    assert!(adapter.apply(&substituted).is_err());
    substituted = observation_request.clone();
    substituted.registration_id = Uuid::new_v4();
    assert!(adapter.apply(&substituted).is_err());
    // Rebuilding a synthetic index can reuse numeric sequence values. Startup
    // identity must invalidate the old observation independently of sequence.
    daemon.stop();
    assert!(adapter.apply(&observation_request).is_err());
    let index = home.join("index-v0.14.0.db");
    assert!(index.is_dir());
    fs::remove_dir_all(index)?;
    daemon.restart()?;
    ready(&api);
    api.scan("fixture")?;
    let rebuilt = adapter
        .apply(&observation_request)?
        .observation
        .context("rebuilt observation")?;
    assert_ne!(first["hub_started_at"], rebuilt["hub_started_at"]);
    assert_eq!(first["adapter_generation"], rebuilt["adapter_generation"]);
    drop(adapter);
    let mut adapter = Hub::open(config.clone())?;
    let reopened = adapter
        .apply(&observation_request)?
        .observation
        .context("reopened observation")?;
    assert_ne!(
        rebuilt["adapter_generation"],
        reopened["adapter_generation"]
    );
    api.patch_folder("fixture", &json!({"paused":true}))?;

    assert!(
        api.folder("fixture")?["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["deviceID"] == device),
        "allowed Add positive control"
    );
    daemon.stop();
    let mut removal = request.clone();
    removal.action = Action::Remove;
    assert!(adapter.apply(&removal).is_err());
    drop(adapter);
    daemon.restart()?;
    ready(&api);
    let mut adapter = Hub::open(config.clone())?;
    assert_eq!(adapter.apply(&removal)?.state, "revoked");
    assert!(
        adapter.apply(&request).is_err(),
        "late Add cannot undo tombstone"
    );
    // Emulate a trusted introducer restoring the folder reference.
    let mut refs = api.folder("fixture")?["devices"]
        .as_array()
        .unwrap()
        .clone();
    refs.push(json!({"deviceID":device}));
    api.patch_folder("fixture", &json!({"devices":refs}))?;
    assert!(api.folder("fixture")?["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["deviceID"] == device));
    adapter.reconcile_revoked()?;
    assert_eq!(api.folder("fixture")?, original_folder);
    let mut fresh = request.clone();
    fresh.registration_id = Uuid::new_v4();
    assert_eq!(adapter.apply(&fresh)?.state, "hub_ready");
    assert_eq!(adapter.apply(&removal)?.state, "revoked");
    adapter.reconcile_revoked()?;
    let mut old_status = removal.clone();
    old_status.action = Action::Status;
    assert_eq!(adapter.apply(&old_status)?.state, "revoked");
    assert!(adapter.apply(&request).is_err());
    assert!(
        api.folder("fixture")?["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["deviceID"] == device),
        "old tombstone must not remove a freshly approved registration"
    );
    drop(adapter);
    let mut adapter = Hub::open(config.clone())?;
    adapter.reconcile_revoked()?;
    assert_eq!(adapter.apply(&fresh)?.state, "hub_ready");
    fresh.action = Action::Remove;
    assert_eq!(adapter.apply(&fresh)?.state, "revoked");
    assert_eq!(api.folder("fixture")?, original_folder);
    assert_eq!(api.folder("unrelated")?, unrelated);
    assert_eq!(api.device(&external)?, external_before);
    let after = api.config()?;
    assert_eq!(before["options"], after["options"]);
    assert_eq!(before["gui"], after["gui"]);
    let mut changed = config.clone();
    changed.hub_device_id = device;
    assert!(
        Hub::open(changed).is_err(),
        "journal cannot change scope on restart"
    );
    drop(adapter);
    let config_path = temp.path().join("adapter.json");
    fs::write(&config_path, serde_json::to_vec(&config)?)?;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;
    let mut process = Command::new(env!("CARGO_BIN_EXE_okilum-sync-hub"))
        .arg("--config")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let transport_result = (|| -> Result<()> {
        for _ in 0..100 {
            if config.socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut status = removal.clone();
        status.action = Action::Status;
        anyhow::ensure!(
            okilum_inboxd::sync_hub::call(&config.socket, &status)?.state == "revoked",
            "UDS status"
        );
        status.owner_id = Uuid::new_v4();
        anyhow::ensure!(
            okilum_inboxd::sync_hub::call(&config.socket, &status).is_err(),
            "UDS scope guard"
        );
        Ok(())
    })();
    let _ = process.kill();
    let _ = process.wait();
    transport_result?;
    daemon.stop();
    assert!(
        api.identity().is_err(),
        "owned process stopped after positive readiness"
    );
    Ok(())
}
