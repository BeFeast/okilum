use super::super::proposal_generation::tests::{
    capture, detail, disposition, identity, output, settings, Fixture,
};
use super::*;
fn failed(r: &mut Runner) -> String {
    let id = capture(r, "Retry original exact thought");
    r.prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    r.finish_proposal_generation(
        &id,
        None,
        Some(&settings()),
        Err(api::Failure::ProviderFailed),
    )
    .unwrap();
    id
}
fn request(r: &Runner, id: &str) -> api::RetryRequest {
    let source = identity();
    api::RetryRequest {
        operation_id: source.message_id.clone(),
        proposal_id: id.into(),
        goal_id: None,
        expected_revision: detail(r, id).source.revision,
        source,
    }
}
#[test]
fn retry_replays_original_receipt_and_preserves_exact_input_history() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let before = detail(&r, &id);
    let request = request(&r, &id);
    let receipt = r
        .proposal_retry(request.clone(), "operator", Some(&settings()))
        .unwrap();
    let api::RetryOutcome::Committed {
        ref previous_attempt_id,
        ref attempt_id,
        ..
    } = receipt.result
    else {
        panic!()
    };
    assert_eq!(previous_attempt_id, &before.record.attempt.id);
    assert_ne!(attempt_id, previous_attempt_id);
    assert_eq!(detail(&r, &id).record.attempt.state, AttemptState::Queued);
    assert_eq!(
        detail(&r, &id).record.attempt_history[0].attempt,
        before.record.attempt
    );
    drop(r);
    let mut r = f.open();
    let replay = r
        .proposal_retry(request.clone(), "operator", Some(&settings()))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.result, receipt.result);
    let job = r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    assert_eq!(
        job.body,
        before
            .record
            .attempt
            .input
            .unwrap()
            .generation
            .unwrap()
            .request_body
    );
    r.finish_proposal_generation(&id, None, Some(&settings()), Ok(output()))
        .unwrap();
    let after = detail(&r, &id);
    assert_eq!(after.record.attempt.state, AttemptState::Draft);
    assert_eq!(after.record.attempt_history.len(), 1);
    assert_eq!(
        after.record.attempt_history[0].failure,
        Some(api::Failure::ProviderFailed)
    );
    assert_eq!(
        r.proposal_retry(request, "operator", Some(&settings()))
            .unwrap()
            .result,
        receipt.result
    );
    drop(r);
    let mut r = f.open();
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    assert_eq!(detail(&r, &id).source, after.source);
}
#[test]
fn rejected_request_gets_durable_not_applied_and_changed_identity_refuses() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let request = request(&r, &id);
    disposition(&mut r, &id, api::Disposition::Rejected);
    let before = detail(&r, &id).source;
    let receipt = r
        .proposal_retry(request.clone(), "operator", Some(&settings()))
        .unwrap();
    assert!(matches!(
        receipt.result,
        api::RetryOutcome::NotApplied { .. }
    ));
    assert_eq!(detail(&r, &id).source, before);
    drop(r);
    let mut r = f.open();
    assert_eq!(
        r.proposal_retry(request.clone(), "operator", None)
            .unwrap()
            .result,
        receipt.result
    );
    let mut changed = request;
    changed.expected_revision = before.revision;
    assert!(r
        .proposal_retry(changed, "operator", Some(&settings()))
        .is_err());
}
#[test]
fn running_retry_interrupts_without_resend_and_explicit_new_retry_keeps_history() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let request = request(&r, &id);
    r.proposal_retry(request, "operator", Some(&settings()))
        .unwrap();
    r.prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    drop(r);
    let mut r = f.open();
    assert_eq!(
        detail(&r, &id).record.attempt.state,
        AttemptState::Interrupted
    );
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    let next = self::request(&r, &id);
    r.proposal_retry(next, "operator", Some(&settings()))
        .unwrap();
    assert_eq!(detail(&r, &id).record.attempt_history.len(), 2);
    r.prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    disposition(
        &mut r,
        &id,
        api::Disposition::Snoozed {
            until: "2099-01-01T00:00:00Z".into(),
        },
    );
    r.finish_proposal_generation(&id, None, Some(&settings()), Ok(output()))
        .unwrap();
    assert_eq!(detail(&r, &id).record.attempt_history.len(), 2);
    assert_eq!(detail(&r, &id).record.history.len(), 1);
}

#[test]
fn reject_queued_retry_prevents_dispatch_and_releases_other_inputs() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let request = request(&r, &id);
    r.proposal_retry(request, "operator", Some(&settings()))
        .unwrap();
    disposition(&mut r, &id, api::Disposition::Rejected);
    assert_eq!(
        detail(&r, &id).record.attempt.state,
        AttemptState::Interrupted
    );
    let next = capture(&mut r, "Other input");
    assert_eq!(
        r.prepare_proposal_generation(Some(settings()))
            .unwrap()
            .unwrap()
            .id,
        next
    );
}

#[test]
fn retry_projection_cuts_recover_original_receipt_and_attempt() {
    use super::super::proposal_drafts::Fault;
    for fault in [
        Fault::BeforeProjection,
        Fault::AfterProjection,
        Fault::AfterReceipt,
    ] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = failed(&mut r);
        let request = request(&r, &id);
        r.proposal_draft_fault = Some(fault);
        assert!(r
            .proposal_retry(request.clone(), "operator", Some(&settings()))
            .is_err());
        let original_attempt = r.drafts().unwrap().intents().unwrap()[&id]
            .attempt
            .id
            .clone();
        drop(r);
        let mut r = f.open();
        let receipt = r
            .proposal_retry(request.clone(), "operator", Some(&settings()))
            .unwrap();
        let api::RetryOutcome::Committed { attempt_id, .. } = receipt.result else {
            panic!()
        };
        assert_eq!(attempt_id, original_attempt);
        assert_eq!(detail(&r, &id).record.attempt_history.len(), 1);
        assert_eq!(
            r.prepare_proposal_generation(Some(settings()))
                .unwrap()
                .unwrap()
                .id,
            id
        );
    }
}

#[test]
fn changed_sources_get_terminal_but_missing_source_keeps_uncertainty() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let request = request(&r, &id);
    let path = r.root.join(detail(&r, &id).record.trigger.source_path);
    let saved = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(r
        .proposal_retry(request.clone(), "operator", Some(&settings()))
        .is_err());
    assert!(r
        .drafts()
        .unwrap()
        .retry_replay(&r.workspace_identity(), &request)
        .unwrap()
        .is_none());
    fs::write(&path, "changed").unwrap();
    let result = r
        .proposal_retry(request.clone(), "operator", Some(&settings()))
        .unwrap();
    assert!(matches!(
        result.result,
        api::RetryOutcome::NotApplied {
            reason: api::RetryRefusal::InputChanged
        }
    ));
    fs::write(&path, saved).unwrap();
    assert_eq!(
        r.proposal_retry(request, "operator", Some(&settings()))
            .unwrap()
            .result,
        result.result
    );
}

#[test]
fn retry_operation_namespace_is_shared_in_both_directions() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let seed = identity();
    r.inbox_capture(
        crate::inbox::CaptureRequest {
            operation_id: seed.message_id.clone(),
            text: "Known capture".into(),
            source: seed.clone(),
        },
        "operator",
    )
    .unwrap();
    let mut collision = request(&r, &id);
    collision.operation_id = seed.message_id.clone();
    collision.source = seed;
    assert!(r
        .proposal_retry(collision, "operator", Some(&settings()))
        .is_err());
    let request = request(&r, &id);
    r.proposal_retry(request.clone(), "operator", Some(&settings()))
        .unwrap();
    assert!(r
        .inbox_capture(
            crate::inbox::CaptureRequest {
                operation_id: request.operation_id.clone(),
                text: "Must refuse reused identity".into(),
                source: request.source.clone()
            },
            "operator"
        )
        .is_err());
    assert!(r
        .proposal_disposition(
            api::Request {
                operation_id: request.operation_id,
                proposal_id: id.clone(),
                goal_id: None,
                expected_revision: detail(&r, &id).source.revision,
                source: request.source,
                disposition: api::Disposition::Rejected
            },
            "operator"
        )
        .is_err());
}

#[test]
fn retry_shares_queue_bound_and_preserves_request_when_full() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    for index in 0..32 {
        capture(&mut r, &format!("Queued {index}"));
    }
    let before = detail(&r, &id).source;
    let request = request(&r, &id);
    let error = r
        .proposal_retry(request.clone(), "operator", Some(&settings()))
        .unwrap_err();
    assert!(error.to_string().contains("queue full"), "{error}");
    assert_eq!(detail(&r, &id).source, before);
    assert!(r
        .drafts()
        .unwrap()
        .retry_replay(&r.workspace_identity(), &request)
        .unwrap()
        .is_none());
    r.prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    assert!(matches!(
        r.proposal_retry(request, "operator", Some(&settings()))
            .unwrap()
            .result,
        api::RetryOutcome::Committed { .. }
    ));
}

#[test]
fn retry_fence_blocks_generation_only_writer_before_mutation() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let source_dir = r.state_dir.join("source");
    let old = SourceStore::open_with_proposal_generation(
        &r.state.brain_id,
        &r.root,
        &source_dir,
        WriteBoundary::Managed,
    )
    .unwrap();
    let write = SourceWrite {
        schema: SCHEMA.into(),
        operation_id: Uuid::new_v4().to_string(),
        brain_id: r.state.brain_id.clone(),
        path: "positive-retry.md".into(),
        expected_revision: None,
        content_base64: STANDARD.encode("positive"),
    };
    old.write(write.clone()).unwrap();
    let request = request(&r, &id);
    r.proposal_retry(request, "operator", Some(&settings()))
        .unwrap();
    let mut blocked = write;
    blocked.path = "forbidden-retry.md".into();
    blocked.operation_id = Uuid::new_v4().to_string();
    assert!(old.write(blocked).is_err());
    assert!(!r.root.join("forbidden-retry.md").exists());
    assert!(SourceStore::open_with_proposal_generation(
        &r.state.brain_id,
        &r.root,
        &source_dir,
        WriteBoundary::Managed
    )
    .is_err());
    drop(old);
    drop(r);
    let r = f.open();
    assert_eq!(detail(&r, &id).record.attempt.state, AttemptState::Queued);
}

#[test]
fn canonical_retry_admission_reserves_running_and_maximum_result_before_ack() {
    use crate::proposals::drafts::{TEST_CANDIDATE_BYTES, TEST_CANONICAL_LIMIT};
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_CANONICAL_LIMIT.with(|v| v.set(1024 * 1024 - 1024));
        }
    }
    let _reset = Reset;
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let request = request(&r, &id);
    r.source.require_proposal_retry().unwrap();
    let workspace = r.workspace_identity();
    let journal = r.state_dir.join("proposal-intents-v1/journal.json");
    let before = fs::read(&journal).unwrap();
    let source_before = detail(&r, &id).source;
    let admit = |r: &mut Runner| {
        r.proposal_store.as_mut().unwrap().reserve_retry(
            workspace.clone(),
            request.clone(),
            None,
            "2026-09-07T00:00:00Z".into(),
        )
    };
    // Calibrate this valid candidate's serialized size without admitting it. Only
    // the test-local budget is reduced; production keeps the full1MiB limit.
    TEST_CANONICAL_LIMIT.with(|v| v.set(1));
    assert!(admit(&mut r)
        .unwrap_err()
        .to_string()
        .contains("canonical proposal exceeds generation publication reserve"));
    let queued_bytes = TEST_CANDIDATE_BYTES.with(|v| v.get());
    let admitted_limit = queued_bytes + 257 * 1024;
    assert!(admitted_limit < 1024 * 1024 - 1024);
    TEST_CANONICAL_LIMIT.with(|v| v.set(admitted_limit - 1));
    assert!(admit(&mut r)
        .unwrap_err()
        .to_string()
        .contains("canonical proposal exceeds generation publication reserve"));
    assert_eq!(fs::read(&journal).unwrap(), before);
    assert_eq!(detail(&r, &id).source, source_before);
    TEST_CANONICAL_LIMIT.with(|v| v.set(admitted_limit));
    admit(&mut r).unwrap();
    r.recover_draft_projections_for(Some(&id)).unwrap();
    r.prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    let generated = api::Generated {
        title: "x".repeat(512),
        criteria: vec!["c".repeat(512); 49],
        rationale: "r".repeat(4096),
        open_questions: vec![],
        citation_ids: vec![],
    };
    generated
        .validate(&detail(&r, &id).record.captured)
        .unwrap();
    let output = serde_json::to_string(&generated).unwrap();
    assert!(output.len() > 29 * 1024 && output.len() <= 32 * 1024);
    r.finish_proposal_generation(&id, None, Some(&settings()), Ok(output))
        .unwrap();
    assert_eq!(detail(&r, &id).record.generated, Some(generated));
    assert!(detail(&r, &id).record.bytes().unwrap().len() <= admitted_limit);
}

#[test]
fn retry_rejects_rewritten_history_even_with_matching_projection_and_receipt() {
    for append_duplicate in [false, true] {
        assert_corrupt_retry_history_refused(append_duplicate);
    }
}

#[test]
fn retry_reopen_rechecks_projection_bytes_and_operation_identity() {
    for change in ["bytes", "operation_id"] {
        let f = Fixture::new();
        let mut r = f.enrolled();
        let id = failed(&mut r);
        let request = request(&r, &id);
        r.proposal_retry(request, "operator", Some(&settings()))
            .unwrap();
        let state_dir = r.state_dir.clone();
        let brain_id = r.state.brain_id.clone();
        let binding = r.source.required_proposal_feed().unwrap().clone();
        let canonical = detail(&r, &id).source;
        let canonical_path = r.root.join(&canonical.path);
        drop(r);
        let open = || {
            crate::proposals::Store::open_bound_without_recovery(&state_dir, &brain_id, &binding)
        };
        // The exact retained transaction must pass before and after the corruption.
        drop(open().unwrap());
        let path = state_dir.join("proposal-intents-v1/journal.json");
        let original = fs::read(&path).unwrap();
        let mut journal: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let projections = journal["intents"][&id]["draft"]["projections"]
            .as_array_mut()
            .unwrap();
        assert!(projections.len() > 1);
        if change == "bytes" {
            let mut bytes = STANDARD
                .decode(projections[0]["write"]["content_base64"].as_str().unwrap())
                .unwrap();
            bytes.push(b'\n');
            projections[0]["write"]["content_base64"] = STANDARD.encode(bytes).into();
        } else {
            projections.last_mut().unwrap()["write"]["operation_id"] =
                projections[0]["write"]["operation_id"].clone();
        }
        let corrupt = serde_json::to_vec(&journal).unwrap();
        fs::write(&path, &corrupt).unwrap();
        assert!(
            open().is_err(),
            "changed {change} must refuse before recovery"
        );
        assert_eq!(fs::read(&path).unwrap(), corrupt);
        assert_eq!(
            fs::read(&canonical_path).unwrap(),
            STANDARD.decode(&canonical.content_base64).unwrap()
        );
        fs::write(&path, &original).unwrap();
        drop(open().unwrap());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

fn assert_corrupt_retry_history_refused(append_duplicate: bool) {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let request = request(&r, &id);
    r.proposal_retry(request.clone(), "operator", Some(&settings()))
        .unwrap();
    let state_dir = r.state_dir.clone();
    let brain_id = r.state.brain_id.clone();
    let binding = r.source.required_proposal_feed().unwrap().clone();
    let canonical = detail(&r, &id).source;
    let canonical_path = r.root.join(&canonical.path);
    drop(r);
    // Positive control: the unmodified retained transaction opens cleanly.
    drop(
        crate::proposals::Store::open_bound_without_recovery(&state_dir, &brain_id, &binding)
            .unwrap(),
    );
    let path = state_dir.join("proposal-intents-v1/journal.json");
    let mut journal: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let draft = &mut journal["intents"][&id]["draft"];
    let mut record: api::Record = serde_json::from_value(draft["record"].clone()).unwrap();
    if append_duplicate {
        let mut forged = record.attempt_history[0].clone();
        forged.attempt.id = Uuid::new_v4().to_string();
        record.attempt_history.push(forged);
    } else {
        record.attempt_history[0].failure = Some(api::Failure::ProviderTimeout);
    }
    let bytes = record.bytes().unwrap();
    let revision = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
    draft["record"] = serde_json::to_value(record).unwrap();
    let projection = draft["projections"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap();
    projection["write"]["content_base64"] = STANDARD.encode(&bytes).into();
    projection["receipt"]["revision"] = revision.clone().into();
    journal["retries"][&request.operation_id]["receipt"]["result"]["revision"] = revision.into();
    let corrupt = serde_json::to_vec_pretty(&journal).unwrap();
    fs::write(&path, &corrupt).unwrap();
    let error =
        crate::proposals::Store::open_bound_without_recovery(&state_dir, &brain_id, &binding)
            .err()
            .expect("rewritten history must refuse before recovery");
    let expected = if append_duplicate {
        "archived attempt does not bind a unique committed retry"
    } else {
        "retry immutable attempt history differs from original projections"
    };
    assert!(error.to_string().contains(expected), "{error}");
    assert_eq!(fs::read(&path).unwrap(), corrupt);
    assert_eq!(
        fs::read(canonical_path).unwrap(),
        STANDARD.decode(canonical.content_base64).unwrap()
    );
}

#[test]
fn canonical_capacity_refusal_is_durable_without_queued_acceptance() {
    use crate::proposals::drafts::TEST_CANONICAL_LIMIT;
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    let request = request(&r, &id);
    let before = detail(&r, &id).source;
    TEST_CANONICAL_LIMIT.with(|v| v.set(1));
    let result = r.proposal_retry(request.clone(), "operator", Some(&settings()));
    TEST_CANONICAL_LIMIT.with(|v| v.set(1024 * 1024 - 1024));
    let receipt = result.unwrap();
    assert!(matches!(
        receipt.result,
        api::RetryOutcome::NotApplied {
            reason: api::RetryRefusal::CapacityExceeded
        }
    ));
    assert_eq!(detail(&r, &id).source, before);
    drop(r);
    let mut r = f.open();
    let replay = r.proposal_retry(request, "operator", None).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.result, receipt.result);
}

#[test]
fn full_attempt_history_gets_durable_not_applied() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = failed(&mut r);
    for index in 0..32 {
        let request = request(&r, &id);
        let started = std::time::Instant::now();
        r.proposal_retry(request, "operator", Some(&settings()))
            .unwrap();
        if index >= 28 {
            eprintln!("history {} retry duration {:?}", index, started.elapsed());
        }
        r.prepare_proposal_generation(Some(settings()))
            .unwrap()
            .unwrap();
        r.finish_proposal_generation(
            &id,
            None,
            Some(&settings()),
            Err(api::Failure::ProviderFailed),
        )
        .unwrap();
    }
    assert_eq!(detail(&r, &id).record.attempt_history.len(), 32);
    let request = request(&r, &id);
    let before = detail(&r, &id).source;
    let receipt = r
        .proposal_retry(request.clone(), "operator", Some(&settings()))
        .unwrap();
    assert!(matches!(
        receipt.result,
        api::RetryOutcome::NotApplied {
            reason: api::RetryRefusal::AttemptIneligible
        }
    ));
    assert_eq!(detail(&r, &id).source, before);
    drop(r);
    let mut r = f.open();
    assert_eq!(
        r.proposal_retry(request, "operator", None).unwrap().result,
        receipt.result
    );
}

#[test]
fn decision_reuse_actual_policy_changes_refuse_retry_but_no_policy_briefs_stay_exact() {
    use super::super::discussion_decision::tests::{get, save, turn};
    use super::super::goal_brief::tests::{fixture, save_reply};
    use okilum_core::decision_reuse::{self as reuse, Disposition};
    for change_policy in [false, true] {
        let (_dir, mut r, _, g, _) = fixture();
        let key = turn(&mut r, &g, "Retained retry constraint 261", None);
        let view = get(&mut r, &key);
        save(&mut r, &key, &view).unwrap();
        r.enroll_proposal_feed(1).unwrap();
        r.enroll_proposal_drafts().unwrap();
        r.enroll_proposal_generation().unwrap();
        save_reply(&mut r, &g, "Actual Attention trigger for retry");
        let job = r
            .prepare_proposal_generation(Some(settings()))
            .unwrap()
            .unwrap();
        let proposal_id = job.id.clone();
        r.finish_proposal_generation(
            &proposal_id,
            Some(&g),
            Some(&settings()),
            Err(api::Failure::ProviderFailed),
        )
        .unwrap();
        let before = r
            .proposal_get(api::Lookup {
                proposal_id: proposal_id.clone(),
                goal_id: Some(g.clone()),
            })
            .unwrap();
        let frozen = before
            .record
            .attempt
            .input
            .as_ref()
            .unwrap()
            .generation
            .as_ref()
            .unwrap();
        let old_brief = frozen.goal_brief.clone().unwrap();
        assert!(old_brief.get("manual_only").is_none());
        assert!(old_brief.get("manual_only_truncated").is_none());
        let frozen_exact = serde_json::to_vec(frozen).unwrap();
        if change_policy {
            let id = view["decision_id"].as_str().unwrap();
            let base = r.read_source(view["path"].as_str().unwrap()).unwrap();
            let p = Disposition::new(
                Uuid::new_v4().to_string(),
                key.expected_actor_id.clone(),
                "2026-09-08T16:00:00Z".into(),
                base.revision.clone(),
            );
            let proposed = reuse::transform(&base, &g, id, &p).unwrap();
            let req = SourceWrite {
                schema: SCHEMA.into(),
                brain_id: base.brain_id.clone(),
                path: base.path.clone(),
                operation_id: p.operation_id,
                expected_revision: Some(base.revision.clone()),
                content_base64: STANDARD.encode(proposed),
            };
            r.with_goal(&g, |r| {
                r.discussion_decision_reuse_write(&g, id, req, base, &key.expected_actor_id)
            })
            .unwrap();
        } else {
            assert_eq!(
                old_brief,
                r.with_goal(&g, |r| r.goal_context_brief(&g)).unwrap()
            );
        }
        let source = identity();
        let req = api::RetryRequest {
            operation_id: source.message_id.clone(),
            proposal_id: proposal_id.clone(),
            goal_id: Some(g.clone()),
            expected_revision: before.source.revision.clone(),
            source,
        };
        let receipt = r
            .proposal_retry(req, "operator", Some(&settings()))
            .unwrap();
        let output = serde_json::to_value(&receipt.result).unwrap();
        if change_policy {
            assert!(output.to_string().contains("input_changed"));
        } else {
            assert!(matches!(
                receipt.result,
                api::RetryOutcome::Committed { .. }
            ));
        }
        let reread = r
            .proposal_get(api::Lookup {
                proposal_id: proposal_id.clone(),
                goal_id: Some(g.clone()),
            })
            .unwrap();
        let persisted = reread
            .record
            .attempt
            .input
            .as_ref()
            .unwrap()
            .generation
            .as_ref()
            .unwrap();
        assert_eq!(frozen_exact, serde_json::to_vec(persisted).unwrap());
        if change_policy {
            assert_eq!(before.source, reread.source);
        }
    }
}
