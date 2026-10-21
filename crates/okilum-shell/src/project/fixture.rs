//! Non-publishing visual fixture and native UI test gateway. No real OAuth,
//! threads or projects are touched; the production ProjectView renders it.
use super::*;
use okilum_t3::{
    Capabilities, GatewayError, ModelSelection, OutgoingMessage, Project, SendReceipt,
    ThreadConfiguration, ThreadPage, ThreadQuery, ThreadSummary,
};
use std::sync::Mutex;

type Result<T> = std::result::Result<T, GatewayError>;
#[derive(Default)]
pub(super) struct Fake {
    pub revoked: bool,
    pub wrong_project: bool,
    pub fail_first: bool,
    pub sends: Mutex<Vec<OutgoingMessage>>,
    pub reads: Mutex<Vec<ThreadQuery>>,
    pub threads: Mutex<Vec<ThreadSummary>>,
    pub list_error: Mutex<Option<String>>,
    pub model_current: Mutex<Option<ModelSelection>>,
    pub model_failure: Mutex<Option<GatewayError>>,
    pub model_read_failure: Mutex<Option<GatewayError>>,
    pub model_writes: Mutex<Vec<ModelSelection>>,
    pub grown: Mutex<bool>,
}
impl ManagerGateway for Fake {
    fn status(&self) -> GatewayStatus {
        if self.revoked {
            GatewayStatus::Revoked
        } else {
            GatewayStatus::Connected {
                expires_at: u64::MAX,
            }
        }
    }
    fn server_identity(&self) -> Result<ServerIdentity> {
        Ok(ServerIdentity {
            environment_id: "environment:fixture".into(),
            label: "Fixture".into(),
        })
    }
    fn list_projects(&self) -> Result<Vec<Project>> {
        unimplemented!()
    }
    fn list_threads(&self, _: &str) -> Result<Vec<ThreadSummary>> {
        if let Some(error) = self.list_error.lock().unwrap().clone() {
            return Err(GatewayError::Protocol(error));
        }
        Ok(self.threads.lock().unwrap().clone())
    }
    fn read_thread(&self, q: &ThreadQuery) -> Result<ThreadPage> {
        self.reads.lock().unwrap().push(q.clone());
        if self.revoked {
            return Err(GatewayError::Revoked);
        }
        let grown = *self.grown.lock().unwrap();
        let items = if q.after_position.is_some() {
            serde_json::json!([])
        } else {
            serde_json::json!([
                {"position":0,"itemId":"user-0","messageId":"message-0","type":"user_message","status":"completed","text":"What is the next step for this project?","updatedAt":"now"},
                {"position":1,"itemId":"reply-1","messageId":"message-1","type":"assistant_message","status":if grown {"completed"}else{"running"},"text":if grown {"The first slice is ready for review.\n\n- The manager stays in T3.\n- Your notes remain the source of truth.\n\n**Next:** check the result, then continue with the executor."}else{"The first slice is ready for review."},"updatedAt":"now"}
            ])
        };
        serde_json::from_value(serde_json::json!({
            "thread":{"threadId":q.thread_id,"projectId":if self.wrong_project {"different-project"}else{"fixture-managers"},"title":"Project manager","status":"running","activeRunId":"run-1","providerInstanceId":"codex","model":"gpt-6.1-sol","runtimeMode":"full-access","updatedAt":"now"},
            "items":items,"nextPosition":1,"hasMore":false
        })).map_err(|error|GatewayError::Protocol(error.to_string()))
    }
    fn send_message(&self, message: &OutgoingMessage) -> Result<SendReceipt> {
        let mut sends = self.sends.lock().unwrap();
        sends.push(message.clone());
        if self.fail_first && sends.len() == 1 {
            return Err(GatewayError::Unreachable(
                "The response was interrupted".into(),
            ));
        }
        Ok(SendReceipt {
            thread_id: message.thread_id.clone(),
            message_id: "message-sent".into(),
            run_id: "run-2".into(),
            status: "queued".into(),
            delivery: "queued".into(),
        })
    }
    fn pending_requests(&self, _: &str) -> Result<Vec<String>> {
        Ok(vec!["question-1".into()])
    }
    fn capabilities(&self) -> Result<Capabilities> {
        serde_json::from_value(serde_json::json!({"runtimeMode":"full-access","providers":[
            {"providerInstanceId":"codex","driverKind":"codex","models":[{"id":"gpt-6.1-sol"},{"id":"gpt-6-astra"}]}]}))
            .map_err(|error| GatewayError::Protocol(error.to_string()))
    }
    fn thread_configuration(&self, thread: &str) -> Result<ThreadConfiguration> {
        if let Some(error) = self.model_read_failure.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(ThreadConfiguration {
            thread_id: thread.into(),
            model_selection: self
                .model_current
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(ModelSelection {
                    instance_id: "codex".into(),
                    model: "gpt-6.1-sol".into(),
                    options: vec![],
                }),
            runtime_mode: "full-access".into(),
            interaction_mode: "default".into(),
        })
    }
    fn configure_model(
        &self,
        thread: &str,
        selection: &ModelSelection,
    ) -> Result<ThreadConfiguration> {
        self.model_writes.lock().unwrap().push(selection.clone());
        if let Some(error) = self.model_failure.lock().unwrap().clone() {
            return Err(error);
        }
        *self.model_current.lock().unwrap() = Some(selection.clone());
        self.thread_configuration(thread)
    }
}

pub(super) fn view(
    state: PathBuf,
    gateway: Arc<Fake>,
    window: &mut Window,
    cx: &mut Context<ProjectView>,
) -> ProjectView {
    let mut view = ProjectView::empty(Some(state), window, cx);
    view.binding=AreaBinding::parse("t3:\n  schema_version: 1\n  environment: example\n  manager: {project_id: fixture-managers, thread_id: 'mcp:fixture-manager', workspace_mode: local}\n  executor: {project_id: fixture-executors}\n  default_models:\n    manager: {instanceId: codex, model: gpt-6.1-sol}\n    executor: {instanceId: codex, model: gpt-6.1-sol}\n").unwrap();
    view.environment = Some(Environment::new("example", "https://t3.example.test").unwrap());
    view.origin.update(cx, |input, cx| {
        input.set_value("https://t3.example.test", window, cx)
    });
    view.gateway = Some(gateway);
    view.chat = Some(Conversation::new("mcp:fixture-manager"));
    view.reason = None;
    view.start_polling(window, cx);
    view
}

#[cfg(feature = "project-ui-harness")]
pub(super) fn open(cx: &mut App) -> bool {
    let Ok(mode) = std::env::var("OKILUM_PROJECT_UI_FIXTURE") else {
        return false;
    };
    if mode.ends_with("light") {
        gpui_component::Theme::change(gpui_component::ThemeMode::Light, None, cx);
    } else {
        gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
    }
    brand::apply_theme(cx);
    let state = reader_history::state_directory().expect("isolated fixture state");
    let gateway = Arc::new(Fake {
        revoked: mode.starts_with("revoked"),
        grown: Mutex::new(true),
        ..Default::default()
    });
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(900.), px(660.)),
                cx,
            ))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title("Okilum — Project fixture");
            let view = cx.new(|cx| view(state, gateway, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("Project fixture window");
    true
}
