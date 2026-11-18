//! Removal is an authenticated, replayable operation in application state.
//! It never deletes vault files or unrelated Syncthing folders/devices.
use super::*;
use serde_json::json;
use std::collections::BTreeSet;

const HANDOFF_REFUSAL: &str = "This introducer still serves plain Syncthing computers. Their surviving relationships need an explicit handoff before removing this computer; no folder was changed";
/// The folder a membership was removed from is gone or changed identity.
#[derive(Debug)]
struct FolderGone;
impl std::fmt::Display for FolderGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("folder gone or changed identity")
    }
}
impl std::error::Error for FolderGone {}
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
    /// The pairing whose approval readmitted the removed computer (#3079).
    /// A readmitted removal is closed: it is kept only as a replay tombstone,
    /// so a late delivery is acknowledged without removing the computer again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    readmitted: Option<Uuid>,
    /// Pairings this computer revoked when it applied its own removal. After
    /// readmission, copies received through them are released (#3079).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    released: BTreeSet<Uuid>,
    /// The admission epoch of the device this removal removes (#3079). Every
    /// readmission raises the device's epoch; a removal from an older epoch
    /// is closed wherever that epoch is known, so a late or replayed delivery,
    /// also after a restart, never takes effect.
    /// Missing in records written before epochs: such a removal counts as
    /// older than any admission known for its device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epoch: Option<Epoch>,
    /// Receivers that acknowledged the open version. Closing the removal
    /// delivers its closed version to them again.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    delivered: BTreeMap<Uuid, Paired>,
    /// What applying this removal took away here, so that a readmission
    /// learned later re-establishes it (#3079).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    undo: Option<Undo>,
}
/// An admission epoch: raised by every readmission. Ordered by counter, then
/// by the readmitting computer's device, so two computers that readmit the
/// same device concurrently never mint the same epoch (#3079).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Epoch {
    counter: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    by: String,
}
/// Effects a removal had here, kept to re-establish them on readmission.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Undo {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pairings: BTreeMap<Uuid, Paired>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grants: Vec<transfer::ShareGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    members: Vec<RemovedMember>,
    /// The removal was closed after taking effect; re-establish on refresh.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pending: bool,
}
/// The removed device's own entry in one folder's membership, as it was.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemovedMember {
    vault: ExistingVault,
    member: serde_json::Value,
}
/// What a removal delivery says about its own state (#3079).
#[derive(Clone, Debug, Default)]
pub(super) struct Version {
    /// The sender has closed this removal.
    pub superseded: bool,
    /// The admission epoch the removal removes; none from older senders.
    pub epoch: Option<Epoch>,
    /// The sender's current admission epoch of the removed device.
    pub admission: Option<Epoch>,
}
/// Removal records allowed in the journal, open ones and readmitted tombstones.
const MAX_REMOVALS: usize = 1024;
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
    /// The sender readmitted the removed computer after recording this removal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    superseded: bool,
    /// The admission epoch this removal removes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epoch: Option<Epoch>,
    /// The sender's current admission epoch of the removed device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admission: Option<Epoch>,
}
pub(super) fn requests(db: &Database, scope: Uuid) -> Vec<Request> {
    db.removals
        .iter()
        .filter(|r| (r.local_applied || departing(r)) && r.pending.contains_key(&scope))
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
                superseded: r.readmitted.is_some(),
                epoch: r.epoch.clone(),
                admission: Some(admitted(db, &r.device)),
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
/// A receiver acknowledges the version it processed (#3079). If the removal
/// was closed after that version was served, the delivery stays pending so
/// the closed version still reaches the receiver.
pub(super) fn acknowledge(
    db: &mut Database,
    scope: Uuid,
    id: Uuid,
    superseded: bool,
) -> Result<()> {
    let removal = db
        .removals
        .iter_mut()
        .find(|r| r.id == id && r.pending.contains_key(&scope) && (r.local_applied || departing(r)))
        .context("Unknown removal acknowledgement")?;
    removal.acknowledged(scope, superseded);
    Ok(())
}
/// This computer's own removal, prepared and not yet finished: a departing
/// introducer serves it to its receivers before pausing its transport.
fn departing(removal: &Removal) -> bool {
    removal.folders.as_ref().is_some_and(|folders| {
        folders
            .iter()
            .any(|folder| folder.vault.daemon.device_id == removal.device)
    })
}
/// The current admission epoch of `device` on this computer (#3079).
pub(super) fn admitted(db: &Database, device: &str) -> Epoch {
    db.admissions.get(device).cloned().unwrap_or_default()
}
/// Whether a removal of `device` from `epoch` is older than what this
/// computer knows. A removal without an epoch (older software or journal) is
/// older than any known admission, and than any closed removal of `device`.
fn stale(db: &Database, device: &str, epoch: Option<&Epoch>, horizon: &Epoch) -> bool {
    match epoch {
        Some(epoch) => epoch < horizon,
        None => {
            *horizon > Epoch::default()
                || db
                    .removals
                    .iter()
                    .any(|r| r.device == device && r.superseded())
        }
    }
}
/// Raise `device`'s admission epoch; every removal of it from an older epoch
/// closes here without effects, those that took effect are re-established on
/// refresh, and the closed versions go to every peer of their vaults.
fn admit(db: &mut Database, device: &str, admission: &Epoch) {
    if *admission <= admitted(db, device) {
        return;
    }
    db.admissions.insert(device.to_owned(), admission.clone());
    let older: Vec<Uuid> = db
        .removals
        .iter()
        .filter(|r| {
            r.device == device
                && !r.superseded()
                && r.epoch.as_ref().is_none_or(|epoch| epoch < admission)
        })
        .map(|r| r.id)
        .collect();
    for removal in db.removals.iter_mut().filter(|r| older.contains(&r.id)) {
        removal.close_learned(Uuid::nil());
    }
    // Every closed removal of the device carries the newer admission onward,
    // also those closed here before, so peers behind this one learn it too.
    let closed: Vec<Uuid> = db
        .removals
        .iter()
        .filter(|r| r.device == device && r.superseded())
        .map(|r| r.id)
        .collect();
    rebroadcast(db, &closed);
}
/// Deliver these closed removals to every peer that shares one of their
/// vaults, the original sender included, so each computer that applied the
/// open version learns of the readmission (#3079).
fn rebroadcast(db: &mut Database, ids: &[Uuid]) {
    for id in ids {
        let Some(vaults) = db
            .removals
            .iter()
            .find(|r| r.id == *id)
            .map(|r| r.vaults.clone())
        else {
            continue;
        };
        let peers: Vec<(Uuid, Paired)> = db
            .peers
            .iter()
            .filter(|(peer, _)| {
                vaults.iter().any(|v| knows(db, **peer, v))
                    || db
                        .transfers
                        .iter()
                        .any(|t| t.peer() == **peer && vaults.contains(t.vault()))
            })
            .map(|(peer, paired)| (*peer, paired.clone()))
            .collect();
        if let Some(removal) = db.removals.iter_mut().find(|r| r.id == *id) {
            for (peer, paired) in peers {
                removal.pending.entry(peer).or_insert(paired);
            }
        }
    }
}
/// An epoch above every admission and removal of `device` known here, minted
/// by this computer.
fn next_epoch(db: &Database, device: &str) -> Result<Epoch> {
    let counter = db
        .removals
        .iter()
        .filter(|r| r.device == device)
        .filter_map(|r| r.epoch.as_ref().map(|e| e.counter))
        .chain([admitted(db, device).counter])
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|counter| *counter <= MAX_EPOCH)
        .context("Removal history of this computer is exhausted; no folder was changed")?;
    Ok(Epoch {
        counter,
        by: own_device(db),
    })
}
/// Admission counters accepted from other computers: a counter that leaves
/// no room to grow could close every later removal here (#3079).
const MAX_EPOCH: u64 = 1_000_000;
impl Epoch {
    fn valid(&self) -> bool {
        self.counter < MAX_EPOCH && (self.by.is_empty() || valid(&self.by))
    }
}
/// This computer's own device: the daemon of its associated vaults.
fn own_device(db: &Database) -> String {
    db.attached
        .first()
        .map(|vault| vault.daemon.device_id.clone())
        .unwrap_or_default()
}
pub(super) fn revoked_operation_allowed(db: &Database, scope: Uuid, operation: &Operation) -> bool {
    match operation {
        Operation::RemoveComputer { id, device, .. } => {
            db.removals.iter().any(|r| r.matches(*id, device))
        }
        Operation::PollRemovals {} => db.removals.iter().any(|r| r.pending.contains_key(&scope)),
        Operation::AcknowledgeRemoval { id, .. } => db
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
    pub(super) fn superseded(&self) -> bool {
        self.readmitted.is_some()
    }
    /// A closed removal keeps its identity for replay deduplication only. Its
    /// closed version goes again to receivers that took the open one.
    fn close(&mut self, pairing: Uuid) {
        self.readmitted = Some(pairing);
        self.local_applied = true;
        self.folders = None;
        self.mesh = None;
        let delivered = std::mem::take(&mut self.delivered);
        self.pending.extend(delivered);
    }
    /// Closed by a readmission learned here: what it took away is
    /// re-established on the next refresh.
    fn close_learned(&mut self, pairing: Uuid) {
        if self.local_applied || self.folders.is_some() {
            if let Some(undo) = &mut self.undo {
                undo.pending = true;
            }
        }
        self.close(pairing);
    }
    /// A receiver acknowledged the version it processed. Only the current
    /// version ends the delivery; an open version is remembered so that a
    /// later close reaches that receiver too.
    fn acknowledged(&mut self, scope: Uuid, superseded: bool) {
        if self.superseded() != superseded {
            return;
        }
        if let Some(peer) = self.pending.remove(&scope) {
            if !superseded {
                self.delivered.insert(scope, peer);
            }
        }
    }
    fn tombstone(id: Uuid, device: &str, pairing: Uuid) -> Self {
        Self {
            id,
            device: device.to_owned(),
            vaults: BTreeSet::new(),
            pending: BTreeMap::new(),
            local_applied: true,
            folders: None,
            mesh: None,
            readmitted: Some(pairing),
            released: BTreeSet::new(),
            epoch: None,
            delivered: BTreeMap::new(),
            undo: None,
        }
    }
}
/// Applied removals of `device` that are still open: the ids a removed
/// computer presents with its next pairing request (#3079).
pub(super) fn applied(db: &Database, device: &str) -> Vec<Uuid> {
    db.removals
        .iter()
        .filter(|r| {
            r.device == device && !r.superseded() && (r.local_applied || r.folders.is_some())
        })
        .map(|r| r.id)
        .take(MAX_REMOVALS)
        .collect()
}
/// The inviter approves `device` again as pairing `pairing` (#3079). Every
/// removal of it recorded so far, and every removal the returning computer
/// says it applied, closes; their later deliveries become no-ops. A removal
/// recorded after this approval is a new decision and applies normally.
pub(super) fn readmit(
    db: &mut Database,
    device: &str,
    pairing: Uuid,
    known: &[Uuid],
) -> Result<()> {
    ensure!(
        !db.removals
            .iter()
            .any(|r| r.device == device && !r.superseded() && !r.local_applied),
        "This computer's removal is still being applied here; approve it again after the removal finishes"
    );
    let new: BTreeSet<Uuid> = known
        .iter()
        .filter(|id| !id.is_nil() && !db.removals.iter().any(|r| r.id == **id))
        .copied()
        .collect();
    ensure!(
        db.removals.len() + new.len() <= MAX_REMOVALS,
        "Removal history limit reached; existing records were kept"
    );
    // Only open removals: a later ordinary invitation between the same two
    // computers readmits nothing, raises no epoch and revokes no pairing.
    let readmitting = !new.is_empty()
        || db
            .removals
            .iter()
            .any(|r| r.device == device && !r.superseded());
    if !readmitting {
        return Ok(());
    }
    // The new admission epoch is above every removal of this device known
    // here, so each of them, delivered late anywhere, is closed (#3079).
    let epoch = next_epoch(db, device)?;
    let mut closed = vec![];
    for removal in db
        .removals
        .iter_mut()
        .filter(|r| r.device == device && !r.superseded())
    {
        // This approval replaces the old pairing and membership with new
        // ones, so nothing is re-established here.
        removal.undo = None;
        removal.close(pairing);
        closed.push(removal.id);
    }
    for id in new {
        db.removals.push(Removal::tombstone(id, device, pairing));
    }
    db.admissions.insert(device.to_owned(), epoch);
    rebroadcast(db, &closed);
    Ok(())
}
/// One removal an approval closed, with the vaults it removed the computer
/// from, sent with that approval (#3079).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Closed {
    id: Uuid,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    vaults: BTreeSet<String>,
}
/// The removals the approval of `pairing` closed, sent with that approval.
pub(super) fn readmitted_by(db: &Database, pairing: Uuid) -> Vec<Closed> {
    db.removals
        .iter()
        .filter(|r| r.readmitted == Some(pairing))
        .map(|r| Closed {
            id: r.id,
            vaults: r.vaults.clone(),
        })
        .collect()
}
/// The returning computer (`own` Syncthing device) received approval
/// `pairing` from `inviter`, which closed `readmitted` (#3079). Close the same
/// removals here, retire the older pairings with that computer, which its
/// removal revoked on the other side, and leave the removed state when no
/// open removal of this computer remains. Whether or not this computer had
/// applied those removals, every earlier pairing through which it received
/// one of their vaults is recorded as released and revoked, so its kept copy
/// can reach the first merge whichever computer invited it back. Pairings
/// carrying only unrelated vaults stay as they are.
pub(super) fn accept_readmission(
    db: &mut Database,
    own: &str,
    pairing: Uuid,
    inviter: &[u8],
    readmitted: &[Closed],
    admission: Option<Epoch>,
) -> Result<()> {
    ensure!(
        admission.as_ref().is_none_or(Epoch::valid),
        "Invalid admission epoch in approval"
    );
    if readmitted.is_empty() {
        // Nothing readmitted by this approval; still learn the epoch.
        if let Some(admission) = &admission {
            admit(db, own, admission);
        }
        return Ok(());
    }
    let new: BTreeSet<Uuid> = readmitted
        .iter()
        .map(|closed| closed.id)
        .filter(|id| !id.is_nil() && !db.removals.iter().any(|r| r.id == *id))
        .collect();
    ensure!(
        db.removals.len() + new.len() <= MAX_REMOVALS,
        "Removal history limit reached; existing records were kept"
    );
    for closed in readmitted {
        match db.removals.iter_mut().find(|r| r.id == closed.id) {
            Some(removal) if removal.device == own => {
                removal.close(pairing);
                removal.vaults.extend(closed.vaults.iter().cloned());
            }
            Some(_) => {}
            None if !closed.id.is_nil() => {
                let mut tombstone = Removal::tombstone(closed.id, own, pairing);
                tombstone.vaults = closed.vaults.clone();
                db.removals.push(tombstone);
            }
            None => {}
        }
    }
    if let Some(admission) = &admission {
        admit(db, own, admission);
    }
    // Frozen now: only pairings that existed before this approval. A journal
    // written before `released` existed computes it here as well.
    let earlier: BTreeSet<Uuid> = db
        .peers
        .keys()
        .chain(db.revoked_peers.keys())
        .copied()
        .filter(|id| *id != pairing)
        .collect();
    let received: Vec<(Uuid, String)> = db
        .transfers
        .iter()
        .map(|t| (t.peer(), t.vault().to_owned()))
        .collect();
    let mut released = BTreeSet::new();
    for removal in db
        .removals
        .iter_mut()
        .filter(|r| r.device == own && r.readmitted == Some(pairing))
    {
        if removal.released.is_empty() {
            removal.released = earlier
                .iter()
                .filter(|peer| {
                    received
                        .iter()
                        .any(|(from, vault)| from == *peer && removal.vaults.contains(vault))
                })
                .copied()
                .collect();
        }
        released.extend(removal.released.iter().copied());
    }
    // Only the memberships those removals covered stop: their transfers are
    // deactivated, and a released pairing is revoked only when it carries no
    // other vault. Unrelated vaults received through it keep syncing.
    let affected: BTreeMap<Uuid, BTreeSet<String>> = db
        .removals
        .iter()
        .filter(|r| r.device == own && r.readmitted == Some(pairing))
        .flat_map(|r| r.released.iter().map(move |peer| (*peer, r.vaults.clone())))
        .fold(BTreeMap::new(), |mut map, (peer, vaults)| {
            map.entry(peer).or_insert_with(BTreeSet::new).extend(vaults);
            map
        });
    for transfer in db.transfers.iter_mut().filter(|t| {
        affected
            .get(&t.peer())
            .is_some_and(|vaults| vaults.contains(t.vault()))
    }) {
        transfer.deactivate();
    }
    let unrelated = |db: &Database, peer: Uuid| {
        db.transfers.iter().any(|t| {
            t.peer() == peer
                && !affected
                    .get(&peer)
                    .is_some_and(|vaults| vaults.contains(t.vault()))
        })
    };
    // The inviter replaced its older pairings with this one; an older pairing
    // stays only while it carries a vault the removals did not cover. Other
    // released pairings keep (or regain, below) their authority: the computers
    // on their other side re-establish theirs when they learn the readmission.
    let older: Vec<_> = db
        .peers
        .iter()
        .chain(db.revoked_peers.iter())
        .filter(|(id, p)| **id != pairing && p.public == inviter && !unrelated(&*db, **id))
        .map(|(id, _)| *id)
        .collect();
    for removal in db
        .removals
        .iter_mut()
        .filter(|r| r.device == own && r.readmitted == Some(pairing))
    {
        if let Some(undo) = &mut removal.undo {
            undo.pairings.retain(|id, _| !older.contains(id));
            undo.pending = true;
        }
    }
    let older: Vec<_> = older
        .into_iter()
        .filter(|id| db.peers.contains_key(id))
        .collect();
    for peer in older {
        if let Some(pair) = db.peers.remove(&peer) {
            db.revoked_peers.insert(peer, pair);
        }
        db.share_grants.retain(|g| !g.for_peer(peer));
        for transfer in db.transfers.iter_mut().filter(|t| t.peer() == peer) {
            transfer.deactivate();
        }
    }
    if !db
        .removals
        .iter()
        .any(|r| r.device == own && !r.superseded() && (r.local_applied || r.folders.is_some()))
    {
        db.local_removed = false;
    }
    Ok(())
}
/// Whether a copy of `vault` received through the revoked pairing `peer` is
/// released after this computer (`own`) was readmitted: its own removal, now
/// closed, revoked that pairing, whichever computer invited it back; or the
/// same computer replaced that pairing by readmitting it.
pub(super) fn released_by_readmission(db: &Database, own: &str, peer: Uuid, vault: &str) -> bool {
    db.removals.iter().any(|r| {
        r.device == own && r.superseded() && r.released.contains(&peer) && r.vaults.contains(vault)
    }) || replaced_by_readmission(db, own, peer)
}
/// Whether `transfer`'s pairing was replaced by a live pairing with the same
/// computer that readmitted this computer (`own`) after its removal.
pub(super) fn replaced_by_readmission(db: &Database, own: &str, peer: Uuid) -> bool {
    let Some(old) = db.revoked_peers.get(&peer) else {
        return false;
    };
    db.peers.iter().any(|(id, current)| {
        current.public == old.public
            && db
                .removals
                .iter()
                .any(|r| r.device == own && r.readmitted == Some(*id))
    })
}
/// Readmission closed this removal while it was being applied (#3079).
fn closed(db: &Database, id: Uuid) -> bool {
    db.removals.iter().any(|r| {
        r.id == id
            && (r.superseded()
                || r.epoch.is_none() && stale(db, &r.device, None, &admitted(db, &r.device)))
    })
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
        let pending = self.shared.db.lock().unwrap().removals.iter().any(|r| {
            r.device == device && !r.superseded() && (!r.local_applied || !r.pending.is_empty())
        });
        if !pending {
            self.queue_removal(Uuid::new_v4(), device, BTreeSet::new(), None, None)?;
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
        epoch: Option<Epoch>,
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
                db.removals.iter().filter(|r| !r.superseded()).count() < 256
                    && db.removals.len() < MAX_REMOVALS,
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
        #[cfg(test)]
        if let Some(hook) = self.shared.removal_hook.lock().unwrap().clone() {
            hook("queue", id);
        }
        let mut db = self.shared.db.lock().unwrap();
        // A closed delivery of the same removal may have arrived during the
        // daemon inspection above (#3079); keep that record, never add another.
        if let Some(old) = db.removals.iter().find(|r| r.id == id) {
            ensure!(
                old.device == device,
                "Removal identity conflicts with its durable request"
            );
            return Ok(());
        }
        let pending = db
            .peers
            .iter()
            .filter(|(peer, _)| {
                (removed_self || Some(**peer) != sender)
                    && vaults.iter().any(|v| knows(&db, **peer, v))
            })
            .map(|(id, peer)| (*id, peer.clone()))
            .collect();
        // A removal decided here takes an epoch above everything known here,
        // so a concurrent readmission elsewhere can never close it (#3079).
        let current = admitted(&db, device);
        let epoch = if sender.is_none() {
            Some(next_epoch(&db, device)?)
        } else {
            epoch
        };
        let older = stale(&db, device, epoch.as_ref(), &current);
        db.removals.push(Removal {
            id,
            device: device.to_owned(),
            vaults,
            pending,
            local_applied: false,
            folders: None,
            mesh: None,
            readmitted: None,
            released: BTreeSet::new(),
            epoch,
            delivered: BTreeMap::new(),
            undo: None,
        });
        if older {
            if let Some(added) = db.removals.last_mut() {
                added.close(Uuid::nil());
            }
        }
        self.save(&db)
    }
    pub(super) fn receive_removal(
        &self,
        scope: Uuid,
        id: Uuid,
        device: String,
        vaults: BTreeSet<String>,
        mesh: Option<Mesh>,
        version: Version,
    ) -> Result<Reply> {
        // The device's epoch on the sender may be newer than here: learn it
        // first. A removal from an older epoch is closed, whether unseen or
        // journalled (also mid-application), and never enters its effects.
        // Waiting for the effect in flight means none starts after this close.
        ensure!(
            version.admission.as_ref().is_none_or(Epoch::valid)
                && version.epoch.as_ref().is_none_or(Epoch::valid),
            "Invalid removal epoch"
        );
        let (learn, stale) = {
            let db = self.shared.db.lock().unwrap();
            let current = admitted(&db, &device);
            let admission = version.admission.clone().unwrap_or_default();
            let horizon = current.clone().max(admission.clone());
            (
                admission > current,
                version.superseded || stale(&db, &device, version.epoch.as_ref(), &horizon),
            )
        };
        if learn || stale {
            let _effects = self.shared.removal_effects.lock().unwrap();
            let mut db = self.shared.db.lock().unwrap();
            ensure!(valid(&device) && !id.is_nil(), "Invalid removal identity");
            if let Some(known) = db.removals.iter().find(|r| r.id == id) {
                ensure!(
                    known.device == device,
                    "Removal identity conflicts with its durable request"
                );
                if known.superseded() && !learn {
                    // Already closed here: a replay changes nothing.
                    return Ok(Reply::RemovalRecorded { id });
                }
            }
            // A pull through a pairing revoked since may still deliver it;
            // a joiner learns its vaults from what it received, not `allowed`.
            let sender = db
                .peers
                .get(&scope)
                .or_else(|| db.revoked_peers.get(&scope));
            ensure!(
                sender.is_some_and(|peer| vaults.iter().any(|v| peer.allowed.contains(v)))
                    || vaults.iter().any(|v| knows(&db, scope, v))
                    || db
                        .transfers
                        .iter()
                        .any(|t| t.peer() == scope && vaults.contains(t.vault())),
                "Removal sender has no shared vault authority"
            );
            // Persist the newer epoch before any effect (#3079).
            if let Some(admission) = &version.admission {
                admit(&mut db, &device, admission);
            }
            if !stale {
                // A current removal: it applies below, under the learned epoch.
                self.save(&db)?;
            } else {
                match db.removals.iter_mut().find(|r| r.id == id) {
                    Some(removal) => {
                        ensure!(
                            removal.device == device,
                            "Removal identity conflicts with its durable request"
                        );
                        if !removal.superseded() {
                            removal.close_learned(scope);
                            rebroadcast(&mut db, &[id]);
                        }
                    }
                    None => {
                        ensure!(
                            db.removals.len() < MAX_REMOVALS,
                            "Removal history limit reached; existing records were kept"
                        );
                        let mut tombstone = Removal::tombstone(id, &device, scope);
                        tombstone.epoch = version.epoch.clone();
                        db.removals.push(tombstone);
                    }
                }
                self.save(&db)?;
                return Ok(Reply::RemovalRecorded { id });
            }
        }
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
        self.queue_removal(id, &device, vaults, Some(scope), version.epoch.clone())?;
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
        if removal.local_applied || removal.superseded() {
            return Ok(true);
        }
        {
            // A journalled removal without an epoch, replayed after an upgrade
            // where its device was readmitted since: close it, apply nothing.
            let mut db = self.shared.db.lock().unwrap();
            if closed(&db, id) {
                if let Some(saved) = db.removals.iter_mut().find(|r| r.id == id) {
                    saved.close(Uuid::nil());
                }
                self.save(&db)?;
                return Ok(true);
            }
        }
        #[cfg(test)]
        if let Some(hook) = self.shared.removal_hook.lock().unwrap().clone() {
            hook("apply", id);
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
            let _effects = self.shared.removal_effects.lock().unwrap();
            let mut db = self.shared.db.lock().unwrap();
            let saved = db.removals.iter_mut().find(|r| r.id == id).unwrap();
            if saved.superseded() {
                return Ok(true);
            }
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
            let _effects = self.shared.removal_effects.lock().unwrap();
            let mut db = self.shared.db.lock().unwrap();
            if closed(&db, id) {
                return Ok(true);
            }
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
            // Held through this folder's mutation, readback and record.
            let _effects = self.shared.removal_effects.lock().unwrap();
            if closed(&self.shared.db.lock().unwrap(), id) {
                return Ok(true);
            }
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
            // Keep the removed member's own entry to re-add it on readmission.
            let removed_entry = folder.before["devices"]
                .as_array()
                .and_then(|members| {
                    members
                        .iter()
                        .find(|m| m["deviceID"] == removal.device.as_str())
                })
                .cloned();
            if let (Some(member), true) = (removed_entry, fresh.daemon.device_id != removal.device)
            {
                if let Some(saved) = db.removals.iter_mut().find(|r| r.id == id) {
                    let undo = saved.undo.get_or_insert_with(Undo::default);
                    if !undo
                        .members
                        .iter()
                        .any(|m| m.vault.folder_id == fresh.folder_id)
                    {
                        undo.members.push(RemovedMember {
                            vault: fresh.clone(),
                            member,
                        });
                    }
                }
            }
            self.save(&db)?;
        }
        let _effects = self.shared.removal_effects.lock().unwrap();
        let mut db = self.shared.db.lock().unwrap();
        if closed(&db, id) {
            return Ok(true);
        }
        deny(&mut db, &removal, removed_self);
        db.removals
            .iter_mut()
            .find(|r| r.id == id)
            .unwrap()
            .local_applied = true;
        self.save(&db)?;
        Ok(true)
    }
    /// Re-establish what removals closed by a readmission learned here took
    /// away: revoked pairings and share grants, then the removed device's own
    /// folder membership entries (#3079). A folder that changed or vanished
    /// since is left as it is.
    pub(super) fn compensate_removals(&self) -> Result<()> {
        self.check()?;
        let pending: Vec<(Uuid, String, Undo)> = self
            .shared
            .db
            .lock()
            .unwrap()
            .removals
            .iter()
            .filter_map(|r| {
                r.undo
                    .as_ref()
                    .filter(|undo| undo.pending)
                    .map(|undo| (r.id, r.device.clone(), undo.clone()))
            })
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        let configs = self.configuration_paths()?;
        for (id, device, undo) in pending {
            let _serial = self.shared.transfer_lock.lock().unwrap();
            {
                let _effects = self.shared.removal_effects.lock().unwrap();
                let mut db = self.shared.db.lock().unwrap();
                // A later removal of the same device that is still open
                // decides now: once applied, the readmission it followed no
                // longer stands here, so nothing is re-established; while it
                // is still being applied, wait, so a later close of it can.
                let newer = db
                    .removals
                    .iter()
                    .filter(|r| r.id != id && r.device == device && !r.superseded())
                    .map(|r| r.local_applied)
                    .reduce(|a, b| a || b);
                match newer {
                    Some(true) => {
                        if let Some(saved) = db.removals.iter_mut().find(|r| r.id == id) {
                            saved.undo = None;
                        }
                        self.save(&db)?;
                        continue;
                    }
                    Some(false) => continue,
                    None => {}
                }
                for (peer, paired) in &undo.pairings {
                    if !db.peers.contains_key(peer) && db.peers.len() < MAX_PEERS {
                        db.revoked_peers.remove(peer);
                        db.peers.insert(*peer, paired.clone());
                    }
                }
                for grant in &undo.grants {
                    if !db
                        .share_grants
                        .iter()
                        .any(|g| g.peer == grant.peer && g.vault == grant.vault)
                    {
                        db.share_grants.push(grant.clone());
                    }
                }
                self.save(&db)?;
            }
            // A folder that is gone or changed identity is dropped; anything
            // else (e.g. the runtime is down) stays pending and is retried.
            let mut retry = vec![];
            for removed in &undo.members {
                match self.restore_member(&configs, &device, removed) {
                    Ok(()) => {}
                    Err(error) if error.is::<FolderGone>() => {}
                    Err(error) => {
                        eprintln!(
                            "okilum-sync: membership of {} not re-established yet: {error:#}",
                            removed.vault.folder_id
                        );
                        retry.push(removed.clone());
                    }
                }
            }
            let mut db = self.shared.db.lock().unwrap();
            if let Some(saved) = db.removals.iter_mut().find(|r| r.id == id) {
                match (&mut saved.undo, retry.is_empty()) {
                    (Some(undo), false) => undo.members = retry,
                    _ => saved.undo = None,
                }
            }
            self.save(&db)?;
        }
        Ok(())
    }
    fn restore_member(
        &self,
        configs: &[PathBuf],
        device: &str,
        removed: &RemovedMember,
    ) -> Result<()> {
        // Reachability first: a runtime that is down is retried later.
        let api = removed.vault.daemon.connect()?;
        let listed = api.config()?["folders"]
            .as_array()
            .context("Folder inventory missing")?
            .iter()
            .any(|f| f["id"] == removed.vault.folder_id.as_str());
        if !listed || !removed.vault.path.exists() {
            return Err(FolderGone.into());
        }
        let fresh = inspect_existing(configs, &removed.vault.path).map_err(|_| FolderGone)?;
        if !existing::same_binding(&removed.vault, &fresh) {
            return Err(FolderGone.into());
        }
        let api = fresh.daemon.connect()?;
        let current = api.folder(&fresh.folder_id)?;
        let members = current["devices"]
            .as_array()
            .context("Folder members missing")?
            .clone();
        if !members.iter().any(|m| m["deviceID"] == device) {
            let _admission =
                crate::admission::configuration(&fresh.daemon, &fresh.folder_id, &fresh.path)?;
            let mut devices = members;
            devices.push(removed.member.clone());
            self.check()?;
            api.patch_folder(&fresh.folder_id, &json!({ "devices": devices }))?;
        }
        let readback = api.folder(&fresh.folder_id)?;
        let mut db = self.shared.db.lock().unwrap();
        source::record_removed_membership(&mut db, &fresh, &readback);
        transfer::record_restored_membership(&mut db, &fresh, device, &readback);
        self.save(&db)
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
                let (id, request_superseded) = (request.id, request.superseded);
                let received = self.receive_removal(
                    scope,
                    request.id,
                    request.device,
                    request.vaults,
                    request.mesh,
                    Version {
                        superseded: request.superseded,
                        epoch: request.epoch,
                        admission: request.admission,
                    },
                );
                let recorded = matches!(
                    received?,
                    Reply::RemovalRecorded { id: recorded } if recorded == id
                );
                // A lost acknowledgement is replayed on the next pull. The
                // receiver records and applies before acknowledging, never after.
                if recorded {
                    let _ = self.call(
                        peer.endpoint,
                        scope,
                        &peer.key,
                        &peer.public,
                        Operation::AcknowledgeRemoval {
                            id,
                            superseded: request_superseded,
                        },
                    );
                }
            }
        }
        let removals = self.shared.db.lock().unwrap().removals.clone();
        #[cfg(test)]
        if let Some(hook) = self.shared.removal_hook.lock().unwrap().clone() {
            hook("push", Uuid::nil());
        }
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
                // Deliver the current version, not the copy (#3079). Its epoch
                // lets a receiver that already knows a newer admission close it
                // instead of applying it, whenever it arrives.
                let Some((superseded, local_applied, prepared, epoch, admission)) = ({
                    let db = self.shared.db.lock().unwrap();
                    db.removals
                        .iter()
                        .find(|r| r.id == removal.id && r.pending.contains_key(&id))
                        .map(|r| {
                            (
                                r.superseded(),
                                r.local_applied,
                                r.folders.is_some(),
                                r.epoch.clone(),
                                Some(admitted(&db, &r.device)),
                            )
                        })
                }) else {
                    continue;
                };
                let reply = self.call(
                    peer.endpoint,
                    id,
                    &peer.key,
                    &peer.public,
                    Operation::RemoveComputer {
                        id: removal.id,
                        device: removal.device.clone(),
                        vaults: removal.vaults.clone(),
                        superseded,
                        epoch,
                        admission,
                    },
                );
                if !local_applied && !prepared && !superseded {
                    if let Ok(Reply::Error { message }) = &reply {
                        anyhow::bail!("Computer removal needs review: {message}");
                    }
                }
                if matches!(reply,Ok(Reply::RemovalRecorded { id }) if id == removal.id) {
                    // Only the delivered version is acknowledged. A close since
                    // keeps the delivery pending for its closed version.
                    let mut db = self.shared.db.lock().unwrap();
                    if let Some(saved) = db.removals.iter_mut().find(|r| r.id == removal.id) {
                        saved.acknowledged(id, superseded);
                        self.save(&db)?;
                    }
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

/// Whether pairing `peer` carries one of `vaults`: a vault received through
/// it, shared to it, or offered in its catalog.
fn carries(db: &Database, peer: Uuid, paired: &Paired, vaults: &BTreeSet<String>) -> bool {
    paired.allowed.iter().any(|v| vaults.contains(v))
        || db
            .transfers
            .iter()
            .any(|t| t.peer() == peer && vaults.contains(t.vault()))
        || db
            .share_grants
            .iter()
            .any(|g| g.for_peer(peer) && vaults.contains(&g.vault))
}
/// Whether pairing `peer` still carries a vault outside `vaults`.
fn carries_other(db: &Database, peer: Uuid, paired: &Paired, vaults: &BTreeSet<String>) -> bool {
    paired.allowed.iter().any(|v| !vaults.contains(v))
        || db
            .transfers
            .iter()
            .any(|t| t.peer() == peer && !vaults.contains(t.vault()))
        || db
            .share_grants
            .iter()
            .any(|g| g.for_peer(peer) && !vaults.contains(&g.vault))
}
fn deny(db: &mut Database, removal: &Removal, removed_self: bool) {
    // A removed computer stops only the memberships its removal covers. A
    // pairing that carries none of those vaults, or also carries another vault,
    // keeps its authority; unrelated vaults keep syncing (#3079).
    let revoked: BTreeSet<_> = db
        .peers
        .iter()
        .filter(|(id, p)| {
            if removed_self {
                carries(&*db, **id, p, &removal.vaults)
                    && !carries_other(&*db, **id, p, &removal.vaults)
            } else {
                p.syncthing_device.as_deref() == Some(removal.device.as_str())
            }
        })
        .map(|(id, _)| *id)
        .collect();
    // Keep what is taken away, to re-establish it if a readmission is learned.
    let pairings: BTreeMap<Uuid, Paired> = revoked
        .iter()
        .filter_map(|peer| db.peers.get(peer).map(|paired| (*peer, paired.clone())))
        .collect();
    let grants: Vec<transfer::ShareGrant> = db
        .share_grants
        .iter()
        .filter(|g| revoked.iter().any(|peer| g.for_peer(*peer)))
        .cloned()
        .collect();
    if let Some(saved) = db.removals.iter_mut().find(|r| r.id == removal.id) {
        if removed_self {
            saved.released.extend(revoked.iter().copied());
        }
        let undo = saved.undo.get_or_insert_with(Undo::default);
        undo.pairings.extend(pairings);
        undo.grants.extend(grants);
    }
    for &peer in &revoked {
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
        for source in db
            .sources
            .iter_mut()
            .filter(|s| removal.vaults.contains(s.vault()))
        {
            source.active = false;
        }
        for transfer in db
            .transfers
            .iter_mut()
            .filter(|t| removal.vaults.contains(t.vault()) || revoked.contains(&t.peer()))
        {
            transfer.deactivate();
        }
    }
    // The computer as a whole is removed only when no pairing is left; with
    // pairings for other vaults it keeps refreshing those (#3079).
    db.local_removed |= removed_self && db.peers.is_empty();
}

pub(super) fn peer_is_live(db: &Database, peer: &Paired) -> bool {
    !db.local_removed
        && db
            .peers
            .get(&peer.peer.id)
            .is_some_and(|current| current.key == peer.key && current.public == peer.public)
        && !db
            .removals
            .iter()
            .filter(|r| !r.superseded())
            .any(|removal| {
                peer.syncthing_device.as_deref() == Some(removal.device.as_str())
                    // This computer's own removal blocks only pairings of the
                    // vaults it covers (#3079).
                    || db
                        .attached
                        .iter()
                        .any(|vault| vault.daemon.device_id == removal.device)
                        && carries(db, peer.peer.id, peer, &removal.vaults)
                        && !carries_other(db, peer.peer.id, peer, &removal.vaults)
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
                superseded: false,
                epoch: None,
                admission: None,
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
            Version::default(),
        );
        assert!(spoofed.is_err());
        // A closed (superseded) delivery records only a tombstone, and only
        // from a sender with authority over a shared vault.
        let tombstone = first.receive_removal(
            peer.peer.id,
            Uuid::new_v4(),
            ["AAAAAAA"; 8].join("-"),
            BTreeSet::from(["not-authorized".into()]),
            None,
            Version {
                superseded: true,
                ..Version::default()
            },
        );
        assert!(tombstone.is_err());
        assert_eq!(
            std::fs::read(first.shared.state.join("direct.json"))?,
            before,
            "an unauthorized closed removal wrote durable state"
        );
        assert_eq!(
            std::fs::read(first.shared.state.join("direct.json"))?,
            before
        );
        first.shutdown();
        second.shutdown();
        Ok(())
    }

    fn removal(id: Uuid, device: &str) -> Removal {
        Removal {
            id,
            device: device.into(),
            vaults: BTreeSet::from(["vault".into()]),
            pending: BTreeMap::new(),
            local_applied: true,
            folders: Some(vec![]),
            mesh: None,
            readmitted: None,
            released: BTreeSet::new(),
            epoch: Some(Epoch::default()),
            delivered: BTreeMap::new(),
            undo: None,
        }
    }
    fn existing_vault(own: &str, vault: &str) -> Result<ExistingVault> {
        Ok(ExistingVault {
            daemon: crate::daemon::DaemonIdentity {
                config_file: "/fixture/config.xml".into(),
                device_id: own.into(),
                certificate_hash: "fixture".into(),
            },
            version: "v2.1.6".into(),
            folder_id: vault.into(),
            path: format!("/fixture/{vault}").into(),
            label: vault.into(),
            paused: false,
            folder_type: "sendreceive".into(),
            peers: vec![],
            root_identity: None,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            stable_root_identity: None,
        })
    }
    /// A short tag (`"AAAA"`) stands for that letter's 63-character device.
    fn epoch(counter: u64, by: &str) -> Epoch {
        let by = match by.chars().next() {
            Some(letter) if by.len() < 63 && by.chars().all(|c| c == letter) => {
                vec![letter.to_string().repeat(7); 8].join("-")
            }
            _ => by.into(),
        };
        Epoch { counter, by }
    }
    fn closing(ids: &[Uuid]) -> Vec<Closed> {
        ids.iter()
            .map(|id| Closed {
                id: *id,
                vaults: BTreeSet::from(["vault".into()]),
            })
            .collect()
    }
    fn pairing(device: &str, public: Vec<u8>) -> Result<Paired> {
        let id = Uuid::new_v4();
        Ok(Paired {
            peer: Peer {
                id,
                name: "Fixture".into(),
                device_id: wire::fingerprint(&public),
            },
            public,
            key: wire::random()?,
            endpoint: "127.0.0.1:9".parse()?,
            allowed: vec!["vault".into()],
            syncthing_device: Some(device.into()),
            addresses: vec![],
        })
    }
    /// #3079: approval closes every open removal of the returning device; the
    /// re-approved pairing is live again and a later removal is a new decision.
    #[test]
    fn issue3079_approval_closes_old_removals_without_hiding_new_ones() -> Result<()> {
        let root = tempfile::tempdir()?;
        let inviter = Controller::open(root.path().join("inviter"), vec![])?;
        let device = ["DDDDDDD"; 8].join("-");
        let other = ["EEEEEEE"; 8].join("-");
        let mut db = inviter.shared.db.lock().unwrap();
        let (old, unrelated, unseen) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        db.removals.push(removal(old, &device));
        db.removals.push(removal(unrelated, &other));
        let returning = pairing(&device, wire::public(&wire::random()?)?)?;
        let id = returning.peer.id;
        db.peers.insert(id, returning.clone());
        assert!(
            !peer_is_live(&db, &returning),
            "negative control: removal blocks the old device"
        );
        let mut open = removal(Uuid::new_v4(), &device);
        open.local_applied = false;
        db.removals.push(open);
        assert!(
            readmit(&mut db, &device, id, &[]).is_err(),
            "an unfinished removal must finish before approval"
        );
        db.removals.pop();
        readmit(&mut db, &device, id, &[unseen])?;
        assert!(
            peer_is_live(&db, &returning),
            "readmitted device gets addresses again"
        );
        let mut closed: Vec<_> = readmitted_by(&db, id).into_iter().map(|c| c.id).collect();
        closed.sort();
        let mut expected = vec![old, unseen];
        expected.sort();
        assert_eq!(closed, expected);
        assert!(db
            .removals
            .iter()
            .all(|r| r.id != unrelated || !r.superseded()));
        // The closed removal goes to every peer of its vault (#3079).
        assert!(db
            .removals
            .iter()
            .find(|r| r.id == old)
            .unwrap()
            .pending
            .contains_key(&id));
        // A later ordinary invitation readmits nothing.
        readmit(&mut db, &device, Uuid::new_v4(), &[])?;
        assert_eq!(readmitted_by(&db, id).len(), 2);
        // A removal recorded after the approval stays open and blocks again.
        db.removals.push(removal(Uuid::new_v4(), &device));
        assert!(!peer_is_live(&db, &returning));
        drop(db);
        inviter.shutdown();
        Ok(())
    }
    /// #3079: the returning computer closes the same removals, retires its
    /// older pairing with that inviter and leaves the removed state; a replay
    /// of a closed removal is acknowledged without applying it.
    #[test]
    fn issue3079_readmitted_computer_leaves_removed_state_and_ignores_replay() -> Result<()> {
        let root = tempfile::tempdir()?;
        let returning = Controller::open(root.path().join("returning"), vec![])?;
        let own = ["DDDDDDD"; 8].join("-");
        let inviter = wire::public(&wire::random()?)?;
        let old = pairing(&["AAAAAAA"; 8].join("-"), inviter.clone())?;
        let new = pairing(&["AAAAAAA"; 8].join("-"), inviter.clone())?;
        let unrelated = pairing(&["BBBBBBB"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let (seen, unseen) = (Uuid::new_v4(), Uuid::new_v4());
        {
            let mut db = returning.shared.db.lock().unwrap();
            db.removals.push(removal(seen, &own));
            db.local_removed = true;
            db.peers.insert(old.peer.id, old.clone());
            db.peers.insert(unrelated.peer.id, unrelated.clone());
            assert_eq!(applied(&db, &own), vec![seen]);
            db.peers.insert(new.peer.id, new.clone());
            // An approval that readmits nothing changes nothing (#2589 pairing).
            accept_readmission(&mut db, &own, new.peer.id, &inviter, &[], None)?;
            assert!(db.local_removed && db.peers.contains_key(&old.peer.id));
            accept_readmission(
                &mut db,
                &own,
                new.peer.id,
                &inviter,
                &closing(&[seen, unseen]),
                None,
            )?;
            assert!(!db.local_removed, "readmitted computer still removed");
            assert!(
                !db.peers.contains_key(&old.peer.id),
                "older pairing stays live"
            );
            assert!(db.revoked_peers.contains_key(&old.peer.id));
            assert!(db.peers.contains_key(&unrelated.peer.id));
            assert!(db.peers.contains_key(&new.peer.id));
            assert!(replaced_by_readmission(&db, &own, old.peer.id));
            assert!(!replaced_by_readmission(&db, &own, unrelated.peer.id));
            assert!(applied(&db, &own).is_empty());
        }
        // A stale delivery of either closed removal applies nothing.
        for id in [seen, unseen] {
            let reply = returning.receive_removal(
                old.peer.id,
                id,
                own.clone(),
                BTreeSet::from(["vault".into()]),
                None,
                Version {
                    superseded: false,
                    ..Version::default()
                },
            )?;
            assert!(matches!(reply, Reply::RemovalRecorded { id: recorded } if recorded == id));
        }
        let db = returning.shared.db.lock().unwrap();
        assert!(!db.local_removed && db.peers.contains_key(&new.peer.id));
        drop(db);
        returning.shutdown();
        Ok(())
    }

    /// #3079 review: a third computer journalled a removal but had not applied
    /// it when the removed computer was readmitted elsewhere. After a restart
    /// the delayed delivery arrives closed: the journalled record closes,
    /// nothing is applied, and the readmitted computer's pairing stays live.
    #[test]
    fn issue3079_journalled_unapplied_removal_closes_on_delayed_closed_delivery() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("third");
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        {
            let third = Controller::open(state.clone(), vec![])?;
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.peers.insert(returning.peer.id, returning.clone());
            let mut open = removal(id, &removed);
            open.local_applied = false;
            open.folders = None;
            open.pending.insert(returning.peer.id, returning.clone());
            db.removals.push(open);
            third.save(&db)?;
            drop(db);
            third.shutdown();
        }
        let third = Controller::open(state, vec![])?;
        assert_eq!(
            third.snapshot()?.pending_removals,
            1,
            "the journalled removal survived restart"
        );
        let reply = third.receive_removal(
            sender.peer.id,
            id,
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version {
                superseded: true,
                ..Version::default()
            },
        )?;
        assert!(matches!(reply, Reply::RemovalRecorded { id: recorded } if recorded == id));
        let db = third.shared.db.lock().unwrap();
        let record = db.removals.iter().find(|r| r.id == id).unwrap();
        assert!(record.superseded() && record.local_applied && record.folders.is_none());
        assert!(
            db.peers.contains_key(&returning.peer.id) && peer_is_live(&db, &returning),
            "a closed removal revoked the readmitted computer"
        );
        // It is passed on closed, so its own pending receivers close it too.
        let onward = requests(&db, returning.peer.id);
        assert_eq!(onward.len(), 1);
        assert!(onward[0].superseded);
        drop(db);
        // A closed delivery naming another device conflicts and changes nothing.
        assert!(third
            .receive_removal(
                sender.peer.id,
                id,
                ["CCCCCCC"; 8].join("-"),
                BTreeSet::from(["vault".into()]),
                None,
                Version {
                    superseded: true,
                    ..Version::default()
                },
            )
            .is_err());
        third.shutdown();
        Ok(())
    }
    /// #3079 review: after readmission through any computer, only the pairings
    /// this computer's own removal revoked release their kept copies.
    #[test]
    fn issue3079_only_pairings_the_own_removal_revoked_are_released() -> Result<()> {
        let root = tempfile::tempdir()?;
        let returning = Controller::open(root.path().join("returning"), vec![])?;
        let own = ["DDDDDDD"; 8].join("-");
        let original = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let other = pairing(&["CCCCCCC"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        let mut db = returning.shared.db.lock().unwrap();
        db.peers.insert(original.peer.id, original.clone());
        let mut own_removal = removal(id, &own);
        own_removal.local_applied = false;
        own_removal.folders = None;
        db.removals.push(own_removal.clone());
        deny(&mut db, &own_removal, true);
        assert!(db.local_removed && db.revoked_peers.contains_key(&original.peer.id));
        assert!(
            !released_by_readmission(&db, &own, original.peer.id, "vault"),
            "a removed computer releases nothing before readmission"
        );
        // Readmitted by a different computer than the one it received from.
        db.peers.insert(other.peer.id, other.clone());
        accept_readmission(
            &mut db,
            &own,
            other.peer.id,
            &other.public,
            &closing(&[id]),
            None,
        )?;
        assert!(!db.local_removed);
        assert!(released_by_readmission(
            &db,
            &own,
            original.peer.id,
            "vault"
        ));
        assert!(!released_by_readmission(
            &db,
            &own,
            original.peer.id,
            "unrelated"
        ));
        assert!(db.peers.contains_key(&other.peer.id));
        // A pairing revoked later, for another reason, is never released.
        let later = pairing(&["EEEEEEE"; 8].join("-"), wire::public(&wire::random()?)?)?;
        db.revoked_peers.insert(later.peer.id, later.clone());
        assert!(!released_by_readmission(&db, &own, later.peer.id, "vault"));
        drop(db);
        returning.shutdown();
        Ok(())
    }

    fn received(peer: Uuid, vault: &str, own: &str, active: bool) -> Result<transfer::Transfer> {
        Ok(serde_json::from_value(json!({
            "peer": peer,
            "vault": vault,
            "path": format!("/fixture/{vault}"),
            "daemon": {"config_file": "/fixture/config.xml", "device_id": own, "certificate_hash": "fixture"},
            "connection": null,
            "active": active,
            "owner": Uuid::new_v4(),
            "phase": "bidirectional",
        }))?)
    }
    /// #3079 review: readmission through another computer releases the
    /// original membership even when this computer never applied its removal,
    /// and for a journal written before `released` existed. Pairings that
    /// carry only unrelated vaults are kept.
    #[test]
    fn issue3079_unapplied_or_pre_upgrade_removal_releases_the_original_membership() -> Result<()> {
        let root = tempfile::tempdir()?;
        let own = ["DDDDDDD"; 8].join("-");
        let inviter = pairing(&["CCCCCCC"; 8].join("-"), wire::public(&wire::random()?)?)?;
        for applied_before_upgrade in [false, true] {
            let returning = Controller::open(
                root.path()
                    .join(format!("returning-{applied_before_upgrade}")),
                vec![],
            )?;
            let original = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
            let unrelated = pairing(&["EEEEEEE"; 8].join("-"), wire::public(&wire::random()?)?)?;
            let id = Uuid::new_v4();
            let mut db = returning.shared.db.lock().unwrap();
            db.peers.insert(unrelated.peer.id, unrelated.clone());
            db.transfers
                .push(received(unrelated.peer.id, "other", &own, true)?);
            if applied_before_upgrade {
                // As main wrote it: applied, every pairing revoked, no `released`.
                db.revoked_peers.insert(original.peer.id, original.clone());
                db.transfers
                    .push(received(original.peer.id, "vault", &own, false)?);
                db.removals.push(removal(id, &own));
                db.local_removed = true;
            } else {
                // Offline during the removal: it never arrived here.
                db.peers.insert(original.peer.id, original.clone());
                db.transfers
                    .push(received(original.peer.id, "vault", &own, true)?);
            }
            db.peers.insert(inviter.peer.id, inviter.clone());
            accept_readmission(
                &mut db,
                &own,
                inviter.peer.id,
                &inviter.public,
                &closing(&[id]),
                None,
            )?;
            assert!(!db.local_removed);
            if applied_before_upgrade {
                assert!(db.revoked_peers.contains_key(&original.peer.id));
            } else {
                // Never revoked here: its other side re-establishes its own.
                assert!(db.peers.contains_key(&original.peer.id));
            }
            assert!(
                released_by_readmission(&db, &own, original.peer.id, "vault"),
                "original membership not released (applied before upgrade: {applied_before_upgrade})"
            );
            assert!(db
                .transfers
                .iter()
                .filter(|t| t.peer() == original.peer.id)
                .all(|t| !t.removal_binding(original.peer.id, "vault")));
            assert!(
                db.peers.contains_key(&unrelated.peer.id)
                    && db
                        .transfers
                        .iter()
                        .any(|t| t.removal_binding(unrelated.peer.id, "other")),
                "an unrelated membership was revoked"
            );
            assert!(!released_by_readmission(
                &db,
                &own,
                unrelated.peer.id,
                "other"
            ));
            assert!(db.peers.contains_key(&inviter.peer.id));
            drop(db);
            returning.shutdown();
        }
        Ok(())
    }
    /// #3079 review: a close delivered while application is suspended after
    /// its initial snapshot stops every later effect; the readmitted
    /// computer's pairing authority survives.
    #[test]
    fn issue3079_close_during_suspended_application_stops_its_effects() -> Result<()> {
        use std::sync::mpsc;
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        {
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.peers.insert(returning.peer.id, returning.clone());
            let mut open = removal(id, &removed);
            open.local_applied = false;
            open.folders = None;
            db.removals.push(open);
            third.save(&db)?;
        }
        let (suspended, at_barrier) = mpsc::channel();
        let (resume, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        *third.shared.removal_hook.lock().unwrap() = Some(Arc::new(move |stage, _| {
            if stage == "apply" {
                suspended.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
            }
        }));
        let applying = {
            let third = third.clone();
            std::thread::spawn(move || {
                let _serial = third.shared.transfer_lock.lock().unwrap();
                third.apply_removal(id)
            })
        };
        at_barrier.recv_timeout(Duration::from_secs(10))?;
        let reply = third.receive_removal(
            sender.peer.id,
            id,
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version {
                superseded: true,
                ..Version::default()
            },
        )?;
        assert!(matches!(reply, Reply::RemovalRecorded { id: recorded } if recorded == id));
        resume.send(())?;
        assert!(applying.join().unwrap()?, "application must end as closed");
        let db = third.shared.db.lock().unwrap();
        let record = db.removals.iter().find(|r| r.id == id).unwrap();
        assert!(record.superseded() && record.folders.is_none());
        assert!(
            db.peers.contains_key(&returning.peer.id) && peer_is_live(&db, &returning),
            "the suspended application revoked the readmitted computer"
        );
        drop(db);
        *third.shared.removal_hook.lock().unwrap() = None;
        third.shutdown();
        Ok(())
    }

    /// Terra review: a removal closed while a push round runs is delivered in
    /// its closed version, never as the stale open copy, and the delivery is
    /// acknowledged only for the version actually sent.
    #[test]
    fn issue3079_push_round_delivers_the_version_closed_after_its_snapshot() -> Result<()> {
        use std::sync::mpsc;
        let root = tempfile::tempdir()?;
        let joiner = Controller::open(root.path().join("joiner"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let seed = wire::random()?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let mut receiver = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&seed)?)?;
        receiver.endpoint = listener.local_addr()?;
        let id = Uuid::new_v4();
        {
            let mut db = joiner.shared.db.lock().unwrap();
            let mut open = removal(id, &removed);
            open.pending.insert(receiver.peer.id, receiver.clone());
            db.removals.push(open);
            joiner.save(&db)?;
        }
        let key = receiver.key;
        let scope = receiver.peer.id;
        let server = std::thread::spawn(move || -> Result<bool> {
            let (mut stream, _) = listener.accept()?;
            wire::limits(&stream)?;
            let (message, _): (Message, _) = wire::open(wire::read(&mut stream)?, &key, None)?;
            let Operation::RemoveComputer { superseded, .. } = message.operation else {
                anyhow::bail!("expected a removal delivery");
            };
            let response = wire::seal(
                scope,
                &key,
                &seed,
                &Response {
                    nonce: message.nonce,
                    result: Reply::RemovalRecorded { id },
                    transport: None,
                },
            )?;
            wire::write(&mut stream, &response)?;
            Ok(superseded)
        });
        let (suspended, at_barrier) = mpsc::channel();
        let (resume, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        *joiner.shared.removal_hook.lock().unwrap() = Some(Arc::new(move |stage, _| {
            if stage == "push" {
                suspended.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
            }
        }));
        let pushing = {
            let joiner = joiner.clone();
            std::thread::spawn(move || joiner.reconcile_removals())
        };
        at_barrier.recv_timeout(Duration::from_secs(10))?;
        {
            // What approve() does on readmission, after the round's snapshot.
            let _effects = joiner.shared.removal_effects.lock().unwrap();
            let mut db = joiner.shared.db.lock().unwrap();
            readmit(&mut db, &removed, Uuid::new_v4(), &[])?;
            joiner.save(&db)?;
        }
        resume.send(())?;
        pushing.join().unwrap()?;
        assert!(
            server.join().unwrap()?,
            "the stale open copy was delivered after the removal was closed"
        );
        let db = joiner.shared.db.lock().unwrap();
        let record = db.removals.iter().find(|r| r.id == id).unwrap();
        assert!(record.superseded() && record.pending.is_empty());
        drop(db);
        *joiner.shared.removal_hook.lock().unwrap() = None;
        joiner.shutdown();
        Ok(())
    }
    /// Terra review: a pull acknowledgement of a version served before the
    /// close keeps the delivery pending for the closed version.
    #[test]
    fn issue3079_acknowledgement_of_the_open_version_keeps_the_closed_delivery_pending(
    ) -> Result<()> {
        let root = tempfile::tempdir()?;
        let inviter = Controller::open(root.path().join("inviter"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let receiver = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        let mut db = inviter.shared.db.lock().unwrap();
        let mut open = removal(id, &removed);
        open.pending.insert(receiver.peer.id, receiver.clone());
        db.removals.push(open);
        assert!(
            !requests(&db, receiver.peer.id)[0].superseded,
            "served open"
        );
        readmit(&mut db, &removed, Uuid::new_v4(), &[])?;
        acknowledge(&mut db, receiver.peer.id, id, false)?;
        let onward = requests(&db, receiver.peer.id);
        assert_eq!(
            onward.len(),
            1,
            "the closed version must still be delivered"
        );
        assert!(onward[0].superseded);
        acknowledge(&mut db, receiver.peer.id, id, true)?;
        assert!(requests(&db, receiver.peer.id).is_empty());
        drop(db);
        inviter.shutdown();
        Ok(())
    }
    /// Terra review: a closed delivery arriving while `queue_removal()` is in
    /// its daemon inspection leaves exactly one, closed record for that ID.
    #[test]
    fn issue3079_closed_delivery_during_queueing_keeps_one_closed_record() -> Result<()> {
        use std::sync::mpsc;
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        {
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.peers.insert(returning.peer.id, returning.clone());
        }
        let id = Uuid::new_v4();
        let (suspended, at_barrier) = mpsc::channel();
        let (resume, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        *third.shared.removal_hook.lock().unwrap() = Some(Arc::new(move |stage, _| {
            if stage == "queue" {
                suspended.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
            }
        }));
        let queueing = {
            let third = third.clone();
            let removed = removed.clone();
            std::thread::spawn(move || {
                third.queue_removal(id, &removed, BTreeSet::new(), None, None)
            })
        };
        at_barrier.recv_timeout(Duration::from_secs(10))?;
        let reply = third.receive_removal(
            sender.peer.id,
            id,
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version {
                superseded: true,
                ..Version::default()
            },
        )?;
        assert!(matches!(reply, Reply::RemovalRecorded { id: recorded } if recorded == id));
        resume.send(())?;
        queueing.join().unwrap()?;
        let db = third.shared.db.lock().unwrap();
        let records: Vec<_> = db.removals.iter().filter(|r| r.id == id).collect();
        assert_eq!(records.len(), 1, "two records for one removal ID");
        assert!(records[0].superseded());
        assert!(db.peers.contains_key(&returning.peer.id));
        drop(db);
        *third.shared.removal_hook.lock().unwrap() = None;
        third.shutdown();
        Ok(())
    }
    /// Terra review: readmission through another computer stops only the
    /// memberships the closed removal covered. A second vault received
    /// through the same pairing keeps its transfer and the pairing stays.
    #[test]
    fn issue3079_readmission_keeps_unrelated_vaults_of_the_same_pairing() -> Result<()> {
        let root = tempfile::tempdir()?;
        let returning = Controller::open(root.path().join("returning"), vec![])?;
        let own = ["DDDDDDD"; 8].join("-");
        let original = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let inviter = pairing(&["CCCCCCC"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        let mut db = returning.shared.db.lock().unwrap();
        db.peers.insert(original.peer.id, original.clone());
        db.transfers
            .push(received(original.peer.id, "vault", &own, true)?);
        db.transfers
            .push(received(original.peer.id, "second", &own, true)?);
        db.peers.insert(inviter.peer.id, inviter.clone());
        accept_readmission(
            &mut db,
            &own,
            inviter.peer.id,
            &inviter.public,
            &closing(&[id]),
            None,
        )?;
        assert!(!db
            .transfers
            .iter()
            .any(|t| t.removal_binding(original.peer.id, "vault")));
        assert!(
            db.transfers
                .iter()
                .any(|t| t.removal_binding(original.peer.id, "second")),
            "an unrelated vault of the same pairing stopped"
        );
        assert!(
            db.peers.contains_key(&original.peer.id),
            "a pairing that still carries another vault was revoked"
        );
        assert!(released_by_readmission(
            &db,
            &own,
            original.peer.id,
            "vault"
        ));
        assert!(!released_by_readmission(
            &db,
            &own,
            original.peer.id,
            "second"
        ));
        drop(db);
        returning.shutdown();
        Ok(())
    }

    /// Terra re-review: a computer that applied its own removal keeps the
    /// pairings and transfers of vaults that removal did not cover, and they
    /// are still live after readmission through another computer.
    #[test]
    fn issue3079_applied_own_removal_keeps_unrelated_memberships() -> Result<()> {
        let root = tempfile::tempdir()?;
        let returning = Controller::open(root.path().join("returning"), vec![])?;
        let own = ["DDDDDDD"; 8].join("-");
        let original = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let mut unrelated = pairing(&["EEEEEEE"; 8].join("-"), wire::public(&wire::random()?)?)?;
        unrelated.allowed = vec![];
        let inviter = pairing(&["CCCCCCC"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        let mut db = returning.shared.db.lock().unwrap();
        db.peers.insert(original.peer.id, original.clone());
        db.peers.insert(unrelated.peer.id, unrelated.clone());
        db.transfers
            .push(received(original.peer.id, "vault", &own, true)?);
        db.transfers
            .push(received(unrelated.peer.id, "other", &own, true)?);
        let mut own_removal = removal(id, &own);
        own_removal.local_applied = false;
        own_removal.folders = None;
        db.removals.push(own_removal.clone());
        // The removal is applied here first, as on the returning computer.
        deny(&mut db, &own_removal, true);
        assert!(db.revoked_peers.contains_key(&original.peer.id));
        assert!(
            !db.local_removed && peer_is_live(&db, &unrelated),
            "the computer as a whole stopped although another vault remains"
        );
        assert!(
            db.peers.contains_key(&unrelated.peer.id)
                && db
                    .transfers
                    .iter()
                    .any(|t| t.removal_binding(unrelated.peer.id, "other")),
            "applying the removal stopped an unrelated vault"
        );
        db.peers.insert(inviter.peer.id, inviter.clone());
        accept_readmission(
            &mut db,
            &own,
            inviter.peer.id,
            &inviter.public,
            &closing(&[id]),
            None,
        )?;
        assert!(!db.local_removed);
        assert!(released_by_readmission(
            &db,
            &own,
            original.peer.id,
            "vault"
        ));
        assert!(db.peers.contains_key(&unrelated.peer.id));
        assert!(db
            .transfers
            .iter()
            .any(|t| t.removal_binding(unrelated.peer.id, "other")));
        drop(db);
        returning.shutdown();
        Ok(())
    }
    /// Terra re-review: the history limit counts only IDs that are new.
    #[test]
    fn issue3079_known_removal_ids_do_not_count_against_the_limit() -> Result<()> {
        let root = tempfile::tempdir()?;
        let controller = Controller::open(root.path().join("limit"), vec![])?;
        let device = ["DDDDDDD"; 8].join("-");
        let mut db = controller.shared.db.lock().unwrap();
        let ids: Vec<Uuid> = (0..1000).map(|_| Uuid::new_v4()).collect();
        for id in &ids {
            db.removals.push(removal(*id, &device));
        }
        let known: Vec<Uuid> = ids.iter().take(600).copied().collect();
        readmit(&mut db, &device, Uuid::new_v4(), &known)?;
        assert_eq!(db.removals.len(), 1000);
        let pairing_id = Uuid::new_v4();
        accept_readmission(&mut db, &device, pairing_id, &[], &closing(&known), None)?;
        assert_eq!(db.removals.len(), 1000);
        let fresh: Vec<Uuid> = (0..25).map(|_| Uuid::new_v4()).collect();
        assert!(
            readmit(&mut db, &device, Uuid::new_v4(), &fresh).is_err(),
            "positive control: 25 new IDs exceed the limit"
        );
        drop(db);
        controller.shutdown();
        Ok(())
    }

    /// Terra round 4: the admission epoch orders removals without timing. A
    /// receiver that learned the newer epoch closes every older removal of
    /// that device, journalled or delivered late, also after a restart, and
    /// none of them takes effect.
    #[test]
    fn issue3079_older_epoch_removals_never_take_effect_once_the_epoch_is_known() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("third");
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        let (journalled, closed, late) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        {
            let third = Controller::open(state.clone(), vec![])?;
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.peers.insert(returning.peer.id, returning.clone());
            let mut open = removal(journalled, &removed);
            open.local_applied = false;
            open.folders = None;
            db.removals.push(open);
            third.save(&db)?;
            drop(db);
            // The closed version of another removal carries the new epoch.
            third.receive_removal(
                sender.peer.id,
                closed,
                removed.clone(),
                BTreeSet::from(["vault".into()]),
                None,
                Version {
                    superseded: true,
                    epoch: Some(Epoch::default()),
                    admission: Some(epoch(1, "AAAA")),
                },
            )?;
            let db = third.shared.db.lock().unwrap();
            assert_eq!(admitted(&db, &removed), epoch(1, "AAAA"));
            assert!(
                db.removals
                    .iter()
                    .find(|r| r.id == journalled)
                    .unwrap()
                    .superseded(),
                "an older journalled removal stayed open"
            );
            drop(db);
            third.shutdown();
        }
        // After a restart, replaying the journalled removal applies nothing,
        // and an open delivery from the old epoch arriving late is closed.
        let third = Controller::open(state, vec![])?;
        let applied = {
            let _serial = third.shared.transfer_lock.lock().unwrap();
            third.apply_removal(journalled)?
        };
        assert!(applied, "a closed removal is finished, not pending");
        third.receive_removal(
            sender.peer.id,
            late,
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version::default(),
        )?;
        let db = third.shared.db.lock().unwrap();
        assert!(db
            .removals
            .iter()
            .find(|r| r.id == late)
            .unwrap()
            .superseded());
        assert!(
            db.peers.contains_key(&returning.peer.id) && peer_is_live(&db, &returning),
            "an older-epoch removal took effect"
        );
        drop(db);
        // A removal decided after the readmission is a new decision: it
        // removes the current epoch and stays open.
        third.queue_removal(Uuid::new_v4(), &removed, BTreeSet::new(), None, None)?;
        let db = third.shared.db.lock().unwrap();
        let newest = db.removals.last().unwrap();
        assert!(
            newest.epoch.as_ref().is_some_and(|e| *e > epoch(1, "AAAA")) && !newest.superseded()
        );
        drop(db);
        third.shutdown();
        Ok(())
    }
    /// Terra round 4: a receiver that acknowledged the open version gets the
    /// closed one again once the removal is closed.
    #[test]
    fn issue3079_closing_redelivers_the_closed_version_to_earlier_receivers() -> Result<()> {
        let root = tempfile::tempdir()?;
        let inviter = Controller::open(root.path().join("inviter"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let receiver = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        let mut db = inviter.shared.db.lock().unwrap();
        let mut open = removal(id, &removed);
        open.pending.insert(receiver.peer.id, receiver.clone());
        db.removals.push(open);
        acknowledge(&mut db, receiver.peer.id, id, false)?;
        assert!(requests(&db, receiver.peer.id).is_empty());
        readmit(&mut db, &removed, Uuid::new_v4(), &[])?;
        let again = requests(&db, receiver.peer.id);
        assert_eq!(again.len(), 1, "the closed version was not delivered again");
        assert!(
            again[0].superseded
                && again[0].admission.as_ref().is_some_and(|a| a.counter == 1)
                && again[0].epoch == Some(Epoch::default())
        );
        acknowledge(&mut db, receiver.peer.id, id, true)?;
        assert!(requests(&db, receiver.peer.id).is_empty());
        drop(db);
        inviter.shutdown();
        Ok(())
    }
    /// Terra round 4: readmission through the former inviter keeps its older
    /// pairing while that pairing still carries a vault the removal did not
    /// cover; only the covered vault's transfer stops.
    #[test]
    fn issue3079_former_inviter_pairing_keeps_its_other_vault() -> Result<()> {
        let root = tempfile::tempdir()?;
        let returning = Controller::open(root.path().join("returning"), vec![])?;
        let own = ["DDDDDDD"; 8].join("-");
        let public = wire::public(&wire::random()?)?;
        let mut older = pairing(&["AAAAAAA"; 8].join("-"), public.clone())?;
        older.allowed = vec![];
        let newer = pairing(&["AAAAAAA"; 8].join("-"), public.clone())?;
        let id = Uuid::new_v4();
        let mut db = returning.shared.db.lock().unwrap();
        db.peers.insert(older.peer.id, older.clone());
        db.transfers
            .push(received(older.peer.id, "vault", &own, true)?);
        db.transfers
            .push(received(older.peer.id, "second", &own, true)?);
        db.peers.insert(newer.peer.id, newer.clone());
        accept_readmission(
            &mut db,
            &own,
            newer.peer.id,
            &public,
            &closing(&[id]),
            Some(epoch(1, "CCCC")),
        )?;
        assert!(
            db.peers.contains_key(&older.peer.id),
            "the former inviter's pairing was revoked although it carries another vault"
        );
        assert!(db
            .transfers
            .iter()
            .any(|t| t.removal_binding(older.peer.id, "second")));
        assert!(!db
            .transfers
            .iter()
            .any(|t| t.removal_binding(older.peer.id, "vault")));
        assert_eq!(admitted(&db, &own).counter, 1);
        drop(db);
        returning.shutdown();
        Ok(())
    }

    /// Terra round 5: epochs are ordered by counter, then by the readmitting
    /// computer, so concurrent readmissions never mint equal epochs; the
    /// greater one closes removals of the other's epoch.
    #[test]
    fn issue3079_concurrent_readmissions_mint_distinct_ordered_epochs() -> Result<()> {
        assert!(epoch(1, "AAAA") < epoch(1, "CCCC"));
        assert!(epoch(1, "CCCC") < epoch(2, ""));
        assert_ne!(epoch(1, "AAAA"), epoch(1, "CCCC"));
        let root = tempfile::tempdir()?;
        let controller = Controller::open(root.path().join("epochs"), vec![])?;
        let device = ["DDDDDDD"; 8].join("-");
        let mut db = controller.shared.db.lock().unwrap();
        let mut from_a = removal(Uuid::new_v4(), &device);
        from_a.epoch = Some(epoch(1, "AAAA"));
        let id = from_a.id;
        db.removals.push(from_a);
        admit(&mut db, &device, &epoch(1, "CCCC"));
        assert!(db
            .removals
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .superseded());
        admit(&mut db, &device, &epoch(1, "AAAA"));
        assert_eq!(
            admitted(&db, &device),
            epoch(1, "CCCC"),
            "an epoch never goes back"
        );
        drop(db);
        controller.shutdown();
        Ok(())
    }
    /// Terra round 5: the epoch a current removal carries is stored before
    /// any of its effects, so a later decision here uses it.
    #[test]
    fn issue3079_received_epoch_is_persisted_before_effects() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        {
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.peers.insert(returning.peer.id, returning.clone());
        }
        // A current removal of the newer epoch: no vault is associated here,
        // so queueing it refuses; the epoch is already stored by then.
        let _ = third.receive_removal(
            sender.peer.id,
            Uuid::new_v4(),
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version {
                superseded: false,
                epoch: Some(epoch(1, "AAAA")),
                admission: Some(epoch(1, "AAAA")),
            },
        );
        drop(third);
        let third = Controller::open(root.path().join("third"), vec![])?;
        assert_eq!(
            admitted(&third.shared.db.lock().unwrap(), &removed),
            epoch(1, "AAAA"),
            "the received epoch was not persisted"
        );
        third.queue_removal(Uuid::new_v4(), &removed, BTreeSet::new(), None, None)?;
        let db = third.shared.db.lock().unwrap();
        let newest = db.removals.last().unwrap();
        assert!(newest.epoch.as_ref().is_some_and(|e| *e > epoch(1, "AAAA")));
        assert!(!newest.superseded(), "a new decision was closed as stale");
        drop(db);
        third.shutdown();
        Ok(())
    }
    /// Terra round 5: a journal from before epochs, with a closed removal of
    /// the device, never applies a late removal without an epoch, delivered or
    /// journalled.
    #[test]
    fn issue3079_removals_without_epoch_are_older_than_any_known_admission() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        let (closed_before, journalled) = (Uuid::new_v4(), Uuid::new_v4());
        {
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.peers.insert(returning.peer.id, returning.clone());
            // As an older version wrote them: no epochs, no admissions.
            let mut closed = removal(closed_before, &removed);
            closed.epoch = None;
            closed.readmitted = Some(Uuid::new_v4());
            db.removals.push(closed);
            let mut open = removal(journalled, &removed);
            open.epoch = None;
            open.local_applied = false;
            open.folders = None;
            db.removals.push(open);
            assert!(db.admissions.is_empty());
        }
        let late = Uuid::new_v4();
        third.receive_removal(
            sender.peer.id,
            late,
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version::default(),
        )?;
        let applied = {
            let _serial = third.shared.transfer_lock.lock().unwrap();
            third.apply_removal(journalled)?
        };
        assert!(applied);
        let db = third.shared.db.lock().unwrap();
        for id in [late, journalled] {
            assert!(db
                .removals
                .iter()
                .find(|r| r.id == id)
                .unwrap()
                .superseded());
        }
        assert!(
            db.peers.contains_key(&returning.peer.id),
            "a removal without an epoch took effect after a readmission"
        );
        drop(db);
        third.shutdown();
        Ok(())
    }
    /// Terra round 5: a computer that applied an old removal before it could
    /// know the readmission re-establishes what it took away once it learns of
    /// it, and passes the closed version on to every peer of the vault.
    #[test]
    fn issue3079_learned_readmission_re_establishes_what_an_applied_removal_took() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        {
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.peers.insert(returning.peer.id, returning.clone());
            let mut open = removal(id, &removed);
            open.local_applied = false;
            open.folders = None;
            db.removals.push(open);
        }
        // The old open removal takes effect here first.
        assert!({
            let _serial = third.shared.transfer_lock.lock().unwrap();
            third.apply_removal(id)?
        });
        {
            let db = third.shared.db.lock().unwrap();
            assert!(
                db.revoked_peers.contains_key(&returning.peer.id),
                "positive control: the removal revoked the pairing"
            );
            let undo = db
                .removals
                .iter()
                .find(|r| r.id == id)
                .unwrap()
                .undo
                .clone();
            assert!(undo.is_some_and(|u| u.pairings.contains_key(&returning.peer.id)));
        }
        // Then the closed version arrives with the readmission's epoch.
        third.receive_removal(
            sender.peer.id,
            id,
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version {
                superseded: true,
                epoch: Some(Epoch::default()),
                admission: Some(epoch(1, "CCCC")),
            },
        )?;
        {
            let db = third.shared.db.lock().unwrap();
            let record = db.removals.iter().find(|r| r.id == id).unwrap();
            assert!(record.superseded());
            assert!(record.undo.as_ref().is_some_and(|u| u.pending));
            assert!(
                record.pending.contains_key(&sender.peer.id),
                "the closed version must reach the original sender too"
            );
        }
        third.compensate_removals()?;
        let db = third.shared.db.lock().unwrap();
        assert!(
            db.peers.contains_key(&returning.peer.id) && peer_is_live(&db, &returning),
            "the revoked pairing was not re-established"
        );
        assert!(db
            .removals
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .undo
            .is_none());
        drop(db);
        third.shutdown();
        Ok(())
    }
    /// Terra round 5: the returning computer re-establishes the pairings its
    /// own removal revoked with other computers; only the pairing its inviter
    /// replaced stays revoked.
    #[test]
    fn issue3079_returning_computer_re_establishes_pairings_its_removal_revoked() -> Result<()> {
        let root = tempfile::tempdir()?;
        let returning = Controller::open(root.path().join("returning"), vec![])?;
        let own = ["DDDDDDD"; 8].join("-");
        let other = pairing(&["EEEEEEE"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let inviter_public = wire::public(&wire::random()?)?;
        let replaced = pairing(&["CCCCCCC"; 8].join("-"), inviter_public.clone())?;
        let newer = pairing(&["CCCCCCC"; 8].join("-"), inviter_public.clone())?;
        let id = Uuid::new_v4();
        {
            let mut db = returning.shared.db.lock().unwrap();
            db.peers.insert(other.peer.id, other.clone());
            db.peers.insert(replaced.peer.id, replaced.clone());
            let mut own_removal = removal(id, &own);
            own_removal.local_applied = false;
            own_removal.folders = None;
            db.removals.push(own_removal.clone());
            deny(&mut db, &own_removal, true);
            assert!(db.revoked_peers.contains_key(&other.peer.id));
            db.peers.insert(newer.peer.id, newer.clone());
            accept_readmission(
                &mut db,
                &own,
                newer.peer.id,
                &inviter_public,
                &closing(&[id]),
                Some(epoch(1, "CCCC")),
            )?;
        }
        returning.compensate_removals()?;
        let db = returning.shared.db.lock().unwrap();
        assert!(
            db.peers.contains_key(&other.peer.id),
            "pairing with another computer stayed revoked"
        );
        assert!(
            !db.peers.contains_key(&replaced.peer.id),
            "the pairing the inviter replaced came back"
        );
        assert!(db.peers.contains_key(&newer.peer.id) && !db.local_removed);
        drop(db);
        returning.shutdown();
        Ok(())
    }
    /// Terra round 5: during its own removal, a kept pairing that still
    /// carries another vault keeps exchanging addresses.
    #[test]
    fn issue3079_kept_pairing_keeps_address_exchange_during_own_removal() -> Result<()> {
        let root = tempfile::tempdir()?;
        let controller = Controller::open(root.path().join("own"), vec![])?;
        let own = ["DDDDDDD"; 8].join("-");
        let mut kept = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        kept.allowed = vec![];
        let mut db = controller.shared.db.lock().unwrap();
        db.peers.insert(kept.peer.id, kept.clone());
        db.transfers
            .push(received(kept.peer.id, "vault", &own, true)?);
        db.transfers
            .push(received(kept.peer.id, "second", &own, true)?);
        db.attached.push(existing_vault(&own, "vault")?);
        let mut own_removal = removal(Uuid::new_v4(), &own);
        own_removal.local_applied = false;
        own_removal.folders = None;
        db.removals.push(own_removal.clone());
        deny(&mut db, &own_removal, true);
        assert!(db.peers.contains_key(&kept.peer.id));
        assert!(
            peer_is_live(&db, &kept),
            "a kept pairing with another vault lost address exchange"
        );
        drop(db);
        controller.shutdown();
        Ok(())
    }

    /// Round 6: a removal decided here after a readmission takes an epoch
    /// above everything known here, so a concurrent readmission minted
    /// elsewhere with the same counter can never close it.
    #[test]
    fn issue3079_new_removal_outranks_a_concurrent_readmission() -> Result<()> {
        let root = tempfile::tempdir()?;
        let alice = Controller::open(root.path().join("alice"), vec![])?;
        let bob = ["BBBBBBB"; 8].join("-");
        let returning = pairing(&bob, wire::public(&wire::random()?)?)?;
        {
            let mut db = alice.shared.db.lock().unwrap();
            db.peers.insert(returning.peer.id, returning.clone());
            // Alice readmitted Bob at (1, Alice).
            db.admissions.insert(bob.clone(), epoch(1, "AAAA"));
        }
        let id = Uuid::new_v4();
        alice.queue_removal(id, &bob, BTreeSet::new(), None, None)?;
        let mut db = alice.shared.db.lock().unwrap();
        let mine = db
            .removals
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .epoch
            .clone()
            .unwrap();
        assert!(
            mine > epoch(1, "CCCC"),
            "a new removal reused a readmission epoch"
        );
        // Carol's concurrent readmission arrives afterwards.
        admit(&mut db, &bob, &epoch(1, "CCCC"));
        assert!(
            !db.removals
                .iter()
                .find(|r| r.id == id)
                .unwrap()
                .superseded(),
            "a concurrent readmission closed a removal decided after it"
        );
        drop(db);
        alice.shutdown();
        Ok(())
    }
    /// Round 6: a newer admission learned through an already-closed removal
    /// is passed on to every peer of its vault as well.
    #[test]
    fn issue3079_newer_admission_on_a_closed_removal_is_passed_on() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let behind = pairing(&["EEEEEEE"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        {
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.peers.insert(behind.peer.id, behind.clone());
            let mut closed = removal(id, &removed);
            closed.readmitted = Some(Uuid::new_v4());
            db.removals.push(closed);
            db.admissions.insert(removed.clone(), epoch(1, "AAAA"));
        }
        third.receive_removal(
            sender.peer.id,
            id,
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version {
                superseded: true,
                epoch: Some(Epoch::default()),
                admission: Some(epoch(2, "CCCC")),
            },
        )?;
        let db = third.shared.db.lock().unwrap();
        assert_eq!(admitted(&db, &removed), epoch(2, "CCCC"));
        let record = db.removals.iter().find(|r| r.id == id).unwrap();
        assert!(
            record.pending.contains_key(&behind.peer.id),
            "the newer admission stayed here"
        );
        let onward = requests(&db, behind.peer.id);
        assert!(onward[0].admission == Some(epoch(2, "CCCC")));
        drop(db);
        third.shutdown();
        Ok(())
    }
    /// Round 6: a membership that cannot be re-established yet (runtime
    /// down) stays pending and is retried; the pairing comes back at once.
    #[test]
    fn issue3079_unreachable_membership_restore_stays_pending() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        {
            let mut db = third.shared.db.lock().unwrap();
            db.revoked_peers
                .insert(returning.peer.id, returning.clone());
            let mut closed = removal(id, &removed);
            closed.readmitted = Some(Uuid::nil());
            closed.undo = Some(Undo {
                pairings: BTreeMap::from([(returning.peer.id, returning.clone())]),
                grants: vec![],
                members: vec![RemovedMember {
                    vault: existing_vault(&["CCCCCCC"; 8].join("-"), "vault")?,
                    member: json!({"deviceID": removed}),
                }],
                pending: true,
            });
            db.removals.push(closed);
        }
        third.compensate_removals()?;
        let db = third.shared.db.lock().unwrap();
        assert!(db.peers.contains_key(&returning.peer.id));
        let undo = db
            .removals
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .undo
            .clone();
        assert!(
            undo.is_some_and(|u| u.pending && u.members.len() == 1),
            "an unreachable membership restore was dropped instead of retried"
        );
        drop(db);
        third.shutdown();
        Ok(())
    }

    /// Round 7: remove, readmit, remove again. A computer that missed the
    /// readmission learns it from the second removal and applies that one.
    /// Compensation of the first must then not re-establish anything.
    #[test]
    fn issue3079_compensation_yields_to_a_newer_applied_removal() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let returning = pairing(&removed, wire::public(&wire::random()?)?)?;
        let member = RemovedMember {
            vault: existing_vault(&["CCCCCCC"; 8].join("-"), "vault")?,
            member: json!({"deviceID": removed}),
        };
        let (older, newer, waiting) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        {
            let mut db = third.shared.db.lock().unwrap();
            db.revoked_peers
                .insert(returning.peer.id, returning.clone());
            let mut first = removal(older, &removed);
            first.readmitted = Some(Uuid::nil());
            first.undo = Some(Undo {
                pairings: BTreeMap::from([(returning.peer.id, returning.clone())]),
                grants: vec![],
                members: vec![member.clone()],
                pending: true,
            });
            db.removals.push(first);
            let mut second = removal(newer, &removed);
            second.epoch = Some(epoch(2, "AAAA"));
            db.removals.push(second);
        }
        third.compensate_removals()?;
        {
            let db = third.shared.db.lock().unwrap();
            assert!(
                !db.peers.contains_key(&returning.peer.id),
                "compensation restored a pairing a newer removal revoked"
            );
            assert!(db
                .removals
                .iter()
                .find(|r| r.id == older)
                .unwrap()
                .undo
                .is_none());
        }
        // A newer removal still being applied: compensation waits.
        {
            let mut db = third.shared.db.lock().unwrap();
            db.removals.retain(|r| r.id != newer);
            let first = db.removals.iter_mut().find(|r| r.id == older).unwrap();
            first.undo = Some(Undo {
                pairings: BTreeMap::from([(returning.peer.id, returning.clone())]),
                grants: vec![],
                members: vec![member],
                pending: true,
            });
            let mut open = removal(waiting, &removed);
            open.local_applied = false;
            open.folders = None;
            db.removals.push(open);
        }
        third.compensate_removals()?;
        let db = third.shared.db.lock().unwrap();
        assert!(!db.peers.contains_key(&returning.peer.id));
        assert!(
            db.removals
                .iter()
                .find(|r| r.id == older)
                .unwrap()
                .undo
                .as_ref()
                .is_some_and(|u| u.pending),
            "compensation must stay pending while a newer removal applies"
        );
        drop(db);
        third.shutdown();
        Ok(())
    }
    /// Round 7: epochs from other computers are bounded, and a counter that
    /// cannot grow fails a new removal in the open instead of closing it.
    #[test]
    fn issue3079_epochs_are_bounded_and_never_wrap() -> Result<()> {
        assert!(epoch(MAX_EPOCH - 1, "").valid());
        assert!(!epoch(MAX_EPOCH, "").valid());
        assert!(!epoch(u64::MAX, "").valid());
        assert!(epoch(1, &["AAAAAAA"; 8].join("-")).valid());
        assert!(!Epoch {
            counter: 1,
            by: "not-a-device".into()
        }
        .valid());
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        third
            .shared
            .db
            .lock()
            .unwrap()
            .peers
            .insert(sender.peer.id, sender.clone());
        assert!(third
            .receive_removal(
                sender.peer.id,
                Uuid::new_v4(),
                removed.clone(),
                BTreeSet::from(["vault".into()]),
                None,
                Version {
                    superseded: true,
                    epoch: None,
                    admission: Some(epoch(u64::MAX, "")),
                },
            )
            .is_err());
        assert_eq!(
            admitted(&third.shared.db.lock().unwrap(), &removed),
            Epoch::default()
        );
        {
            // Even if a counter at the limit is already stored, a new removal
            // refuses instead of wrapping and closing itself.
            let mut db = third.shared.db.lock().unwrap();
            db.admissions.insert(removed.clone(), epoch(MAX_EPOCH, ""));
            assert!(next_epoch(&db, &removed).is_err());
        }
        assert!(third
            .queue_removal(Uuid::new_v4(), &removed, BTreeSet::new(), None, None)
            .is_err());
        assert!(third.shared.db.lock().unwrap().removals.is_empty());
        third.shutdown();
        Ok(())
    }
}
