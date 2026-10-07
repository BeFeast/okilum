#![cfg(target_os = "linux")]
//! Isolated controller fixture; no user daemon, config or production endpoint.
use anyhow::{ensure, Context, Result};
use serde_json::json;
use std::{
    fs,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tessera_sync::{Syncthing, FIXTURE_IGNORES};
use tessera_sync_controller::{
    daemon::discover,
    enrollment::{Intent, Snapshot},
    folder::FolderController,
    pairing::{Descriptor, Registration, State},
};
use uuid::Uuid;

struct Peer {
    child: Option<Child>,
    binary: PathBuf,
    home: PathBuf,
    vault: PathBuf,
    api: Syncthing,
    rest: SocketAddr,
    listen: SocketAddr,
    id: String,
}
impl Peer {
    fn new(root: &Path, name: &str, binary: PathBuf) -> Result<Self> {
        let home = root.join(name);
        fs::create_dir(&home)?;
        let vault = home.join("vault");
        fs::create_dir(&vault)?;
        let rest_guard = TcpListener::bind("127.0.0.1:0")?;
        let listen_guard = TcpListener::bind("127.0.0.1:0")?;
        let rest = rest_guard.local_addr()?;
        let listen = listen_guard.local_addr()?;
        ensure!(
            Command::new(&binary)
                .env_clear()
                .args(["generate", "--home"])
                .arg(&home)
                .env("HOME", &home)
                .stdout(Stdio::null())
                .status()?
                .success(),
            "generate failed"
        );
        // Offline fixture setup only. Runtime mutations below all use the Rust boundary.
        let output = Command::new("python3").arg("-c").arg(r#"
import sys,xml.etree.ElementTree as E
p,rest,listen=sys.argv[1:]
t=E.parse(p);r=t.getroot()
for f in list(r.findall('folder')):r.remove(f)
g=r.find('gui');g.find('address').text=rest
opts=r.find('options')
for k,v in {'globalAnnounceEnabled':'false','localAnnounceEnabled':'false','relaysEnabled':'false','natEnabled':'false','urAccepted':'-1','autoUpgradeIntervalH':'0','startBrowser':'false'}.items():
 e=opts.find(k)
 if e is None:e=E.SubElement(opts,k)
 e.text=v
for e in list(opts.findall('listenAddress')):opts.remove(e)
E.SubElement(opts,'listenAddress').text='tcp://'+listen
t.write(p)
print(g.find('apikey').text)
"#).arg(home.join("config.xml")).arg(rest.to_string()).arg(listen.to_string()).output()?;
        ensure!(output.status.success(), "fixture config failed");
        let key = String::from_utf8(output.stdout)?;
        let api = Syncthing::connect(rest, key.trim())?;
        drop(rest_guard);
        drop(listen_guard);
        let mut peer = Self {
            child: None,
            binary,
            home,
            vault,
            api,
            rest,
            listen,
            id: String::new(),
        };
        peer.start()?;
        peer.id = peer.api.identity()?["myID"]
            .as_str()
            .context("identity absent")?
            .into();
        Ok(peer)
    }
    fn start(&mut self) -> Result<()> {
        let log = fs::File::create(self.home.join("fixture.log"))?;
        self.child = Some(
            Command::new(&self.binary)
                .env_clear()
                .args([
                    "serve",
                    "--no-browser",
                    "--no-restart",
                    "--no-upgrade",
                    "--home",
                ])
                .arg(&self.home)
                .env("HOME", &self.home)
                .env("GOMAXPROCS", "2")
                .env("STMONITORED", "1")
                .stdout(log.try_clone()?)
                .stderr(log)
                .spawn()?,
        );
        wait("REST ready", || Ok(self.api.identity().is_ok()))
    }
    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            child.wait()?;
        }
        Ok(())
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn wait(label: &str, mut f: impl FnMut() -> Result<bool>) -> Result<()> {
    let start = Instant::now();
    loop {
        if f()? {
            return Ok(());
        }
        ensure!(
            start.elapsed() < Duration::from_secs(60),
            "timed out: {label}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}
fn write(root: &Path, path: &str, content: &str) -> Result<()> {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, content)?;
    Ok(())
}
fn device(peer: &Peer, introducer: bool) -> serde_json::Value {
    json!({"deviceID":peer.id,"name":"fixture-peer","addresses":[format!("tcp://{}",peer.listen)],"introducer":introducer,"skipIntroductionRemovals":true,"autoAcceptFolders":false})
}
fn folder(peer: &Peer, other: &Peer, id: &str, kind: &str) -> serde_json::Value {
    json!({"id":id,"label":"compatibility","path":peer.vault,"type":kind,"paused":true,"rescanIntervalS":0,"fsWatcherEnabled":false,"devices":[{"deviceID":peer.id},{"deviceID":other.id}]})
}

#[test]
#[ignore = "requires CT141 and both pinned isolated binaries"]
fn owned_folder_and_external_replica_keep_their_boundaries() -> Result<()> {
    let root = tempfile::tempdir()?;
    let hub = Peer::new(
        root.path(),
        "hub",
        std::env::var_os("TESSERA_SYNC_HUB")
            .context("hub binary")?
            .into(),
    )?;
    let mut client = Peer::new(
        root.path(),
        "client",
        std::env::var_os("TESSERA_SYNC_CLIENT")
            .context("client binary")?
            .into(),
    )?;
    assert_eq!(hub.api.version()?["version"], "v1.29.5");
    assert!(Syncthing::connect(client.rest, "wrong-key")?
        .identity()
        .is_err());
    let config_path = client.home.join("config.xml");
    let configs = vec![config_path.clone()];
    let inventory = discover(&configs);
    let identity = inventory.select(&config_path)?.identity.clone();
    hub.api.add_device(&device(&client, false))?;
    hub.api
        .add_paused_folder(&folder(&hub, &client, "controller-fixture", "sendreceive"))?;
    let policy = FIXTURE_IGNORES
        .iter()
        .map(|s| (*s).to_owned())
        .collect::<Vec<_>>();
    hub.api.set_ignores("controller-fixture", FIXTURE_IGNORES)?;
    write(&hub.vault, "canary.md", "initial content")?;
    write(&hub.vault, ".tessera-index/excluded", "must remain at hub")?;
    hub.api
        .patch_folder("controller-fixture", &json!({"paused":false}))?;
    hub.api.scan("controller-fixture")?;
    let unrelated_path = root.path().join("unrelated");
    fs::create_dir(&unrelated_path)?;
    client.api.add_paused_folder(&json!({"id":"unrelated","path":unrelated_path,"paused":true,"type":"sendreceive","devices":[{"deviceID":client.id}]}))?;
    let unrelated = client.api.folder("unrelated")?;
    let id = Uuid::new_v4();
    // The service protocol is tested separately; this fixture supplies the exact
    // HubReady binding after synthetic hub setup, never a production grant.
    let approval = Snapshot {
        request_id: id,
        intent: Intent::Pair,
        registration: Some(Registration {
            id,
            vault: Uuid::new_v4(),
            device_id: client.id.clone(),
            name: "fixture".into(),
            state: State::HubReady,
            last_error: None,
        }),
        descriptor: Some(Descriptor {
            folder_id: "controller-fixture".into(),
            hub_device_id: hub.id.clone(),
            hub_address: format!("tcp://{}", hub.listen),
            ignores: policy,
        }),
        removal: None,
    };
    let barrier = std::sync::Barrier::new(2);
    let attempts = [
        root.path().join("controller"),
        root.path().join("competing"),
    ];
    let outcomes = thread::scope(|scope| {
        let handles = attempts
            .iter()
            .map(|state| {
                let barrier = &barrier;
                let identity = &identity;
                let configs = &configs;
                let approval = &approval;
                let vault = &client.vault;
                scope.spawn(move || {
                    barrier.wait();
                    FolderController::new(state.clone()).enroll(identity, configs, approval, vault)
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut outcomes = outcomes
        .into_iter()
        .zip(&attempts)
        .map(|(outcome, state)| {
            match outcome {
                Ok(status) => Ok(status),
                Err(error) => {
                    ensure!(
                        error.to_string().contains("would block"),
                        "unexpected failure: {error}"
                    );
                    // The nonblocking process lock reports busy; retry after the
                    // winner commits must see a replica, never another owner.
                    FolderController::new(state.clone()).enroll(
                        &identity,
                        &configs,
                        &approval,
                        &client.vault,
                    )
                }
            }
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(outcomes.iter().filter(|s| !s.reused).count(), 1);
    let owner = outcomes.iter().position(|s| !s.reused).unwrap();
    let controller_state = attempts[owner].clone();
    let s = outcomes.swap_remove(owner);
    assert!(!s.reused && s.folder["type"] == "receiveonly");
    wait("owned folder receives canary", || {
        Ok(fs::read_to_string(client.vault.join("canary.md"))
            .ok()
            .as_deref()
            == Some("initial content"))
    })?;
    assert!(!client.vault.join(".tessera-index/excluded").exists());
    assert_eq!(client.api.device(&hub.id)?["introducer"], true);
    let reopened = FolderController::new(controller_state.clone());
    assert!(
        !reopened
            .enroll(&identity, &configs, &approval, &client.vault)?
            .reused
    );
    assert_eq!(client.api.folder("unrelated")?, unrelated);
    // A foreign operation cannot be adopted on replay, even if its path,
    // folder ID and device configuration otherwise match.
    let owned_label = client.api.folder("controller-fixture")?["label"].clone();
    client
        .api
        .patch_folder("controller-fixture", &json!({"label":"another owner"}))?;
    assert!(reopened
        .enroll(&identity, &configs, &approval, &client.vault)
        .is_err());
    client
        .api
        .patch_folder("controller-fixture", &json!({"label":owned_label}))?;
    // Explicit reuse owns neither the existing folder nor the existing hub.
    let reuse = FolderController::new(root.path().join("reuse"));
    assert!(
        reuse
            .enroll(&identity, &configs, &approval, &client.vault)?
            .reused
    );
    reuse.pause(true).context("pause reused replica")?;
    assert_eq!(client.api.folder("controller-fixture")?["paused"], true);
    let result = reuse.remove().context("remove reused replica")?;
    assert!(result.removed && result.external_sync_retained);
    assert_eq!(client.api.folder("controller-fixture")?["paused"], false);
    assert!(client.api.device(&hub.id).is_ok());
    write(&hub.vault, "canary.md", "after reuse remove")?;
    hub.api.scan("controller-fixture")?;
    wait("external sync continues after reuse removal", || {
        Ok(fs::read_to_string(client.vault.join("canary.md"))
            .ok()
            .as_deref()
            == Some("after reuse remove"))
    })?;
    assert!(reuse
        .enroll(&identity, &configs, &approval, &client.vault)
        .is_err());
    // After Remove completes, later external changes belong to the external
    // owner. Repeating Remove is observational and must not resume the folder.
    client
        .api
        .patch_folder("controller-fixture", &json!({"paused":true}))?;
    assert!(reuse.remove()?.external_sync_retained);
    assert_eq!(client.api.folder("controller-fixture")?["paused"], true);
    client
        .api
        .patch_folder("controller-fixture", &json!({"paused":false}))?;
    // Unavailable daemon inventory blocks mutation rather than being ignored.
    let incomplete = vec![config_path, root.path().join("missing.xml")];
    assert!(reopened
        .enroll(&identity, &incomplete, &approval, &client.vault)
        .is_err());
    let unknown = root.path().join("unknown");
    fs::create_dir(&unknown)?;
    fs::write(unknown.join("local.md"), "must not be imported")?;
    let mut other = approval.clone();
    other.descriptor.as_mut().unwrap().folder_id = "unknown-folder".into();
    let unknown_controller = FolderController::new(root.path().join("unknown-controller"));
    assert!(unknown_controller
        .enroll(&identity, &configs, &other, &unknown)
        .is_err());
    let mut changed_policy = approval.clone();
    changed_policy
        .descriptor
        .as_mut()
        .unwrap()
        .ignores
        .push("/different-policy".into());
    let mismatch = FolderController::new(root.path().join("policy-controller"));
    let before = client.api.folder("controller-fixture")?;
    assert!(mismatch
        .enroll(&identity, &configs, &changed_policy, &client.vault)
        .is_err());
    assert_eq!(client.api.folder("controller-fixture")?, before);
    wait("authenticated hub connection observed", || {
        Ok(reopened.status()?.hub_connected)
    })?;
    let observed = reopened.status()?.last_connected_at;
    assert!(observed.is_some());
    // Offline Remove is journaled before REST, and restart cannot re-enroll.
    client.stop()?;
    let offline = FolderController::new(controller_state.clone());
    assert_eq!(offline.last_connected_at()?, observed);
    assert!(offline.status().is_err());
    assert_eq!(offline.last_connected_at()?, observed);
    assert!(reopened.remove().is_err());
    client.start()?;
    assert!(reopened
        .enroll(&identity, &configs, &approval, &client.vault)
        .is_err());
    let removed = reopened.remove().context("remove owned folder")?;
    assert!(removed.removed && !removed.external_sync_retained);
    assert!(client.api.folder("controller-fixture").is_err());
    assert!(client.api.device(&hub.id).is_err());
    assert_eq!(client.api.folder("unrelated")?, unrelated);
    assert_eq!(
        fs::read_to_string(client.vault.join("canary.md"))?,
        "after reuse remove"
    );
    client.api.verify_identity(&client.id)?;
    Ok(())
}
