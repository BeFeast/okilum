//! Removal is an authenticated, replayable operation in application state.
//! It never deletes vault files or unrelated Syncthing folders/devices.
use super::*;
use serde_json::json;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncComputer {
    pub device_id: String,
    pub name: String,
    pub paired: Option<Uuid>,
    pub connected: Option<bool>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Removal {
    id: Uuid,
    device: String,
    vaults: BTreeSet<String>,
    pending: BTreeMap<Uuid, Paired>,
    local_applied: bool,
}
impl Removal {
    pub(super) fn matches(&self, id: Uuid, device: &str) -> bool {
        self.id == id && self.device == device
    }
}
fn valid(device: &str) -> bool {
    device.len() == 63
        && device.split('-').count() == 8
        && device.split('-').all(|p| {
            p.len() == 7
                && p.bytes()
                    .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))
        })
}
fn knows(db: &Database, peer: Uuid, vault: &str) -> bool {
    db.peers
        .get(&peer)
        .is_some_and(|p| p.allowed.iter().any(|v| v == vault))
        || db.transfers.iter().any(|t| t.removal_binding(peer, vault))
}
impl Controller {
    pub fn sync_computers(&self) -> Result<Vec<SyncComputer>> {
        self.check()?;
        let (peers, attached) = {
            let db = self.shared.db.lock().unwrap();
            if db.local_removed {
                return Ok(vec![]);
            }
            (db.peers.clone(), db.attached.clone())
        };
        let configs = self.configuration_paths()?;
        let mut result: BTreeMap<String, SyncComputer> = peers
            .iter()
            .filter_map(|(id, p)| {
                p.syncthing_device.as_ref().map(|device| {
                    (
                        device.clone(),
                        SyncComputer {
                            device_id: device.clone(),
                            name: p.peer.name.clone(),
                            paired: Some(*id),
                            connected: None,
                        },
                    )
                })
            })
            .collect();
        for vault in attached {
            let fresh = inspect_existing(&configs, &vault.path)?;
            ensure!(
                existing::same_binding(&vault, &fresh),
                "Computer inventory vault identity changed"
            );
            let api = fresh.daemon.connect()?;
            let config = api.config()?;
            let connections = api.connections().ok();
            for device in fresh
                .peers
                .iter()
                .filter(|id| **id != fresh.daemon.device_id)
            {
                let name = config["devices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|d| d["deviceID"] == *device)
                    .and_then(|d| d["name"].as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Computer {}…", &device[..device.len().min(7)]));
                let row = result.entry(device.clone()).or_insert(SyncComputer {
                    device_id: device.clone(),
                    name,
                    paired: None,
                    connected: None,
                });
                row.connected = connections
                    .as_ref()
                    .and_then(|c| c["connections"][device]["connected"].as_bool());
            }
        }
        Ok(result.into_values().collect())
    }
    pub fn remove_computer(&self, device: &str) -> Result<Snapshot> {
        self.check()?;
        ensure!(valid(device), "Invalid computer device ID");
        let computers = self.sync_computers()?;
        ensure!(
            computers.iter().any(|c| c.device_id == device),
            "Computer is no longer in these vaults"
        );
        let id = Uuid::new_v4();
        self.queue_removal(id, device, BTreeSet::new(), None)?;
        self.reconcile_removals()?;
        self.refresh_attached()?;
        let mut snapshot = self.snapshot()?;
        snapshot.computers = self.sync_computers()?;
        Ok(snapshot)
    }
    fn queue_removal(
        &self,
        id: Uuid,
        device: &str,
        requested: BTreeSet<String>,
        sender: Option<Uuid>,
    ) -> Result<()> {
        self.check()?;
        ensure!(valid(device) && !id.is_nil(), "Invalid removal identity");
        let (attached, known) = {
            let db = self.shared.db.lock().unwrap();
            if let Some(old) = db.removals.iter().find(|r| r.id == id) {
                ensure!(
                    old.device == device,
                    "Removal identity conflicts with its durable request"
                );
                return Ok(());
            }
            ensure!(
                db.removals.len() < 256,
                "Removal history limit reached; existing records were kept"
            );
            if let Some(sender) = sender {
                ensure!(
                    db.peers.contains_key(&sender)
                        && !requested.is_empty()
                        && requested.iter().any(|v| knows(&db, sender, v)),
                    "Removal sender has no shared vault authority"
                );
            }
            (
                db.attached.clone(),
                db.peers
                    .values()
                    .any(|p| p.syncthing_device.as_deref() == Some(device)),
            )
        };
        let configs = self.configuration_paths()?;
        let mut vaults = BTreeSet::new();
        for vault in attached {
            let fresh = inspect_existing(&configs, &vault.path)?;
            ensure!(
                existing::same_binding(&vault, &fresh),
                "Removal vault identity changed"
            );
            if fresh.peers.iter().any(|p| p == device) {
                vaults.insert(fresh.folder_id);
            }
        }
        ensure!(
            known || !vaults.is_empty(),
            "Removed computer is not in these vaults"
        );
        if sender.is_some() {
            ensure!(
                requested.iter().any(|v| vaults.contains(v)),
                "Removal does not identify a shared computer"
            );
        }
        let mut db = self.shared.db.lock().unwrap();
        let pending = db
            .peers
            .iter()
            .filter(|(peer, _)| {
                Some(**peer) != sender && vaults.iter().any(|v| knows(&db, **peer, v))
            })
            .map(|(id, peer)| (*id, peer.clone()))
            .collect();
        db.removals.push(Removal {
            id,
            device: device.to_owned(),
            vaults,
            pending,
            local_applied: false,
        });
        self.save(&db)
    }
    pub(super) fn receive_removal(
        &self,
        scope: Uuid,
        id: Uuid,
        device: String,
        vaults: BTreeSet<String>,
    ) -> Result<Reply> {
        self.queue_removal(id, &device, vaults, Some(scope))?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        self.apply_removal(id)?;
        Ok(Reply::RemovalRecorded { id })
    }
    fn apply_removal(&self, id: Uuid) -> Result<()> {
        let (removal, attached) = {
            let db = self.shared.db.lock().unwrap();
            (
                db.removals
                    .iter()
                    .find(|r| r.id == id)
                    .context("Removal intent missing")?
                    .clone(),
                db.attached.clone(),
            )
        };
        if removal.local_applied && removal.pending.is_empty() {
            return Ok(());
        }
        let configs = self.configuration_paths()?;
        let mut removed_self = false;
        for vault in attached
            .into_iter()
            .filter(|v| removal.vaults.contains(&v.folder_id))
        {
            self.check()?;
            let fresh = inspect_existing(&configs, &vault.path)?;
            ensure!(
                existing::same_binding(&vault, &fresh) && fresh.daemon.device_id != removal.device,
                "Removal binding changed; no unrelated folder was modified"
            );
            let api = fresh.daemon.connect()?;
            let before = api.folder(&fresh.folder_id)?;
            let members = before["devices"]
                .as_array()
                .context("Removal folder membership missing")?;
            if !members.iter().any(|d| d["deviceID"] == removal.device) {
                continue;
            }
            let is_self = fresh.daemon.device_id == removal.device;
            removed_self |= is_self;
            let after_members: Vec<_> = members
                .iter()
                .filter(|d| is_self || d["deviceID"] != removal.device)
                .cloned()
                .collect();
            self.check()?;
            ensure!(
                api.folder(&fresh.folder_id)? == before,
                "Removal folder changed; retry after review"
            );
            let changes = if is_self {
                json!({"paused":true})
            } else {
                json!({"devices":after_members})
            };
            api.patch_folder(&fresh.folder_id, &changes)?;
            let after = api.folder(&fresh.folder_id)?;
            let mut expected = before;
            if is_self {
                expected["paused"] = json!(true);
            } else {
                expected["devices"] = json!(after_members);
            }
            ensure!(
                after == expected,
                "Removal readback changed unrelated configuration"
            );
            let mut db = self.shared.db.lock().unwrap();
            #[cfg(target_os = "linux")]
            source::record_removed_membership(&mut db, &fresh, &after);
            transfer::record_removed_membership(&mut db, &fresh, &removal.device, &after);
            self.save(&db)?;
        }
        let mut db = self.shared.db.lock().unwrap();
        let revoked: Vec<_> = db
            .peers
            .iter()
            .filter(|(_, p)| {
                removed_self || p.syncthing_device.as_deref() == Some(removal.device.as_str())
            })
            .map(|(id, _)| *id)
            .collect();
        for peer in revoked {
            if let Some(pair) = db.peers.remove(&peer) {
                db.revoked_peers.insert(peer, pair);
            }
            db.share_grants.retain(|g| !g.for_peer(peer));
            if db.catalog_peer == Some(peer) {
                db.catalog.clear();
                db.catalog_peer = None;
            }
        }
        db.local_removed |= removed_self;
        db.removals
            .iter_mut()
            .find(|r| r.id == id)
            .unwrap()
            .local_applied = true;
        self.save(&db)
    }
    pub(super) fn reconcile_removals(&self) -> Result<()> {
        self.check()?;
        let removals = self.shared.db.lock().unwrap().removals.clone();
        for removal in removals {
            {
                let _guard = self.shared.transfer_lock.lock().unwrap();
                self.apply_removal(removal.id)?;
            }
            for (id, peer) in removal.pending {
                let reply = self.call(
                    peer.endpoint,
                    id,
                    &peer.key,
                    &peer.public,
                    Operation::RemoveComputer {
                        id: removal.id,
                        device: removal.device.clone(),
                        vaults: removal.vaults.clone(),
                    },
                );
                if matches!(reply,Ok(Reply::RemovalRecorded { id }) if id == removal.id) {
                    let mut db = self.shared.db.lock().unwrap();
                    db.removals
                        .iter_mut()
                        .find(|r| r.id == removal.id)
                        .unwrap()
                        .pending
                        .remove(&id);
                    self.save(&db)?;
                }
            }
        }
        Ok(())
    }
}
pub(super) fn pending(db: &Database) -> usize {
    db.removals
        .iter()
        .filter(|r| !r.local_applied || !r.pending.is_empty())
        .count()
}
