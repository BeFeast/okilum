//! One automatic child of an explicit fresh Save. No source RPC occurs here.
use super::*;
use crate::brain::merge_preview::{preview_merge, MergePreview};

fn canonical_id(value: &Value, key: &str) -> Result<String, String> {
    let text = field(value, key)?;
    let id = Uuid::parse_str(text).map_err(|_| "Invalid automatic Save identity.")?;
    if id.to_string() != text {
        return Err("Noncanonical automatic Save identity.".into());
    }
    Ok(text.into())
}

fn resolution_id(brain: &str, path: &str, original: &str) -> String {
    let mut hash = Sha256::new();
    for field in ["tessera-auto-disjoint/v1", brain, path, original] {
        hash.update(field.as_bytes());
        hash.update([0]);
    }
    let mut bytes: [u8; 16] = hash.finalize()[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

fn snapshot_bytes(value: &Value, workspace: &Value, path: &Value) -> Result<Vec<u8>, String> {
    if value["schema"] != "ai-brain/v1"
        || value["brain_id"] != workspace["brain_id"]
        || &value["path"] != path
        || value["media_type"] != "text/markdown"
    {
        return Err("Automatic merge source identity or media type does not match.".into());
    }
    let encoded = field(value, "content_base64")?;
    if encoded.len() > MAX_TEXT_BYTES.div_ceil(3) * 4 {
        return Err("Automatic merge source exceeds the recovery budget.".into());
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid automatic merge bytes.")?;
    if bytes.len() > MAX_TEXT_BYTES || field(value, "revision")? != revision(&bytes) {
        return Err("Automatic merge source revision does not match its bytes.".into());
    }
    Ok(bytes)
}

impl Draft {
    /// Route even a malformed v2/version-8 pending request to strict auto receipt
    /// validation, never to the predecessor's unsafe generic acknowledgement.
    pub fn is_automatic_request(&self, request: &Value) -> bool {
        self.automatic_format
            && request["request"]["operation_id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
                .is_some_and(|id| id.get_version_num() == 8)
    }
}

impl EditorRecovery {
    pub fn retain_auto_resolution(
        &self,
        expected: &Draft,
        original_request: &Value,
        conflict: &Value,
    ) -> Result<AutoResolution, String> {
        let _lock = self.lock()?;
        let mut d = self.compare(expected)?;
        self.validate_request(&d, original_request, true)?;
        if d.pending_save.as_ref() != Some(original_request) || d.conflict.is_some() {
            return Err("Automatic resolution requires the exact fresh pending Save.".into());
        }
        let original = &original_request["request"];
        let parent = canonical_id(original, "operation_id")?;
        if conflict["conflict"]["conflict_id"] != parent
            || conflict["conflict"]["path"] != d.base["path"]
            || conflict["conflict"]["expected_revision"] != original["expected_revision"]
            || conflict["conflict"]["reason"] != "stale_revision"
            || conflict["base"] != original_request["base"]
            || conflict["proposed"]["content_base64"] != original["content_base64"]
        {
            return Err("Automatic conflict does not match the exact submitted Save.".into());
        }
        let base = snapshot_bytes(&conflict["base"], &self.workspace, &d.base["path"])?;
        let proposed = snapshot_bytes(&conflict["proposed"], &self.workspace, &d.base["path"])?;
        let current = if conflict["current"].is_null() {
            None
        } else {
            Some(snapshot_bytes(
                &conflict["current"],
                &self.workspace,
                &d.base["path"],
            )?)
        };
        let merged = match preview_merge(Some(&base), current.as_deref(), &proposed) {
            MergePreview::Manual { reason } => return Ok(AutoResolution::Manual { reason }),
            MergePreview::Ready { text, .. } => text,
        };
        let child = json!({"op":"source_write","base":conflict["current"],"request":{
            "schema":"ai-brain/v1",
            "operation_id":resolution_id(field(&self.workspace,"brain_id")?,field(&d.base,"path")?,&parent),
            "brain_id":self.workspace["brain_id"],"path":d.base["path"],
            "expected_revision":conflict["current"]["revision"],
            "content_base64":STANDARD.encode(merged.as_bytes())}});
        d.automatic_format = true;
        d.text = merged;
        d.conflict = Some(conflict.clone());
        d.pending_save = Some(child.clone());
        Self::bump(&mut d)?;
        self.publish(&d)?;
        Ok(AutoResolution::Ready {
            draft: Box::new(d),
            request: child,
        })
    }

    pub fn acknowledge_auto_resolution(
        &self,
        id: &str,
        request: &Value,
        receipt: &Value,
    ) -> Result<Draft, String> {
        let _lock = self.lock()?;
        let mut d = self.read(id)?;
        self.validate_request(&d, request, false)?;
        let conflict = d
            .conflict
            .as_ref()
            .ok_or("Automatic Save lost its original conflict.")?;
        let parent = canonical_id(&conflict["conflict"], "conflict_id")?;
        let r = &request["request"];
        let bytes = STANDARD
            .decode(field(r, "content_base64")?)
            .map_err(|_| "Invalid Save bytes.")?;
        if !d.automatic_format
            || d.pending_save.as_ref() != Some(request)
            || r["operation_id"]
                != resolution_id(
                    field(&self.workspace, "brain_id")?,
                    field(&d.base, "path")?,
                    &parent,
                )
            || request["base"] != conflict["current"]
            || receipt["operation_id"] != r["operation_id"]
            || receipt["path"] != r["path"]
            || receipt["previous_revision"] != r["expected_revision"]
            || field(receipt, "revision")? != revision(&bytes)
            || !matches!(receipt["outcome"].as_str(), Some("written" | "unchanged"))
        {
            return Err("Automatic Save receipt does not match its retained child.".into());
        }
        let mut current = request["base"].clone();
        current["revision"] = receipt["revision"].clone();
        current["content_base64"] = r["content_base64"].clone();
        d.conflict.as_mut().unwrap()["current"] = current;
        d.pending_save = None;
        // Keep the true original base even when newer text arrived. The UI must
        // decide whether to adopt saved bytes or expose this retained conflict.
        Self::bump(&mut d)?;
        self.publish(&d)?;
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::merge_preview::ManualReason;

    struct Fixture {
        root: PathBuf,
        store: EditorRecovery,
        workspace: Value,
        original: Value,
        pending: Draft,
        conflict: Value,
    }
    impl Fixture {
        fn new(base: &str, current: &str, proposed: &str) -> Self {
            let root = std::env::temp_dir().join(format!("okilum-auto-save-{}", Uuid::new_v4()));
            let workspace = json!({"brain_id":Uuid::new_v4().to_string(),"root":"/synthetic/auto-save","records_dir":"records","managed":true});
            let snapshot = |text: &str| json!({"schema":"ai-brain/v1","brain_id":workspace["brain_id"],"path":"notes/exact.md","revision":revision(text.as_bytes()),"content_base64":STANDARD.encode(text),"media_type":"text/markdown"});
            let store = EditorRecovery::at(root.clone(), &workspace).unwrap();
            let original = json!({"op":"source_write","base":snapshot(base),"request":{"schema":"ai-brain/v1","operation_id":Uuid::new_v4().to_string(),"brain_id":workspace["brain_id"],"path":"notes/exact.md","expected_revision":snapshot(base)["revision"],"content_base64":STANDARD.encode(proposed)}});
            let draft = store.create(&snapshot(base), proposed).unwrap();
            let pending = store.retain_save(&draft, &original).unwrap();
            let conflict = json!({"base":snapshot(base),"current":snapshot(current),"proposed":snapshot(proposed),"conflict":{"conflict_id":original["request"]["operation_id"],"path":"notes/exact.md","expected_revision":snapshot(base)["revision"],"current_revision":snapshot(current)["revision"],"reason":"stale_revision"}});
            Self {
                root,
                store,
                workspace,
                original,
                pending,
                conflict,
            }
        }
        fn ready(&self) -> (Draft, Value) {
            match self
                .store
                .retain_auto_resolution(&self.pending, &self.original, &self.conflict)
                .unwrap()
            {
                AutoResolution::Ready { draft, request } => (*draft, request),
                other => panic!("Expected independent merge, got {other:?}"),
            }
        }
        fn receipt(request: &Value) -> Value {
            let r = &request["request"];
            json!({"operation_id":r["operation_id"],"path":r["path"],"previous_revision":r["expected_revision"],"revision":revision(&STANDARD.decode(r["content_base64"].as_str().unwrap()).unwrap()),"outcome":"written"})
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn automatic_intent_preserves_exact_bytes_and_binds_original_child() {
        let base = "\u{feff}---\r\ncolor: blue\r\n---\r\n# Note\n[[link]]\nEnd";
        let current = base.replace("blue", "green");
        let proposed = base.replace("End", "Конец 🧠");
        let f = Fixture::new(base, &current, &proposed);
        let (d, r) = f.ready();
        assert_eq!(d.text, current.replace("End", "Конец 🧠"));
        assert_eq!(d.base, f.pending.base);
        assert_eq!(d.conflict, Some(f.conflict.clone()));
        assert_eq!(r["base"], f.conflict["current"]);
        assert_eq!(
            r["request"]["expected_revision"],
            f.conflict["current"]["revision"]
        );
        assert!(d.is_automatic_request(&r));
        assert!(d.automatic_format);
        assert_eq!(f.store.read(&d.id).unwrap(), d);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(f.store.path(&d.id).unwrap()).unwrap())
                .unwrap()["schema"],
            AUTO_SCHEMA
        );
    }

    #[test]
    fn equivalent_edits_preserve_current_and_use_one_deterministic_identity() {
        let f = Fixture::new("A\nB\n", "X\nB\n", "X\nB\n");
        let (d, r) = f.ready();
        assert_eq!(d.text, "X\nB\n");
        let id = resolution_id(
            f.workspace["brain_id"].as_str().unwrap(),
            "notes/exact.md",
            f.original["request"]["operation_id"].as_str().unwrap(),
        );
        assert_eq!(r["request"]["operation_id"], id);
        assert_ne!(
            id,
            resolution_id(
                f.workspace["brain_id"].as_str().unwrap(),
                "notes/other.md",
                f.original["request"]["operation_id"].as_str().unwrap()
            )
        );
        assert!(f.store.retain_auto_resolution(&d, &r, &f.conflict).is_err());
        assert!(f
            .store
            .retain_auto_resolution(&f.pending, &f.original, &f.conflict)
            .is_err());
        assert_eq!(f.store.read(&d.id).unwrap(), d);
    }

    #[test]
    fn overlap_deleted_current_and_invalid_utf8_remain_original_pending() {
        for kind in ["overlap", "deleted", "not_utf8"] {
            let mut f = Fixture::new("A\nB\n", "X\nB\n", "Y\nB\n");
            if kind == "deleted" {
                f.conflict["current"] = Value::Null;
            }
            if kind == "not_utf8" {
                f.conflict["current"]["content_base64"] = json!(STANDARD.encode([255]));
                f.conflict["current"]["revision"] = json!(revision(&[255]));
            }
            let result = f
                .store
                .retain_auto_resolution(&f.pending, &f.original, &f.conflict)
                .unwrap();
            assert!(matches!(result, AutoResolution::Manual { .. }));
            assert_eq!(f.store.read(&f.pending.id).unwrap(), f.pending);
        }
    }

    #[test]
    fn malformed_source_or_operation_identity_never_publishes_child() {
        for change in [
            "brain", "path", "hash", "media", "origin", "reason", "proposal",
        ] {
            let mut f = Fixture::new("A\nB\n", "A\nY\n", "X\nB\n");
            match change {
                "brain" => f.conflict["current"]["brain_id"] = json!(Uuid::new_v4().to_string()),
                "path" => f.conflict["current"]["path"] = json!("notes/other.md"),
                "hash" => f.conflict["current"]["revision"] = json!(revision(b"wrong")),
                "media" => f.conflict["current"]["media_type"] = json!("image/png"),
                "origin" => {
                    f.conflict["conflict"]["conflict_id"] = json!(Uuid::new_v4().to_string())
                }
                "reason" => f.conflict["conflict"]["reason"] = json!("unmanaged_writers"),
                "proposal" => {
                    f.conflict["proposed"]["content_base64"] = json!(STANDARD.encode("Other\nB\n"))
                }
                _ => unreachable!(),
            }
            assert!(
                f.store
                    .retain_auto_resolution(&f.pending, &f.original, &f.conflict)
                    .is_err(),
                "{change}"
            );
            assert_eq!(f.store.read(&f.pending.id).unwrap(), f.pending);
        }
    }

    #[test]
    fn newer_generation_refuses_auto_replacement_and_remains_inspectable() {
        let f = Fixture::new("A\nB\n", "A\nY\n", "X\nB\n");
        let newer = f.store.update(&f.pending, "X+\nB\n").unwrap();
        assert!(f
            .store
            .retain_auto_resolution(&f.pending, &f.original, &f.conflict)
            .is_err());
        let d = f
            .store
            .record_conflict(&newer.id, &f.original, &f.conflict)
            .unwrap();
        assert_eq!(d.text, "X+\nB\n");
        assert_eq!(d.base, f.pending.base);
        assert_eq!(d.conflict, Some(f.conflict.clone()));
    }

    #[test]
    fn late_newer_text_keeps_true_base_after_auto_ack_and_generic_ack_is_refused() {
        let f = Fixture::new("A\nB\n", "A\nY\n", "X\nB\n");
        let (d, r) = f.ready();
        let newer = f.store.update(&d, "X+\nB\n").unwrap();
        let receipt = Fixture::receipt(&r);
        assert!(f.store.acknowledge(&d.id, &r, &receipt).is_err());
        assert_eq!(f.store.read(&d.id).unwrap(), newer);
        let acknowledged = f
            .store
            .acknowledge_auto_resolution(&d.id, &r, &receipt)
            .unwrap();
        assert_eq!(acknowledged.text, "X+\nB\n");
        assert_eq!(acknowledged.base, f.pending.base);
        assert!(acknowledged.pending_save.is_none());
        assert_eq!(
            snapshot_text(
                &acknowledged.conflict.as_ref().unwrap()["current"],
                &f.workspace
            )
            .unwrap(),
            "X\nY\n"
        );
        assert_eq!(
            acknowledged.conflict.as_ref().unwrap()["proposed"],
            f.conflict["proposed"]
        );
        assert!(acknowledged.automatic_format);
    }

    #[test]
    fn unchanged_auto_ack_retains_record_until_native_input_epoch_check() {
        let f = Fixture::new("A\nB\n", "A\nY\n", "X\nB\n");
        let (d, r) = f.ready();
        let acknowledged = f
            .store
            .acknowledge_auto_resolution(&d.id, &r, &Fixture::receipt(&r))
            .unwrap();
        assert_eq!(acknowledged.text, "X\nY\n");
        assert_eq!(acknowledged.base, f.pending.base);
        assert_eq!(f.store.read(&d.id).unwrap(), acknowledged);
        let newer = f.store.update(&acknowledged, "X+\nB\n").unwrap();
        assert!(f.store.discard(&acknowledged).is_err());
        assert_eq!(f.store.read(&d.id).unwrap(), newer);
    }

    #[test]
    fn restart_retries_exact_child_and_forged_receipt_cannot_change_base() {
        let f = Fixture::new("A\nB\n", "A\nY\n", "X\nB\n");
        let (d, r) = f.ready();
        let reopened = EditorRecovery::at(f.root.clone(), &f.workspace).unwrap();
        let loaded = reopened.list().unwrap().drafts.pop().unwrap();
        assert_eq!(loaded, d);
        assert_eq!(reopened.retain_save(&loaded, &r).unwrap(), loaded);
        let mut changed = r.clone();
        changed["request"]["operation_id"] = json!(Uuid::new_v4().to_string());
        assert!(reopened.retain_save(&loaded, &changed).is_err());
        let mut receipt = Fixture::receipt(&r);
        receipt["revision"] = json!(revision(b"other"));
        assert!(reopened
            .acknowledge_auto_resolution(&d.id, &r, &receipt)
            .is_err());
        assert_eq!(reopened.read(&d.id).unwrap(), loaded);
    }

    #[test]
    fn failed_auto_publication_retains_child_for_explicit_recheck_not_send() {
        let f = Fixture::new("A\nB\n", "A\nY\n", "X\nB\n");
        f.store
            .fail_after_publish
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(f
            .store
            .retain_auto_resolution(&f.pending, &f.original, &f.conflict)
            .is_err());
        let d = f.store.list().unwrap().drafts.pop().unwrap();
        assert!(d.automatic_format);
        let r = d.pending_save.as_ref().unwrap();
        assert!(d.is_automatic_request(r));
        assert_eq!(f.store.retain_save(&d, r).unwrap(), d);
    }

    #[test]
    fn budget_failure_does_not_change_the_original_record_format() {
        let base = "A".repeat(crate::brain::merge_preview::MAX_INPUT_BYTES + 1);
        let f = Fixture::new(&base, &base, &base);
        assert!(matches!(
            f.store
                .retain_auto_resolution(&f.pending, &f.original, &f.conflict)
                .unwrap(),
            AutoResolution::Manual {
                reason: ManualReason::InputLimit
            }
        ));
        assert_eq!(f.store.read(&f.pending.id).unwrap(), f.pending);
        assert!(!f.pending.automatic_format);
    }
}
