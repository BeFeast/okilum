//! Empty-folder first receive. Unknown nonempty folders never enter this path.
use super::*;
use crate::daemon::{self, DaemonIdentity};
use crate::readiness::HubStatus;
use okilum_sync::{validate_destination, Destination};
use serde_json::{json, Value};
use std::time::Instant;

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
    let lines = ignore_lines(&api.ignores(&source.folder_id)?)?;
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
            &json!({"deviceID":device,"name":"Okilum paired computer","addresses":addresses,
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
    let mut actual_peers = folder["devices"]
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
    ensure!(
        actual_peers == expected_peers,
        "owned replica membership changed"
    );
    ensure!(
        folder["versioning"]["type"] == "staggered"
            && folder["versioning"]["params"]["maxAge"] == "2592000",
        "owned version history policy changed"
    );
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
        let mut expected = expected.clone();
        let mut actual = folder.clone();
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
            transfer.phase == Phase::Preparing && folder["paused"] == true,
            "missing first-receive configuration witness"
        );
    }
    Ok(())
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
                )
            })
    })
}
impl Controller {
    /// Called by refresh. No service grant and no externally supplied readiness
    /// receipt can authorize promotion; every interval comes from the paired peer.
    pub fn reconcile_transfers(&self) -> Result<()> {
        self.check()?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        let transfers = self.shared.db.lock().unwrap().transfers.clone();
        for mut transfer in transfers {
            if transfer.legacy_hold() || !transfer.active || transfer.phase == Phase::Preparing {
                continue;
            }
            match self.reconcile_one(&mut transfer) {
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
        if before["label"] != *label {
            self.check()?;
            ensure!(
                api.folder(&transfer.vault)? == before,
                "receiver changed before relabel"
            );
            api.patch_folder(&transfer.vault, &json!({"label":label}))?;
        }
        let after = api.folder(&transfer.vault)?;
        validate_owned(transfer, &after)?;
        let mut expected = before;
        expected["label"] = json!(label);
        ensure!(
            after == expected,
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

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Result<(tempfile::TempDir, Transfer, Value, Observation, Value)> {
        let root = tempfile::tempdir()?;
        let transfer = Transfer {
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
}
