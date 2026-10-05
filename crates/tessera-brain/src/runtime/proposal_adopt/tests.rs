use super::super::{
    proposal_adoption::tests as context, proposal_generation::tests as generation,
    proposal_inbox_adoption::tests as inbox,
};
use super::*;

#[test]
fn inbox_public_form_creates_one_original_goal_and_replays_before_freshness() {
    let f = generation::Fixture::new();
    let mut r = f.enrolled();
    let id = generation::capture(&mut r, "Original captured thought for public form");
    r.prepare_proposal_generation(Some(generation::settings()))
        .unwrap()
        .unwrap();
    r.finish_proposal_generation(
        &id,
        None,
        Some(&generation::settings()),
        Ok(generation::output()),
    )
    .unwrap();
    let form = inbox::adopt(&r, &id);
    let request = api::AdoptRequest::Inbox(Box::new(form.clone()));
    let reply = r.proposal_adopt(request.clone(), "operator").unwrap();
    assert!(!reply.replayed);
    let api::AdoptOutcome::CommittedInbox { receipt } = &reply.result else {
        panic!()
    };
    let goal = r
        .with_goal(&receipt.target.goal_id, |r| Ok(r.snapshot()?.goal.unwrap()))
        .unwrap();
    assert_eq!(goal.title, form.title);
    assert_eq!(goal.criteria, form.criteria);
    assert!(goal.stage_ids.is_empty() && goal.task_ref.is_none());
    fs::write(
        r.root.join(r.path("inbox", &form.capture_id)),
        "Later manual source change",
    )
    .unwrap();
    drop(r);
    let mut r = f.open();
    let replay = r.proposal_adopt(request.clone(), "operator").unwrap();
    assert!(replay.replayed);
    let api::AdoptOutcome::CommittedInbox { receipt: original } = replay.result else {
        panic!()
    };
    assert_eq!(original.target, receipt.target);
    assert_eq!(original.child, receipt.child);
    let api::AdoptRequest::Inbox(mut changed) = request else {
        panic!()
    };
    changed.title = "Changed identity payload must refuse".into();
    assert!(r
        .proposal_adopt(api::AdoptRequest::Inbox(changed), "operator")
        .is_err());
}

#[test]
fn context_public_form_validates_original_base_with_another_goal_selected() {
    let f = context::Fixture::new();
    let mut r = f.enrolled();
    let (goal, id) = context::ready(&mut r);
    let mut form = context::adoption(&r, &goal, &id);
    let app = crate::application::Application::unconfigured();
    let base = app
        .context_prepare(
            &mut r,
            goal.clone(),
            "Original manual context".into(),
            form.scope.clone(),
            vec![],
            vec![],
        )
        .unwrap();
    let base: crate::context::ReviewedPacket =
        serde_json::from_value(base["packet"].clone()).unwrap();
    form.expected_base_packet = Some(base.id.clone());
    form.expected_base_revision = Some(base.revision.clone());
    form.guidance = format!("{}\nAdded proposal guidance", base.text);
    let mut other_goal = r.snapshot().unwrap().goal.unwrap();
    other_goal.id = Uuid::new_v4().to_string();
    other_goal.title = "A different goal definition".into();
    let other = other_goal.id.clone();
    r.create_goal(other_goal, "# Unrelated goal".into())
        .unwrap();
    let request = api::AdoptRequest::Context(Box::new(form.clone()));
    r.state.route(&other).unwrap();
    let other_application = r.application_state();
    // The same base looks stale under the wrong selected goal; this positive
    // control makes the cross-owner preflight failure observable.
    assert!(crate::context::read(&r, &goal, &base.id).unwrap().stale);
    assert!(
        !r.with_goal(&goal, |r| crate::context::read(r, &goal, &base.id))
            .unwrap()
            .stale
    );
    let reply = r.proposal_adopt(request.clone(), "operator").unwrap();
    let api::AdoptOutcome::CommittedContext { receipt } = &reply.result else {
        panic!()
    };
    assert_eq!(receipt.goal_id, goal);
    let packet = crate::context::read(&r, &goal, &receipt.target.packet_id).unwrap();
    assert_eq!(packet.text, form.guidance);
    assert!(!packet.reviewed);
    assert_eq!(r.snapshot().unwrap().goal.unwrap().id, other);
    assert_eq!(r.application_state(), other_application);
    drop(r);
    let mut r = f.open();
    assert!(r.proposal_adopt(request, "operator").unwrap().replayed);
}

#[test]
fn public_adoption_pending_crash_cuts_recover_original_targets() {
    use super::super::proposal_adoption::Fault as C;
    for fault in [
        C::AfterIntent,
        C::BeforeTarget,
        C::AfterTarget,
        C::AfterTargetReceipt,
        C::AfterPointer,
    ] {
        let f = context::Fixture::new();
        let mut r = f.enrolled();
        let (goal, id) = context::ready(&mut r);
        let request = api::AdoptRequest::Context(Box::new(context::adoption(&r, &goal, &id)));
        r.proposal_adoption_fault = Some(fault);
        assert!(r.proposal_adopt(request.clone(), "operator").is_err());
        let target = r
            .drafts()
            .unwrap()
            .adoption(&id)
            .unwrap()
            .target()
            .unwrap()
            .packet_id()
            .to_owned();
        drop(r);
        let mut r = f.open();
        let reply = r.proposal_adopt(request, "operator").unwrap();
        let api::AdoptOutcome::CommittedContext { receipt } = reply.result else {
            panic!()
        };
        assert_eq!(receipt.target.packet_id, target);
        assert!(reply.replayed);
    }
    use super::super::proposal_inbox_adoption::Fault as I;
    for fault in [I::ParentIntent, I::ParentReceipt] {
        let f = inbox::Fixture::new();
        let mut r = f.enrolled();
        let id = inbox::ready(&mut r);
        let request = api::AdoptRequest::Inbox(Box::new(inbox::adopt(&r, &id)));
        r.proposal_inbox_adoption_fault = Some(fault);
        assert!(r.proposal_adopt(request.clone(), "operator").is_err());
        let target = r
            .drafts()
            .unwrap()
            .inbox_adoption(&id)
            .unwrap()
            .target_ref()
            .unwrap();
        drop(r);
        let mut r = f.open();
        let reply = r.proposal_adopt(request, "operator").unwrap();
        let api::AdoptOutcome::CommittedInbox { receipt } = reply.result else {
            panic!()
        };
        assert_eq!(receipt.target, target);
        assert!(reply.replayed);
    }
}

#[test]
fn missing_source_stays_uncertain_positive_change_gets_durable_terminal() {
    let f = inbox::Fixture::new();
    let mut r = f.enrolled();
    let id = inbox::ready(&mut r);
    let form = inbox::adopt(&r, &id);
    let request = api::AdoptRequest::Inbox(Box::new(form.clone()));
    let path = r.root.join(r.path("inbox", &form.capture_id));
    let original = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(r.proposal_adopt(request.clone(), "operator").is_err());
    assert!(r
        .drafts()
        .unwrap()
        .terminal_adoption_replay(&r.workspace_identity(), &request)
        .unwrap()
        .is_none());
    fs::write(&path, [original.as_slice(), b"\nHuman edit"].concat()).unwrap();
    let reply = r.proposal_adopt(request.clone(), "operator").unwrap();
    assert!(matches!(
        reply.result,
        api::AdoptOutcome::NotApplied {
            reason: api::AdoptRefusal::InputChanged
        }
    ));
    fs::write(path, original).unwrap();
    drop(r);
    let mut r = f.open();
    let replay = r.proposal_adopt(request, "operator").unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.result, reply.result);
}

#[test]
fn terminal_adoption_and_retry_share_the_same_operation_namespace() {
    let f = generation::Fixture::new();
    let mut r = f.enrolled();
    let id = generation::capture(&mut r, "Namespace fixture");
    r.prepare_proposal_generation(Some(generation::settings()))
        .unwrap()
        .unwrap();
    r.finish_proposal_generation(
        &id,
        None,
        Some(&generation::settings()),
        Err(api::Failure::ProviderFailed),
    )
    .unwrap();
    let form = inbox::adopt(&r, &id);
    let adopt = api::AdoptRequest::Inbox(Box::new(form.clone()));
    let retry = api::RetryRequest {
        operation_id: form.operation_id.clone(),
        proposal_id: id.clone(),
        goal_id: None,
        expected_revision: form.expected_revision.clone(),
        source: form.source.clone(),
    };
    let reply = r.proposal_adopt(adopt, "operator").unwrap();
    assert!(matches!(
        reply.result,
        api::AdoptOutcome::NotApplied {
            reason: api::AdoptRefusal::AttemptIneligible
        }
    ));
    assert!(r
        .proposal_retry(retry, "operator", Some(&generation::settings()))
        .is_err());
    let form = inbox::adopt(&r, &id);
    let retry = api::RetryRequest {
        operation_id: form.operation_id.clone(),
        proposal_id: id,
        goal_id: None,
        expected_revision: form.expected_revision.clone(),
        source: form.source.clone(),
    };
    r.proposal_retry(retry, "operator", Some(&generation::settings()))
        .unwrap();
    assert!(r
        .proposal_adopt(api::AdoptRequest::Inbox(Box::new(form)), "operator")
        .is_err());
}

#[test]
fn public_terminal_fence_refuses_preopened_retry_writer_without_source_changes() {
    let f = generation::Fixture::new();
    let mut r = f.enrolled();
    let id = generation::capture(&mut r, "Fenced public form");
    r.prepare_proposal_generation(Some(generation::settings()))
        .unwrap()
        .unwrap();
    r.finish_proposal_generation(
        &id,
        None,
        Some(&generation::settings()),
        Err(api::Failure::ProviderFailed),
    )
    .unwrap();
    let old = SourceStore::open_with_proposal_retry(
        &r.state.brain_id,
        &r.root,
        &r.state_dir.join("source"),
        WriteBoundary::Managed,
    )
    .unwrap();
    let mut write = SourceWrite {
        schema: SCHEMA.into(),
        operation_id: Uuid::new_v4().to_string(),
        brain_id: r.state.brain_id.clone(),
        path: "positive.md".into(),
        expected_revision: None,
        content_base64: STANDARD.encode("positive control"),
    };
    old.write(write.clone()).unwrap();
    let request = api::AdoptRequest::Inbox(Box::new(inbox::adopt(&r, &id)));
    r.proposal_adopt(request, "operator").unwrap();
    write.operation_id = Uuid::new_v4().to_string();
    write.path = "forbidden.md".into();
    assert!(old.write(write).is_err());
    assert!(!r.root.join("forbidden.md").exists());
    assert!(SourceStore::open_with_proposal_retry(
        &r.state.brain_id,
        &r.root,
        &r.state_dir.join("source"),
        WriteBoundary::Managed
    )
    .is_err());
}

#[test]
fn invalid_manual_form_retires_exact_request_without_accepting_target() {
    let f = context::Fixture::new();
    let mut r = f.open();
    r.enroll_proposal_feed(1).unwrap();
    r.enroll_proposal_drafts().unwrap();
    let (goal, id) = context::ready(&mut r);
    let app = crate::application::Application::unconfigured();
    let text = "# Evidence\n\nPinned source.\n";
    fs::write(r.root.join("evidence.md"), text).unwrap();
    let source = r.read_source("evidence.md").unwrap();
    let citation = crate::retrieval::Citation {
        citation_id: crate::retrieval::citation_id("evidence.md", &source.revision, 1, 3),
        path: "evidence.md".into(),
        revision: source.revision,
        start_line: 1,
        end_line: 3,
        locator: "L1-L3".into(),
        excerpt: text.into(),
        metadata: Default::default(),
    };
    let scope = crate::retrieval::SearchScope {
        goal_id: goal.clone(),
        mode: "project".into(),
        ..Default::default()
    };
    let base = app
        .context_prepare(
            &mut r,
            goal.clone(),
            "Manual context".into(),
            scope.clone(),
            vec![citation.clone()],
            vec![citation.citation_id.clone()],
        )
        .unwrap();
    let base: crate::context::ReviewedPacket =
        serde_json::from_value(base["packet"].clone()).unwrap();
    app.context_revise(
        &mut r,
        goal.clone(),
        base.id.clone(),
        base.revision,
        "Original manual guidance — 雪".into(),
    )
    .unwrap();
    let base = crate::context::read(&r, &goal, &base.id).unwrap();
    let mut form = context::adoption(&r, &goal, &id);
    form.expected_base_packet = Some(base.id.clone());
    form.expected_base_revision = Some(base.revision.clone());
    form.scope = scope;
    form.citations = vec![citation.clone()];
    form.pinned_citation_ids = vec![citation.citation_id];
    form.guidance = format!("{}\nAdded proposal guidance", base.text);
    let before = context::inventory(&r.root);
    for variant in 0..4 {
        let mut bad = form.clone();
        bad.source = context::identity();
        bad.operation_id = bad.source.message_id.clone();
        match variant {
            0 => bad.pinned_citation_ids.clear(),
            1 => bad.guidance = "Dropped original guidance".into(),
            2 => bad.guidance = "x".repeat(65_537),
            _ => bad.citations[0].excerpt = "Edited citation bytes".into(),
        }
        let request = api::AdoptRequest::Context(Box::new(bad.clone()));
        let receipt = r.proposal_adopt(request.clone(), "operator").unwrap();
        assert!(matches!(
            receipt.result,
            api::AdoptOutcome::NotApplied {
                reason: api::AdoptRefusal::InvalidForm
            }
        ));
        assert!(r.drafts().unwrap().adoption_replay(&bad).unwrap().is_none());
        assert_eq!(context::inventory(&r.root), before);
        assert!(!r.source.required_proposal_adoption());
        assert!(!r.drafts().unwrap().adoption_enabled());
        assert!(r.proposal_adopt(request, "operator").unwrap().replayed);
    }
    let reply = r
        .proposal_adopt(
            api::AdoptRequest::Context(Box::new(form.clone())),
            "operator",
        )
        .unwrap();
    let api::AdoptOutcome::CommittedContext { receipt } = reply.result else {
        panic!()
    };
    let packet = crate::context::read(&r, &goal, &receipt.target.packet_id).unwrap();
    assert_eq!(packet.text, form.guidance);
    assert_eq!(packet.citations, form.citations);
    assert_eq!(packet.pinned_citation_ids, form.pinned_citation_ids);
    assert!(!packet.reviewed);
}

#[test]
fn capacity_refusal_precedes_coordinator_enrollment_and_retains_exact_outcome() {
    for destination in ["context", "inbox"] {
        let f = context::Fixture::new();
        let mut r = f.open();
        r.enroll_proposal_feed(1).unwrap();
        r.enroll_proposal_drafts().unwrap();
        let request = if destination == "context" {
            let (goal, id) = context::ready(&mut r);
            api::AdoptRequest::Context(Box::new(context::adoption(&r, &goal, &id)))
        } else {
            let id = inbox::ready(&mut r);
            api::AdoptRequest::Inbox(Box::new(inbox::adopt(&r, &id)))
        };
        let path = r.state_dir.join("proposal-intents-v1/journal.json");
        let original = fs::read(&path).unwrap();
        let mut journal: Value = serde_json::from_slice(&original).unwrap();
        let mut size = original.len();
        let mut cursor = journal["cursor"].as_u64().unwrap();
        // Enough room for a terminal request, but insufficient room for either
        // adoption's mandatory target/projection/receipt completion reserve.
        let limit = 16 * 1024 * 1024 - 24 * 1024;
        loop {
            let next = cursor + 1;
            let key = next.to_string();
            let delta = 2 * (key.len() + 64 + 6) + key.len() - cursor.to_string().len();
            if size + delta > limit {
                break;
            }
            journal["events"][&key] = request.proposal_id().into();
            journal["event_bytes"][&key] = "a".repeat(64).into();
            cursor = next;
            size += delta;
        }
        journal["cursor"] = cursor.into();
        let padded = serde_json::to_vec(&journal).unwrap();
        assert_eq!(size, padded.len());
        fs::write(&path, padded).unwrap();
        r.proposal_store
            .as_mut()
            .unwrap()
            .reload_without_recovery()
            .unwrap();
        let sources = context::inventory(&r.root);
        let receipt = r.proposal_adopt(request.clone(), "operator").unwrap();
        assert!(matches!(
            receipt.result,
            api::AdoptOutcome::NotApplied {
                reason: api::AdoptRefusal::CapacityExceeded
            }
        ));
        assert_eq!(context::inventory(&r.root), sources);
        assert!(!r.source.required_proposal_adoption());
        assert!(!r.source.required_proposal_inbox_adoption());
        assert!(!r.drafts().unwrap().adoption_enabled());
        assert!(!r.drafts().unwrap().inbox_adoption_enabled());
        // Remove only synthetic duplicate-event padding before reopening the
        // full Runner, whose feed cursor must match its actual source consumer.
        // Retain the committed terminal receipt byte-for-byte as a JSON value.
        let mut retained: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let baseline: Value = serde_json::from_slice(&original).unwrap();
        for field in ["events", "event_bytes", "cursor"] {
            retained[field] = baseline[field].clone();
        }
        fs::write(&path, serde_json::to_vec(&retained).unwrap()).unwrap();
        drop(r);
        let mut r = f.open();
        let replay = r.proposal_adopt(request.clone(), "operator").unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.result, receipt.result);
        assert_eq!(context::inventory(&r.root), sources);
        let mut changed = request;
        match &mut changed {
            api::AdoptRequest::Inbox(r) => r.title.push_str(" edited"),
            api::AdoptRequest::Context(r) => r.guidance.push_str(" edited"),
        }
        assert!(r.proposal_adopt(changed, "operator").is_err());
    }
}
