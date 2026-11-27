//! Retained settings changes survive a closed desktop and ambiguous delivery.
use super::native_outbox::{write_once, InboxJournal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub operation_id: String,
    pub expected_revision: u64,
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pending {
    schema: String,
    pub workspace: Value,
    pub request: Request,
}
impl Pending {
    pub fn wire(&self) -> Value {
        let mut value = serde_json::to_value(&self.request).unwrap();
        value["op"] = json!("suggestions_set");
        value
    }
}
pub(crate) struct SuggestionsJournal {
    root: PathBuf,
    workspace: Value,
}
impl SuggestionsJournal {
    pub fn open(workspace: &Value) -> Result<Self, String> {
        let inner = InboxJournal::open(workspace)?;
        Ok(Self {
            root: inner.root.join("suggestions-settings"),
            workspace: workspace.clone(),
        })
    }
    pub fn prepare(&self, expected_revision: u64, enabled: bool) -> Pending {
        Pending {
            schema: "okilum-suggestions-outbox/v1".into(),
            workspace: self.workspace.clone(),
            request: Request {
                operation_id: Uuid::new_v4().to_string(),
                expected_revision,
                enabled,
            },
        }
    }
    fn path(&self, pending: &Pending, extension: &str) -> Result<PathBuf, String> {
        let id = Uuid::parse_str(&pending.request.operation_id)
            .map_err(|_| "Invalid Suggestions change identity")?;
        if pending.schema != "okilum-suggestions-outbox/v1"
            || pending.workspace != self.workspace
            || id.to_string() != pending.request.operation_id
        {
            return Err("Suggestions change belongs to another workspace.".into());
        }
        Ok(self.root.join(format!("{id}.{extension}")))
    }
    pub fn retain(&self, pending: &Pending) -> Result<(), String> {
        write_once(
            &self.path(pending, "json")?,
            &serde_json::to_vec(pending).map_err(|e| e.to_string())?,
        )
    }
    fn valid_receipt(&self, pending: &Pending, outcome: &Value) -> bool {
        if outcome["schema"] != "okilum-suggestions-outcome/v1"
            || outcome["workspace"] != pending.workspace
            || outcome["request"] != serde_json::to_value(&pending.request).unwrap()
        {
            return false;
        }
        match outcome["status"].as_str() {
            Some("not_applied") => {
                outcome["receipt"].is_null()
                    && matches!(
                        outcome["reason"].as_str(),
                        Some("provider_unavailable" | "revision_changed" | "identity_conflict")
                    )
            }
            Some("committed") => {
                let receipt = &outcome["receipt"];
                outcome["reason"].is_null()
                    && receipt["schema"] == "okilum-suggestions-receipt/v1"
                    && receipt["workspace"] == pending.workspace
                    && receipt["request"] == serde_json::to_value(&pending.request).unwrap()
                    && receipt["actor"].as_str().is_some_and(|s| !s.is_empty())
                    && pending
                        .request
                        .expected_revision
                        .checked_add(1)
                        .is_some_and(|revision| receipt["revision"].as_u64() == Some(revision))
                    && receipt["enabled"] == pending.request.enabled
                    && receipt["replayed"].is_boolean()
            }
            _ => false,
        }
    }
    pub fn acknowledge(&self, pending: &Pending, receipt: &Value) -> Result<(), String> {
        if !self.valid_receipt(pending, receipt) {
            return Err("Suggestions acknowledgement does not match the retained change. Recover the same delivery.".into());
        }
        if std::fs::read(self.path(pending, "json")?).map_err(|e| e.to_string())?
            != serde_json::to_vec(pending).unwrap()
        {
            return Err("The retained Suggestions change has changed.".into());
        }
        let mut receipt = receipt.clone();
        if receipt["status"] == "committed" {
            receipt["receipt"]["replayed"] = json!(false);
        }
        write_once(
            &self.path(pending, "done")?,
            &serde_json::to_vec(&json!({"request":pending,"receipt":receipt})).unwrap(),
        )
    }
    pub fn pending(&self) -> Result<Vec<Pending>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        let mut result = vec![];
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let pending: Pending =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if path != self.path(&pending, "json")? {
                return Err("Suggestions journal filename changed.".into());
            }
            match std::fs::read(self.path(&pending, "done")?) {
                Ok(bytes) => {
                    let done: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    if done["request"] != serde_json::to_value(&pending).unwrap()
                        || !self.valid_receipt(&pending, &done["receipt"])
                    {
                        return Err("Suggestions receipt identity changed.".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => result.push(pending),
                Err(e) => return Err(e.to_string()),
            }
        }
        result.sort_by(|a, b| a.request.operation_id.cmp(&b.request.operation_id));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn workspace() -> Value {
        json!({"brain_id":"bb000000-0000-4000-8000-000000000200","root":"/fixture/brain","records_dir":"records","managed":true})
    }
    fn journal(root: &std::path::Path) -> SuggestionsJournal {
        SuggestionsJournal {
            root: root.to_path_buf(),
            workspace: workspace(),
        }
    }
    fn outcome(p: &Pending) -> Value {
        json!({"schema":"okilum-suggestions-outcome/v1","workspace":p.workspace,"request":p.request,
        "status":"committed","reason":null,"receipt":{
            "schema":"okilum-suggestions-receipt/v1","workspace":p.workspace,"request":p.request,
            "actor":"local operator","revision":p.request.expected_revision+1,"enabled":p.request.enabled,"replayed":false
        }})
    }
    #[test]
    fn settings_delivery_reopens_exactly_and_only_bound_outcomes_retire_it() {
        let root = std::env::temp_dir().join(format!("suggestions-outbox-{}", Uuid::new_v4()));
        let j = journal(&root);
        let p = j.prepare(4, true);
        j.retain(&p).unwrap();
        assert_eq!(journal(&root).pending().unwrap(), vec![p.clone()]);
        let good = outcome(&p);
        for pointer in [
            "/workspace/root",
            "/request/enabled",
            "/receipt/workspace/root",
            "/receipt/request/operation_id",
            "/receipt/enabled",
            "/receipt/revision",
            "/receipt/replayed",
        ] {
            let mut wrong = good.clone();
            *wrong.pointer_mut(pointer).unwrap() = Value::Null;
            assert!(j.acknowledge(&p, &wrong).is_err(), "{pointer}");
            assert_eq!(journal(&root).pending().unwrap(), vec![p.clone()]);
        }
        let mut wrong_revision = good.clone();
        wrong_revision["receipt"]["revision"] = json!(p.request.expected_revision + 2);
        assert!(j.acknowledge(&p, &wrong_revision).is_err());
        assert_eq!(journal(&root).pending().unwrap(), vec![p.clone()]);
        // A disk failure after backend acceptance must retain the same delivery.
        std::fs::create_dir(j.path(&p, "done").unwrap()).unwrap();
        assert!(j.acknowledge(&p, &good).is_err());
        std::fs::remove_dir(j.path(&p, "done").unwrap()).unwrap();
        assert_eq!(journal(&root).pending().unwrap(), vec![p.clone()]);
        let mut replay = good.clone();
        replay["receipt"]["replayed"] = json!(true);
        j.acknowledge(&p, &replay).unwrap();
        j.acknowledge(&p, &good).unwrap();
        assert!(journal(&root).pending().unwrap().is_empty());
        let p = j.prepare(5, false);
        j.retain(&p).unwrap();
        let refused = json!({"schema":"okilum-suggestions-outcome/v1","workspace":p.workspace,"request":p.request,"status":"not_applied","receipt":null,"reason":"revision_changed"});
        for reason in ["unsupported", "unknown"] {
            let mut uncertain = refused.clone();
            uncertain["reason"] = json!(reason);
            assert!(j.acknowledge(&p, &uncertain).is_err());
            assert_eq!(journal(&root).pending().unwrap(), vec![p.clone()]);
        }
        j.acknowledge(&p, &refused).unwrap();
        assert!(journal(&root).pending().unwrap().is_empty());
        let mut changed = journal(&root);
        changed.workspace["root"] = json!("/other/brain");
        assert!(changed.pending().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
