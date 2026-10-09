//! Dedicated workspace-bound disposition delivery; exact requests survive restart.
use super::native_outbox::{write_once, InboxJournal};
use super::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Disposition {
    Rejected,
    Snoozed { until: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub operation_id: String,
    pub proposal_id: String,
    pub goal_id: Option<String>,
    pub expected_revision: String,
    pub disposition: Disposition,
    pub source: Value,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    schema: String,
    pub workspace: Value,
    pub path: String,
    pub request: Request,
}
impl Pending {
    pub fn wire(&self) -> Value {
        let mut value = serde_json::to_value(&self.request).unwrap();
        value["op"] = json!("proposal_disposition");
        value["_proposal_pending"] = serde_json::to_value(self).unwrap();
        value
    }
}
fn revision(value: &Value) -> bool {
    value.as_str().is_some_and(|v| {
        v.len() == 71 && v.starts_with("sha256:") && v[7..].bytes().all(|b| b.is_ascii_hexdigit())
    })
}
pub(super) fn valid_receipt(p: &Pending, receipt: &Value) -> bool {
    if receipt["outcome"] == "not_applied" {
        return valid_terminal_receipt(p, receipt);
    }
    let r = &p.request;
    receipt["operation_id"] == r.operation_id
        && receipt["proposal_id"] == r.proposal_id
        && receipt["path"] == p.path
        && receipt["previous_revision"] == r.expected_revision
        && revision(&receipt["revision"])
        && receipt["revision"] != receipt["previous_revision"]
        && receipt["actor"] == r.source["actor_id"]
        && receipt["disposition"] == serde_json::to_value(&r.disposition).unwrap()
        && receipt["at"].as_str().is_some_and(|v| {
            time::OffsetDateTime::parse(v, &time::format_description::well_known::Rfc3339).is_ok()
        })
        && receipt["replayed"].is_boolean()
}
pub(super) fn valid_terminal_receipt(p: &Pending, receipt: &Value) -> bool {
    let parse = |v: &Value| {
        v.as_str().and_then(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
        })
    };
    let Disposition::Snoozed { until } = &p.request.disposition else {
        return false;
    };
    receipt["schema"] == "tessera-proposal-terminal/v1"
        && receipt["outcome"] == "not_applied"
        && receipt["workspace"] == p.workspace
        && receipt["request"] == serde_json::to_value(&p.request).unwrap()
        && receipt["path"] == p.path
        && receipt["reason"] == "deadline_elapsed"
        && receipt["replayed"].is_boolean()
        && parse(&receipt["at"]).is_some_and(|at| {
            at.offset() == time::UtcOffset::UTC
                && parse(&json!(until)).is_some_and(|deadline| deadline <= at)
        })
}
pub(super) struct ProposalJournal {
    inner: InboxJournal,
    root: PathBuf,
}
impl ProposalJournal {
    pub fn open(workspace: &Value) -> Result<Self, String> {
        Ok(Self::from_inner(InboxJournal::open(workspace)?))
    }
    pub(super) fn from_inner(inner: InboxJournal) -> Self {
        let root = inner.root.join("proposal-disposition");
        Self { inner, root }
    }
    pub fn prepare(
        &self,
        detail: &Value,
        action: Disposition,
        actor: &str,
    ) -> Result<Pending, String> {
        let source = &detail["source"];
        let record = &detail["record"];
        if record["brain_id"] != self.inner.workspace["brain_id"]
            || source["brain_id"] != self.inner.workspace["brain_id"]
            || !revision(&source["revision"])
            || source["revision"] != detail["current_revision"]
            || detail["projection_pending"] != false
            || !matches!(
                record["attempt"]["state"].as_str(),
                Some("queued" | "running" | "interrupted" | "draft" | "failed" | "stale")
            )
            || !matches!(
                record["disposition"]["kind"].as_str(),
                Some("unreviewed" | "rejected" | "snoozed")
            )
            || !array(&detail["stale_reasons"]).is_empty()
        {
            return Err(
                "This proposal changed or is pending. Refresh and inspect it before deciding."
                    .into(),
            );
        }
        let seed = self.inner.request("Proposal disposition", actor)?;
        let pending = Pending {
            schema: "tessera-proposal-disposition-outbox/v1".into(),
            workspace: self.inner.workspace.clone(),
            path: text(&source["path"]),
            request: Request {
                operation_id: text(&seed["operation_id"]),
                proposal_id: text(&record["id"]),
                goal_id: record["goal_id"].as_str().map(str::to_owned),
                expected_revision: text(&source["revision"]),
                disposition: action,
                source: seed["source"].clone(),
            },
        };
        self.validate(&pending)?;
        Ok(pending)
    }
    fn validate(&self, p: &Pending) -> Result<(), String> {
        let r = &p.request;
        if p.schema != "tessera-proposal-disposition-outbox/v1"
            || p.workspace != self.inner.workspace
            || r.source["instance_id"] != self.inner.instance
            || Uuid::parse_str(&r.operation_id).is_err()
            || r.source["message_id"] != r.operation_id
            || r.source["update_id"] != r.operation_id
            || r.source["channel"] != "native"
            || r.source["actor_id"].as_str().is_none_or(str::is_empty)
            || !revision(&json!(r.expected_revision))
            || r.proposal_id.len() != 64
            || !r.proposal_id.bytes().all(|b| b.is_ascii_hexdigit())
            || r.goal_id
                .as_ref()
                .is_some_and(|v| Uuid::parse_str(v).is_err())
            || p.path
                != format!(
                    "{}/proposal-{}.md",
                    text(&self.inner.workspace["records_dir"]),
                    r.proposal_id
                )
        {
            return Err("Proposal delivery belongs to another client, workspace or source.".into());
        }
        if let Disposition::Snoozed { until } = &r.disposition {
            let at =
                time::OffsetDateTime::parse(until, &time::format_description::well_known::Rfc3339)
                    .map_err(|e| e.to_string())?;
            if at.offset() != time::UtcOffset::UTC {
                return Err("Snooze delivery must use an absolute UTC deadline.".into());
            }
        }
        Ok(())
    }
    fn path(&self, p: &Pending, ext: &str) -> Result<PathBuf, String> {
        self.validate(p)?;
        Ok(self
            .root
            .join(format!("{}.{}", p.request.operation_id, ext)))
    }
    pub fn retain(&self, p: &Pending) -> Result<(), String> {
        write_once(
            &self.path(p, "json")?,
            &serde_json::to_vec(p).map_err(|e| e.to_string())?,
        )
    }
    pub fn acknowledge(&self, p: &Pending, receipt: &Value) -> Result<(), String> {
        if !valid_receipt(p, receipt) {
            return Err("Proposal acknowledgement does not match the retained operation; recover the same delivery.".into());
        }
        let bytes = serde_json::to_vec(p).map_err(|e| e.to_string())?;
        if std::fs::read(self.path(p, "json")?).map_err(|e| e.to_string())? != bytes {
            return Err("Retained proposal request changed.".into());
        }
        // A committed replay after a local fsync failure retains the same receipt.
        let mut receipt = receipt.clone();
        receipt["replayed"] = json!(false);
        if let Some(object) = receipt.as_object_mut() {
            object.retain(|k, _| !k.starts_with('_'));
        }
        write_once(
            &self.path(p, "done")?,
            &serde_json::to_vec(&json!({"request":p,"receipt":receipt})).unwrap(),
        )
    }
    pub fn pending(&self) -> Result<Vec<Pending>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        let mut pending = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let p: Pending =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if self.path(&p, "json")? != path {
                return Err("Proposal filename differs from its operation.".into());
            }
            match std::fs::read(self.path(&p, "done")?) {
                Ok(bytes) => {
                    let done: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    if done["request"] != serde_json::to_value(&p).unwrap()
                        || !valid_receipt(&p, &done["receipt"])
                    {
                        return Err("Proposal receipt identity changed.".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => pending.push(p),
                Err(e) => return Err(e.to_string()),
            }
        }
        pending.sort_by(|a, b| a.request.operation_id.cmp(&b.request.operation_id));
        Ok(pending)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    pub fn workspace() -> Value {
        json!({"brain_id":"cc000000-0000-4000-8000-000000000183","root":"/synthetic/proposals","records_dir":"records","managed":true})
    }
    pub fn detail() -> Value {
        let id = "a".repeat(64);
        let revision = format!("sha256:{}", "b".repeat(64));
        json!({"record":{"schema":"tessera-proposal/v1","record_type":"proposal","id":id,"brain_id":workspace()["brain_id"],"goal_id":null,"disposition":{"kind":"unreviewed"},"attempt":{"state":"draft"}},"source":{"brain_id":workspace()["brain_id"],"path":format!("records/proposal-{id}.md"),"revision":revision},"current_revision":revision,"projection_pending":false,"stale_reasons":[]})
    }
    pub fn journal(dir: &std::path::Path) -> ProposalJournal {
        ProposalJournal::from_inner(InboxJournal::at(dir.to_path_buf(), &workspace()).unwrap())
    }
    pub fn receipt(p: &Pending) -> Value {
        json!({"operation_id":p.request.operation_id,"proposal_id":p.request.proposal_id,"path":p.path,"previous_revision":p.request.expected_revision,"revision":format!("sha256:{}","c".repeat(64)),"actor":p.request.source["actor_id"],"at":"2026-09-07T13:00:00Z","disposition":p.request.disposition,"replayed":false})
    }
    #[test]
    fn proposal_exact_outbox_reopen_and_receipt_binding() {
        let dir = std::env::temp_dir().join(format!("proposal-outbox-{}", uuid()));
        let j = journal(&dir);
        let p = j
            .prepare(&detail(), Disposition::Rejected, "operator")
            .unwrap();
        j.retain(&p).unwrap();
        assert_eq!(journal(&dir).pending().unwrap(), vec![p.clone()]);
        let good = receipt(&p);
        for field in [
            "operation_id",
            "proposal_id",
            "path",
            "previous_revision",
            "revision",
            "actor",
            "at",
            "disposition",
            "replayed",
        ] {
            let mut bad = good.clone();
            bad[field] = Value::Null;
            assert!(j.acknowledge(&p, &bad).is_err(), "{field}");
            assert_eq!(j.pending().unwrap(), vec![p.clone()]);
        }
        j.acknowledge(&p, &good).unwrap();
        let mut replay = good.clone();
        replay["replayed"] = json!(true);
        j.acknowledge(&p, &replay).unwrap();
        assert!(journal(&dir).pending().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn proposal_outbox_rejects_replacement_and_wrong_workspace() {
        let dir = std::env::temp_dir().join(format!("proposal-outbox-{}", uuid()));
        let j = journal(&dir);
        let p = j
            .prepare(
                &detail(),
                Disposition::Snoozed {
                    until: "2099-01-01T00:00:00Z".into(),
                },
                "operator",
            )
            .unwrap();
        j.retain(&p).unwrap();
        let mut replacement = p.clone();
        replacement.request.disposition = Disposition::Rejected;
        assert!(j.retain(&replacement).is_err());
        replacement = p.clone();
        replacement.workspace["root"] = json!("/other");
        assert!(j.retain(&replacement).is_err());
        assert_eq!(j.pending().unwrap(), vec![p]);
        for (field, value) in [
            ("projection_pending", json!(true)),
            ("stale_reasons", json!(["input changed"])),
            ("current_revision", json!("sha256:changed")),
        ] {
            let mut stale = detail();
            stale[field] = value;
            assert!(j
                .prepare(&stale, Disposition::Rejected, "operator")
                .is_err());
        }
        let mut future = detail();
        future["record"]["disposition"]["kind"] = json!("adopted");
        assert!(j
            .prepare(&future, Disposition::Rejected, "operator")
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod terminal_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn terminal_receipt_requires_exact_request_and_durable_acknowledgement() {
        let dir = std::env::temp_dir().join(format!("terminal-outbox-{}", uuid()));
        let j = tests::journal(&dir);
        let p = j
            .prepare(
                &tests::detail(),
                Disposition::Snoozed {
                    until: "2020-01-01T00:00:00Z".into(),
                },
                "operator",
            )
            .unwrap();
        j.retain(&p).unwrap();
        let good = json!({"schema":"tessera-proposal-terminal/v1","outcome":"not_applied","workspace":p.workspace,"request":p.request,"path":p.path,"reason":"deadline_elapsed","at":"2026-09-07T12:00:00Z","replayed":false});
        for field in [
            "schema",
            "outcome",
            "workspace",
            "request",
            "path",
            "reason",
            "at",
            "replayed",
        ] {
            let mut bad = good.clone();
            bad[field] = Value::Null;
            assert!(j.acknowledge(&p, &bad).is_err(), "{field}");
            assert_eq!(tests::journal(&dir).pending().unwrap(), vec![p.clone()]);
        }
        let mut wrong = good.clone();
        wrong["request"]["disposition"]["until"] = json!("2019-01-01T00:00:00Z");
        assert!(j.acknowledge(&p, &wrong).is_err());
        let done = j.path(&p, "done").unwrap();
        std::fs::create_dir(&done).unwrap();
        assert!(j.acknowledge(&p, &good).is_err());
        std::fs::remove_dir(done).unwrap();
        assert_eq!(j.pending().unwrap(), vec![p.clone()]);
        j.acknowledge(&p, &good).unwrap();
        assert!(tests::journal(&dir).pending().unwrap().is_empty());
        let mut replay = good.clone();
        replay["replayed"] = json!(true);
        j.acknowledge(&p, &replay).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
