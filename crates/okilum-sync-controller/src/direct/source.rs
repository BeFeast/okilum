//! Explicit source enrollment into an already running, pinned managed instance.
//! No lifecycle operations, external adoption, override/revert, or file deletion.
use super::*;
use crate::daemon::{self, DaemonIdentity};
use serde_json::{json, Value};
use std::fs;
#[cfg(unix)]
use std::{io::Write, os::unix::fs::OpenOptionsExt};

// The Reader's canonical per-machine exclusions, plus the approved worktree
// exclusions from sync-enrollment.md. Never use FIXTURE_IGNORES as live policy.
pub(super) const IGNORES: &[&str] = &[
    ".okilum-index",
    ".okilum-save-*",
    "/.claude/worktrees",
    "/worktrees",
    ".okilum-create-*",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    #[serde(default)]
    history: history::Policy,
    #[serde(default)]
    create_ignores: create_ignores::Policy,
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
        let root = existing::VaultRoot::open(vault)
            .with_context(|| format!("open vault to share {}", vault.display()))?;
        let path = vault
            .canonicalize()
            .with_context(|| format!("resolve vault to share {}", vault.display()))?;
        root.verify(&path)?;
        let configs = self.configuration_paths()?;
        // Existing external enrollment must be recognized before touching a
        // managed runtime that may never have been prepared on this computer.
        let saved = self
            .shared
            .db
            .lock()
            .unwrap()
            .sources
            .iter()
            .any(|s| s.path == path);
        if !saved {
            let marker = path.join(".stfolder");
            match fs::symlink_metadata(&marker) {
                Ok(_) => {
                    anyhow::bail!("existing Syncthing vault requires Choose existing synced vault")
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("inspect Syncthing marker {}", marker.display()))
                }
            }
            if daemon::discover(&configs)
                .folders()?
                .iter()
                .any(|(_, folder)| folder == &path)
            {
                anyhow::bail!("existing Syncthing vault requires Choose existing synced vault");
            }
        }
        #[cfg(any(target_os = "linux", windows))]
        let daemon = daemon::managed_identity(prepared);
        #[cfg(target_os = "macos")]
        let daemon = crate::sidecar::macos::bootstrap::managed_source_identity(prepared);
        let daemon = daemon.with_context(|| {
            format!("open prepared managed Sync runtime {}", prepared.display())
        })?;
        ensure!(
            configs.len() == 1
                && configs[0].canonicalize().with_context(|| format!(
                    "resolve selected Sync configuration {}",
                    configs[0].display()
                ))? == daemon.config_file,
            "select the managed Sync connection before sharing"
        );
        for state in [self.shared.state.as_path(), prepared] {
            ensure!(
                !state
                    .canonicalize()
                    .with_context(|| format!("resolve Sync state {}", state.display()))?
                    .starts_with(&path),
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
                let id = format!("okilum-{}", Uuid::new_v4());
                no_overlap(&configs, &path, &id)?;
                let ignores = policy(&path)?;
                let label = path
                    .file_name()
                    .context("vault name missing")?
                    .to_string_lossy();
                let source = Source {
                    history: history::Policy::default(),
                    create_ignores: create_ignores::Policy::current(),
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
        let _admission = crate::admission::configuration(&daemon, &source.id, &path)?;
        no_overlap(&configs, &path, &source.id)?;
        root.verify(&path)?;
        self.check()?;
        if source.active {
            self.apply_source_ignores(&mut source)?;
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
                    .canonicalize()
                    .context("resolve shared source folder path")?
                    == path,
            "shared source binding changed"
        );
        ensure!(
            fs::read(path.join(".stignore")).with_context(|| format!(
                "read shared vault ignore policy {}",
                path.join(".stignore").display()
            ))? == source.ignores,
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
        Err(e) => Err(e).with_context(|| format!("inspect ignore policy {}", file.display())),
        Ok(m) => {
            ensure!(
                m.is_file() && !m.file_type().is_symlink() && m.len() <= 65536,
                "ignore policy requires explicit review"
            );
            let bytes = fs::read(&file)
                .with_context(|| format!("read ignore policy {}", file.display()))?;
            if std::str::from_utf8(&bytes)?
                .lines()
                .any(|s| s.starts_with("#include"))
            {
                return shared_ignores::validate_source_file(path);
            }
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
#[cfg(unix)]
fn seed_policy(path: &Path, expected: &[u8]) -> Result<()> {
    let file = path.join(".stignore");
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&file)
    {
        Ok(mut output) => {
            output
                .write_all(expected)
                .with_context(|| format!("write ignore policy {}", file.display()))?;
            output
                .sync_all()
                .with_context(|| format!("persist ignore policy {}", file.display()))?;
            fs::File::open(path)
                .and_then(|directory| directory.sync_all())
                .with_context(|| format!("persist shared vault directory {}", path.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                policy(path)? == expected,
                "ignore policy changed; explicit review required"
            );
        }
        Err(e) => {
            return Err(e).with_context(|| format!("create ignore policy {}", file.display()))
        }
    }
    Ok(())
}

#[cfg(windows)]
fn seed_policy(path: &Path, expected: &[u8]) -> Result<()> {
    let directory = okilum_core::windows_files::Directory::open(path)?;
    let name = std::ffi::OsStr::new(".stignore");
    match directory.open_file(name) {
        Ok(_) => ensure!(
            policy(path)? == expected,
            "ignore policy changed; explicit review required"
        ),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            directory.create(name, expected)?;
            ensure!(
                policy(path)? == expected,
                "ignore policy changed; explicit review required"
            );
        }
        Err(error) => return Err(error),
    }
    Ok(())
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
    fn apply_source_ignores(&self, source: &mut Source) -> Result<()> {
        if source.create_ignores.complete {
            return Ok(());
        }
        ensure!(
            !source.history.has_pending(),
            "Ignore migration waits for the pending history intent"
        );
        let api = self.source_history_binding(source)?;
        let bytes = create_ignores::Policy::read(&source.path, source.root_identity)?;
        let expected: Vec<_> = std::str::from_utf8(&source.ignores)?
            .lines()
            .map(str::to_owned)
            .collect();
        if source
            .create_ignores
            .prepare(&api.folder(&source.id)?, &bytes, &expected)?
        {
            self.save_source(source)?;
            source.create_ignores.apply(
                &api,
                &source.id,
                &source.path,
                source.root_identity,
                || self.check(),
            )?;
            source.ignores = format!("{}\n", create_ignores::lines().join("\n")).into_bytes();
            source.create_ignores.finish();
        }
        self.save_source(source)
    }
    pub(super) fn reconcile_source_ignores(&self, warnings: &mut Vec<PolicyWarning>) {
        let sources = self.shared.db.lock().unwrap().sources.clone();
        for mut source in sources.into_iter().filter(|s| s.active) {
            if let Err(error) = self.apply_source_ignores(&mut source) {
                warnings.push(PolicyWarning {
                    path: source.path.clone(),
                    message: format!("{error:#}"),
                });
            }
        }
    }
    pub(super) fn record_source_ignore_policy(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let source = self
            .shared
            .db
            .lock()
            .unwrap()
            .sources
            .iter()
            .find(|s| s.active && s.path == path)
            .cloned();
        if let Some(mut source) = source {
            self.source_history_binding(&source)?;
            source.ignores = bytes.to_vec();
            source.create_ignores = create_ignores::Policy::current();
            self.save_source(&source)?;
        }
        Ok(())
    }
    pub(super) fn reconcile_source_shared_ignores(&self, warnings: &mut Vec<PolicyWarning>) {
        let sources = self.shared.db.lock().unwrap().sources.clone();
        for source in sources.into_iter().filter(|s| s.active) {
            let result = self.source_history_binding(&source).and_then(|_| {
                self.reconcile_shared_ignore(&source.path, source.root_identity, &source.ignores)
            });
            match result {
                Ok(Some(bytes)) => {
                    if let Err(error) = self.record_source_ignore_policy(&source.path, &bytes) {
                        warnings.push(PolicyWarning {
                            path: source.path.clone(),
                            message: format!("{error:#}"),
                        });
                    }
                }
                Ok(None) => {}
                Err(error) => warnings.push(PolicyWarning {
                    path: source.path.clone(),
                    message: format!("{error:#}"),
                }),
            }
        }
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

pub(super) fn validate_removal(db: &Database, vault: &ExistingVault, before: &Value) -> Result<()> {
    for source in db.sources.iter().filter(|s| {
        s.active && s.path == vault.path && s.daemon == vault.daemon && s.id == vault.folder_id
    }) {
        ensure!(
            !source.history.has_pending() && !source.create_ignores.has_pending(),
            "Removal waits for the existing source policy intent"
        );
        let mut expected = source
            .witness
            .clone()
            .context("Source ownership witness missing")?;
        // Source enrollment can share with further explicitly authorized peers.
        // The removal changes only its named member; every other field stays exact.
        expected["devices"] = before["devices"].clone();
        expected["paused"] = before["paused"].clone();
        ensure!(
            expected == *before,
            "Source configuration changed before removal"
        );
    }
    Ok(())
}

pub(super) fn record_removed_membership(db: &mut Database, vault: &ExistingVault, after: &Value) {
    for source in db
        .sources
        .iter_mut()
        .filter(|s| s.path == vault.path && s.daemon == vault.daemon && s.id == vault.folder_id)
    {
        source.witness = Some(after.clone());
    }
}

#[cfg(all(test, unix))]
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
