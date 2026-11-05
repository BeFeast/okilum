//! Durable, folder-scoped history policy for folders created by Okilum only.
use super::*;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct RetentionDays(u16);
impl Default for RetentionDays {
    fn default() -> Self {
        Self(30)
    }
}
impl TryFrom<u16> for RetentionDays {
    type Error = anyhow::Error;
    fn try_from(days: u16) -> Result<Self> {
        ensure!(
            (1..=3650).contains(&days),
            "History retention must be 1–3650 days"
        );
        Ok(Self(days))
    }
}
impl From<RetentionDays> for u16 {
    fn from(value: RetentionDays) -> Self {
        value.0
    }
}
impl RetentionDays {
    pub fn days(self) -> u16 {
        self.0
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetentionSetting {
    pub path: PathBuf,
    pub days: RetentionDays,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryWarning {
    pub path: PathBuf,
    pub message: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    #[serde(default)]
    pub days: RetentionDays,
    pending: Option<Change>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    before: Value,
    after: Value,
}
impl Policy {
    /// Start an intent before the PATCH. Exact before/after snapshots make lost
    /// acknowledgements recoverable without accepting unrelated config changes.
    pub fn prepare(&mut self, folder: &Value) -> Result<bool> {
        if let Some(change) = &self.pending {
            ensure!(
                folder == &change.before || folder == &change.after,
                "History policy changed during reconciliation"
            );
            return Ok(true);
        }
        let mut after = folder.clone();
        let versioning = &folder["versioning"];
        ensure!(
            versioning["fsPath"].as_str().is_none_or(str::is_empty)
                && versioning["fsType"]
                    .as_str()
                    .is_none_or(|t| t.is_empty() || t == "basic"),
            "Custom Sync version stores need explicit review"
        );
        let mut versioning = versioning.as_object().cloned().unwrap_or_default();
        versioning.insert("type".into(), json!("staggered"));
        let mut params = versioning
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        params.insert(
            "maxAge".into(),
            json!((u64::from(self.days.0) * 86400).to_string()),
        );
        versioning.insert("params".into(), Value::Object(params));
        after["versioning"] = Value::Object(versioning);
        if &after == folder {
            return Ok(false);
        }
        self.pending = Some(Change {
            before: folder.clone(),
            after,
        });
        Ok(true)
    }
    pub fn apply(&self, api: &okilum_sync::Syncthing, id: &str) -> Result<Value> {
        let change = self.pending.as_ref().context("History intent missing")?;
        let current = api.folder(id)?;
        if current == change.before {
            api.patch_folder(id, &json!({"versioning":change.after["versioning"]}))?;
        } else {
            ensure!(
                current == change.after,
                "History folder changed before PATCH"
            );
        }
        let after = api.folder(id)?;
        ensure!(
            after == change.after,
            "History policy readback changed configuration"
        );
        Ok(after)
    }
    pub fn finish(&mut self) {
        self.pending = None;
    }
    /// Only a journaled versioning transition may differ from the old witness.
    pub fn witness_folder(&self, folder: &Value) -> Result<Value> {
        let mut normalized = folder.clone();
        if let Some(change) = &self.pending {
            ensure!(
                folder == &change.before || folder == &change.after,
                "History transition witness changed"
            );
            normalized["versioning"] = change.before["versioning"].clone();
        }
        Ok(normalized)
    }
}
impl Controller {
    /// Explicit per-computer retention choice. External/legacy folders have no
    /// entry here and cannot acquire versioning authority through this API.
    pub fn set_history_retention(&self, path: &Path, days: RetentionDays) -> Result<Snapshot> {
        self.check()?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        #[cfg(target_os = "linux")]
        if self.set_source_retention(path, days)? {
            return self.snapshot();
        }
        ensure!(
            self.set_transfer_retention(path, days)?,
            "Only a folder created by Okilum has a managed history policy"
        );
        self.snapshot()
    }
    pub(super) fn history_daemon(
        &self,
        daemon: &crate::daemon::DaemonIdentity,
    ) -> Result<okilum_sync::Syncthing> {
        self.check()?;
        ensure!(
            self.configuration_paths()?
                .iter()
                .any(|p| p.canonicalize().is_ok_and(|p| p == daemon.config_file)),
            "History runtime is no longer selected"
        );
        daemon.connect()
    }
    pub(super) fn reconcile_history(&self) -> Result<()> {
        self.check()?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        let mut warnings = Vec::new();
        #[cfg(target_os = "linux")]
        self.reconcile_source_history(&mut warnings);
        self.reconcile_transfer_history(&mut warnings);
        *self.shared.history_warnings.lock().unwrap() = warnings;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn issue2045_policy_units_idempotence_and_lost_ack_guard() -> Result<()> {
        assert!(RetentionDays::try_from(0).is_err());
        assert!(serde_json::from_value::<RetentionDays>(json!(0)).is_err());
        let mut policy = Policy::default();
        let old = json!({"id":"owned","type":"sendreceive","versioning":{"type":"","params":{},"fsPath":"","fsType":"basic","cleanupIntervalS":3600},"maxConflicts":-1});
        assert!(policy.prepare(&old)?);
        let change = policy.pending.as_ref().unwrap().clone();
        assert_eq!(change.after["versioning"]["params"]["maxAge"], "2592000");
        assert_eq!(change.after["maxConflicts"], -1);
        let restored: Policy = serde_json::from_slice(&serde_json::to_vec(&policy)?)?;
        assert_eq!(restored.witness_folder(&change.after)?, old);
        let mut unrelated = change.after.clone();
        unrelated["paused"] = json!(true);
        assert!(policy.prepare(&unrelated).is_err());
        policy.finish();
        assert!(!policy.prepare(&change.after)?);
        policy.days = RetentionDays::try_from(90)?;
        assert!(policy.prepare(&change.after)?);
        assert_eq!(
            policy.pending.unwrap().after["versioning"]["params"]["maxAge"],
            "7776000"
        );
        Ok(())
    }
}
