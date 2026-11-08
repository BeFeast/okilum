//! Dial hints from the authenticated paired channel, never discovery or a URL.
//! Only the selected managed runtime is changed; external reuse stays untouched.
use super::*;
use crate::daemon::DaemonIdentity;
use serde_json::{json, Value};
use std::time::Instant;

const MAX_ADDRESSES: usize = 32;
const CONNECTION_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Transport {
    device: String,
    addresses: Vec<String>,
}

fn endpoint(value: &str) -> Option<SocketAddr> {
    if value.len() > 128 {
        return None;
    }
    let literal = value
        .strip_prefix("tcp://")
        .or_else(|| value.strip_prefix("quic://"))?;
    let address: SocketAddr = literal.parse().ok()?;
    let ip = match address.ip() {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    };
    let link_local = match ip {
        IpAddr::V4(ip) => ip.is_link_local() || ip.is_broadcast(),
        IpAddr::V6(ip) => ip.is_unicast_link_local(),
    };
    (address.port() != 0
        && !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !link_local)
        .then_some(address)
}
fn filtered(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result: Vec<_> = values
        .into_iter()
        .filter(|s| endpoint(s).is_some())
        .collect();
    result.sort();
    result.dedup();
    result.truncate(MAX_ADDRESSES);
    result
}
fn advertised(status: &Value) -> Vec<String> {
    filtered(
        status["connectionServiceStatus"]
            .as_object()
            .into_iter()
            .flat_map(|services| {
                services
                    .values()
                    .flat_map(|s| s["lanAddresses"].as_array().into_iter().flatten())
            })
            .filter_map(Value::as_str)
            .map(str::to_owned),
    )
}
pub(super) fn with_dynamic(values: &[String]) -> Vec<String> {
    let mut addresses = filtered(values.iter().cloned());
    addresses.push("dynamic".into());
    addresses
}

#[cfg(target_os = "linux")]
pub(super) fn managed(configs: &[PathBuf]) -> Result<Option<DaemonIdentity>> {
    let [config] = configs else { return Ok(None) };
    let Some(state) = config.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    if !state.join("prepared.json").try_exists()? {
        return Ok(None);
    }
    let identity = crate::daemon::managed_transport_identity(state)?;
    ensure!(
        identity.config_file == config.canonicalize()?,
        "selected managed configuration changed"
    );
    Ok(Some(identity))
}
#[cfg(target_os = "macos")]
pub(super) fn managed(configs: &[PathBuf]) -> Result<Option<DaemonIdentity>> {
    let [config] = configs else {
        return Ok(None);
    };
    let state = crate::sidecar::macos::active_state_directory()?;
    let Some(owned) = crate::sidecar::discovery::managed_configuration_path(&state)? else {
        return Ok(None);
    };
    if config.canonicalize()? != owned.canonicalize()? {
        return Ok(None);
    }
    // Transport hints are optional. An unclaimed preparation must not prevent
    // creating an invitation; source enrollment independently requires Ready.
    Ok(crate::sidecar::macos::bootstrap::managed_source_identity(&state).ok())
}

/// Whether `daemon` is this computer's own Okilum-managed Syncthing. Device
/// trust such as an introducer role never changes in an existing user Syncthing.
#[cfg(target_os = "linux")]
pub(super) fn owned_runtime(configs: &[PathBuf], daemon: &DaemonIdentity) -> Result<bool> {
    Ok(managed(configs)?.is_some_and(|managed| managed == *daemon))
}
#[cfg(target_os = "macos")]
pub(super) fn owned_runtime(_configs: &[PathBuf], daemon: &DaemonIdentity) -> Result<bool> {
    let state = crate::sidecar::macos::active_state_directory()?;
    Ok(
        match crate::sidecar::discovery::managed_configuration_path(&state)? {
            Some(config) => config.canonicalize()? == daemon.config_file,
            None => false,
        },
    )
}

#[cfg(windows)]
pub(super) fn managed(configs: &[PathBuf]) -> Result<Option<DaemonIdentity>> {
    let [config] = configs else { return Ok(None) };
    let state = crate::sidecar::windows::bootstrap::active_state_directory()?;
    let owned = state.join("config/config.xml");
    if !owned.try_exists()? || config.canonicalize()? != owned.canonicalize()? {
        return Ok(None);
    }
    Ok(crate::daemon::managed_identity(&state).ok())
}
#[cfg(windows)]
pub(super) fn owned_runtime(configs: &[PathBuf], daemon: &DaemonIdentity) -> Result<bool> {
    Ok(managed(configs)?.is_some_and(|managed| managed == *daemon))
}

pub(super) fn remember(peer: &mut Paired, transport: &Transport) -> Result<()> {
    ensure!(
        transport.addresses.len() <= MAX_ADDRESSES,
        "too many peer addresses"
    );
    ensure!(
        peer.syncthing_device
            .as_ref()
            .is_none_or(|id| id == &transport.device),
        "paired Syncthing identity changed"
    );
    // Use the same certificate syntax gate as the explicitly approved request.
    Request {
        id: peer.peer.id,
        name: peer.peer.name.clone(),
        public: peer.public.clone(),
        syncthing_device: Some(transport.device.clone()),
    }
    .validate(&peer.public)?;
    peer.syncthing_device = Some(transport.device.clone());
    let addresses = filtered(transport.addresses.iter().cloned());
    // REST may become ready before its transport listener. Retain the last
    // usable hints through that startup interval instead of reverting to dynamic.
    if !addresses.is_empty() {
        peer.addresses = addresses;
    }
    Ok(())
}
fn closed_channel(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe
            )
        })
    })
}

impl Controller {
    pub(super) fn local_transport(&self) -> Result<Option<Transport>> {
        let Some(identity) = managed(&self.configuration_paths()?)? else {
            return Ok(None);
        };
        Ok(Some(Transport {
            device: identity.device_id.clone(),
            addresses: advertised(&identity.connect()?.identity()?),
        }))
    }
    pub(super) fn accept_transport(&self, peer: &mut Paired, transport: &Transport) -> Result<()> {
        let previous = peer.addresses.clone();
        remember(peer, transport)?;
        self.apply_addresses(peer, &previous)
    }
    fn apply_addresses(&self, peer: &Paired, previous: &[String]) -> Result<()> {
        if peer.addresses.is_empty() && previous.is_empty() {
            return Ok(());
        }
        let Some(identity) = managed(&self.configuration_paths()?)? else {
            return Ok(());
        };
        let Some(device) = &peer.syncthing_device else {
            return Ok(());
        };
        ensure!(device != &identity.device_id, "peer is the local daemon");
        let api = identity.connect()?;
        // Address exchange never grants device trust or creates folder membership.
        let config = api.config()?;
        let devices = config["devices"]
            .as_array()
            .context("invalid device inventory")?;
        if let Some(current) = devices.iter().find(|d| d["deviceID"] == *device) {
            let existing = current["addresses"]
                .as_array()
                .context("invalid peer addresses")?;
            let mut addresses: Vec<String> = existing
                .iter()
                .filter_map(Value::as_str)
                .filter(|a| !previous.iter().any(|old| old.as_str() == *a))
                .map(str::to_owned)
                .collect();
            addresses.extend(peer.addresses.iter().cloned());
            addresses.push("dynamic".into());
            addresses.sort();
            addresses.dedup();
            if json!(addresses) != current["addresses"] {
                let _admission = crate::managed_launch::runtime_configuration(&identity)?;
                api.set_device_addresses(device, &addresses)?;
            }
        }
        Ok(())
    }
    pub(super) fn reconcile_peer_addresses(&self) -> Result<()> {
        if managed(&self.configuration_paths()?)?.is_none() {
            return Ok(());
        }
        let (served, peers) = {
            let db = self.shared.db.lock().unwrap();
            (served(&db), db.peers.values().cloned().collect::<Vec<_>>())
        };
        for peer in peers {
            let live = || removal::peer_is_live(&self.shared.db.lock().unwrap(), &peer);
            if !live() {
                continue;
            }
            // Reapply durable hints at startup; an offline computer cannot erase them.
            self.apply_addresses(&peer, &peer.addresses)?;
            // A computer that joined this one reaches it at one of this
            // computer's own endpoints; calling that would call this computer.
            if !served.contains(&peer.endpoint) {
                // Refresh transport hints without changing the user's selected
                // catalog when more than one computer is paired.
                match self.call(
                    peer.endpoint,
                    peer.peer.id,
                    &peer.key,
                    &peer.public,
                    Operation::Catalog {},
                ) {
                    Ok(Reply::Catalog { .. }) => {}
                    Err(_) if !live() => {}
                    // Revocation or process exit can close an optional hint
                    // exchange before a reply. Keep the saved hints; no new
                    // transport/configuration authority comes from that socket.
                    Err(error) if transfer::offline(&error) || closed_channel(&error) => {}
                    Err(error) => return Err(error),
                    _ if !live() => {}
                    _ => anyhow::bail!("authenticated peer catalog unavailable"),
                }
            }
        }
        Ok(())
    }
    /// The status line for paired computers this computer's Syncthing does not
    /// reach. A computer whose vaults keep syncing through another connected
    /// member of the same folders (introduced by the share peer, #2622) is
    /// simply offline; the firewall hint is only for a computer whose vaults no
    /// connected device carries (#2589).
    pub(super) fn refresh_connection_status(&self) -> Result<()> {
        let Some(identity) = managed(&self.configuration_paths()?)? else {
            return Ok(());
        };
        let api = identity.connect()?;
        let connections = api.connections()?;
        let config = api.config()?;
        let devices = config["devices"]
            .as_array()
            .context("invalid device inventory")?;
        // REST reads below do not hold the journal lock.
        let peers: Vec<_> = self
            .shared
            .db
            .lock()
            .unwrap()
            .peers
            .values()
            .cloned()
            .collect();
        let paired = peers
            .iter()
            .filter_map(|p| Some((p.syncthing_device.clone()?, p.peer.name.clone())))
            .collect();
        let observed = Observed {
            local: &identity.device_id,
            config: &config,
            connections: &connections,
            paired: &paired,
        };
        let mut links = BTreeMap::new();
        for peer in &peers {
            let Some(device) = &peer.syncthing_device else {
                continue;
            };
            if devices.iter().any(|d| d["deviceID"] == *device) {
                let link = observed.link(
                    device,
                    |folder| api.status(folder).ok(),
                    |folder, member| api.completion(folder, member).ok(),
                );
                links.insert(peer.peer.id, link);
            }
        }
        let mut watch = self.shared.connection_watch.lock().unwrap();
        let mut offline = Vec::new();
        for peer in &peers {
            match links.get(&peer.peer.id) {
                None => {}
                Some(PeerLink::Connected) => {
                    watch.since.remove(&peer.peer.id);
                }
                Some(link) => {
                    let since = watch.since.entry(peer.peer.id).or_insert_with(Instant::now);
                    offline.push((peer, link, since.elapsed()));
                }
            }
        }
        watch.status = offline_status(offline.iter().map(|&(peer, link, elapsed)| {
            (
                peer.peer.name.as_str(),
                peer.addresses.as_slice(),
                link,
                elapsed,
            )
        }));
        watch.links = links;
        Ok(())
    }
    /// What this computer's Syncthing last observed about each paired computer
    /// (#2589). Memory only; empty until a refresh reads a managed runtime.
    pub fn peer_links(&self) -> Result<BTreeMap<Uuid, PeerLink>> {
        self.check()?;
        Ok(self.shared.connection_watch.lock().unwrap().links.clone())
    }
}
#[derive(Default)]
pub(super) struct ConnectionWatch {
    since: BTreeMap<Uuid, Instant>,
    pub status: Option<String>,
    links: BTreeMap<Uuid, PeerLink>,
}
/// A paired computer as this computer's Syncthing sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PeerLink {
    Connected,
    /// Offline, while every vault shared with it keeps syncing through other
    /// connected members of the same Syncthing folders. `up_to_date` when this
    /// computer and one of them hold the whole of every such vault.
    Relayed {
        through: Vec<Relay>,
        up_to_date: bool,
    },
    /// Offline, and no other connected computer carries one of its vaults.
    Unreachable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Relay {
    pub device: String,
    /// The paired computer's name, else a usable Syncthing device name.
    pub name: Option<String>,
}
/// One read of this computer's Syncthing: `/rest/config`, `/rest/system/connections`
/// and the Okilum names of paired Syncthing devices.
struct Observed<'a> {
    local: &'a str,
    config: &'a Value,
    connections: &'a Value,
    paired: &'a BTreeMap<String, String>,
}
impl Observed<'_> {
    fn connected(&self, device: &str) -> bool {
        let connection = &self.connections["connections"][device];
        connection["connected"] == true && connection["paused"] != true
    }
    /// The vaults of `device` are the folders that list it. Each one must be
    /// sent and received here and still shared by another connected member
    /// (`/rest/db/completion` remoteState) to keep syncing without `device`.
    /// `status` reads `/rest/db/status`, `completion` `/rest/db/completion`.
    fn link(
        &self,
        device: &str,
        mut status: impl FnMut(&str) -> Option<Value>,
        mut completion: impl FnMut(&str, &str) -> Option<Value>,
    ) -> PeerLink {
        if self.connected(device) {
            return PeerLink::Connected;
        }
        let mut through = BTreeMap::new();
        let mut up_to_date = true;
        for folder in self.config["folders"].as_array().into_iter().flatten() {
            let members: Vec<&str> = folder["devices"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|member| member["deviceID"].as_str())
                .collect();
            if !members.contains(&device) {
                continue;
            }
            let Some(id) = folder["id"]
                .as_str()
                .filter(|_| folder["type"] == "sendreceive" && folder["paused"] != true)
            else {
                return PeerLink::Unreachable;
            };
            let mut carried = false;
            let mut complete = false;
            for member in members {
                if [self.local, device].contains(&member) || !self.connected(member) {
                    continue;
                }
                let remote = completion(id, member);
                // A member that no longer shares this folder back carries nothing.
                if remote
                    .as_ref()
                    .is_some_and(|r| r["remoteState"].as_str().is_some_and(|s| s != "valid"))
                {
                    continue;
                }
                carried = true;
                complete |=
                    remote.is_some_and(|r| r["completion"].as_f64().is_some_and(|c| c >= 100.0));
                through.entry(member).or_insert_with(|| self.relay(member));
            }
            if !carried {
                return PeerLink::Unreachable;
            }
            up_to_date &= complete && status(id).is_some_and(|s| s["needTotalItems"] == 0);
        }
        if through.is_empty() {
            // Nothing is shared with it, so nothing else can carry its changes.
            return PeerLink::Unreachable;
        }
        PeerLink::Relayed {
            through: through.into_values().collect(),
            up_to_date,
        }
    }
    fn relay(&self, device: &str) -> Relay {
        let name = self.paired.get(device).cloned().or_else(|| {
            self.config["devices"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|d| d["deviceID"] == device)
                .and_then(|d| d["name"].as_str())
                .filter(|name| {
                    *name != super::transfer::PAIRED_DEVICE_NAME && validate_name(name).is_ok()
                })
                .map(str::to_owned)
        });
        Relay {
            device: device.to_owned(),
            name,
        }
    }
}
/// Each offline computer waits CONNECTION_WAIT so a reconnect does not flash a
/// message. One whose vaults nothing else carries gets the firewall hint;
/// otherwise the first offline computer gets a calm line.
fn offline_status<'a>(
    offline: impl IntoIterator<Item = (&'a str, &'a [String], &'a PeerLink, Duration)>,
) -> Option<String> {
    let mut calm = None;
    for (name, addresses, link, elapsed) in offline {
        if elapsed < CONNECTION_WAIT {
            continue;
        }
        match link {
            PeerLink::Relayed { through, .. } => {
                if calm.is_none() {
                    calm = Some(relayed_status(name, through));
                }
            }
            _ => return disconnected_status(name, addresses, elapsed),
        }
    }
    calm
}
fn relayed_status(name: &str, through: &[Relay]) -> String {
    let names: Vec<&str> = through.iter().filter_map(|r| r.name.as_deref()).collect();
    let connected = match (names.as_slice(), through.len()) {
        ([one], 1) => format!("{one} is connected"),
        ([first, second], 2) => format!("{first} and {second} are connected"),
        (_, 1) => "Another computer is connected".into(),
        (_, count) => format!("{count} other computers are connected"),
    };
    format!("{connected}. {name} is offline and will catch up when it reconnects.")
}
fn disconnected_status(name: &str, addresses: &[String], elapsed: Duration) -> Option<String> {
    if elapsed < CONNECTION_WAIT {
        return None;
    }
    let mut ports: Vec<_> = addresses
        .iter()
        .filter_map(|a| endpoint(a))
        .map(|a| a.port())
        .collect();
    ports.sort();
    ports.dedup();
    let action = if ports.is_empty() {
        "Allow Okilum Sync through the firewall on that computer".into()
    } else {
        format!(
            "Open TCP port {} on that computer",
            ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Some(format!("Not connected to {name}. Its firewall may block incoming connections. {action}, or keep both computers on the same network."))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn address_reconciliation_does_not_rehash_the_runtime_package() -> Result<()> {
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir()?;
        let state = root.path().join("prepared");
        let home = state.join("syncthing");
        private::directory(&state)?;
        private::directory(&home)?;
        let config = home.join("config.xml");
        private::write(&config, br#"<configuration><gui tls="false"><address>127.0.0.1:12345</address><apikey>synthetic-key</apikey></gui></configuration>"#)?;
        let certificate = home.join("cert.pem");
        private::write(&certificate, b"synthetic certificate")?;
        // A deliberately different package is a deterministic tripwire for a
        // full managed_identity check, without a timing assertion or a daemon.
        let executable = root.path().join("syncthing");
        private::write(&executable, b"changed package")?;
        let device = ["AAAAAAA"; 8].join("-");
        let mut receipt = json!({
            "request": {"executable": executable, "rest_address": "127.0.0.1:12345",
                "listen_address": "0.0.0.0:23456"},
            "binary_version": crate::daemon::CLIENT_VERSION,
            "binary_hash": crate::linux_bootstrap::BINARY_SHA,
            "identity": {"config_file": config, "device_id": device,
                "certificate_hash": format!("{:x}", Sha256::digest(b"synthetic certificate"))}
        });
        let file = state.join("prepared.json");
        private::write(&file, &serde_json::to_vec(&receipt)?)?;
        let configs = std::slice::from_ref(&config);
        assert_eq!(managed(configs)?.unwrap().config_file, config);
        // Positive control: preparation/enable/source enrollment still verify
        // the package and reject it before attempting any REST connection.
        assert!(crate::daemon::managed_identity(&state)
            .unwrap_err()
            .to_string()
            .contains("selected managed runtime preparation changed"));
        receipt["binary_hash"] = json!("untrusted hash");
        private::write(&file, &serde_json::to_vec(&receipt)?)?;
        assert!(managed(configs).is_err(), "receipt pin was not checked");
        receipt["binary_hash"] = json!(crate::linux_bootstrap::BINARY_SHA);
        private::write(&file, &serde_json::to_vec(&receipt)?)?;
        private::write(&certificate, b"different certificate")?;
        assert!(managed(configs).is_err(), "certificate pin was not checked");
        Ok(())
    }

    #[test]
    fn peer_address_filter_rejects_loopback_link_local_and_handles_ipv6() {
        let invalid = [
            "tcp://127.0.0.1:22000",
            "tcp://[::1]:22000",
            "tcp://[::ffff:127.0.0.1]:22000",
            "tcp://169.254.1.2:22000",
            "tcp://[fe80::1]:22000",
            "tcp://[fe80::1%2]:22000",
            "tcp://[::ffff:169.254.2.1]:22000",
            "tcp://0.0.0.0:22000",
            "tcp://[::]:22000",
            "tcp://224.0.0.1:22000",
            "tcp://255.255.255.255:22000",
            "tcp://[ff02::1]:22000",
            "tcp://192.0.2.1:0",
            "tcp://example.com:22000",
            "https://192.0.2.1:22000",
            "tcp://192.0.2.1:22000/path",
            "dynamic",
        ];
        assert!(filtered(invalid.map(str::to_owned)).is_empty());
        assert_eq!(
            filtered(
                [
                    "tcp://10.10.0.83:22000",
                    "tcp://[fd00::123]:22000",
                    "tcp://10.10.0.83:22000",
                    "quic://[2001:db8::1]:22000"
                ]
                .map(str::to_owned)
            ),
            [
                "quic://[2001:db8::1]:22000",
                "tcp://10.10.0.83:22000",
                "tcp://[fd00::123]:22000"
            ]
        );
        let status = json!({"connectionServiceStatus":{"tcp://0.0.0.0:22000":{
            "lanAddresses":["tcp://127.0.0.1:22000", "tcp://10.10.0.83:22000"],
            "wanAddresses":["tcp://192.0.2.1:12345"]}}});
        assert_eq!(advertised(&status), ["tcp://10.10.0.83:22000"]);
    }
    #[test]
    fn authenticated_hints_cannot_replace_the_approved_device() -> Result<()> {
        let public = wire::public(&wire::random()?)?;
        let device = ["AAAAAAA"; 8].join("-");
        let mut peer = Paired {
            peer: Peer {
                id: Uuid::new_v4(),
                name: "Desktop".into(),
                device_id: wire::fingerprint(&public),
            },
            public,
            key: [0; 32],
            endpoint: "192.0.2.1:12345".parse()?,
            allowed: vec![],
            syncthing_device: Some(device.clone()),
            addresses: vec![],
        };
        remember(
            &mut peer,
            &Transport {
                device: device.clone(),
                addresses: vec![
                    "tcp://10.0.0.1:22000".into(),
                    "tcp://127.0.0.1:22000".into(),
                ],
            },
        )?;
        assert_eq!(peer.addresses, ["tcp://10.0.0.1:22000"]);
        remember(
            &mut peer,
            &Transport {
                device: device.clone(),
                addresses: vec![],
            },
        )?;
        assert_eq!(
            peer.addresses,
            ["tcp://10.0.0.1:22000"],
            "startup erased usable hints"
        );
        remember(
            &mut peer,
            &Transport {
                device: device.clone(),
                addresses: vec!["tcp://10.0.0.2:23000".into()],
            },
        )?;
        assert_eq!(
            peer.addresses,
            ["tcp://10.0.0.2:23000"],
            "new endpoint retained stale hints"
        );
        let before = serde_json::to_value(&peer)?;
        assert!(remember(
            &mut peer,
            &Transport {
                device: ["BBBBBBB"; 8].join("-"),
                addresses: vec![]
            }
        )
        .is_err());
        assert_eq!(serde_json::to_value(&peer)?, before);
        assert!(remember(
            &mut peer,
            &Transport {
                device,
                addresses: vec!["tcp://10.0.0.1:22000".into(); 33]
            }
        )
        .is_err());
        assert_eq!(serde_json::to_value(&peer)?, before);
        Ok(())
    }
    #[test]
    fn not_connected_status_is_bounded_and_actionable() {
        let addresses = vec!["tcp://10.10.0.83:23456".into()];
        assert!(disconnected_status("Desktop", &addresses, Duration::from_secs(29)).is_none());
        let status = disconnected_status("Desktop", &addresses, Duration::from_secs(30)).unwrap();
        assert!(status.starts_with("Not connected to Desktop."));
        assert!(status.contains("firewall") && status.contains("Open TCP port 23456"));
        assert!(status.contains("same network"));
        assert!(disconnected_status("Desktop", &[], Duration::from_secs(31))
            .unwrap()
            .contains("Allow Okilum Sync"));
    }

    /// A receiver's Syncthing view of the "brain" vault it got from idunn; hub
    /// was introduced by idunn (#2622).
    fn mesh(hub_connected: bool) -> (Value, Value) {
        let config = json!({
            "devices": [
                {"deviceID":"RECEIVER","name":"receiver"},
                {"deviceID":"IDUNN","name":"Okilum paired computer"},
                {"deviceID":"HUB","name":"hub","introducedBy":"IDUNN"}
            ],
            "folders": [{"id":"brain","type":"sendreceive","paused":false,"devices":[
                {"deviceID":"RECEIVER"},{"deviceID":"IDUNN"},
                {"deviceID":"HUB","introducedBy":"IDUNN"}
            ]}]
        });
        let connections = json!({"connections":{
            "IDUNN":{"connected":false,"paused":false},
            "HUB":{"connected":hub_connected,"paused":false}
        }});
        (config, connections)
    }
    fn caught_up(_: &str) -> Option<Value> {
        Some(json!({"state":"idle","needTotalItems":0}))
    }
    fn hub_has_all(folder: &str, device: &str) -> Option<Value> {
        assert_eq!((folder, device), ("brain", "HUB"));
        Some(json!({"completion":100,"needItems":0,"remoteState":"valid"}))
    }
    fn paired(names: &[(&str, &str)]) -> BTreeMap<String, String> {
        names
            .iter()
            .map(|(device, name)| (device.to_string(), name.to_string()))
            .collect()
    }
    fn link(config: &Value, connections: &Value, names: &BTreeMap<String, String>) -> PeerLink {
        Observed {
            local: "RECEIVER",
            config,
            connections,
            paired: names,
        }
        .link("IDUNN", caught_up, hub_has_all)
    }
    fn hub(name: Option<&str>) -> Relay {
        Relay {
            device: "HUB".into(),
            name: name.map(Into::into),
        }
    }
    const NO_ADDRESSES: &[String] = &[];

    #[test]
    fn issue2589_offline_peer_with_a_connected_complete_member_is_calm() {
        let (config, connections) = mesh(true);
        let names = paired(&[("IDUNN", "idunn")]);
        let relayed = link(&config, &connections, &names);
        assert_eq!(
            relayed,
            PeerLink::Relayed {
                through: vec![hub(Some("hub"))],
                up_to_date: true
            }
        );
        let addresses = vec!["tcp://10.10.0.83:22000".to_owned()];
        let status =
            offline_status([("idunn", addresses.as_slice(), &relayed, CONNECTION_WAIT)]).unwrap();
        assert_eq!(
            status,
            "hub is connected. idunn is offline and will catch up when it reconnects."
        );
        assert!(!status.contains("firewall") && !status.contains("TCP port"));
        // A paired computer is named as in Okilum, not by its Syncthing name.
        let names = paired(&[("IDUNN", "idunn"), ("HUB", "M4")]);
        let PeerLink::Relayed { through, .. } = link(&config, &connections, &names) else {
            panic!("hub carries the vault");
        };
        assert_eq!(
            relayed_status("idunn", &through),
            "M4 is connected. idunn is offline and will catch up when it reconnects."
        );
        // Still catching up through hub: calm, only not up to date yet.
        let observed = Observed {
            local: "RECEIVER",
            config: &config,
            connections: &connections,
            paired: &names,
        };
        let partial = |_: &str, _: &str| Some(json!({"completion":40.5,"remoteState":"valid"}));
        assert!(matches!(
            observed.link("IDUNN", caught_up, partial),
            PeerLink::Relayed {
                up_to_date: false,
                ..
            }
        ));
        let needs = |_: &str| Some(json!({"state":"syncing","needTotalItems":3}));
        assert!(matches!(
            observed.link("IDUNN", needs, hub_has_all),
            PeerLink::Relayed {
                up_to_date: false,
                ..
            }
        ));
        // Unknown names are counted, never shown as device IDs or placeholders.
        let unnamed = [
            Relay {
                device: "HUB".into(),
                name: None,
            },
            Relay {
                device: "NAS".into(),
                name: None,
            },
        ];
        assert!(
            relayed_status("idunn", &unnamed[..1]).starts_with("Another computer is connected.")
        );
        assert!(relayed_status("idunn", &unnamed).starts_with("2 other computers are connected."));
        let mut placeholder = config.clone();
        placeholder["devices"][2]["name"] = json!(super::super::transfer::PAIRED_DEVICE_NAME);
        assert_eq!(
            link(&placeholder, &connections, &paired(&[])),
            PeerLink::Relayed {
                through: vec![hub(None)],
                up_to_date: true
            }
        );
    }
    #[test]
    fn issue2589_offline_peer_with_no_connected_member_keeps_the_firewall_hint() {
        let (config, connections) = mesh(false);
        let names = paired(&[("IDUNN", "idunn")]);
        let unreachable = link(&config, &connections, &names);
        assert_eq!(unreachable, PeerLink::Unreachable);
        let addresses = vec!["tcp://10.10.0.83:22000".to_owned()];
        let status =
            offline_status([("idunn", addresses.as_slice(), &unreachable, CONNECTION_WAIT)])
                .unwrap();
        assert!(status.starts_with("Not connected to idunn."), "{status}");
        assert!(status.contains("firewall") && status.contains("Open TCP port 22000"));
        // Before the reconnect grace period, neither line flashes.
        assert_eq!(
            offline_status([("idunn", NO_ADDRESSES, &unreachable, Duration::from_secs(29))]),
            None
        );

        let (config, connections) = mesh(true);
        // A connected member that stopped sharing the vault back carries nothing.
        let observed = Observed {
            local: "RECEIVER",
            config: &config,
            connections: &connections,
            paired: &names,
        };
        let not_sharing =
            |_: &str, _: &str| Some(json!({"completion":0,"remoteState":"notSharing"}));
        assert_eq!(
            observed.link("IDUNN", caught_up, not_sharing),
            PeerLink::Unreachable
        );
        let mut paused = connections.clone();
        paused["connections"]["HUB"]["paused"] = json!(true);
        assert_eq!(link(&config, &paused, &names), PeerLink::Unreachable);
        // A vault this computer only receives is not bidirectional yet.
        let mut receive_only = config.clone();
        receive_only["folders"][0]["type"] = json!("receiveonly");
        assert_eq!(
            link(&receive_only, &connections, &names),
            PeerLink::Unreachable
        );
        // A second vault that nobody else shares still needs idunn itself.
        let mut second = config.clone();
        second["folders"].as_array_mut().unwrap().push(json!({
            "id":"notes","type":"sendreceive","paused":false,
            "devices":[{"deviceID":"RECEIVER"},{"deviceID":"IDUNN"}]
        }));
        let observed = Observed {
            local: "RECEIVER",
            config: &second,
            connections: &connections,
            paired: &names,
        };
        let only_brain = |folder: &str, device: &str| {
            assert_eq!(device, "HUB");
            (folder == "brain").then(|| json!({"completion":100,"remoteState":"valid"}))
        };
        assert_eq!(
            observed.link("IDUNN", caught_up, only_brain),
            PeerLink::Unreachable
        );
        // A paired computer that shares no vault has nothing to carry.
        let mut unshared = config.clone();
        unshared["folders"] = json!([]);
        assert_eq!(link(&unshared, &connections, &names), PeerLink::Unreachable);
        // Another offline computer whose vaults sync elsewhere never hides the hint.
        let relayed = link(&config, &connections, &names);
        let status = offline_status([
            ("idunn", NO_ADDRESSES, &relayed, CONNECTION_WAIT),
            (
                "sindri",
                NO_ADDRESSES,
                &PeerLink::Unreachable,
                CONNECTION_WAIT,
            ),
        ])
        .unwrap();
        assert!(status.starts_with("Not connected to sindri."), "{status}");
    }
    #[test]
    fn issue2589_connected_peer_keeps_the_normal_status() -> Result<()> {
        let (config, mut connections) = mesh(false);
        connections["connections"]["IDUNN"]["connected"] = json!(true);
        let names = paired(&[("IDUNN", "idunn")]);
        let no_reads =
            |_: &str| -> Option<Value> { panic!("a connected peer needs no folder reads") };
        let observed = Observed {
            local: "RECEIVER",
            config: &config,
            connections: &connections,
            paired: &names,
        };
        assert_eq!(
            observed.link("IDUNN", no_reads, |_, _| None),
            PeerLink::Connected
        );
        assert_eq!(offline_status([]), None);

        // The controller publishes exactly what refresh observed.
        let state = tempfile::tempdir()?;
        let controller = Controller::open(state.path().join("direct"), vec![])?;
        assert_eq!(controller.snapshot()?.status, "Ready");
        assert!(controller.peer_links()?.is_empty());
        let id = Uuid::new_v4();
        let calm = "hub is connected. idunn is offline and will catch up when it reconnects.";
        {
            let mut watch = controller.shared.connection_watch.lock().unwrap();
            watch.status = Some(calm.into());
            watch.links.insert(id, PeerLink::Connected);
        }
        assert_eq!(controller.snapshot()?.status, calm);
        assert_eq!(controller.peer_links()?[&id], PeerLink::Connected);
        assert_eq!(
            serde_json::to_value(PeerLink::Relayed {
                through: vec![hub(Some("hub"))],
                up_to_date: true
            })?,
            json!({"state":"relayed","through":[{"device":"HUB","name":"hub"}],"up_to_date":true})
        );
        controller.shutdown();
        Ok(())
    }
}
