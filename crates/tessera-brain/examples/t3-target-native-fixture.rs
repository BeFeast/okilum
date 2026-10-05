//! Synthetic native acceptance fixture using the production Runner and service.
//! Creates a fresh isolated directory only; never accepts an existing brain.
use anyhow::{ensure, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, net::TcpListener, path::PathBuf};
use tessera_brain::{
    application::{Application, ApplicationConfig, T3Settings},
    *,
};
use tessera_core::source::WriteBoundary;

const WHEN: &str = "2026-10-03T00:00:00Z";
fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}
struct Seed;
impl Adapter for Seed {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: "t3".into(),
            cancel: false,
        }
    }
    fn start(&mut self, e: &StartEnvelope) -> Result<StartReply> {
        Ok(StartReply::Accepted {
            binding: EngineRef {
                engine: "t3".into(),
                instance_id: "synthetic-old-environment".into(),
                thread_id: Some(t3::T3Adapter::thread_id(e)?),
                turn_id: Some("synthetic-old-turn".into()),
                task_id: None,
            },
        })
    }
    fn observe(&mut self, _: &EngineRef, _: &BTreeMap<String, String>) -> Result<Vec<EngineEvent>> {
        anyhow::bail!("seed adapter has no transport")
    }
    fn reconcile(&mut self, _: &StartEnvelope, _: Option<&EngineRef>) -> Result<ReconcileReply> {
        anyhow::bail!("seed adapter has no transport")
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2,
        "usage: t3-target-native-fixture FRESH_ABSOLUTE_DIRECTORY FAKE_T3_BASE_URL"
    );
    let root = PathBuf::from(&args[0]);
    ensure!(
        root.is_absolute() && !root.exists(),
        "fixture directory must be fresh and absolute"
    );
    let endpoint = &args[1];
    let addr: std::net::SocketAddr = endpoint
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("literal loopback HTTP endpoint required"))?
        .parse()?;
    ensure!(
        addr.ip().is_loopback(),
        "only loopback synthetic provider allowed"
    );
    fs::create_dir(&root)?;
    fs::create_dir_all(root.join("brain/records"))?;
    fs::create_dir(root.join("runtime"))?;
    fs::create_dir_all(root.join("config/tessera"))?;
    fs::write(root.join("token"), "synthetic-only-319")?;
    let config = || RunnerConfig {
        brain_id: id(1),
        root: root.join("brain"),
        operational_dir: root.join("runtime"),
        records_dir: "records".into(),
        boundary: WriteBoundary::Managed,
    };
    let mut runner = Runner::open(config())?;
    let settings = T3Settings {
        base_url: endpoint.clone(),
        token_env: format!("file:{}", root.join("token").display()),
        environment_id: "synthetic-old-environment".into(),
        project_id: "synthetic-project".into(),
        model_instance_id: "synthetic-provider".into(),
        model: "synthetic-model".into(),
        runtime_mode: "approval-required".into(),
        interaction_mode: "default".into(),
    };
    let app_config = ApplicationConfig {
        actor: "Synthetic acceptance".into(),
        chat: None,
        todoist: None,
        maestro: None,
        t3: Some(settings.clone()),
    };
    let (app, adapters) = Application::configure(app_config, &root.join("runtime"), &mut runner)?;
    runner.create_goal(
        Goal {
            id: id(2),
            title: "Preserved synthetic history".into(),
            status: "draft".into(),
            criteria: vec![Criterion {
                id: "C1".into(),
                description: "Review preserved result".into(),
                requires_human: false,
            }],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        },
        "# Preserved synthetic history".into(),
    )?;
    let revision = runner.goal_source()?.revision;
    runner.prepare_stage(
        Stage {
            id: id(10),
            goal_id: id(2),
            engine: "t3".into(),
            status: "ready".into(),
            criterion_ids: vec!["C1".into()],
            context_id: id(11),
            result_ids: vec![],
            extra: BTreeMap::new(),
        },
        ContextPacket {
            id: id(11),
            goal_id: id(2),
            stage_id: id(10),
            goal_revision: revision,
            goal: "Preserved synthetic history".into(),
            decisions: vec![],
            constraints: vec![],
            sources: vec![],
            previous_result_id: None,
            next_step: "Review".into(),
            extra: BTreeMap::new(),
        },
        id(12),
        BTreeMap::from([
            ("environment_id".into(), json!(settings.environment_id)),
            ("project_id".into(), json!(settings.project_id)),
            ("created_at".into(), json!(WHEN)),
        ]),
    )?;
    let envelope = runner.snapshot()?.dispatch.unwrap();
    let binding = runner.start(&mut Seed)?.binding.unwrap();
    let event = EngineEvent {
        operation_id: envelope.operation_id.clone(),
        engine_ref: binding,
        event_id: "synthetic-terminal".into(),
        stream_id: "synthetic-stream".into(),
        sequence: Some(1),
        cursor: Some("synthetic-cursor-1".into()),
        observed_at: WHEN.into(),
        payload: EventPayload::Outcome(Outcome {
            outcome: "succeeded".into(),
            summary: "Synthetic settled history, pending human review".into(),
            sources: vec![],
            evidence: vec![Evidence {
                id: "e1".into(),
                kind: "engine_response".into(),
                source: SourceRef {
                    uri: "fixture:synthetic-evidence".into(),
                    revision: None,
                    locator: None,
                },
                description: "Synthetic preserved evidence".into(),
                observed_at: WHEN.into(),
                status: "unverified".into(),
            }],
            verification: "unverified".into(),
            criterion_evaluations: vec![],
        }),
    };
    fs::write(
        root.join("runtime/t3-receipts")
            .join(format!("{}.json", envelope.operation_id)),
        serde_json::to_vec(
            &json!({"fingerprint":format!("{:x}",Sha256::digest(serde_json::to_vec(&envelope)?)),"event":event}),
        )?,
    )?;
    runner.ingest(event)?;
    // Model a legacy missing origin only within this new synthetic directory.
    // No invented URL is assigned to its old envelope, binding, or receipt.
    drop(runner);
    let state_path = root.join("runtime/state.json");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    state["application"]["provider_identity"] = serde_json::Value::Null;
    fs::write(&state_path, serde_json::to_vec_pretty(&state)?)?;
    let runner = Runner::open(config())?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let profile = json!({"schema":"tessera-workspace/v1","label":"#319 isolated production acceptance","endpoint":listener.local_addr()?.to_string(),"identity":runner.workspace_identity()});
    fs::write(
        root.join("config/tessera/workspace.json"),
        serde_json::to_vec_pretty(&profile)?,
    )?;
    fs::write(
        root.join("fixture.json"),
        serde_json::to_vec_pretty(
            &json!({"schema":"tessera-native-fixture/v1","workspace":profile,"candidate":{"base_url":endpoint,"token_env":settings.token_env,"environment_id":"synthetic-new-environment","project_id":"synthetic-project","model_instance_id":"synthetic-provider","model":"synthetic-model","runtime_mode":"approval-required","interaction_mode":"default"},"production_runner":true,"production_service":true,"synthetic_provider":true,"seed_operation":envelope.operation_id}),
        )?,
    )?;
    println!("{}", root.join("fixture.json").display());
    service::serve_application(listener, runner, adapters, app)
}
