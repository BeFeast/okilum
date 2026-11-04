//! Disposable Syncthing daemons only; no user inventory, config or lifecycle.
#[path = "fixture_process.rs"]
mod fixture_process;
use anyhow::{ensure, Context, Result};
use okilum_sync::Syncthing;
use serde_json::json;
use std::{
    fs,
    net::{SocketAddr, TcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
pub struct Peer {
    pub child: Option<Child>,
    pub binary: PathBuf,
    pub home: PathBuf,
    pub vault: PathBuf,
    pub api: Syncthing,
    pub api_key: String,
    pub rest: SocketAddr,
    pub listen: SocketAddr,
    pub id: String,
}
impl Peer {
    pub fn new(root: &Path, name: &str, binary: PathBuf) -> Result<Self> {
        let home = root.join(name);
        fs::create_dir(&home)?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
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
        fs::set_permissions(home.join("config.xml"), fs::Permissions::from_mode(0o600))?;
        let api = Syncthing::connect(rest, key.trim())?;
        drop(rest_guard);
        drop(listen_guard);
        let mut peer = Self {
            child: None,
            binary,
            home,
            vault,
            api,
            api_key: key.trim().to_owned(),
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
    /// Real managed preparation, with no systemd registration. Only this fixture
    /// owns/reaps the child; production source sharing cannot start a daemon.
    pub fn managed(root: &Path, name: &str, binary: PathBuf) -> Result<(Self, PathBuf)> {
        Self::managed_with_listener(root, name, binary, false, true)
    }
    pub fn managed_lan(root: &Path, name: &str, binary: PathBuf) -> Result<(Self, PathBuf)> {
        Self::managed_with_listener(root, name, binary, true, true)
    }
    // Shared test helper: only network_policy.rs uses it, direct_bidirectional.rs does not.
    #[allow(dead_code)]
    pub fn managed_network(root: &Path, name: &str, binary: PathBuf) -> Result<(Self, PathBuf)> {
        Self::managed_with_listener(root, name, binary, true, false)
    }
    fn managed_with_listener(
        root: &Path,
        name: &str,
        binary: PathBuf,
        lan: bool,
        isolated: bool,
    ) -> Result<(Self, PathBuf)> {
        use okilum_sync_controller::daemon::{prepare, prepare_lan, Preparation};
        let state = root.join(name);
        fs::create_dir(&state)?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        let prepared = state.join("prepared");
        let vault = state.join("ordinary-vault");
        fs::create_dir(&vault)?;
        let rest_guard = TcpListener::bind("127.0.0.1:0")?;
        let listen_guard = TcpListener::bind(if lan { "0.0.0.0:0" } else { "127.0.0.1:0" })?;
        let rest = rest_guard.local_addr()?;
        let listen = listen_guard.local_addr()?;
        let prepare = if lan { prepare_lan } else { prepare };
        let (instance, identity) = prepare(
            &prepared,
            &Preparation {
                executable: binary.clone(),
                rest_address: rest,
                listen_address: listen,
            },
        )?;
        let mut xml = xmltree::Element::parse(fs::read(&identity.config_file)?.as_slice())?;
        if isolated {
            // Explicit-address acceptance must never fall back to UDP discovery.
            let options = xml
                .get_mut_child("options")
                .context("fixture options missing")?;
            for key in [
                "localAnnounceEnabled",
                "globalAnnounceEnabled",
                "relaysEnabled",
                "natEnabled",
            ] {
                options
                    .get_mut_child(key)
                    .context("fixture flag missing")?
                    .children = vec![xmltree::XMLNode::Text("false".into())];
            }
            for key in ["stunKeepaliveStartS", "stunKeepaliveMinS"] {
                options
                    .get_mut_child(key)
                    .context("fixture STUN missing")?
                    .children = vec![xmltree::XMLNode::Text("0".into())];
            }
            options.children.retain(|node| !matches!(node, xmltree::XMLNode::Element(e)
                if e.name == "listenAddress" && e.get_text().is_some_and(|s| s.starts_with("dynamic+"))));
            let mut bytes = Vec::new();
            xml.write(&mut bytes)?;
            fs::write(&identity.config_file, bytes)?;
        }
        let key = xml
            .get_child("gui")
            .and_then(|g| g.get_child("apikey"))
            .and_then(xmltree::Element::get_text)
            .context("fixture key missing")?
            .into_owned();
        let api = Syncthing::connect(rest, &key)?;
        drop(rest_guard);
        drop(listen_guard);
        let mut peer = Self {
            child: None,
            binary,
            home: instance.home,
            vault,
            api,
            api_key: key,
            rest,
            listen,
            id: identity.device_id,
        };
        peer.start()?;
        Ok((peer, prepared))
    }
    // Fixture-only connection control; production enrollment deliberately refuses
    // to mutate existing devices through add_device.
    pub fn pause_remote(&self, id: &str, paused: bool) -> Result<()> {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()?
            .patch(format!("http://{}/rest/config/devices/{id}", self.rest))
            .header("X-API-Key", &self.api_key)
            .json(&json!({"paused":paused}))
            .send()?
            .error_for_status()?;
        Ok(())
    }
    pub fn start(&mut self) -> Result<()> {
        self.stop()?;
        let log = fs::File::create(self.home.join("fixture.log"))?;
        self.child = Some(fixture_process::spawn(
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
                .stderr(log),
        )?);
        wait("REST ready", || Ok(self.api.identity().is_ok()))
    }
    pub fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            fixture_process::stop(&mut child)?;
        }
        Ok(())
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
pub fn wait(label: &str, mut f: impl FnMut() -> Result<bool>) -> Result<()> {
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
