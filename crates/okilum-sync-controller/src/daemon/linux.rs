//! Linux process discovery and explicit package preparation. Never imported on macOS.
use super::{fingerprint, DaemonIdentity, CLIENT_VERSION};
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
use xmltree::{Element, XMLNode};

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
            } else if let Some(v) = arg
                .strip_prefix(&format!("{long}="))
                .or_else(|| arg.strip_prefix(&format!("{short}=")))
            {
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
/// Authenticate a completed, pinned managed preparation without launching a process.
/// An existing external home has no preparation receipt and cannot be adopted.
pub fn managed_identity(state: &Path) -> Result<DaemonIdentity> {
    let saved = managed_preparation(state)?;
    ensure!(
        executable_hash(&saved.request.executable)? == saved.binary_hash,
        "selected managed runtime preparation changed"
    );
    saved.identity.connect()?;
    Ok(saved.identity)
}

fn managed_preparation(state: &Path) -> Result<Prepared> {
    let saved: Prepared = serde_json::from_slice(&private::read(&state.join("prepared.json"))?)?;
    ensure!(
        saved.identity.config_file == state.join("syncthing/config.xml").canonicalize()?
            && saved.binary_version == CLIENT_VERSION
            && saved.binary_hash == crate::linux_bootstrap::BINARY_SHA,
        "selected managed runtime preparation changed"
    );
    saved.identity.verify_local()?;
    Ok(saved)
}

/// Address-only reconciliation of an already selected runtime uses the private
/// preparation receipt and pinned certificate. Its caller authenticates the live
/// daemon before REST effects. Package verification remains at preparation,
/// enable and source enrollment; hashing the entire executable in every paired
/// RPC can exhaust the channel's three-second response budget in debug builds.
/// This proof grants no lifecycle, device-trust or folder authority.
pub(crate) fn managed_transport_identity(state: &Path) -> Result<DaemonIdentity> {
    Ok(managed_preparation(state)?.identity)
}
pub(crate) fn managed_listen_address(state: &Path) -> Result<SocketAddr> {
    Ok(managed_preparation(state)?.request.listen_address)
}
/// Only call after explicit Enable. The random state directory must be new or
/// contain this preparation's durable intent; never point it at an external home.
pub fn prepare(state: &Path, request: &Preparation) -> Result<(ManagedInstance, DaemonIdentity)> {
    prepare_with_policy(state, request, false)
}

/// Managed transport with public discovery and relays; pairing itself stays in LAN.
pub fn prepare_lan(
    state: &Path,
    request: &Preparation,
) -> Result<(ManagedInstance, DaemonIdentity)> {
    prepare_with_policy(state, request, true)
}
fn validate_endpoints(rest: SocketAddr, listen: SocketAddr, lan: bool) -> Result<()> {
    ensure!(
        rest.ip().is_loopback()
            && rest.port() != 0
            && listen.port() != 0
            && rest != listen
            && (listen.ip().is_loopback() || lan && listen.ip().is_unspecified()),
        "invalid isolated endpoint allocation"
    );
    Ok(())
}
fn prepare_with_policy(
    state: &Path,
    request: &Preparation,
    lan: bool,
) -> Result<(ManagedInstance, DaemonIdentity)> {
    ensure!(
        state.is_absolute() && request.executable.is_absolute(),
        "absolute preparation paths required"
    );
    validate_endpoints(request.rest_address, request.listen_address, lan)?;
    let _lock = private::lock(state)?;
    let policy_file = state.join("transport-policy.json");
    if policy_file.try_exists()? {
        let saved: bool = serde_json::from_slice(&private::read(&policy_file)?)?;
        ensure!(saved == lan, "prepared transport policy changed");
    } else {
        ensure!(
            !lan || !state.join("prepare.json").try_exists()?,
            "legacy preparation needs explicit recovery"
        );
        private::write(&policy_file, &serde_json::to_vec(&lan)?)?;
    }
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
    // Once launch authority exists, a missing preparation receipt is uncertain
    // ownership, not permission to regenerate/rewrite a live runtime's config.
    // Launch/configuration admission must never be bypassed by preparation retry.
    ensure!(
        !state.join("managed-launch").try_exists()?,
        "managed preparation receipt missing; explicit recovery required"
    );
    if home.try_exists()? {
        let existing = Element::parse(private::read(&home.join("config.xml"))?.as_slice())?;
        ensure!(
            !existing
                .children
                .iter()
                .filter_map(XMLNode::as_element)
                .any(|e| e.name == "folder"),
            "prepared folder configuration has no receipt; explicit recovery required"
        );
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
        ("globalAnnounceEnabled", if lan { "true" } else { "false" }),
        ("localAnnounceEnabled", if lan { "true" } else { "false" }),
        ("natEnabled", if lan { "true" } else { "false" }),
        ("relaysEnabled", if lan { "true" } else { "false" }),
        ("stunKeepaliveStartS", if lan { "180" } else { "0" }),
        ("stunKeepaliveMinS", if lan { "20" } else { "0" }),
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
    if lan {
        let mut relay = Element::new("listenAddress");
        relay
            .children
            .push(XMLNode::Text(crate::network_policy::RELAY_LISTENER.into()));
        options.children.push(XMLNode::Element(relay));
    }
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
    crate::managed_launch::initialize(&ManagedInstance {
        home: home.clone(),
        executable: request.executable.clone(),
    })?;
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
    fn short_equals_paths_override_environment_without_splitting_values() -> Result<()> {
        let cwd = Path::new("/process cwd");
        let env = [
            "STHOMEDIR=/environment/home",
            "STCONFDIR=/environment/config",
        ];
        assert_eq!(
            process_configs(&["syncthing", "serve", "-C=relative config"], &env, cwd)?,
            vec![cwd.join("relative config/config.xml")]
        );
        assert_eq!(
            process_configs(
                &["syncthing", "serve", "-H=/explicit home"],
                &["STHOMEDIR=/environment/home"],
                cwd,
            )?,
            vec![PathBuf::from("/explicit home/config.xml")]
        );
        // Config selection still takes precedence over the home selection.
        assert_eq!(
            process_configs(
                &[
                    "syncthing",
                    "serve",
                    "-H=/explicit home",
                    "-C=config=directory"
                ],
                &env,
                cwd,
            )?,
            vec![cwd.join("config=directory/config.xml")]
        );
        Ok(())
    }

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

#[cfg(test)]
mod lan_policy_tests {
    use super::*;
    #[test]
    fn lan_listen_is_explicit_and_rest_stays_loopback() {
        let rest = "127.0.0.1:12345".parse().unwrap();
        let lan = "0.0.0.0:23456".parse().unwrap();
        assert!(validate_endpoints(rest, lan, true).is_ok());
        assert!(validate_endpoints(rest, lan, false).is_err());
        assert!(validate_endpoints(lan, rest, true).is_err());
        assert!(validate_endpoints(rest, "192.0.2.1:23456".parse().unwrap(), true).is_err());
        assert!(validate_endpoints(rest, "0.0.0.0:0".parse().unwrap(), true).is_err());
        assert!(validate_endpoints(rest, rest, true).is_err());
    }
}
