//! Explicit user-turn promotion. SourceStore owns the only durable save ledger.
use super::*;
use okilum_core::source::{ErrorCode, RecoveryRecord, WriteOutcome};
use serde_json::json;

pub(crate) const KIND: &str = "discussion-decision";
pub(crate) const MAX_BYTES: u64 = 8192;
const MAX_RECOVERY: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Key {
    pub goal_id: String,
    pub conversation_id: String,
    pub turn_id: String,
    pub expected_actor_id: String,
}
impl Key {
    fn validate(&self) -> Result<()> {
        for id in [&self.goal_id, &self.conversation_id, &self.turn_id] {
            uuid(id)?;
        }
        ensure!(
            !self.expected_actor_id.trim().is_empty() && self.expected_actor_id.len() <= 4096,
            "Invalid original actor"
        );
        Ok(())
    }
    pub(super) fn id(&self, brain: &str, operation: bool) -> String {
        let namespace = if operation {
            "tessera/discussion-decision/operation/v1"
        } else {
            "tessera/discussion-decision/id/v1"
        };
        let mut hash = Sha256::new();
        for value in [
            namespace,
            brain,
            &self.goal_id,
            &self.conversation_id,
            &self.turn_id,
            &self.expected_actor_id,
        ] {
            hash.update((value.len() as u32).to_be_bytes());
            hash.update(value.as_bytes());
        }
        let digest = hash.finalize();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 15) | 128;
        bytes[8] = (bytes[8] & 63) | 128;
        Uuid::from_bytes(bytes).to_string()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Origin {
    pub schema: String,
    pub conversation_id: String,
    pub conversation_path: String,
    pub source_revision: String,
    pub turn_id: String,
    pub user_message_index: usize,
    pub user_message_sha256: String,
    pub transcript_prefix_sha256: String,
    pub context_receipt_sha256: String,
    pub created_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Decision {
    pub schema: String,
    pub record_type: String,
    pub brain_id: String,
    pub id: String,
    pub goal_id: String,
    pub actor_id: String,
    pub received_at: String,
    pub verification: String,
    pub discussion_origin: Origin,
}
impl Decision {
    pub(crate) fn key(&self) -> Key {
        Key {
            goal_id: self.goal_id.clone(),
            conversation_id: self.discussion_origin.conversation_id.clone(),
            turn_id: self.discussion_origin.turn_id.clone(),
            expected_actor_id: self.actor_id.clone(),
        }
    }
}
fn revision(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn hash(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Invalid provenance digest"
    );
    Ok(())
}
fn snapshot(request: &SourceWrite) -> Result<SourceSnapshot> {
    ensure!(
        request.content_base64.len() <= (MAX_BYTES as usize).div_ceil(3) * 4,
        "Decision exceeds 8 KiB"
    );
    let bytes = STANDARD.decode(&request.content_base64)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "Decision exceeds 8 KiB");
    Ok(SourceSnapshot {
        schema: request.schema.clone(),
        brain_id: request.brain_id.clone(),
        path: request.path.clone(),
        revision: revision(&bytes),
        content_base64: request.content_base64.clone(),
        media_type: "text/markdown".into(),
    })
}
impl Runner {
    fn decision_route(&self, key: &Key) -> Result<()> {
        key.validate()?;
        ensure!(
            self.managed() && self.state.goal_id.as_deref() == Some(&key.goal_id),
            "Decision requires the original managed goal"
        );
        Ok(())
    }
    pub(crate) fn parse_discussion_decision(
        &self,
        source: &SourceSnapshot,
    ) -> Result<(Decision, String)> {
        let bytes = STANDARD.decode(&source.content_base64)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES && source.revision == revision(&bytes),
            "Invalid decision source bytes"
        );
        let (meta, body) = parse_document(source)?;
        ensure!(
            !meta.contains_key(serde_yaml::Value::String("reuse_disposition".into())),
            "Automatic decision contradicts reuse disposition"
        );
        let d: Decision = serde_yaml::from_value(serde_yaml::Value::Mapping(meta))?;
        let key = d.key();
        key.validate()?;
        ensure!(
            d.schema == SCHEMA
                && d.record_type == KIND
                && d.brain_id == self.state.brain_id
                && source.brain_id == d.brain_id
                && source.schema == SCHEMA
                && d.id == key.id(&d.brain_id, false)
                && source.path == self.path(KIND, &d.id),
            "Decision ownership/path mismatch"
        );
        let o = &d.discussion_origin;
        ensure!(
            d.verification == "unverified"
                && !body.trim().is_empty()
                && o.schema == "tessera-discussion-decision-origin/v1"
                && o.conversation_path == self.path("conversation", &o.conversation_id),
            "Invalid user decision provenance"
        );
        hash(
            o.source_revision
                .strip_prefix("sha256:")
                .context("Invalid original revision")?,
        )?;
        for value in [
            &o.user_message_sha256,
            &o.transcript_prefix_sha256,
            &o.context_receipt_sha256,
        ] {
            hash(value)?;
        }
        ensure!(
            o.user_message_sha256 == format!("{:x}", Sha256::digest(body.as_bytes())),
            "Decision body changed from original user text"
        );
        valid_time(&d.received_at)?;
        valid_time(&o.created_at)?;
        Ok((d, body))
    }
    fn decision_request(&self, key: &Key, request: &SourceWrite) -> Result<(Decision, String)> {
        ensure!(
            request.expected_revision.is_none()
                && request.operation_id == key.id(&self.state.brain_id, true),
            "Decision create operation mismatch"
        );
        let (d, body) = self.parse_discussion_decision(&snapshot(request)?)?;
        ensure!(
            &d.key() == key,
            "Decision request belongs to another original turn"
        );
        Ok((d, body))
    }
    pub(super) fn decision_journal(&self, key: &Key, r: &RecoveryRecord) -> Result<()> {
        self.decision_request(key, &r.request)?;
        ensure!(
            r.base.is_none(),
            "Create-only decision has a fabricated target base"
        );
        ensure!(
            r.receipt.is_none() || r.conflict.is_none(),
            "Contradictory decision recovery"
        );
        if let Some(receipt) = &r.receipt {
            ensure!(
                r.preimage_base64.is_none()
                    && r.previous_revision.is_none()
                    && r.divergent_observations_base64.is_empty()
                    && receipt.operation_id == r.request.operation_id
                    && receipt.path == r.request.path
                    && receipt.previous_revision.is_none()
                    && receipt.revision == snapshot(&r.request)?.revision
                    && receipt.outcome == WriteOutcome::Written,
                "Invalid create receipt"
            );
        } else if let Some(conflict) = &r.conflict {
            let observed = r
                .preimage_base64
                .as_ref()
                .map(|s| STANDARD.decode(s).map(|b| revision(&b)))
                .transpose()?;
            ensure!(
                observed == r.previous_revision
                    && conflict.current_revision == observed
                    && conflict.expected_revision.is_none()
                    && conflict.conflict_id == r.request.operation_id
                    && conflict.path == r.request.path,
                "Invalid create conflict"
            );
        } else {
            ensure!(
                r.previous_revision.is_none()
                    && r.preimage_base64.is_none()
                    && r.divergent_observations_base64.is_empty(),
                "Divergent unresolved decision intent"
            );
        }
        Ok(())
    }
    fn decision_lookup(&self, key: &Key) -> Result<Value> {
        self.decision_route(key)?;
        let id = key.id(&self.state.brain_id, false);
        let operation = key.id(&self.state.brain_id, true);
        let path = self.path(KIND, &id);
        let mut view = json!({"identity":key,"decision_id":id,"operation_id":operation,"path":path,"state":"not_recorded","request":null,"receipt":null,"text":null,"origin":null,"target_status":"missing","current_revision":null,"error":null});
        let record = match self
            .source
            .recovery_record_bounded(&operation, MAX_RECOVERY)
        {
            Ok(r) => Some(r),
            Err(e) if e.code == ErrorCode::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(record) = &record {
            self.decision_journal(key, record)?;
        }
        let target = match self.source.read_bounded(&path, MAX_BYTES) {
            Ok(s) => Some(s),
            Err(e) if e.code == ErrorCode::NotFound => None,
            Err(_) => {
                view["target_status"] = json!("unavailable");
                None
            }
        };
        if let Some(target) = &target {
            view["current_revision"] = json!(target.revision);
            view["target_status"] = json!("changed");
        }
        if let Some(r) = record {
            let (d, body) = self.decision_request(key, &r.request)?;
            let exact = target
                .as_ref()
                .is_some_and(|s| s.content_base64 == r.request.content_base64);
            view["request"] = json!(r.request);
            view["text"] = json!(body);
            view["origin"] = json!(d.discussion_origin);
            view["state"] = json!(if r.receipt.is_some() {
                "already_saved"
            } else if r.conflict.is_some() {
                "conflict"
            } else {
                "pending"
            });
            if exact {
                view["target_status"] = json!(if r.receipt.is_some() {
                    "unchanged"
                } else {
                    "target_matches_proposed"
                });
            }
            view["receipt"] = json!(r.receipt);
            if r.conflict.is_some() {
                // Current collision text is independently bounded, never an acknowledgement.
                view["current_source"] = json!(target);
            }
            view["conflict"] = json!(r.conflict);
        } else if let Some(target) = &target {
            let (original, policy) =
                okilum_core::decision_reuse::original(target, &key.goal_id, &id)
                    .map_err(anyhow::Error::msg)?;
            let (d, body) = self.parse_discussion_decision(&original)?;
            view["reuse_state"] = json!(if policy.is_some() {
                "manual_only"
            } else {
                "automatic"
            });
            ensure!(
                d.key() == *key,
                "Existing decision target belongs to another turn"
            );
            view["state"] = json!("existing_canonical");
            view["target_status"] = json!("unchanged");
            view["text"] = json!(body);
            view["origin"] = json!(d.discussion_origin);
        } else {
            ensure!(
                view["target_status"] == "missing",
                "Decision target is unreadable or oversized"
            );
        }
        if let Some(target) = target.as_ref() {
            if let Ok((_, Some(_))) = self.decision_reuse_admission(target, &key.goal_id, &id) {
                view["reuse_state"] = json!("manual_only");
            }
        }
        Ok(view)
    }
    fn decision_failure(&self, key: &Key, error: anyhow::Error) -> Value {
        json!({"identity":key,"decision_id":key.id(&self.state.brain_id,false),"operation_id":key.id(&self.state.brain_id,true),"path":self.path(KIND,&key.id(&self.state.brain_id,false)),"state":"recovery_required","error":error.to_string(),"request":null,"receipt":null})
    }
    pub(crate) fn discussion_decision_get(&self, key: &Key, actor: &str) -> Result<Value> {
        self.decision_route(key)?;
        let mut view = match self.decision_lookup(key) {
            Ok(v) => v,
            Err(e) => return Ok(self.decision_failure(key, e)),
        };
        if view["state"] != "not_recorded" {
            return Ok(view);
        }
        ensure!(
            key.expected_actor_id == actor,
            "Only your own saved user turns can be retained"
        );
        let (origin, body) = crate::discussion_context::decision_origin(self, key)?;
        let d = Decision {
            schema: SCHEMA.into(),
            record_type: KIND.into(),
            brain_id: self.state.brain_id.clone(),
            id: key.id(&self.state.brain_id, false),
            goal_id: key.goal_id.clone(),
            actor_id: key.expected_actor_id.clone(),
            received_at: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)?,
            verification: "unverified".into(),
            discussion_origin: origin.clone(),
        };
        let bytes = format!("---\n{}---\n{}", serde_yaml::to_string(&d)?, body).into_bytes();
        ensure!(bytes.len() as u64<=MAX_BYTES,"The complete decision exceeds 8 KiB; this exact turn cannot be retained as brief input");
        let request = SourceWrite {
            schema: SCHEMA.into(),
            operation_id: key.id(&self.state.brain_id, true),
            brain_id: self.state.brain_id.clone(),
            path: self.path(KIND, &d.id),
            expected_revision: None,
            content_base64: STANDARD.encode(&bytes),
        };
        self.decision_request(key, &request)?;
        view["request"] = json!(request);
        view["text"] = json!(body);
        view["origin"] = json!(origin);
        Ok(view)
    }
    pub(crate) fn discussion_decision_save(
        &mut self,
        key: &Key,
        request: SourceWrite,
        actor: &str,
    ) -> Result<Value> {
        self.decision_route(key)?;
        let (d, body) = self.decision_request(key, &request)?;
        let previous = match self.decision_lookup(key) {
            Ok(v) => v,
            Err(e) => return Ok(self.decision_failure(key, e)),
        };
        if !previous["request"].is_null() {
            ensure!(
                previous["request"] == json!(request),
                "Operation already retains different exact decision bytes"
            );
        }
        if previous["state"] == "already_saved"
            || previous["state"] == "conflict"
            || previous["state"] == "existing_canonical"
        {
            return Ok(previous);
        }
        ensure!(
            key.expected_actor_id == actor,
            "Only your own saved user turns can be retained"
        );
        self.flush_writes()?;
        let (origin, text) = crate::discussion_context::decision_origin(self, key)?;
        ensure!(origin==d.discussion_origin && text==body,"Original conversation changed since review; preserve this proposal and reload its saved turn");
        if let Err(e) = self.source.write_with_base(request, None) {
            if e.code != ErrorCode::Conflict {
                return Ok(self.decision_failure(key, e.into()));
            }
        }
        self.decision_lookup(key)
    }
    pub(crate) fn admit_discussion_decision(&self, source: &SourceSnapshot) -> Result<Decision> {
        let (d, _) = self.parse_discussion_decision(source)?;
        let key = d.key();
        match self
            .source
            .recovery_record_bounded(&key.id(&d.brain_id, true), MAX_RECOVERY)
        {
            Ok(r) => {
                self.decision_journal(&key, &r)?;
                ensure!(
                    r.request.content_base64 == source.content_base64,
                    "Canonical decision differs from retained original save"
                );
            }
            Err(e) if e.code == ErrorCode::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(d)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::goal_brief::tests::fixture;
    use super::*;
    use crate::application::Application;
    use crate::chat::{ChatConfig, ChatEvent};

    fn app() -> Application {
        let mut a = Application::unconfigured();
        a.chat = Some(ChatConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "fixture".into(),
            api_key: "synthetic".into(),
            idle_timeout: std::time::Duration::from_secs(1),
        });
        a
    }
    pub(crate) fn turn(r: &mut Runner, g: &str, body: &str, id: Option<String>) -> Key {
        let a = app();
        let actor = a.local_actor().to_owned();
        // Preparation and synthetic completion deliberately do not invoke a provider.
        let prepared = r
            .with_goal(g, |r| {
                a.chat_start_prepared(r, g.into(), body.into(), vec![], id)
            })
            .unwrap();
        r.with_goal(g, |r| {
            Application::chat_event(
                r,
                &prepared.id,
                ChatEvent::Complete {
                    text: "Synthetic assistant answer".into(),
                },
            )
        })
        .unwrap();
        Key {
            goal_id: g.into(),
            conversation_id: prepared.id,
            turn_id: prepared.turn_id,
            expected_actor_id: actor,
        }
    }
    pub(crate) fn get(r: &mut Runner, key: &Key) -> Value {
        r.with_goal(&key.goal_id, |r| {
            r.discussion_decision_get(key, app().local_actor())
        })
        .unwrap()
    }
    pub(crate) fn save(r: &mut Runner, key: &Key, view: &Value) -> Result<Value> {
        r.with_goal(&key.goal_id, |r| {
            r.discussion_decision_save(
                key,
                serde_json::from_value(view["request"].clone())?,
                app().local_actor(),
            )
        })
    }
    fn inventory(dir: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                files.extend(inventory(&p));
            } else {
                files.insert(p.clone(), fs::read(p).unwrap());
            }
        }
        files
    }
    #[test]
    fn decision_ids_are_versioned_length_delimited_and_stable() {
        let key = Key {
            goal_id: "00000000-0000-4000-8000-000000000002".into(),
            conversation_id: "00000000-0000-4000-8000-000000000003".into(),
            turn_id: "00000000-0000-4000-8000-000000000004".into(),
            expected_actor_id: "local:oleg".into(),
        };
        assert_eq!(
            key.id("00000000-0000-4000-8000-000000000001", false),
            "6ff223da-25be-858c-ba2c-f3b3c6c2e4e3"
        );
        assert_eq!(
            key.id("00000000-0000-4000-8000-000000000001", true),
            "3ad4968d-2780-8afb-8ec9-e1da3aa16263"
        );
    }
    #[test]
    fn decision_exact_save_reopen_brief_isolation_manual_merge_and_original_suppression() {
        let (_temp, mut r, config, a, b) = fixture();
        let body = " \r\nUse **option B**; keep database. é 👨‍👩‍👧\r\n\t";
        let key = turn(&mut r, &a, body, None);
        let before = inventory(&config.root);
        let state_before = fs::read(config.operational_dir.join("state.json")).unwrap();
        let review = get(&mut r, &key);
        assert_eq!(review["state"], "not_recorded");
        assert_eq!(review["text"], body);
        assert_eq!(
            before,
            inventory(&config.root),
            "review/cancel is read-only"
        );
        let result = save(&mut r, &key, &review).unwrap();
        assert_eq!(result["state"], "already_saved");
        assert_eq!(result["target_status"], "unchanged");
        for (path, bytes) in before {
            assert_eq!(
                fs::read(path).unwrap(),
                bytes,
                "existing goal/conversation/stage source unchanged"
            );
        }
        assert_eq!(
            fs::read(config.operational_dir.join("state.json")).unwrap(),
            state_before,
            "no Attention/proposal/task/dispatch mutation"
        );
        let request: SourceWrite = serde_json::from_value(review["request"].clone()).unwrap();
        let source = r.source.read(&request.path).unwrap();
        assert_eq!(r.parse_discussion_decision(&source).unwrap().1, body);
        assert_eq!(save(&mut r, &key, &review).unwrap(), result, "exact replay");
        let brief = r.with_goal(&a, |r| r.goal_context_brief(&a)).unwrap();
        assert_eq!(brief["inputs"].as_array().unwrap().len(), 1);
        assert_eq!(brief["inputs"][0]["actor_id"], key.expected_actor_id);
        assert_eq!(brief["inputs"][0]["verification"], "unverified");
        assert!(
            r.with_goal(&b, |r| r.goal_context_brief(&b)).unwrap()["inputs"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let same = r
            .with_goal(&a, |r| {
                app().chat_start_prepared(
                    r,
                    a.clone(),
                    "Continue".into(),
                    vec![],
                    Some(key.conversation_id.clone()),
                )
            })
            .unwrap();
        assert!(
            !same.body.contains("Saved user decision"),
            "original transcript already contains this turn"
        );
        let fresh = r
            .with_goal(&a, |r| {
                app().chat_start_prepared(
                    r,
                    a.clone(),
                    "New discussion".into(),
                    vec![request.path.clone()],
                    None,
                )
            })
            .unwrap();
        let context = r
            .with_goal(&a, |r| {
                app().chat_get_context(r, &fresh.id, Some(&fresh.turn_id))
            })
            .unwrap();
        let inputs = context["context_snapshot"]["snapshot"]["inputs"]
            .as_array()
            .unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["kind"], KIND);
        assert_eq!(inputs[0]["reasons"], json!(["manual", "saved_decision"]));
        drop(r);
        let mut r = Runner::open(config).unwrap();
        assert_eq!(get(&mut r, &key)["state"], "already_saved");
    }
    #[test]
    fn decision_pending_lost_receipt_survives_append_without_inventing_ack_or_rewrite() {
        let (_temp, mut r, config, a, _) = fixture();
        let key = turn(&mut r, &a, "Keep B", None);
        let review = get(&mut r, &key);
        save(&mut r, &key, &review).unwrap();
        let request: SourceWrite = serde_json::from_value(review["request"].clone()).unwrap();
        let journal = config
            .operational_dir
            .join("source")
            .join(format!("{}.json", request.operation_id));
        let mut record: RecoveryRecord =
            serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
        record.receipt = None;
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        turn(&mut r, &a, "Later turn", Some(key.conversation_id.clone()));
        let before = inventory(&config.root);
        let pending = get(&mut r, &key);
        assert_eq!(pending["state"], "pending");
        assert_eq!(pending["target_status"], "target_matches_proposed");
        assert!(pending["receipt"].is_null());
        assert!(save(&mut r, &key, &review)
            .unwrap_err()
            .to_string()
            .contains("changed since review"));
        assert_eq!(before, inventory(&config.root));
        assert_eq!(
            r.with_goal(&a, |r| r.goal_context_brief(&a)).unwrap()["inputs"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "canonical existence remains useful despite pending receipt"
        );
        let mut altered = review.clone();
        altered["request"]["content_base64"] = json!(STANDARD.encode(b"different"));
        assert!(save(&mut r, &key, &altered).is_err());
    }
    #[test]
    fn decision_receipt_replay_after_deleted_origin_and_target_is_read_only() {
        let (_temp, mut r, config, a, _) = fixture();
        let key = turn(&mut r, &a, "Keep B", None);
        let review = get(&mut r, &key);
        save(&mut r, &key, &review).unwrap();
        fs::remove_file(config.root.join(text_field(&review["path"]))).unwrap();
        fs::remove_file(
            config
                .root
                .join(r.path("conversation", &key.conversation_id)),
        )
        .unwrap();
        let before = inventory(&config.root);
        let result = save(&mut r, &key, &review).unwrap();
        assert_eq!(result["state"], "already_saved");
        assert_eq!(result["target_status"], "missing");
        assert_eq!(before, inventory(&config.root));
    }
    fn text_field(v: &Value) -> &str {
        v.as_str().unwrap()
    }
    #[test]
    fn decision_invalid_legacy_actor_origin_and_size_refuse_without_promotion() {
        let (_temp, mut r, config, a, _) = fixture();
        let key = turn(&mut r, &a, "Keep B", None);
        let review = get(&mut r, &key);
        let mut other = key.clone();
        other.expected_actor_id = "another actor".into();
        assert!(r
            .with_goal(&a, |r| r.discussion_decision_get(&other, "another actor"))
            .is_err());
        let p = config
            .root
            .join(r.path("conversation", &key.conversation_id));
        let original = fs::read(&p).unwrap();
        let source = r
            .source
            .read_bounded(
                &r.path("conversation", &key.conversation_id),
                crate::discussion_context::MAX_CONVERSATION,
            )
            .unwrap();
        let (mut meta, body) = parse_document(&source).unwrap();
        meta.remove(serde_yaml::Value::String("discussion_context".into()));
        fs::write(
            &p,
            format!(
                "---\n{}---\n{}",
                serde_yaml::to_string(&meta).unwrap(),
                body
            ),
        )
        .unwrap();
        assert!(r
            .with_goal(&a, |r| r.discussion_decision_get(&key, app().local_actor()))
            .is_err());
        assert!(save(&mut r, &key, &review).is_err());
        fs::write(&p, original).unwrap();
        let huge = turn(&mut r, &a, &"x".repeat(8192), None);
        assert!(r
            .with_goal(&a, |r| r
                .discussion_decision_get(&huge, app().local_actor()))
            .unwrap_err()
            .to_string()
            .contains("8 KiB"));
        assert!(fs::read_dir(config.root.join("records"))
            .unwrap()
            .all(|p| !p
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("discussion-decision-")));
    }
    #[test]
    fn decision_conflicting_create_preimage_and_corrupt_journal_stay_visible() {
        let (_temp, mut r, config, a, _) = fixture();
        let key = turn(&mut r, &a, "Keep B", None);
        let review = get(&mut r, &key);
        let request: SourceWrite = serde_json::from_value(review["request"].clone()).unwrap();
        fs::write(
            config.root.join(&request.path),
            b"An unrelated existing target",
        )
        .unwrap();
        assert_eq!(get(&mut r, &key)["state"], "recovery_required");
        // Exercise the real SourceStore collision branch and its non-null preimage.
        assert_eq!(
            r.source.write(request.clone()).unwrap_err().code,
            ErrorCode::Conflict
        );
        assert_eq!(get(&mut r, &key)["state"], "conflict");
        let journal = config
            .operational_dir
            .join("source")
            .join(format!("{}.json", request.operation_id));
        fs::write(&journal, b"broken").unwrap();
        assert_eq!(get(&mut r, &key)["state"], "recovery_required");
        assert_eq!(
            fs::read(config.root.join(&request.path)).unwrap(),
            b"An unrelated existing target"
        );
    }
    #[test]
    fn decision_retained_intent_retries_once_and_portable_record_rebuilds_without_ledger() {
        let (_temp, mut r, config, a, _) = fixture();
        let key = turn(&mut r, &a, "  Keep B\r\n", None);
        let review = get(&mut r, &key);
        let request: SourceWrite = serde_json::from_value(review["request"].clone()).unwrap();
        let journal = config
            .operational_dir
            .join("source")
            .join(format!("{}.json", request.operation_id));
        let record = RecoveryRecord {
            request: request.clone(),
            base: None,
            preimage_base64: None,
            previous_revision: None,
            receipt: None,
            conflict: None,
            divergent_observations_base64: vec![],
        };
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        let pending = get(&mut r, &key);
        assert_eq!(pending["state"], "pending");
        assert_eq!(pending["target_status"], "missing");
        assert_eq!(pending["request"], review["request"]);
        assert_eq!(
            save(&mut r, &key, &review).unwrap()["state"],
            "already_saved"
        );
        let mut altered = review.clone();
        let (mut d, body) = r.decision_request(&key, &request).unwrap();
        d.received_at = "2026-09-07T00:00:00Z".into();
        altered["request"]["content_base64"] = json!(STANDARD.encode(format!(
            "---\n{}---\n{}",
            serde_yaml::to_string(&d).unwrap(),
            body
        )));
        assert!(save(&mut r, &key, &altered)
            .unwrap_err()
            .to_string()
            .contains("different exact"));
        fs::remove_file(&journal).unwrap();
        assert_eq!(get(&mut r, &key)["state"], "existing_canonical");
        assert_eq!(
            r.with_goal(&a, |r| r.goal_context_brief(&a)).unwrap()["inputs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let source = r.source.read(&request.path).unwrap();
        let (mut d, body) = r.parse_discussion_decision(&source).unwrap();
        d.goal_id = Uuid::new_v4().to_string();
        fs::write(
            config.root.join(&request.path),
            format!("---\n{}---\n{}", serde_yaml::to_string(&d).unwrap(), body),
        )
        .unwrap();
        assert_eq!(get(&mut r, &key)["state"], "recovery_required");
    }
    #[test]
    #[ignore = "Creates only the explicitly requested isolated compatibility fixture"]
    fn decision_fixture_for_old_writer_review() {
        let directory = PathBuf::from(
            std::env::var("OKILUM_DECISION253_FIXTURE")
                .expect("Explicit isolated fixture path required"),
        );
        assert!(!directory.exists(), "Never replace an earlier fixture");
        fs::create_dir_all(directory.join("brain/records")).unwrap();
        fs::create_dir(directory.join("state")).unwrap();
        let config = RunnerConfig {
            brain_id: Uuid::new_v4().to_string(),
            root: directory.join("brain"),
            operational_dir: directory.join("state"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let (mut r, a, b) = super::super::goal_brief::tests::fixture_goals(&config);
        let key = turn(
            &mut r,
            &a,
            "Use option B; keep the existing database.\r\n",
            None,
        );
        let review = get(&mut r, &key);
        let saved = save(&mut r, &key, &review).unwrap();
        let next = turn(
            &mut r,
            &a,
            "Use the retained decision in this new Discussion.",
            None,
        );
        let context = r
            .with_goal(&a, |r| {
                app().chat_get_context(r, &next.conversation_id, Some(&next.turn_id))
            })
            .unwrap();
        assert_eq!(
            context["context_snapshot"]["snapshot"]["inputs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let data = json!({"workspace":r.workspace_identity(),"goal_a":a,"goal_b":b,"origin_key":key,"review":review,"saved":saved,"new_conversation_key":next,"new_conversation_path":r.path("conversation",&next.conversation_id),"frozen_context":context,"provider_calls":0,"preparation":"Direct Application preparation and synthetic completion; no network provider/client invocation"});
        drop(r);
        fs::write(
            directory.join("fixture.json"),
            serde_json::to_vec_pretty(&data).unwrap(),
        )
        .unwrap();
    }
}
