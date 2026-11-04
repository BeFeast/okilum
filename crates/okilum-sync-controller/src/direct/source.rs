//! Explicit source enrollment into an already running, pinned managed instance.
//! No lifecycle operations, external adoption, override/revert, or file deletion.
use super::*;
use crate::daemon::{self, DaemonIdentity};
use serde_json::{json, Value};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt};

// The Reader's canonical per-machine exclusions, plus the approved worktree
// exclusions from sync-enrollment.md. Never use FIXTURE_IGNORES as live policy.
pub(super) const IGNORES: &[&str] = &[
    ".okilum-index",
    ".okilum-save-*",
    "/.claude/worktrees",
    "/worktrees",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    #[serde(default)]
    history: history::Policy,
    id: String,
    pub(super) path: PathBuf,
    root_identity: (u64, u64),
    daemon: DaemonIdentity,
    ignores: Vec<u8>,
    folder: Value,
    witness: Option<Value>,
    pub(super) active: bool,
}
impl Controller {
    /// User consent to share this ordinary vault with this computer's approved
    /// peers, including computers paired before enrollment. `prepared` is the
    /// selected managed runtime's existing preparation directory, not a new home.
    pub fn share_vault(&self, prepared: &Path, vault: &Path) -> Result<Snapshot> {
        self.check()?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        let root = existing::VaultRoot::open(vault)?;
        let path = vault.canonicalize()?;
        root.verify(&path)?;
        let daemon = daemon::managed_identity(prepared)?;
        let configs = self.configuration_paths()?;
        ensure!(
            configs.len() == 1 && configs[0].canonicalize()? == daemon.config_file,
            "select the managed Sync connection before sharing"
        );
        for state in [self.shared.state.as_path(), prepared] {
            ensure!(
                !state.canonicalize()?.starts_with(&path),
                "operational state must be outside the vault"
            );
        }
        let identity = root.identity()?;
        let mut source = {
            let mut db = self.shared.db.lock().unwrap();
            if let Some(saved) = db.sources.iter().find(|s| s.path == path) {
                ensure!(
                    saved.daemon == daemon && saved.root_identity == identity,
                    "shared source identity changed"
                );
                saved.clone()
            } else {
                match fs::symlink_metadata(path.join(".stfolder")) {
                    Ok(_) => anyhow::bail!(
                        "existing Syncthing vault requires Choose existing synced vault"
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                let id = format!("okilum-{}", Uuid::new_v4());
                no_overlap(&configs, &path, &id)?;
                let ignores = policy(&path)?;
                let label = path
                    .file_name()
                    .context("vault name missing")?
                    .to_string_lossy();
                let source = Source {
                    history: history::Policy::default(),
                    folder: json!({"id":id,"label":label,"path":path,"type":"sendreceive","paused":true,
                        "devices":[{"deviceID":daemon.device_id}],"maxConflicts":-1,
                        "versioning":{"type":"staggered","params":{"maxAge":"2592000"}}}),
                    id,
                    path: path.clone(),
                    root_identity: identity,
                    daemon: daemon.clone(),
                    ignores,
                    witness: None,
                    active: false,
                };
                db.sources.push(source.clone());
                self.save(&db)?;
                source
            }
        };
        no_overlap(&configs, &path, &source.id)?;
        root.verify(&path)?;
        self.check()?;
        if source.active {
            ensure!(
                policy(&path)? == source.ignores,
                "ignore policy changed; explicit review required"
            );
        } else {
            seed_policy(&path, &source.ignores)?;
        }
        let api = daemon.connect()?;
        let exists = api.config()?["folders"]
            .as_array()
            .context("invalid folder inventory")?
            .iter()
            .any(|f| f["id"] == source.id);
        if !exists {
            ensure!(
                !source.active && source.witness.is_none(),
                "shared source folder disappeared"
            );
            no_overlap(&configs, &path, &source.id)?;
            root.verify(&path)?;
            self.check()?;
            api.add_paused_folder(&source.folder)?;
        }
        let folder = api.folder(&source.id)?;
        if !source.active {
            let mut expected = source
                .witness
                .clone()
                .unwrap_or_else(|| source.folder.clone());
            let mut actual = folder.clone();
            // A lost unpause acknowledgement is recoverable only from a saved witness.
            if source.witness.is_some() {
                expected["paused"] = json!(true);
                actual["paused"] = json!(true);
            }
            ensure!(
                if source.witness.is_some() {
                    actual == expected
                } else {
                    requested_fields(&actual, &expected)
                },
                "source folder configuration changed"
            );
            source.witness = Some(folder.clone());
            self.save_source(&source)?;
        }
        ensure!(
            folder["id"] == source.id
                && folder["type"] == "sendreceive"
                && Path::new(folder["path"].as_str().context("source path missing")?)
                    .canonicalize()?
                    == path,
            "shared source binding changed"
        );
        ensure!(
            fs::read(path.join(".stignore"))? == source.ignores,
            "ignore policy changed; explicit review required"
        );
        root.verify(&path)?;
        self.check()?;
        if !source.active && folder["paused"] == true {
            api.patch_folder(&source.id, &json!({"paused":false}))?;
        }
        let attached = inspect_existing(&configs, &path)?;
        ensure!(
            !attached.paused,
            "shared source is paused; resume it explicitly"
        );
        root.verify(&path)?;
        let mut db = self.shared.db.lock().unwrap();
        source.active = true;
        *db.sources
            .iter_mut()
            .find(|s| s.id == source.id)
            .context("source intent missing")? = source.clone();
        if let Some(old) = db.attached.iter_mut().find(|v| v.folder_id == source.id) {
            ensure!(
                existing::same_binding(old, &attached),
                "source association changed"
            );
            *old = attached;
        } else {
            db.attached.push(attached);
        }
        for peer in db.peers.values_mut() {
            if !peer.allowed.contains(&source.id) {
                peer.allowed.push(source.id.clone());
            }
        }
        self.save(&db)?;
        snapshot(&db)
    }
    fn save_source(&self, source: &Source) -> Result<()> {
        let mut db = self.shared.db.lock().unwrap();
        *db.sources
            .iter_mut()
            .find(|s| s.id == source.id)
            .context("source intent missing")? = source.clone();
        self.save(&db)
    }
}
// New-folder read-back expands Syncthing defaults inside devices/versioning.
// Once recorded, the full expanded witness above must match exactly.
fn requested_fields(actual: &Value, expected: &Value) -> bool {
    match expected {
        Value::Object(fields) => fields
            .iter()
            .all(|(key, value)| requested_fields(&actual[key], value)),
        Value::Array(items) => actual.as_array().is_some_and(|observed| {
            observed.len() == items.len()
                && observed
                    .iter()
                    .zip(items)
                    .all(|(a, e)| requested_fields(a, e))
        }),
        _ => actual == expected,
    }
}
fn no_overlap(configs: &[PathBuf], path: &Path, id: &str) -> Result<()> {
    for (other_id, other) in daemon::discover(configs).folders()? {
        ensure!(
            other_id == id && other == path
                || !path.starts_with(&other) && !other.starts_with(path),
            "vault overlaps another Syncthing folder; use Choose existing synced vault"
        );
    }
    Ok(())
}
fn policy(path: &Path) -> Result<Vec<u8>> {
    let file = path.join(".stignore");
    match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(format!("{}\n", IGNORES.join("\n")).into_bytes())
        }
        Err(e) => Err(e.into()),
        Ok(m) => {
            ensure!(
                m.is_file() && !m.file_type().is_symlink() && m.len() <= 65536,
                "ignore policy requires explicit review"
            );
            let bytes = fs::read(&file)?;
            let text = std::str::from_utf8(&bytes)?;
            let lines = text.lines().map(str::trim).collect::<Vec<_>>();
            // Do not guess includes or precedence of user negations. Existing
            // approved policies are copied byte-for-byte, never rewritten.
            ensure!(
                lines.len() <= 1024
                    && lines.iter().all(|l| l.len() <= 4096
                        && !l.chars().any(char::is_control)
                        && !l.starts_with("#include")
                        && !l.contains('!'))
                    && IGNORES.iter().all(|p| lines.contains(p)),
                "ignore policy requires explicit review"
            );
            Ok(bytes)
        }
    }
}
fn seed_policy(path: &Path, expected: &[u8]) -> Result<()> {
    let file = path.join(".stignore");
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&file)
    {
        Ok(mut file) => {
            file.write_all(expected)?;
            file.sync_all()?;
            fs::File::open(path)?.sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                policy(path)? == expected,
                "ignore policy changed; explicit review required"
            );
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_policy_preserves_existing_bytes_and_refuses_unknown_precedence() -> Result<()> {
        let root = tempfile::tempdir()?;
        let canonical = policy(root.path())?;
        seed_policy(root.path(), &canonical)?;
        let custom = format!("// keep CRLF\r\n{}\r\nprivate\r\n", IGNORES.join("\r\n"));
        fs::write(root.path().join(".stignore"), &custom)?;
        seed_policy(root.path(), custom.as_bytes())?;
        assert_eq!(fs::read(root.path().join(".stignore"))?, custom.as_bytes());
        for unknown in ["#include policy", "!**\n.okilum-index", "private"] {
            fs::write(root.path().join(".stignore"), unknown)?;
            assert!(policy(root.path()).is_err());
            assert_eq!(fs::read_to_string(root.path().join(".stignore"))?, unknown);
        }
        Ok(())
    }
}

impl Controller {
    fn source_history_binding(&self, source: &Source) -> Result<okilum_sync::Syncthing> {
        ensure!(
            source.active && source.witness.is_some(),
            "Source history ownership missing"
        );
        let root = existing::VaultRoot::open(&source.path)?;
        root.verify(&source.path)?;
        ensure!(
            root.identity()? == source.root_identity,
            "Source directory identity changed"
        );
        let api = self.history_daemon(&source.daemon)?;
        let folder = api.folder(&source.id)?;
        ensure!(
            folder["id"] == source.id
                && folder["type"] == "sendreceive"
                && Path::new(folder["path"].as_str().context("Missing source path")?)
                    .canonicalize()?
                    == source.path,
            "Source history binding changed"
        );
        Ok(api)
    }
    fn apply_source_history(&self, source: &mut Source) -> Result<()> {
        let api = self.source_history_binding(source)?;
        if source.history.prepare(&api.folder(&source.id)?)? {
            self.save_source(source)?;
            self.check()?;
            let after = source.history.apply(&api, &source.id)?;
            source.witness = Some(after);
            source.history.finish();
            self.save_source(source)?;
        }
        Ok(())
    }
    pub(super) fn set_source_retention(&self, path: &Path, days: RetentionDays) -> Result<bool> {
        let source = self
            .shared
            .db
            .lock()
            .unwrap()
            .sources
            .iter()
            .find(|s| s.active && s.path == path)
            .cloned();
        let Some(mut source) = source else {
            return Ok(false);
        };
        self.apply_source_history(&mut source)?;
        source.history.days = days;
        self.save_source(&source)?;
        self.apply_source_history(&mut source)?;
        Ok(true)
    }
    pub(super) fn reconcile_source_history(&self, warnings: &mut Vec<history::HistoryWarning>) {
        let sources = self.shared.db.lock().unwrap().sources.clone();
        for mut source in sources.into_iter().filter(|s| s.active) {
            if let Err(error) = self.apply_source_history(&mut source) {
                warnings.push(history::HistoryWarning { path: source.path.clone(), message: format!("Previous Sync versions could not be updated: {error:#}. Check this folder and its Sync connection.") });
            }
        }
    }
}
pub(super) fn history_settings(db: &Database) -> Vec<RetentionSetting> {
    db.sources
        .iter()
        .filter(|s| s.active)
        .map(|s| RetentionSetting {
            path: s.path.clone(),
            days: s.history.days,
        })
        .collect()
}
