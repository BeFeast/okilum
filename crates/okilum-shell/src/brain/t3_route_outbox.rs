//! Exact future-target adoption requests survive uncertain delivery and restart.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pending {
    schema: String,
    pub workspace: Value,
    pub request: Value,
}
impl Pending {
    pub fn prepare(workspace: Value, review: Value) -> Self {
        Self {
            schema: "tessera-t3-target-outbox/v1".into(),
            workspace,
            request: json!({"operation_id":Uuid::new_v4().to_string(),"review":review}),
        }
    }
    pub fn wire(&self) -> Value {
        json!({"op":"t3_target_adopt","request":self.request})
    }
}
pub(crate) struct RouteJournal {
    root: PathBuf,
    workspace: Value,
}
impl RouteJournal {
    pub fn open(workspace: &Value) -> Result<Self, String> {
        let brain = workspace["brain_id"]
            .as_str()
            .ok_or("Missing workspace identity")?;
        Uuid::parse_str(brain).map_err(|_| "Invalid workspace identity")?;
        if !workspace.is_object() || workspace["root"].as_str().is_none_or(str::is_empty) {
            return Err("Incomplete workspace identity".into());
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .ok_or("No private configuration directory")?;
        if !base.is_absolute() {
            return Err("Private configuration directory must be absolute".into());
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(workspace).map_err(|_| "Invalid workspace identity")?
            )
        );
        // Merely opening or reviewing Connections creates no files. Publication
        // begins only after the explicit adoption click retains an intent.
        Ok(Self {
            root: base.join("okilum/t3-target-outbox").join(digest),
            workspace: workspace.clone(),
        })
    }
    fn path(&self, pending: &Pending, extension: &str) -> Result<PathBuf, String> {
        let id = pending.request["operation_id"]
            .as_str()
            .ok_or("Missing adoption identity")?;
        let uuid = Uuid::parse_str(id).map_err(|_| "Invalid adoption identity")?;
        if uuid.to_string() != id
            || pending.workspace != self.workspace
            || pending.schema != "tessera-t3-target-outbox/v1"
        {
            return Err("Target adoption belongs to another workspace or schema.".into());
        }
        Ok(self.root.join(format!("{id}.{extension}")))
    }
    pub fn retain(&self, pending: &Pending) -> Result<(), String> {
        write_once(
            &self.path(pending, "json")?,
            &serde_json::to_vec(pending).map_err(|_| "Cannot encode target adoption")?,
        )
    }
    fn valid_outcome(&self, pending: &Pending, outcome: &Value) -> bool {
        outcome["schema"] == "tessera-t3-target-outcome/v1"
            && outcome["workspace"] == pending.workspace
            && outcome["request"] == pending.request
            && match outcome["status"].as_str() {
                Some("not_applied") => {
                    outcome["receipt"].is_null()
                        && outcome["reason"].as_str().is_some_and(|s| !s.is_empty())
                }
                Some("committed") => {
                    let r = &outcome["receipt"];
                    r["schema"] == "tessera-t3-target-receipt/v1"
                        && r["operation_id"] == pending.request["operation_id"]
                        && r["generation_id"].as_str().is_some_and(|s| !s.is_empty())
                        && r["future_only"] == true
                        && outcome["reason"].is_null()
                }
                _ => false,
            }
    }
    pub fn acknowledge(&self, pending: &Pending, outcome: &Value) -> Result<(), String> {
        if !self.valid_outcome(pending, outcome) {
            return Err("Target adoption acknowledgement does not match the retained request. Recover the same delivery.".into());
        }
        if std::fs::read(self.path(pending, "json")?)
            .map_err(|_| "Cannot read retained adoption")?
            != serde_json::to_vec(pending).unwrap()
        {
            return Err("Retained target adoption changed.".into());
        }
        let mut normalized = outcome.clone();
        if normalized["status"] == "committed" {
            normalized["receipt"]["replayed"] = json!(false);
        }
        write_once(
            &self.path(pending, "done")?,
            &serde_json::to_vec(&json!({"pending":pending,"outcome":normalized})).unwrap(),
        )
    }
    pub fn pending(&self) -> Result<Vec<Pending>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(_) => return Err("Cannot read target adoption history.".into()),
        };
        let mut pending = vec![];
        for entry in entries {
            let path = entry
                .map_err(|_| "Cannot read target adoption entry")?
                .path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let p: Pending = serde_json::from_slice(
                &std::fs::read(&path).map_err(|_| "Cannot read target adoption")?,
            )
            .map_err(|_| "Invalid retained target adoption")?;
            if path != self.path(&p, "json")? {
                return Err("Target adoption filename does not match its identity.".into());
            }
            match std::fs::read(self.path(&p, "done")?) {
                Ok(bytes) => {
                    let done: Value = serde_json::from_slice(&bytes)
                        .map_err(|_| "Invalid target adoption acknowledgement")?;
                    if done["pending"] != serde_json::to_value(&p).unwrap()
                        || !self.valid_outcome(&p, &done["outcome"])
                    {
                        return Err("Target adoption acknowledgement changed.".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => pending.push(p),
                Err(_) => return Err("Cannot read target adoption acknowledgement.".into()),
            }
        }
        pending.sort_by(|a, b| {
            a.request["operation_id"]
                .as_str()
                .cmp(&b.request["operation_id"].as_str())
        });
        Ok(pending)
    }
}

// Immutable publication: fsync content, create-only hard link, fsync directories.
fn write_once(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let root = path.parent().ok_or("Invalid adoption history path")?;
    let mut missing = Vec::new();
    let mut parent = root;
    while !parent.exists() {
        missing.push(parent);
        parent = parent
            .parent()
            .ok_or("Invalid adoption history directory")?;
    }
    std::fs::create_dir_all(root).map_err(|_| "Cannot create private adoption history")?;
    for directory in missing.iter().rev() {
        std::fs::File::open(directory.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|_| "Cannot sync adoption history directory")?;
    }
    let temp = root.join(format!(".adoption-{}.tmp", Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match std::fs::hard_link(&temp, path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(path)? != bytes {
                    return Err(std::io::Error::other("Target adoption identity conflict"));
                }
            }
            Err(e) => return Err(e),
        }
        std::fs::File::open(root)?.sync_all()
    })();
    let _ = std::fs::remove_file(temp);
    result.map_err(|_| "Cannot durably publish target adoption; reload its exact retained identity before retrying.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    struct Fixture {
        root: PathBuf,
        journal: RouteJournal,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("okilum-route-outbox-{}", Uuid::new_v4()));
            let journal = RouteJournal {
                root: root.clone(),
                workspace: json!({"brain_id":"fixture","root":"/fixture","managed":true}),
            };
            Self { root, journal }
        }
        fn pending(&self) -> Pending {
            Pending::prepare(
                self.journal.workspace.clone(),
                json!({"candidate":{"environment_id":"new"},"guard":{"revision":"exact-revision"},"ready":true}),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn outcome(p: &Pending) -> Value {
        json!({"schema":"tessera-t3-target-outcome/v1","workspace":p.workspace,"request":p.request,"status":"committed","reason":null,"receipt":{"schema":"tessera-t3-target-receipt/v1","operation_id":p.request["operation_id"],"generation_id":"generation-new","future_only":true,"replayed":false}})
    }
    #[test]
    fn opening_and_loading_empty_outbox_does_not_persist_anything() {
        let workspace = json!({"brain_id":Uuid::new_v4().to_string(),"root":"/fixture/brain","records_dir":"records","managed":true});
        let journal = RouteJournal::open(&workspace).unwrap();
        assert!(!journal.root.exists());
        assert!(journal.pending().unwrap().is_empty());
        assert!(!journal.root.exists());
    }
    #[test]
    fn response_loss_restart_retains_exact_wire_and_acknowledges_only_bound_receipt() {
        let f = Fixture::new();
        let p = f.pending();
        f.journal.retain(&p).unwrap();
        // Fake transport loses the first acknowledgement; the only transmitted
        // bodies are production Pending::wire results, never rebuilt requests.
        let mut transmitted = vec![p.wire()];
        let reopened = RouteJournal {
            root: f.root.clone(),
            workspace: f.journal.workspace.clone(),
        };
        let retained = reopened.pending().unwrap();
        assert_eq!(retained, vec![p.clone()]);
        assert_eq!(retained[0].wire(), p.wire());
        transmitted.push(retained[0].wire());
        assert_eq!(transmitted[0], transmitted[1]);
        assert_eq!(transmitted[0]["op"], "t3_target_adopt");
        let mut forged = outcome(&p);
        forged["request"]["operation_id"] = json!(Uuid::new_v4().to_string());
        assert!(reopened.acknowledge(&p, &forged).is_err());
        assert_eq!(reopened.pending().unwrap().len(), 1);
        let good = outcome(&p);
        reopened.acknowledge(&p, &good).unwrap();
        assert!(reopened.pending().unwrap().is_empty());
        let mut replay = good;
        replay["receipt"]["replayed"] = json!(true);
        reopened.acknowledge(&p, &replay).unwrap();
    }
    #[test]
    fn changed_payload_same_id_and_wrong_workspace_fail_closed() {
        let f = Fixture::new();
        let p = f.pending();
        f.journal.retain(&p).unwrap();
        let mut changed = p.clone();
        changed.request["review"]["candidate"]["environment_id"] = json!("other");
        assert!(f.journal.retain(&changed).is_err());
        let wrong = RouteJournal {
            root: f.root.clone(),
            workspace: json!({"brain_id":"other"}),
        };
        assert!(wrong.pending().is_err());
        let mut rejected = outcome(&p);
        rejected["status"] = json!("not_applied");
        rejected["receipt"] = Value::Null;
        rejected["reason"] = json!("revision_changed");
        f.journal.acknowledge(&p, &rejected).unwrap();
        assert!(f.journal.pending().unwrap().is_empty());
    }
}
