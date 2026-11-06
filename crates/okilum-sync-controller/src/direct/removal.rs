//! Removal is an authenticated, replayable operation in application state.
//! It never deletes vault files or unrelated Syncthing folders/devices.
use super::*;
use serde_json::json;
use std::collections::BTreeSet;

const HANDOFF_REFUSAL: &str = "This introducer still serves plain Syncthing computers. Their surviving relationships need an explicit handoff before removing this computer; no folder was changed";
#[derive(Debug)]
struct NeedsHandoff;
impl std::fmt::Display for NeedsHandoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(HANDOFF_REFUSAL)
    }
}
impl std::error::Error for NeedsHandoff {}
pub(super) fn public_refusal(error: &anyhow::Error) -> Option<&'static str> {
    error.is::<NeedsHandoff>().then_some(HANDOFF_REFUSAL)
}

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
    #[serde(default)]
    folders: Option<Vec<FolderRemoval>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mesh: Option<Mesh>,
}
/// Final membership from the departing introducer, kept only in private state.
/// It uses the same vault-scoped trust as a Syncthing introduction, and never
/// carries pairing keys, arbitrary device settings or folder configuration.
type Mesh = BTreeMap<String, Vec<MeshPeer>>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MeshPeer {
    device: String,
    addresses: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FolderRemoval {
    vault: ExistingVault,
    before: serde_json::Value,
    after: serde_json::Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    id: Uuid,
    device: String,
    vaults: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mesh: Option<Mesh>,
}
pub(super) fn requests(db: &Database, scope: Uuid) -> Vec<Request> {
    db.removals
        .iter()
        .filter(|r| {
            (r.local_applied || db.local_removed && r.folders.is_some())
                && r.pending.contains_key(&scope)
        })
        .map(|r| {
            let peer = &r.pending[&scope];
            let vaults: BTreeSet<_> = r
                .vaults
                .iter()
                .filter(|v| {
                    peer.allowed.contains(v)
                        || knows(db, scope, v)
                        || db.attached.iter().any(|a| {
                            a.folder_id == **v
                                && peer.syncthing_device.as_ref().is_some_and(|device| {
                                    transfer::removal_needs_handoff(db, a, device)
                                })
                        })
                })
                .cloned()
                .collect();
            Request {
                id: r.id,
                device: r.device.clone(),
                mesh: r.mesh.as_ref().map(|mesh| {
                    mesh.iter()
                        .filter(|(v, _)| vaults.contains(*v))
                        .map(|(v, peers)| (v.clone(), peers.clone()))
                        .collect()
                }),
                vaults,
            }
        })
        .collect()
}
pub(super) fn acknowledge(db: &mut Database, scope: Uuid, id: Uuid) -> Result<()> {
    let departing = db.local_removed;
    let removal = db
        .removals
        .iter_mut()
        .find(|r| {
            r.id == id
                && r.pending.contains_key(&scope)
                && (r.local_applied || departing && r.folders.is_some())
        })
        .context("Unknown removal acknowledgement")?;
    removal.pending.remove(&scope);
    Ok(())
}
pub(super) fn revoked_operation_allowed(db: &Database, scope: Uuid, operation: &Operation) -> bool {
    match operation {
        Operation::RemoveComputer { id, device, .. } => {
            db.removals.iter().any(|r| r.matches(*id, device))
        }
        Operation::PollRemovals {} => db.removals.iter().any(|r| r.pending.contains_key(&scope)),
        Operation::AcknowledgeRemoval { id } => db
            .removals
            .iter()
            .any(|r| r.id == *id && r.pending.contains_key(&scope)),
        _ => false,
    }
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
            // Inventory must not turn a moved/unavailable history binding into
            // a global Sync failure. Mutation paths still require exact binding.
            let Ok(fresh) = inspect_existing(&configs, &vault.path) else {
                continue;
            };
            if !existing::same_binding(&vault, &fresh) {
                continue;
            }
            let Ok(api) = fresh.daemon.connect() else {
                continue;
            };
            let Ok(config) = api.config() else {
                continue;
            };
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
                    .unwrap_or_else(|| {
                        format!("Computer {}…", device.chars().take(7).collect::<String>())
                    });
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
        let pending = self
            .shared
            .db
            .lock()
            .unwrap()
            .removals
            .iter()
            .any(|r| r.device == device && (!r.local_applied || !r.pending.is_empty()));
        if !pending {
            self.queue_removal(Uuid::new_v4(), device, BTreeSet::new(), None)?;
        }
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
        let (attached, known, authorized) = {
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
                requested
                    .iter()
                    .filter(|v| sender.is_some_and(|peer| knows(&db, peer, v)))
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            )
        };
        let configs = self.configuration_paths()?;
        let mut vaults = BTreeSet::new();
        let mut removed_self = false;
        for vault in attached {
            if sender.is_some() && !authorized.contains(&vault.folder_id) {
                continue;
            }
            let fresh = inspect_existing(&configs, &vault.path)?;
            ensure!(
                existing::same_binding(&vault, &fresh),
                "Removal vault identity changed"
            );
            // Syncthing may deliver an introduction removal before this
            // authenticated application operation. Record/ack that shared
            // scope even when its transport member is already absent.
            if fresh.peers.iter().any(|p| p == device) || authorized.contains(&fresh.folder_id) {
                removed_self |= fresh.daemon.device_id == device;
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
                (removed_self || Some(**peer) != sender)
                    && vaults.iter().any(|v| knows(&db, **peer, v))
            })
            .map(|(id, peer)| (*id, peer.clone()))
            .collect();
        db.removals.push(Removal {
            id,
            device: device.to_owned(),
            vaults,
            pending,
            local_applied: false,
            folders: None,
            mesh: None,
        });
        self.save(&db)
    }
    pub(super) fn receive_removal(
        &self,
        scope: Uuid,
        id: Uuid,
        device: String,
        vaults: BTreeSet<String>,
        mesh: Option<Mesh>,
    ) -> Result<Reply> {
        if let Some(mesh) = &mesh {
            let db = self.shared.db.lock().unwrap();
            let peer = db
                .peers
                .get(&scope)
                .or_else(|| db.revoked_peers.get(&scope))
                .context("Unknown departing introducer")?;
            ensure!(
                peer.syncthing_device.as_deref() == Some(device.as_str()),
                "Only the departing introducer can hand off its mesh"
            );
            ensure!(
                !mesh.is_empty() && mesh.len() <= 256,
                "Invalid removal mesh size"
            );
            for (vault, members) in mesh {
                ensure!(
                    vaults.contains(vault)
                        && (knows(&db, scope, vault)
                            || db.removals.iter().any(|r| r.id == id
                                && r.device == device
                                && r.vaults.contains(vault))),
                    "Unrequested mesh handoff"
                );
                let mut seen = BTreeSet::new();
                ensure!(members.len() <= MAX_PEERS, "Excessive removal mesh");
                for member in members {
                    ensure!(
                        valid(&member.device)
                            && member.device != device
                            && seen.insert(&member.device)
                            && !member.addresses.is_empty()
                            && member.addresses.len() <= 32
                            && member
                                .addresses
                                .iter()
                                .all(|a| a.len() <= 4096 && !a.chars().any(char::is_control)),
                        "Invalid removal mesh member"
                    );
                }
            }
        }
        self.queue_removal(id, &device, vaults, Some(scope))?;
        if let Some(mesh) = mesh {
            let mut db = self.shared.db.lock().unwrap();
            let removal = db.removals.iter_mut().find(|r| r.id == id).unwrap();
            ensure!(
                removal.mesh.as_ref().is_none_or(|old| old == &mesh),
                "Removal mesh changed during replay"
            );
            removal.mesh = Some(mesh);
            self.save(&db)?;
        }
        self.prepare_removal(id)?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        if self.apply_removal(id)? {
            Ok(Reply::RemovalRecorded { id })
        } else {
            Ok(Reply::Pending)
        }
    }
    fn prepare_removal(&self, id: Uuid) -> Result<()> {
        let fresh = self
            .shared
            .db
            .lock()
            .unwrap()
            .removals
            .iter()
            .find(|r| r.id == id)
            .is_some_and(|r| !r.local_applied && r.folders.is_none());
        if fresh {
            // Finish existing durable policy transitions before capturing the
            // removal preimages. Replay of a planned removal never re-enters
            // ordinary reconciliation with a stale ownership witness.
            self.reconcile_history()?;
            self.reconcile_create_ignores()?;
        }
        Ok(())
    }
    fn apply_removal(&self, id: Uuid) -> Result<bool> {
        let (mut removal, attached) = {
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
        if removal.local_applied {
            return Ok(true);
        }
        {
            let db = self.shared.db.lock().unwrap();
            ensure!(
                db.revoked_peers.len() + db.peers.len() <= 256,
                "Removal replay credential limit reached; no folder was changed"
            );
        }
        let configs = self.configuration_paths()?;
        if removal.folders.is_none() {
            let mut folders = Vec::new();
            let mut mesh = Mesh::new();
            for vault in attached
                .into_iter()
                .filter(|v| removal.vaults.contains(&v.folder_id))
            {
                let fresh = inspect_existing(&configs, &vault.path)?;
                ensure!(
                    existing::same_binding(&vault, &fresh),
                    "Removal binding changed; no unrelated folder was modified"
                );
                let api = fresh.daemon.connect()?;
                let before = api.folder(&fresh.folder_id)?;
                let needs_handoff = transfer::removal_needs_handoff(
                    &self.shared.db.lock().unwrap(),
                    &fresh,
                    &removal.device,
                );
                if needs_handoff && removal.mesh.is_none() {
                    // Ask the still-serving introducer first. It freezes the
                    // final common-vault mesh before revoking its authority.
                    return Ok(false);
                }
                if fresh.daemon.device_id == removal.device {
                    let peers = capture_mesh(&api.config()?, &before, &removal.device)?;
                    let db = self.shared.db.lock().unwrap();
                    if !peers.iter().all(|member| {
                        db.peers.iter().any(|(scope, peer)| {
                            peer.syncthing_device.as_deref() == Some(member.device.as_str())
                                && knows(&db, *scope, &fresh.folder_id)
                        })
                    }) {
                        return Err(NeedsHandoff.into());
                    }
                    mesh.insert(fresh.folder_id.clone(), peers);
                }
                {
                    let db = self.shared.db.lock().unwrap();
                    source::validate_removal(&db, &fresh, &before)?;
                    transfer::validate_removal(&db, &fresh, &before)?;
                }
                let mut after = before.clone();
                if fresh.daemon.device_id == removal.device {
                    after["paused"] = json!(true);
                } else {
                    after["devices"] = json!(handoff_members(
                        &self.shared.db.lock().unwrap(),
                        &fresh,
                        &removal,
                        &before
                    )?);
                }
                folders.push(FolderRemoval {
                    vault: fresh,
                    before,
                    after,
                });
            }
            // Publish every exact preimage before the first REST mutation. A
            // lost PATCH reply can then finish the witness update after restart.
            let mut db = self.shared.db.lock().unwrap();
            let saved = db.removals.iter_mut().find(|r| r.id == id).unwrap();
            saved.folders = Some(folders.clone());
            if !mesh.is_empty() {
                saved.mesh = Some(mesh.clone());
                removal.mesh = Some(mesh);
            }
            self.save(&db)?;
            removal.folders = Some(folders);
        }
        let removed_self = removal
            .folders
            .as_ref()
            .unwrap()
            .iter()
            .any(|f| f.vault.daemon.device_id == removal.device);
        if removed_self && !removal.pending.is_empty() {
            // Pausing an introducer first would deintroduce every surviving
            // receiver from its siblings. Deny application authority now, but
            // keep its transport announcement until every pending receiver has
            // detached that provenance and acknowledged the departure.
            let mut db = self.shared.db.lock().unwrap();
            deny(&mut db, &removal, true);
            self.save(&db)?;
            return Ok(false);
        }
        for folder in removal.folders.as_ref().unwrap() {
            self.check()?;
            let fresh = inspect_existing(&configs, &folder.vault.path)?;
            ensure!(
                existing::same_binding(&folder.vault, &fresh),
                "Removal vault identity changed"
            );
            let api = fresh.daemon.connect()?;
            let current = api.folder(&fresh.folder_id)?;
            let view = |value: &serde_json::Value| {
                transfer::removal_view(
                    &self.shared.db.lock().unwrap(),
                    &fresh,
                    &removal.device,
                    value,
                )
            };
            let before = view(&folder.before)?;
            let after = view(&folder.after)?;
            let actual = view(&current)?;
            ensure!(
                actual == before || actual == after,
                "Removal folder changed outside its recorded operation; review required"
            );
            if actual != after {
                let changes = if fresh.daemon.device_id == removal.device {
                    json!({"paused":true})
                } else {
                    // Never restore introduced members that Syncthing has
                    // already removed since the intent was recorded.
                    let members = handoff_members(
                        &self.shared.db.lock().unwrap(),
                        &fresh,
                        &removal,
                        &current,
                    )?;
                    if transfer::removal_needs_handoff(
                        &self.shared.db.lock().unwrap(),
                        &fresh,
                        &removal.device,
                    ) {
                        ensure!(
                            super::addresses::owned_runtime(&configs, &fresh.daemon)?,
                            "Mesh handoff requires an Okilum-managed runtime"
                        );
                        let known = api.config()?["devices"]
                            .as_array()
                            .context("Device inventory missing")?
                            .clone();
                        for peer in removal
                            .mesh
                            .as_ref()
                            .and_then(|m| m.get(&fresh.folder_id))
                            .context("Removal mesh missing")?
                        {
                            if peer.device != fresh.daemon.device_id
                                && !known.iter().any(|d| d["deviceID"] == peer.device)
                            {
                                self.check()?;
                                api.add_device(&json!({"deviceID":peer.device,"addresses":peer.addresses,"introducer":false,"autoAcceptFolders":false}))?;
                            }
                        }
                    }
                    json!({"devices":members})
                };
                self.check()?;
                api.patch_folder(&fresh.folder_id, &changes)?;
            }
            let readback = api.folder(&fresh.folder_id)?;
            ensure!(
                view(&readback)? == after,
                "Removal readback changed unrelated configuration"
            );
            let mut db = self.shared.db.lock().unwrap();
            source::record_removed_membership(&mut db, &fresh, &readback);
            transfer::record_removed_membership(&mut db, &fresh, &removal.device, &readback);
            self.save(&db)?;
        }
        let mut db = self.shared.db.lock().unwrap();
        deny(&mut db, &removal, removed_self);
        db.removals
            .iter_mut()
            .find(|r| r.id == id)
            .unwrap()
            .local_applied = true;
        self.save(&db)?;
        Ok(true)
    }
    pub(super) fn reconcile_removals(&self) -> Result<()> {
        self.check()?;
        // Pairing pins the inviter's listener on both ends. Joiners therefore
        // pull pending operations through that authenticated channel; attempting
        // to push to an inviter-owned endpoint would call ourselves.
        let (local, peers) = {
            let db = self.shared.db.lock().unwrap();
            (
                served(&db),
                db.peers
                    .iter()
                    .chain(db.revoked_peers.iter())
                    .map(|(id, p)| (*id, p.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        for (scope, peer) in peers {
            if local.contains(&peer.endpoint) {
                continue;
            }
            let Ok(Reply::Removals { requests }) = self.call(
                peer.endpoint,
                scope,
                &peer.key,
                &peer.public,
                Operation::PollRemovals {},
            ) else {
                continue;
            };
            for request in requests {
                let recorded = matches!(
                    self.receive_removal(scope, request.id, request.device, request.vaults, request.mesh)?,
                    Reply::RemovalRecorded { id } if id == request.id
                );
                // A lost acknowledgement is replayed on the next pull. The
                // receiver records and applies before acknowledging, never after.
                if recorded {
                    let _ = self.call(
                        peer.endpoint,
                        scope,
                        &peer.key,
                        &peer.public,
                        Operation::AcknowledgeRemoval { id: request.id },
                    );
                }
            }
        }
        let removals = self.shared.db.lock().unwrap().removals.clone();
        for removal in removals {
            self.prepare_removal(removal.id)?;
            {
                let _guard = self.shared.transfer_lock.lock().unwrap();
                self.apply_removal(removal.id)?;
            }
            for (id, peer) in removal.pending {
                if local.contains(&peer.endpoint) {
                    continue;
                }
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
                if !removal.local_applied && removal.folders.is_none() {
                    if let Ok(Reply::Error { message }) = &reply {
                        anyhow::bail!("Computer removal needs review: {message}");
                    }
                }
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
fn capture_mesh(
    config: &serde_json::Value,
    folder: &serde_json::Value,
    removed: &str,
) -> Result<Vec<MeshPeer>> {
    let devices = config["devices"]
        .as_array()
        .context("Device inventory missing")?;
    let mut peers = Vec::new();
    for member in folder["devices"]
        .as_array()
        .context("Folder members missing")?
    {
        let device = member["deviceID"]
            .as_str()
            .context("Missing mesh device ID")?;
        if device == removed {
            continue;
        }
        ensure!(
            member["encryptionPassword"]
                .as_str()
                .unwrap_or("")
                .is_empty(),
            "Encrypted member requires an explicit introducer handoff"
        );
        let definition = devices
            .iter()
            .find(|d| d["deviceID"] == device)
            .context("Missing introduced device")?;
        let addresses = definition["addresses"]
            .as_array()
            .context("Mesh addresses missing")?
            .iter()
            .map(|a| {
                a.as_str()
                    .map(str::to_owned)
                    .context("Invalid mesh address")
            })
            .collect::<Result<Vec<_>>>()?;
        peers.push(MeshPeer {
            device: device.into(),
            addresses,
        });
    }
    ensure!(peers.len() <= MAX_PEERS, "Excessive removal mesh");
    Ok(peers)
}
fn handoff_members(
    db: &Database,
    vault: &ExistingVault,
    removal: &Removal,
    folder: &serde_json::Value,
) -> Result<Vec<serde_json::Value>> {
    let mut full = folder.clone();
    if transfer::removal_needs_handoff(db, vault, &removal.device) {
        let mesh = removal
            .mesh
            .as_ref()
            .and_then(|m| m.get(&vault.folder_id))
            .context("Departing introducer mesh missing")?;
        ensure!(
            mesh.iter().any(|p| p.device == vault.daemon.device_id),
            "Mesh handoff omits this receiver"
        );
        let members = full["devices"]
            .as_array_mut()
            .context("Folder members missing")?;
        for peer in mesh {
            if !members.iter().any(|m| m["deviceID"] == peer.device) {
                members.push(json!({"deviceID":peer.device,"introducedBy":removal.device,"encryptionPassword":""}));
            }
        }
    }
    transfer::removal_members(db, vault, &removal.device, &full)
}

fn deny(db: &mut Database, removal: &Removal, removed_self: bool) {
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
    if removed_self {
        for source in &mut db.sources {
            source.active = false;
        }
        for transfer in &mut db.transfers {
            transfer.deactivate();
        }
    }
    db.local_removed |= removed_self;
}

pub(super) fn peer_is_live(db: &Database, peer: &Paired) -> bool {
    !db.local_removed
        && db
            .peers
            .get(&peer.peer.id)
            .is_some_and(|current| current.key == peer.key && current.public == peer.public)
        && !db.removals.iter().any(|removal| {
            peer.syncthing_device.as_deref() == Some(removal.device.as_str())
                || db
                    .attached
                    .iter()
                    .any(|vault| vault.daemon.device_id == removal.device)
        })
}

pub(super) fn pending(db: &Database) -> usize {
    db.removals
        .iter()
        .filter(|r| !r.local_applied || !r.pending.is_empty())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn departing_mesh_contains_only_scoped_members_and_addresses() -> Result<()> {
        let departing = ["AAAAAAA"; 8].join("-");
        let remaining = ["BBBBBBB"; 8].join("-");
        let unrelated = ["CCCCCCC"; 8].join("-");
        let config = json!({"devices":[
            {"deviceID":departing,"addresses":["dynamic"]},
            {"deviceID":remaining,"addresses":["tcp://127.0.0.1:22000","dynamic"],"introducer":true,"autoAcceptFolders":true},
            {"deviceID":unrelated,"addresses":["dynamic"]}
        ]});
        let mut folder = json!({"devices":[{"deviceID":departing},{"deviceID":remaining,"encryptionPassword":""}]});
        let mesh = capture_mesh(&config, &folder, &departing)?;
        assert_eq!(
            mesh,
            vec![MeshPeer {
                device: remaining,
                addresses: vec!["tcp://127.0.0.1:22000".into(), "dynamic".into()]
            }]
        );
        assert!(serde_json::to_value(mesh)?[0].get("introducer").is_none());
        folder["devices"][1]["encryptionPassword"] = json!("encrypted-token");
        assert!(capture_mesh(&config, &folder, &departing).is_err());
        Ok(())
    }
    #[test]
    fn authenticated_peer_without_a_shared_vault_cannot_remove_a_computer() -> Result<()> {
        let root = tempfile::tempdir()?;
        let first = Controller::open(root.path().join("first"), vec![])?;
        let second = Controller::open(root.path().join("second"), vec![])?;
        let url = first
            .begin_pairing_at("First", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
            .offer_url
            .unwrap();
        second.redeem(&url, "Second")?;
        first.approve(first.snapshot()?.pending.unwrap().request_id)?;
        second.refresh_pairing()?;
        let peer = second
            .shared
            .db
            .lock()
            .unwrap()
            .peers
            .values()
            .next()
            .unwrap()
            .clone();
        assert!(
            second.catalog(peer.peer.id)?.is_empty(),
            "authenticated positive channel control"
        );
        let file = first.shared.state.join("direct.json");
        let before = std::fs::read(&file)?;
        let reply = second.call(
            peer.endpoint,
            peer.peer.id,
            &peer.key,
            &peer.public,
            Operation::RemoveComputer {
                id: Uuid::new_v4(),
                device: ["AAAAAAA"; 8].join("-"),
                vaults: BTreeSet::from(["not-authorized".into()]),
            },
        )?;
        assert!(matches!(reply, Reply::Error { .. }));
        assert_eq!(
            std::fs::read(file)?,
            before,
            "unauthorized removal wrote durable state"
        );
        // An authenticated peer cannot present a different computer's final
        // mesh; rejection must precede even creation of a durable removal.
        let spoofed = first.receive_removal(
            peer.peer.id,
            Uuid::new_v4(),
            ["AAAAAAA"; 8].join("-"),
            BTreeSet::from(["not-authorized".into()]),
            Some(BTreeMap::from([("not-authorized".into(), vec![])])),
        );
        assert!(spoofed.is_err());
        assert_eq!(
            std::fs::read(first.shared.state.join("direct.json"))?,
            before
        );
        first.shutdown();
        second.shutdown();
        Ok(())
    }
}
