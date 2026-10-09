//! Exact guarded approval values. Provider decisions do not prove execution.
use crate::maestro::{Client, Settings};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub const NEW_DECISIONS_ENABLED: bool = true;
pub const MAX_DECISION_RESPONSE: usize = 256 * 1024;
pub const ENROLLMENT_FIELD: &str = "maestro_approval_decisions";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub version: String,
    pub project_id: String,
    pub project_name: String,
    pub project_repo: String,
    pub approval_id: String,
    pub created_at: String,
    pub decision_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub issue: u64,
    pub pr: u64,
    pub head_sha: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub expected: Expected,
    pub decision_id: String,
    pub action: String,
    pub target: Target,
    pub summary: String,
    pub risk: String,
    pub evidence: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approved,
    Rejected,
}
impl Decision {
    pub fn route(&self) -> &'static str {
        match self {
            Self::Approved => "approve",
            Self::Rejected => "reject",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub expected: Expected,
    pub decision: Decision,
    pub actor: String,
    pub reason: String,
    pub at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation_id: String,
    pub goal_id: String,
    pub expected_link_id: String,
    pub instance: Value,
    pub review: Review,
    pub decision: Decision,
    pub actor: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub goal_id: String,
    pub expected_link_id: String,
    pub approval_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct View {
    pub supported: bool,
    pub status: String,
    pub review: Option<Review>,
    pub decision_receipt: Option<Receipt>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub ok: bool,
    pub replayed: bool,
    pub receipt: Receipt,
    pub execution_status: String,
}
/// This attempt was refused. It cannot resolve an earlier uncertain attempt.
#[derive(Debug)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Refusal {}
/// Local validation failed before network I/O. Earlier attempts remain unknown.
#[derive(Debug)]
pub struct NotSent {
    pub code: String,
}
impl std::fmt::Display for NotSent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Guarded decision not sent: {}", self.code)
    }
}
impl std::error::Error for NotSent {}
/// A response cannot prove the original decision. Never archive on this error.
#[derive(Debug)]
pub struct Uncertain {
    pub code: String,
    pub http_status: Option<u16>,
}
impl std::fmt::Display for Uncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Guarded decision outcome unknown: {}", self.code)
    }
}
impl std::error::Error for Uncertain {}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub version: String,
    pub supported: bool,
    pub actions: Vec<String>,
}
impl Capability {
    pub fn allows_decision(&self) -> bool {
        self.version == "v1" && self.supported && self.actions == ["merge_pr"]
    }
}
fn bounded(s: &str, max: usize) -> bool {
    s.len() <= max && !s.contains('\0')
}
impl Expected {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == "v1" && uuid::Uuid::parse_str(&self.project_id).is_ok(),
            "Unsupported guarded approval identity"
        );
        ensure!(
            !self.project_name.is_empty()
                && bounded(&self.project_name, 256)
                && !self.project_repo.is_empty()
                && bounded(&self.project_repo, 512),
            "Invalid guarded project identity"
        );
        ensure!(
            !self.approval_id.is_empty()
                && ![".", ".."].contains(&self.approval_id.as_str())
                && bounded(&self.approval_id, 256),
            "Invalid exact approval ID"
        );
        let at = OffsetDateTime::parse(&self.created_at, &Rfc3339)?;
        ensure!(
            at.unix_timestamp() > 0 && bounded(&self.created_at, 64),
            "Invalid approval mint"
        );
        ensure!(
            self.decision_revision.starts_with("v1:")
                && self.decision_revision.len() > 3
                && bounded(&self.decision_revision, 256),
            "Unsupported decision revision"
        );
        Ok(())
    }
}
impl Review {
    pub fn validate(&self) -> Result<()> {
        self.expected.validate()?;
        ensure!(
            self.action == "merge_pr"
                && !self.decision_id.is_empty()
                && bounded(&self.decision_id, 256),
            "Unsupported approval action"
        );
        ensure!(
            self.target.issue > 0
                && self.target.pr > 0
                && self.target.head_sha.len() == 40
                && self.target.head_sha.bytes().all(|c| c.is_ascii_hexdigit()),
            "Unsupported approval target"
        );
        ensure!(
            bounded(&self.summary, 16384)
                && bounded(&self.risk, 256)
                && self.evidence.len() <= 128
                && self.evidence.iter().all(|e| bounded(e, 16384)),
            "Approval review exceeds limit"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= 128 * 1024,
            "Approval review exceeds limit"
        );
        Ok(())
    }
}
impl Receipt {
    pub fn validate(&self) -> Result<()> {
        self.expected.validate()?;
        ensure!(
            !self.actor.is_empty() && bounded(&self.actor, 1024) && bounded(&self.reason, 32768),
            "Invalid decision receipt attribution"
        );
        ensure!(
            bounded(&self.at, 64)
                && OffsetDateTime::parse(&self.at, &Rfc3339)?.unix_timestamp() > 0,
            "Invalid decision receipt time"
        );
        Ok(())
    }
    pub fn matches(&self, request: &Request) -> bool {
        self.validate().is_ok()
            && self.expected == request.review.expected
            && self.decision == request.decision
    }
}
impl Request {
    pub fn validate(&self) -> Result<()> {
        for id in [&self.operation_id, &self.goal_id, &self.expected_link_id] {
            uuid::Uuid::parse_str(id)?;
        }
        self.review.validate()?;
        ensure!(
            self.instance.as_object().is_some_and(|o| o.len() == 2),
            "Invalid provider identity"
        );
        Settings {
            base_url: self.instance["base_url"]
                .as_str()
                .context("Missing provider origin")?
                .into(),
            instance_id: self.instance["instance_id"]
                .as_str()
                .context("Missing provider instance")?
                .into(),
            token_env: None,
            ui_origin: None,
        }
        .validate()?;
        ensure!(
            !self.actor.is_empty() && bounded(&self.actor, 1024) && bounded(&self.reason, 32768),
            "Invalid decision attribution"
        );
        ensure!(
            serde_json::to_vec(&self.provider_body())?.len() <= 64 * 1024,
            "Guarded decision body exceeds limit"
        );
        Ok(())
    }
    pub fn provider_body(&self) -> Value {
        json!({"actor":self.actor,"reason":self.reason,"expected":self.review.expected})
    }
    pub fn matches_local_receipt(&self, value: &Value) -> bool {
        value["operation_id"] == self.operation_id
            && value["goal_id"] == self.goal_id
            && value["link_id"] == self.expected_link_id
            && value["instance"] == self.instance
            && serde_json::from_value::<Receipt>(value["decision_receipt"].clone())
                .is_ok_and(|r| r.matches(self))
    }
}
/// Parse fresh Fleet data directly; guarded fields are not enrolled/persisted by ordinary polling.
pub fn view(raw: &Value, expected_project: &Expected) -> Result<View> {
    ensure!(
        uuid::Uuid::parse_str(&expected_project.project_id).is_ok()
            && !expected_project.project_name.is_empty()
            && bounded(&expected_project.project_name, 256)
            && !expected_project.project_repo.is_empty()
            && bounded(&expected_project.project_repo, 512)
            && !expected_project.approval_id.is_empty()
            && bounded(&expected_project.approval_id, 256),
        "Invalid guarded review selection"
    );
    let projects = raw["projects"]
        .as_array()
        .context("Missing Maestro projects")?;
    ensure!(projects.len() <= 256, "Too many Maestro projects");
    let matches: Vec<_> = projects
        .iter()
        .filter(|p| {
            p["project_id"] == expected_project.project_id
                || p["name"] == expected_project.project_name
        })
        .collect();
    ensure!(
        matches.len() == 1,
        "Guarded project identity unavailable or ambiguous"
    );
    let project = matches[0];
    ensure!(
        project["project_id"] == expected_project.project_id
            && project["name"] == expected_project.project_name
            && project["repo"] == expected_project.project_repo,
        "Guarded project identity changed"
    );
    // Older providers may omit an empty approvals array; that is absence of
    // proof, never support for an unguarded decision.
    let rows = raw
        .get("approvals")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    ensure!(rows.len() <= 8192, "Too many Maestro approvals");
    let matches: Vec<_> = rows
        .iter()
        .filter(|a| {
            a["id"] == expected_project.approval_id
                && a["project_name"] == expected_project.project_name
        })
        .collect();
    ensure!(
        matches.len() == 1,
        "Exact approval unavailable or ambiguous"
    );
    let row = matches[0];
    ensure!(
        row["project_repo"] == expected_project.project_repo,
        "Guarded approval repository changed"
    );
    let identity_matches = |e: &Expected| {
        e.project_id == expected_project.project_id
            && e.project_name == expected_project.project_name
            && e.project_repo == expected_project.project_repo
            && e.approval_id == expected_project.approval_id
    };
    // The current review can have a newer mint/revision. The caller must compare
    // against its retained request, rather than silently replacing that request.
    let review = serde_json::from_value::<Review>(row["guarded_review"].clone())
        .ok()
        .filter(|r| {
            r.validate().is_ok() && identity_matches(&r.expected) && row["action"] == r.action
        });
    // Receipt recovery is independent of current review eligibility/capability.
    let receipt = serde_json::from_value::<Receipt>(row["decision_receipt"].clone())
        .ok()
        .filter(|r| r.validate().is_ok() && identity_matches(&r.expected));
    let supported = serde_json::from_value::<Capability>(project["guarded_approvals"].clone())
        .is_ok_and(|c| c.allows_decision());
    let status = row["status"].as_str().unwrap_or("unknown");
    ensure!(bounded(status, 128), "Invalid approval execution status");
    Ok(View {
        supported,
        status: status.into(),
        review,
        decision_receipt: receipt,
    })
}

impl Client {
    pub fn guarded_view(&self, expected: &Expected) -> Result<View> {
        view(&self.fleet_raw()?, expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::Duration,
    };

    fn request(base: &str) -> Request {
        Request {
            operation_id: "01000000-0000-4000-8000-000000000236".into(),
            goal_id: "01000000-0000-4000-8000-000000000237".into(),
            expected_link_id: "01000000-0000-4000-8000-000000000238".into(),
            instance: crate::maestro::tests::settings(base).identity(),
            review: Review {
                expected: Expected {
                    version: "v1".into(),
                    project_id: "01000000-0000-4000-8000-000000000152".into(),
                    project_name: "fixture".into(),
                    project_repo: "fixture/test".into(),
                    approval_id: "approval-123".into(),
                    created_at: "2026-09-08T06:00:00.123456789Z".into(),
                    decision_revision: "v1:opaque-review-revision".into(),
                },
                decision_id: "decision-123".into(),
                action: "merge_pr".into(),
                target: Target {
                    issue: 42,
                    pr: 34,
                    head_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                },
                summary: "Exact reviewed summary".into(),
                risk: "low".into(),
                evidence: vec![],
            },
            decision: Decision::Approved,
            actor: "retrying-client".into(),
            reason: "Local review".into(),
        }
    }
    fn receipt(r: &Request) -> Receipt {
        Receipt {
            expected: r.review.expected.clone(),
            decision: r.decision.clone(),
            actor: "original-authenticated-actor".into(),
            reason: "Original decision reason".into(),
            at: "2026-09-08T06:01:00Z".into(),
        }
    }
    fn success(r: &Request) -> Value {
        json!({"ok":true,"replayed":true,"receipt":receipt(r),"execution_status":"execution_skipped"})
    }
    fn fleet(r: &Request) -> Value {
        let mut raw = crate::maestro::tests::raw();
        raw["projects"][0]["guarded_approvals"] =
            json!({"version":"v1","supported":true,"actions":["merge_pr"]});
        raw["approvals"] = json!([{"id":r.review.expected.approval_id,"project_name":"fixture","project_repo":"fixture/test",
            "issue_number":42,"action":"merge_pr","status":"pending","summary":"Rewritten display summary",
            "guarded_review":r.review,"decision_receipt":receipt(r)}]);
        raw
    }

    #[test]
    fn exact_review_and_original_receipt_survive_disabled_capability() {
        let r = request("http://127.0.0.1:8786");
        let mut raw = fleet(&r);
        let projected = view(&raw, &r.review.expected).unwrap();
        assert!(projected.supported);
        assert_eq!(projected.review, Some(r.review.clone()));
        assert_ne!(
            projected.review.unwrap().summary,
            raw["approvals"][0]["summary"]
        );
        raw["projects"][0]["guarded_approvals"] = Value::Null;
        raw["approvals"][0]["guarded_review"] = Value::Null;
        raw["approvals"][0]["status"] = json!("execution_skipped");
        let projected = view(&raw, &r.review.expected).unwrap();
        assert!(!projected.supported);
        assert!(projected.review.is_none());
        assert!(projected.decision_receipt.unwrap().matches(&r));
        assert_eq!(projected.status, "execution_skipped");
    }

    #[test]
    fn projection_refuses_ambiguous_identity_and_unsupported_review_without_losing_receipt() {
        let r = request("http://127.0.0.1:8786");
        for pointer in [
            "/approvals/0/guarded_review/target/session",
            "/approvals/0/guarded_review/review_repair",
        ] {
            let mut raw = fleet(&r);
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            raw.pointer_mut(parent).unwrap()[key] = json!("extra");
            let v = view(&raw, &r.review.expected).unwrap();
            assert!(v.review.is_none());
            assert!(v.decision_receipt.unwrap().matches(&r));
        }
        for field in ["project_repo", "project_id", "approval_id"] {
            let mut raw = fleet(&r);
            raw["approvals"][0]["guarded_review"]["expected"][field] = json!("wrong");
            assert!(view(&raw, &r.review.expected).unwrap().review.is_none());
        }
        let mut duplicate = fleet(&r);
        let row = duplicate["approvals"][0].clone();
        duplicate["approvals"].as_array_mut().unwrap().push(row);
        assert!(view(&duplicate, &r.review.expected).is_err());
        let mut duplicate = fleet(&r);
        let mut row = duplicate["projects"][0].clone();
        row["repo"] = json!("other/repo");
        duplicate["projects"].as_array_mut().unwrap().push(row);
        assert!(view(&duplicate, &r.review.expected).is_err());
    }

    #[test]
    fn reminted_receipt_and_opposite_decision_cannot_match_retained_request() {
        let r = request("http://127.0.0.1:8786");
        let mut raw = fleet(&r);
        raw["approvals"][0]["decision_receipt"]["expected"]["created_at"] =
            json!("2026-09-09T00:00:00Z");
        let current = view(&raw, &r.review.expected)
            .unwrap()
            .decision_receipt
            .unwrap();
        assert!(!current.matches(&r));
        let mut opposite = receipt(&r);
        opposite.decision = Decision::Rejected;
        assert!(!opposite.matches(&r));
        let mut changed = receipt(&r);
        changed.expected.decision_revision = "v1:different".into();
        assert!(!changed.matches(&r));
        assert!(receipt(&r).matches(&r)); // Other actor/reason is valid semantic replay.
        let local = json!({"operation_id":r.operation_id,"goal_id":r.goal_id,"link_id":r.expected_link_id,"instance":r.instance,"decision_receipt":receipt(&r)});
        assert!(r.matches_local_receipt(&local));
        let mut changed = local;
        changed["goal_id"] = json!("other-goal");
        assert!(!r.matches_local_receipt(&changed));
    }

    #[test]
    fn invalid_capability_and_legacy_missing_fields_never_enable_control() {
        let r = request("http://127.0.0.1:8786");
        for cap in [
            Value::Null,
            json!({"version":"v2","supported":true,"actions":["merge_pr"]}),
            json!({"version":"v1","supported":true,"actions":["merge_pr","stop_worker"]}),
            json!({"version":"v1","supported":true,"actions":["merge_pr"],"extra":true}),
        ] {
            let mut raw = fleet(&r);
            raw["projects"][0]["guarded_approvals"] = cap;
            assert!(!view(&raw, &r.review.expected).unwrap().supported);
        }
        let mut raw = fleet(&r);
        raw["approvals"][0]
            .as_object_mut()
            .unwrap()
            .remove("guarded_review");
        raw["approvals"][0]
            .as_object_mut()
            .unwrap()
            .remove("decision_receipt");
        let v = view(&raw, &r.review.expected).unwrap();
        assert!(v.review.is_none() && v.decision_receipt.is_none());
    }

    // Capture every request, including an accidental automatic retry. A valid
    // first response is the positive control for the absence assertions.
    fn exchange<F>(
        status: u16,
        make_body: F,
        truncate: bool,
    ) -> (Result<Response>, String, Value, usize)
    where
        F: FnOnce(&Request) -> Vec<u8>,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut r = request(&base);
        r.review.expected.approval_id = "approval /?#%".into();
        r.review.expected.project_name = "fixture & review".into();
        let body = make_body(&r);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_server = stop.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
            }
            let mut request_body = vec![0; length];
            reader.read_exact(&mut request_body).unwrap();
            let content_length = body.len() + if truncate { 10 } else { 0 };
            let header = format!("HTTP/1.1 {status} Response\r\nContent-Length: {content_length}\r\nConnection: close\r\nLocation: /unguarded-fallback\r\n\r\n");
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            drop(reader);
            drop(stream);
            listener.set_nonblocking(true).unwrap();
            let mut count = 1;
            while !stop_server.load(Ordering::SeqCst) {
                if let Ok((mut stream, _)) = listener.accept() {
                    count += 1;
                    let _ = stream.write_all(
                        b"HTTP/1.1 500 Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                thread::sleep(Duration::from_millis(2));
            }
            while listener.accept().is_ok() {
                count += 1;
            }
            (first, serde_json::from_slice(&request_body).unwrap(), count)
        });
        let result = Client::new(crate::maestro::tests::settings(&base))
            .unwrap()
            .guarded_decide(&r);
        stop.store(true, Ordering::SeqCst);
        let (line, body, count) = server.join().unwrap();
        assert_eq!(body, r.provider_body());
        (result, line, body, count)
    }

    #[test]
    fn post_is_single_exact_encoded_request_and_preserves_original_replay_receipt() {
        let (result, line, body, count) =
            exchange(200, |r| serde_json::to_vec(&success(r)).unwrap(), false);
        assert_eq!(count, 1);
        assert_eq!(line,"POST /api/v1/fleet/approvals/approval%20%2F%3F%23%25/approve?project=fixture+%26+review HTTP/1.1\r\n");
        assert_eq!(body.as_object().unwrap().len(), 3);
        assert!(body.get("expected").is_some());
        let response = result.unwrap();
        assert!(response.replayed);
        assert_eq!(response.receipt.actor, "original-authenticated-actor");
        assert_eq!(response.receipt.reason, "Original decision reason");
        assert_eq!(response.execution_status, "execution_skipped");
    }

    #[test]
    fn uncertain_responses_never_become_refusal_or_retry() {
        for (status, body, truncate) in [
            (200, b"not json".to_vec(), false),
            (200, b"{}".to_vec(), false),
            (302, Vec::new(), false),
            (401, b"unauthorized".to_vec(), false),
            (403, Vec::new(), false),
            (
                500,
                serde_json::to_vec(
                    &json!({"ok":false,"error":{"code":"persistence_failed","message":"unknown"}}),
                )
                .unwrap(),
                false,
            ),
            (
                409,
                serde_json::to_vec(
                    &json!({"ok":false,"error":{"code":"unresolved","message":"missing"}}),
                )
                .unwrap(),
                false,
            ),
            (200, vec![b'x'; MAX_DECISION_RESPONSE + 1], false),
            (200, b"{}".to_vec(), true),
        ] {
            let (result, _, _, count) = exchange(status, |_| body, truncate);
            let error = result.unwrap_err();
            assert!(
                error.downcast_ref::<Uncertain>().is_some(),
                "{status}: {error}"
            );
            assert!(error.downcast_ref::<Refusal>().is_none());
            assert_eq!(count, 1);
        }
        let (result, _, _, count) = exchange(
            200,
            |r| {
                let mut b = success(r);
                b["receipt"]["expected"]["decision_revision"] = json!("v1:other");
                serde_json::to_vec(&b).unwrap()
            },
            false,
        );
        assert!(result.unwrap_err().downcast_ref::<Uncertain>().is_some());
        assert_eq!(count, 1);
    }

    #[test]
    fn structured_refusal_only_describes_current_attempt_and_status_code_pair() {
        for (status, code) in [
            (400, "invalid_expected"),
            (409, "revision_conflict"),
            (409, "identity_conflict"),
            (409, "decision_conflict"),
            (422, "unsupported"),
        ] {
            let (result, _, _, count) = exchange(
                status,
                |_| {
                    serde_json::to_vec(
                        &json!({"ok":false,"error":{"code":code,"message":"refused"}}),
                    )
                    .unwrap()
                },
                false,
            );
            assert_eq!(
                result.unwrap_err().downcast_ref::<Refusal>().unwrap().code,
                code
            );
            assert_eq!(count, 1);
        }
        let (result, _, _, _) = exchange(
            500,
            |_| {
                serde_json::to_vec(&json!({"ok":false,"error":{"code":"invalid_expected","message":"wrong status"}})).unwrap()
            },
            false,
        );
        assert!(result.unwrap_err().downcast_ref::<Uncertain>().is_some());
    }

    #[test]
    fn invalid_local_request_or_changed_instance_sends_nothing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let client = Client::new(crate::maestro::tests::settings(&base)).unwrap();
        let mut r = request(&base);
        r.instance["instance_id"] = json!("01000000-0000-4000-8000-000000000239");
        assert!(client
            .guarded_decide(&r)
            .unwrap_err()
            .downcast_ref::<NotSent>()
            .is_some());
        r = request(&base);
        r.reason = "x".repeat(65537);
        assert!(client
            .guarded_decide(&r)
            .unwrap_err()
            .downcast_ref::<NotSent>()
            .is_some());
        r = request(&base);
        r.review.target.head_sha = "short".into();
        assert!(client
            .guarded_decide(&r)
            .unwrap_err()
            .downcast_ref::<NotSent>()
            .is_some());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
