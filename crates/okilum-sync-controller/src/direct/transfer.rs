//! Empty-folder first receive. Unknown nonempty folders never enter this path.
use super::*;
use crate::daemon::{self, DaemonIdentity};
use crate::readiness::HubStatus;
use okilum_sync::{validate_destination, Destination};
use serde_json::{json, Value};
use std::time::Instant;

/// The Syncthing name a source gives every receiving computer. Introduction
/// copies it to other members, so it never identifies a computer (#2589).
pub(super) const PAIRED_DEVICE_NAME: &str = "Okilum paired computer";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Connection {
    folder_id: String,
    device_id: String,
    ignores: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Transfer {
    #[serde(default)]
    history: history::Policy,
    #[serde(default)]
    create_ignores: create_ignores::Policy,
    peer: Uuid,
    vault: String,
    path: PathBuf,
    daemon: DaemonIdentity,
    connection: Option<Connection>,
    active: bool,
    #[serde(default)]
    owner: Option<Uuid>,
    #[serde(default)]
    destination_identity: Option<DirectoryIdentity>,
    #[serde(default)]
    phase: Phase,
    #[serde(default)]
    witness: Option<Value>,
    /// Authorized display name; also a durable label-only PATCH intent.
    #[serde(default)]
    label: Option<String>,
}

impl Transfer {
    /// Display-only binding from the authenticated source share response.
    /// It does not grant transport configuration or sharing authority.
    pub(super) fn overview_binding(&self) -> Option<(Uuid, String)> {
        self.connection
            .as_ref()
            .map(|connection| (self.peer, connection.device_id.clone()))
    }
}

pub(super) fn share(
    source: &ExistingVault,
    device: &str,
    addresses: &[String],
) -> Result<Connection> {
    // Validate the certificate identity syntax before allowing any configuration
    // mutation. Transport itself authenticates possession of its private key.
    ensure!(
        device.len() == 63
            && device.split('-').count() == 8
            && device.split('-').all(|p| p.len() == 7
                && p.bytes()
                    .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))),
        "invalid Syncthing device identity"
    );
    ensure!(
        device != source.daemon.device_id,
        "cannot share with the source daemon"
    );
    let api = source.daemon.connect()?;
    let folder = api.folder(&source.folder_id)?;
    ensure!(
        Path::new(folder["path"].as_str().context("source path missing")?).canonicalize()?
            == source.path,
        "source replica changed"
    );
    // Preserve all existing device settings, including external roles. Never
    // acquire lifecycle/removal authority over an existing external peer.
    // Reject unsupported ignore policy before the first scoped mutation.
    let mut lines = ignore_lines(&api.ignores(&source.folder_id)?)?;
    // The destination is a new, owned folder: service staging is never note
    // content. Keep the source's local file intact, including custom rules.
    if !lines.iter().any(|line| line == create_ignores::STAGING) {
        lines.insert(0, create_ignores::STAGING.into());
    }
    let config = api.config()?;
    let devices = config["devices"]
        .as_array()
        .context("invalid device inventory")?;
    let addresses =
        if super::addresses::managed(std::slice::from_ref(&source.daemon.config_file))?.is_some() {
            super::addresses::with_dynamic(addresses)
        } else {
            vec!["dynamic".into()]
        };
    if !devices.iter().any(|d| d["deviceID"] == device) {
        api.add_device(
            &json!({"deviceID":device,"name":PAIRED_DEVICE_NAME,"addresses":addresses,
            "introducer":false,"autoAcceptFolders":false}),
        )?;
    }
    let mut peers = folder["devices"]
        .as_array()
        .context("invalid folder peers")?
        .clone();
    if !peers.iter().any(|p| p["deviceID"] == device) {
        // Syncthing has no CAS: re-read immediately, serialize this controller's
        // writes, and retain every field on every existing membership.
        let current = api.folder(&source.folder_id)?;
        ensure!(
            current == folder,
            "source folder changed; retry after administrative changes finish"
        );
        peers.push(json!({"deviceID":device}));
        api.patch_folder(&source.folder_id, &json!({"devices":peers}))?;
    }

    Ok(Connection {
        folder_id: source.folder_id.clone(),
        device_id: source.daemon.device_id.clone(),
        ignores: lines,
    })
}
fn ignore_lines(value: &Value) -> Result<Vec<String>> {
    let field = value.get("ignore").context("ignore policy missing")?;
    let lines = if field.is_null() {
        vec![]
    } else {
        field
            .as_array()
            .context("invalid ignore policy")?
            .iter()
            .map(|v| v.as_str().map(str::to_owned).context("invalid ignore line"))
            .collect::<Result<Vec<_>>>()?
    };
    ensure!(
        lines.len() <= 1024
            && lines.iter().all(|s| s.len() <= 4096
                && !s.chars().any(char::is_control)
                && !s.trim_start().starts_with("#include")),
        "ignore policy requires explicit review"
    );
    Ok(lines)
}
impl Controller {
    pub fn attach(&self, peer_id: Uuid, vault_id: &str, destination: &Path) -> Result<Snapshot> {
        let guard = self.shared.transfer_lock.lock().unwrap();
        self.attach_locked(peer_id, vault_id, destination)?;
        drop(guard);
        self.reconcile_transfers()?;
        self.refresh_attached()?;
        self.snapshot()
    }
    fn attach_locked(&self, peer_id: Uuid, vault_id: &str, destination: &Path) -> Result<()> {
        self.check()?;
        ensure!(
            catalog_contains(&self.shared.db.lock().unwrap(), peer_id, vault_id),
            "choose a vault from the selected computer's authorized catalog first"
        );
        ensure!(
            !vault_id.is_empty()
                && vault_id.len() <= 128
                && vault_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && vault_id != "."
                && vault_id != "..",
            "invalid vault identity"
        );
        let path = destination.canonicalize()?;
        ensure!(path.is_dir(), "destination must be a directory");
        ensure!(
            !self.shared.state.canonicalize()?.starts_with(&path),
            "destination contains operational state"
        );
        let configs = self.shared.configs.lock().unwrap().clone();
        let inventory = daemon::discover(&configs);
        let folders = inventory.folders()?;
        ensure!(
            inventory.candidates.len() == 1,
            "explicit verified runtime bootstrap required; select exactly one existing daemon"
        );
        let selected = &inventory.candidates[0];
        let daemon = selected.identity.clone();
        let api = daemon.connect()?;
        let (paired, mut transfer) = {
            let mut db = self.shared.db.lock().unwrap();
            let paired = db
                .peers
                .get(&peer_id)
                .cloned()
                .context("unknown paired computer")?;
            ensure!(
                catalog_contains(&db, peer_id, vault_id),
                "choose a vault from the authorized catalog first"
            );
            let existing = db
                .transfers
                .iter()
                .find(|t| t.peer == peer_id && t.vault == vault_id)
                .cloned();
            let transfer = if let Some(t) = existing {
                ensure!(
                    t.path == path && t.daemon == daemon,
                    "saved attachment destination or identity differs"
                );
                validate_destination(&path, vault_id, &folders)?;
                t
            } else {
                let validated = validate_destination(&path, vault_id, &folders)?;
                // External known replicas use the separate read-only consent path.
                ensure!(
                    matches!(validated, Destination::Empty(_)),
                    "use attach-existing for an existing replica"
                );
                let t = Transfer {
                    history: history::Policy::default(),
                    create_ignores: create_ignores::Policy::current(),
                    peer: peer_id,
                    vault: vault_id.into(),
                    path: path.clone(),
                    daemon: daemon.clone(),
                    connection: None,
                    active: false,
                    owner: Some(Uuid::new_v4()),
                    destination_identity: Some(DirectoryIdentity::read(&path)?),
                    phase: Phase::Preparing,
                    witness: None,
                    label: None,
                };
                db.transfers.push(t.clone());
                self.save(&db)?;
                t
            };
            (paired, transfer)
        };
        ensure!(
            transfer.owner.is_some() && transfer.phase != Phase::Legacy,
            "legacy first receive has no ownership witness; cannot acquire promotion authority"
        );
        if transfer.active {
            validate_owned(&transfer, &api.folder(vault_id)?)?;
            return Ok(());
        }
        let connection = match &transfer.connection {
            Some(c) => c.clone(),
            None => {
                let response = self.call(
                    paired.endpoint,
                    peer_id,
                    &paired.key,
                    &paired.public,
                    Operation::Share {
                        vault: vault_id.into(),
                        device: daemon.device_id.clone(),
                    },
                )?;
                let Reply::Shared { connection } = response else {
                    anyhow::bail!("authorized vault sharing unavailable");
                };
                ensure!(
                    connection.folder_id == vault_id,
                    "authorized folder identity differs"
                );
                transfer.connection = Some(connection.clone());
                self.save_transfer(&transfer)?;
                connection
            }
        };
        self.check()?;
        let config = api.config()?;
        let addresses = if super::addresses::managed(&configs)?.is_some() {
            let db = self.shared.db.lock().unwrap();
            let peer = db.peers.get(&peer_id).context("paired computer missing")?;
            ensure!(
                peer.syncthing_device
                    .as_ref()
                    .is_none_or(|id| id == &connection.device_id),
                "source transport identity differs"
            );
            super::addresses::with_dynamic(&peer.addresses)
        } else {
            vec!["dynamic".into()]
        };
        if !config["devices"]
            .as_array()
            .context("invalid device inventory")?
            .iter()
            .any(|d| d["deviceID"] == connection.device_id)
        {
            api.add_device(&json!({"deviceID":connection.device_id,"addresses":addresses,"introducer":false,"autoAcceptFolders":false}))?;
        }
        let exists = config["folders"]
            .as_array()
            .context("invalid folder inventory")?
            .iter()
            .any(|f| f["id"] == vault_id);
        if !exists {
            // Revalidate the empty-folder/no-overlap gate immediately before
            // creation; never use stale empty-destination evidence after a pause.
            validate_destination(&path, vault_id, &daemon::discover(&configs).folders()?)?;
            api.add_paused_folder(&json!({"id":vault_id,"label":owned_label(&transfer)?,"path":path,"type":"receiveonly","paused":true,
                "devices":[{"deviceID":daemon.device_id},{"deviceID":connection.device_id}],
                "maxConflicts":-1,"versioning":{"type":"staggered","params":{"maxAge":"2592000"}}}))?;
        }
        let folder = api.folder(vault_id)?;
        validate_owned(&transfer, &folder)?;
        // Capture the complete expanded configuration before the first unpause.
        // The UUID label predates folder creation and scopes lost-create recovery.
        if transfer.witness.is_none() {
            transfer.witness = Some(folder.clone());
            self.save_transfer(&transfer)?;
        }
        self.relabel_transfer(&mut transfer)?;
        if folder["paused"] == true {
            api.set_ignores(
                vault_id,
                &connection
                    .ignores
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )?;
            self.check()?;
            api.patch_folder(vault_id, &json!({"paused":false}))?;
        }
        transfer.active = true;
        transfer.phase = Phase::Receiving;
        transfer.witness = Some(api.folder(vault_id)?);
        self.save_transfer(&transfer)?;
        let attached = inspect_existing(&configs, &path)?;
        let mut db = self.shared.db.lock().unwrap();
        if !db.attached.iter().any(|a| a.folder_id == vault_id) {
            db.attached.push(attached);
        }
        self.save(&db)?;
        Ok(())
    }
    fn save_transfer(&self, transfer: &Transfer) -> Result<()> {
        let mut db = self.shared.db.lock().unwrap();
        let saved = db
            .transfers
            .iter_mut()
            .find(|t| t.peer == transfer.peer && t.vault == transfer.vault)
            .context("attachment intent missing")?;
        *saved = transfer.clone();
        self.save(&db)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}
impl DirectoryIdentity {
    fn read(path: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "destination must remain a real directory"
        );
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    #[default]
    Legacy,
    Preparing,
    Receiving,
    Promoting,
    Bidirectional,
}
impl Transfer {
    pub(super) fn preparing(&self) -> bool {
        self.phase != Phase::Bidirectional
    }
    pub(super) fn legacy_hold(&self) -> bool {
        self.owner.is_none() || self.destination_identity.is_none() || self.phase == Phase::Legacy
    }
    /// The paired computer this vault is received from.
    pub(super) fn peer(&self) -> Uuid {
        self.peer
    }
}
fn owned_label(transfer: &Transfer) -> Result<String> {
    Ok(format!(
        "Okilum Sync {}",
        transfer.owner.context("owned folder identity missing")?
    ))
}
fn expected_label(transfer: &Transfer) -> Result<String> {
    match &transfer.witness {
        Some(witness) => Ok(witness["label"]
            .as_str()
            .context("owned label missing")?
            .into()),
        None => owned_label(transfer),
    }
}
#[derive(Debug)]
struct MembershipChanged;
impl std::fmt::Display for MembershipChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("owned replica membership changed")
    }
}
impl std::error::Error for MembershipChanged {}

fn validate_owned(transfer: &Transfer, folder: &Value) -> Result<()> {
    ensure!(
        !transfer.legacy_hold(),
        "legacy replica has no ownership witness"
    );
    ensure!(
        transfer.destination_identity.as_ref() == Some(&DirectoryIdentity::read(&transfer.path)?),
        "destination directory identity changed"
    );
    ensure!(
        transfer.path.canonicalize()? == transfer.path,
        "destination identity/path changed"
    );
    ensure!(
        Path::new(folder["path"].as_str().context("folder path missing")?).canonicalize()?
            == transfer.path
            && folder["id"] == transfer.vault
            && (folder["label"] == expected_label(transfer)?
                || transfer.witness.is_some()
                    && transfer
                        .label
                        .as_ref()
                        .is_some_and(|label| folder["label"] == *label)),
        "owned replica identity changed"
    );
    let connection = transfer
        .connection
        .as_ref()
        .context("owned connection missing")?;
    // Members that Syncthing added for the introducing share peer are not part
    // of the owned configuration; any other change is still refused below.
    let owned = owned_view(transfer, folder)?;
    let mut actual_peers = owned["devices"]
        .as_array()
        .context("folder peers missing")?
        .iter()
        .map(|v| {
            v["deviceID"]
                .as_str()
                .map(str::to_owned)
                .context("peer identity missing")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut expected_peers = vec![
        transfer.daemon.device_id.clone(),
        connection.device_id.clone(),
    ];
    actual_peers.sort();
    expected_peers.sort();
    if actual_peers != expected_peers {
        return Err(MembershipChanged.into());
    }
    let allowed_mode = match transfer.phase {
        Phase::Promoting => folder["type"] == "receiveonly" || folder["type"] == "sendreceive",
        Phase::Bidirectional => folder["type"] == "sendreceive",
        _ => folder["type"] == "receiveonly",
    };
    ensure!(
        allowed_mode,
        "owned replica mode changed outside its promotion intent"
    );
    if let Some(expected) = &transfer.witness {
        let mut expected = owned_view(transfer, expected)?;
        let mut actual = owned_view(transfer, &transfer.history.witness_folder(folder)?)?;
        // Label changes require a durable authorized intent above; all other
        // fields still match the full preparation/promotion witness.
        expected["label"] = actual["label"].clone();
        expected["paused"] = json!(false);
        actual["paused"] = json!(false);
        expected["type"] = json!("receiveonly");
        actual["type"] = json!("receiveonly");
        ensure!(
            actual == expected,
            "owned replica configuration witness changed"
        );
    } else {
        ensure!(
            transfer.phase == Phase::Preparing
                && folder["paused"] == true
                && folder["versioning"]["type"] == "staggered"
                && folder["versioning"]["params"]["maxAge"].as_str()
                    == Some(
                        (u64::from(transfer.history.days.days()) * 86400)
                            .to_string()
                            .as_str()
                    ),
            "missing first-receive configuration witness or history policy"
        );
    }
    Ok(())
}
/// The folder without members that Syncthing introduced on behalf of the share
/// peer (#2589). The receiver and the share peer themselves are always kept.
fn owned_view(transfer: &Transfer, folder: &Value) -> Result<Value> {
    let peer = &transfer
        .connection
        .as_ref()
        .context("owned connection missing")?
        .device_id;
    let members = folder["devices"]
        .as_array()
        .context("folder peers missing")?;
    let mut owned = folder.clone();
    owned["devices"] = Value::Array(
        members
            .iter()
            .filter(|member| {
                member["deviceID"] == transfer.daemon.device_id.as_str()
                    || member["deviceID"] == peer.as_str()
                    || !introduced_by(members, member, peer)
            })
            .cloned()
            .collect(),
    );
    Ok(owned)
}
/// Whether Syncthing added `member` because the share peer is this receiver's
/// introducer: `introducedBy` names the peer, or a member the peer introduced
/// (Syncthing makes an introducer's introducers introducers too). A member with
/// no or another introducer was added by something else and stays refused.
fn introduced_by(members: &[Value], member: &Value, peer: &str) -> bool {
    let mut introducer = member["introducedBy"].as_str();
    // A chain longer than the member list can only be a cycle.
    for _ in 0..members.len() {
        match introducer {
            Some(id) if id == peer => return true,
            Some(id) if !id.is_empty() => {
                introducer = members
                    .iter()
                    .find(|m| m["deviceID"] == id)
                    .and_then(|m| m["introducedBy"].as_str());
            }
            _ => return false,
        }
    }
    false
}
/// The paired computer that shared this vault, as the Syncthing device that may
/// introduce this receiver to every computer it shares the vault with (#2589).
/// Only a bidirectional owned vault of a still-paired computer qualifies, and
/// never a computer that receives a vault from here: two computers must not
/// introduce each other.
fn introducer_for<'a>(db: &Database, transfer: &'a Transfer) -> Option<&'a str> {
    let device = &transfer.connection.as_ref()?.device_id;
    let peer = db.peers.get(&transfer.peer)?;
    (transfer.active
        && transfer.phase == Phase::Bidirectional
        && !transfer.legacy_hold()
        && peer.syncthing_device.as_ref().is_none_or(|id| id == device)
        && !db.share_grants.iter().any(|g| g.peer == transfer.peer))
    .then_some(device.as_str())
}

/// Source authorization is independent from the receiver's claimed readiness.
/// The exact approved computer/daemon identity and canonical folder are durable.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShareGrant {
    pub peer: Uuid,
    pub vault: String,
    receiver: String,
    source: ExistingVault,
}
impl ShareGrant {
    pub(super) fn new(peer: Uuid, source: &ExistingVault, receiver: &str) -> Self {
        Self {
            peer,
            vault: source.folder_id.clone(),
            receiver: receiver.into(),
            source: source.clone(),
        }
    }
    pub(super) fn validate(&self, source: &ExistingVault, receiver: &str) -> Result<()> {
        ensure!(
            self.receiver == receiver && existing::same_binding(&self.source, source),
            "explicit share scope changed"
        );
        Ok(())
    }
    pub(super) fn observe(&self, configs: &[PathBuf], generation: Uuid) -> Result<Observation> {
        ensure!(
            configs.iter().any(|p| p
                .canonicalize()
                .is_ok_and(|p| p == self.source.daemon.config_file)),
            "source daemon is no longer selected"
        );
        self.source.daemon.verify_local()?;
        let api = self.source.daemon.connect()?;
        let identity = api.identity()?;
        let version = api.version()?["version"]
            .as_str()
            .context("source version missing")?
            .to_owned();
        ensure!(
            daemon::compatible_external_version(&version),
            "unsupported source readiness counters"
        );
        let folder = api.folder(&self.vault)?;
        ensure!(
            folder["id"] == self.vault
                && Path::new(folder["path"].as_str().context("source path missing")?)
                    .canonicalize()?
                    == self.source.path,
            "source folder path or identity changed"
        );
        let devices = folder["devices"]
            .as_array()
            .context("source folder peers missing")?;
        ensure!(
            devices.iter().any(|d| d["deviceID"] == self.receiver)
                && devices
                    .iter()
                    .any(|d| d["deviceID"] == self.source.daemon.device_id),
            "explicit source share was removed"
        );
        let started = identity["startTime"]
            .as_str()
            .context("source daemon start time missing")?
            .to_owned();
        ensure!(!started.is_empty(), "source daemon generation missing");
        let config_hash = wire::fingerprint(&serde_json::to_vec(&folder)?);
        Ok(Observation {
            id: Uuid::new_v4(),
            pair: self.peer,
            folder: self.vault.clone(),
            sender: self.source.daemon.device_id.clone(),
            receiver: self.receiver.clone(),
            generation,
            started,
            version,
            expires: now()? + 30,
            config_hash,
            folder_type: folder["type"]
                .as_str()
                .context("source mode missing")?
                .into(),
            paused: folder["paused"]
                .as_bool()
                .context("source pause state missing")?,
            connected: api.connections()?["connections"][&self.receiver]["connected"]
                .as_bool()
                .unwrap_or(false),
            remote_state: api.completion(&self.vault, &self.receiver)?["remoteState"]
                .as_str()
                .unwrap_or("unknown")
                .into(),
            status: api.status(&self.vault)?,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observation {
    id: Uuid,
    pair: Uuid,
    folder: String,
    sender: String,
    receiver: String,
    generation: Uuid,
    started: String,
    version: String,
    expires: u64,
    config_hash: String,
    folder_type: String,
    paused: bool,
    connected: bool,
    remote_state: String,
    status: Value,
}
struct Receipt {
    observation: Observation,
    received: Instant,
}
impl Receipt {
    fn new(observation: Observation, transfer: &Transfer) -> Result<Self> {
        let connection = transfer.connection.as_ref().context("connection missing")?;
        ensure!(
            observation.pair == transfer.peer
                && observation.folder == transfer.vault
                && observation.sender == connection.device_id
                && observation.receiver == transfer.daemon.device_id
                && !observation.id.is_nil()
                && !observation.generation.is_nil()
                && !observation.started.is_empty()
                && daemon::compatible_external_version(&observation.version),
            "direct readiness scope mismatch"
        );
        ensure!(
            observation.expires > now()? && observation.expires <= now()? + 30,
            "direct readiness observation expired"
        );
        Ok(Self {
            observation,
            received: Instant::now(),
        })
    }
    fn fresh(&self) -> Result<bool> {
        Ok(self.received.elapsed() < Duration::from_secs(30)
            && self.observation.expires > now()?
            && self.observation.expires <= now()? + 30)
    }
}
fn settled(status: &HubStatus) -> bool {
    status.state == "idle"
        && status.need_total_items == Some(0)
        && status.receive_only_total_items == Some(0)
        && status.errors == Some(0)
        && status.pull_errors == Some(0)
        && status.error.as_deref() == Some("")
        && status.invalid.as_deref() == Some("")
        && status.watch_error.as_deref() == Some("")
        && status.global_total_items.is_some()
        && status.global_total_items == status.local_total_items
        && status.global_bytes.is_some()
        && status.global_bytes == status.local_bytes
        && status.global_directories.is_some()
        && status.global_symlinks.is_some()
}
/// Same guarded interval contract as readiness::interval_complete, but both
/// authenticated direct peers use explicit 2.1.x file-byte counters. Never apply
/// the legacy 1.29.5 128-byte directory/symlink adjustment to this protocol.
fn interval_complete(a: &Receipt, b: &Receipt, local: &Value) -> Result<bool> {
    let first = &a.observation;
    let second = &b.observation;
    let ready = |o: &Observation| -> bool {
        !o.paused
            && o.folder_type == "sendreceive"
            && o.connected
            && o.remote_state == "valid"
            && serde_json::from_value::<HubStatus>(o.status.clone())
                .is_ok_and(|status| settled(&status))
    };
    if !a.fresh()?
        || !b.fresh()?
        || !ready(first)
        || !ready(second)
        || a.received > b.received
        || first.id == second.id
        || first.pair != second.pair
        || first.folder != second.folder
        || first.sender != second.sender
        || first.receiver != second.receiver
        || first.generation != second.generation
        || first.started != second.started
        || first.version != second.version
        || first.config_hash != second.config_hash
        || serde_json::from_value::<HubStatus>(first.status.clone())?
            != serde_json::from_value::<HubStatus>(second.status.clone())?
    {
        return Ok(false);
    }
    let status: HubStatus = serde_json::from_value(second.status.clone())?;
    let received = local["remoteSequence"][&second.sender]
        .as_u64()
        .is_some_and(|s| s >= status.sequence)
        || status.sequence == 0
            && status.global_total_items == Some(0)
            && local["remoteSequence"][&second.sender].is_null();
    Ok(received
        && local["state"] == "idle"
        && [
            "needTotalItems",
            "receiveOnlyTotalItems",
            "errors",
            "pullErrors",
        ]
        .iter()
        .all(|k| local[*k].as_u64() == Some(0))
        && ["error", "invalid", "watchError"]
            .iter()
            .all(|k| local[*k].as_str() == Some(""))
        && local["globalTotalItems"].as_u64() == status.global_total_items
        // A fresh receiver never holds records of files deleted before it joined,
        // so totals that include deletions never match (#2589: 9556 local vs 9706
        // global for brain with 150 old deletions). Compare existing entries only.
        && ["Files", "Directories", "Symlinks"].iter().all(|kind| {
            let local_count = local[format!("local{kind}")].as_u64();
            local_count.is_some() && local_count == local[format!("global{kind}")].as_u64()
        })
        && local["globalBytes"].as_u64() == status.global_bytes
        && local["localBytes"].as_u64() == status.global_bytes)
}

pub(super) fn offline(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_connect() || e.is_timeout())
            || cause.downcast_ref::<std::io::Error>().is_some_and(|e| {
                matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::NotConnected
                ) || host_down(e)
            })
    })
}
/// No computer answers at that address: it is switched off, asleep or on
/// another network. A background refresh treats it as offline, never as a
/// failure, and it is not a firewall problem (#2589). macOS reports a sleeping
/// LAN host as EHOSTDOWN, which std leaves uncategorized.
pub(super) fn host_down(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::HostUnreachable
            | std::io::ErrorKind::NetworkUnreachable
            | std::io::ErrorKind::NetworkDown
    ) || error.raw_os_error() == Some(libc::EHOSTDOWN)
}
/// The paired channel's connect error, with a hint that matches the cause.
pub(super) fn not_connected(endpoint: SocketAddr, error: anyhow::Error) -> anyhow::Error {
    let asleep = error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(host_down)
    });
    let hint = if asleep {
        "It may be asleep, switched off or on another network. Wake it, or keep both computers on the same network.".to_owned()
    } else {
        format!("Its firewall may block incoming connections. Open TCP port {} on that computer, or keep both computers on the same network.", endpoint.port())
    };
    error.context(format!(
        "Not connected to the other computer at {endpoint}. {hint}"
    ))
}
impl Controller {
    /// Called by refresh. No service grant and no externally supplied readiness
    /// receipt can authorize promotion; every interval comes from the paired peer.
    pub fn reconcile_transfers(&self) -> Result<()> {
        self.check()?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        let transfers = self.shared.db.lock().unwrap().transfers.clone();
        let history_warnings = self.shared.history_warnings.lock().unwrap().clone();
        for mut transfer in transfers {
            // A failed history ownership check must not be retried through a
            // different mutation path, or stop healthy folders progressing.
            if history_warnings
                .iter()
                .any(|warning| warning.path == transfer.path)
            {
                continue;
            }
            if transfer.legacy_hold() || !transfer.active || transfer.phase == Phase::Preparing {
                continue;
            }
            match self
                .reconcile_one(&mut transfer)
                .and_then(|()| self.introduce_share_peer(&transfer))
            {
                Ok(()) => {}
                Err(error) if offline(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn observe_transfer(&self, transfer: &Transfer, peer: &Paired) -> Result<Option<Receipt>> {
        match self.call(
            peer.endpoint,
            transfer.peer,
            &peer.key,
            &peer.public,
            Operation::Observe {
                vault: transfer.vault.clone(),
                device: transfer.daemon.device_id.clone(),
            },
        )? {
            Reply::Observed { observation } => Ok(Some(Receipt::new(observation, transfer)?)),
            Reply::NotReady => Ok(None),
            _ => anyhow::bail!(
                "authenticated peer readiness unavailable or its share authority changed"
            ),
        }
    }
    fn relabel_transfer(&self, transfer: &mut Transfer) -> Result<()> {
        let api = transfer.daemon.connect()?;
        let before = api.folder(&transfer.vault)?;
        validate_owned(transfer, &before)?;
        if transfer.label.is_none() {
            let cached = {
                let db = self.shared.db.lock().unwrap();
                (db.catalog_peer == Some(transfer.peer))
                    .then(|| {
                        db.catalog
                            .iter()
                            .find(|v| v.id == transfer.vault)
                            .map(|v| v.label.clone())
                    })
                    .flatten()
            };
            let label = match cached {
                Some(label) => label,
                None => {
                    let paired = self
                        .shared
                        .db
                        .lock()
                        .unwrap()
                        .peers
                        .get(&transfer.peer)
                        .cloned()
                        .context("unknown paired computer")?;
                    // Background migration must not replace the catalog selected in Settings.
                    let Reply::Catalog { catalog } = self.call(
                        paired.endpoint,
                        transfer.peer,
                        &paired.key,
                        &paired.public,
                        Operation::Catalog {},
                    )?
                    else {
                        anyhow::bail!("authorized receiver catalog unavailable");
                    };
                    catalog
                        .into_iter()
                        .find(|v| v.id == transfer.vault)
                        .context("authorized receiver name unavailable")?
                        .label
                }
            };
            ensure!(
                !label.is_empty() && label.len() <= 4096 && !label.chars().any(char::is_control),
                "invalid authorized vault name"
            );
            transfer.label = Some(label);
            // Persist before PATCH so a lost acknowledgement can be read back.
            self.save_transfer(transfer)?;
        }
        let label = transfer.label.as_ref().context("receiver name missing")?;
        // Syncthing may add introduced members at any time; compare the rest.
        if before["label"] != *label {
            self.check()?;
            ensure!(
                owned_view(transfer, &api.folder(&transfer.vault)?)?
                    == owned_view(transfer, &before)?,
                "receiver changed before relabel"
            );
            api.patch_folder(&transfer.vault, &json!({"label":label}))?;
        }
        let after = api.folder(&transfer.vault)?;
        validate_owned(transfer, &after)?;
        let mut expected = before;
        expected["label"] = json!(label);
        ensure!(
            owned_view(transfer, &after)? == owned_view(transfer, &expected)?,
            "receiver label readback changed configuration"
        );
        if transfer.witness.as_ref() != Some(&after) {
            transfer.witness = Some(after);
            self.save_transfer(transfer)?;
        }
        Ok(())
    }
    fn reconcile_one(&self, transfer: &mut Transfer) -> Result<()> {
        transfer.daemon.verify_local()?;
        let configs = self.configuration_paths()?;
        ensure!(
            configs.iter().any(|p| p
                .canonicalize()
                .is_ok_and(|p| p == transfer.daemon.config_file)),
            "owned daemon is no longer selected"
        );
        self.relabel_transfer(transfer)?;
        let api = transfer.daemon.connect()?;
        let folder = api.folder(&transfer.vault)?;
        validate_owned(transfer, &folder)?;
        if transfer.phase == Phase::Bidirectional {
            return Ok(());
        }
        if transfer.phase == Phase::Promoting && folder["type"] == "sendreceive" {
            // Lost PATCH acknowledgement: only the prior durable intent and exact
            // owned witness authorize observational completion, never another PATCH.
            transfer.phase = Phase::Bidirectional;
            self.save_transfer(transfer)?;
            return Ok(());
        }
        let peer = self
            .shared
            .db
            .lock()
            .unwrap()
            .peers
            .get(&transfer.peer)
            .cloned()
            .context("paired share authority removed")?;
        let connection = transfer
            .connection
            .as_ref()
            .context("owned connection missing")?;
        if folder["paused"] != false
            || ignore_lines(&api.ignores(&transfer.vault)?)? != connection.ignores
        {
            return Ok(());
        }
        let Some(first) = self.observe_transfer(transfer, &peer)? else {
            return Ok(());
        };
        self.check()?;
        api.scan(&transfer.vault)?;
        let Some(second) = self.observe_transfer(transfer, &peer)? else {
            return Ok(());
        };
        self.check()?;
        api.scan(&transfer.vault)?;
        let local = api.status(&transfer.vault)?;
        let current = api.folder(&transfer.vault)?;
        validate_owned(transfer, &current)?;
        if current != folder
            || ignore_lines(&api.ignores(&transfer.vault)?)? != connection.ignores
            || !interval_complete(&first, &second, &local)?
        {
            return Ok(());
        }
        self.check()?;
        transfer.phase = Phase::Promoting;
        self.save_transfer(transfer)?;
        // Recheck local edits and configuration immediately before the scoped
        // PATCH. A resumed Promoting record must re-observe if it is still receiving.
        api.scan(&transfer.vault)?;
        let latest = api.status(&transfer.vault)?;
        let latest_folder = api.folder(&transfer.vault)?;
        validate_owned(transfer, &latest_folder)?;
        if latest_folder != current
            || ignore_lines(&api.ignores(&transfer.vault)?)? != connection.ignores
            || !interval_complete(&first, &second, &latest)?
        {
            return Ok(());
        }
        self.check()?;
        api.patch_folder(&transfer.vault, &json!({"type":"sendreceive"}))?;
        let promoted = api.folder(&transfer.vault)?;
        validate_owned(transfer, &promoted)?;
        ensure!(
            promoted["type"] == "sendreceive",
            "promotion readback failed"
        );
        transfer.phase = Phase::Bidirectional;
        self.save_transfer(transfer)
    }
    /// Once the received vault is bidirectional, its share peer introduces this
    /// receiver to every computer that shares the vault with it, so the vault
    /// keeps syncing while that one computer is asleep (#2589). Existing
    /// pairings get this on their next refresh. Only this computer's own
    /// Okilum-managed runtime changes; an existing user Syncthing keeps its
    /// owner's device roles. A device that already introduces is only read.
    /// Syncthing applies introductions from the peer's next cluster config.
    fn introduce_share_peer(&self, transfer: &Transfer) -> Result<()> {
        let Some(device) = introducer_for(&self.shared.db.lock().unwrap(), transfer) else {
            return Ok(());
        };
        if !super::addresses::owned_runtime(&self.configuration_paths()?, &transfer.daemon)? {
            return Ok(());
        }
        let api = transfer.daemon.connect()?;
        if api.device(device)?["introducer"] != true {
            self.check()?;
            api.set_device_introducer(device)?;
        }
        Ok(())
    }
}

pub(super) fn receiver_label<'a>(db: &'a Database, attached: &ExistingVault) -> Option<&'a str> {
    let transfer = db.transfers.iter().find(|t| {
        t.active
            && t.path == attached.path
            && t.vault == attached.folder_id
            && t.daemon == attached.daemon
    })?;
    transfer.label.as_deref().or_else(|| {
        (db.catalog_peer == Some(transfer.peer))
            .then(|| {
                db.catalog
                    .iter()
                    .find(|v| v.id == attached.folder_id)
                    .map(|v| v.label.as_str())
            })
            .flatten()
    })
}

/// Offline display projection for recent vaults/window titles, including when
/// Sync is disabled. Reads durable names without opening a controller or daemon.
pub fn received_vault_names(state: &Path) -> Result<Vec<(PathBuf, String)>> {
    let file = state.join("direct.json");
    if !file.try_exists()? {
        return Ok(vec![]);
    }
    let db: Database = serde_json::from_slice(&private::read(&file)?)?;
    Ok(db
        .attached
        .iter()
        .filter_map(|attached| Some((attached.path.clone(), receiver_label(&db, attached)?.into())))
        .collect())
}

impl Controller {
    fn apply_transfer_ignores(&self, transfer: &mut Transfer) -> Result<()> {
        if transfer.create_ignores.complete {
            return Ok(());
        }
        ensure!(
            !transfer.history.has_pending(),
            "Ignore migration waits for the pending history intent"
        );
        let api = self.history_daemon(&transfer.daemon)?;
        let folder = api.folder(&transfer.vault)?;
        validate_owned(transfer, &folder)?;
        let identity = transfer
            .destination_identity
            .as_ref()
            .context("Ignore policy root witness missing")?;
        let root = existing::VaultRoot::open(&transfer.path)?;
        root.verify(&transfer.path)?;
        let identity = (identity.device, identity.inode);
        let bytes = create_ignores::Policy::read(&transfer.path, identity)?;
        let connection = transfer
            .connection
            .as_ref()
            .context("Ignore policy connection missing")?;
        if transfer
            .create_ignores
            .prepare(&folder, &bytes, &connection.ignores)?
        {
            self.save_transfer(transfer)?;
            transfer.create_ignores.apply(
                &api,
                &transfer.vault,
                &transfer.path,
                identity,
                || self.check(),
            )?;
            transfer.connection.as_mut().unwrap().ignores = create_ignores::lines();
            transfer.create_ignores.finish();
        }
        self.save_transfer(transfer)
    }
    pub(super) fn reconcile_transfer_ignores(&self, warnings: &mut Vec<PolicyWarning>) {
        let transfers = self.shared.db.lock().unwrap().transfers.clone();
        for mut transfer in transfers
            .into_iter()
            .filter(|t| t.active && !t.legacy_hold() && t.phase != Phase::Preparing)
        {
            if let Err(error) = self.apply_transfer_ignores(&mut transfer) {
                warnings.push(PolicyWarning {
                    path: transfer.path.clone(),
                    message: format!("{error:#}"),
                });
            }
        }
    }
    fn apply_transfer_history(&self, transfer: &mut Transfer) -> Result<()> {
        let api = self.history_daemon(&transfer.daemon)?;
        let folder = api.folder(&transfer.vault)?;
        validate_owned(transfer, &folder)?;
        if transfer.history.prepare(&folder)? {
            self.save_transfer(transfer)?;
            self.check()?;
            let after = transfer.history.apply(&api, &transfer.vault)?;
            transfer.witness = Some(after);
            transfer.history.finish();
            self.save_transfer(transfer)?;
        }
        Ok(())
    }
    pub(super) fn set_transfer_retention(&self, path: &Path, days: RetentionDays) -> Result<bool> {
        let transfer = self
            .shared
            .db
            .lock()
            .unwrap()
            .transfers
            .iter()
            .find(|t| t.active && !t.legacy_hold() && t.path == path)
            .cloned();
        let Some(mut transfer) = transfer else {
            return Ok(false);
        };
        self.apply_transfer_history(&mut transfer)?;
        transfer.history.days = days;
        self.save_transfer(&transfer)?;
        self.apply_transfer_history(&mut transfer)?;
        Ok(true)
    }
    pub(super) fn reconcile_transfer_history(
        &self,
        warnings: &mut Vec<history::HistoryWarning>,
    ) -> Result<()> {
        let mut membership_error = None;
        let transfers = self.shared.db.lock().unwrap().transfers.clone();
        for mut transfer in transfers
            .into_iter()
            .filter(|t| t.active && !t.legacy_hold() && t.phase != Phase::Preparing)
        {
            if let Err(error) = self.apply_transfer_history(&mut transfer) {
                warnings.push(history::HistoryWarning { path: transfer.path.clone(), message: format!("Previous Sync versions could not be updated: {error:#}. Check this folder and its Sync connection.") });
                if error.is::<MembershipChanged>() {
                    membership_error = Some(error);
                }
            }
        }
        match membership_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
pub(super) fn history_settings(db: &Database) -> Vec<RetentionSetting> {
    db.transfers
        .iter()
        .filter(|t| t.active && !t.legacy_hold())
        .map(|t| RetentionSetting {
            path: t.path.clone(),
            days: t.history.days,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Result<(tempfile::TempDir, Transfer, Value, Observation, Value)> {
        let root = tempfile::tempdir()?;
        let transfer = Transfer {
            history: history::Policy::default(),
            create_ignores: create_ignores::Policy::current(),
            peer: Uuid::new_v4(),
            vault: "fixture".into(),
            path: root.path().to_owned(),
            daemon: DaemonIdentity {
                config_file: root.path().join("config.xml"),
                device_id: "receiver".into(),
                certificate_hash: "fixture".into(),
            },
            connection: Some(Connection {
                folder_id: "fixture".into(),
                device_id: "sender".into(),
                ignores: vec![],
            }),
            active: true,
            owner: Some(Uuid::new_v4()),
            destination_identity: Some(DirectoryIdentity::read(root.path())?),
            phase: Phase::Receiving,
            witness: None,
            label: None,
        };
        let folder = json!({"id":"fixture","label":owned_label(&transfer)?,"path":root.path(),"type":"receiveonly","paused":false,
            "devices":[{"deviceID":"receiver"},{"deviceID":"sender"}],"versioning":{"type":"staggered","params":{"maxAge":"2592000"}}});
        let status = json!({"state":"idle","sequence":5,"globalTotalItems":2,"localTotalItems":2,"globalDirectories":1,
            "globalSymlinks":0,"globalBytes":12,"localBytes":12,"globalFiles":1,"localFiles":1,
            "localDirectories":1,"localSymlinks":0,"globalDeleted":0,"localDeleted":0,"needTotalItems":0,"receiveOnlyTotalItems":0,
            "errors":0,"pullErrors":0,"error":"","invalid":"","watchError":""});
        let observation = Observation {
            id: Uuid::new_v4(),
            pair: transfer.peer,
            folder: "fixture".into(),
            sender: "sender".into(),
            receiver: "receiver".into(),
            generation: Uuid::new_v4(),
            started: "daemon-start".into(),
            version: "v2.1.7".into(),
            expires: now()? + 30,
            config_hash: "config-hash".into(),
            folder_type: "sendreceive".into(),
            paused: false,
            connected: true,
            remote_state: "valid".into(),
            status: status.clone(),
        };
        let mut local = status;
        local["remoteSequence"] = json!({"sender":5});
        Ok((root, transfer, folder, observation, local))
    }
    #[test]
    fn fresh_receiver_without_old_deletion_records_is_ready() -> Result<()> {
        // #2589: brain on idunn carries 150 deletion records a fresh muninn never had.
        let (_root, transfer, _folder, mut observation, mut local) = fixture()?;
        observation.status["globalTotalItems"] = json!(4);
        observation.status["globalDeleted"] = json!(2);
        observation.status["localTotalItems"] = json!(4); // the sender keeps its tombstones
        let first = Receipt::new(observation.clone(), &transfer)?;
        let mut next = observation;
        next.id = Uuid::new_v4();
        let second = Receipt::new(next, &transfer)?;
        local["globalTotalItems"] = json!(4);
        local["globalDeleted"] = json!(2);
        local["localTotalItems"] = json!(2);
        assert!(interval_complete(&first, &second, &local)?);
        local["localFiles"] = json!(0);
        assert!(
            !interval_complete(&first, &second, &local)?,
            "a missing file is not ready"
        );
        Ok(())
    }
    #[test]
    fn direct_21_interval_counts_directory_without_legacy_synthetic_bytes() -> Result<()> {
        let (_root, transfer, _folder, observation, mut local) = fixture()?;
        let first = Receipt::new(observation.clone(), &transfer)?;
        let mut next = observation;
        next.id = Uuid::new_v4();
        let second = Receipt::new(next.clone(), &transfer)?;
        assert!(
            interval_complete(&first, &second, &local)?,
            "directory positive control must use 2.1.x file bytes"
        );
        local["receiveOnlyTotalItems"] = json!(1);
        assert!(!interval_complete(&first, &second, &local)?);
        local["receiveOnlyTotalItems"] = json!(0);
        for key in [
            "errors",
            "pullErrors",
            "globalBytes",
            "localTotalItems",
            "watchError",
        ] {
            let mut incomplete = next.clone();
            incomplete.status.as_object_mut().unwrap().remove(key);
            let incomplete = Receipt::new(incomplete, &transfer)?;
            assert!(
                !interval_complete(&first, &incomplete, &local)?,
                "missing {key} became ready"
            );
        }
        let mut restarted = next.clone();
        restarted.generation = Uuid::new_v4();
        assert!(!interval_complete(
            &first,
            &Receipt::new(restarted, &transfer)?,
            &local
        )?);
        let mut restarted = next;
        restarted.started = "restarted-daemon".into();
        assert!(!interval_complete(
            &first,
            &Receipt::new(restarted, &transfer)?,
            &local
        )?);
        Ok(())
    }
    #[test]
    fn ownership_and_durable_phase_never_adopt_an_external_receiveonly_folder() -> Result<()> {
        let (_root, mut transfer, mut folder, _observation, _local) = fixture()?;
        transfer.witness = Some(folder.clone());
        validate_owned(&transfer, &folder)?;
        let mut forged = folder.clone();
        forged["label"] = json!("external receiveonly");
        assert!(validate_owned(&transfer, &forged).is_err());
        folder["type"] = json!("sendreceive");
        assert!(
            validate_owned(&transfer, &folder).is_err(),
            "Receiving adopted an external mode change"
        );
        transfer.phase = Phase::Promoting;
        validate_owned(&transfer, &folder)?; // exact owned lost-ACK readback
        transfer.owner = None;
        assert!(validate_owned(&transfer, &folder).is_err());
        let mut legacy = serde_json::to_value(&transfer)?;
        for field in ["owner", "phase", "witness", "destination_identity"] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        let legacy: Transfer = serde_json::from_value(legacy)?;
        assert!(legacy.legacy_hold());
        assert!(legacy.preparing());
        Ok(())
    }
    #[test]
    fn receiver_label_intent_recovers_lost_ack_without_accepting_other_changes() -> Result<()> {
        let (_root, mut transfer, folder, _, _) = fixture()?;
        transfer.witness = Some(folder.clone());
        let mut renamed = folder.clone();
        renamed["label"] = json!("Notes");
        assert!(
            validate_owned(&transfer, &renamed).is_err(),
            "unrecorded label change refused"
        );
        transfer.label = Some("Notes".into());
        validate_owned(&transfer, &folder)?;
        validate_owned(&transfer, &renamed)?;
        for (field, value) in [
            ("rescanIntervalS", json!(99)),
            ("path", json!("/other")),
            ("devices", json!([])),
            ("type", json!("sendreceive")),
        ] {
            let mut changed = renamed.clone();
            changed[field] = value;
            assert!(
                validate_owned(&transfer, &changed).is_err(),
                "relabel accepted {field}"
            );
        }
        transfer.witness = Some(renamed.clone());
        validate_owned(&transfer, &renamed)?;
        assert!(
            validate_owned(&transfer, &folder).is_err(),
            "completed relabel cannot revert to UUID"
        );
        transfer.witness = None;
        transfer.phase = Phase::Preparing;
        renamed["paused"] = json!(true);
        assert!(
            validate_owned(&transfer, &renamed).is_err(),
            "name alone cannot authorize lost-create recovery"
        );
        Ok(())
    }
    fn paired(id: Uuid, device: Option<&str>) -> Result<Paired> {
        let public = wire::public(&wire::random()?)?;
        Ok(Paired {
            peer: Peer {
                id,
                name: "Source computer".into(),
                device_id: wire::fingerprint(&public),
            },
            public,
            key: [0; 32],
            endpoint: "127.0.0.1:9".parse()?,
            allowed: vec!["fixture".into()],
            syncthing_device: device.map(Into::into),
            addresses: vec![],
        })
    }
    fn grant(transfer: &Transfer) -> ShareGrant {
        let source = ExistingVault {
            daemon: transfer.daemon.clone(),
            version: "v2.1.6".into(),
            folder_id: "other-vault".into(),
            path: transfer.path.clone(),
            label: "Shared from here".into(),
            paused: false,
            folder_type: "sendreceive".into(),
            peers: vec![],
            root_identity: None,
        };
        ShareGrant::new(transfer.peer, &source, "sender")
    }
    #[test]
    fn members_introduced_by_the_share_peer_keep_the_vault_owned() -> Result<()> {
        let (_root, mut transfer, folder, _, _) = fixture()?;
        transfer.witness = Some(folder.clone());
        // Syncthing appends the computers the share peer vouches for, and the
        // members that an introduced introducer vouches for in turn.
        let mut meshed = folder.clone();
        meshed["devices"].as_array_mut().unwrap().extend([
            json!({"deviceID":"hub","introducedBy":"sender","encryptionPassword":""}),
            json!({"deviceID":"nas","introducedBy":"hub","encryptionPassword":""}),
        ]);
        validate_owned(&transfer, &folder)?; // positive control
        validate_owned(&transfer, &meshed)?;
        transfer.phase = Phase::Bidirectional;
        meshed["type"] = json!("sendreceive");
        validate_owned(&transfer, &meshed)?;
        // A witness read back after introduction still matches once the peer
        // stops sharing with those computers and Syncthing removes them.
        transfer.witness = Some(meshed);
        let mut removed = folder;
        removed["type"] = json!("sendreceive");
        validate_owned(&transfer, &removed)?;
        Ok(())
    }
    #[test]
    fn issue2045_history_transition_preserves_introducer_ownership_guards() -> Result<()> {
        let (_root, mut transfer, mut folder, _, _) = fixture()?;
        folder["devices"]
            .as_array_mut()
            .unwrap()
            .push(json!({"deviceID":"hub","introducedBy":"sender","encryptionPassword":""}));
        transfer.witness = Some(folder.clone());
        transfer.history.days = RetentionDays::try_from(90)?;
        assert!(transfer.history.prepare(&folder)?);
        validate_owned(&transfer, &folder)?;
        // Simulate a lost acknowledgement of the journaled history PATCH.
        let mut after = folder.clone();
        after["versioning"]["type"] = json!("staggered");
        after["versioning"]["params"]["maxAge"] = json!("7776000");
        validate_owned(&transfer, &after)?;
        let mut unrelated = after.clone();
        unrelated["paused"] = json!(true);
        assert!(validate_owned(&transfer, &unrelated).is_err());
        transfer.witness = Some(after.clone());
        transfer.history.finish();
        validate_owned(&transfer, &after)?;
        after["devices"]
            .as_array_mut()
            .unwrap()
            .retain(|d| d["deviceID"] != "hub");
        validate_owned(&transfer, &after)?;
        after["devices"]
            .as_array_mut()
            .unwrap()
            .push(json!({"deviceID":"stranger"}));
        assert!(validate_owned(&transfer, &after).is_err());
        Ok(())
    }
    #[test]
    fn members_without_the_share_peer_as_introducer_are_refused() -> Result<()> {
        let (_root, mut transfer, folder, _, _) = fixture()?;
        transfer.witness = Some(folder.clone());
        validate_owned(&transfer, &folder)?; // positive control
        for extra in [
            vec![json!({"deviceID":"stranger"})],
            vec![json!({"deviceID":"stranger","introducedBy":""})],
            vec![json!({"deviceID":"stranger","introducedBy":"elsewhere"})],
            vec![json!({"deviceID":"stranger","introducedBy":"receiver"})],
            // A cycle never reaches the share peer.
            vec![
                json!({"deviceID":"one","introducedBy":"two"}),
                json!({"deviceID":"two","introducedBy":"one"}),
            ],
        ] {
            let mut changed = folder.clone();
            changed["devices"]
                .as_array_mut()
                .unwrap()
                .extend(extra.clone());
            let error = validate_owned(&transfer, &changed).unwrap_err();
            assert!(
                error.to_string().contains("membership"),
                "{extra:?}: {error}"
            );
        }
        // An introduced member never stands in for the share peer itself.
        let mut replaced = folder;
        replaced["devices"] =
            json!([{"deviceID":"receiver"},{"deviceID":"hub","introducedBy":"sender"}]);
        assert!(validate_owned(&transfer, &replaced).is_err());
        Ok(())
    }
    #[test]
    fn only_the_share_peer_of_a_bidirectional_vault_may_introduce() -> Result<()> {
        let (_root, mut transfer, _, _, _) = fixture()?;
        let state = tempfile::tempdir()?;
        let controller = Controller::open(state.path().join("direct"), vec![])?;
        let mut db = controller.shared.db.lock().unwrap();
        transfer.phase = Phase::Bidirectional;
        assert_eq!(introducer_for(&db, &transfer), None, "unpaired computer");
        db.peers.insert(transfer.peer, paired(transfer.peer, None)?);
        assert_eq!(introducer_for(&db, &transfer), Some("sender"));
        for phase in [Phase::Receiving, Phase::Promoting] {
            transfer.phase = phase;
            assert_eq!(introducer_for(&db, &transfer), None, "{phase:?}");
        }
        transfer.phase = Phase::Bidirectional;
        db.peers.get_mut(&transfer.peer).unwrap().syncthing_device = Some("other".into());
        assert_eq!(introducer_for(&db, &transfer), None, "transport differs");
        db.peers.get_mut(&transfer.peer).unwrap().syncthing_device = Some("sender".into());
        assert_eq!(introducer_for(&db, &transfer), Some("sender"));
        db.share_grants.push(grant(&transfer));
        assert_eq!(
            introducer_for(&db, &transfer),
            None,
            "the peer receives from here: no mutual introducers"
        );
        db.share_grants.clear();
        transfer.owner = None;
        assert_eq!(introducer_for(&db, &transfer), None, "legacy receive");
        drop(db);
        controller.shutdown();
        Ok(())
    }
    /// Loopback Syncthing REST double for the exact routes one reconcile uses.
    #[cfg(target_os = "linux")]
    fn fake_rest(state: Arc<Mutex<Value>>) -> Result<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let _ = respond(&mut stream, &state);
            }
        });
        Ok(address)
    }
    #[cfg(target_os = "linux")]
    fn respond(stream: &mut std::net::TcpStream, state: &Mutex<Value>) -> Result<()> {
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
        let device = path.strip_prefix("/rest/config/devices/");
        let mut state = state.lock().unwrap();
        let reply = match (method, path) {
            ("GET", "/rest/system/status") => {
                Some(json!({"myID":"receiver","startTime":"fixture"}))
            }
            ("GET", "/rest/system/version") => Some(json!({"version":"v2.1.6"})),
            ("GET", "/rest/config/folders/fixture") => Some(state["folder"].clone()),
            ("GET", _) => device.and_then(|id| state["devices"].get(id).cloned()),
            ("PATCH", _) if device.is_some_and(|id| state["devices"].get(id).is_some()) => {
                let id = device.unwrap_or_default();
                let changes: Value = serde_json::from_slice(&body)?;
                for (key, value) in changes.as_object().context("patch object")? {
                    state["devices"][id][key] = value.clone();
                }
                state["patches"].as_array_mut().unwrap().push(json!(id));
                Some(Value::Null)
            }
            _ => None,
        };
        let (status, body) = match reply {
            Some(Value::Null) => ("200 OK", String::new()),
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
    #[cfg(target_os = "linux")]
    #[test]
    fn existing_pairing_marks_only_its_share_peer_as_introducer_once() -> Result<()> {
        use sha2::{Digest, Sha256};
        let (_root, mut transfer, mut folder, _, _) = fixture()?;
        let runtime = tempfile::tempdir()?;
        let base = runtime.path().canonicalize()?;
        let prepared = base.join("prepared");
        let home = prepared.join("syncthing");
        private::directory(&prepared)?;
        private::directory(&home)?;
        let rest = Arc::new(Mutex::new(json!({
            "folder": null,
            "devices": {
                "sender": {"deviceID":"sender","addresses":["dynamic"],"introducer":false},
                "hub": {"deviceID":"hub","addresses":["dynamic"],"introducer":false,"introducedBy":"sender"}
            },
            "patches": []
        })));
        let address = fake_rest(rest.clone())?;
        let config = home.join("config.xml");
        private::write(&config, format!(r#"<configuration><gui tls="false"><address>{address}</address><apikey>fixture-key</apikey></gui></configuration>"#).as_bytes())?;
        private::write(&home.join("cert.pem"), b"fixture certificate")?;
        let identity = DaemonIdentity {
            config_file: config.canonicalize()?,
            device_id: "receiver".into(),
            certificate_hash: format!("{:x}", Sha256::digest(b"fixture certificate")),
        };
        // An existing pairing, already bidirectional before this change.
        transfer.daemon = identity.clone();
        transfer.phase = Phase::Bidirectional;
        folder["type"] = json!("sendreceive");
        transfer.witness = Some(folder.clone());
        transfer.label = folder["label"].as_str().map(Into::into);
        rest.lock().unwrap()["folder"] = folder.clone();
        let controller = Controller::open(base.join("direct"), vec![config.clone()])?;
        {
            let mut db = controller.shared.db.lock().unwrap();
            db.peers
                .insert(transfer.peer, paired(transfer.peer, Some("sender"))?);
            db.transfers.push(transfer.clone());
        }
        let patches = || rest.lock().unwrap()["patches"].clone();
        controller.reconcile_transfers()?;
        assert_eq!(
            patches(),
            json!([]),
            "an external Syncthing keeps its roles"
        );
        let receipt = json!({
            "request": {"executable": base.join("syncthing"), "rest_address": address,
                "listen_address": "127.0.0.1:22000"},
            "binary_version": crate::daemon::CLIENT_VERSION,
            "binary_hash": crate::linux_bootstrap::BINARY_SHA,
            "identity": identity,
        });
        private::write(
            &prepared.join("prepared.json"),
            &serde_json::to_vec(&receipt)?,
        )?;
        controller
            .shared
            .db
            .lock()
            .unwrap()
            .share_grants
            .push(grant(&transfer));
        controller.reconcile_transfers()?;
        assert_eq!(patches(), json!([]), "no mutual introducers");
        controller.shared.db.lock().unwrap().share_grants.clear();
        controller.reconcile_transfers()?;
        assert_eq!(patches(), json!(["sender"]));
        assert_eq!(
            rest.lock().unwrap()["devices"]["sender"]["introducer"],
            true
        );
        assert_eq!(rest.lock().unwrap()["devices"]["hub"]["introducer"], false);
        controller.reconcile_transfers()?;
        assert_eq!(patches(), json!(["sender"]), "the flag is set only once");
        // Syncthing meshes the vault with the peer's computers: still owned.
        rest.lock().unwrap()["folder"]["devices"]
            .as_array_mut()
            .unwrap()
            .push(json!({"deviceID":"hub","introducedBy":"sender","encryptionPassword":""}));
        controller.reconcile_transfers()?;
        assert_eq!(
            controller.shared.db.lock().unwrap().transfers[0].phase,
            Phase::Bidirectional
        );
        // Negative control: a member that the peer did not introduce.
        rest.lock().unwrap()["folder"]["devices"]
            .as_array_mut()
            .unwrap()
            .push(json!({"deviceID":"stranger","introducedBy":"","encryptionPassword":""}));
        let error = controller.reconcile_transfers().unwrap_err();
        assert!(format!("{error:#}").contains("membership"), "{error:#}");
        assert_eq!(patches(), json!(["sender"]));
        controller.shutdown();
        Ok(())
    }
    #[test]
    fn replaced_destination_directory_fails_even_when_its_canonical_path_matches() -> Result<()> {
        let root = tempfile::tempdir()?;
        let destination = root.path().join("vault");
        std::fs::create_dir(&destination)?;
        let identity = DirectoryIdentity::read(&destination)?;
        std::fs::rename(&destination, root.path().join("previous"))?;
        std::fs::create_dir(&destination)?;
        assert_ne!(identity, DirectoryIdentity::read(&destination)?);
        Ok(())
    }
    #[test]
    fn issue2589_a_sleeping_paired_computer_is_offline_without_a_firewall_hint() {
        use std::io::{Error, ErrorKind};
        let endpoint: SocketAddr = "10.10.0.83:42321".parse().unwrap();
        let connect = |error: Error| {
            not_connected(
                endpoint,
                anyhow::Error::new(error).context("paired computer unavailable"),
            )
        };
        for error in [
            Error::from(ErrorKind::HostUnreachable),
            Error::from(ErrorKind::NetworkUnreachable),
            Error::from(ErrorKind::NetworkDown),
            // macOS: a sleeping LAN host, which std does not categorize.
            Error::from_raw_os_error(libc::EHOSTDOWN),
        ] {
            let kind = error.kind();
            let error = connect(error);
            // Background refresh defers a sleeping computer like a refused one.
            assert!(offline(&error), "{kind:?}");
            let text = error.to_string();
            assert!(
                text.starts_with("Not connected to the other computer at 10.10.0.83:42321.")
                    && text.contains("asleep")
                    && !text.contains("firewall"),
                "{kind:?}: {text}"
            );
        }
        // A refused or silently dropped connection keeps the firewall hint.
        for kind in [ErrorKind::ConnectionRefused, ErrorKind::TimedOut] {
            let error = connect(Error::from(kind));
            assert!(offline(&error));
            assert!(
                error.to_string().contains("Open TCP port 42321"),
                "{kind:?}"
            );
        }
        assert!(!offline(&anyhow::Error::new(Error::from(
            ErrorKind::PermissionDenied
        ))));
    }
}
