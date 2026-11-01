//! Empty-folder first receive. Unknown nonempty folders never enter this path.
use super::*;
use crate::daemon::{self, DaemonIdentity};
use okilum_sync::{validate_destination, Destination};
use serde_json::{json, Value};

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
}

pub(super) fn share(source: &ExistingVault, device: &str) -> Result<Connection> {
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
    if !devices.iter().any(|d| d["deviceID"] == device) {
        api.add_device(
            &json!({"deviceID":device,"name":"Okilum paired computer","addresses":["dynamic"],
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
        self.check()?;
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
                db.catalog.iter().any(|v| v.id == vault_id),
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
                };
                db.transfers.push(t.clone());
                self.save(&db)?;
                t
            };
            (paired, transfer)
        };
        if transfer.active {
            return self.snapshot();
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
        if !config["devices"]
            .as_array()
            .context("invalid device inventory")?
            .iter()
            .any(|d| d["deviceID"] == connection.device_id)
        {
            api.add_device(&json!({"deviceID":connection.device_id,"addresses":["dynamic"],"introducer":false,"autoAcceptFolders":false}))?;
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
            api.add_paused_folder(&json!({"id":vault_id,"label":vault_id,"path":path,"type":"receiveonly","paused":true,
                "devices":[{"deviceID":daemon.device_id},{"deviceID":connection.device_id}],
                "versioning":{"type":"staggered","params":{"maxAge":"2592000"}}}))?;
        }
        let folder = api.folder(vault_id)?;
        ensure!(
            Path::new(folder["path"].as_str().context("folder path missing")?).canonicalize()?
                == path
                && folder["type"] == "receiveonly"
                && folder["devices"]
                    .as_array()
                    .context("invalid folder peers")?
                    .iter()
                    .any(|d| d["deviceID"] == connection.device_id),
            "prepared folder differs; external configuration unchanged"
        );
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
        self.save_transfer(&transfer)?;
        let attached = inspect_existing(&configs, &path)?;
        let mut db = self.shared.db.lock().unwrap();
        if !db.attached.iter().any(|a| a.folder_id == vault_id) {
            db.attached.push(attached);
        }
        self.save(&db)?;
        snapshot(&db)
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
