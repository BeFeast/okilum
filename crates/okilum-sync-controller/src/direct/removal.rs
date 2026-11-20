//! Removal is an authenticated, replayable operation in application state.
//! It never deletes vault files or unrelated Syncthing folders/devices.
//!
//! For each vault, one computer decides: the computer that shared it. Every
//! other computer applies that owner's decision and does not mint a removal
//! or a readmission. Remove on a non-owner is a request the owner applies.
//! Removing the owner itself is still the #2042 handoff, which the owner runs
//! when it applies that request (#3079).
//!
//! For each vault, one computer decides: the computer that shared it. Every
//! other computer applies that owner's removal and readmission, and does not
//! mint either. Remove on a non-owner stores a request; the owner applies it.
//! Removing the owner itself is that request, carried out by the existing
//! introducer handoff (#2042, #3079).
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
    /// A received removal relayed onward: for each relay recipient, the
    /// vaults this computer shared with it, the only ones it is that
    /// recipient's owner for. Captured before applying the removal revokes
    /// those grants (#3105).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    relayed: BTreeMap<Uuid, BTreeSet<String>>,
    /// Vaults whose effects finished before a later delivery of the same
    /// removal named more vaults: one decision can reach this computer as
    /// disjoint subsets through different relays, each the owner of its own
    /// vaults. Their folders are not touched again (#3195).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    done: BTreeSet<String>,
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
/// A non-owner's request that the vault owner remove `device` (#3079).
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemovalRequest {
    id: Uuid,
    device: String,
    vaults: BTreeSet<String>,
    owner: Uuid,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    accepted: bool,
}
pub(super) fn requests(db: &Database, scope: Uuid) -> Vec<Request> {
    db.removals
        .iter()
        .filter(|r| (r.local_applied || departing(r)) && r.pending.contains_key(&scope))
        .map(|r| {
            let peer = &r.pending[&scope];
            let vaults: BTreeSet<_> = r
                .vaults_for(scope)
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
                admission: known_admission_for(db, &r.device, &vaults),
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
/// The admission to put on the wire: none before any readmission, so a
/// peer on the previous version, which refuses unknown fields, still
/// accepts the message (#3079).
pub(super) fn known_admission(db: &Database, device: &str) -> Option<Epoch> {
    Some(admitted(db, device)).filter(|epoch| epoch.counter != 0)
}
/// The admission asserted for these vaults, not the highest counter stored
/// for any other vault of the same device (#3079).
fn known_admission_for(db: &Database, device: &str, vaults: &BTreeSet<String>) -> Option<Epoch> {
    if let Some(per) = db.admission_vaults.get(device) {
        if !vaults.is_empty() && vaults.iter().any(|vault| per.contains_key(vault)) {
            let counter = vaults
                .iter()
                .map(|vault| per.get(vault).copied().unwrap_or(0))
                .min()
                .unwrap_or(0);
            if counter == 0 {
                return None;
            }
            let by = db
                .admissions
                .get(device)
                .filter(|epoch| epoch.counter == counter)
                .map(|epoch| epoch.by.clone())
                .unwrap_or_default();
            return Some(Epoch { counter, by });
        }
    }
    known_admission(db, device)
}
/// Whether a removal of `device` from `epoch` is older than what this
/// computer knows. A removal without an epoch (older software or journal) is
/// older than any known admission, and than any closed removal of `device`.
/// `vaults` limits which admission can close this removal. A counter learned
/// for vault A does not make a removal of vault B stale. `message` means
/// `horizon` was just received for exactly these vaults (#3079).
fn stale_for(
    db: &Database,
    device: &str,
    epoch: Option<&Epoch>,
    horizon: &Epoch,
    vaults: Option<&BTreeSet<String>>,
    message: bool,
) -> bool {
    if let Some(vaults) = vaults {
        if db.admission_vaults.contains_key(device) || message {
            let epoch_counter = epoch.map(|epoch| epoch.counter).unwrap_or(0);
            let by_counter = !vaults.is_empty()
                && vaults.iter().all(|vault| {
                    let stored = db
                        .admission_vaults
                        .get(device)
                        .and_then(|per| per.get(vault))
                        .copied()
                        .unwrap_or(0);
                    // `horizon` is the admission this message's owner asserted
                    // for these vaults. It is not the device-wide maximum.
                    let effective = if message {
                        stored.max(horizon.counter)
                    } else {
                        stored
                    };
                    epoch_counter < effective
                });
            if epoch.is_some() {
                return by_counter;
            }
            return by_counter
                || db.removals.iter().any(|removal| {
                    removal.device == device
                        && removal.superseded()
                        && !removal.vaults.is_empty()
                        && removal.vaults.is_subset(vaults)
                });
        }
    }
    match epoch {
        // Only a strictly newer admission counter closes a removal: a removal
        // and a readmission minted concurrently with the same counter are
        // unordered, and the removal, a later explicit decision, stands.
        Some(epoch) => epoch.counter < horizon.counter,
        None => {
            *horizon > Epoch::default()
                || db
                    .removals
                    .iter()
                    .any(|r| r.device == device && r.superseded())
        }
    }
}
fn removal_in_scope(removal: &Removal, scope: &BTreeSet<String>) -> bool {
    !removal.vaults.is_empty() && removal.vaults.is_subset(scope)
}
/// Raise `device`'s admission for every vault. Fixture tests use this when one
/// computer is the only decider; a learned admission uses `admit_for`.
#[cfg(test)]
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
                && r.epoch
                    .as_ref()
                    .is_none_or(|epoch| epoch.counter < admission.counter)
        })
        .map(|r| r.id)
        .collect();
    for removal in db.removals.iter_mut().filter(|r| older.contains(&r.id)) {
        removal.close_learned(Uuid::nil());
    }
    let closed: Vec<Uuid> = db
        .removals
        .iter()
        .filter(|r| r.device == device && r.superseded())
        .map(|r| r.id)
        .collect();
    rebroadcast(db, &closed);
}
/// Record `admission` for `scope` only. Removals that name any other vault
/// stay open, and the device-wide counter does not cover them (#3079).
fn admit_for(db: &mut Database, device: &str, admission: &Epoch, scope: &BTreeSet<String>) {
    if scope.is_empty() {
        return;
    }
    let older: Vec<Uuid> = db
        .removals
        .iter()
        .filter(|removal| {
            removal.device == device
                && !removal.superseded()
                && removal_in_scope(removal, scope)
                && removal
                    .epoch
                    .as_ref()
                    .is_none_or(|epoch| epoch.counter < admission.counter)
        })
        .map(|removal| removal.id)
        .collect();
    for removal in db.removals.iter_mut().filter(|r| older.contains(&r.id)) {
        removal.close_learned(Uuid::nil());
    }
    // Already-closed removals of these vaults carry the newer admission
    // onward. Removals of any other vault are not announced.
    let pass_on: Vec<Uuid> = db
        .removals
        .iter()
        .filter(|removal| {
            removal.device == device && removal.superseded() && removal_in_scope(removal, scope)
        })
        .map(|removal| removal.id)
        .collect();
    rebroadcast(db, &pass_on);
    let per = db.admission_vaults.entry(device.to_owned()).or_default();
    for vault in scope {
        let counter = per.entry(vault.clone()).or_insert(0);
        *counter = (*counter).max(admission.counter);
    }
    if *admission > admitted(db, device) {
        db.admissions.insert(device.to_owned(), admission.clone());
    }
}
/// Deliver these closed removals to every peer that shares one of their
/// vaults, the original sender included, so each computer that applied the
/// open version learns of the readmission (#3079).
fn rebroadcast(db: &mut Database, ids: &[Uuid]) {
    // Closing this computer's own removal is not this computer's to announce.
    let own = own_device(db);
    for id in ids {
        if db
            .removals
            .iter()
            .any(|r| r.id == *id && !own.is_empty() && r.device == own)
        {
            continue;
        }
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
                // Only the vault owner announces a close. A receiver that
                // learned it does not push that close back (#3079).
                vaults
                    .iter()
                    .any(|v| foreign_owner(db, v).is_none() && knows(db, **peer, v))
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
    next_epoch_above(db, device, 0)
}
/// The highest admission or removal counter of `device` known here.
/// A pairing request may not claim a history far beyond it (#3079).
pub(super) fn high_water(db: &Database, device: &str) -> u64 {
    db.removals
        .iter()
        .filter(|r| r.device == device)
        .filter_map(|r| r.epoch.as_ref().map(|e| e.counter))
        .chain([admitted(db, device).counter])
        .max()
        .unwrap_or(0)
}
/// Like `next_epoch`, also strictly above `floor`.
fn next_epoch_above(db: &Database, device: &str, floor: u64) -> Result<Epoch> {
    let counter = db
        .removals
        .iter()
        .filter(|r| r.device == device)
        .filter_map(|r| r.epoch.as_ref().map(|e| e.counter))
        .chain([admitted(db, device).counter, floor])
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|counter| *counter < MAX_EPOCH)
        .context("Removal history of this computer is exhausted; no folder was changed")?;
    Ok(Epoch {
        counter,
        by: own_device(db),
    })
}
/// Admission counters accepted from other computers: a counter that leaves
/// no room to grow could close every later removal here (#3079).
pub(super) const MAX_EPOCH: u64 = 1_000_000;
/// Closed records learned from other computers stop here, keeping room for
/// open removals decided or received (256) below `MAX_REMOVALS` (#3079).
const MAX_TOMBSTONES: usize = MAX_REMOVALS - 256;
impl Epoch {
    fn valid(&self) -> bool {
        self.counter < MAX_EPOCH && (self.by.is_empty() || valid(&self.by))
    }
}
/// Queue the closed version of removal `id` to the computer `scope` that just
/// delivered it open, so that computer learns of the close (#3079).
fn queue_closed_to(db: &mut Database, id: Uuid, scope: Uuid) {
    let Some(paired) = db
        .peers
        .get(&scope)
        .or_else(|| db.revoked_peers.get(&scope))
        .cloned()
    else {
        return;
    };
    let own = own_device(db);
    if let Some(removal) = db
        .removals
        .iter_mut()
        .find(|r| r.id == id && (own.is_empty() || r.device != own))
    {
        // A closed record's vaults are fixed: a delivery never widens them.
        removal.pending.insert(scope, paired);
    }
}
/// The vaults of a delivery that are associated here and that its sender
/// is authorized for: the only vaults a new tombstone records (#3079).
fn authorized_vaults(db: &Database, scope: Uuid, vaults: &BTreeSet<String>) -> BTreeSet<String> {
    let sender = db
        .peers
        .get(&scope)
        .or_else(|| db.revoked_peers.get(&scope));
    vaults
        .iter()
        .filter(|v| db.attached.iter().any(|a| a.folder_id == **v))
        .filter(|v| {
            sender.is_some_and(|peer| peer.allowed.contains(v))
                || knows(db, scope, v)
                || db
                    .transfers
                    .iter()
                    .any(|t| t.peer() == scope && t.vault() == v.as_str())
        })
        .cloned()
        .collect()
}
/// Whether an approval's admission for this computer (`own`) is one it can
/// take: a few cycles ahead at most, with room to grow below the limit.
pub(super) fn admission_in_range(db: &Database, own: &str, admission: &Epoch) -> bool {
    admission.valid()
        && admission.counter <= high_water(db, own).saturating_add(64)
        && admission.counter.saturating_add(1) < MAX_EPOCH
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
    /// The vaults delivered to `scope`: a relay recipient gets only the
    /// vaults it received from here (#3105), every other receiver all of them.
    fn vaults_for(&self, scope: Uuid) -> BTreeSet<String> {
        self.relayed
            .get(&scope)
            .map(|relayed| self.vaults.intersection(relayed).cloned().collect())
            .unwrap_or_else(|| self.vaults.clone())
    }
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
            relayed: BTreeMap::new(),
            done: BTreeSet::new(),
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
/// Readmission without per-removal vaults, as from an older joiner.
#[cfg(test)]
pub(super) fn readmit(
    db: &mut Database,
    device: &str,
    pairing: Uuid,
    known: &[Uuid],
    high_water: u64,
) -> Result<()> {
    readmit_with_vaults(db, device, pairing, known, &BTreeMap::new(), high_water)
}
/// The vaults each removal the returning computer applied covered there,
/// sent with its pairing request (#3079).
pub(super) fn applied_vaults(db: &Database, device: &str) -> BTreeMap<Uuid, BTreeSet<String>> {
    db.removals
        .iter()
        .filter(|r| {
            r.device == device && !r.superseded() && (r.local_applied || r.folders.is_some())
        })
        .map(|r| (r.id, r.vaults.clone()))
        .take(MAX_REMOVALS)
        .collect()
}
pub(super) fn readmit_with_vaults(
    db: &mut Database,
    device: &str,
    pairing: Uuid,
    known: &[Uuid],
    known_vaults: &BTreeMap<Uuid, BTreeSet<String>>,
    high_water: u64,
) -> Result<()> {
    // The returning computer may know a few cycles this one missed, not an
    // arbitrary jump that would exhaust the epoch range (#3079).
    ensure!(
        high_water < MAX_EPOCH && high_water <= self::high_water(db, device).saturating_add(64),
        "Invalid removal history in pairing request"
    );
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
        db.removals.len() + new.len() <= MAX_TOMBSTONES || new.is_empty(),
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
    // The vault owner mints the next admission from its own history. Another
    // computer's counter is not a second writer (#3079).
    let epoch = next_epoch(db, device)?;
    let mut closed = vec![];
    // Only vaults this computer decides. A removal of someone else's vault
    // stays open (#3079).
    let close_ids: Vec<Uuid> = db
        .removals
        .iter()
        .filter(|removal| {
            removal.device == device
                && !removal.superseded()
                && !removal.vaults.is_empty()
                && removal
                    .vaults
                    .iter()
                    .all(|vault| foreign_owner(db, vault).is_none())
        })
        .map(|removal| removal.id)
        .collect();
    let mut owned = BTreeSet::new();
    for removal in db
        .removals
        .iter_mut()
        .filter(|removal| close_ids.contains(&removal.id))
    {
        owned.extend(removal.vaults.iter().cloned());
        removal.undo = None;
        removal.close(pairing);
        closed.push(removal.id);
    }
    // A removal the returning computer applied but this computer never saw:
    // close it for the vaults shared here and deliver that closed version to
    // their peers, which may still hold it open (#3079).
    // Only the vaults that removal covered, and only those shared here.
    // Tombstones cover only vaults this computer decides. A received vault
    // stays the owner's to close (#3079).
    let shared: BTreeSet<String> = db
        .attached
        .iter()
        .filter(|v| foreign_owner(db, &v.folder_id).is_none())
        .map(|v| v.folder_id.clone())
        .collect();
    for id in new {
        // An empty or partial scope grants nothing: the approver must own
        // every vault the returning computer names (#3079).
        let Some(reported) = known_vaults.get(&id).filter(|vaults| {
            !vaults.is_empty() && vaults.iter().all(|vault| positively_owns(db, vault))
        }) else {
            continue;
        };
        let mut tombstone = Removal::tombstone(id, device, pairing);
        tombstone.vaults = reported.clone();
        db.removals.push(tombstone);
        closed.push(id);
    }
    owned.extend(shared.iter().cloned());
    if !owned.is_empty() {
        admit_for(db, device, &epoch, &owned);
    } else if db.removals.iter().all(|removal| {
        removal.device != device
            || removal.superseded()
            || removal
                .vaults
                .iter()
                .all(|vault| foreign_owner(db, vault).is_none())
    }) {
        db.admissions.insert(device.to_owned(), epoch);
    }
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
    if readmitted.is_empty() {
        // No vault was named, so this approval cannot raise a device-wide
        // admission or close another owner's removal (#3079).
        return Ok(());
    }
    ensure!(
        admission
            .as_ref()
            .is_none_or(|a| admission_in_range(db, own, a)),
        "Invalid admission epoch in approval"
    );
    // Closed IDs beyond the tombstone room are not recorded here; the
    // admission epoch alone still closes those removals if they arrive.
    for closed in readmitted {
        let room = db.removals.len() < MAX_TOMBSTONES;
        let close_existing = db.removals.iter().any(|removal| {
            removal.id == closed.id && removal.device == own && approver_owns(db, inviter, removal)
        });
        let record_new = !db.removals.iter().any(|removal| removal.id == closed.id)
            && !closed.id.is_nil()
            && room
            && approver_owns_vaults(db, inviter, &closed.vaults);
        if close_existing {
            if let Some(removal) = db
                .removals
                .iter_mut()
                .find(|removal| removal.id == closed.id)
            {
                removal.close(pairing);
            }
        } else if record_new {
            let mut tombstone = Removal::tombstone(closed.id, own, pairing);
            tombstone.vaults = closed.vaults.clone();
            db.removals.push(tombstone);
        }
    }
    if let Some(admission) = &admission {
        let scope: BTreeSet<String> = readmitted
            .iter()
            .filter(|closed| approver_owns_vaults(db, inviter, &closed.vaults))
            .flat_map(|closed| closed.vaults.iter().cloned())
            .collect();
        admit_for(db, own, admission, &scope);
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
                || r.epoch.is_none()
                    && stale_for(
                        db,
                        &r.device,
                        None,
                        &admitted(db, &r.device),
                        Some(&r.vaults),
                        false,
                    ))
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
/// The pairings through which this computer shared some of `vaults` onward
/// to the removed `device`, each with exactly those vaults: its own durable
/// share grant names that device. For those vaults only, this computer is
/// the recipient's vault owner, so the recipient accepts the relayed
/// decision; a vault it received from another computer is not named. The
/// sender and every other peer get nothing, and a computer without such a
/// grant waits for no one (#3105).
fn onward_to_removed(
    db: &Database,
    sender: Uuid,
    device: &str,
    vaults: &BTreeSet<String>,
) -> BTreeMap<Uuid, BTreeSet<String>> {
    db.peers
        .iter()
        .filter(|(peer, paired)| {
            **peer != sender && paired.syncthing_device.as_deref() == Some(device)
        })
        .filter_map(|(peer, _)| {
            let shared: BTreeSet<String> = db
                .share_grants
                .iter()
                .filter(|grant| {
                    grant.for_peer(*peer) && grant.receives(device) && vaults.contains(&grant.vault)
                })
                .map(|grant| grant.vault.clone())
                .collect();
            (!shared.is_empty()).then_some((*peer, shared))
        })
        .collect()
}
fn device_of(db: &Database, peer: Uuid) -> Option<String> {
    db.peers
        .get(&peer)
        .or_else(|| db.revoked_peers.get(&peer))
        .and_then(|paired| paired.syncthing_device.clone())
}
/// The live paired computer this vault was received from.
/// None when this computer shared it, or when no receipt is recorded.
/// A later pairing with the same Syncthing device replaces a revoked one,
/// so a readmitted owner is still the owner (#3079).
pub(super) fn foreign_owner(db: &Database, vault: &str) -> Option<Uuid> {
    if db.sources.iter().any(|source| source.owns(vault)) {
        return None;
    }
    let recorded = db
        .transfers
        .iter()
        .find(|transfer| transfer.vault() == vault)?
        .peer();
    let Some(device) = device_of(db, recorded) else {
        return Some(recorded);
    };
    // Keep the pairing the vault was actually received through while it is
    // still live. Another pairing with the same device, including one whose
    // UUID sorts first and whose listener is local, is not the owner route.
    if db.peers.contains_key(&recorded) {
        return Some(recorded);
    }
    db.peers
        .iter()
        .find_map(|(id, peer)| {
            (peer.syncthing_device.as_deref() == Some(device.as_str())).then_some(*id)
        })
        .or(Some(recorded))
}
/// This computer enrolled `vault`. Absence of a transfer is not ownership.
fn positively_owns(db: &Database, vault: &str) -> bool {
    db.sources.iter().any(|source| source.owns(vault))
        || db
            .attached
            .iter()
            .any(|attached| attached.folder_id == vault)
            && db
                .transfers
                .iter()
                .all(|transfer| transfer.vault() != vault)
}
fn pairing_public(db: &Database, peer: Uuid) -> Option<&[u8]> {
    db.peers
        .get(&peer)
        .or_else(|| db.revoked_peers.get(&peer))
        .map(|paired| paired.public.as_slice())
}
/// Every vault of this removal was received from the approving computer.
/// No recorded owner (unit journals) keeps the previous close.
fn approver_owns(db: &Database, inviter: &[u8], removal: &Removal) -> bool {
    approver_owns_vaults(db, inviter, &removal.vaults)
}
fn approver_owns_vaults(db: &Database, inviter: &[u8], vaults: &BTreeSet<String>) -> bool {
    if vaults.is_empty() {
        return false;
    }
    let owners: Vec<Option<Uuid>> = vaults
        .iter()
        .map(|vault| foreign_owner(db, vault))
        .collect();
    if owners.iter().all(Option::is_none) {
        return true;
    }
    owners.iter().all(|owner| {
        owner.is_some_and(|peer| pairing_public(db, peer).is_some_and(|public| public == inviter))
    })
}
/// A removal is the vault owner's decision when every vault was received
/// from `sender`'s computer. Empty, or any vault this computer itself
/// shared, is not a decision a peer may deliver (#3079).
pub(super) fn removal_from_owner(db: &Database, sender: Uuid, vaults: &BTreeSet<String>) -> bool {
    if vaults.is_empty()
        || vaults
            .iter()
            .any(|vault| db.sources.iter().any(|source| source.owns(vault)))
    {
        return false;
    }
    let sender_device = device_of(db, sender);
    vaults.iter().all(|vault| match foreign_owner(db, vault) {
        Some(owner) => sender_device.is_some() && device_of(db, owner) == sender_device,
        None => true,
    })
}
/// `device` has an open removal of a vault this computer received from
/// someone else, or the returning computer reports one this computer has
/// not seen. Only that vault's owner may readmit it (#3079).
pub(super) fn ensure_may_readmit(
    db: &Database,
    device: &str,
    known: &[Uuid],
    known_vaults: &BTreeMap<Uuid, BTreeSet<String>>,
) -> Result<()> {
    let blocked = db.removals.iter().any(|removal| {
        removal.device == device
            && !removal.superseded()
            && removal
                .vaults
                .iter()
                .any(|vault| foreign_owner(db, vault).is_some())
    }) || known.iter().any(|id| {
        if db.removals.iter().any(|removal| removal.id == *id) {
            return false;
        }
        match known_vaults.get(id) {
            Some(vaults) => vaults
                .iter()
                .any(|vault| foreign_owner(db, vault).is_some()),
            None => db
                .attached
                .iter()
                .any(|vault| foreign_owner(db, &vault.folder_id).is_some()),
        }
    });
    ensure!(!blocked, "Only the vault owner can readmit this computer");
    Ok(())
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
        let pending = {
            let db = self.shared.db.lock().unwrap();
            db.removals.iter().any(|r| {
                r.device == device && !r.superseded() && (!r.local_applied || !r.pending.is_empty())
            }) || db
                .removal_requests
                .iter()
                .any(|r| r.device == device && !r.accepted)
        };
        if !pending {
            let scope = self.removal_scope(device)?;
            let mut acted = false;
            if let Some(owned) = scope.get(&None).cloned() {
                if !owned.is_empty() {
                    self.queue_removal(Uuid::new_v4(), device, owned, None, None)?;
                    acted = true;
                }
            }
            for (owner, vaults) in &scope {
                if let Some(owner) = owner {
                    self.queue_request(device, vaults.clone(), *owner)?;
                    acted = true;
                }
            }
            if !acted {
                self.queue_removal(Uuid::new_v4(), device, BTreeSet::new(), None, None)?;
            }
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
        let mut requested = requested;
        let (attached, known, authorized, extending) = {
            let db = self.shared.db.lock().unwrap();
            let extending = match db.removals.iter().find(|r| r.id == id) {
                Some(old) => {
                    ensure!(
                        old.device == device,
                        "Removal identity conflicts with its durable request"
                    );
                    // A replay changes nothing. A received delivery of the
                    // same open decision may still name vaults this record
                    // lacks: another relay owns those for this computer and
                    // delivers its own subset under the same id (#3195).
                    let Some(sender) = sender.filter(|_| !old.superseded()) else {
                        return Ok(());
                    };
                    requested = requested
                        .difference(&old.vaults)
                        .filter(|v| knows(&db, sender, v))
                        .cloned()
                        .collect();
                    if requested.is_empty() {
                        return Ok(());
                    }
                    true
                }
                None => false,
            };
            ensure!(
                extending
                    || db.removals.iter().filter(|r| !r.superseded()).count() < 256
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
                extending,
            )
        };
        let configs = self.configuration_paths()?;
        let mut vaults = BTreeSet::new();
        for vault in attached {
            // A decision made here covers only the vaults this call named.
            // An empty set keeps the previous "every vault that has the
            // device" behavior for a local removal that did not split owners.
            if sender.is_none() && !requested.is_empty() && !requested.contains(&vault.folder_id) {
                continue;
            }
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
            // A request names the vaults even when Syncthing already dropped
            // the member; the owner's decision still covers that scope.
            if fresh.peers.iter().any(|p| p == device)
                || authorized.contains(&fresh.folder_id)
                || sender.is_none() && requested.contains(&fresh.folder_id)
            {
                vaults.insert(fresh.folder_id);
            }
        }
        if extending && vaults.is_empty() {
            // Nothing of the further subset is here: as before, a duplicate.
            return Ok(());
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
        if let Some(index) = db.removals.iter().position(|r| r.id == id) {
            ensure!(
                db.removals[index].device == device,
                "Removal identity conflicts with its durable request"
            );
            if !extending || db.removals[index].superseded() {
                return Ok(());
            }
            // The same decision for further vaults, from their owner: the one
            // record covers them too and applies again, to those folders
            // only. What finished already stays done (#3195).
            let sender = sender.context("Removal sender missing")?;
            let added: BTreeSet<String> = vaults
                .difference(&db.removals[index].vaults)
                .cloned()
                .collect();
            if added.is_empty() {
                return Ok(());
            }
            let relayed = onward_to_removed(&db, sender, device, &added);
            let relays: Vec<(Uuid, Paired)> = relayed
                .keys()
                .filter_map(|peer| db.peers.get(peer).map(|paired| (*peer, paired.clone())))
                .collect();
            let removal = &mut db.removals[index];
            if removal.local_applied {
                let finished = removal.vaults.clone();
                removal.done.extend(finished);
                removal.local_applied = false;
            }
            removal.vaults.extend(added);
            for (peer, shared) in relayed {
                removal.relayed.entry(peer).or_default().extend(shared);
            }
            for (peer, paired) in relays {
                removal.delivered.remove(&peer);
                removal.pending.insert(peer, paired);
            }
            return self.save(&db);
        }
        // Only the computer that decided the removal delivers it. A receiver,
        // including the removed computer, applies the decision and does not
        // send RemoveComputer onward: peers reject that, and a leaf would
        // wait forever for an acknowledgement before pausing (#2042, #3079).
        // One exception: when this computer shared the vault onward to the
        // removed computer itself, it is that computer's vault owner and the
        // only route for the decision, so it relays it there (#3105).
        let relayed = sender
            .map(|sender| onward_to_removed(&db, sender, device, &vaults))
            .unwrap_or_default();
        let pending = if sender.is_some() {
            relayed
                .keys()
                .filter_map(|peer| db.peers.get(peer).map(|paired| (*peer, paired.clone())))
                .collect()
        } else {
            db.peers
                .iter()
                .filter(|(peer, _)| vaults.iter().any(|v| knows(&db, **peer, v)))
                .map(|(id, peer)| (*id, peer.clone()))
                .collect()
        };
        // A removal decided here takes an epoch above everything known here,
        // so a concurrent readmission elsewhere can never close it (#3079).
        let current = admitted(&db, device);
        let epoch = if sender.is_none() {
            Some(next_epoch(&db, device)?)
        } else {
            epoch
        };
        let older = stale_for(&db, device, epoch.as_ref(), &current, Some(&vaults), false);
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
            relayed,
            done: BTreeSet::new(),
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
        ensure!(
            removal_from_owner(&self.shared.db.lock().unwrap(), scope, &vaults),
            "Only the vault owner can remove a computer from this vault"
        );
        // The device's epoch on the sender may be newer than here: learn it
        // first. A removal from an older epoch is closed, whether unseen or
        // journalled (also mid-application), and never enters its effects.
        // Waiting for the effect in flight means none starts after this close.
        ensure!(
            version.admission.as_ref().is_none_or(Epoch::valid)
                && version.epoch.as_ref().is_none_or(Epoch::valid),
            "Invalid removal epoch"
        );
        if let Some(admission) = &version.admission {
            // A learned admission can be a few cycles ahead, not an arbitrary
            // jump that would leave the device unremovable; refuse it so the
            // sender retries with a real one (#3079).
            let db = self.shared.db.lock().unwrap();
            ensure!(
                admission.counter <= high_water(&db, &device).saturating_add(64)
                    && admission.counter.saturating_add(1) < MAX_EPOCH,
                "Invalid removal epoch"
            );
        }
        // The removed computer, through any pairing it kept, cannot close its
        // own removal or assert a newer admission of itself: that needs a
        // fresh approval from a computer that still syncs the vault (#3079).
        // Neither can an unidentified, since-revoked pairing.
        // A since-revoked pairing (any device's) only replays what it is
        // recorded for: closing and admission changes need a live pairing.
        let from_removed = {
            let db = self.shared.db.lock().unwrap();
            match db.peers.get(&scope) {
                Some(peer) => peer.syncthing_device.as_deref() == Some(device.as_str()),
                None => true,
            }
        };
        let (learn, stale) = {
            let db = self.shared.db.lock().unwrap();
            let stored_behind = version.admission.as_ref().is_some_and(|admission| {
                vaults.iter().any(|vault| {
                    db.admission_vaults
                        .get(&device)
                        .and_then(|per| per.get(vault))
                        .copied()
                        .unwrap_or(0)
                        < admission.counter
                })
            });
            // The horizon is only the admission the sender asserted for the
            // vaults in this message. The device-wide maximum belongs to
            // other owners and must not make this removal stale (#3079).
            let asserted = version.admission.clone().unwrap_or_default();
            (
                stored_behind,
                version.superseded
                    || stale_for(
                        &db,
                        &device,
                        version.epoch.as_ref(),
                        &asserted,
                        Some(&vaults),
                        version.admission.is_some(),
                    ),
            )
        };
        if (learn || stale) && !from_removed {
            let _effects = self.shared.removal_effects.lock().unwrap();
            let mut db = self.shared.db.lock().unwrap();
            ensure!(valid(&device) && !id.is_nil(), "Invalid removal identity");
            if let Some(known) = db.removals.iter().find(|r| r.id == id) {
                ensure!(
                    known.device == device,
                    "Removal identity conflicts with its durable request"
                );
                // Closing needs the owner of everything the record covers.
                // Learning an admission touches only this delivery's vaults,
                // checked above; the record may hold another owner's subset
                // of the same decision (#3195).
                if stale && !known.vaults.is_empty() {
                    ensure!(
                        removal_from_owner(&db, scope, &known.vaults),
                        "Only the vault owner can remove a computer from this vault"
                    );
                }
                if known.superseded() && !learn {
                    // Already closed here: a replay changes nothing. An open
                    // delivery means the sender still holds it open, so the
                    // closed version is queued to it before acknowledging.
                    if !version.superseded {
                        queue_closed_to(&mut db, id, scope);
                        self.save(&db)?;
                    }
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
            // Persist the newer epoch for these vaults only, before any
            // effect. Another owner's vault is not closed (#3079).
            if let Some(admission) = &version.admission {
                admit_for(&mut db, &device, admission, &vaults);
            }
            if !stale {
                // A current removal: it applies below, under the learned epoch.
                self.save(&db)?;
            } else {
                let room = db.removals.len() < MAX_TOMBSTONES;
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
                    // An unknown closed removal is remembered while there is
                    // tombstone room; the learned epoch closes it regardless.
                    None if room => {
                        let mut tombstone = Removal::tombstone(id, &device, scope);
                        tombstone.epoch = version.epoch.clone();
                        tombstone.vaults = authorized_vaults(&db, scope, &vaults);
                        db.removals.push(tombstone);
                    }
                    None => {}
                }
                if !version.superseded {
                    queue_closed_to(&mut db, id, scope);
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
        // Vaults named since the preimages were captured: a further subset of
        // the same decision from another owner (#3195).
        let captured: BTreeSet<String> = removal
            .folders
            .iter()
            .flatten()
            .map(|f| f.vault.folder_id.clone())
            .collect();
        let uncaptured = removal
            .folders
            .as_ref()
            .is_some_and(|_| removal.vaults.iter().any(|v| !captured.contains(v)));
        if removal.folders.is_none() || uncaptured {
            let mut folders = removal.folders.clone().unwrap_or_default();
            let mut mesh = removal.mesh.clone().unwrap_or_default();
            for vault in attached.into_iter().filter(|v| {
                removal.vaults.contains(&v.folder_id) && !captured.contains(&v.folder_id)
            }) {
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
                // A further subset needs the introducer's mesh for its own
                // vault, not only the one captured for the first (#3195).
                if needs_handoff
                    && removal
                        .mesh
                        .as_ref()
                        .is_none_or(|m| uncaptured && !m.contains_key(&fresh.folder_id))
                {
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
            // Finished under an earlier subset of this decision; the
            // revocations since change its view, and it needs nothing (#3195).
            if removal.done.contains(&folder.vault.folder_id) {
                continue;
            }
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
    /// Vaults that currently contain `device`, grouped by who decides them.
    /// `None` is this computer. A paired id is the computer the vault was
    /// received from (#3079).
    fn removal_scope(&self, device: &str) -> Result<BTreeMap<Option<Uuid>, BTreeSet<String>>> {
        let attached = self.shared.db.lock().unwrap().attached.clone();
        let configs = self.configuration_paths()?;
        let mut scope: BTreeMap<Option<Uuid>, BTreeSet<String>> = BTreeMap::new();
        for vault in attached {
            let fresh = inspect_existing(&configs, &vault.path)?;
            ensure!(
                existing::same_binding(&vault, &fresh),
                "Removal vault identity changed"
            );
            if fresh.peers.iter().any(|peer| peer == device) || fresh.daemon.device_id == device {
                let db = self.shared.db.lock().unwrap();
                scope
                    .entry(foreign_owner(&db, &fresh.folder_id))
                    .or_default()
                    .insert(fresh.folder_id);
            }
        }
        Ok(scope)
    }
    fn queue_request(&self, device: &str, vaults: BTreeSet<String>, owner: Uuid) -> Result<()> {
        let mut db = self.shared.db.lock().unwrap();
        ensure!(
            db.peers.contains_key(&owner),
            "Vault owner is not paired on this computer"
        );
        if db
            .removal_requests
            .iter()
            .any(|request| request.device == device && request.owner == owner && !request.accepted)
        {
            return Ok(());
        }
        ensure!(
            db.removal_requests.len() < 256,
            "Removal history limit reached; existing records were kept"
        );
        db.removal_requests.push(RemovalRequest {
            id: Uuid::new_v4(),
            device: device.to_owned(),
            vaults,
            owner,
            accepted: false,
        });
        self.save(&db)
    }
    /// The vault owner turns a non-owner's request into its own removal.
    /// Removing this computer still runs the #2042 handoff (#3079).
    pub(super) fn accept_removal_request(
        &self,
        sender: Uuid,
        id: Uuid,
        device: String,
        vaults: BTreeSet<String>,
    ) -> Result<Reply> {
        ensure!(
            valid(&device) && !id.is_nil() && !vaults.is_empty() && vaults.len() <= 256,
            "Invalid removal request"
        );
        {
            let db = self.shared.db.lock().unwrap();
            ensure!(
                db.peers.contains_key(&sender)
                    && vaults
                        .iter()
                        .all(|vault| foreign_owner(&db, vault).is_none())
                    && vaults.iter().all(|vault| knows(&db, sender, vault)),
                "Only the vault owner decides removal"
            );
            if db.removals.iter().any(|removal| {
                removal.id == id
                    && removal.device == device
                    && (removal.local_applied || removal.superseded())
            }) {
                return Ok(Reply::RemovalRecorded { id });
            }
        }
        self.queue_removal(id, &device, vaults, None, None)?;
        self.prepare_removal(id)?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        self.apply_removal(id)?;
        Ok(Reply::RemovalRecorded { id })
    }
    fn deliver_removal_requests(&self) -> Result<()> {
        let (local, batch) = {
            let db = self.shared.db.lock().unwrap();
            let local = served(&db);
            let batch: Vec<(RemovalRequest, Paired)> = db
                .removal_requests
                .iter()
                .filter(|request| !request.accepted)
                .filter_map(|request| {
                    db.peers
                        .get(&request.owner)
                        .map(|peer| (request.clone(), peer.clone()))
                })
                .collect();
            (local, batch)
        };
        for (request, peer) in batch {
            if local.contains(&peer.endpoint) {
                continue;
            }
            let reply = self.call(
                peer.endpoint,
                request.owner,
                &peer.key,
                &peer.public,
                Operation::RequestRemoval {
                    id: request.id,
                    device: request.device.clone(),
                    vaults: request.vaults.clone(),
                },
            );
            if let Ok(Reply::Error { message }) = &reply {
                anyhow::bail!("{message}");
            }
            if matches!(reply, Ok(Reply::RemovalRecorded { .. })) {
                let mut db = self.shared.db.lock().unwrap();
                if let Some(saved) = db
                    .removal_requests
                    .iter_mut()
                    .find(|saved| saved.id == request.id)
                {
                    saved.accepted = true;
                }
                self.save(&db)?;
            }
        }
        Ok(())
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
                let Some((superseded, local_applied, prepared, epoch, vaults, admission)) = ({
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
                                r.vaults_for(id),
                                known_admission_for(&db, &r.device, &r.vaults_for(id)),
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
                        vaults,
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
        // After this round's pull, so a request the owner just accepted is
        // not acknowledged until the next refresh. The handoff snapshot
        // still shows every receiver pending (#2042).
        self.deliver_removal_requests()
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
    // The removed device loses only the vaults this removal names. A pairing
    // or grant that also carries another vault keeps that authority (#3079).
    let affected: Vec<Uuid> = db
        .peers
        .iter()
        .filter(|(id, peer)| {
            let named =
                !removed_self && peer.syncthing_device.as_deref() == Some(removal.device.as_str());
            let departing = removed_self && carries(db, **id, peer, &removal.vaults);
            named || departing
        })
        .map(|(id, _)| *id)
        .collect();
    // A pairing that carries only the removed vault is revoked. One that also
    // carries another vault keeps that authority. Decide before stripping.
    let revoked: BTreeSet<_> = affected
        .iter()
        .copied()
        .filter(|id| {
            db.peers.get(id).is_some_and(|peer| {
                carries(db, *id, peer, &removal.vaults)
                    && !carries_other(db, *id, peer, &removal.vaults)
            })
        })
        .collect();
    for id in &affected {
        if let Some(peer) = db.peers.get_mut(id) {
            peer.allowed.retain(|vault| !removal.vaults.contains(vault));
        }
    }
    db.share_grants
        .retain(|grant| !(affected.contains(&grant.peer) && removal.vaults.contains(&grant.vault)));
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
            .filter(|t| removal.vaults.contains(t.vault()))
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
                let named = peer.syncthing_device.as_deref() == Some(removal.device.as_str())
                    || db
                        .attached
                        .iter()
                        .any(|vault| vault.daemon.device_id == removal.device);
                named
                    && carries(db, peer.peer.id, peer, &removal.vaults)
                    && !carries_other(db, peer.peer.id, peer, &removal.vaults)
            })
}

pub(super) fn pending(db: &Database) -> usize {
    db.removals
        .iter()
        .filter(|r| !r.local_applied || !r.pending.is_empty())
        .count()
        + db.removal_requests
            .iter()
            .filter(|request| !request.accepted)
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
            relayed: BTreeMap::new(),
            done: BTreeSet::new(),
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
            readmit(&mut db, &device, id, &[], 0).is_err(),
            "an unfinished removal must finish before approval"
        );
        db.removals.pop();
        readmit(&mut db, &device, id, &[unseen], 0)?;
        assert!(
            peer_is_live(&db, &returning),
            "readmitted device gets addresses again"
        );
        let mut closed: Vec<_> = readmitted_by(&db, id).into_iter().map(|c| c.id).collect();
        closed.sort();
        let mut expected = vec![old];
        expected.sort();
        assert_eq!(
            closed, expected,
            "an unseen id with no owned scope is not closed"
        );
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
        readmit(&mut db, &device, Uuid::new_v4(), &[], 0)?;
        assert_eq!(readmitted_by(&db, id).len(), 1);
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
                &original.public,
                &closing(&[id]),
                None,
            )?;
            assert!(!db.local_removed);
            assert!(
                db.revoked_peers.contains_key(&original.peer.id),
                "the owner's readmission retires the pairing that only carried the removed vault (applied before upgrade: {applied_before_upgrade})"
            );
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
            readmit(&mut db, &removed, Uuid::new_v4(), &[], 0)?;
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
        readmit(&mut db, &removed, Uuid::new_v4(), &[], 0)?;
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
            &original.public,
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
            &original.public,
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
        readmit(&mut db, &device, Uuid::new_v4(), &known, 0)?;
        assert_eq!(db.removals.len(), 1000);
        let pairing_id = Uuid::new_v4();
        accept_readmission(&mut db, &device, pairing_id, &[], &closing(&known), None)?;
        assert_eq!(db.removals.len(), 1000);
        let fresh: Vec<Uuid> = (0..25).map(|_| Uuid::new_v4()).collect();
        assert!(
            readmit(&mut db, &device, Uuid::new_v4(), &fresh, 0).is_err(),
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
        readmit(&mut db, &removed, Uuid::new_v4(), &[], 0)?;
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
        // A removal from the previous admission closes on the new one; a
        // removal minted concurrently with the same counter does not.
        let mut older = removal(Uuid::new_v4(), &device);
        older.epoch = Some(Epoch::default());
        let id = older.id;
        db.removals.push(older);
        let mut concurrent = removal(Uuid::new_v4(), &device);
        concurrent.epoch = Some(epoch(1, "AAAA"));
        let concurrent_id = concurrent.id;
        db.removals.push(concurrent);
        admit(&mut db, &device, &epoch(1, "CCCC"));
        assert!(db
            .removals
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .superseded());
        assert!(!db
            .removals
            .iter()
            .find(|r| r.id == concurrent_id)
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
        }
        // Alice removes Bob; Carol readmits him concurrently. Both mint
        // counter 1, and Carol's device sorts after Alice's (empty here).
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
        let carol = epoch(1, "CCCC");
        assert!(
            mine.counter == carol.counter && mine < carol,
            "control: the same-counter pair with the readmitter sorting higher"
        );
        admit(&mut db, &bob, &carol);
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

    /// Round 7: a removal the returning computer reports that the approver
    /// never saw is closed for the vaults shared here and delivered to their
    /// peers; an open delivery of it later is answered by queueing the closed
    /// version back to its sender.
    #[test]
    fn issue3079_unseen_removal_closed_at_approval_reaches_its_originator() -> Result<()> {
        let root = tempfile::tempdir()?;
        let approver = Controller::open(root.path().join("approver"), vec![])?;
        let bob = ["BBBBBBB"; 8].join("-");
        let originator = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let unseen = Uuid::new_v4();
        let mut db = approver.shared.db.lock().unwrap();
        db.attached
            .push(existing_vault(&["CCCCCCC"; 8].join("-"), "vault")?);
        db.peers.insert(originator.peer.id, originator.clone());
        readmit_with_vaults(
            &mut db,
            &bob,
            Uuid::new_v4(),
            &[unseen],
            &BTreeMap::from([(unseen, BTreeSet::from(["vault".into()]))]),
            0,
        )?;
        let record = db.removals.iter().find(|r| r.id == unseen).unwrap();
        assert!(record.superseded() && record.vaults.contains("vault"));
        assert!(
            record.pending.contains_key(&originator.peer.id),
            "the closed version reaches nobody"
        );
        // The originator delivered it, then acknowledged the closed version
        // was not yet sent: simulate it still pending elsewhere.
        db.removals
            .iter_mut()
            .find(|r| r.id == unseen)
            .unwrap()
            .pending
            .clear();
        drop(db);
        let reply = approver.receive_removal(
            originator.peer.id,
            unseen,
            bob.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version {
                superseded: false,
                epoch: Some(Epoch::default()),
                admission: Some(Epoch::default()),
            },
        )?;
        assert!(matches!(reply, Reply::RemovalRecorded { id } if id == unseen));
        let db = approver.shared.db.lock().unwrap();
        let record = db.removals.iter().find(|r| r.id == unseen).unwrap();
        assert!(
            record.pending.contains_key(&originator.peer.id),
            "an open delivery of a closed removal was acknowledged without sending the close back"
        );
        let back = requests(&db, originator.peer.id);
        assert!(back
            .iter()
            .any(|r| r.superseded && r.vaults.contains("vault")));
        drop(db);
        approver.shutdown();
        Ok(())
    }
    /// Review 83043: over the signed paired channel, a removed computer that
    /// replays its own removal as closed with its revoked credentials closes
    /// nothing: the removal stays open, catalog access stays denied, and
    /// refresh restores no pairing.
    #[test]
    fn issue3079_removed_computer_cannot_close_its_own_removal_with_revoked_credentials(
    ) -> Result<()> {
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
            second.catalog(peer.peer.id).is_ok(),
            "positive channel control"
        );
        let removed = ["BBBBBBB"; 8].join("-");
        let id = Uuid::new_v4();
        {
            // First removed Second (its Syncthing device `removed`) from "vault".
            let mut db = first.shared.db.lock().unwrap();
            let mut paired = db.peers.remove(&peer.peer.id).unwrap();
            paired.syncthing_device = Some(removed.clone());
            paired.allowed = vec!["vault".into()];
            db.revoked_peers.insert(peer.peer.id, paired.clone());
            let mut applied = removal(id, &removed);
            applied.undo = Some(Undo {
                pairings: BTreeMap::from([(peer.peer.id, paired)]),
                ..Undo::default()
            });
            db.removals.push(applied);
            first.save(&db)?;
        }
        let reply = second.call(
            peer.endpoint,
            peer.peer.id,
            &peer.key,
            &peer.public,
            Operation::RemoveComputer {
                id,
                device: removed.clone(),
                vaults: BTreeSet::from(["vault".into()]),
                superseded: true,
                epoch: None,
                admission: None,
            },
        );
        // Answered or refused, nothing may close.
        let _ = reply;
        {
            let db = first.shared.db.lock().unwrap();
            let record = db.removals.iter().find(|r| r.id == id).unwrap();
            assert!(
                !record.superseded(),
                "the removed computer closed its own removal"
            );
            assert!(record.undo.as_ref().is_some_and(|u| !u.pending));
            assert!(
                !db.peers.contains_key(&peer.peer.id),
                "the removed computer restored its own pairing"
            );
        }
        assert!(
            second.catalog(peer.peer.id).is_err(),
            "catalog access came back"
        );
        first.shutdown();
        second.shutdown();
        Ok(())
    }
    /// Review 83138: a since-revoked pairing of another computer cannot close
    /// someone else's removal or raise its admission; the removal stays open
    /// and the removed computer's pairing is not restored.
    #[test]
    fn issue3079_revoked_pairing_cannot_close_another_computers_removal() -> Result<()> {
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
        let removed = ["BBBBBBB"; 8].join("-");
        let removed_pairing = pairing(&removed, wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        {
            let mut db = first.shared.db.lock().unwrap();
            // Second's own pairing was revoked for an unrelated reason; it is
            // a different device than the one removed.
            let mut sender = db.peers.remove(&peer.peer.id).unwrap();
            sender.syncthing_device = Some(["EEEEEEE"; 8].join("-"));
            sender.allowed = vec!["vault".into()];
            db.revoked_peers.insert(peer.peer.id, sender);
            db.revoked_peers
                .insert(removed_pairing.peer.id, removed_pairing.clone());
            let mut applied = removal(id, &removed);
            applied.undo = Some(Undo {
                pairings: BTreeMap::from([(removed_pairing.peer.id, removed_pairing.clone())]),
                ..Undo::default()
            });
            db.removals.push(applied);
            first.save(&db)?;
        }
        for version in [(true, None), (false, Some(epoch(9, "EEEE")))] {
            let _ = second.call(
                peer.endpoint,
                peer.peer.id,
                &peer.key,
                &peer.public,
                Operation::RemoveComputer {
                    id,
                    device: removed.clone(),
                    vaults: BTreeSet::from(["vault".into()]),
                    superseded: version.0,
                    epoch: None,
                    admission: version.1,
                },
            );
        }
        let db = first.shared.db.lock().unwrap();
        let record = db.removals.iter().find(|r| r.id == id).unwrap();
        assert!(
            !record.superseded(),
            "a revoked pairing closed another computer's removal"
        );
        assert!(record.undo.as_ref().is_some_and(|u| !u.pending));
        assert_eq!(
            admitted(&db, &removed),
            Epoch::default(),
            "a revoked pairing raised the admission"
        );
        assert!(!db.peers.contains_key(&removed_pairing.peer.id));
        drop(db);
        first.shutdown();
        second.shutdown();
        Ok(())
    }
    /// A pairing request cannot claim a history far above this computer's.
    /// The mark does not raise the owner's counter (#3079).
    #[test]
    fn issue3079_high_water_cannot_exhaust_the_epoch_range() -> Result<()> {
        let root = tempfile::tempdir()?;
        let carol = Controller::open(root.path().join("carol"), vec![])?;
        let bob = ["BBBBBBB"; 8].join("-");
        let mut db = carol.shared.db.lock().unwrap();
        let known = vec![Uuid::new_v4()];
        assert!(readmit(&mut db, &bob, Uuid::new_v4(), &known, MAX_EPOCH - 2).is_err());
        assert!(readmit(&mut db, &bob, Uuid::new_v4(), &known, 65).is_err());
        assert!(
            db.removals.is_empty() && db.admissions.is_empty(),
            "a refused request changed state"
        );
        readmit(&mut db, &bob, Uuid::new_v4(), &known, 64)?;
        assert_eq!(
            admitted(&db, &bob).counter,
            1,
            "the returning computer's mark does not raise the owner's counter"
        );
        db.admissions.insert(bob.clone(), epoch(MAX_EPOCH - 1, ""));
        assert!(
            next_epoch(&db, &bob).is_err(),
            "a minted counter would be refused by peers"
        );
        db.admissions.insert(bob.clone(), epoch(MAX_EPOCH - 2, ""));
        assert!(next_epoch(&db, &bob)?.valid());
        drop(db);
        carol.shutdown();
        Ok(())
    }
    /// Review 83138: tombstones of unknown closed removals stop well below the
    /// journal limit, so a real removal still fits.
    #[test]
    fn issue3079_unknown_tombstones_leave_room_for_real_removals() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        {
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            // A paired device, so a removal of it can be decided here.
            let target = pairing(&removed, wire::public(&wire::random()?)?)?;
            db.peers.insert(target.peer.id, target);
            for _ in 0..MAX_TOMBSTONES {
                let mut closed = removal(Uuid::new_v4(), &["CCCCCCC"; 8].join("-"));
                closed.readmitted = Some(Uuid::nil());
                db.removals.push(closed);
            }
        }
        let unknown = Uuid::new_v4();
        third.receive_removal(
            sender.peer.id,
            unknown,
            removed.clone(),
            BTreeSet::from(["vault".into()]),
            None,
            Version {
                superseded: true,
                ..Version::default()
            },
        )?;
        {
            let mut db = third.shared.db.lock().unwrap();
            assert_eq!(
                db.removals.len(),
                MAX_TOMBSTONES,
                "an unknown tombstone was stored past the line"
            );
            assert!(
                readmit(&mut db, &removed, Uuid::new_v4(), &[Uuid::new_v4()], 0).is_err(),
                "an approval would cross the tombstone line"
            );
        }
        // An open removal decided here still fits.
        third.queue_removal(Uuid::new_v4(), &removed, BTreeSet::new(), None, None)?;
        assert_eq!(
            third.shared.db.lock().unwrap().removals.len(),
            MAX_TOMBSTONES + 1
        );
        third.shutdown();
        Ok(())
    }

    /// Review 83236: an unseen removal is tombstoned only for the vaults the
    /// returning computer says it covered and this computer shares, and the
    /// returning computer keeps the vaults it recorded itself.
    #[test]
    fn issue3079_unseen_removal_vaults_are_never_widened() -> Result<()> {
        let root = tempfile::tempdir()?;
        let approver = Controller::open(root.path().join("approver"), vec![])?;
        let own = ["CCCCCCC"; 8].join("-");
        let bob = ["BBBBBBB"; 8].join("-");
        let unseen = Uuid::new_v4();
        let pairing_id = Uuid::new_v4();
        {
            let mut db = approver.shared.db.lock().unwrap();
            db.attached.push(existing_vault(&own, "v1")?);
            db.attached.push(existing_vault(&own, "v2")?);
            readmit_with_vaults(
                &mut db,
                &bob,
                pairing_id,
                &[unseen],
                &BTreeMap::from([(unseen, BTreeSet::from(["v1".into(), "v9".into()]))]),
                0,
            )?;
            assert!(
                db.removals.iter().all(|r| r.id != unseen),
                "a scope this computer does not fully own is not closed"
            );
            readmit_with_vaults(
                &mut db,
                &bob,
                pairing_id,
                &[unseen],
                &BTreeMap::from([(unseen, BTreeSet::from(["v1".into()]))]),
                0,
            )?;
            let record = db.removals.iter().find(|r| r.id == unseen).unwrap();
            assert_eq!(record.vaults, BTreeSet::from(["v1".into()]));
            let other = Uuid::new_v4();
            readmit(&mut db, &bob, Uuid::new_v4(), &[other], 0)?;
            assert!(
                db.removals.iter().all(|r| r.id != other),
                "an unknown scope grants nothing"
            );
        }
        approver.shutdown();
        let returning = Controller::open(root.path().join("returning"), vec![])?;
        let mut db = returning.shared.db.lock().unwrap();
        let mut own_removal = removal(unseen, &bob);
        own_removal.vaults = BTreeSet::from(["v1".into()]);
        db.removals.push(own_removal);
        let inviter = pairing(&own, wire::public(&wire::random()?)?)?;
        db.peers.insert(pairing_id, inviter.clone());
        accept_readmission(
            &mut db,
            &bob,
            pairing_id,
            &inviter.public,
            &[Closed {
                id: unseen,
                vaults: BTreeSet::from(["v1".into(), "v2".into()]),
            }],
            Some(epoch(1, "CCCC")),
        )?;
        assert_eq!(
            db.removals.iter().find(|r| r.id == unseen).unwrap().vaults,
            BTreeSet::from(["v1".into()]),
            "the approval widened the returning computer's own removal"
        );
        assert!(!released_by_readmission(&db, &bob, Uuid::new_v4(), "v2"));
        drop(db);
        returning.shutdown();
        Ok(())
    }
    /// Review 83236: a delivered admission far ahead of this computer's history
    /// or at the end of the range is refused before anything closes.
    #[test]
    fn issue3079_learned_admission_is_bounded() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let removed = ["BBBBBBB"; 8].join("-");
        let sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        {
            let mut db = third.shared.db.lock().unwrap();
            db.peers.insert(sender.peer.id, sender.clone());
            db.removals.push(removal(id, &removed));
        }
        for counter in [65, MAX_EPOCH - 1] {
            assert!(third
                .receive_removal(
                    sender.peer.id,
                    id,
                    removed.clone(),
                    BTreeSet::from(["vault".into()]),
                    None,
                    Version {
                        superseded: true,
                        epoch: Some(Epoch::default()),
                        admission: Some(epoch(counter, "AAAA")),
                    },
                )
                .is_err());
        }
        {
            let db = third.shared.db.lock().unwrap();
            assert_eq!(admitted(&db, &removed), Epoch::default());
            assert!(!db
                .removals
                .iter()
                .find(|r| r.id == id)
                .unwrap()
                .superseded());
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
                admission: Some(epoch(64, "AAAA")),
            },
        )?;
        assert_eq!(
            admitted(&third.shared.db.lock().unwrap(), &removed).counter,
            64
        );
        third.shutdown();
        Ok(())
    }

    /// Review 83320: a live peer's open delivery of a closed removal with an
    /// extra vault leaves its vaults unchanged, and a new tombstone records
    /// only vaults associated here that the sender is authorized for.
    #[test]
    fn issue3079_deliveries_never_widen_removal_vaults() -> Result<()> {
        let root = tempfile::tempdir()?;
        let third = Controller::open(root.path().join("third"), vec![])?;
        let own = ["CCCCCCC"; 8].join("-");
        let removed = ["BBBBBBB"; 8].join("-");
        let mut sender = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        sender.allowed = vec!["v1".into()];
        let (closed, unknown) = (Uuid::new_v4(), Uuid::new_v4());
        {
            let mut db = third.shared.db.lock().unwrap();
            db.attached.push(existing_vault(&own, "v1")?);
            db.attached.push(existing_vault(&own, "v2")?);
            db.peers.insert(sender.peer.id, sender.clone());
            let mut record = removal(closed, &removed);
            record.vaults = BTreeSet::from(["v1".into()]);
            record.readmitted = Some(Uuid::nil());
            db.removals.push(record);
        }
        let widened = BTreeSet::from(["v1".into(), "v2".into(), "v9".into()]);
        third.receive_removal(
            sender.peer.id,
            closed,
            removed.clone(),
            widened.clone(),
            None,
            Version::default(),
        )?;
        third.receive_removal(
            sender.peer.id,
            unknown,
            removed.clone(),
            widened,
            None,
            Version {
                superseded: true,
                ..Version::default()
            },
        )?;
        let db = third.shared.db.lock().unwrap();
        let record = db.removals.iter().find(|r| r.id == closed).unwrap();
        assert_eq!(
            record.vaults,
            BTreeSet::from(["v1".into()]),
            "a delivery widened a closed removal"
        );
        assert!(record.pending.contains_key(&sender.peer.id));
        let tombstone = db.removals.iter().find(|r| r.id == unknown).unwrap();
        assert_eq!(
            tombstone.vaults,
            BTreeSet::from(["v1".into()]),
            "a tombstone recorded vaults the sender is not authorized for or that are not here"
        );
        drop(db);
        third.shutdown();
        Ok(())
    }
    /// Review 83320: an approval's admission far ahead of this computer's
    /// history is refused before anything changes; one within 64 readmits.
    #[test]
    fn issue3079_approval_admission_is_bounded() -> Result<()> {
        let root = tempfile::tempdir()?;
        let returning = Controller::open(root.path().join("returning"), vec![])?;
        let own = ["BBBBBBB"; 8].join("-");
        let inviter = pairing(&["CCCCCCC"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let id = Uuid::new_v4();
        let mut db = returning.shared.db.lock().unwrap();
        db.removals.push(removal(id, &own));
        db.local_removed = true;
        db.peers.insert(inviter.peer.id, inviter.clone());
        for counter in [65, MAX_EPOCH - 1] {
            assert!(accept_readmission(
                &mut db,
                &own,
                inviter.peer.id,
                &inviter.public,
                &closing(&[id]),
                Some(epoch(counter, "CCCC")),
            )
            .is_err());
            assert!(db.local_removed);
            assert!(!db
                .removals
                .iter()
                .find(|r| r.id == id)
                .unwrap()
                .superseded());
            assert_eq!(admitted(&db, &own), Epoch::default());
        }
        assert!(!admission_in_range(&db, &own, &epoch(65, "CCCC")));
        // A plain pairing with an out-of-range admission learns nothing.
        accept_readmission(
            &mut db,
            &own,
            inviter.peer.id,
            &inviter.public,
            &[],
            Some(epoch(500, "CCCC")),
        )?;
        assert_eq!(admitted(&db, &own), Epoch::default());
        accept_readmission(
            &mut db,
            &own,
            inviter.peer.id,
            &inviter.public,
            &closing(&[id]),
            Some(epoch(64, "CCCC")),
        )?;
        assert!(!db.local_removed, "an approval within range must readmit");
        drop(db);
        returning.shutdown();
        Ok(())
    }

    /// Review 83358: before any readmission no admission goes on the wire,
    /// so peers on the previous version (which refuse unknown fields) accept
    /// removal deliveries and approvals unchanged.
    #[test]
    fn issue3079_default_admission_is_not_sent() -> Result<()> {
        let root = tempfile::tempdir()?;
        let controller = Controller::open(root.path().join("wire"), vec![])?;
        let device = ["BBBBBBB"; 8].join("-");
        let receiver = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let mut db = controller.shared.db.lock().unwrap();
        let mut legacy = removal(Uuid::new_v4(), &device);
        legacy.epoch = None;
        legacy.pending.insert(receiver.peer.id, receiver.clone());
        db.removals.push(legacy);
        assert_eq!(known_admission(&db, &device), None);
        let pulled = serde_json::to_value(requests(&db, receiver.peer.id))?;
        assert!(pulled[0].get("admission").is_none() && pulled[0].get("epoch").is_none());
        let pushed = serde_json::to_value(Operation::RemoveComputer {
            id: Uuid::new_v4(),
            device: device.clone(),
            vaults: BTreeSet::new(),
            superseded: false,
            epoch: None,
            admission: known_admission(&db, &device),
        })?;
        assert!(pushed.get("admission").is_none() && pushed.get("epoch").is_none());
        db.admissions.insert(device.clone(), epoch(1, "CCCC"));
        assert_eq!(known_admission(&db, &device), Some(epoch(1, "CCCC")));
        drop(db);
        controller.shutdown();
        Ok(())
    }

    /// Two owners, two counters. A's new removal of X still applies after C's
    /// higher admission of Y, and the wire names X's own counter.
    #[test]
    fn issue3079_new_removal_of_x_survives_a_higher_admission_of_y() -> Result<()> {
        let root = tempfile::tempdir()?;
        let computer = Controller::open(root.path().join("b"), vec![])?;
        let bob = ["BBBBBBB"; 8].join("-");
        let mut db = computer.shared.db.lock().unwrap();
        db.admissions.insert(bob.clone(), epoch(4, "CCCC"));
        db.admission_vaults.insert(
            bob.clone(),
            BTreeMap::from([("x".into(), 2u64), ("y".into(), 4)]),
        );
        let x = BTreeSet::from(["x".into()]);
        assert!(
            !stale_for(
                &db,
                &bob,
                Some(&epoch(3, "AAAA")),
                &epoch(2, "AAAA"),
                Some(&x),
                true
            ),
            "Y's admission suppressed a new removal of X"
        );
        assert_eq!(
            known_admission_for(&db, &bob, &x).map(|epoch| epoch.counter),
            Some(2)
        );
        assert_eq!(
            known_admission_for(&db, &bob, &BTreeSet::from(["y".into()]))
                .map(|epoch| epoch.counter),
            Some(4)
        );
        drop(db);
        computer.shutdown();
        Ok(())
    }

    /// Readmission into X does not close or advertise Y's removal.
    #[test]
    fn issue3079_readmission_into_x_leaves_ys_removal_open() -> Result<()> {
        let root = tempfile::tempdir()?;
        let approver = Controller::open(root.path().join("a"), vec![])?;
        let bob = ["BBBBBBB"; 8].join("-");
        let (x_id, y_id) = (Uuid::new_v4(), Uuid::new_v4());
        let pairing_id = Uuid::new_v4();
        {
            let mut db = approver.shared.db.lock().unwrap();
            db.attached
                .push(existing_vault(&["AAAAAAA"; 8].join("-"), "x")?);
            readmit_with_vaults(
                &mut db,
                &bob,
                pairing_id,
                &[x_id, y_id],
                &BTreeMap::from([
                    (x_id, BTreeSet::from(["x".into()])),
                    (y_id, BTreeSet::from(["y".into()])),
                ]),
                0,
            )?;
            assert!(db.removals.iter().any(|r| r.id == x_id && r.superseded()));
            assert!(db.removals.iter().all(|r| r.id != y_id));
            assert!(readmitted_by(&db, pairing_id).iter().all(|c| c.id != y_id));
        }
        approver.shutdown();
        let returning = Controller::open(root.path().join("b"), vec![])?;
        let mut db = returning.shared.db.lock().unwrap();
        let mut y_removal = removal(y_id, &bob);
        y_removal.vaults = BTreeSet::from(["y".into()]);
        db.removals.push(y_removal);
        let c_public = wire::public(&wire::random()?)?;
        let a_public = wire::public(&wire::random()?)?;
        let c = pairing(&["CCCCCCC"; 8].join("-"), c_public)?;
        db.peers.insert(c.peer.id, c.clone());
        db.transfers.push(received(c.peer.id, "y", &bob, true)?);
        accept_readmission(
            &mut db,
            &bob,
            pairing_id,
            &a_public,
            &[Closed {
                id: y_id,
                vaults: BTreeSet::from(["y".into()]),
            }],
            Some(epoch(1, "AAAA")),
        )?;
        assert!(
            !db.removals
                .iter()
                .find(|r| r.id == y_id)
                .unwrap()
                .superseded(),
            "A's approval closed C's removal of Y"
        );
        drop(db);
        returning.shutdown();
        Ok(())
    }

    /// The vault's recorded pairing wins over another live pairing whose id
    /// sorts first.
    #[test]
    fn issue3079_foreign_owner_keeps_the_transfer_pairing() -> Result<()> {
        let root = tempfile::tempdir()?;
        let computer = Controller::open(root.path().join("receiver"), vec![])?;
        let device = ["AAAAAAA"; 8].join("-");
        let local_id = Uuid::from_u128(1);
        let remote_id = Uuid::from_u128(2);
        let mut db = computer.shared.db.lock().unwrap();
        for id in [local_id, remote_id] {
            let public = wire::public(&wire::random()?)?;
            let mut peer = pairing(&device, public)?;
            peer.peer.id = id;
            db.peers.insert(id, peer);
        }
        db.transfers.push(received(remote_id, "x", &device, true)?);
        assert_eq!(foreign_owner(&db, "x"), Some(remote_id));
        drop(db);
        computer.shutdown();
        Ok(())
    }

    /// Removing B from X leaves C's pairing and grant for Y in place.
    #[test]
    fn issue3079_removal_of_x_keeps_ys_authority() -> Result<()> {
        let root = tempfile::tempdir()?;
        let computer = Controller::open(root.path().join("c"), vec![])?;
        let bob = ["BBBBBBB"; 8].join("-");
        let public = wire::public(&wire::random()?)?;
        let mut peer = pairing(&bob, public)?;
        peer.allowed = vec!["x".into(), "y".into()];
        let id = peer.peer.id;
        let mut db = computer.shared.db.lock().unwrap();
        db.peers.insert(id, peer);
        db.share_grants.push(transfer::ShareGrant::new(
            id,
            &existing_vault(&["CCCCCCC"; 8].join("-"), "y")?,
            "bob",
        ));
        let mut removal = removal(Uuid::new_v4(), &bob);
        removal.vaults = BTreeSet::from(["x".into()]);
        db.removals.push(removal.clone());
        deny(&mut db, &removal, false);
        assert!(db.peers.contains_key(&id), "the Y pairing was revoked");
        assert!(db
            .share_grants
            .iter()
            .any(|grant| grant.for_peer(id) && grant.vault == "y"));
        assert!(!db.peers[&id].allowed.iter().any(|vault| vault == "x"));
        assert!(db.peers[&id].allowed.iter().any(|vault| vault == "y"));
        assert!(peer_is_live(&db, &db.peers[&id]));
        drop(db);
        computer.shutdown();
        Ok(())
    }

    /// #3105: C received the vault from its owner A and shared it onward to
    /// B. A's decision to remove B reaches C, and C is B's only route, so C
    /// relays it to B. Nothing else is relayed.
    #[test]
    fn issue3105_received_removal_reaches_the_computer_shared_onward_to() -> Result<()> {
        let root = tempfile::tempdir()?;
        let computer = Controller::open(root.path().join("c"), vec![])?;
        let own = ["CCCCCCC"; 8].join("-");
        let bob = ["BBBBBBB"; 8].join("-");
        let dave = ["DDDDDDD"; 8].join("-");
        let owner = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let onward = pairing(&bob, wire::public(&wire::random()?)?)?;
        let other = pairing(&dave, wire::public(&wire::random()?)?)?;
        let (a, b, d) = (owner.peer.id, onward.peer.id, other.peer.id);
        let mut db = computer.shared.db.lock().unwrap();
        db.peers.insert(a, owner);
        db.peers.insert(b, onward);
        db.peers.insert(d, other);
        let vaults = BTreeSet::from(["vault".to_string()]);
        // No grant: a receiver relays nothing (the #3079 rule).
        assert!(onward_to_removed(&db, a, &bob, &vaults).is_empty());
        db.share_grants.push(transfer::ShareGrant::new(
            b,
            &existing_vault(&own, "vault")?,
            &bob,
        ));
        db.share_grants.push(transfer::ShareGrant::new(
            d,
            &existing_vault(&own, "vault")?,
            &dave,
        ));
        // Only the removed computer's own onward pairing gets the decision.
        let relayed = onward_to_removed(&db, a, &bob, &vaults);
        assert_eq!(relayed.keys().copied().collect::<Vec<_>>(), vec![b]);
        // Never back to the sender, never for another vault, and a removal
        // of a computer this one did not share with stays local.
        assert!(onward_to_removed(&db, b, &bob, &vaults).is_empty());
        assert!(onward_to_removed(&db, a, &bob, &BTreeSet::from(["other".into()])).is_empty());
        assert!(onward_to_removed(&db, a, &["EEEEEEE"; 8].join("-"), &vaults).is_empty());
        drop(db);
        computer.shutdown();
        Ok(())
    }

    /// #3105 review: C received V1 and V2 from A, and shared only V1 onward
    /// to B; B received V2 from D. A removes B from both. C relays only V1 to
    /// B, on the pull and the push path, so B accepts it as from V1's owner;
    /// B gets no V2 decision from C, which it would refuse as a whole.
    #[test]
    fn issue3105_relay_names_only_the_vaults_shared_onward() -> Result<()> {
        let root = tempfile::tempdir()?;
        let receiver = Controller::open(root.path().join("c"), vec![])?;
        let carol = ["CCCCCCC"; 8].join("-");
        let bob = ["BBBBBBB"; 8].join("-");
        let owner = pairing(&["AAAAAAA"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let mut onward = pairing(&bob, wire::public(&wire::random()?)?)?;
        // Approval exposes every attached vault in the catalog scope.
        onward.allowed = vec!["v1".into(), "v2".into()];
        let (a, b) = (owner.peer.id, onward.peer.id);
        let both = BTreeSet::from(["v1".to_string(), "v2".to_string()]);
        let v1 = BTreeSet::from(["v1".to_string()]);
        let mut db = receiver.shared.db.lock().unwrap();
        db.peers.insert(a, owner);
        db.peers.insert(b, onward.clone());
        db.share_grants.push(transfer::ShareGrant::new(
            b,
            &existing_vault(&carol, "v1")?,
            &bob,
        ));
        let relayed = onward_to_removed(&db, a, &bob, &both);
        assert_eq!(relayed, BTreeMap::from([(b, v1.clone())]));
        let mut decision = removal(Uuid::new_v4(), &bob);
        decision.vaults = both.clone();
        decision.pending.insert(b, onward);
        decision.relayed = relayed;
        // Push path payload and its admission scope.
        assert_eq!(decision.vaults_for(b), v1);
        assert_eq!(decision.vaults_for(Uuid::new_v4()), both);
        db.removals.push(decision);
        // Pull path payload.
        let pulled = requests(&db, b);
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].vaults, v1);
        drop(db);
        receiver.shutdown();

        // On B, C owns V1 and D owns V2: C's V1 decision is accepted, the
        // unscoped one would be refused for V2.
        let removed = Controller::open(root.path().join("b"), vec![])?;
        let via_c = pairing(&carol, wire::public(&wire::random()?)?)?;
        let via_d = pairing(&["DDDDDDD"; 8].join("-"), wire::public(&wire::random()?)?)?;
        let (c, d) = (via_c.peer.id, via_d.peer.id);
        let mut db = removed.shared.db.lock().unwrap();
        db.peers.insert(c, via_c);
        db.peers.insert(d, via_d);
        db.transfers.push(received(c, "v1", &bob, true)?);
        db.transfers.push(received(d, "v2", &bob, true)?);
        assert!(removal_from_owner(&db, c, &v1));
        assert!(!removal_from_owner(&db, c, &both));
        drop(db);
        removed.shutdown();
        Ok(())
    }

    /// Loopback Syncthing REST double for one computer's removal: inventory,
    /// folder reads and folder PATCHes, which it counts (#3195).
    #[cfg(target_os = "linux")]
    fn removal_rest(
        state: std::sync::Arc<std::sync::Mutex<serde_json::Value>>,
    ) -> Result<std::net::SocketAddr> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let _ = removal_rest_reply(&mut stream, &state);
            }
        });
        Ok(address)
    }
    #[cfg(target_os = "linux")]
    fn removal_rest_reply(
        stream: &mut std::net::TcpStream,
        state: &std::sync::Mutex<serde_json::Value>,
    ) -> Result<()> {
        use std::io::{BufRead, BufReader, Read, Write};
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            if header.trim().is_empty() {
                break;
            }
            if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse()?;
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default();
        let path = parts.next().unwrap_or_default();
        let mut state = state.lock().unwrap();
        let folder = path.strip_prefix("/rest/config/folders/").and_then(|id| {
            state["folders"]
                .as_array()
                .and_then(|all| all.iter().position(|f| f["id"] == id))
        });
        let reply = match (method, path, folder) {
            ("GET", "/rest/system/status", _) => Some(json!({"myID": state["me"].clone()})),
            ("GET", "/rest/system/version", _) => Some(json!({"version":"v2.1.6"})),
            ("GET", "/rest/config", _) => Some(
                json!({"folders": state["folders"].clone(), "devices": state["devices"].clone()}),
            ),
            ("GET", _, Some(index)) => Some(state["folders"][index].clone()),
            ("PATCH", _, Some(index)) => {
                let changes: serde_json::Value = serde_json::from_slice(&body)?;
                for (key, value) in changes.as_object().context("patch object")? {
                    state["folders"][index][key] = value.clone();
                }
                let id = state["folders"][index]["id"].clone();
                state["patches"].as_array_mut().unwrap().push(id);
                Some(serde_json::Value::Null)
            }
            _ => None,
        };
        let (status, body) = match reply {
            Some(serde_json::Value::Null) => ("200 OK", String::new()),
            Some(value) => ("200 OK", value.to_string()),
            None => ("404 Not Found", String::new()),
        };
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )?;
        Ok(())
    }

    /// #3195 (split from #3106 review 87342): A removes B from V1 and V2. B
    /// received V1 through C and V2 through D, so each relay is the owner of
    /// one vault on B and delivers only that vault, under the same removal
    /// id. B applies both subsets whichever arrives first, across a restart
    /// between them, keeps its files, and a replay of either changes nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn issue3195_one_removal_through_two_relays_applies_both_subsets() -> Result<()> {
        use sha2::{Digest, Sha256};
        let bob = ["BBBBBBB"; 8].join("-");
        let carol = ["CCCCCCC"; 8].join("-");
        let dave = ["DDDDDDD"; 8].join("-");
        // Restart: the old controller and its worker are gone first.
        let reopen = |old: Controller, state: &Path, configs: &[PathBuf]| -> Result<Controller> {
            let worker = std::sync::Arc::downgrade(&old.shared);
            old.shutdown();
            drop(old);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while worker.strong_count() != 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(worker.strong_count(), 0, "worker did not terminate");
            Controller::open(state.to_path_buf(), configs.to_vec())
        };
        for c_first in [true, false] {
            let root = tempfile::tempdir()?;
            let base = root.path().canonicalize()?;
            let home = base.join("syncthing");
            private::directory(&home)?;
            let (v1, v2) = (base.join("v1"), base.join("v2"));
            for (dir, name) in [(&v1, "v1"), (&v2, "v2")] {
                std::fs::create_dir(dir)?;
                std::fs::write(dir.join("keep.md"), format!("kept in {name}"))?;
            }
            let folder = |id: &str, path: &Path, relay: &str| {
                json!({"id": id, "label": id, "path": path, "type": "sendreceive",
                    "paused": false, "devices": [{"deviceID": bob}, {"deviceID": relay}]})
            };
            let rest = std::sync::Arc::new(std::sync::Mutex::new(json!({
                "me": bob,
                "folders": [folder("v1", &v1, &carol), folder("v2", &v2, &dave)],
                "devices": [
                    {"deviceID": bob, "addresses": ["dynamic"]},
                    {"deviceID": carol, "addresses": ["tcp://127.0.0.1:22001"]},
                    {"deviceID": dave, "addresses": ["tcp://127.0.0.1:22002"]},
                ],
                "patches": [],
            })));
            let address = removal_rest(rest.clone())?;
            let config = home.join("config.xml");
            private::write(&config, format!(r#"<configuration><gui tls="false"><address>{address}</address><apikey>fixture-key</apikey></gui></configuration>"#).as_bytes())?;
            private::write(&home.join("cert.pem"), b"fixture certificate")?;
            let configs = vec![config.canonicalize()?];
            let mut via_c = pairing(&carol, wire::public(&wire::random()?)?)?;
            via_c.allowed = vec!["v1".into()];
            let mut via_d = pairing(&dave, wire::public(&wire::random()?)?)?;
            via_d.allowed = vec!["v2".into()];
            let (c, d) = (via_c.peer.id, via_d.peer.id);
            let state = base.join("b");
            let removed = Controller::open(state.clone(), configs.clone())?;
            {
                let mut db = removed.shared.db.lock().unwrap();
                db.peers.insert(c, via_c);
                db.peers.insert(d, via_d);
                // B received V1 from C and V2 from D: each relay owns one.
                db.transfers.push(received(c, "v1", &bob, false)?);
                db.transfers.push(received(d, "v2", &bob, false)?);
                db.attached.push(inspect_existing(&configs, &v1)?);
                db.attached.push(inspect_existing(&configs, &v2)?);
                let identity = &db.attached[0].daemon;
                assert_eq!(
                    identity.certificate_hash,
                    format!("{:x}", Sha256::digest(b"fixture certificate"))
                );
                assert!(removal_from_owner(&db, c, &BTreeSet::from(["v1".into()])));
                assert!(removal_from_owner(&db, d, &BTreeSet::from(["v2".into()])));
                removed.save(&db)?;
            }
            let removed = reopen(removed, &state, &configs)?;
            let id = Uuid::new_v4();
            let (first, second) = if c_first {
                ((c, "v1"), (d, "v2"))
            } else {
                ((d, "v2"), (c, "v1"))
            };
            let paused = |vault: &str| {
                rest.lock().unwrap()["folders"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f["id"] == vault && f["paused"] == true)
            };
            let patches = || rest.lock().unwrap()["patches"].as_array().unwrap().len();
            let deliver = |removed: &Controller, (scope, vault): (Uuid, &str)| -> Result<()> {
                let reply = removed.receive_removal(
                    scope,
                    id,
                    bob.clone(),
                    BTreeSet::from([vault.to_string()]),
                    None,
                    Version::default(),
                )?;
                ensure!(
                    matches!(reply, Reply::RemovalRecorded { id: recorded } if recorded == id),
                    "delivery of {vault} was not recorded"
                );
                Ok(())
            };
            deliver(&removed, first)?;
            assert!(paused(first.1), "first subset applied");
            assert!(
                !paused(second.1),
                "the first relay's subset names one vault"
            );
            assert_eq!(patches(), 1);
            // Restart between the two subsets: the record is durable.
            let removed = reopen(removed, &state, &configs)?;
            deliver(&removed, second)?;
            assert!(
                paused(second.1),
                "the second relay's subset of the same removal was acknowledged without pausing its folder (c_first={c_first})"
            );
            assert!(paused(first.1));
            assert_eq!(patches(), 2, "each folder is paused exactly once");
            // Replays of either subset, also after another restart, change nothing.
            let removed = reopen(removed, &state, &configs)?;
            deliver(&removed, first)?;
            deliver(&removed, second)?;
            assert_eq!(patches(), 2, "a replay applied the removal again");
            {
                let db = removed.shared.db.lock().unwrap();
                let records: Vec<_> = db.removals.iter().filter(|r| r.id == id).collect();
                assert_eq!(records.len(), 1, "one decision, one record");
                assert_eq!(
                    records[0].vaults,
                    BTreeSet::from(["v1".into(), "v2".into()])
                );
                assert!(records[0].local_applied && !records[0].superseded());
                assert_eq!(pending(&db), 0);
                assert!(db.local_removed, "both pairings ended with their vaults");
            }
            for (dir, name) in [(&v1, "v1"), (&v2, "v2")] {
                assert_eq!(
                    std::fs::read_to_string(dir.join("keep.md"))?,
                    format!("kept in {name}"),
                    "removed copy was modified"
                );
            }
            removed.shutdown();
        }
        Ok(())
    }
}
