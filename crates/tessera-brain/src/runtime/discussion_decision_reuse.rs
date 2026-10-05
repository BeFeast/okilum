//! Manual-only disposition: canonical bytes plus existing CAS/recovery receipts.
use super::*;
use serde_json::json;
use tessera_core::decision_reuse::{self as reuse, Disposition};
use tessera_core::source::{ErrorCode, RecoveryRecord, WriteOutcome, WriteReceipt};
const MAX_RECOVERY: u64 = 64 * 1024;

impl Runner {
    fn reuse_route(&self, goal: &str, id: &str) -> Result<String> {
        uuid(goal)?;
        uuid(id)?;
        ensure!(
            self.managed() && self.state.goal_id.as_deref() == Some(goal),
            "Reuse settings require the original managed goal"
        );
        Ok(self.path(super::discussion_decision::KIND, id))
    }
    /// The canonical predecessor is exact even when the portable copy has no journal.
    pub(super) fn decision_reuse_admission(
        &self,
        source: &SourceSnapshot,
        goal: &str,
        id: &str,
    ) -> Result<(super::discussion_decision::Decision, Option<Disposition>)> {
        ensure!(
            source.path == self.path(super::discussion_decision::KIND, id),
            "Decision path mismatch"
        );
        let (base, policy) = reuse::original(source, goal, id).map_err(anyhow::Error::msg)?;
        let d = self.admit_discussion_decision(&base)?;
        if let Some(p) = &policy {
            ensure!(
                p.actor_id == d.actor_id,
                "Disposition must belong to original actor"
            );
        }
        Ok((d, policy))
    }
    fn reuse_create_resolved(&self, decision: &super::discussion_decision::Decision) -> Result<()> {
        match self
            .source
            .recovery_record_bounded(&decision.key().id(&self.state.brain_id, true), MAX_RECOVERY)
        {
            Ok(record) => {
                self.decision_journal(&decision.key(), &record)?;
                ensure!(
                    record.receipt.is_some(),
                    "Recover the original decision Save before changing reuse"
                );
            }
            Err(e) if e.code == ErrorCode::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
    fn reuse_record(&self, goal: &str, id: &str, operation: &str) -> Result<RecoveryRecord> {
        let path = self.reuse_route(goal, id)?;
        let r = self
            .source
            .recovery_record_bounded(operation, MAX_RECOVERY)?;
        let base = r
            .base
            .as_ref()
            .context("Reuse operation has no real base")?;
        reuse::validate_write(base, &r.request, goal, id).map_err(anyhow::Error::msg)?;
        ensure!(
            r.request.path == path
                && r.request.brain_id == self.state.brain_id
                && base.brain_id == self.state.brain_id
                && r.request.operation_id == operation
                && !(r.receipt.is_some() && r.conflict.is_some()),
            "Reuse recovery identity/receipt mismatch"
        );
        if let Some(receipt) = &r.receipt {
            ensure!(
                r.preimage_base64.as_ref() == Some(&base.content_base64)
                    && r.previous_revision.as_ref() == Some(&base.revision)
                    && r.divergent_observations_base64.is_empty()
                    && receipt.operation_id == operation
                    && receipt.path == path
                    && receipt.previous_revision == r.request.expected_revision
                    && receipt.revision
                        == reuse::proposed(&r.request)
                            .map_err(anyhow::Error::msg)?
                            .revision
                    && receipt.outcome == WriteOutcome::Written,
                "Invalid reuse receipt/preimage"
            );
        }
        if r.receipt.is_none() && r.conflict.is_none() {
            ensure!(
                r.preimage_base64.as_ref() == Some(&base.content_base64)
                    && r.previous_revision.as_ref() == Some(&base.revision)
                    && r.divergent_observations_base64.is_empty(),
                "Invalid or divergent pending reuse operation"
            );
        }
        if let Some(c) = &r.conflict {
            let observed = r
                .preimage_base64
                .as_ref()
                .map(|s| STANDARD.decode(s).map(|b| reuse::revision(&b)))
                .transpose()?;
            ensure!(
                c.conflict_id == operation
                    && c.path == path
                    && c.expected_revision == r.request.expected_revision
                    && c.current_revision == r.previous_revision
                    && observed == r.previous_revision,
                "Invalid reuse conflict"
            );
        }
        Ok(r)
    }
    pub fn discussion_decision_reuse_get(
        &self,
        goal: &str,
        id: &str,
        operation: Option<&str>,
        actor: &str,
    ) -> Result<Value> {
        let path = self.reuse_route(goal, id)?;
        if let Some(operation) = operation {
            let r = self.reuse_record(goal, id, operation)?;
            ensure!(
                r.conflict.is_some(),
                "Operation has no terminal reuse conflict"
            );
            let (current, status) = match self.source.read_bounded(&path, reuse::MAX_BYTES as u64) {
                Ok(s) => (Some(s), "available"),
                Err(e) if e.code == ErrorCode::NotFound => (None, "missing"),
                Err(_) => (None, "unavailable_or_oversized"),
            };
            return Ok(
                json!({"conflict":r.conflict,"base":r.base,"proposed":reuse::proposed(&r.request).map_err(anyhow::Error::msg)?,"current":current,"current_status":status}),
            );
        }
        let source = match self.source.read_bounded(&path, reuse::MAX_BYTES as u64) {
            Ok(s) => s,
            Err(e) => {
                return Ok(
                    json!({"goal_id":goal,"decision_id":id,"path":path,"state":"unavailable","eligible":false,"reason":e.to_string(),"source":null}),
                )
            }
        };
        let result = (|| -> Result<Value> {
            let (d, policy) = self.decision_reuse_admission(&source, goal, id)?;
            let (base, _) = reuse::original(&source, goal, id).map_err(anyhow::Error::msg)?;
            let (_, body) = self.parse_discussion_decision(&base)?;
            let reason = if policy.is_some() {
                None
            } else if d.actor_id != actor {
                Some("Only the original local actor may change reuse".into())
            } else {
                self.reuse_create_resolved(&d).err().map(|e| e.to_string())
            };
            let raw = String::from_utf8(STANDARD.decode(&source.content_base64)?)?;
            let lines = raw.split_inclusive('\n').count();
            let citation = crate::retrieval::Citation {
                citation_id: crate::retrieval::citation_id(&path, &source.revision, 1, lines),
                path: path.clone(),
                revision: source.revision.clone(),
                start_line: 1,
                end_line: lines,
                locator: format!("L1-L{lines}"),
                excerpt: raw.clone(),
                metadata: crate::retrieval::source_metadata(&raw),
            };
            crate::retrieval::validate_citation(
                &source,
                &citation,
                goal,
                "goal",
                &self.state.records_dir,
            )?;
            Ok(
                json!({"goal_id":goal,"decision_id":id,"path":path,"state":if policy.is_some(){"manual_only"}else{"automatic"},"eligible":policy.is_none()&&reason.is_none(),"reason":reason,"source":source,"text":body,"actor_id":d.actor_id,"origin":d.discussion_origin,"disposition":policy,"citation":citation}),
            )
        })();
        Ok(result.unwrap_or_else(|e|json!({"goal_id":goal,"decision_id":id,"path":path,"state":"unavailable","eligible":false,"reason":e.to_string(),"source":source})))
    }
    pub fn discussion_decision_reuse_write(
        &mut self,
        goal: &str,
        id: &str,
        request: SourceWrite,
        base: SourceSnapshot,
        actor: &str,
    ) -> Result<WriteReceipt> {
        let path = self.reuse_route(goal, id)?;
        let policy =
            reuse::validate_write(&base, &request, goal, id).map_err(anyhow::Error::msg)?;
        ensure!(
            request.path == path
                && request.brain_id == self.state.brain_id
                && base.brain_id == self.state.brain_id,
            "Reuse Save belongs to another owner"
        );
        // A receipt is historical acknowledgement, before any projection flush/read.
        match self.reuse_record(goal, id, &request.operation_id) {
            Ok(r) => {
                ensure!(
                    r.request == request && r.base.as_ref() == Some(&base),
                    "Reuse attempt changed its exact request/base"
                );
                if let Some(receipt) = r.receipt {
                    return Ok(receipt);
                }
                if let Some(c) = r.conflict {
                    return Err(tessera_core::source::SourceError {
                        code: ErrorCode::Conflict,
                        message: "Reuse Save has a terminal source conflict".into(),
                        conflict: Some(Box::new(c)),
                    }
                    .into());
                }
            }
            Err(e)
                if e.downcast_ref::<tessera_core::source::SourceError>()
                    .is_some_and(|e| e.code == ErrorCode::NotFound) => {}
            Err(e) => return Err(e),
        }
        ensure!(
            policy.actor_id == actor,
            "Only the original local actor may change reuse"
        );
        self.flush_writes()?;
        let d = self.admit_discussion_decision(&base)?;
        ensure!(
            d.goal_id == goal && d.id == id && d.actor_id == actor,
            "Original decision identity mismatch"
        );
        self.reuse_create_resolved(&d)?;
        // SourceStore CAS retains the observed preimage. Bound it before that
        // read under the managed owner; arbitrary external writers remain outside
        // the existing managed-write exclusion guarantee.
        match self.source.read_bounded(&path, reuse::MAX_BYTES as u64) {
            Ok(_) => {}
            Err(e) if e.code == ErrorCode::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(self.source.write_with_base(request, Some(base))?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::discussion_decision::tests::{get, save, turn};
    use super::super::goal_brief::tests::fixture;
    use super::*;
    fn request(base: &SourceSnapshot, g: &str, id: &str, actor: &str) -> SourceWrite {
        let p = Disposition::new(
            Uuid::new_v4().to_string(),
            actor.into(),
            "2026-09-08T15:00:00Z".into(),
            base.revision.clone(),
        );
        let bytes = reuse::transform(base, g, id, &p).unwrap();
        SourceWrite {
            schema: SCHEMA.into(),
            brain_id: base.brain_id.clone(),
            path: base.path.clone(),
            operation_id: p.operation_id,
            expected_revision: Some(base.revision.clone()),
            content_base64: STANDARD.encode(bytes),
        }
    }
    #[test]
    fn reuse_keeps_exact_history_and_metadata_only_brief_and_historical_receipt_before_flush() {
        let (_dir, mut r, _, g, b) = fixture();
        let before_b = r.with_goal(&b, |r| r.goal_context_brief(&b)).unwrap();
        let key = turn(
            &mut r,
            &g,
            "Use option B; keep the existing database.",
            None,
        );
        let view = get(&mut r, &key);
        save(&mut r, &key, &view).unwrap();
        let id = view["decision_id"].as_str().unwrap();
        let base: SourceSnapshot = r.read_source(view["path"].as_str().unwrap()).unwrap();
        let req = request(&base, &g, id, &key.expected_actor_id);
        let before = r.with_goal(&g, |r| r.goal_context_brief(&g)).unwrap();
        assert!(before.get("manual_only").is_none());
        assert!(before["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == id));
        let receipt = r
            .with_goal(&g, |r| {
                r.discussion_decision_reuse_write(
                    &g,
                    id,
                    req.clone(),
                    base.clone(),
                    &key.expected_actor_id,
                )
            })
            .unwrap();
        let after = r.with_goal(&g, |r| r.goal_context_brief(&g)).unwrap();
        assert!(!after.to_string().contains("existing database"));
        assert_eq!(after["manual_only"][0]["id"], id);
        assert!(after["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["id"] != id));
        assert_eq!(
            before_b,
            r.with_goal(&b, |r| r.goal_context_brief(&b)).unwrap()
        );
        let inspected = r
            .with_goal(&g, |r| {
                r.discussion_decision_reuse_get(&g, id, None, &key.expected_actor_id)
            })
            .unwrap();
        assert_eq!(inspected["state"], "manual_only");
        assert_eq!(
            inspected["text"],
            "Use option B; keep the existing database."
        );
        let current: SourceSnapshot = serde_json::from_value(inspected["source"].clone()).unwrap();
        assert_eq!(reuse::original(&current, &g, id).unwrap().0, base);
        assert_eq!(get(&mut r, &key)["reuse_state"], "manual_only");
        // A pending projection would fail if flushed. Returning the receipt must not touch it.
        r.state.pending_writes.push(SourceWrite {
            schema: SCHEMA.into(),
            operation_id: Uuid::new_v4().to_string(),
            brain_id: base.brain_id.clone(),
            path: "../invalid.md".into(),
            expected_revision: None,
            content_base64: STANDARD.encode("untouched"),
        });
        let pending = r.state.pending_writes.clone();
        std::fs::remove_file(r.root.join(&base.path)).unwrap();
        assert_eq!(
            receipt,
            r.with_goal(&g, |r| r.discussion_decision_reuse_write(
                &g,
                id,
                req.clone(),
                base.clone(),
                "different-current-actor"
            ))
            .unwrap()
        );
        assert_eq!(pending, r.state.pending_writes);
        assert!(!r.root.join(&base.path).exists());
    }
    #[test]
    fn terminal_conflict_then_fresh_uuid_accepts_restored_base_without_reusing_old_attempt() {
        let (_dir, mut r, _, g, _) = fixture();
        let key = turn(&mut r, &g, "Keep exact input", None);
        let view = get(&mut r, &key);
        save(&mut r, &key, &view).unwrap();
        let id = view["decision_id"].as_str().unwrap();
        let path = view["path"].as_str().unwrap();
        let base = r.read_source(path).unwrap();
        let req = request(&base, &g, id, &key.expected_actor_id);
        std::fs::write(r.root.join(path), "External conflict").unwrap();
        assert!(r
            .with_goal(&g, |r| r.discussion_decision_reuse_write(
                &g,
                id,
                req.clone(),
                base.clone(),
                &key.expected_actor_id
            ))
            .is_err());
        let conflict = r
            .with_goal(&g, |r| {
                r.discussion_decision_reuse_get(
                    &g,
                    id,
                    Some(&req.operation_id),
                    &key.expected_actor_id,
                )
            })
            .unwrap();
        assert_eq!(conflict["conflict"]["conflict_id"], req.operation_id);
        std::fs::write(
            r.root.join(path),
            STANDARD.decode(&base.content_base64).unwrap(),
        )
        .unwrap();
        assert!(r
            .with_goal(&g, |r| r.discussion_decision_reuse_write(
                &g,
                id,
                req.clone(),
                base.clone(),
                &key.expected_actor_id
            ))
            .is_err());
        let next = request(&base, &g, id, &key.expected_actor_id);
        assert_ne!(next.operation_id, req.operation_id);
        r.with_goal(&g, |r| {
            r.discussion_decision_reuse_write(&g, id, next, base, &key.expected_actor_id)
        })
        .unwrap();
    }
    #[test]
    fn bounded_preflight_and_corrupt_pending_never_commit_or_acknowledge() {
        let (_dir, mut r, _, g, _) = fixture();
        let key = turn(&mut r, &g, "Bounded source", None);
        let view = get(&mut r, &key);
        save(&mut r, &key, &view).unwrap();
        let id = view["decision_id"].as_str().unwrap();
        let path = view["path"].as_str().unwrap();
        let base = r.read_source(path).unwrap();
        let req = request(&base, &g, id, &key.expected_actor_id);
        let journal = r
            .state_dir
            .join("source")
            .join(format!("{}.json", req.operation_id));
        std::fs::write(r.root.join(path), vec![b'x'; reuse::MAX_BYTES + 1]).unwrap();
        assert!(r
            .with_goal(&g, |r| r.discussion_decision_reuse_write(
                &g,
                id,
                req.clone(),
                base.clone(),
                &key.expected_actor_id
            ))
            .is_err());
        assert!(!journal.exists());
        let proposed = reuse::proposed(&req).unwrap();
        std::fs::write(
            r.root.join(path),
            STANDARD.decode(&proposed.content_base64).unwrap(),
        )
        .unwrap();
        let record = RecoveryRecord {
            base: Some(base.clone()),
            request: req.clone(),
            preimage_base64: Some(base.content_base64.clone()),
            previous_revision: Some(base.revision.clone()),
            receipt: None,
            conflict: None,
            divergent_observations_base64: vec![],
        };
        for field in [
            "preimage_base64",
            "previous_revision",
            "divergent_observations_base64",
        ] {
            let mut corrupt = serde_json::to_value(&record).unwrap();
            corrupt[field] = if field == "divergent_observations_base64" {
                json!([null])
            } else {
                Value::Null
            };
            let exact = serde_json::to_vec(&corrupt).unwrap();
            std::fs::write(&journal, &exact).unwrap();
            assert!(r
                .with_goal(&g, |r| r.discussion_decision_reuse_write(
                    &g,
                    id,
                    req.clone(),
                    base.clone(),
                    &key.expected_actor_id
                ))
                .is_err());
            assert_eq!(std::fs::read(&journal).unwrap(), exact);
        }
        // An actual coherent lost receipt can be resolved by exactly the same request.
        std::fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        r.with_goal(&g, |r| {
            r.discussion_decision_reuse_write(
                &g,
                id,
                req.clone(),
                base.clone(),
                &key.expected_actor_id,
            )
        })
        .unwrap();
        // Cross-workspace self-consistent request/journal is not this workspace's receipt.
        let original = std::fs::read(&journal).unwrap();
        let mut forged: Value = serde_json::from_slice(&original).unwrap();
        forged["request"]["brain_id"] = json!(Uuid::new_v4().to_string());
        std::fs::write(&journal, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(r
            .with_goal(&g, |r| r.reuse_record(&g, id, &req.operation_id))
            .is_err());
    }
    #[test]
    fn portable_policy_preserves_source_and_known_create_anchor_rejects_relabelled_history() {
        let (_dir, mut r, _, g, _) = fixture();
        let key = turn(&mut r, &g, "Portable original", None);
        let view = get(&mut r, &key);
        save(&mut r, &key, &view).unwrap();
        let id = view["decision_id"].as_str().unwrap();
        let path = view["path"].as_str().unwrap();
        let base = r.read_source(path).unwrap();
        let req = request(&base, &g, id, &key.expected_actor_id);
        r.with_goal(&g, |r| {
            r.discussion_decision_reuse_write(
                &g,
                id,
                req.clone(),
                base.clone(),
                &key.expected_actor_id,
            )
        })
        .unwrap();
        // A self-consistent policy around a modified predecessor cannot bypass local original save.
        let mut changed = base.clone();
        let bytes = String::from_utf8(STANDARD.decode(&base.content_base64).unwrap())
            .unwrap()
            .replacen("received_at:", "custom: changed\nreceived_at:", 1);
        changed.revision = reuse::revision(bytes.as_bytes());
        changed.content_base64 = STANDARD.encode(bytes);
        let changed_request = request(&changed, &g, id, &key.expected_actor_id);
        std::fs::write(
            r.root.join(path),
            STANDARD.decode(&changed_request.content_base64).unwrap(),
        )
        .unwrap();
        assert_ne!(get(&mut r, &key)["reuse_state"], "manual_only");
        assert_eq!(
            r.with_goal(&g, |r| r.discussion_decision_reuse_get(
                &g,
                id,
                None,
                &key.expected_actor_id
            ))
            .unwrap()["state"],
            "unavailable"
        );
        // Portable exported canonical bytes have no create/disposition journals.
        std::fs::write(
            r.root.join(path),
            STANDARD.decode(&req.content_base64).unwrap(),
        )
        .unwrap();
        std::fs::remove_file(
            r.state_dir
                .join("source")
                .join(format!("{}.json", key.id(&r.state.brain_id, true))),
        )
        .unwrap();
        std::fs::remove_file(
            r.state_dir
                .join("source")
                .join(format!("{}.json", req.operation_id)),
        )
        .unwrap();
        let manual = r
            .with_goal(&g, |r| {
                r.discussion_decision_reuse_get(&g, id, None, "another-reader")
            })
            .unwrap();
        assert_eq!(manual["state"], "manual_only");
        assert_eq!(manual["text"], "Portable original");
        assert!(
            r.with_goal(&g, |r| r.goal_context_brief(&g)).unwrap()["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["id"] != id)
        );
    }
}

#[cfg(test)]
mod fixture_export {
    use super::*;
    fn copy(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let p = entry.path();
            let dest = to.join(entry.file_name());
            if p.is_dir() {
                copy(&p, &dest)?;
            } else {
                fs::copy(p, dest)?;
            }
        }
        Ok(())
    }
    fn inventory(root: &std::path::Path) -> Result<BTreeMap<String, String>> {
        fn visit(
            root: &std::path::Path,
            dir: &std::path::Path,
            out: &mut BTreeMap<String, String>,
        ) -> Result<()> {
            for entry in fs::read_dir(dir)? {
                let p = entry?.path();
                if p.is_dir() {
                    visit(root, &p, out)?;
                } else {
                    out.insert(
                        p.strip_prefix(root)?.to_string_lossy().to_string(),
                        reuse::revision(&fs::read(p)?),
                    );
                }
            }
            Ok(())
        }
        let mut out = BTreeMap::new();
        visit(root, root, &mut out)?;
        Ok(out)
    }
    #[test]
    #[ignore = "requires an empty TESSERA_REUSE_FIXTURE directory"]
    fn export_decision_reuse261_fixture() {
        use super::super::discussion_decision::tests::{get, save, turn};
        let target = PathBuf::from(std::env::var_os("TESSERA_REUSE_FIXTURE").unwrap());
        assert!(target.is_dir() && fs::read_dir(&target).unwrap().next().is_none());
        let working = target.join("working");
        fs::create_dir_all(working.join("brain/records")).unwrap();
        fs::create_dir_all(working.join("state")).unwrap();
        let config = RunnerConfig {
            brain_id: Uuid::new_v4().to_string(),
            root: working.join("brain"),
            operational_dir: working.join("state"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let (mut r, a, b) = super::super::goal_brief::tests::fixture_goals(&config);
        let key = turn(
            &mut r,
            &a,
            "Use option B; keep the existing database. Exact retained input 261.",
            None,
        );
        let create = get(&mut r, &key);
        save(&mut r, &key, &create).unwrap();
        let control = turn(
            &mut r,
            &a,
            "Keep the public API stable. Automatic positive control 261.",
            None,
        );
        let control_create = get(&mut r, &control);
        save(&mut r, &control, &control_create).unwrap();
        let id = create["decision_id"].as_str().unwrap();
        let path = create["path"].as_str().unwrap();
        let base = r.read_source(path).unwrap();
        r.export_exact(&target.join("active-brain.tar")).unwrap();
        copy(&working, &target.join("active-local")).unwrap();
        let policy = Disposition::new(
            Uuid::new_v4().to_string(),
            key.expected_actor_id.clone(),
            "2026-09-08T16:00:00Z".into(),
            base.revision.clone(),
        );
        let proposed = reuse::transform(&base, &a, id, &policy).unwrap();
        let request = SourceWrite {
            schema: SCHEMA.into(),
            brain_id: base.brain_id.clone(),
            path: base.path.clone(),
            operation_id: policy.operation_id,
            expected_revision: Some(base.revision.clone()),
            content_base64: STANDARD.encode(proposed),
        };
        let receipt = r
            .with_goal(&a, |r| {
                r.discussion_decision_reuse_write(
                    &a,
                    id,
                    request.clone(),
                    base.clone(),
                    &key.expected_actor_id,
                )
            })
            .unwrap();
        r.export_exact(&target.join("manual-brain.tar")).unwrap();
        copy(&working, &target.join("manual-local")).unwrap();
        for name in ["active", "manual"] {
            let dest = target.join(format!("{name}-portable"));
            fs::create_dir(&dest).unwrap();
            tar::Archive::new(fs::File::open(target.join(format!("{name}-brain.tar"))).unwrap())
                .unpack(&dest)
                .unwrap();
        }
        let mut inventories = BTreeMap::new();
        for name in [
            "active-local",
            "manual-local",
            "active-portable",
            "manual-portable",
        ] {
            inventories.insert(name, inventory(&target.join(name)).unwrap());
        }
        let metadata = json!({"schema":"tessera-reuse-fixture/v1","logical_root":config.root,"logical_state":config.operational_dir,"workspace":r.workspace_identity(),"goal_a":a,"goal_b":b,"origin_key":key,"decision_id":id,"decision_path":path,"create_view":create,"base":base,"policy_request":request,"policy_receipt":receipt,"positive_control":control_create,"inventories":inventories,"provider_calls":0,"archive_exports":"actual Runner::export_exact followed by independent tar extraction"});
        fs::write(
            target.join("fixture.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();
    }
}
