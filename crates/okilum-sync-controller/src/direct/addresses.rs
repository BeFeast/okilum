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
#[cfg(not(target_os = "linux"))]
pub(super) fn managed(_configs: &[PathBuf]) -> Result<Option<DaemonIdentity>> {
    Ok(None)
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
                #[cfg(target_os = "linux")]
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
        let (endpoint, peers) = {
            let db = self.shared.db.lock().unwrap();
            (db.endpoint, db.peers.values().cloned().collect::<Vec<_>>())
        };
        for peer in peers {
            // Reapply durable hints at startup; an offline computer cannot erase them.
            self.apply_addresses(&peer, &peer.addresses)?;
            if endpoint != Some(peer.endpoint) {
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
                    Err(error) if transfer::offline(&error) => {}
                    Err(error) => return Err(error),
                    _ => anyhow::bail!("authenticated peer catalog unavailable"),
                }
            }
        }
        Ok(())
    }
    pub(super) fn refresh_connection_status(&self) -> Result<()> {
        let Some(identity) = managed(&self.configuration_paths()?)? else {
            return Ok(());
        };
        let api = identity.connect()?;
        let connections = api.connections()?;
        let devices = api.config()?["devices"]
            .as_array()
            .context("invalid device inventory")?
            .clone();
        let db = self.shared.db.lock().unwrap();
        let mut watch = self.shared.connection_watch.lock().unwrap();
        watch.status = None;
        for peer in db.peers.values() {
            let Some(device) = &peer.syncthing_device else {
                continue;
            };
            if !devices.iter().any(|d| d["deviceID"] == *device) {
                continue;
            }
            if connections["connections"][device]["connected"] == true {
                watch.since.remove(&peer.peer.id);
                continue;
            }
            let elapsed = watch
                .since
                .entry(peer.peer.id)
                .or_insert_with(Instant::now)
                .elapsed();
            if let Some(status) = disconnected_status(&peer.peer.name, &peer.addresses, elapsed) {
                if watch.status.is_none() {
                    watch.status = Some(status);
                }
            }
        }
        Ok(())
    }
}
#[derive(Default)]
pub(super) struct ConnectionWatch {
    since: BTreeMap<Uuid, Instant>,
    pub status: Option<String>,
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
}
