use super::*;
use crate::inbox::{CaptureRequest, SourceIdentity};

pub(crate) struct Fixture {
    _dir: tempfile::TempDir,
    config: RunnerConfig,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("brain/records")).unwrap();
        fs::create_dir(dir.path().join("state")).unwrap();
        Self {
            config: RunnerConfig {
                brain_id: Uuid::new_v4().to_string(),
                root: dir.path().join("brain"),
                operational_dir: dir.path().join("state"),
                records_dir: "records".into(),
                boundary: WriteBoundary::Managed,
            },
            _dir: dir,
        }
    }
    pub(crate) fn open(&self) -> Runner {
        Runner::open(RunnerConfig {
            brain_id: self.config.brain_id.clone(),
            root: self.config.root.clone(),
            operational_dir: self.config.operational_dir.clone(),
            records_dir: self.config.records_dir.clone(),
            boundary: self.config.boundary,
        })
        .unwrap()
    }
    pub(crate) fn enrolled(&self) -> Runner {
        let mut r = self.open();
        r.enroll_proposal_feed(1).unwrap();
        r.enroll_proposal_drafts().unwrap();
        r.enroll_proposal_generation().unwrap();
        r
    }
}
pub(crate) fn identity() -> SourceIdentity {
    let id = Uuid::new_v4().to_string();
    SourceIdentity {
        channel: "native".into(),
        instance_id: Uuid::new_v4().to_string(),
        account_id: "local".into(),
        actor_id: "operator".into(),
        chat_id: None,
        topic_id: None,
        message_id: id.clone(),
        update_id: id,
        uri: None,
    }
}
pub(crate) fn capture(r: &mut Runner, text: &str) -> String {
    let source = identity();
    let receipt = r
        .inbox_capture(
            CaptureRequest {
                operation_id: source.message_id.clone(),
                text: text.into(),
                source,
            },
            "operator",
        )
        .unwrap();
    r.drafts()
        .unwrap()
        .intents()
        .unwrap()
        .iter()
        .find(|(_, i)| i.trigger.identity.record_id == receipt.capture_id)
        .unwrap()
        .0
        .clone()
}
pub(crate) fn settings() -> ChatSettings {
    ChatSettings {
        base_url: "http://127.0.0.1:1234/v1".into(),
        model: "fixture-model".into(),
        api_key_env: "TESSERA_PROPOSAL_FIXTURE_KEY".into(),
    }
}
pub(crate) fn output() -> String {
    json!({"title":"Prepare kit","criteria":["Kit ready"],"rationale":"Have tools available","open_questions":[],"citation_ids":[]}).to_string()
}
pub(crate) fn detail(r: &Runner, id: &str) -> api::Detail {
    r.proposal_get(api::Lookup {
        proposal_id: id.into(),
        goal_id: None,
    })
    .unwrap()
}
pub(crate) fn disposition(r: &mut Runner, id: &str, value: api::Disposition) {
    r.proposal_disposition(
        api::Request {
            operation_id: Uuid::new_v4().to_string(),
            proposal_id: id.into(),
            goal_id: None,
            expected_revision: detail(r, id).source.revision,
            disposition: value,
            source: identity(),
        },
        "operator",
    )
    .unwrap();
}

#[test]
fn exact_readable_input_completes_and_restart_never_regenerates() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r, "Visit мастерская tomorrow");
    let job = r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    assert_eq!(job.id, id);
    let body: Value = serde_json::from_str(&job.body).unwrap();
    assert!(body["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("Visit мастерская tomorrow"));
    let before = detail(&r, &id);
    assert_eq!(
        before.record.attempt.input.as_ref().unwrap().input_sha256,
        format!("{:x}", Sha256::digest(job.body.as_bytes()))
    );
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    r.finish_proposal_generation(&id, None, Some(&settings()), Ok(output()))
        .unwrap();
    let completed = detail(&r, &id);
    assert_eq!(completed.record.attempt.state, AttemptState::Draft);
    drop(r);
    let mut r = f.open();
    assert_eq!(detail(&r, &id).source, completed.source);
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
}

#[test]
fn snooze_retains_history_after_result_and_reject_wins() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r, "First");
    r.prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    let until = "2099-01-01T00:00:00Z".to_owned();
    disposition(
        &mut r,
        &id,
        api::Disposition::Snoozed {
            until: until.clone(),
        },
    );
    assert!(r
        .proposal_generation_current(&id, None, Some(&settings()))
        .unwrap());
    r.finish_proposal_generation(&id, None, Some(&settings()), Ok(output()))
        .unwrap();
    let d = detail(&r, &id);
    assert_eq!(d.record.disposition, api::Disposition::Snoozed { until });
    assert_eq!(d.record.history.len(), 1);
    assert_eq!(d.record.attempt.state, AttemptState::Draft);
    let id = capture(&mut r, "Second");
    r.prepare_proposal_generation(Some(settings()))
        .unwrap()
        .unwrap();
    disposition(&mut r, &id, api::Disposition::Rejected);
    r.finish_proposal_generation(&id, None, Some(&settings()), Ok(output()))
        .unwrap();
    assert!(detail(&r, &id).record.generated.is_none());
    assert_eq!(
        detail(&r, &id).record.disposition,
        api::Disposition::Rejected
    );
}

#[test]
fn changed_and_oversized_input_are_retained_without_starving_other_inputs() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let changed = capture(&mut r, "Original");
    let path = r.drafts().unwrap().intents().unwrap()[&changed]
        .trigger
        .source_path
        .clone();
    fs::write(r.root.join(path), "Changed").unwrap();
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    let oversized = capture(&mut r, &"x".repeat(65_000));
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    let good = capture(&mut r, "Valid later input");
    assert_eq!(
        r.prepare_proposal_generation(Some(settings()))
            .unwrap()
            .unwrap()
            .id,
        good
    );
    let page = r
        .proposal_list(api::ListRequest {
            goal_id: None,
            limit: 100,
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.generation.len(), 2);
    assert!(page.generation.iter().all(|i| i.issue.is_some()));
    assert_eq!(
        r.drafts().unwrap().intents().unwrap()[&oversized]
            .attempt
            .state,
        AttemptState::Failed
    );
    drop(r);
    let r = f.open();
    assert_eq!(
        detail(&r, &good).record.attempt.state,
        AttemptState::Interrupted
    );
    assert_eq!(
        r.drafts().unwrap().intents().unwrap()[&changed].generation_issue,
        Some(api::GenerationIssue::SourceChanged)
    );
}

#[test]
fn unavailable_config_and_provider_failure_are_visible_without_resend() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r, "Missing settings");
    assert!(r.prepare_proposal_generation(None).unwrap().is_none());
    assert_eq!(
        detail(&r, &id).record.failure,
        Some(api::Failure::ProviderUnavailable)
    );
    let id = capture(&mut r, "Provider failure");
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
    assert_eq!(
        detail(&r, &id).record.failure,
        Some(api::Failure::ProviderFailed)
    );
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
}

#[test]
fn restart_and_projection_conflict_retain_uncertain_attempt_and_allow_another() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let mut ids = [
        capture(&mut r, "First input"),
        capture(&mut r, "Second input"),
    ];
    ids.sort();
    let [id, next] = ids;
    fs::write(r.root.join(r.path("proposal", &id)), "Manual version").unwrap();
    assert!(r.prepare_proposal_generation(Some(settings())).is_err());
    r.recover_generation_without_worker().unwrap();
    assert_eq!(
        r.drafts().unwrap().intents().unwrap()[&id].attempt.state,
        AttemptState::Interrupted
    );
    assert_eq!(
        r.prepare_proposal_generation(Some(settings()))
            .unwrap()
            .unwrap()
            .id,
        next
    );
    drop(r);
    let mut r = f.open();
    assert!(r
        .prepare_proposal_generation(Some(settings()))
        .unwrap()
        .is_none());
    assert_eq!(
        detail(&r, &next).record.attempt.state,
        AttemptState::Interrupted
    );
    assert_eq!(
        fs::read_to_string(r.root.join(r.path("proposal", &id))).unwrap(),
        "Manual version"
    );
}

#[test]
fn queued_and_failed_inputs_share_bounded_owner_cursor() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let a = capture(&mut r, "A");
    let b = capture(&mut r, "B");
    let c = capture(&mut r, "C");
    let mut expected = vec![a, b, c];
    expected.sort();
    let mut cursor = None;
    let mut found = vec![];
    loop {
        let page = r
            .proposal_list(api::ListRequest {
                goal_id: None,
                limit: 1,
                cursor,
            })
            .unwrap();
        assert!(page.items.is_empty());
        assert_eq!(page.generation.len(), 1);
        found.push(page.generation[0].proposal_id.clone());
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(found, expected);
}

#[test]
fn generation_fence_blocks_prior_source_handles_and_missing_fence_on_reopen() {
    let f = Fixture::new();
    let mut r = f.open();
    r.enroll_proposal_feed(1).unwrap();
    r.enroll_proposal_drafts().unwrap();
    let source_dir = r.state_dir.join("source");
    let old = SourceStore::open_with_proposal_inbox_adoption(
        &f.config.brain_id,
        &r.root,
        &source_dir,
        WriteBoundary::Managed,
    )
    .unwrap();
    let mut write = SourceWrite {
        schema: SCHEMA.into(),
        operation_id: Uuid::new_v4().to_string(),
        brain_id: f.config.brain_id.clone(),
        path: "positive.md".into(),
        expected_revision: None,
        content_base64: STANDARD.encode("positive"),
    };
    old.write(write.clone()).unwrap();
    r.enroll_proposal_generation().unwrap();
    write.operation_id = Uuid::new_v4().to_string();
    write.path = "forbidden.md".into();
    assert!(old.write(write).is_err());
    assert!(!r.root.join("forbidden.md").exists());
    assert!(SourceStore::open_with_proposal_inbox_adoption(
        &f.config.brain_id,
        &r.root,
        &source_dir,
        WriteBoundary::Managed
    )
    .is_err());
    let id = capture(&mut r, "Changed before generation");
    let path = r.drafts().unwrap().intents().unwrap()[&id]
        .trigger
        .source_path
        .clone();
    fs::write(r.root.join(path), "changed").unwrap();
    r.prepare_proposal_generation(Some(settings())).unwrap();
    let binding_path = source_dir.join("binding.json");
    drop(old);
    drop(r);
    let mut binding: Value = serde_json::from_slice(&fs::read(&binding_path).unwrap()).unwrap();
    binding
        .as_object_mut()
        .unwrap()
        .remove("required_proposal_generation");
    fs::write(&binding_path, serde_json::to_vec(&binding).unwrap()).unwrap();
    assert!(Runner::open(f.config).is_err());
}

#[test]
fn invalid_saved_model_fails_once_without_starving_valid_input() {
    let f = Fixture::new();
    let mut r = f.enrolled();
    let id = capture(&mut r, "Invalid model");
    let mut invalid = settings();
    invalid.model = String::new();
    assert!(r
        .prepare_proposal_generation(Some(invalid))
        .unwrap()
        .is_none());
    assert_eq!(
        r.drafts().unwrap().intents().unwrap()[&id].attempt.state,
        AttemptState::Failed
    );
    let next = capture(&mut r, "Valid next");
    assert_eq!(
        r.prepare_proposal_generation(Some(settings()))
            .unwrap()
            .unwrap()
            .id,
        next
    );
}

#[test]
fn decision_reuse_new_proposal_body_excludes_manual_text_including_status() {
    use super::super::discussion_decision::tests::{get, save, turn};
    use super::super::goal_brief::tests::{fixture, save_reply};
    use tessera_core::decision_reuse::{self as reuse, Disposition};
    let (_dir, mut r, _, g, _) = fixture();
    let key = turn(&mut r, &g, "Unique automatic policy constraint 261", None);
    let view = get(&mut r, &key);
    save(&mut r, &key, &view).unwrap();
    let reply = save_reply(&mut r, &g, "Independent proposal trigger");
    let path = reply.path.unwrap();
    let source = r.read_source(&path).unwrap();
    let trigger = crate::proposals::CommittedTrigger {
        identity: crate::proposals::Identity {
            brain_id: r.state.brain_id.clone(),
            kind: crate::proposals::TriggerKind::Decision,
            record_id: reply.decision_id.unwrap(),
            source_revision: source.revision,
            policy_version: 1,
        },
        goal_id: Some(g.clone()),
        source_path: path,
        received_at: "2026-09-08T16:00:00Z".into(),
    };
    let (_, before) = r.generation_input(&trigger, Some(settings())).unwrap();
    assert!(before
        .generation
        .as_ref()
        .unwrap()
        .request_body
        .contains("Unique automatic policy constraint 261"));
    let base = r.read_source(view["path"].as_str().unwrap()).unwrap();
    let id = view["decision_id"].as_str().unwrap();
    let p = Disposition::new(
        Uuid::new_v4().to_string(),
        key.expected_actor_id.clone(),
        "2026-09-08T16:00:00Z".into(),
        base.revision.clone(),
    );
    let proposed = reuse::transform(&base, &g, id, &p).unwrap();
    let request = SourceWrite {
        schema: SCHEMA.into(),
        brain_id: base.brain_id.clone(),
        path: base.path.clone(),
        expected_revision: Some(base.revision.clone()),
        operation_id: p.operation_id,
        content_base64: STANDARD.encode(proposed),
    };
    r.with_goal(&g, |r| {
        r.discussion_decision_reuse_write(&g, id, request, base, &key.expected_actor_id)
    })
    .unwrap();
    let (_, after) = r.generation_input(&trigger, Some(settings())).unwrap();
    let body = &after.generation.as_ref().unwrap().request_body;
    assert!(!body.contains("Unique automatic policy constraint 261"));
    assert!(body.contains("manual_only"));
    assert!(body.contains("Independent proposal trigger"));
}
