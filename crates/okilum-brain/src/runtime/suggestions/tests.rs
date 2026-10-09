use super::*;
use crate::runtime::proposal_generation::tests::{capture, output, settings, Fixture};

fn request(revision: u64, enabled: bool) -> api::SetRequest {
    api::SetRequest {
        operation_id: Uuid::new_v4().to_string(),
        expected_revision: revision,
        enabled,
    }
}
fn provider() -> api::Provider {
    api::Provider {
        available: true,
        model: Some("fixture-model".into()),
        message: "configured".into(),
    }
}
fn ordinary_capture(r: &mut Runner, text: &str) {
    let source = crate::runtime::proposal_generation::tests::identity();
    r.inbox_capture(
        crate::inbox::CaptureRequest {
            operation_id: source.message_id.clone(),
            text: text.into(),
            source,
        },
        "operator",
    )
    .unwrap();
}
#[test]
fn ordinary_activation_has_no_backfill_and_pause_queues_across_restart() {
    let f = Fixture::new();
    let mut r = f.open();
    ordinary_capture(&mut r, "historical input");
    let before = fs::read(r.state_dir.join("state.json")).unwrap();
    assert_eq!(r.suggestions_status(provider()).unwrap().mode, "disabled");
    assert_eq!(before, fs::read(r.state_dir.join("state.json")).unwrap());
    assert!(!r.source.required_suggestions_control());
    let enable = request(0, true);
    r.suggestions_set(enable.clone(), "operator", true).unwrap();
    assert!(r.drafts().unwrap().intents().unwrap().is_empty());
    let binding = r.source.required_proposal_feed().unwrap().clone();
    let first = capture(&mut r, "new thought");
    let job = r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    assert_eq!(first, job.id);
    r.suggestions_set(request(1, false), "operator", false)
        .unwrap();
    assert!(r
        .proposal_generation_current(&job.id, None, Some(&job.settings))
        .unwrap());
    r.finish_proposal_generation(&job.id, None, Some(&job.settings), Ok(output()))
        .unwrap();
    let second = capture(&mut r, "thought while paused");
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    assert_eq!(r.suggestions_status(provider()).unwrap().queued, 1);
    drop(r);
    let mut r = f.open();
    assert_eq!(r.suggestions_status(provider()).unwrap().mode, "paused");
    let replay = r.suggestions_set(enable, "operator", false).unwrap();
    assert!(replay.enabled && replay.replayed);
    assert_eq!(r.suggestions_status(provider()).unwrap().mode, "paused");
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    r.suggestions_set(request(2, true), "operator", true)
        .unwrap();
    assert_eq!(&binding, r.source.required_proposal_feed().unwrap());
    assert_eq!(
        r.prepare_proposal_generation(Some(settings()))
            .unwrap()
            .unwrap()
            .id,
        second
    );
    assert_eq!(r.drafts().unwrap().intents().unwrap().len(), 2);
}
#[test]
fn activation_cuts_recover_same_request_without_dispatching_partial_state() {
    for cut in 0..=6 {
        let f = Fixture::new();
        let mut r = f.open();
        let command = request(0, true);
        r.suggestions_fault = Some(cut);
        assert!(r
            .suggestions_set(command.clone(), "operator", true)
            .is_err());
        if cut < 6 {
            assert!(!r.proposal_generation_enabled());
        }
        let prior_binding = r.source.required_proposal_feed().cloned();
        drop(r);
        let mut r = f.open();
        if cut == 0 {
            assert!(!r.proposal_generation_enabled());
            assert_eq!(r.suggestions_status(provider()).unwrap().mode, "disabled");
        }
        let receipt = r
            .suggestions_set(command.clone(), "operator", true)
            .unwrap();
        assert_eq!(receipt.request, command);
        assert_eq!(receipt.revision, 1);
        assert_eq!(r.state.suggestions.as_ref().unwrap().receipts.len(), 1);
        if let Some(mut prior) = prior_binding {
            prior.active = true;
            assert_eq!(&prior, r.source.required_proposal_feed().unwrap());
        }
        assert!(r.drafts().unwrap().intents().unwrap().is_empty());
    }
}
#[test]
fn missing_provider_and_guard_conflicts_remain_terminal_without_feed_enrollment() {
    let f = Fixture::new();
    let mut r = f.open();
    let command = request(0, true);
    assert!(r
        .suggestions_set(command.clone(), "operator", false)
        .is_err());
    assert!(r.source.required_suggestions_control());
    assert!(r.source.required_proposal_feed().is_none());
    assert!(r
        .suggestions_set(command.clone(), "operator", true)
        .is_err());
    drop(r);
    let mut r = f.open();
    assert_eq!(
        r.suggestions_set(command, "changed saved actor", true)
            .unwrap_err()
            .downcast_ref::<api::Refusal>(),
        Some(&api::Refusal::ProviderUnavailable)
    );
    let command = request(0, true);
    r.suggestions_set(command.clone(), "operator", true)
        .unwrap();
    let mut changed = command.clone();
    changed.enabled = false;
    assert!(r.suggestions_set(changed, "operator", true).is_err());
    let replay = r
        .suggestions_set(command.clone(), "other actor", true)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.actor, "operator");
    assert!(r
        .suggestions_set(request(0, false), "operator", true)
        .is_err());
    r.suggestions_set(request(1, false), "operator", false)
        .unwrap();
    assert!(
        r.suggestions_set(command, "operator", false)
            .unwrap()
            .replayed
    );
    assert_eq!(r.suggestions_status(provider()).unwrap().mode, "paused");
}
#[test]
fn first_pause_only_fences_control_and_preserves_ordinary_capture() {
    let f = Fixture::new();
    let mut r = f.open();
    r.suggestions_set(request(0, false), "operator", false)
        .unwrap();
    assert!(r.source.required_suggestions_control());
    assert!(r.source.required_proposal_feed().is_none());
    ordinary_capture(&mut r, "still an ordinary capture");
    assert_eq!(r.suggestions_status(provider()).unwrap().mode, "disabled");
    drop(r);
    let r = f.open();
    assert!(r.proposal_store.is_none());
}
#[test]
fn pause_restart_preserves_uncertain_attempt_without_resend() {
    let f = Fixture::new();
    let mut r = f.open();
    r.suggestions_set(request(0, true), "operator", true)
        .unwrap();
    let id = capture(&mut r, "first running request");
    r.prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    r.suggestions_set(request(1, false), "operator", false)
        .unwrap();
    drop(r);
    let mut r = f.open();
    assert_eq!(
        r.drafts().unwrap().intents().unwrap()[&id].attempt.state,
        AttemptState::Interrupted
    );
    r.suggestions_set(request(2, true), "operator", true)
        .unwrap();
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    assert_eq!(
        r.drafts().unwrap().intents().unwrap()[&id].attempt.state,
        AttemptState::Interrupted
    );
}
#[test]
fn settings_and_capture_operation_ids_are_reserved_in_both_directions() {
    let f = Fixture::new();
    let mut r = f.open();
    let source = crate::runtime::proposal_generation::tests::identity();
    let capture_request = crate::inbox::CaptureRequest {
        operation_id: source.message_id.clone(),
        text: "original".into(),
        source,
    };
    r.inbox_capture(capture_request.clone(), "operator")
        .unwrap();
    let mut set = request(0, true);
    set.operation_id = capture_request.operation_id.clone();
    assert!(r.suggestions_set(set, "operator", true).is_err());
    let set = request(0, true);
    r.suggestions_set(set.clone(), "operator", true).unwrap();
    let mut collision = capture_request;
    collision.operation_id = set.operation_id;
    assert!(r.inbox_capture(collision, "operator").is_err());
}
#[test]
fn predecessor_source_open_and_already_open_writes_are_fenced() {
    let f = Fixture::new();
    let mut r = f.open();
    let source_dir = r.state_dir.join("source");
    let old = SourceStore::open_with_public_proposal_adoption(
        &r.state.brain_id,
        &r.root,
        &source_dir,
        WriteBoundary::Managed,
    )
    .unwrap();
    let write = SourceWrite {
        schema: okilum_core::source::SCHEMA.into(),
        brain_id: r.state.brain_id.clone(),
        operation_id: Uuid::new_v4().to_string(),
        path: "records/control.md".into(),
        expected_revision: None,
        content_base64: STANDARD.encode("positive control"),
    };
    old.write(write.clone()).unwrap();
    r.suggestions_set(request(0, false), "operator", false)
        .unwrap();
    assert!(old.write(write).is_err());
    assert!(SourceStore::open_with_public_proposal_adoption(
        &r.state.brain_id,
        &r.root,
        &source_dir,
        WriteBoundary::Managed
    )
    .is_err());
    assert!(SourceStore::open_read_only(&r.state.brain_id, &r.root, &source_dir).is_ok());
}

#[test]
fn paused_retry_retains_frozen_request_and_settings_operation_namespace() {
    use crate::runtime::proposal_generation::tests::{detail, identity};
    let f = Fixture::new();
    let mut r = f.open();
    r.suggestions_set(request(0, true), "operator", true)
        .unwrap();
    let id = capture(&mut r, "retry after a failed attempt");
    let job = r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    r.finish_proposal_generation(
        &id,
        None,
        Some(&job.settings),
        Err(crate::proposal::Failure::ProviderFailed),
    )
    .unwrap();
    let pause = request(1, false);
    r.suggestions_set(pause.clone(), "operator", false).unwrap();
    let source = identity();
    let mut retry = crate::proposal::RetryRequest {
        operation_id: pause.operation_id.clone(),
        proposal_id: id.clone(),
        goal_id: None,
        expected_revision: detail(&r, &id).source.revision,
        source,
    };
    assert!(r
        .proposal_retry(retry.clone(), "operator", Some(&settings()))
        .is_err());
    retry.operation_id = retry.source.message_id.clone();
    r.proposal_retry(retry.clone(), "operator", Some(&settings()))
        .unwrap();
    let mut conflict = request(2, true);
    conflict.operation_id = retry.operation_id.clone();
    assert_eq!(
        r.suggestions_set(conflict, "operator", true)
            .unwrap_err()
            .downcast_ref::<api::Refusal>(),
        Some(&api::Refusal::IdentityConflict)
    );
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    drop(r);
    let mut r = f.open();
    assert!(
        r.proposal_retry(retry, "operator", Some(&settings()))
            .unwrap()
            .replayed
    );
    r.suggestions_set(request(2, true), "operator", true)
        .unwrap();
    let next = r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    assert_eq!(next.body, job.body);
    assert_eq!(next.id, id);
}

#[test]
fn post_rename_errors_reload_durable_terminal_or_accepted_control_before_other_writes() {
    for provider_available in [false, true] {
        let f = Fixture::new();
        let mut r = f.open();
        let command = request(0, true);
        r.suggestions_persist_fault
            .set(Some(if provider_available { 2 } else { 1 }));
        let error = r
            .suggestions_set(command.clone(), "operator", provider_available)
            .unwrap_err();
        assert!(error.to_string().contains("after journal rename"));
        ordinary_capture(&mut r, "unrelated capture after ambiguous fsync");
        drop(r);
        let mut r = f.open();
        let result = r.suggestions_set(command, "operator", true);
        if provider_available {
            let receipt = result.unwrap();
            assert!(receipt.replayed);
            assert_eq!(receipt.revision, 1);
        } else {
            assert_eq!(
                result.unwrap_err().downcast_ref::<api::Refusal>(),
                Some(&api::Refusal::ProviderUnavailable)
            );
            assert!(r.source.required_proposal_feed().is_none());
        }
    }
}
