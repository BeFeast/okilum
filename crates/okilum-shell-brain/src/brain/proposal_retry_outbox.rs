//! Retry delivery is distinct from a new model attempt. Retain before sending.
use super::native_outbox::{write_once, InboxJournal};
use super::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub operation_id: String,
    pub proposal_id: String,
    pub goal_id: Option<String>,
    pub expected_revision: String,
    pub source: Value,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    schema: String,
    pub workspace: Value,
    pub path: String,
    pub previous_attempt_id: String,
    pub request: Request,
}
impl Pending {
    pub fn wire(&self) -> Value {
        let mut value = serde_json::to_value(&self.request).unwrap();
        value["op"] = json!("proposal_retry");
        value["_proposal_pending"] = serde_json::to_value(self).unwrap();
        value
    }
}
fn revision(v: &Value) -> bool {
    v.as_str().is_some_and(|v| {
        v.len() == 71 && v.starts_with("sha256:") && v[7..].bytes().all(|b| b.is_ascii_hexdigit())
    })
}
fn canonical_uuid(v: &str) -> bool {
    Uuid::parse_str(v).is_ok_and(|id| id.to_string() == v)
}
pub(super) fn eligible(detail: &Value) -> bool {
    let record = &detail["record"];
    if record
        .get("attempt_history")
        .is_some_and(|value| value.as_array().is_none_or(|history| history.len() >= 32))
    {
        return false;
    }
    matches!(
        record["attempt"]["state"].as_str(),
        Some("failed" | "interrupted")
    ) && matches!(
        record["disposition"]["kind"].as_str(),
        Some("unreviewed" | "snoozed")
    ) && detail["projection_pending"] == false
        && detail["stale_reasons"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && detail["source"]["revision"] == detail["current_revision"]
        && record["attempt"]["input"]["generation"]["settings"].is_object()
        && record["attempt"]["input"]["generation"]["request_body"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
}
pub(super) fn valid_receipt(p: &Pending, receipt: &Value) -> bool {
    if receipt["schema"] != "okilum-proposal-retry/v1"
        || receipt["workspace"] != p.workspace
        || receipt["request"] != serde_json::to_value(&p.request).unwrap()
        || receipt["path"] != p.path
        || !receipt["replayed"].is_boolean()
        || !receipt["at"].as_str().is_some_and(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                .is_ok_and(|v| v.offset() == time::UtcOffset::UTC)
        })
    {
        return false;
    }
    let result = &receipt["result"];
    match result["outcome"].as_str() {
        Some("committed") => {
            result["previous_attempt_id"] == p.previous_attempt_id
                && result["attempt_id"].as_str().is_some_and(canonical_uuid)
                && result["attempt_id"] != result["previous_attempt_id"]
                && result["previous_revision"] == p.request.expected_revision
                && revision(&result["revision"])
                && result["revision"] != result["previous_revision"]
        }
        Some("not_applied") => matches!(
            result["reason"].as_str(),
            Some(
                "revision_changed"
                    | "disposition_changed"
                    | "attempt_ineligible"
                    | "input_changed"
                    | "capacity_exceeded"
            )
        ),
        _ => false,
    }
}
pub(super) struct RetryJournal {
    inner: InboxJournal,
    root: PathBuf,
}
impl RetryJournal {
    pub fn open(workspace: &Value) -> Result<Self, String> {
        Ok(Self::from_inner(InboxJournal::open(workspace)?))
    }
    fn from_inner(inner: InboxJournal) -> Self {
        let root = inner.root.join("proposal-retry");
        Self { inner, root }
    }
    pub fn prepare(&self, detail: &Value, actor: &str) -> Result<Pending, String> {
        if !eligible(detail)
            || detail["record"]["brain_id"] != self.inner.workspace["brain_id"]
            || detail["source"]["brain_id"] != self.inner.workspace["brain_id"]
        {
            return Err("Refresh and inspect a failed or interrupted suggestion with unchanged input before Retry.".into());
        }
        let seed = self.inner.request("Retry suggestion", actor)?;
        let p = Pending {
            schema: "okilum-proposal-retry-outbox/v1".into(),
            workspace: self.inner.workspace.clone(),
            path: text(&detail["source"]["path"]),
            previous_attempt_id: text(&detail["record"]["attempt"]["id"]),
            request: Request {
                operation_id: text(&seed["operation_id"]),
                proposal_id: text(&detail["record"]["id"]),
                goal_id: detail["record"]["goal_id"].as_str().map(str::to_owned),
                expected_revision: text(&detail["source"]["revision"]),
                source: seed["source"].clone(),
            },
        };
        self.validate(&p)?;
        Ok(p)
    }
    fn validate(&self, p: &Pending) -> Result<(), String> {
        let r = &p.request;
        if p.schema != "okilum-proposal-retry-outbox/v1"
            || p.workspace != self.inner.workspace
            || !canonical_uuid(&p.previous_attempt_id)
            || !canonical_uuid(&r.operation_id)
            || r.source["instance_id"] != self.inner.instance
            || r.source["message_id"] != r.operation_id
            || r.source["update_id"] != r.operation_id
            || r.source["channel"] != "native"
            || r.source["actor_id"].as_str().is_none_or(str::is_empty)
            || !revision(&json!(r.expected_revision))
            || r.proposal_id.len() != 64
            || !r.proposal_id.bytes().all(|b| b.is_ascii_hexdigit())
            || r.goal_id.as_ref().is_some_and(|s| !canonical_uuid(s))
            || p.path
                != format!(
                    "{}/proposal-{}.md",
                    text(&self.inner.workspace["records_dir"]),
                    r.proposal_id
                )
        {
            return Err(
                "Retry delivery belongs to another client, workspace, attempt or source.".into(),
            );
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
            return Err("Retry acknowledgement does not match the retained request. Recover the same delivery.".into());
        }
        if std::fs::read(self.path(p, "json")?).map_err(|e| e.to_string())?
            != serde_json::to_vec(p).unwrap()
        {
            return Err("Retained Retry request changed.".into());
        }
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
        let mut pending = vec![];
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let p: Pending =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if self.path(&p, "json")? != path {
                return Err("Retry filename differs from its operation.".into());
            }
            match std::fs::read(self.path(&p, "done")?) {
                Ok(bytes) => {
                    let done: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    if done["request"] != serde_json::to_value(&p).unwrap()
                        || !valid_receipt(&p, &done["receipt"])
                    {
                        return Err("Retry receipt identity changed.".into());
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
    pub fn detail() -> Value {
        let mut d = super::super::proposal_outbox::tests::detail();
        d["record"]["attempt"] = json!({"id":"bb000000-0000-4000-8000-000000000192","state":"failed","input":{"generation":{"settings":{"model":"fixture"},"request_body":"{}"}}});
        d
    }
    pub fn journal(dir: &std::path::Path) -> RetryJournal {
        RetryJournal::from_inner(
            InboxJournal::at(
                dir.to_path_buf(),
                &super::super::proposal_outbox::tests::workspace(),
            )
            .unwrap(),
        )
    }
    pub fn receipt(p: &Pending) -> Value {
        json!({"schema":"okilum-proposal-retry/v1","workspace":p.workspace,"request":p.request,"path":p.path,"at":"2026-09-07T14:00:00Z","replayed":false,"result":{"outcome":"committed","previous_attempt_id":p.previous_attempt_id,"attempt_id":"bb000000-0000-4000-8000-000000000193","previous_revision":p.request.expected_revision,"revision":format!("sha256:{}","c".repeat(64))}})
    }
    #[test]
    fn retry_outbox_binds_full_receipt_and_recovers_after_ack_write_failure() {
        let dir = std::env::temp_dir().join(format!("retry-outbox-{}", uuid()));
        let j = journal(&dir);
        let p = j.prepare(&detail(), "operator").unwrap();
        j.retain(&p).unwrap();
        assert_eq!(journal(&dir).pending().unwrap(), vec![p.clone()]);
        let good = receipt(&p);
        for field in [
            "schema",
            "workspace",
            "request",
            "path",
            "at",
            "replayed",
            "result",
        ] {
            let mut bad = good.clone();
            bad[field] = Value::Null;
            assert!(j.acknowledge(&p, &bad).is_err(), "{field}");
        }
        for field in [
            "previous_attempt_id",
            "attempt_id",
            "previous_revision",
            "revision",
        ] {
            let mut bad = good.clone();
            bad["result"][field] = Value::Null;
            assert!(j.acknowledge(&p, &bad).is_err(), "{field}");
        }
        let done = j.path(&p, "done").unwrap();
        std::fs::create_dir(&done).unwrap();
        assert!(j.acknowledge(&p, &good).is_err());
        std::fs::remove_dir(done).unwrap();
        assert_eq!(j.pending().unwrap(), vec![p.clone()]);
        j.acknowledge(&p, &good).unwrap();
        let mut replay = good;
        replay["replayed"] = json!(true);
        j.acknowledge(&p, &replay).unwrap();
        assert!(journal(&dir).pending().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn retry_refuses_changed_owner_stale_pending_and_unretained_input() {
        let dir = std::env::temp_dir().join(format!("retry-eligibility-{}", uuid()));
        let j = journal(&dir);
        let mut exhausted = detail();
        exhausted["record"]["attempt_history"] =
            json!(vec![json!({"attempt":{"state":"failed"}}); 32]);
        assert!(j.prepare(&exhausted, "operator").is_err());
        exhausted["record"]["attempt_history"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(j.prepare(&exhausted, "operator").is_ok());
        for (field, value) in [
            ("projection_pending", json!(true)),
            ("stale_reasons", json!(["changed"])),
            ("current_revision", json!("changed")),
        ] {
            let mut d = detail();
            d[field] = value;
            assert!(j.prepare(&d, "operator").is_err(), "{field}");
        }
        for state in ["queued", "running", "draft", "stale"] {
            let mut d = detail();
            d["record"]["attempt"]["state"] = json!(state);
            assert!(j.prepare(&d, "operator").is_err(), "{state}");
        }
        let mut d = detail();
        d["record"]["attempt"]["input"] = Value::Null;
        assert!(j.prepare(&d, "operator").is_err());
        d = detail();
        d["record"]["disposition"]["kind"] = json!("rejected");
        assert!(j.prepare(&d, "operator").is_err());
        d = detail();
        d["record"]["brain_id"] = json!("another");
        assert!(j.prepare(&d, "operator").is_err());
        d = detail();
        d["record"]["attempt"]["state"] = json!("interrupted");
        d["record"]["disposition"] = json!({"kind":"snoozed","until":"2099-01-01T00:00:00Z"});
        assert!(j.prepare(&d, "operator").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn retry_terminal_only_retires_exact_typed_refusal() {
        let dir = std::env::temp_dir().join(format!("retry-terminal-{}", uuid()));
        let j = journal(&dir);
        let p = j.prepare(&detail(), "operator").unwrap();
        j.retain(&p).unwrap();
        let mut terminal = receipt(&p);
        terminal["result"] = json!({"outcome":"not_applied","reason":"revision_changed"});
        for reason in ["attempt_ineligible", "capacity_exceeded"] {
            let mut bounded = terminal.clone();
            bounded["result"]["reason"] = json!(reason);
            assert!(valid_receipt(&p, &bounded));
            let alternate = journal(&dir.join(reason));
            let prior = alternate.prepare(&detail(), "operator").unwrap();
            alternate.retain(&prior).unwrap();
            bounded["workspace"] = prior.workspace.clone();
            bounded["request"] = serde_json::to_value(&prior.request).unwrap();
            alternate.acknowledge(&prior, &bounded).unwrap();
            assert!(journal(&dir.join(reason)).pending().unwrap().is_empty());
        }
        let mut bad = terminal.clone();
        bad["request"]["expected_revision"] = json!("changed");
        assert!(j.acknowledge(&p, &bad).is_err());
        bad = terminal.clone();
        bad["result"]["reason"] = json!("runtime_error");
        assert!(j.acknowledge(&p, &bad).is_err());
        assert_eq!(j.pending().unwrap(), vec![p.clone()]);
        j.acknowledge(&p, &terminal).unwrap();
        assert!(j.pending().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
