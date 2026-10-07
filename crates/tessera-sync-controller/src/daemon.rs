//! Read-only daemon discovery and explicitly requested offline preparation.
//! A prepared instance does not launch or register a service.
use crate::{lifecycle::ManagedInstance, private};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    net::SocketAddr,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};
use tessera_sync::Syncthing;
use xmltree::{Element, XMLNode};

pub const CLIENT_VERSION: &str = "v2.1.6";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DaemonIdentity {
    pub config_file: PathBuf,
    pub device_id: String,
    pub certificate_hash: String,
}
impl DaemonIdentity {
    /// Re-read the credential locally and authenticate the same instance.
    pub fn connect(&self) -> Result<Syncthing> {
        let config = read_config(&self.config_file)?;
        ensure!(
            fingerprint(&self.config_file)? == self.certificate_hash,
            "daemon certificate changed"
        );
        let api = Syncthing::connect(config.address, &config.key)?;
        api.verify_identity(&self.device_id)?;
        ensure!(
            api.version()?["version"] == CLIENT_VERSION,
            "unsupported Syncthing version"
        );
        Ok(api)
    }
}
pub struct Candidate {
    pub identity: DaemonIdentity,
    pub folders: Vec<(String, PathBuf)>,
    pub version: String,
}
pub struct Discovery {
    pub candidates: Vec<Candidate>,
    /// An inaccessible candidate cannot silently be treated as no existing daemon.
    pub unavailable: Vec<PathBuf>,
}
impl Discovery {
    pub fn select(&self, config: &Path) -> Result<&Candidate> {
        ensure!(
            self.unavailable.is_empty(),
            "resolve unavailable Syncthing configurations before enrollment"
        );
        let config = config.canonicalize()?;
        self.candidates
            .iter()
            .find(|c| c.identity.config_file == config)
            .context("explicit daemon selection required")
    }
    pub fn folders(&self) -> Result<Vec<(String, PathBuf)>> {
        ensure!(
            self.unavailable.is_empty(),
            "daemon inventory is incomplete"
        );
        Ok(self
            .candidates
            .iter()
            .flat_map(|c| c.folders.clone())
            .collect())
    }
}
/// The caller supplies explicit paths in addition to standard/proc discovery.
/// No credential, process, home, directory or service is created here.
pub fn discover(configs: &[PathBuf]) -> Discovery {
    let mut paths = BTreeSet::new();
    let mut result = Discovery {
        candidates: vec![],
        unavailable: vec![],
    };
    for path in configs {
        let inspected = (|| -> Result<Candidate> {
            let path = path.canonicalize()?;
            ensure!(paths.insert(path.clone()), "duplicate candidate");
            let config = read_config(&path)?;
            let api = Syncthing::connect(config.address, &config.key)?;
            let version = api.version()?["version"]
                .as_str()
                .context("missing version")?
                .to_owned();
            ensure!(
                [CLIENT_VERSION, "v1.29.5"].contains(&version.as_str()),
                "unsupported inventory version"
            );
            let id = api.identity()?["myID"]
                .as_str()
                .context("missing device identity")?
                .to_owned();
            let all = api.config()?;
            let folders = all["folders"]
                .as_array()
                .context("invalid daemon inventory")?
                .iter()
                .map(|f| -> Result<_> {
                    Ok((
                        f["id"].as_str().context("missing folder ID")?.to_owned(),
                        PathBuf::from(f["path"].as_str().context("missing folder path")?)
                            .canonicalize()?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Candidate {
                identity: DaemonIdentity {
                    device_id: id,
                    certificate_hash: fingerprint(&path)?,
                    config_file: path,
                },
                folders,
                version,
            })
        })();
        match inspected {
            Ok(candidate) => result.candidates.push(candidate),
            Err(_)
                if path.canonicalize().is_ok_and(|p| {
                    result
                        .candidates
                        .iter()
                        .any(|c| c.identity.config_file == p)
                }) => {}
            Err(_) => result.unavailable.push(path.clone()),
        }
    }
    result
}
/// Linux user-owned process configuration paths; no scan of vault contents.
/// An unreadable Syncthing process config is surfaced rather than ignored.
pub fn configuration_paths(
    home: &Path,
    state_home: &Path,
    config_home: &Path,
) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for p in [
        state_home.join("syncthing/config.xml"),
        config_home.join("syncthing/config.xml"),
        home.join(".local/state/syncthing/config.xml"),
        home.join(".config/syncthing/config.xml"),
    ] {
        if p.try_exists()? {
            paths.insert(p);
        }
    }
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let process = entry.path();
        let Ok(meta) = fs::metadata(&process) else {
            continue;
        };
        if meta.uid() != rustix::process::geteuid().as_raw() {
            continue;
        }
        let Ok(exe) = fs::read_link(process.join("exe")) else {
            continue;
        };
        if exe
            .file_name()
            .is_none_or(|n| n != "syncthing" && n != "syncthing (deleted)")
        {
            continue;
        }
        let bytes =
            fs::read(process.join("cmdline")).context("cannot inspect existing Syncthing")?;
        let args = bytes
            .split(|b| *b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect::<Vec<_>>();
        let env = fs::read(process.join("environ"))
            .context("cannot inspect existing Syncthing configuration")?;
        let env = env
            .split(|b| *b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect::<Vec<_>>();
        let cwd = fs::read_link(process.join("cwd"))?;
        for path in process_configs(&args, &env, &cwd)? {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}
/// Resolve each process independently: another process's default path is never
/// evidence for this one's inventory. Probe commands do not serve any folders.
fn process_configs(args: &[&str], env: &[&str], cwd: &Path) -> Result<Vec<PathBuf>> {
    if args.iter().skip(1).any(|s| {
        [
            "generate",
            "device-id",
            "cli",
            "--version",
            "version",
            "--help",
            "-h",
        ]
        .contains(s)
    }) {
        return Ok(vec![]);
    }
    let argument = |long: &str, short: &str| -> Result<Option<PathBuf>> {
        let mut value = None;
        for (i, arg) in args.iter().enumerate().skip(1) {
            if *arg == long || *arg == short {
                value = Some(PathBuf::from(
                    args.get(i + 1).context("missing daemon path argument")?,
                ));
            } else if let Some(v) = arg.strip_prefix(&format!("{long}=")) {
                value = Some(PathBuf::from(v));
            }
        }
        Ok(value)
    };
    let variable = |name: &str| {
        env.iter()
            .find_map(|s| s.strip_prefix(name))
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    };
    let config = argument("--config", "-C")?.or_else(|| variable("STCONFDIR="));
    let home = argument("--home", "-H")?.or_else(|| variable("STHOMEDIR="));
    let absolute = |p: PathBuf| if p.is_absolute() { p } else { cwd.join(p) };
    if let Some(p) = config.or(home) {
        return Ok(vec![absolute(p).join("config.xml")]);
    }
    let home = variable("HOME=").context("existing daemon home cannot be resolved")?;
    ensure!(home.is_absolute(), "existing daemon HOME must be absolute");
    let state = variable("XDG_STATE_HOME=").unwrap_or_else(|| home.join(".local/state"));
    let config = variable("XDG_CONFIG_HOME=").unwrap_or_else(|| home.join(".config"));
    let mut found = vec![];
    for path in [state, config] {
        let path = absolute(path).join("syncthing/config.xml");
        if path.try_exists()? {
            found.push(path);
        }
    }
    ensure!(
        !found.is_empty(),
        "existing Syncthing configuration must be selected explicitly"
    );
    Ok(found)
}
struct RestConfig {
    address: SocketAddr,
    key: String,
}
fn read_config(path: &Path) -> Result<RestConfig> {
    let xml = Element::parse(private::read(path)?.as_slice())?;
    let gui = xml.get_child("gui").context("GUI configuration missing")?;
    ensure!(
        gui.attributes.get("tls").is_none_or(|s| s == "false"),
        "existing HTTPS REST requires explicit support"
    );
    let address: SocketAddr = gui
        .get_child("address")
        .and_then(Element::get_text)
        .context("REST address missing")?
        .parse()?;
    ensure!(
        address.ip().is_loopback() && address.port() != 0,
        "REST must be literal loopback"
    );
    let key = gui
        .get_child("apikey")
        .and_then(Element::get_text)
        .context("REST credential missing")?
        .into_owned();
    ensure!(!key.is_empty(), "REST credential missing");
    Ok(RestConfig { address, key })
}
fn fingerprint(config: &Path) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(private::public_file(&config.with_file_name("cert.pem"))?)
    ))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    pub executable: PathBuf,
    pub rest_address: SocketAddr,
    /// Development policy: loopback-only transport, discovery/relay/NAT off.
    pub listen_address: SocketAddr,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    request: Preparation,
    binary_version: String,
    binary_hash: String,
    identity: DaemonIdentity,
}
/// Only call after explicit Enable. The random state directory must be new or
/// contain this preparation's durable intent; never point it at an external home.
pub fn prepare(state: &Path, request: &Preparation) -> Result<(ManagedInstance, DaemonIdentity)> {
    ensure!(
        state.is_absolute() && request.executable.is_absolute(),
        "absolute preparation paths required"
    );
    ensure!(
        request.rest_address.ip().is_loopback()
            && request.listen_address.ip().is_loopback()
            && request.rest_address.port() != 0
            && request.listen_address.port() != 0
            && request.rest_address != request.listen_address,
        "invalid isolated endpoint allocation"
    );
    let _lock = private::lock(state)?;
    let intent = state.join("prepare.json");
    let home = state.join("syncthing");
    if intent.try_exists()? {
        let saved: Preparation = serde_json::from_slice(&private::read(&intent)?)?;
        ensure!(
            &saved == request,
            "preparation endpoint or executable changed"
        );
    } else {
        ensure!(!home.exists(), "existing daemon home cannot be adopted");
        private::write(&intent, &serde_json::to_vec(request)?)?;
    }
    let version = Command::new(&request.executable)
        .arg("--version")
        .env_clear()
        .env("HOME", &home)
        .output()?;
    ensure!(
        version.status.success()
            && String::from_utf8_lossy(&version.stdout)
                .split_whitespace()
                .nth(1)
                == Some(CLIENT_VERSION),
        "unsupported package Syncthing version"
    );
    let binary_hash = executable_hash(&request.executable)?;
    let receipt = state.join("prepared.json");
    if receipt.try_exists()? {
        let saved: Prepared = serde_json::from_slice(&private::read(&receipt)?)?;
        ensure!(
            &saved.request == request
                && saved.binary_version == CLIENT_VERSION
                && saved.binary_hash == binary_hash
                && fingerprint(&saved.identity.config_file)? == saved.identity.certificate_hash,
            "prepared identity or package binary changed"
        );
        return Ok((
            ManagedInstance {
                home,
                executable: request.executable.clone(),
            },
            saved.identity,
        ));
    }
    private::directory(&home)?;
    let output = Command::new(&request.executable)
        .args(["generate", "--home"])
        .arg(&home)
        .env_clear()
        .env("HOME", &home)
        .output()?;
    ensure!(
        output.status.success(),
        "offline Syncthing preparation failed"
    );
    for name in ["config.xml", "cert.pem", "key.pem"] {
        let path = home.join(name);
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "prepared file replaced"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let config = home.join("config.xml");
    let mut xml = Element::parse(private::read(&config)?.as_slice())?;
    xml.children
        .retain(|node| !matches!(node,XMLNode::Element(e) if e.name=="folder"));
    let gui = xml.get_mut_child("gui").context("GUI missing")?;
    gui.attributes.insert("tls".into(), "false".into());
    set(gui, "address", &request.rest_address.to_string());
    let options = xml.get_mut_child("options").context("options missing")?;
    for (name, value) in [
        ("globalAnnounceEnabled", "false"),
        ("localAnnounceEnabled", "false"),
        ("natEnabled", "false"),
        ("relaysEnabled", "false"),
        ("startBrowser", "false"),
        ("autoUpgradeIntervalH", "0"),
        ("urAccepted", "-1"),
        ("crashReportingEnabled", "false"),
    ] {
        set(options, name, value);
    }
    options
        .children
        .retain(|n| !matches!(n,XMLNode::Element(e) if e.name=="listenAddress"));
    set(
        options,
        "listenAddress",
        &format!("tcp://{}", request.listen_address),
    );
    let mut bytes = Vec::new();
    xml.write(&mut bytes)?;
    private::write(&config, &bytes)?;
    let output = Command::new(&request.executable)
        .args(["device-id", "--home"])
        .arg(&home)
        .env_clear()
        .env("HOME", &home)
        .output()?;
    ensure!(
        output.status.success(),
        "prepared device identity unavailable"
    );
    let id = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(id.len() == 63, "invalid prepared identity");
    let identity = DaemonIdentity {
        config_file: config.clone(),
        device_id: id,
        certificate_hash: fingerprint(&config)?,
    };
    private::write(
        &receipt,
        &serde_json::to_vec(&Prepared {
            request: request.clone(),
            binary_version: CLIENT_VERSION.into(),
            binary_hash,
            identity: identity.clone(),
        })?,
    )?;
    Ok((
        ManagedInstance {
            home,
            executable: request.executable.clone(),
        },
        identity,
    ))
}
// Package replacement between prepare and Enable remains coordinated external
// administration, just like replacement of the service definition. Re-check on
// every preparation/re-enable, even when the durable receipt already exists.
fn executable_hash(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "package executable must be a regular file"
    );
    let mut digest = Sha256::new();
    let mut block = [0u8; 8192];
    loop {
        let read = file.read(&mut block)?;
        if read == 0 {
            break;
        }
        digest.update(&block[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn set(parent: &mut Element, name: &str, value: &str) {
    if parent.get_child(name).is_none() {
        parent.children.push(XMLNode::Element(Element::new(name)));
    }
    let child = parent.get_mut_child(name).unwrap();
    child.children = vec![XMLNode::Text(value.into())];
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_process_resolves_its_own_paths_or_fails_closed() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cwd = root.path();
        assert!(process_configs(&["syncthing", "serve"], &[], cwd).is_err());
        assert_eq!(
            process_configs(&["syncthing", "serve", "--home", "relative"], &[], cwd)?,
            vec![cwd.join("relative/config.xml")]
        );
        assert_eq!(
            process_configs(
                &["syncthing", "serve", "--home=/other", "--config=/specific"],
                &[],
                cwd
            )?,
            vec![PathBuf::from("/specific/config.xml")]
        );
        assert!(process_configs(&["syncthing", "--version"], &[], cwd)?.is_empty());
        fs::create_dir_all(cwd.join(".local/state/syncthing"))?;
        fs::write(cwd.join(".local/state/syncthing/config.xml"), "fixture")?;
        let home = format!("HOME={}", cwd.display());
        assert_eq!(
            process_configs(&["syncthing", "serve"], &[&home], cwd)?,
            vec![cwd.join(".local/state/syncthing/config.xml")]
        );
        Ok(())
    }
}
