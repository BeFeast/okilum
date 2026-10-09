//! Complete adoption form delivery is retained before sending and retired only by a bound receipt.
use super::native_outbox::{write_once, InboxJournal};
use super::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    schema: String,
    pub workspace: Value,
    pub path: String,
    pub detail: Value,
    pub goal_at_submit: String,
    pub request: Value,
}
impl Pending {
    pub fn wire(&self) -> Value {
        let mut value = self.request.clone();
        value["op"] = json!("proposal_adopt");
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
fn validate_form(r: &Value, detail: &Value) -> Result<(), String> {
    if r["destination"] == "inbox" {
        let Some(title) = r["title"].as_str() else {
            return Err("Goal title is required.".into());
        };
        let Some(criteria) = r["criteria"].as_array() else {
            return Err("Outcome criteria are required.".into());
        };
        let mut ids = BTreeSet::new();
        if title.trim().is_empty()
            || title.len() > 512
            || criteria.is_empty()
            || criteria.len() > 50
            || criteria.iter().any(|c| {
                c["description"]
                    .as_str()
                    .is_none_or(|s| s.trim().is_empty() || s.len() > 4096)
                    || !c["requires_human"].is_boolean()
                    || c["id"]
                        .as_str()
                        .is_none_or(|s| s.is_empty() || s.len() > 128 || !ids.insert(s))
            })
            || r["capture_id"] != detail["record"]["trigger"]["identity"]["record_id"]
            || r["expected_capture_revision"]
                != detail["record"]["trigger"]["identity"]["source_revision"]
        {
            return Err("Planning fields or original capture identity are invalid; your draft remains editable.".into());
        }
    } else if r["destination"] == "context" {
        if r["goal_id"] != detail["record"]["goal_id"]
            || r["scope"]["goal_id"] != r["goal_id"]
            || r["query"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty() || s.len() > 2048)
            || !revision(&r["expected_goal_revision"])
            || r["expected_base_packet"].is_null() != r["expected_base_revision"].is_null()
            || (!r["expected_base_packet"].is_null()
                && (!r["expected_base_packet"]
                    .as_str()
                    .is_some_and(canonical_uuid)
                    || !revision(&r["expected_base_revision"])))
        {
            return Err(
                "Context owner, query or base revision is invalid; your draft remains editable."
                    .into(),
            );
        }
        let Some(citations) = r["citations"].as_array() else {
            return Err("Context citations are required.".into());
        };
        let Some(pins) = r["pinned_citation_ids"].as_array() else {
            return Err("Context pin identities are required.".into());
        };
        let mut ids = BTreeSet::new();
        let mut pinned = BTreeSet::new();
        let size = citations
            .iter()
            .try_fold(text(&r["guidance"]).len(), |n, c| {
                c["excerpt"]
                    .as_str()
                    .filter(|s| s.len() <= 8192)
                    .and_then(|s| n.checked_add(s.len()))
            });
        if r["guidance"].as_str().is_none_or(|s| s.trim().is_empty())
            || citations.len() > 20
            || size.is_none_or(|n| n > 65536)
            || citations
                .iter()
                .any(|c| c["citation_id"].as_str().is_none_or(|s| !ids.insert(s)))
            || pins.iter().any(|v| {
                v.as_str()
                    .is_none_or(|s| !ids.contains(s) || !pinned.insert(s))
            })
        {
            return Err(
                "Guidance and citations exceed the context budget or a retained pin is missing."
                    .into(),
            );
        }
    } else {
        return Err("Unknown suggestion destination.".into());
    }
    Ok(())
}
pub(super) fn valid_receipt(p: &Pending, receipt: &Value) -> bool {
    if receipt["schema"] != "okilum-proposal-adopt/v1"
        || receipt["workspace"] != p.workspace
        || receipt["request"] != p.request
        || !receipt["replayed"].is_boolean()
        || !receipt["at"].as_str().is_some_and(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                .is_ok_and(|v| v.offset() == time::UtcOffset::UTC)
        })
    {
        return false;
    }
    let result = &receipt["result"];
    if result["outcome"] == "not_applied" {
        return matches!(
            result["reason"].as_str(),
            Some(
                "revision_changed"
                    | "disposition_changed"
                    | "attempt_ineligible"
                    | "input_changed"
                    | "destination_changed"
                    | "invalid_form"
                    | "capacity_exceeded"
            )
        );
    }
    let r = &result["receipt"];
    let target = &r["target"];
    if r["operation_id"] != p.request["operation_id"]
        || r["proposal_id"] != p.request["proposal_id"]
        || r["actor"] != p.request["source"]["actor_id"]
        || r["at"] != receipt["at"]
        || !r["replayed"].is_boolean()
        || !revision(&target["revision"])
        || !target["goal_id"].as_str().is_some_and(canonical_uuid)
    {
        return false;
    }
    let records = text(&p.workspace["records_dir"]);
    let (write, projection) = match result["outcome"].as_str() {
        Some("committed_context") if p.request["destination"] == "context" => {
            if target["goal_id"] != p.request["goal_id"]
                || r["goal_id"] != p.request["goal_id"]
                || !target["packet_id"].as_str().is_some_and(canonical_uuid)
                || target["path"]
                    != format!(
                        "{records}/reviewed-context-{}.md",
                        text(&target["packet_id"])
                    )
                || !matches!(
                    r["pointer"].as_str(),
                    Some("updated" | "already_selected" | "preserved_newer")
                )
            {
                return false;
            }
            (&r["target_receipt"], &r["projection_receipt"])
        }
        Some("committed_inbox") if p.request["destination"] == "inbox" => {
            if target["path"] != format!("{records}/goal-{}.md", text(&target["goal_id"]))
                || !target["child_operation_id"]
                    .as_str()
                    .is_some_and(canonical_uuid)
                || r["child"]["outcome"]["goal_id"] != target["goal_id"]
            {
                return false;
            }
            let outcome = &r["child"]["outcome"];
            let origin = &outcome["origin"];
            if outcome["receipt"]["operation_id"] != target["child_operation_id"]
                || outcome["receipt"]["status"] != "committed"
                || !outcome["receipt"]["replayed"].is_boolean()
                || origin["capture_id"] != p.request["capture_id"]
                || origin["revision"] != p.request["expected_capture_revision"]
                || origin["brain_id"] != p.workspace["brain_id"]
                || origin["operation_id"] != target["child_operation_id"]
                || origin["planned_by"] != p.request["source"]
                || !origin["planned_at"].as_str().is_some_and(|s| {
                    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                        .is_ok()
                })
                || origin["source_snapshot"] != p.detail["record"]["captured"]["trigger_source"]
                || origin["path"]
                    != format!("{records}/inbox-{}.md", text(&p.request["capture_id"]))
            {
                return false;
            }
            (&r["child"]["source"], &r["projection"])
        }
        _ => return false,
    };
    write["operation_id"].as_str().is_some_and(canonical_uuid)
        && projection["operation_id"]
            .as_str()
            .is_some_and(canonical_uuid)
        && write["operation_id"] != projection["operation_id"]
        && write["previous_revision"].is_null()
        && projection["previous_revision"] == p.request["expected_revision"]
        && write["outcome"] == "written"
        && projection["outcome"] == "written"
        && write["path"] == target["path"]
        && write["revision"] == target["revision"]
        && projection["path"] == p.path
        && revision(&projection["revision"])
}
pub(super) struct AdoptionJournal {
    inner: InboxJournal,
    root: PathBuf,
}
impl AdoptionJournal {
    pub fn open(workspace: &Value) -> Result<Self, String> {
        Ok(Self::from_inner(InboxJournal::open(workspace)?))
    }
    fn from_inner(inner: InboxJournal) -> Self {
        let root = inner.root.join("proposal-adoption");
        Self { inner, root }
    }
    pub fn prepare(
        &self,
        detail: &Value,
        mut fields: Value,
        actor: &str,
        goal_at_submit: String,
    ) -> Result<Pending, String> {
        let owner = &detail["record"]["goal_id"];
        if !super::proposal_form_state::eligible(detail, &self.inner.workspace, owner) {
            return Err(
                "Refresh and inspect an unchanged generated suggestion before using it.".into(),
            );
        }
        validate_form(&fields, detail)?;
        let seed = self.inner.request("Use suggestion", actor)?;
        fields["operation_id"] = seed["operation_id"].clone();
        fields["source"] = seed["source"].clone();
        fields["proposal_id"] = detail["record"]["id"].clone();
        fields["expected_revision"] = detail["source"]["revision"].clone();
        let p = Pending {
            schema: "okilum-proposal-adoption-outbox/v1".into(),
            workspace: self.inner.workspace.clone(),
            path: text(&detail["source"]["path"]),
            detail: detail.clone(),
            goal_at_submit,
            request: fields,
        };
        self.validate(&p)?;
        Ok(p)
    }
    fn validate(&self, p: &Pending) -> Result<(), String> {
        let r = &p.request;
        let owner = if r["destination"] == "context" {
            r["goal_id"].clone()
        } else {
            Value::Null
        };
        if p.schema != "okilum-proposal-adoption-outbox/v1"
            || p.workspace != self.inner.workspace
            || !r["operation_id"].as_str().is_some_and(canonical_uuid)
            || r["source"]["instance_id"] != self.inner.instance
            || r["source"]["message_id"] != r["operation_id"]
            || r["source"]["update_id"] != r["operation_id"]
            || r["source"]["channel"] != "native"
            || r["source"]["actor_id"].as_str().is_none_or(str::is_empty)
            || !super::proposal_ui::valid_detail(&p.detail, &p.workspace, &owner)
            || p.path != p.detail["source"]["path"]
            || r["expected_revision"] != p.detail["source"]["revision"]
            || r["proposal_id"] != p.detail["record"]["id"]
            || !matches!(r["destination"].as_str(), Some("inbox" | "context"))
            || serde_json::to_vec(r).map_err(|e| e.to_string())?.len() > 256 * 1024
        {
            return Err("Adoption delivery identity or bounds changed.".into());
        }
        Ok(())
    }
    fn path(&self, p: &Pending, ext: &str) -> Result<PathBuf, String> {
        self.validate(p)?;
        Ok(self
            .root
            .join(format!("{}.{}", text(&p.request["operation_id"]), ext)))
    }
    pub fn retain(&self, p: &Pending) -> Result<(), String> {
        write_once(
            &self.path(p, "json")?,
            &serde_json::to_vec(p).map_err(|e| e.to_string())?,
        )
    }
    pub fn acknowledge(&self, p: &Pending, receipt: &Value) -> Result<(), String> {
        if !valid_receipt(p, receipt) {
            return Err("Adoption acknowledgement does not match the retained request. Recover the same delivery.".into());
        }
        if std::fs::read(self.path(p, "json")?).map_err(|e| e.to_string())?
            != serde_json::to_vec(p).unwrap()
        {
            return Err("Retained Adoption request changed.".into());
        }
        let mut receipt = receipt.clone();
        receipt["replayed"] = json!(false);
        if receipt["result"]["receipt"].is_object() {
            receipt["result"]["receipt"]["replayed"] = json!(false);
        }
        if receipt["result"]["receipt"]["child"]["outcome"]["receipt"].is_object() {
            receipt["result"]["receipt"]["child"]["outcome"]["receipt"]["replayed"] = json!(false);
        }
        if let Some(object) = receipt.as_object_mut() {
            object.retain(|k, _| !k.starts_with('_'));
        }
        write_once(
            &self.path(p, "done")?,
            &serde_json::to_vec(&json!({"request":p,"receipt":receipt})).unwrap(),
        )
    }
    pub fn completed(&self) -> Result<Vec<(Pending, Value)>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        let mut records = vec![];
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("done") {
                continue;
            }
            let done: Value =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let p: Pending =
                serde_json::from_value(done["request"].clone()).map_err(|e| e.to_string())?;
            if self.path(&p, "done")? != path
                || !valid_receipt(&p, &done["receipt"])
                || std::fs::read(self.path(&p, "json")?).map_err(|e| e.to_string())?
                    != serde_json::to_vec(&p).unwrap()
            {
                return Err("Retired adoption identity changed.".into());
            }
            records.push((p, done["receipt"].clone()));
        }
        records.sort_by_key(|(_, r)| text(&r["at"]));
        Ok(records)
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
                return Err("Adoption filename differs from its operation.".into());
            }
            match std::fs::read(self.path(&p, "done")?) {
                Ok(bytes) => {
                    let done: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    if done["request"] != serde_json::to_value(&p).unwrap()
                        || !valid_receipt(&p, &done["receipt"])
                    {
                        return Err("Adoption receipt identity changed.".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => pending.push(p),
                Err(e) => return Err(e.to_string()),
            }
        }
        pending.sort_by(|a, b| {
            text(&a.request["operation_id"]).cmp(&text(&b.request["operation_id"]))
        });
        Ok(pending)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn controls(destination: &str) -> (Pending, Value) {
        // Actual service receipts from the context/Inbox coordinator acceptance
        // fixture; only the machine-specific workspace root was normalized.
        let (pending, receipt) = match destination {
            "context" => (
                include_str!("fixtures/adoption-context-pending.json"),
                include_str!("fixtures/adoption-context-receipt.json"),
            ),
            "inbox" => (
                include_str!("fixtures/adoption-inbox-pending.json"),
                include_str!("fixtures/adoption-inbox-receipt.json"),
            ),
            _ => unreachable!(),
        };
        (
            serde_json::from_str(pending).unwrap(),
            serde_json::from_str(receipt).unwrap(),
        )
    }
    #[test]
    fn adoption_actual_receipts_bind_original_target_and_reject_inconsistent_acknowledgements() {
        for destination in ["context", "inbox"] {
            let (p, r) = controls(destination);
            assert!(
                valid_receipt(&p, &r),
                "actual {destination} receipt rejected"
            );
            let mut pointers = vec![
                "/schema",
                "/workspace/brain_id",
                "/request/operation_id",
                "/request/expected_revision",
                "/result/receipt/operation_id",
                "/result/receipt/proposal_id",
                "/result/receipt/actor",
                "/result/receipt/at",
                "/result/receipt/target/goal_id",
                "/result/receipt/target/path",
                "/result/receipt/target/revision",
            ];
            if destination == "context" {
                pointers.extend([
                    "/result/receipt/goal_id",
                    "/result/receipt/pointer",
                    "/result/receipt/target/packet_id",
                    "/result/receipt/target_receipt/revision",
                    "/result/receipt/target_receipt/outcome",
                    "/result/receipt/projection_receipt/previous_revision",
                    "/result/receipt/projection_receipt/operation_id",
                ]);
            } else {
                pointers.extend([
                    "/result/receipt/target/child_operation_id",
                    "/result/receipt/child/outcome/receipt/operation_id",
                    "/result/receipt/child/outcome/receipt/status",
                    "/result/receipt/child/outcome/origin/capture_id",
                    "/result/receipt/child/outcome/origin/source_snapshot/content_base64",
                    "/result/receipt/child/outcome/origin/planned_by/actor_id",
                    "/result/receipt/child/source/revision",
                    "/result/receipt/projection/previous_revision",
                    "/result/receipt/projection/operation_id",
                ]);
            }
            for pointer in pointers {
                let mut bad = r.clone();
                *bad.pointer_mut(pointer).unwrap() = json!("wrong");
                assert!(!valid_receipt(&p, &bad), "accepted {destination} {pointer}");
            }
        }
    }
    #[test]
    fn adoption_journal_reopen_ack_replay_and_terminal_draft_preserve_exact_request() {
        for destination in ["context", "inbox"] {
            let (mut p, mut r) = controls(destination);
            let dir = std::env::temp_dir().join(format!("adoption-outbox-{}", uuid()));
            let j =
                AdoptionJournal::from_inner(InboxJournal::at(dir.clone(), &p.workspace).unwrap());
            p.request["source"]["instance_id"] = json!(j.inner.instance);
            r["request"] = p.request.clone();
            if destination == "inbox" {
                r["result"]["receipt"]["child"]["outcome"]["origin"]["planned_by"] =
                    p.request["source"].clone();
            }
            j.retain(&p).unwrap();
            assert_eq!(j.pending().unwrap(), vec![p.clone()]);
            let mut replacement = p.clone();
            replacement.request["expected_revision"] = json!("changed");
            assert!(j.retain(&replacement).is_err());
            let done = j.path(&p, "done").unwrap();
            std::fs::create_dir_all(&done).unwrap();
            assert!(j.acknowledge(&p, &r).is_err());
            assert!(j.pending().is_err());
            std::fs::remove_dir(&done).unwrap();
            r["replayed"] = json!(false);
            r["result"]["receipt"]["replayed"] = json!(false);
            j.acknowledge(&p, &r).unwrap();
            r["replayed"] = json!(true);
            r["result"]["receipt"]["replayed"] = json!(true);
            j.acknowledge(&p, &r).unwrap();
            assert!(j.pending().unwrap().is_empty());
            assert_eq!(j.completed().unwrap()[0].0, p);
            std::fs::remove_dir_all(dir).unwrap();
        }
        let (mut p, _) = controls("inbox");
        let dir = std::env::temp_dir().join(format!("adoption-terminal-{}", uuid()));
        let j = AdoptionJournal::from_inner(InboxJournal::at(dir.clone(), &p.workspace).unwrap());
        p.request["source"]["instance_id"] = json!(j.inner.instance);
        j.retain(&p).unwrap();
        let mut terminal = json!({"schema":"okilum-proposal-adopt/v1","workspace":p.workspace,"request":p.request,"at":"2026-09-07T17:00:00Z","replayed":false,"result":{"outcome":"not_applied","reason":"invalid_form"}});
        for reason in ["storage_error", "unknown", "provider_failed"] {
            terminal["result"]["reason"] = json!(reason);
            assert!(j.acknowledge(&p, &terminal).is_err());
            assert_eq!(j.pending().unwrap(), vec![p.clone()]);
        }
        terminal["result"]["reason"] = json!("capacity_exceeded");
        j.acknowledge(&p, &terminal).unwrap();
        assert!(j.pending().unwrap().is_empty());
        assert_eq!(j.completed().unwrap()[0].0.request, p.request);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
