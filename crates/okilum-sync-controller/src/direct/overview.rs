//! Read-only UI observations. No changes to pairing, trust or network topology.
use super::*;

#[derive(Clone, Debug, Default)]
pub struct Overview {
    pub computers: BTreeMap<Uuid, ComputerOverview>,
    pub vaults: BTreeMap<PathBuf, VaultOverview>,
}
#[derive(Clone, Debug, Default)]
pub struct ComputerOverview {
    pub can_receive: bool,
    pub shared_vaults: Vec<PathBuf>,
    pub connected: Option<bool>,
    pub last_seen: Option<String>,
    pub vaults: Option<usize>,
}
#[derive(Clone, Debug, Default)]
pub struct VaultOverview {
    pub available: bool,
    pub state: String,
    pub files: u64,
    pub bytes: u64,
    pub needed_bytes: u64,
    pub shared_with: Vec<String>,
}
impl Controller {
    /// Called on the background executor, never from Render.
    pub fn overview(&self) -> Result<Overview> {
        self.check()?;
        let (peers, attached, endpoint) = {
            let db = self.shared.db.lock().unwrap();
            let mut peers = db.peers.values().cloned().collect::<Vec<_>>();
            // External Syncthing does not participate in managed transport
            // exchange, but a completed share reply already names its device.
            for (id, device) in db
                .transfers
                .iter()
                .filter_map(transfer::Transfer::overview_binding)
            {
                if let Some(peer) = peers
                    .iter_mut()
                    .find(|peer| peer.peer.id == id && peer.syncthing_device.is_none())
                {
                    peer.syncthing_device = Some(device);
                }
            }
            (peers, db.attached.clone(), db.endpoint)
        };
        let mut result = Overview::default();
        for peer in &peers {
            result.computers.insert(
                peer.peer.id,
                ComputerOverview {
                    can_receive: Some(peer.endpoint) != endpoint,
                    shared_vaults: attached
                        .iter()
                        .filter(|vault| {
                            peer.syncthing_device
                                .as_ref()
                                .is_some_and(|device| vault.peers.contains(device))
                        })
                        .map(|vault| vault.path.clone())
                        .collect(),
                    ..Default::default()
                },
            );
        }
        for vault in &attached {
            let mut observed = VaultOverview {
                state: if vault.paused {
                    "Paused"
                } else {
                    "Checking sync status"
                }
                .into(),
                ..Default::default()
            };
            // Preserve plain Syncthing peers in the sharing list as well.
            for device in vault
                .peers
                .iter()
                .filter(|device| *device != &vault.daemon.device_id)
            {
                observed.shared_with.push(
                    peers
                        .iter()
                        .find(|peer| peer.syncthing_device.as_ref() == Some(device))
                        .map(|peer| peer.peer.name.clone())
                        .unwrap_or_else(|| {
                            format!("Syncthing {}", device.chars().take(7).collect::<String>())
                        }),
                );
            }
            if let Ok(api) = vault.daemon.connect() {
                if let Ok(status) = api.status(&vault.folder_id) {
                    observed.available = true;
                    observed.files = status["globalFiles"].as_u64().unwrap_or(0);
                    observed.bytes = status["globalBytes"].as_u64().unwrap_or(0);
                    observed.needed_bytes = status["needBytes"].as_u64().unwrap_or(0);
                    if !vault.paused {
                        observed.state = if status["state"] == "error"
                            || status["pullErrors"].as_u64().unwrap_or(0) > 0
                        {
                            "Needs attention"
                        } else if status["state"] == "idle"
                            && status["needTotalItems"].as_u64() == Some(0)
                        {
                            "Up to date"
                        } else {
                            "Syncing"
                        }
                        .into();
                    }
                }
            }
            result.vaults.insert(vault.path.clone(), observed);
        }
        for config in self.configuration_paths()? {
            let inventory = crate::daemon::discover(&[config]);
            for candidate in inventory.candidates {
                let Ok(api) = candidate.identity.connect() else {
                    continue;
                };
                let Ok(connections) = api.connections() else {
                    continue;
                };
                let stats = api.device_stats().unwrap_or_default();
                let Ok(config) = api.config() else { continue };
                for peer in &peers {
                    let Some(device) = &peer.syncthing_device else {
                        continue;
                    };
                    let computer = result.computers.get_mut(&peer.peer.id).unwrap();
                    if let Some(connected) =
                        connections["connections"][device]["connected"].as_bool()
                    {
                        computer.connected = Some(connected);
                    }
                    computer.last_seen = stats[device]["lastSeen"]
                        .as_str()
                        .filter(|seen| !seen.starts_with("1970-") && !seen.starts_with("0001-"))
                        .map(str::to_owned);
                    let count = config["folders"]
                        .as_array()
                        .map(|folders| {
                            folders
                                .iter()
                                .filter(|folder| {
                                    folder["devices"].as_array().is_some_and(|devices| {
                                        devices.iter().any(|d| d["deviceID"] == *device)
                                    })
                                })
                                .count()
                        })
                        .unwrap_or(0);
                    computer.vaults = Some(computer.vaults.unwrap_or(0) + count);
                }
            }
        }
        Ok(result)
    }
}
