//! Test-only enrollment and source setup. All three acceptance triggers are
//! committed later through the actual service; this exporter generates no draft.
use super::*;
use crate::application::Application;
use crate::inbox::{CaptureRequest, SourceIdentity};

fn identity() -> SourceIdentity {
    let event = Uuid::new_v4().to_string();
    SourceIdentity {
        channel: "native".into(),
        instance_id: Uuid::new_v4().to_string(),
        account_id: "local".into(),
        actor_id: "local operator".into(),
        chat_id: None,
        topic_id: None,
        message_id: event.clone(),
        update_id: event,
        uri: None,
    }
}

fn goal(runner: &mut Runner, marker: &str) -> String {
    let id = Uuid::new_v4().to_string();
    runner
        .create_goal(
            Goal {
                id: id.clone(),
                title: format!("Prepare workshop visit {marker}"),
                status: "draft".into(),
                criteria: vec![
                    Criterion {
                        id: "C1".into(),
                        description: format!("Operator confirms the {marker} kit contents"),
                        requires_human: true,
                    },
                    Criterion {
                        id: "C2".into(),
                        description: format!("Operator confirms the {marker} appointment"),
                        requires_human: true,
                    },
                ],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            format!("# Workshop visit {marker}\n\nKeep ownership with {marker}. Scheduling remains unconfirmed.\n"),
        )
        .unwrap();
    id
}

fn manual_context(runner: &mut Runner, goal: &str, marker: &str) -> Value {
    let path = format!("evidence-{marker}.md");
    let text =
        format!("# Workshop evidence {marker}\n\nOriginal unverified inventory for {marker}.\n");
    fs::write(runner.root.join(&path), &text).unwrap();
    runner
        .with_goal(goal, |runner| {
            let source = runner.read_source(&path)?;
            let citation = crate::retrieval::Citation {
                citation_id: crate::retrieval::citation_id(&path, &source.revision, 1, 3),
                path: path.clone(),
                revision: source.revision,
                start_line: 1,
                end_line: 3,
                locator: "L1-L3".into(),
                excerpt: text.clone(),
                metadata: Default::default(),
            };
            let app = Application::unconfigured();
            let prepared = app.context_prepare(
                runner,
                goal.into(),
                format!("Manual context for {marker}"),
                crate::retrieval::SearchScope {
                    goal_id: goal.into(),
                    mode: "project".into(),
                    ..Default::default()
                },
                vec![citation.clone()],
                vec![citation.citation_id],
            )?;
            let packet: crate::context::ReviewedPacket =
                serde_json::from_value(prepared["packet"].clone())?;
            app.context_revise(
                runner,
                goal.into(),
                packet.id.clone(),
                packet.revision,
                format!("Manual {marker} guidance — 雪. Preserve this exact text and pinned source."),
            )?;
            let retained = crate::context::read(runner, goal, &packet.id)?;
            Ok(json!({"id":retained.id,"revision":retained.revision,"text":retained.text,"pinned_citation_ids":retained.pinned_citation_ids,"path":runner.path("reviewed-context", &packet.id)}))
        })
        .unwrap()
}

#[test]
#[ignore = "requires empty OKILUM_GENERATION_FIXTURE directory"]
fn export_proposal_generation_service_fixture() {
    let target = PathBuf::from(std::env::var_os("OKILUM_GENERATION_FIXTURE").unwrap());
    assert!(target.is_dir() && fs::read_dir(&target).unwrap().next().is_none());
    fs::create_dir_all(target.join("brain/records")).unwrap();
    fs::create_dir(target.join("state")).unwrap();
    let brain_id = Uuid::new_v4().to_string();
    let mut runner = Runner::open(RunnerConfig {
        brain_id: brain_id.clone(),
        root: target.join("brain"),
        records_dir: "records".into(),
        operational_dir: target.join("state"),
        boundary: WriteBoundary::Managed,
    })
    .unwrap();
    let goal_a = goal(&mut runner, "AMBER");
    let goal_b = goal(&mut runner, "COBALT");
    let historical_source = identity();
    let historical_capture = runner
        .inbox_capture(
            CaptureRequest {
                operation_id: Uuid::new_v4().to_string(),
                text: "Historical capture before generation enrollment: no backfill".into(),
                source: historical_source,
            },
            "local operator",
        )
        .unwrap();
    runner
        .with_goal(&goal_a, |runner| {
            runner.attention("decision", "Choose the AMBER workshop appointment");
            runner.persist()
        })
        .unwrap();
    let stage_id = Uuid::new_v4().to_string();
    let context_id = Uuid::new_v4().to_string();
    let operation_id = Uuid::new_v4().to_string();
    let goal_revision = runner
        .source
        .read(&runner.path("goal", &goal_b))
        .unwrap()
        .revision;
    runner
        .prepare_stage(
            Stage {
                id: stage_id.clone(),
                goal_id: goal_b.clone(),
                engine: "t3".into(),
                status: "ready".into(),
                criterion_ids: vec!["C1".into()],
                context_id: context_id.clone(),
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            ContextPacket {
                id: context_id,
                goal_id: goal_b.clone(),
                stage_id,
                goal_revision,
                goal: "Prepare workshop visit COBALT".into(),
                decisions: vec![],
                constraints: vec!["Human confirmation remains mandatory".into()],
                sources: vec![],
                previous_result_id: None,
                next_step: "Fixture setup only; await a sourced synthetic engine result".into(),
                extra: BTreeMap::new(),
            },
            operation_id.clone(),
            BTreeMap::new(),
        )
        .unwrap();
    let binding = EngineRef {
        engine: "t3".into(),
        instance_id: "generation191-fixture".into(),
        thread_id: Some("generation191-synthetic-thread".into()),
        turn_id: Some("generation191-synthetic-turn".into()),
        task_id: None,
    };
    runner
        .with_goal(&goal_b, |runner| {
            runner.apply_start(StartReply::Accepted {
                binding: binding.clone(),
            })?;
            runner.persist()
        })
        .unwrap();
    let context_a = manual_context(&mut runner, &goal_a, "AMBER");
    let context_b = manual_context(&mut runner, &goal_b, "COBALT");
    let evidence = runner.read_source("evidence-COBALT.md").unwrap();
    let event = EngineEvent {
        operation_id,
        engine_ref: binding,
        event_id: "generation191-sourced-result".into(),
        stream_id: "generation191-synthetic-thread/generation191-synthetic-turn".into(),
        sequence: Some(1),
        cursor: Some("1".into()),
        observed_at: crate::inbox::now().unwrap(),
        payload: EventPayload::Outcome(Outcome {
            outcome: "succeeded".into(),
            summary: "COBALT kit list prepared; appointment and human check remain unconfirmed"
                .into(),
            sources: vec![SourceRef {
                uri: "evidence-COBALT.md".into(),
                revision: Some(evidence.revision),
                locator: Some("L1-L3".into()),
            }],
            evidence: vec![],
            verification: "unverified".into(),
            criterion_evaluations: vec![],
        }),
    };
    runner.enroll_proposal_feed(1).unwrap();
    runner.enroll_proposal_drafts().unwrap();
    runner.enroll_proposal_generation().unwrap();
    assert!(runner.drafts().unwrap().intents().unwrap().is_empty());
    fs::write(
        target.join("fixture.json"),
        serde_json::to_vec_pretty(&json!({
            "brain_id":brain_id,"goal_a":goal_a,"goal_b":goal_b,
            "context_a":context_a,"context_b":context_b,
            "historical_capture":historical_capture,"engine_event":event,
            "setup_scope":"Two goals, manual contexts, decision Attention and synthetic accepted engine binding precede enrollment. No result ingestion, new capture, Attention reply, provider call or generated proposal occurs in this exporter."
        })).unwrap(),
    ).unwrap();
}
