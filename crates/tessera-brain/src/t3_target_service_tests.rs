use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Barrier,
};
fn fixture() -> (
    tempfile::TempDir,
    Arc<Mutex<Backend>>,
    crate::t3_routes::AdoptRequest,
) {
    let (temp, runner, config, app) = crate::runtime::t3_routes::tests::fixture();
    let request = crate::runtime::t3_routes::tests::request(&runner, &config);
    let jobs = crate::context_jobs::Jobs::open(runner.operational_root()).unwrap();
    let backend = Arc::new(Mutex::new(Backend {
        runner,
        app,
        adapters: Adapters::new(),
        exports: Default::default(),
        index: None,
        index_error: None,
        context_jobs: jobs,
        todoist_picker: Default::default(),
    }));
    (temp, backend, request)
}
fn discovery(candidate: &crate::application::T3Settings) -> Result<Value> {
    Ok(json!({"environment_id":candidate.environment_id,"projects":[{"id":candidate.project_id}]}))
}
struct CountStart(Arc<AtomicUsize>);
impl Adapter for CountStart {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: "t3".into(),
            cancel: false,
        }
    }
    fn start(&mut self, _: &StartEnvelope) -> Result<StartReply> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(StartReply::Indeterminate {
            reason: "positive control".into(),
        })
    }
    fn observe(&mut self, _: &EngineRef, _: &BTreeMap<String, String>) -> Result<Vec<EngineEvent>> {
        panic!("unexpected observe")
    }
    fn reconcile(&mut self, _: &StartEnvelope, _: Option<&EngineRef>) -> Result<ReconcileReply> {
        panic!("unexpected reconcile")
    }
}
#[test]
fn concurrent_identical_adoption_returns_same_commit_and_provider_work_requires_separate_start() {
    let (_temp, backend, request) = fixture();
    let barrier = Arc::new(Barrier::new(2));
    let probes = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(AtomicUsize::new(0));
    let adapter_key = format!("t3:{}", crate::t3_routes::digest(&request.review.candidate));
    backend
        .lock()
        .unwrap()
        .adapters
        .insert(adapter_key, Box::new(CountStart(starts.clone())));
    let threads = (0..2)
        .map(|_| {
            let backend = backend.clone();
            let request = request.clone();
            let barrier = barrier.clone();
            let probes = probes.clone();
            std::thread::spawn(move || {
                dispatch_t3_target_with(
                    &backend,
                    &Command::T3TargetAdopt { request },
                    |candidate| {
                        probes.fetch_add(1, Ordering::SeqCst);
                        barrier.wait();
                        discovery(candidate)
                    },
                )
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let outcomes = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0]["status"], "committed");
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        dispatch_t3_target_with(
            &backend,
            &Command::T3TargetAdopt {
                request: request.clone()
            },
            |_| panic!("duplicate must not rediscover")
        )
        .unwrap(),
        outcomes[0]
    );
    let mut owner = backend.lock().unwrap();
    let snap = owner.runner.snapshot().unwrap();
    let goal_id = snap.goal.unwrap().id;
    let previous = snap.stage.unwrap().result_ids.last().cloned();
    owner
        .handle(Command::StagePrepare {
            goal_id: goal_id.clone(),
            conversation_id: None,
            source_paths: vec![],
            criterion_ids: vec!["C1".into()],
            next_step: "Future stage".into(),
            reviewed_packet: None,
            previous_result_id: previous,
        })
        .unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    let operation = owner
        .runner
        .snapshot()
        .unwrap()
        .dispatch
        .unwrap()
        .operation_id;
    let key = format!(
        "t3:{}:{}",
        crate::t3_routes::digest(&request.review.candidate),
        operation
    );
    owner
        .adapters
        .insert(key, Box::new(CountStart(starts.clone())));
    owner
        .handle(Command::Start {
            goal_id: Some(goal_id),
            expected: None,
        })
        .unwrap();
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "positive control proves provider work counter is wired"
    );
}
#[test]
fn prepare_is_read_only_and_stale_selection_is_certified_not_applied_without_probe() {
    let (_temp, backend, request) = fixture();
    let path = backend
        .lock()
        .unwrap()
        .runner
        .operational_root()
        .join("state.json");
    let before = std::fs::read(&path).unwrap();
    let prepared = dispatch_t3_target_with(
        &backend,
        &Command::T3TargetPrepare {
            candidate: request.review.candidate.clone(),
            compatibility_manifest: None,
        },
        discovery,
    )
    .unwrap();
    assert_eq!(prepared, serde_json::to_value(&request.review).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut stale = request.clone();
    stale.review.guard.revision = "stale".into();
    let refused = dispatch_t3_target_with(
        &backend,
        &Command::T3TargetAdopt {
            request: stale.clone(),
        },
        |_| panic!("stale selection must not probe"),
    )
    .unwrap();
    assert_eq!(refused["status"], "not_applied");
    assert_eq!(refused["request"], serde_json::to_value(stale).unwrap());
    assert_eq!(std::fs::read(path).unwrap(), before);
}
#[test]
fn workspace_and_unknown_fields_cannot_bypass_typed_route_boundary() {
    let (_temp, backend, request) = fixture();
    let workspace = backend.lock().unwrap().runner.workspace_identity();
    let good = json!({"schema":"ai-brain/workspace-v1","id":"wire","expected_workspace":workspace,"op":"t3_target_adopt","request":request});
    assert!(serde_json::from_value::<Request>(good.clone()).is_ok());
    let mut invalid = good.clone();
    invalid
        .as_object_mut()
        .unwrap()
        .remove("expected_workspace");
    assert!(serde_json::from_value::<Request>(invalid).is_err());
    let mut invalid = good;
    invalid["request"]["override_guard"] = json!(true);
    assert!(serde_json::from_value::<Request>(invalid).is_err());
}

#[test]
fn cold_poll_uses_historical_receipt_and_unknown_origin_refuses_before_adapter_creation() {
    for unknown in [false, true] {
        let (temp, runner, config, app) = if unknown {
            crate::runtime::t3_routes::tests::unknown_origin_fixture()
        } else {
            crate::runtime::t3_routes::tests::fixture()
        };
        let request = crate::runtime::t3_routes::tests::request(&runner, &config);
        let jobs = crate::context_jobs::Jobs::open(runner.operational_root()).unwrap();
        let backend = Arc::new(Mutex::new(Backend {
            runner,
            app,
            adapters: Adapters::new(),
            exports: Default::default(),
            index: None,
            index_error: None,
            context_jobs: jobs,
            todoist_picker: Default::default(),
        }));
        dispatch_t3_target_with(
            &backend,
            &Command::T3TargetAdopt {
                request: request.clone(),
            },
            discovery,
        )
        .unwrap();
        let mut owner = backend.lock().unwrap();
        let snap = owner.runner.snapshot().unwrap();
        let goal = snap.goal.unwrap().id;
        let op = snap.dispatch.unwrap().operation_id;
        if unknown {
            std::fs::remove_file(temp.path().join("token")).unwrap();
            for missing_receipt in [false, true] {
                if missing_receipt {
                    std::fs::remove_file(
                        temp.path()
                            .join("runtime/t3-receipts")
                            .join(format!("{op}.json")),
                    )
                    .unwrap();
                }
                let err = owner
                    .handle(Command::Poll {
                        goal_id: Some(goal.clone()),
                    })
                    .unwrap_err();
                assert!(
                    err.to_string()
                        .contains("historical_operation_nonreplayable"),
                    "{err:#}"
                );
                assert!(
                    owner.adapters.is_empty(),
                    "unroutable history never creates any adapter or resolves missing credential"
                );
            }
        } else {
            owner
                .handle(Command::Poll {
                    goal_id: Some(goal),
                })
                .unwrap();
            let historical = config.t3.unwrap();
            assert!(owner.adapters.contains_key(&format!(
                "t3:{}:{op}",
                crate::t3_routes::digest(&historical)
            )));
            assert_eq!(owner.adapters.len(), 1);
            assert!(!owner.adapters.contains_key(&format!(
                "t3:{}:{op}",
                crate::t3_routes::digest(&request.review.candidate)
            )));
        }
    }
}

#[test]
fn legacy_configured_adapter_requires_retained_origin_before_operation_calls() {
    struct Probe(Arc<AtomicUsize>);
    impl Adapter for Probe {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                engine: "t3".into(),
                cancel: false,
            }
        }
        fn start(&mut self, _: &StartEnvelope) -> Result<StartReply> {
            panic!("no provider Start authorized")
        }
        fn observe(
            &mut self,
            _: &EngineRef,
            _: &BTreeMap<String, String>,
        ) -> Result<Vec<EngineEvent>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        }
        fn reconcile(
            &mut self,
            _: &StartEnvelope,
            _: Option<&EngineRef>,
        ) -> Result<ReconcileReply> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ReconcileReply::Unknown {
                reason: "synthetic positive control".into(),
            })
        }
    }
    for known in [false, true] {
        let (_temp, runner, _, app) = if known {
            crate::runtime::t3_routes::tests::fixture()
        } else {
            crate::runtime::t3_routes::tests::unknown_origin_fixture()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let mut adapters = Adapters::new();
        adapters.insert("t3".into(), Box::new(Probe(calls.clone())));
        let jobs = crate::context_jobs::Jobs::open(runner.operational_root()).unwrap();
        let mut backend = Backend {
            runner,
            app,
            adapters,
            exports: Default::default(),
            index: None,
            index_error: None,
            context_jobs: jobs,
            todoist_picker: Default::default(),
        };
        let goal = backend.runner.snapshot().unwrap().goal.unwrap().id;
        for command in [
            Command::Poll {
                goal_id: Some(goal.clone()),
            },
            Command::Reconcile {
                goal_id: Some(goal.clone()),
            },
        ] {
            let result = backend.handle(command);
            if known {
                result.unwrap();
            } else {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("historical_origin_unavailable"));
            }
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if known { 2 } else { 0 },
            "known-route positive control proves both operation probes are wired"
        );
    }
}

#[test]
fn ordinary_second_goal_binds_before_first_stage_and_starts_without_reconfiguration() {
    for chat_first in [false, true] {
        let (_temp, backend, _) = fixture();
        let mut owner = backend.lock().unwrap();
        let goal = Goal {
            id: uuid::Uuid::new_v4().to_string(),
            title: "Second ordinary goal".into(),
            status: "draft".into(),
            criteria: vec![Criterion {
                id: "C1".into(),
                description: "Check".into(),
                requires_human: false,
            }],
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::new(),
        };
        let goal_id = goal.id.clone();
        owner
            .handle(Command::CreateGoal {
                goal,
                body: "# Second".into(),
            })
            .unwrap();
        if chat_first {
            owner.app.chat = Some(crate::chat::ChatConfig {
                api_key: "synthetic-no-network".into(),
                base_url: "http://127.0.0.1:9".into(),
                model: "synthetic".into(),
                idle_timeout: std::time::Duration::from_secs(1),
            });
            let Backend { runner, app, .. } = &mut *owner;
            runner
                .with_goal(&goal_id, |runner| {
                    let (conversation, _, _) = app.chat_start(
                        runner,
                        goal_id.clone(),
                        "Prepare a plan".into(),
                        vec![],
                        None,
                    )?;
                    // Complete the already persisted intent locally; no chat transport.
                    Application::chat_event(
                        runner,
                        &conversation,
                        crate::chat::ChatEvent::Complete {
                            text: "Synthetic plan".into(),
                        },
                    )?;
                    assert!(!runner.application_state()["provider_identity"]["t3"].is_null());
                    Ok(())
                })
                .unwrap();
        }
        owner
            .handle(Command::StagePrepare {
                goal_id: goal_id.clone(),
                conversation_id: None,
                source_paths: vec![],
                criterion_ids: vec!["C1".into()],
                next_step: "Check fresh stage".into(),
                reviewed_packet: None,
                previous_result_id: None,
            })
            .unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        owner
            .adapters
            .insert("t3".into(), Box::new(CountStart(starts.clone())));
        owner
            .handle(Command::Start {
                goal_id: Some(goal_id.clone()),
                expected: None,
            })
            .unwrap();
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "new goal retains supported explicit first Start"
        );
        owner
            .runner
            .with_goal(&goal_id, |runner| {
                assert!(!runner.application_state()["provider_identity"]["t3"].is_null());
                Ok(())
            })
            .unwrap();
    }
}
