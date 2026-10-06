//! Real binaries are opt-in: scripts/sync-compatibility.sh on the remote build host.
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
use tessera_sync::{validate_destination, Destination, Syncthing, FIXTURE_IGNORES};

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
#[ignore = "requires pinned isolated Syncthing binaries; run scripts/sync-compatibility.sh"]
fn hub_1295_client_216() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut hub = Peer::new(
        root.path(),
        "hub",
        std::env::var_os("TESSERA_SYNC_HUB")
            .context("hub binary missing")?
            .into(),
    )?;
    let mut client = Peer::new(
        root.path(),
        "client",
        std::env::var_os("TESSERA_SYNC_CLIENT")
            .context("client binary missing")?
            .into(),
    )?;
    assert_eq!(hub.api.version()?["version"], "v1.29.5");
    assert_eq!(client.api.version()?["version"], "v2.1.6");
    assert_ne!(hub.id, client.id);
    assert!(Syncthing::connect(client.rest, "wrong-key")?
        .identity()
        .is_err());
    assert!(client.api.verify_identity(&hub.id).is_err());
    client.api.verify_identity(&client.id)?;
    eprintln!("stage: authenticated identity");
    let baseline = client.api.config()?;
    hub.api.add_device(&device(&client, false))?;
    client.api.add_device(&device(&hub, true))?;
    client.api.add_device(&device(&hub, true))?;
    assert!(client.api.add_device(&device(&hub, false)).is_err());
    assert_eq!(client.api.device(&hub.id)?["introducer"], true);
    assert_eq!(
        client.api.device(&hub.id)?["skipIntroductionRemovals"],
        true
    );
    assert_eq!(client.api.device(&hub.id)?["autoAcceptFolders"], false);
    assert_eq!(hub.api.device(&client.id)?["introducer"], false);
    assert_eq!(hub.api.device(&client.id)?["autoAcceptFolders"], false);
    assert!(matches!(
        validate_destination(&client.vault, "vault", &[])?,
        Destination::Empty(_)
    ));
    // Sentinel unrelated folder is never touched by enrollment.
    let unrelated_path = client.home.join("unrelated");
    fs::create_dir(&unrelated_path)?;
    client.api.add_paused_folder(
        &json!({"id":"unrelated","path":unrelated_path,"paused":true,"type":"sendreceive"}),
    )?;
    let unrelated = client.api.folder("unrelated")?;
    hub.api
        .add_paused_folder(&folder(&hub, &client, "vault", "sendreceive"))?;
    client
        .api
        .add_paused_folder(&folder(&client, &hub, "vault", "receiveonly"))?;
    assert!(client
        .api
        .add_paused_folder(&folder(&client, &hub, "vault", "sendreceive"))
        .is_err());
    for peer in [&hub, &client] {
        // Regression: Syncthing returns null for an empty policy, not always [].
        peer.api.set_ignores("vault", &[])?;
        peer.api.set_ignores("vault", FIXTURE_IGNORES)?;
    }
    assert!(client.api.set_ignores("vault", &["* "]).is_err());
    assert_eq!(
        client.api.ignores("vault")?["ignore"],
        json!(FIXTURE_IGNORES)
    );
    eprintln!("stage: ignores seeded");
    let allowed = ["canary.md", ".claude/skills/example/SKILL.md"];
    let blocked = [
        ".tessera-index/private",
        ".claude/worktrees/private.md",
        "worktrees/private.md",
    ];
    for path in allowed.iter().chain(blocked.iter()) {
        write(&hub.vault, path, "canary-v1")?;
    }
    for peer in [&hub, &client] {
        peer.api.patch_folder("vault", &json!({"paused":false}))?;
        peer.api.scan("vault")?;
    }
    wait("allowed files delivered", || {
        Ok(allowed
            .iter()
            .all(|p| fs::read_to_string(client.vault.join(p)).ok().as_deref() == Some("canary-v1")))
    })?;
    wait("initial receive complete", || {
        let s = client.api.status("vault")?;
        Ok(s["state"] == "idle" && s["needTotalItems"] == 0)
    })?;
    for path in blocked {
        assert!(
            !client.vault.join(path).exists(),
            "excluded file transferred: {path}"
        );
    }
    assert_eq!(client.api.folder("vault")?["type"], "receiveonly");
    let known = vec![("vault".into(), client.vault.clone())];
    assert!(matches!(
        validate_destination(&client.vault, "vault", &known)?,
        Destination::KnownReplica(_)
    ));
    assert!(validate_destination(&client.vault, "vault", &[]).is_err());
    assert!(validate_destination(&client.vault, "other", &known).is_err());
    eprintln!("stage: initial receive complete");
    // Receive-only local changes remain local, proven alongside incoming progress.
    write(&client.vault, "local-only.md", "local")?;
    client.api.scan("vault")?;
    write(&hub.vault, "canary.md", "canary-v2")?;
    hub.api.scan("vault")?;
    wait("incoming progress while receive-only", || {
        Ok(fs::read_to_string(client.vault.join("canary.md"))
            .ok()
            .as_deref()
            == Some("canary-v2"))
    })?;
    assert!(!hub.vault.join("local-only.md").exists());
    assert!(
        client.api.status("vault")?["receiveOnlyTotalItems"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    let shared_devices = client.api.folder("vault")?["devices"].clone();
    // Pausing the folder stops all peers; resuming supplies the positive control.
    client.api.patch_folder("vault", &json!({"paused":true}))?;
    write(&hub.vault, "paused-canary.md", "resume")?;
    hub.api.scan("vault")?;
    thread::sleep(Duration::from_secs(1));
    assert!(!client.vault.join("paused-canary.md").exists());
    eprintln!("stage: restart paused client");
    client.stop()?;
    client.start()?;
    client.api.verify_identity(&client.id)?;
    assert_eq!(client.api.folder("vault")?["paused"], true);
    client.api.patch_folder("vault", &json!({"paused":false}))?;
    wait("resume positive control", || {
        Ok(client.vault.join("paused-canary.md").exists())
    })?;
    assert_eq!(client.api.folder("vault")?["devices"], shared_devices);
    assert_eq!(client.api.folder("unrelated")?, unrelated);
    assert_eq!(client.api.config()?["options"], baseline["options"]);
    assert_eq!(client.api.config()?["gui"], baseline["gui"]);
    eprintln!("stage: marker fault");
    // Never repair a missing mount marker automatically.
    fs::remove_dir_all(client.vault.join(".stfolder"))?;
    let _ = client.api.scan("vault");
    wait("missing marker surfaced", || {
        Ok(client.api.status("vault")?["error"]
            .as_str()
            .is_some_and(|e| e.contains("marker")))
    })?;
    assert!(!client.vault.join(".stfolder").exists());
    client.stop()?;
    ensure!(
        TcpListener::bind(client.rest).is_ok(),
        "owned REST listener still active"
    );
    hub.stop()?;
    ensure!(
        TcpListener::bind(hub.rest).is_ok(),
        "owned hub REST listener still active"
    );
    println!("PASS: 1.29.5 ↔ 2.1.6 isolated REST/auth/config/ignores/receive/pause/restart/marker checks");
    Ok(())
}
