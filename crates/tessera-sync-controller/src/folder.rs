//! Scoped local folder enrollment. This layer never owns an external daemon's
//! service, never replaces a full config, and never deletes canonical files.
use crate::{
    daemon::{discover, DaemonIdentity},
    enrollment::{Intent, Snapshot},
    pairing::{Descriptor, State},
    private,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tessera_sync::{validate_destination, Destination, Syncthing};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
enum Phase {
    Preparing,
    Receiving,
    Promoting,
    Active,
    Reused,
    Removed,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    daemon: DaemonIdentity,
    owner: Uuid,
    registration: Uuid,
    vault: Uuid,
    descriptor: Descriptor,
    path: PathBuf,
    owns_folder: bool,
    owns_hub: bool,
    original_paused: bool,
    changed_pause: bool,
    desired_paused: bool,
    remove_requested: bool,
    phase: Phase,
    #[serde(default)]
    last_connected_at: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct LocalStatus {
    pub reused: bool,
    pub removed: bool,
    /// True means external configuration remains; it is not a fleet isolation receipt.
    pub external_sync_retained: bool,
    pub folder: Value,
    pub status: Value,
    pub errors: Value,
    pub hub_connected: bool,
    /// Unix seconds of the last authenticated observation of a hub connection.
    /// This is not a receipt of complete synchronization or peer availability.
    pub last_connected_at: Option<u64>,
}
pub struct FolderController {
    state: PathBuf,
}
impl FolderController {
    pub fn new(state: PathBuf) -> Self {
        Self { state }
    }
    /// Call after a fresh Enrollment::poll. Inventory paths must include all
    /// discovered daemon configs, refreshed by the desktop before each operation.
    pub fn enroll(
        &self,
        daemon: &DaemonIdentity,
        configs: &[PathBuf],
        approval: &Snapshot,
        destination: &Path,
    ) -> Result<LocalStatus> {
        ensure!(
            approval.intent == Intent::Pair && approval.removal.is_none(),
            "registration is being removed"
        );
        let registration = approval.registration.as_ref().context("grant missing")?;
        ensure!(
            registration.state == State::HubReady && registration.device_id == daemon.device_id,
            "grant not ready for selected daemon"
        );
        let descriptor = approval
            .descriptor
            .as_ref()
            .context("connection descriptor unavailable")?;
        descriptor.validate()?;
        // Serialize all Tessera enrollment inventories for this OS user, even
        // when independent attempts use different durable state directories.
        // This lock is coordination only; durable authority stays in the journal.
        let _enrollment_lock = private::lock(&Path::new("/tmp").join(format!(
            "tessera-sync-enrollment-{}",
            rustix::process::geteuid().as_raw()
        )))?;
        let _lock = private::lock(&self.state)?;
        let inventory = discover(configs);
        let selected = inventory.select(&daemon.config_file)?;
        ensure!(
            &selected.identity == daemon,
            "selected daemon identity changed"
        );
        let api = daemon.connect()?;
        let path = destination.canonicalize()?;
        let mut j = if self.file().try_exists()? {
            let j = self.load()?;
            ensure!(
                j.daemon == *daemon
                    && j.path == path
                    && j.registration == registration.id
                    && j.vault == registration.vault
                    && j.descriptor == *descriptor,
                "saved folder scope differs"
            );
            ensure!(
                !j.remove_requested,
                "removed enrollment requires a new operation"
            );
            // Recheck overlap against every current inventory, including resumes.
            validate_destination(&path, &descriptor.folder_id, &inventory.folders()?)?;
            j
        } else {
            let destination =
                validate_destination(&path, &descriptor.folder_id, &inventory.folders()?)?;
            let owns_folder = matches!(destination, Destination::Empty(_));
            let config = api.config()?;
            let hub_exists = devices(&config)?
                .iter()
                .any(|d| d["deviceID"] == descriptor.hub_device_id);
            let original_paused = if owns_folder {
                false
            } else {
                // Another daemon's same ID/path is not evidence for selected reuse.
                ensure!(
                    selected
                        .folders
                        .iter()
                        .any(|(id, p)| id == &descriptor.folder_id && p == &path),
                    "replica is not in selected daemon"
                );
                let folder = api.folder(&descriptor.folder_id)?;
                validate_path(&folder, &path)?;
                ensure!(
                    hub_exists && shares(&folder, &descriptor.hub_device_id),
                    "replica does not share with approved hub"
                );
                ensure!(
                    policy_matches(&api.ignores(&descriptor.folder_id)?, &descriptor.ignores)?,
                    "existing ignore policy differs; external sync has not been changed"
                );
                folder["paused"].as_bool().context("missing paused state")?
            };
            let j = Journal {
                daemon: daemon.clone(),
                owner: Uuid::new_v4(),
                registration: registration.id,
                vault: registration.vault,
                descriptor: descriptor.clone(),
                path,
                owns_folder,
                owns_hub: !hub_exists,
                original_paused,
                changed_pause: false,
                desired_paused: original_paused,
                remove_requested: false,
                last_connected_at: None,
                phase: if owns_folder {
                    Phase::Preparing
                } else {
                    Phase::Reused
                },
            };
            // Ownership is durable before POST; retries can read back a lost response.
            self.save(&j)?;
            j
        };
        if j.owns_folder {
            let config = api.config()?;
            if j.owns_hub {
                api.add_device(&hub(&j))?;
            } else {
                ensure!(
                    devices(&config)?
                        .iter()
                        .any(|d| d["deviceID"] == j.descriptor.hub_device_id),
                    "external hub record disappeared"
                );
            }
            let exists = folders(&api.config()?)?
                .iter()
                .any(|f| f["id"] == j.descriptor.folder_id);
            if !exists {
                ensure!(
                    j.phase == Phase::Preparing,
                    "owned folder was removed externally; explicit recovery required"
                );
                // A crash before creation must not authorize later nonempty data.
                ensure!(
                    j.path.read_dir()?.next().is_none(),
                    "destination changed before creation"
                );
                api.add_paused_folder(&json!({"id":j.descriptor.folder_id,"label":owned_label(&j),"path":j.path,"type":"receiveonly","paused":true,"devices":[{"deviceID":j.daemon.device_id},{"deviceID":j.descriptor.hub_device_id}]}))?;
            }
            let folder = api.folder(&j.descriptor.folder_id)?;
            validate_owned(&j, &folder)?;
            if j.phase == Phase::Preparing {
                ensure!(
                    folder["paused"] == true,
                    "unfinished folder unexpectedly unpaused"
                );
                let lines = j
                    .descriptor
                    .ignores
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                api.set_ignores(&j.descriptor.folder_id, &lines)?;
                j.phase = Phase::Receiving;
                self.save(&j)?;
            }
            ensure!(
                policy_matches(
                    &api.ignores(&j.descriptor.folder_id)?,
                    &j.descriptor.ignores
                )?,
                "ignore policy changed; will not unpause"
            );
            api.patch_folder(&j.descriptor.folder_id, &json!({"paused":j.desired_paused}))?;
        }
        self.status_locked(&mut j, &api)
    }
    /// Explicit user action changes only the selected folder, across all peers.
    pub fn pause(&self, paused: bool) -> Result<LocalStatus> {
        let _lock = private::lock(&self.state)?;
        let mut j = self.load()?;
        ensure!(
            !j.remove_requested && j.phase != Phase::Preparing,
            "folder is not ready"
        );
        let api = j.daemon.connect()?;
        let folder = api.folder(&j.descriptor.folder_id)?;
        validate_path(&folder, &j.path)?;
        if !paused {
            ensure!(
                policy_matches(
                    &api.ignores(&j.descriptor.folder_id)?,
                    &j.descriptor.ignores
                )?,
                "ignore policy changed; will not resume"
            );
        }
        j.desired_paused = paused;
        j.changed_pause = true;
        self.save(&j)?;
        api.patch_folder(&j.descriptor.folder_id, &json!({"paused":paused}))?;
        self.status_locked(&mut j, &api)
    }
    /// Read-only first-receive gate for Settings. This does not change folder
    /// type; crash-safe promotion is a separate operation. The gate always
    /// obtains new service observations and scans the authenticated local daemon.
    pub fn first_receive_ready(
        &self,
        enrollment: &crate::enrollment::Enrollment,
        service: &crate::pairing::Service,
    ) -> Result<bool> {
        let _lock = private::lock(&self.state)?;
        let j = self.load()?;
        ensure!(
            j.owns_folder && !j.remove_requested && j.phase == Phase::Receiving,
            "first receive is not active"
        );
        let api = j.daemon.connect()?;
        self.receive_ready_locked(&j, &api, &|| enrollment.readiness(service))
    }
    /// Explicit controller step after pairing. Only an owned new replica can
    /// become bidirectional; external reused folders retain their mode.
    pub fn promote(&self, source: &impl crate::readiness::Source) -> Result<bool> {
        let _lock = private::lock(&self.state)?;
        let mut j = self.load()?;
        ensure!(
            j.owns_folder
                && !j.remove_requested
                && matches!(j.phase, Phase::Receiving | Phase::Promoting | Phase::Active),
            "promotion is not allowed"
        );
        let api = j.daemon.connect()?;
        let folder = api.folder(&j.descriptor.folder_id)?;
        validate_owned(&j, &folder)?;
        if folder["type"] == "sendreceive" {
            // A lost PATCH response can leave the durable intent behind the
            // daemon. Read-back completes that operation without another PATCH.
            j.phase = Phase::Active;
            self.save(&j)?;
            return Ok(true);
        }
        if !self.receive_ready_locked(&j, &api, source)? {
            return Ok(false);
        }
        j.phase = Phase::Promoting;
        self.save(&j)?;
        // No mode change is retried from journal intent alone: if PATCH never
        // happened, the next call obtains a fresh pair of observations above.
        api.patch_folder(&j.descriptor.folder_id, &json!({"type":"sendreceive"}))?;
        let promoted = api.folder(&j.descriptor.folder_id)?;
        validate_owned(&j, &promoted)?;
        ensure!(
            promoted["type"] == "sendreceive",
            "promotion was not applied"
        );
        j.phase = Phase::Active;
        self.save(&j)?;
        Ok(true)
    }
    fn receive_ready_locked(
        &self,
        j: &Journal,
        api: &Syncthing,
        source: &impl crate::readiness::Source,
    ) -> Result<bool> {
        let folder = api.folder(&j.descriptor.folder_id)?;
        validate_owned(j, &folder)?;
        if folder["paused"] != false
            || !policy_matches(
                &api.ignores(&j.descriptor.folder_id)?,
                &j.descriptor.ignores,
            )?
        {
            return Ok(false);
        }
        let first = source.observe()?;
        let observation = first.observation();
        ensure!(
            observation.registration_id == j.registration
                && observation.vault_id == j.vault
                && observation.device_id == j.daemon.device_id
                && observation.folder_id == j.descriptor.folder_id
                && observation.hub_device_id == j.descriptor.hub_device_id,
            "folder and readiness scope differ"
        );
        api.scan(&j.descriptor.folder_id)?;
        let second = source.observe()?;
        // Catch edits made while waiting for the second service observation.
        api.scan(&j.descriptor.folder_id)?;
        let status = api.status(&j.descriptor.folder_id)?;
        let current = api.folder(&j.descriptor.folder_id)?;
        validate_owned(j, &current)?;
        if current != folder
            || !policy_matches(
                &api.ignores(&j.descriptor.folder_id)?,
                &j.descriptor.ignores,
            )?
        {
            return Ok(false);
        }
        crate::readiness::interval_complete(&first, &second, &status)
    }
    pub fn status(&self) -> Result<LocalStatus> {
        let _lock = private::lock(&self.state)?;
        let mut j = self.load()?;
        let api = j.daemon.connect()?;
        self.status_locked(&mut j, &api)
    }
    /// Persist cooperative removal even if the daemon is offline. Does not revoke
    /// the service grant: caller must also reconcile Enrollment::remove.
    pub fn remove(&self) -> Result<LocalStatus> {
        let _lock = private::lock(&self.state)?;
        let mut j = self.load()?;
        // A completed removal relinquishes authority over a reused replica.
        // Repeating Remove must not restore its old pause state again after
        // the external owner has changed it.
        if j.phase == Phase::Removed {
            let api = j.daemon.connect()?;
            return self.status_locked(&mut j, &api);
        }
        j.remove_requested = true;
        self.save(&j)?;
        let api = j.daemon.connect()?;
        let config = api.config()?;
        if let Some(folder) = folders(&config)?
            .iter()
            .find(|f| f["id"] == j.descriptor.folder_id)
        {
            validate_path(folder, &j.path)?;
            if j.owns_folder {
                validate_owned(&j, folder)?;
                api.patch_folder(&j.descriptor.folder_id, &json!({"paused":true}))?;
                api.remove_folder(&j.descriptor.folder_id)?;
            } else if j.changed_pause {
                ensure!(
                    folder["paused"] == j.desired_paused || folder["paused"] == j.original_paused,
                    "external pause state changed"
                );
                api.patch_folder(
                    &j.descriptor.folder_id,
                    &json!({"paused":j.original_paused}),
                )?;
            }
        }
        if j.owns_hub {
            let config = api.config()?;
            if let Some(device) = devices(&config)?
                .iter()
                .find(|d| d["deviceID"] == j.descriptor.hub_device_id)
            {
                if !folders(&config)?
                    .iter()
                    .any(|f| shares(f, &j.descriptor.hub_device_id))
                {
                    let expected = hub(&j);
                    ensure!(
                        expected
                            .as_object()
                            .unwrap()
                            .iter()
                            .all(|(key, value)| device.get(key) == Some(value)),
                        "owned hub record changed; retained for review"
                    );
                    api.remove_device(&j.descriptor.hub_device_id)?;
                }
            }
        }
        j.phase = Phase::Removed;
        self.save(&j)?;
        self.status_locked(&mut j, &api)
    }
    /// Read the last observed connection even when REST is unavailable. No
    /// daemon start, service registration or filesystem mutation is performed.
    pub fn last_connected_at(&self) -> Result<Option<u64>> {
        Ok(self.load()?.last_connected_at)
    }
    fn status_locked(&self, j: &mut Journal, api: &Syncthing) -> Result<LocalStatus> {
        let removed = j.phase == Phase::Removed;
        let exists = folders(&api.config()?)?
            .iter()
            .any(|f| f["id"] == j.descriptor.folder_id);
        ensure!(
            removed || exists,
            "folder disappeared; explicit recovery required"
        );
        let folder = if exists {
            api.folder(&j.descriptor.folder_id)?
        } else {
            Value::Null
        };
        if exists {
            validate_path(&folder, &j.path)?;
        }
        let status = if exists {
            api.status(&j.descriptor.folder_id)
                .context("read folder status")?
        } else {
            Value::Null
        };
        // Syncthing has no live folder-errors runner while paused (404).
        // Null means unavailable here, not an assertion of zero errors.
        let errors = if exists && folder["paused"] != true {
            api.errors(&j.descriptor.folder_id)
                .context("read folder errors")?
        } else {
            Value::Null
        };
        let connected = api.connections()?["connections"][&j.descriptor.hub_device_id]["connected"]
            .as_bool()
            .unwrap_or(false);
        if connected && !removed {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            if j.last_connected_at
                .is_none_or(|last| now.saturating_sub(last) >= 60)
            {
                j.last_connected_at = Some(now);
                self.save(j)?;
            }
        }
        Ok(LocalStatus {
            reused: !j.owns_folder,
            removed,
            external_sync_retained: removed && !j.owns_folder && exists,
            folder,
            status,
            errors,
            hub_connected: connected,
            last_connected_at: j.last_connected_at,
        })
    }
    fn file(&self) -> PathBuf {
        self.state.join("folder.json")
    }
    fn load(&self) -> Result<Journal> {
        Ok(serde_json::from_slice(&private::read(&self.file())?)?)
    }
    fn save(&self, j: &Journal) -> Result<()> {
        private::write(&self.file(), &serde_json::to_vec(j)?)
    }
}
fn folders(config: &Value) -> Result<&Vec<Value>> {
    config["folders"]
        .as_array()
        .context("invalid folders inventory")
}
fn devices(config: &Value) -> Result<&Vec<Value>> {
    config["devices"]
        .as_array()
        .context("invalid devices inventory")
}
fn shares(folder: &Value, id: &str) -> bool {
    folder["devices"]
        .as_array()
        .is_some_and(|devices| devices.iter().any(|d| d["deviceID"] == id))
}
fn validate_path(folder: &Value, path: &Path) -> Result<()> {
    let actual =
        Path::new(folder["path"].as_str().context("missing folder path")?).canonicalize()?;
    ensure!(actual == path, "folder path changed");
    Ok(())
}
fn validate_owned(j: &Journal, folder: &Value) -> Result<()> {
    validate_path(folder, &j.path)?;
    ensure!(
        folder["label"] == owned_label(j)
            && match j.phase {
                Phase::Active => folder["type"] == "sendreceive",
                Phase::Promoting =>
                    folder["type"] == "receiveonly" || folder["type"] == "sendreceive",
                _ => folder["type"] == "receiveonly",
            }
            && shares(folder, &j.descriptor.hub_device_id)
            && shares(folder, &j.daemon.device_id),
        "owned folder configuration changed"
    );
    Ok(())
}
fn owned_label(j: &Journal) -> String {
    format!("Tessera Sync {}", j.owner)
}
fn hub(j: &Journal) -> Value {
    json!({"deviceID":j.descriptor.hub_device_id,"addresses":[j.descriptor.hub_address],"introducer":true,"skipIntroductionRemovals":true,"autoAcceptFolders":false})
}
fn policy_matches(value: &Value, expected: &[String]) -> Result<bool> {
    let actual = value.get("ignore").context("missing ignore policy")?;
    Ok(*actual == json!(expected) || expected.is_empty() && actual.is_null())
}
