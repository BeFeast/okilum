//! Immutable Discussion delivery identity. Retained entries can only be looked up.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const SCHEMA: &str = "okilum-discussion-outbox/v1";
const MAX_ENTRY: u64 = 4 * 1024 * 1024;
const MAX_TERMINAL: u64 = MAX_ENTRY + 64 * 1024 + 1024;
fn nullable<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(d)
}
fn uuid_valid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok_and(|v| v.to_string() == s)
}
fn digest_valid(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn revision_valid(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(digest_valid)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub op: String,
    pub operation_id: String,
    pub goal_id: String,
    pub expected_actor_id: String,
    #[serde(deserialize_with = "nullable")]
    pub conversation_id: Option<String>,
    pub message: String,
    pub source_paths: Vec<String>,
    pub request_sha256: String,
}
impl Request {
    pub fn canonical_bytes(&self, brain: &str) -> Vec<u8> {
        serde_json::to_vec(&(
            "okilum-discussion-send-request/v1",
            brain,
            &self.goal_id,
            &self.expected_actor_id,
            &self.conversation_id,
            &self.message,
            &self.source_paths,
        ))
        .unwrap()
    }
    pub fn digest(&self, brain: &str) -> String {
        format!("{:x}", Sha256::digest(self.canonical_bytes(brain)))
    }
    pub fn lookup(&self) -> Value {
        json!({"op":"chat_send_get","operation_id":self.operation_id,"goal_id":self.goal_id,
            "expected_actor_id":self.expected_actor_id,"request_sha256":self.request_sha256})
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub schema: String,
    pub workspace: Value,
    pub endpoint: SocketAddr,
    pub draft: String,
    pub request: Request,
}
impl Pending {
    pub fn prepare(
        workspace: Value,
        endpoint: SocketAddr,
        goal: String,
        actor: String,
        conversation: Option<String>,
        draft: String,
        mut paths: Vec<String>,
    ) -> Result<Self, String> {
        paths.sort();
        paths.dedup();
        let mut request = Request {
            op: "chat_send".into(),
            operation_id: uuid(),
            goal_id: goal,
            expected_actor_id: actor,
            conversation_id: conversation,
            message: draft.trim().into(),
            source_paths: paths,
            request_sha256: String::new(),
        };
        request.request_sha256 = request.digest(&text(&workspace["brain_id"]));
        let pending = Self {
            schema: SCHEMA.into(),
            workspace,
            endpoint,
            draft,
            request,
        };
        pending.validate()?;
        Ok(pending)
    }
    fn validate(&self) -> Result<(), String> {
        let r = &self.request;
        if self.schema != SCHEMA
            || !uuid_valid(&text(&self.workspace["brain_id"]))
            || self.workspace["root"]
                .as_str()
                .is_none_or(|s| !Path::new(s).is_absolute())
            || self.workspace["records_dir"]
                .as_str()
                .is_none_or(str::is_empty)
            || self.workspace["managed"] != true
            || !self.endpoint.ip().is_loopback()
            || r.op != "chat_send"
            || !uuid_valid(&r.operation_id)
            || !uuid_valid(&r.goal_id)
            || r.expected_actor_id.is_empty()
            || r.expected_actor_id.len() > 4096
            || r.conversation_id.as_ref().is_some_and(|s| !uuid_valid(s))
            || r.message.is_empty()
            || r.message != self.draft.trim()
            || r.source_paths.len() > 32
            || r.source_paths.windows(2).any(|p| p[0] >= p[1])
            || r.request_sha256 != r.digest(&text(&self.workspace["brain_id"]))
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() as u64 > MAX_ENTRY
        {
            return Err(
                "The retained Discussion owner, draft or request identity is invalid.".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Record {
    #[serde(deserialize_with = "nullable")]
    original_conversation_id: Option<String>,
    conversation_id: String,
    turn_id: String,
    provider_request_sha256: String,
    context_receipt_sha256: String,
    source_path: String,
    projection_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WriteReceipt {
    operation_id: String,
    path: String,
    #[serde(deserialize_with = "nullable")]
    previous_revision: Option<String>,
    revision: String,
    outcome: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct OperationResult {
    schema: String,
    operation_id: String,
    brain_id: String,
    goal_id: String,
    actor_id: String,
    request_sha256: String,
    status: String,
    #[serde(deserialize_with = "nullable")]
    record: Option<Record>,
    #[serde(deserialize_with = "nullable")]
    source_receipt: Option<WriteReceipt>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rejection {
    code: String,
    message: String,
    operation_id: String,
    brain_id: String,
    goal_id: String,
    request_sha256: String,
    recorded: bool,
}
/// Transport envelopes have already been matched by rpc_guarded. Rejection is
/// terminal only for the direct first send, never a lookup or generic error.
pub(super) fn classify(p: &Pending, data: &Value, direct: bool) -> Result<&'static str, String> {
    let fail = || {
        "Discussion acknowledgement does not match the retained operation; recovery remains available.".to_string()
    };
    if serde_json::to_vec(data).map_err(|e| e.to_string())?.len() > 64 * 1024 {
        return Err(fail());
    }
    if let Some(error) = data.get("_discussion_send_error") {
        let r: Rejection = serde_json::from_value(error.clone()).map_err(|_| fail())?;
        if direct
            && r.code == "discussion_send_rejected"
            && !r.recorded
            && !r.message.is_empty()
            && r.operation_id == p.request.operation_id
            && r.brain_id == p.workspace["brain_id"]
            && r.goal_id == p.request.goal_id
            && r.request_sha256 == p.request.request_sha256
        {
            return Ok("rejected");
        }
        return Err(fail());
    }
    let r: OperationResult = serde_json::from_value(data.clone()).map_err(|_| fail())?;
    if r.schema != "okilum-discussion-send-result/v1"
        || r.operation_id != p.request.operation_id
        || r.brain_id != p.workspace["brain_id"]
        || r.goal_id != p.request.goal_id
        || r.actor_id != p.request.expected_actor_id
        || r.request_sha256 != p.request.request_sha256
    {
        return Err(fail());
    }
    if r.status == "unknown" {
        return if r.record.is_none() && r.source_receipt.is_none() {
            Ok("unknown")
        } else {
            Err(fail())
        };
    }
    let record = r.record.ok_or_else(fail)?;
    if record.original_conversation_id != p.request.conversation_id
        || !uuid_valid(&record.conversation_id)
        || !uuid_valid(&record.turn_id)
        || !digest_valid(&record.provider_request_sha256)
        || !digest_valid(&record.context_receipt_sha256)
        || !revision_valid(&record.projection_revision)
        || record.source_path
            != format!(
                "{}/conversation-{}.md",
                text(&p.workspace["records_dir"]),
                record.conversation_id
            )
        || p.request
            .conversation_id
            .as_ref()
            .is_some_and(|c| c != &record.conversation_id)
    {
        return Err(fail());
    }
    match r.status.as_str() {
        "projected" => {
            let receipt = r.source_receipt.ok_or_else(fail)?;
            if receipt.operation_id != p.request.operation_id
                || receipt.path != record.source_path
                || receipt.revision != record.projection_revision
                || receipt
                    .previous_revision
                    .as_ref()
                    .is_some_and(|s| !revision_valid(s))
                || (p.request.conversation_id.is_none() && receipt.previous_revision.is_some())
                || !matches!(receipt.outcome.as_str(), "written" | "unchanged")
            {
                return Err(fail());
            }
            Ok("projected")
        }
        "pending_projection" if r.source_receipt.is_none() => Ok("pending_projection"),
        "projection_conflict" if r.source_receipt.is_none() => Ok("projection_conflict"),
        _ => Err(fail()),
    }
}

#[derive(Clone)]
pub(super) struct Journal {
    root: PathBuf,
    #[cfg(test)]
    fail_retain_after_publish: bool,
    #[cfg(test)]
    fail_ack_after_publish: bool,
    #[cfg(test)]
    fail_terminal_confirmation: bool,
}
#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub pending: Pending,
    pub terminal: Option<Value>,
}
impl Journal {
    #[cfg(test)]
    pub(super) fn root_for_test(&self) -> &Path {
        &self.root
    }
    #[cfg(test)]
    pub(super) fn at(root: PathBuf) -> Self {
        Self {
            root,
            fail_retain_after_publish: false,
            #[cfg(test)]
            fail_ack_after_publish: false,
            #[cfg(test)]
            fail_terminal_confirmation: false,
        }
    }
    pub fn open() -> Self {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        Self {
            root: base.join("okilum/discussion-outbox"),
            #[cfg(test)]
            fail_retain_after_publish: false,
            #[cfg(test)]
            fail_ack_after_publish: false,
            #[cfg(test)]
            fail_terminal_confirmation: false,
        }
    }
    fn path(&self, p: &Pending, ext: &str) -> Result<PathBuf, String> {
        p.validate()?;
        Ok(self
            .root
            .join(format!("{}.{}", p.request.operation_id, ext)))
    }
    #[cfg(test)]
    pub(super) fn failing_after_publish(mut self) -> Self {
        self.fail_retain_after_publish = true;
        self
    }
    pub fn retain(&self, p: &Pending) -> Result<(), String> {
        native_outbox::write_once(
            &self.path(p, "json")?,
            &serde_json::to_vec(p).map_err(|e| e.to_string())?,
        )?;
        #[cfg(test)]
        if self.fail_retain_after_publish {
            return Err("Injected failure after Discussion outbox publication".into());
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn failing_after_terminal_publish(mut self) -> Self {
        self.fail_ack_after_publish = true;
        self
    }
    pub fn acknowledge(&self, p: &Pending, data: &Value, direct: bool) -> Result<(), String> {
        let status = classify(p, data, direct)?;
        if !matches!(status, "projected" | "rejected") {
            return Ok(());
        }
        if read(&self.path(p, "json")?)? != serde_json::to_vec(p).unwrap() {
            return Err("Retained Discussion request changed.".into());
        }
        native_outbox::write_once(
            &self.path(p, "done")?,
            &serde_json::to_vec(
                &json!({"pending":p,"direct":direct && status=="rejected","reply":data}),
            )
            .unwrap(),
        )?;
        #[cfg(test)]
        if self.fail_ack_after_publish {
            return Err("Injected failure after Discussion terminal publication".into());
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn failing_terminal_confirmation(mut self) -> Self {
        self.fail_terminal_confirmation = true;
        self
    }
    /// A new view has no memory of an earlier failed publication. Confirm the
    /// immutable terminal file and its directory before treating it as durable.
    fn confirm_terminal(&self, path: &Path) -> Result<(), String> {
        #[cfg(test)]
        if self.fail_terminal_confirmation {
            return Err("Injected terminal durability confirmation failure".into());
        }
        std::fs::File::open(path).and_then(|file| file.sync_all())
            .and_then(|_| std::fs::File::open(&self.root)?.sync_all())
            .map_err(|e| format!("Cannot confirm Discussion terminal durability: {e}. Original requests are retained."))
    }
    pub fn entries(&self) -> Result<Vec<Entry>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        let mut result = vec![];
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let p: Pending = serde_json::from_slice(&read(&path)?).map_err(|e| e.to_string())?;
            if self.path(&p, "json")? != path {
                return Err("Discussion filename differs from its operation.".into());
            }
            let terminal = match read(&self.path(&p, "done")?) {
                Ok(bytes) => {
                    let done: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    if done["pending"] != serde_json::to_value(&p).unwrap()
                        || !done["direct"].is_boolean()
                        || !matches!(
                            classify(&p, &done["reply"], done["direct"] == true)?,
                            "projected" | "rejected"
                        )
                    {
                        return Err(
                            "Discussion terminal receipt changed; retained requests are preserved."
                                .into(),
                        );
                    }
                    self.confirm_terminal(&self.path(&p, "done")?)?;
                    Some(done["reply"].clone())
                }
                Err(_)
                    if !self
                        .path(&p, "done")?
                        .try_exists()
                        .map_err(|e| e.to_string())? =>
                {
                    None
                }
                Err(e) => return Err(e),
            };
            result.push(Entry {
                pending: p,
                terminal,
            });
        }
        result.sort_by(|a, b| {
            a.pending
                .request
                .operation_id
                .cmp(&b.pending.request.operation_id)
        });
        Ok(result)
    }
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut bytes = vec![];
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_TERMINAL + 1).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_TERMINAL {
        return Err("Discussion recovery file exceeds its read budget.".into());
    }
    Ok(bytes)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    pub(in crate::brain) fn pending() -> Pending {
        Pending::prepare(json!({"brain_id":uuid(),"root":"/isolated/brain","records_dir":"records","managed":true}),
            "127.0.0.1:12345".parse().unwrap(),uuid(),"operator".into(),None," Question\r\n ".into(),vec!["z.md".into(),"a.md".into(),"z.md".into()]).unwrap()
    }
    pub(in crate::brain) fn projected(p: &Pending) -> Value {
        let conversation = p.request.conversation_id.clone().unwrap_or_else(uuid);
        let revision = format!("sha256:{}", "a".repeat(64));
        let path = format!("records/conversation-{conversation}.md");
        json!({"schema":"okilum-discussion-send-result/v1","operation_id":p.request.operation_id,"brain_id":p.workspace["brain_id"],
            "goal_id":p.request.goal_id,"actor_id":p.request.expected_actor_id,"request_sha256":p.request.request_sha256,"status":"projected",
            "record":{"original_conversation_id":p.request.conversation_id,"conversation_id":conversation,"turn_id":uuid(),"provider_request_sha256":"b".repeat(64),"context_receipt_sha256":"c".repeat(64),"source_path":path,"projection_revision":revision},
            "source_receipt":{"operation_id":p.request.operation_id,"path":path,"previous_revision":null,"revision":revision,"outcome":"written"}})
    }
    fn journal() -> Journal {
        Journal::at(std::env::temp_dir().join(format!("discussion-outbox-{}", uuid())))
    }
    #[test]
    fn discussion_digest_matches_shared_exact_utf8_fixtures() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../../../docs/fixtures/discussion-send-request-digests-v1.json"
        ))
        .unwrap();
        for f in array(&fixtures["cases"]) {
            let input = &f["input"];
            let r = Request {
                op: "chat_send".into(),
                operation_id: uuid(),
                goal_id: text(&input["goal_id"]),
                expected_actor_id: text(&input["expected_actor_id"]),
                conversation_id: input["conversation_id"].as_str().map(str::to_owned),
                message: text(&input["message"]),
                source_paths: array(&input["source_paths"]).iter().map(text).collect(),
                request_sha256: text(&f["request_sha256"]),
            };
            let bytes = r.canonical_bytes(&text(&input["brain_id"]));
            assert_eq!(bytes, text(&f["canonical_json"]).into_bytes());
            assert_eq!(bytes.len() as u64, f["utf8_bytes"].as_u64().unwrap());
            assert_eq!(r.digest(&text(&input["brain_id"])), r.request_sha256);
        }
    }
    #[test]
    fn discussion_frozen_draft_request_lookup_and_immutable_terminal_reopen() {
        let j = journal();
        let p = pending();
        j.retain(&p).unwrap();
        assert_eq!(p.draft, " Question\r\n ");
        assert_eq!(p.request.message, "Question");
        assert_eq!(p.request.source_paths, vec!["a.md", "z.md"]);
        let mut changed = p.clone();
        changed.request.message = "Other".into();
        changed.draft = "Other".into();
        changed.request.request_sha256 = changed
            .request
            .digest(&text(&changed.workspace["brain_id"]));
        assert!(j.retain(&changed).is_err());
        let restored = j.entries().unwrap().remove(0).pending;
        assert_eq!(restored, p);
        let lookup = restored.request.lookup();
        assert_eq!(lookup["op"], "chat_send_get");
        assert!(lookup.get("message").is_none());
        assert!(lookup.get("source_paths").is_none());
        assert_eq!(lookup["expected_actor_id"], "operator");
        let r = projected(&p);
        j.acknowledge(&p, &r, false).unwrap();
        j.acknowledge(&p, &r, true).unwrap();
        let done = j.entries().unwrap().remove(0);
        assert_eq!(done.pending, p);
        assert_eq!(done.terminal, Some(r));
        std::fs::remove_dir_all(j.root).unwrap();
    }
    #[test]
    fn discussion_response_requires_exact_owner_shapes_and_projection_receipt() {
        let p = pending();
        let good = projected(&p);
        assert_eq!(classify(&p, &good, true).unwrap(), "projected");
        for pointer in [
            "/schema",
            "/operation_id",
            "/brain_id",
            "/goal_id",
            "/actor_id",
            "/request_sha256",
            "/record/original_conversation_id",
            "/record/conversation_id",
            "/record/turn_id",
            "/record/provider_request_sha256",
            "/record/context_receipt_sha256",
            "/record/source_path",
            "/record/projection_revision",
            "/source_receipt/operation_id",
            "/source_receipt/path",
            "/source_receipt/previous_revision",
            "/source_receipt/revision",
            "/source_receipt/outcome",
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = json!("wrong");
            assert!(classify(&p, &bad, true).is_err(), "{pointer}");
        }
        for field in ["record", "source_receipt"] {
            let mut bad = good.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert!(classify(&p, &bad, true).is_err());
        }
        let mut bad = good.clone();
        bad["extra"] = json!(true);
        assert!(classify(&p, &bad, true).is_err());
        let mut unknown = good.clone();
        unknown["status"] = json!("unknown");
        unknown["record"] = Value::Null;
        unknown["source_receipt"] = Value::Null;
        assert_eq!(classify(&p, &unknown, false).unwrap(), "unknown");
        for status in ["pending_projection", "projection_conflict"] {
            let mut pending = good.clone();
            pending["status"] = json!(status);
            assert!(classify(&p, &pending, false).is_err());
            pending["source_receipt"] = Value::Null;
            assert_eq!(classify(&p, &pending, false).unwrap(), status);
        }
    }
    #[test]
    fn discussion_only_exact_direct_precheckpoint_rejection_can_retire() {
        let j = journal();
        let p = pending();
        j.retain(&p).unwrap();
        let r = json!({"_discussion_send_error":{"code":"discussion_send_rejected","message":"Input unavailable","operation_id":p.request.operation_id,"brain_id":p.workspace["brain_id"],"goal_id":p.request.goal_id,"request_sha256":p.request.request_sha256,"recorded":false}});
        assert!(j.acknowledge(&p, &r, false).is_err());
        assert!(j.entries().unwrap()[0].terminal.is_none());
        for (key, value) in [
            ("code", json!("discussion_send_recovery_error")),
            ("recorded", json!(true)),
            ("operation_id", json!(uuid())),
            ("request_sha256", json!("x")),
        ] {
            let mut bad = r.clone();
            bad["_discussion_send_error"][key] = value;
            assert!(j.acknowledge(&p, &bad, true).is_err());
        }
        j.acknowledge(&p, &r, true).unwrap();
        assert_eq!(j.entries().unwrap()[0].terminal, Some(r));
        std::fs::remove_dir_all(j.root).unwrap();
    }
    #[test]
    fn discussion_terminal_write_failure_retains_original_and_boundary_reopens() {
        let j = journal();
        let mut p = pending();
        // A near-limit pending record must still fit when wrapped in a terminal.
        p.draft = "x".repeat((MAX_ENTRY as usize - 4096) / 2);
        p.request.message = p.draft.clone();
        p.request.request_sha256 = p.request.digest(&text(&p.workspace["brain_id"]));
        j.retain(&p).unwrap();
        let r = projected(&p);
        let done = j.path(&p, "done").unwrap();
        std::fs::create_dir(&done).unwrap();
        assert!(j.acknowledge(&p, &r, true).is_err());
        assert_eq!(
            serde_json::from_slice::<Pending>(&read(&j.path(&p, "json").unwrap()).unwrap())
                .unwrap(),
            p
        );
        std::fs::remove_dir(&done).unwrap();
        j.acknowledge(&p, &r, true).unwrap();
        assert!(j.entries().unwrap()[0].terminal.is_some());
        let mut over = p.clone();
        over.draft.push_str(&"x".repeat(4096));
        over.request.message = over.draft.clone();
        over.request.request_sha256 = over.request.digest(&text(&over.workspace["brain_id"]));
        assert!(j.retain(&over).is_err());
        std::fs::remove_dir_all(j.root).unwrap();
    }
    #[test]
    fn discussion_lookup_rpc_preserves_key_and_rejects_forged_transport_rejection() {
        use std::net::TcpListener;
        for wrong_id in [false, true] {
            let p = pending();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = listener.local_addr().unwrap();
            let expected = p.clone();
            let thread = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let r: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(r["op"], "chat_send_get");
                assert_eq!(r["expected_workspace"], expected.workspace);
                assert_eq!(r["operation_id"], expected.request.operation_id);
                assert!(r.get("message").is_none());
                let reply = json!({"schema":r["schema"],"id":if wrong_id{json!(uuid())}else{r["id"].clone()},"ok":false,"error":{"code":"discussion_send_rejected","message":"pretend terminal","operation_id":expected.request.operation_id,"brain_id":expected.workspace["brain_id"],"goal_id":expected.request.goal_id,"request_sha256":expected.request.request_sha256,"recorded":false}});
                writeln!(reader.get_mut(), "{reply}").unwrap();
            });
            let response = rpc_guarded(endpoint, p.request.lookup(), Some(&p.workspace));
            thread.join().unwrap();
            if wrong_id {
                assert!(response.is_err());
            } else {
                assert!(classify(&p, &response.unwrap(), false).is_err());
            }
        }
    }
    #[test]
    fn discussion_actual_backend_wire_fixtures_validate_and_retire_exact_requests() {
        for (bytes, status) in [
            (
                include_str!("fixtures/discussion-send-projected.json"),
                "projected",
            ),
            (
                include_str!("fixtures/discussion-send-rejected.json"),
                "rejected",
            ),
        ] {
            // Exact synthetic service responses produced by the backend's real
            // correlated-send acceptance, including its captured workspace guard.
            let fixture: Value = serde_json::from_str(bytes).unwrap();
            let mut request = fixture["request"].clone();
            let workspace = request
                .as_object_mut()
                .unwrap()
                .remove("expected_workspace")
                .unwrap();
            request.as_object_mut().unwrap().remove("schema");
            request.as_object_mut().unwrap().remove("id");
            let request: Request = serde_json::from_value(request).unwrap();
            let p = Pending {
                schema: SCHEMA.into(),
                workspace,
                endpoint: "127.0.0.1:1".parse().unwrap(),
                draft: request.message.clone(),
                request,
            };
            p.validate().unwrap();
            let data = if status == "rejected" {
                json!({"_discussion_send_error":fixture["error"]})
            } else {
                fixture["data"].clone()
            };
            assert_eq!(classify(&p, &data, true).unwrap(), status);
            let j = journal();
            j.retain(&p).unwrap();
            j.acknowledge(&p, &data, true).unwrap();
            assert_eq!(j.entries().unwrap()[0].terminal, Some(data));
            std::fs::remove_dir_all(j.root).unwrap();
        }
    }
}
