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
    /// Any peer carrying this vault, including introduced Syncthing mesh peers.
    pub connected: bool,
    pub state: String,
    pub files: u64,
    pub bytes: u64,
    pub needed_bytes: u64,
    pub needed_items: u64,
    pub errors: bool,
    pub shared_with: Vec<String>,
}
impl VaultOverview {
    fn read_status(&mut self, status: &serde_json::Value) {
        self.files = status["globalFiles"].as_u64().unwrap_or(0);
        self.bytes = status["globalBytes"].as_u64().unwrap_or(0);
        self.needed_bytes = status["needBytes"].as_u64().unwrap_or(0);
        self.needed_items = status["needTotalItems"].as_u64().unwrap_or_else(|| {
            [
                "needFiles",
                "needDirectories",
                "needSymlinks",
                "needDeletes",
            ]
            .iter()
            .map(|key| status[key].as_u64().unwrap_or(0))
            .sum()
        });
        self.errors = status["state"] == "error"
            || status["pullErrors"].as_u64().unwrap_or(0) > 0
            || status["errors"].as_u64().unwrap_or(0) > 0;
    }
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
                    if let Ok(connections) = api.connections() {
                        observed.connected = vault
                            .peers
                            .iter()
                            .filter(|device| *device != &vault.daemon.device_id)
                            .any(|device| connections["connections"][device]["connected"] == true);
                    }
                    observed.read_status(&status);
                    if !vault.paused {
                        observed.state = if observed.errors {
                            "Needs attention"
                        } else if status["state"] == "idle" && observed.needed_items == 0 {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_and_zero_byte_pending_items_are_distinct() {
        let mut vault = VaultOverview::default();
        vault.read_status(&serde_json::json!({"state":"idle", "needTotalItems":0}));
        assert_eq!(vault.needed_items, 0);
        assert!(!vault.errors);
        vault.read_status(&serde_json::json!({"state":"idle", "needTotalItems":1, "needBytes":0}));
        assert_eq!(vault.needed_items, 1);
        assert_eq!(vault.needed_bytes, 0);
        vault.read_status(&serde_json::json!({"state":"idle", "needFiles":0, "needDirectories":1, "needDeletes":2}));
        assert_eq!(
            vault.needed_items, 3,
            "older REST fields retain pending zero-byte work"
        );
    }
    #[test]
    fn status_error_counts_are_visible_and_recovery_clears_them() {
        let mut vault = VaultOverview::default();
        for status in [
            serde_json::json!({"state":"error"}),
            serde_json::json!({"state":"idle", "pullErrors":1}),
            serde_json::json!({"state":"idle", "errors":1}),
        ] {
            vault.read_status(&status);
            assert!(vault.errors);
            vault.read_status(&serde_json::json!({"state":"idle", "needTotalItems":0}));
            assert!(!vault.errors);
        }
    }
}
