use super::*;
struct Fixture {
    dir: tempfile::TempDir,
    brain_id: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("brain/records")).unwrap();
        fs::create_dir(dir.path().join("state")).unwrap();
        Self {
            dir,
            brain_id: Uuid::new_v4().to_string(),
        }
    }
    fn open(&self) -> Runner {
        let mut runner = Runner::open(RunnerConfig {
            brain_id: self.brain_id.clone(),
            root: self.dir.path().join("brain"),
            operational_dir: self.dir.path().join("state"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        })
        .unwrap();
        runner.inbox_planning_enabled = true;
        runner
    }
}
fn source() -> inbox_api::SourceIdentity {
    let id = Uuid::new_v4().to_string();
    inbox_api::SourceIdentity {
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
fn setup(r: &mut Runner) -> api::Request {
    let capture = r
        .inbox_capture(
            inbox_api::CaptureRequest {
                operation_id: Uuid::new_v4().to_string(),
                text: "Exact original thought\r\n  without newline".into(),
                source: source(),
            },
            "operator",
        )
        .unwrap();
    api::Request {
        operation_id: Uuid::new_v4().to_string(),
        capture_id: capture.capture_id,
        expected_capture_revision: capture.revision,
        title: "Planned goal".into(),
        criteria: vec![Criterion {
            id: "C1".into(),
            description: "Observable result".into(),
            requires_human: true,
        }],
        source: source(),
    }
}
fn code(error: &anyhow::Error) -> &str {
    error.downcast_ref::<inbox_api::InboxError>().unwrap().code
}
#[test]
fn plan_retains_exact_source_once_and_replays_after_restart_and_source_edit() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = setup(&mut r);
    let source = r.inbox_get(&req.capture_id).unwrap().source;
    let first = r.inbox_plan(req.clone(), "operator").unwrap();
    assert_eq!(first.receipt.status, "committed");
    assert_eq!(first.origin.source_snapshot, source);
    assert_eq!(r.inbox_get(&req.capture_id).unwrap().source, source);
    assert!(r.snapshot().unwrap().stage.is_none());
    let inbox = r.inbox_get(&req.capture_id).unwrap();
    assert_eq!(inbox.planned_goals.len(), 1);
    assert_eq!(inbox.planned_goals[0].goal_id, first.goal_id);
    fs::write(
        r.root.join(&source.path),
        STANDARD
            .decode(&source.content_base64)
            .unwrap()
            .into_iter()
            .chain(b"changed".iter().copied())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    drop(r);
    let mut r = f.open();
    let replay = r.inbox_plan(req.clone(), "operator").unwrap();
    assert!(replay.receipt.replayed);
    assert_eq!(replay.goal_id, first.goal_id);
    assert_eq!(r.goal_ids().len(), 1);
    let mut changed = req.clone();
    changed.title = "different".into();
    assert_eq!(
        code(&r.inbox_plan(changed, "operator").unwrap_err()),
        "inbox_plan_operation_conflict"
    );
    let mut fresh = req;
    fresh.operation_id = Uuid::new_v4().to_string();
    fresh.source = super::tests::source();
    assert_eq!(
        code(&r.inbox_plan(fresh, "operator").unwrap_err()),
        "inbox_plan_source_changed"
    );
    assert_eq!(r.goal_ids().len(), 1);
}
#[test]
fn planning_crash_before_and_after_source_recovers_original_goal() {
    for after_source in [false, true] {
        let f = Fixture::new();
        let mut r = f.open();
        let req = setup(&mut r);
        if after_source {
            r.interrupt_after_write = Some(1);
        } else {
            r.interrupt_after_plan_intent = true;
        }
        assert!(r.inbox_plan(req.clone(), "operator").is_err());
        let retained = r.state.inbox_plan_journal.operations[&req.operation_id]
            .outcome
            .clone();
        assert_eq!(retained.receipt.status, "pending");
        assert_eq!(
            r.root.join(r.path("goal", &retained.goal_id)).exists(),
            after_source
        );
        drop(r);
        let mut r = f.open();
        let done = r.inbox_plan(req, "operator").unwrap();
        assert!(done.receipt.replayed);
        assert_eq!(done.goal_id, retained.goal_id);
        assert_eq!(r.goal_ids().len(), 1);
        assert!(r.state.pending_writes.is_empty());
    }
}
#[test]
fn invalid_plan_is_nonmutating_and_second_explicit_plan_preserves_current_goal() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = setup(&mut r);
    let before = fs::read(r.state_dir.join("state.json")).unwrap();
    let mut invalid = req.clone();
    invalid.criteria.clear();
    assert_eq!(
        code(&r.inbox_plan(invalid, "operator").unwrap_err()),
        "inbox_plan_invalid_request"
    );
    assert_eq!(before, fs::read(r.state_dir.join("state.json")).unwrap());
    let first = r.inbox_plan(req.clone(), "operator").unwrap();
    let mut second = req;
    second.operation_id = Uuid::new_v4().to_string();
    second.source = source();
    let second = r.inbox_plan(second, "operator").unwrap();
    assert_ne!(first.goal_id, second.goal_id);
    assert_eq!(r.snapshot().unwrap().goal.unwrap().id, first.goal_id);
    assert_eq!(r.goal_ids().len(), 2);
}
#[test]
fn enrollment_and_canonical_origin_detect_lost_plan_inventory() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = setup(&mut r);
    let planned = r.inbox_plan(req.clone(), "operator").unwrap();
    let path = r.state_dir.join("state.json");
    drop(r);
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("inbox_plan_journal");
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let mut r = f.open();
    assert!(!r.inbox_plan_writable());
    assert_eq!(
        r.inbox_get(&req.capture_id).unwrap().planned_goals[0].goal_id,
        planned.goal_id
    );
    assert_eq!(
        code(&r.inbox_plan(req, "operator").unwrap_err()),
        "inbox_plan_recovery_required"
    );
}
#[test]
fn reviewed_packet_includes_origin_visibly_and_origin_changes_invalidate_freshness() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = setup(&mut r);
    r.inbox_plan(req, "operator").unwrap();
    let goal = r.snapshot().unwrap().goal.unwrap();
    let scope = crate::retrieval::SearchScope {
        goal_id: goal.id.clone(),
        mode: "goal".into(),
        ..Default::default()
    };
    let packet =
        crate::context::create(&r, goal.clone(), "Next step".into(), scope, vec![], vec![])
            .unwrap();
    assert!(packet
        .text
        .contains("Original thought — operator input, unverified"));
    assert!(packet
        .text
        .contains("Exact original thought\r\n  without newline"));
    assert!(packet.citations.is_empty());
    let legacy = Goal {
        extra: BTreeMap::new(),
        ..goal.clone()
    };
    let old = crate::retrieval::sha(
        &serde_json::to_vec(&serde_json::json!({"title":legacy.title,"criteria":legacy.criteria}))
            .unwrap(),
    );
    assert_eq!(crate::context::goal_definition(&legacy).unwrap(), old);
    assert_ne!(crate::context::goal_definition(&goal).unwrap(), old);
    let mut changed = goal;
    changed.extra.get_mut("origin_inbox").unwrap()["planned_at"] = serde_json::json!("different");
    assert_ne!(
        crate::context::goal_definition(&changed).unwrap(),
        packet.goal_definition_sha256
    );
}
#[test]
fn actual_chat_request_has_exact_origin_once_without_provider_call() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = setup(&mut r);
    let outcome = r.inbox_plan(req, "operator").unwrap();
    let mut app = crate::application::Application::unconfigured();
    app.chat = Some(crate::chat::ChatConfig {
        base_url: "http://127.0.0.1:1/v1".into(),
        model: "fixture".into(),
        api_key: "synthetic".into(),
        idle_timeout: std::time::Duration::from_secs(1),
    });
    let (_, _, request) = app
        .chat_start(
            &mut r,
            outcome.goal_id,
            "Discuss this thought".into(),
            vec![outcome.origin.path],
            None,
        )
        .unwrap();
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|m| m
                .content
                .contains("Exact original thought\r\n  without newline"))
            .count(),
        1
    );
    assert!(request.context.sources.is_empty());
    assert!(r.snapshot().unwrap().stage.is_none());
}
#[test]
fn origin_size_counts_against_visible_packet_budget() {
    let f = Fixture::new();
    let mut r = f.open();
    let mut req = setup(&mut r);
    let source = r.inbox_get(&req.capture_id).unwrap().source;
    let text = String::from_utf8(STANDARD.decode(source.content_base64).unwrap()).unwrap();
    let (record, _) = inbox_api::parse(&text).unwrap();
    let oversized = format!(
        "---\n{}---\n{}",
        serde_yaml::to_string(&record).unwrap(),
        "x".repeat(65536)
    );
    fs::write(r.root.join(source.path), oversized).unwrap();
    req.expected_capture_revision = r.inbox_get(&req.capture_id).unwrap().item.revision;
    r.inbox_plan(req, "operator").unwrap();
    let goal = r.snapshot().unwrap().goal.unwrap();
    let scope = crate::retrieval::SearchScope {
        goal_id: goal.id.clone(),
        mode: "goal".into(),
        ..Default::default()
    };
    assert!(
        crate::context::create(&r, goal, "Next".into(), scope, vec![], vec![])
            .unwrap_err()
            .to_string()
            .contains("64 KiB")
    );
}
#[test]
fn maintenance_gate_keeps_exact_replay_and_origin_context_but_blocks_new_plans() {
    let f = Fixture::new();
    let mut r = f.open();
    let req = setup(&mut r);
    let original = r.inbox_plan(req.clone(), "operator").unwrap();
    r.inbox_planning_enabled = false;
    assert!(!r.inbox_plan_writable());
    let replay = r.inbox_plan(req.clone(), "operator").unwrap();
    assert!(replay.receipt.replayed);
    assert_eq!(replay.goal_id, original.goal_id);
    let goal = r.snapshot().unwrap().goal.unwrap();
    assert!(api::operator_input(&goal)
        .unwrap()
        .unwrap()
        .contains("Exact original thought"));
    let mut different = req;
    different.operation_id = Uuid::new_v4().to_string();
    different.source = source();
    assert_eq!(
        code(&r.inbox_plan(different, "operator").unwrap_err()),
        "inbox_plan_recovery_required"
    );
}
#[test]
fn legacy_stage_context_contains_pinned_operator_input_without_starting_engine() {
    let f = Fixture::new();
    let mut r = f.open();
    let request = setup(&mut r);
    let outcome = r.inbox_plan(request, "operator").unwrap();
    let token = f.dir.path().join("fixture-token");
    fs::write(&token, "synthetic").unwrap();
    let settings = crate::application::ApplicationConfig {
        maestro: None,
        actor: "operator".into(),
        chat: None,
        todoist: None,
        t3: Some(crate::application::T3Settings {
            base_url: "http://127.0.0.1:1".into(),
            token_env: format!("file:{}", token.display()),
            environment_id: "fixture".into(),
            project_id: "fixture".into(),
            model_instance_id: "fixture".into(),
            model: "fixture".into(),
            runtime_mode: "approval-required".into(),
            interaction_mode: "default".into(),
        }),
    };
    let (app, _) =
        crate::application::Application::configure(settings, &r.state_dir.clone(), &mut r).unwrap();
    app.stage_prepare_with_previous(
        &mut r,
        outcome.goal_id,
        None,
        vec![outcome.origin.path],
        vec!["C1".into()],
        "Explicit next step".into(),
        None,
    )
    .unwrap();
    let snapshot = r.snapshot().unwrap();
    assert_eq!(snapshot.phase.as_deref(), Some("prepared"));
    let packet = snapshot.dispatch.unwrap().packet;
    assert_eq!(packet.decisions.len(), 1);
    assert!(packet.decisions[0].contains("Exact original thought\r\n  without newline"));
    assert!(packet.sources.is_empty());
    assert!(snapshot.binding.is_none());
}
#[test]
fn exact_export_keeps_original_inbox_and_canonical_origin_without_operational_journal() {
    use std::io::Read;
    let f = Fixture::new();
    let mut r = f.open();
    let request = setup(&mut r);
    let outcome = r.inbox_plan(request, "operator").unwrap();
    let goal_path = r.path("goal", &outcome.goal_id);
    let goal_bytes = fs::read(r.root.join(&goal_path)).unwrap();
    let inbox_bytes = fs::read(r.root.join(&outcome.origin.path)).unwrap();
    let destination = f.dir.path().join("exact.tar");
    let receipt = r.export_exact(&destination).unwrap();
    assert!(!receipt.manifest.execution_restored);
    let mut files = BTreeMap::new();
    for entry in tar::Archive::new(File::open(destination).unwrap())
        .entries()
        .unwrap()
    {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().to_string();
        let mut bytes = vec![];
        entry.read_to_end(&mut bytes).unwrap();
        files.insert(path, bytes);
    }
    assert_eq!(files[&format!("brain/{goal_path}")], goal_bytes);
    assert_eq!(
        files[&format!("brain/{}", outcome.origin.path)],
        inbox_bytes
    );
    assert!(!files.keys().any(|p| p.ends_with("state.json")));
}
