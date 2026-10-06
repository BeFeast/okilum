//! Private host adapter: one operator-bound owner/vault, never an HTTP REST proxy.
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};
use tessera_sync::Syncthing;
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub owner_id: Uuid,
    pub vault_id: Uuid,
    pub folder_id: String,
    pub folder_path: PathBuf,
    pub hub_device_id: String,
    pub rest_address: SocketAddr,
    pub rest_key_file: PathBuf,
    pub data_dir: PathBuf,
    pub socket: PathBuf,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Add,
    Remove,
    Status,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub owner_id: Uuid,
    pub vault_id: Uuid,
    pub registration_id: Uuid,
    pub device_id: String,
    pub action: Action,
}
#[derive(Deserialize, Serialize)]
pub struct Reply {
    pub state: String,
}

pub fn call(socket: &Path, request: &Request) -> Result<Reply> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    use std::io::Read;
    BufReader::new(stream).take(4096).read_line(&mut line)?;
    let reply: Reply = serde_json::from_str(&line)?;
    ensure!(reply.state != "unavailable", "adapter unavailable");
    Ok(reply)
}

pub struct Hub {
    config: Config,
    db: Connection,
    api: Syncthing,
}
impl Hub {
    pub fn open(config: Config) -> Result<Self> {
        ensure!(
            !config.owner_id.is_nil() && !config.vault_id.is_nil(),
            "missing scope"
        );
        ensure!(
            crate::sync::valid_device(&config.hub_device_id),
            "invalid hub identity"
        );
        private_directory(&config.data_dir)?;
        private_file(&config.rest_key_file)?;
        let key = std::fs::read_to_string(&config.rest_key_file)?;
        let api = Syncthing::connect(config.rest_address, key.trim())?;
        let db = Connection::open(config.data_dir.join("hub.db"))?;
        db.busy_timeout(Duration::from_secs(3))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS scope(binding TEXT NOT NULL); CREATE TABLE IF NOT EXISTS registrations(id TEXT PRIMARY KEY,device TEXT NOT NULL,revoked INTEGER NOT NULL CHECK(revoked IN (0,1)),owned INTEGER NOT NULL CHECK(owned IN (0,1))); CREATE UNIQUE INDEX IF NOT EXISTS one_active_device ON registrations(device) WHERE revoked=0; ")?;
        let binding = json!([
            config.owner_id,
            config.vault_id,
            config.folder_id,
            config.folder_path,
            config.hub_device_id
        ])
        .to_string();
        let saved: Option<String> = db
            .query_row("SELECT binding FROM scope", [], |r| r.get(0))
            .optional()?;
        match saved {
            Some(saved) => ensure!(saved == binding, "adapter scope changed"),
            None => {
                db.execute("INSERT INTO scope VALUES(?1)", [binding])?;
            }
        }
        Ok(Self { config, db, api })
    }
    fn check_hub(&self) -> Result<serde_json::Value> {
        self.api.verify_identity(&self.config.hub_device_id)?;
        ensure!(
            self.api.version()?["version"] == "v1.29.5",
            "unsupported hub version"
        );
        let folder = self.api.folder(&self.config.folder_id)?;
        ensure!(
            folder["path"].as_str() == self.config.folder_path.to_str(),
            "configured folder path changed"
        );
        Ok(folder)
    }
    fn remove_share(&self, device: &str) -> Result<()> {
        let folder = self.check_hub()?;
        let before = folder["devices"].as_array().context("invalid shares")?;
        let after: Vec<_> = before
            .iter()
            .filter(|d| d["deviceID"] != device)
            .cloned()
            .collect();
        if before.len() != after.len() {
            // Recheck immediately before writing; no whole-config mutation.
            ensure!(
                self.api.folder(&self.config.folder_id)?["devices"] == folder["devices"],
                "shares changed concurrently"
            );
            self.api
                .patch_folder(&self.config.folder_id, &json!({"devices":after}))?;
        }
        Ok(())
    }
    pub fn apply(&mut self, request: &Request) -> Result<Reply> {
        ensure!(
            request.owner_id == self.config.owner_id && request.vault_id == self.config.vault_id,
            "scope rejected"
        );
        ensure!(
            !request.registration_id.is_nil()
                && crate::sync::valid_device(&request.device_id)
                && request.device_id != self.config.hub_device_id,
            "invalid registration"
        );
        let existing: Option<(String, bool, bool)> = self
            .db
            .query_row(
                "SELECT device,revoked,owned FROM registrations WHERE id=?1",
                [request.registration_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((device, _, _)) = &existing {
            ensure!(device == &request.device_id, "registration replay differs");
        }
        let another_active: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM registrations WHERE device=?1 AND id<>?2 AND revoked=0)",
            params![request.device_id, request.registration_id.to_string()],
            |r| r.get(0),
        )?;
        match request.action {
            Action::Remove => {
                // Tombstone is durable even if the hub is offline or Add never ran.
                self.db.execute("INSERT INTO registrations(id,device,revoked,owned) VALUES(?1,?2,1,0) ON CONFLICT(id) DO UPDATE SET revoked=1",params![request.registration_id.to_string(),request.device_id])?;
                if !another_active && existing.as_ref().is_some_and(|(_, _, owned)| *owned) {
                    self.remove_share(&request.device_id)?;
                }
                Ok(Reply {
                    state: "revoked".into(),
                })
            }
            Action::Add => {
                ensure!(
                    !existing.as_ref().is_some_and(|(_, revoked, _)| *revoked),
                    "registration revoked"
                );
                ensure!(!another_active, "device has another active registration");
                let mut folder = self.check_hub()?;
                if existing.is_none() {
                    let previously_owned:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM registrations WHERE device=?1 AND revoked=1 AND owned=1)",[&request.device_id],|r|r.get(0))?;
                    let config = self.api.config()?;
                    ensure!(
                        previously_owned
                            || !config["devices"]
                                .as_array()
                                .context("invalid devices")?
                                .iter()
                                .any(|d| d["deviceID"] == request.device_id),
                        "existing external device requires explicit reconciliation"
                    );
                    ensure!(
                        previously_owned
                            || !folder["devices"]
                                .as_array()
                                .context("invalid shares")?
                                .iter()
                                .any(|d| d["deviceID"] == request.device_id),
                        "existing external share"
                    );
                    self.db.execute(
                        "INSERT INTO registrations(id,device,revoked,owned) VALUES(?1,?2,0,1)",
                        params![request.registration_id.to_string(), request.device_id],
                    )?;
                }
                self.api.add_device(&json!({"deviceID":request.device_id,"name":"Tessera desktop","addresses":["dynamic"],"introducer":false,"skipIntroductionRemovals":true,"autoAcceptFolders":false}))?;
                folder = self.check_hub()?;
                let before = folder["devices"].as_array().context("invalid shares")?;
                if !before.iter().any(|d| d["deviceID"] == request.device_id) {
                    let mut after = before.clone();
                    after.push(json!({"deviceID":request.device_id}));
                    ensure!(
                        self.api.folder(&self.config.folder_id)?["devices"] == folder["devices"],
                        "shares changed concurrently"
                    );
                    self.api
                        .patch_folder(&self.config.folder_id, &json!({"devices":after}))?;
                }
                Ok(Reply {
                    state: "hub_ready".into(),
                })
            }
            Action::Status => {
                let (_, revoked, owned) = existing.context("unknown registration")?;
                let f = self.check_hub()?;
                let shared = f["devices"]
                    .as_array()
                    .context("invalid shares")?
                    .iter()
                    .any(|d| d["deviceID"] == request.device_id);
                Ok(Reply {
                    state: if revoked {
                        if shared && owned && !another_active {
                            "removal_pending"
                        } else {
                            "revoked"
                        }
                    } else if shared {
                        "hub_ready"
                    } else {
                        "provisioning"
                    }
                    .into(),
                })
            }
        }
    }
    pub fn reconcile_revoked(&self) -> Result<()> {
        let mut q = self
            .db
            .prepare("SELECT DISTINCT device FROM registrations old WHERE revoked=1 AND owned=1 AND NOT EXISTS(SELECT 1 FROM registrations current WHERE current.device=old.device AND current.revoked=0)")?;
        let devices = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for device in devices {
            self.remove_share(&device)?;
        }
        Ok(())
    }
}
pub fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if !path.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink() && m.permissions().mode() & 0o077 == 0,
        "directory must be private"
    );
    Ok(())
}
pub fn private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() && !m.file_type().is_symlink() && m.permissions().mode() & 0o077 == 0,
        "credential/config file must be private"
    );
    Ok(())
}
pub fn serve(config: Config) -> Result<()> {
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    private_directory(config.socket.parent().context("socket parent missing")?)?;
    private_directory(&config.data_dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.data_dir.join("adapter.lock"))?;
    lock.try_lock().context("adapter already running")?;
    let mut hub = Hub::open(config.clone())?;
    if config.socket.exists() {
        use std::os::unix::fs::FileTypeExt;
        ensure!(
            std::fs::symlink_metadata(&config.socket)?
                .file_type()
                .is_socket(),
            "socket path occupied"
        );
        ensure!(
            UnixStream::connect(&config.socket).is_err(),
            "socket already active"
        );
        std::fs::remove_file(&config.socket)?;
    }
    let listener = UnixListener::bind(&config.socket)?;
    std::fs::set_permissions(&config.socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let mut last = std::time::Instant::now() - Duration::from_secs(5);
    loop {
        if last.elapsed() >= Duration::from_secs(5) {
            let _ = hub.reconcile_revoked();
            last = std::time::Instant::now();
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                let result = (|| -> Result<Reply> {
                    use std::io::Read;
                    let mut line = String::new();
                    BufReader::new(stream.try_clone()?)
                        .take(4096)
                        .read_line(&mut line)?;
                    ensure!(line.ends_with('\n'), "incomplete request");
                    hub.apply(&serde_json::from_str::<Request>(&line)?)
                })();
                let reply = result.unwrap_or(Reply {
                    state: "unavailable".into(),
                });
                let _ = serde_json::to_writer(&mut stream, &reply);
                let _ = stream.write_all(b"\n");
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => bail!(e),
        }
    }
}
