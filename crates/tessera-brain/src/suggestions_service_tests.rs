use super::*;
use crate::suggestions::{Outcome, SetRequest};
use tessera_core::source::WriteBoundary;
use uuid::Uuid;

fn fixture() -> (tempfile::TempDir, Arc<Mutex<Backend>>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("brain");
    let operational = dir.path().join("state");
    std::fs::create_dir_all(root.join("records")).unwrap();
    std::fs::create_dir(&operational).unwrap();
    let runner = Runner::open(RunnerConfig {
        brain_id: Uuid::new_v4().to_string(),
        root,
        operational_dir: operational.clone(),
        records_dir: "records".into(),
        boundary: WriteBoundary::Managed,
    })
    .unwrap();
    let backend = Backend {
        runner,
        adapters: Adapters::new(),
        app: Application::unconfigured(),
        exports: crate::export::ExportDownloads::default(),
        todoist_picker: Default::default(),
        index: None,
        index_error: None,
        context_jobs: crate::context_jobs::Jobs::open(&operational).unwrap(),
    };
    (dir, Arc::new(Mutex::new(backend)))
}
fn set(shared: &Arc<Mutex<Backend>>, request: SetRequest) -> Outcome {
    let workspace = shared.lock().unwrap().runner.workspace_identity();
    serde_json::from_value(
        dispatch(shared, Command::SuggestionsSet { request }, Some(workspace)).unwrap(),
    )
    .unwrap()
}
#[test]
fn suggestions_wire_is_workspace_guarded_and_rejects_extra_fields() {
    for op in ["suggestions_get", "suggestions_set"] {
        let mut wire = json!({"schema":"ai-brain/workspace-v1","id":"control","expected_workspace":{},"op":op});
        if op == "suggestions_set" {
            wire["operation_id"] = Uuid::new_v4().to_string().into();
            wire["expected_revision"] = 0.into();
            wire["enabled"] = true.into();
        }
        assert!(serde_json::from_value::<Request>(wire.clone()).is_ok());
        let mut invalid = wire.clone();
        invalid["extra"] = true.into();
        assert!(serde_json::from_value::<Request>(invalid).is_err());
        let mut invalid = wire.clone();
        invalid["schema"] = SCHEMA.into();
        assert!(serde_json::from_value::<Request>(invalid).is_err());
        let mut invalid = wire;
        invalid
            .as_object_mut()
            .unwrap()
            .remove("expected_workspace");
        assert!(serde_json::from_value::<Request>(invalid).is_err());
    }
}
#[test]
fn suggestions_service_retains_definite_refusal_and_returns_current_state_after_old_receipt() {
    let (_dir, shared) = fixture();
    let request = SetRequest {
        operation_id: Uuid::new_v4().to_string(),
        expected_revision: 0,
        enabled: true,
    };
    let refusal = set(&shared, request.clone());
    assert_eq!(refusal.status, "not_applied");
    assert!(refusal.receipt.is_none());
    assert_eq!(refusal.request, request);
    assert_eq!(
        refusal.reason,
        Some(crate::suggestions::Refusal::ProviderUnavailable)
    );
    {
        let mut b = shared.lock().unwrap();
        b.app.settings.chat = Some(crate::application::ChatSettings {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "test-model".into(),
            api_key_env: "TEST_ONLY".into(),
        });
        b.app.chat = Some(chat::ChatConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "test-model".into(),
            api_key: "test-only".into(),
            idle_timeout: Duration::from_secs(1),
        });
        crate::settings::save(b.runner.operational_root(), &b.app.settings, None).unwrap();
    }
    assert_eq!(set(&shared, request).status, "not_applied");
    let enable = SetRequest {
        operation_id: Uuid::new_v4().to_string(),
        expected_revision: 0,
        enabled: true,
    };
    let accepted = set(&shared, enable.clone());
    assert_eq!(accepted.status, "committed");
    assert_eq!(accepted.receipt.unwrap().request, enable);
    let pause = SetRequest {
        operation_id: Uuid::new_v4().to_string(),
        expected_revision: 1,
        enabled: false,
    };
    {
        let mut b = shared.lock().unwrap();
        let mut config = b.app.settings.clone();
        config.actor = "Changed saved Review actor".into();
        config.chat = None;
        let operational = b.runner.operational_root().to_path_buf();
        crate::settings::save(&operational, &config, None).unwrap();
        b.app = Application::configure_runtime(config, &operational, &mut b.runner, false)
            .unwrap()
            .0;
    }
    assert_eq!(set(&shared, pause).status, "committed");
    let old_enable = set(&shared, enable).receipt.unwrap();
    assert!(old_enable.replayed);
    assert_eq!(old_enable.actor, "local operator");
    let mut b = shared.lock().unwrap();
    let state = b.handle(Command::SuggestionsGet).unwrap();
    assert_eq!(state["mode"], "paused");
    assert_eq!(state["revision"], 2);
    assert_eq!(state["provider"]["available"], false);
    assert_eq!(state["can_change"], true);
}
#[test]
fn wrong_workspace_refuses_before_control_acceptance() {
    let (_dir, shared) = fixture();
    let request = SetRequest {
        operation_id: Uuid::new_v4().to_string(),
        expected_revision: 0,
        enabled: false,
    };
    assert!(dispatch(
        &shared,
        Command::SuggestionsSet { request },
        Some(json!({"wrong":"workspace"}))
    )
    .is_err());
    let b = shared.lock().unwrap();
    assert!(!b.runner.suggestions_controlled());
    assert_eq!(
        b.runner
            .suggestions_status(b.suggestions_provider())
            .unwrap()
            .revision,
        0
    );
}
