//! Non-publishing visual fixture and native UI test gateway. No real OAuth,
//! threads or projects are touched; the production ProjectView renders it.
use super::*;
use okilum_t3::{
    Capabilities, GatewayError, ModelSelection, OutgoingMessage, Project, SendReceipt,
    ThreadConfiguration, ThreadPage, ThreadQuery, ThreadSummary,
};
use std::sync::Mutex;

/// Reader normalizes its vault root. Build all fixture paths in that same form,
/// including on macOS where the temporary directory is reached through /var.
#[cfg(test)]
pub(super) fn canonical_tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

/// Join every owned worker before reporting its first panic. Cleanup must keep
/// the original test failure when it runs during unwinding, rather than aborting
/// the binary with a second panic from Drop.
#[cfg(test)]
pub(super) fn join_workers(workers: impl IntoIterator<Item = std::thread::JoinHandle<()>>) {
    let mut failure = None;
    for worker in workers {
        if let Err(error) = worker.join() {
            if failure.is_none() {
                failure = Some(error);
            }
        }
    }
    if !std::thread::panicking() {
        if let Some(error) = failure {
            std::panic::resume_unwind(error);
        }
    }
}

type Result<T> = std::result::Result<T, GatewayError>;
#[derive(Default)]
pub(super) struct Fake {
    pub revoked: bool,
    pub many_models: bool,
    pub picker_reference: bool,
    pub model_catalog: Mutex<Option<Capabilities>>,
    pub connection_status: Option<GatewayStatus>,
    pub wrong_project: bool,
    pub fail_first: bool,
    pub sends: Mutex<Vec<OutgoingMessage>>,
    pub reads: Mutex<Vec<ThreadQuery>>,
    pub threads: Mutex<Vec<ThreadSummary>>,
    pub list_error: Mutex<Option<String>>,
    pub model_current: Mutex<Option<ModelSelection>>,
    pub model_failure: Mutex<Option<GatewayError>>,
    pub model_read_failure: Mutex<Option<GatewayError>>,
    pub question_failure: Option<GatewayError>,
    pub model_writes: Mutex<Vec<ModelSelection>>,
    pub grown: Mutex<bool>,
    pub local_projects: Mutex<Option<Vec<Project>>>,
    pub local_creates: Mutex<Vec<String>>,
    pub local_starts: Mutex<Vec<String>>,
}
impl ManagerGateway for Fake {
    fn status(&self) -> GatewayStatus {
        if let Some(status) = &self.connection_status {
            return status.clone();
        }
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
        if let Some(projects) = self.local_projects.lock().unwrap().clone() {
            return Ok(projects);
        }
        Ok(vec![Project {
            id: "fixture-executors".into(),
            title: "Executors".into(),
            workspace_root: "/server/repo".into(),
            default_model_selection: Some(ModelSelection {
                instance_id: "codex".into(),
                model: "gpt-6.1-sol".into(),
                options: vec![],
            }),
            ..Default::default()
        }])
    }
    fn create_project(&self, title: &str, root: &str) -> Result<Project> {
        self.local_creates.lock().unwrap().push(root.into());
        let project = Project {
            id: "fixture-local".into(),
            title: title.into(),
            workspace_root: root.into(),
            ..Default::default()
        };
        self.local_projects
            .lock()
            .unwrap()
            .as_mut()
            .expect("local fixture explicitly enabled")
            .push(project.clone());
        Ok(project)
    }
    fn read_project(&self, id: &str) -> Result<Project> {
        self.list_projects()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| GatewayError::Protocol("Project not found".into()))
    }
    fn start_manager(&self, project: &str, _: &str, _: &str) -> Result<String> {
        self.local_starts.lock().unwrap().push(project.into());
        Ok("mcp:fixture-local-manager".into())
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
        let project_id = if self.local_projects.lock().unwrap().is_some() {
            "fixture-local"
        } else {
            "fixture-managers"
        };
        let items = if q.after_position.is_some() {
            serde_json::json!([])
        } else if self.picker_reference {
            serde_json::json!([
                {"position":0,"itemId":"user-0","messageId":"message-0","type":"user_message","status":"completed","text":"The model picker should stay quick and easy to use in a narrow Manager panel.","updatedAt":"now"},
                {"position":1,"itemId":"reply-1","messageId":"message-1","type":"assistant_message","status":"completed","text":"The connection is ready. Your current model is **GPT-6 Astra**, with **High** reasoning.\n\n- Search across your connected providers.\n- Star the models you use most often.\n- Choose only the reasoning options supported by that model.\n\nYour draft stays here while you compare models.","updatedAt":"now"}
            ])
        } else {
            serde_json::json!([
                {"position":0,"itemId":"user-0","messageId":"message-0","type":"user_message","status":"completed","text":"What is the next step for this project?","updatedAt":"now"},
                {"position":1,"itemId":"reply-1","messageId":"message-1","type":"assistant_message","status":if grown {"completed"}else{"running"},"text":if grown {"The first slice is ready for review.\n\n- The manager stays in T3.\n- Your notes remain the source of truth.\n\n**Next:** check the result, then continue with the executor."}else{"The first slice is ready for review."},"updatedAt":"now"}
            ])
        };
        serde_json::from_value(serde_json::json!({
            "thread":{"threadId":q.thread_id,"projectId":if self.wrong_project {"different-project"}else{project_id},"title":"Project manager","status":if self.picker_reference {"completed"} else {"running"},"activeRunId":if self.picker_reference {None::<&str>} else {Some("run-1")},"providerInstanceId":"codex","model":if self.picker_reference {"gpt-6-astra"} else {"gpt-6.1-sol"},"runtimeMode":"full-access","updatedAt":"now"},
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
        if let Some(error) = &self.question_failure {
            return Err(error.clone());
        }
        Ok(if self.picker_reference {
            Vec::new()
        } else {
            vec!["question-1".into()]
        })
    }
    fn capabilities(&self) -> Result<Capabilities> {
        if let Some(catalog) = self.model_catalog.lock().unwrap().clone() {
            return Ok(catalog);
        }
        let mut models = vec![
            serde_json::json!({"id":"gpt-6.1-sol"}),
            serde_json::json!({"id":"gpt-6-astra"}),
        ];
        if self.many_models {
            models.extend((0..40).map(|index| serde_json::json!({"id":format!("model-{index}"), "label":format!("Catalog model {index}")})));
        }
        serde_json::from_value(serde_json::json!({"runtimeMode":"full-access","providers":[
            {"providerInstanceId":"codex","driverKind":"codex","models":models}]}))
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
    #[cfg(test)]
    reader_features::enable_test_modules(cx);
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

/// Deterministic screenshot fixture. These are advertised per-model option
/// descriptors, not provider-family assumptions in the production picker.
#[cfg(any(test, feature = "project-ui-harness"))]
fn picker_reference_catalog() -> Capabilities {
    use serde_json::json;
    let effort = |levels: &[(&str, &str)], default: &str| {
        json!({
            "type":"select","id":"reasoningEffort","label":"Reasoning",
            "description":"Choose the reasoning level supported by this model.",
            "options":levels.iter().map(|(id,label)| json!({"id":id,"label":label,"isDefault":*id==default})).collect::<Vec<_>>(),
            "currentValue":default
        })
    };
    let fast = json!({"type":"boolean","id":"fastMode","label":"Fast","description":"Prefer faster responses when available.","currentValue":false});
    let model = |id: &str,
                 name: &str,
                 short: &str,
                 vendor: &str,
                 descriptors: Vec<serde_json::Value>| {
        json!({
            "id":id,"label":name,"shortName":short,"subProvider":vendor,"capabilities":{"optionDescriptors":descriptors}
        })
    };
    let standard_effort = effort(
        &[
            ("low", "Low"),
            ("medium", "Medium"),
            ("high", "High"),
            ("xhigh", "Extra High"),
            ("max", "Max"),
        ],
        "medium",
    );
    let mut claude_effort = effort(
        &[
            ("low", "Low"),
            ("medium", "Medium"),
            ("high", "High"),
            ("max", "Max"),
            ("ultrathink", "Ultracode"),
        ],
        "high",
    );
    claude_effort["promptInjectedValues"] = json!(["ultrathink"]);
    serde_json::from_value(json!({"runtimeMode":"full-access","providers":[
        {"providerInstanceId":"codex","driverKind":"codex","displayName":"OpenAI","models":[
            model("gpt-6-astra","OpenAI · GPT-6 Astra","GPT-6 Astra","OpenAI",vec![standard_effort.clone(),fast.clone()]),
            model("gpt-6.1-sol","OpenAI · GPT-6.1 Sol","GPT-6.1 Sol","OpenAI",vec![standard_effort.clone()]),
            model("gpt-6-luna","OpenAI · GPT-6 Luna","GPT-6 Luna","OpenAI",vec![standard_effort.clone(),fast])
        ]},
        {"providerInstanceId":"claude-primary","driverKind":"claudeAgent","displayName":"Claude","models":[
            model("claude-opus-5-5","Anthropic · Claude Opus 5.5","Claude Opus 5.5","Anthropic",vec![claude_effort]),
            model("claude-fable-5-1","Anthropic · Claude Fable 5.1","Claude Fable 5.1","Anthropic",vec![effort(&[("low","Low"),("high","High")],"high")])
        ]},
        {"providerInstanceId":"claude-relay","driverKind":"claudeAgent","displayName":"Claude → Astra","models":[
            model("gpt-6-astra","OpenAI · GPT-6 Astra","GPT-6 Astra","OpenAI",vec![standard_effort])
        ]},
        {"providerInstanceId":"grok","driverKind":"grok","displayName":"Grok","models":[
            model("grok-4-7","xAI · Grok 4.7","Grok 4.7","xAI",vec![]),
            model("grok-4-7-fast","xAI · Grok 4.7 Fast","Grok 4.7 Fast","xAI",vec![])
        ]},
        {"providerInstanceId":"opencode","driverKind":"opencode","displayName":"OpenCode","models":[
            model("kimi-k3","Moonshot · Kimi K3","Kimi K3","Moonshot",vec![]),
            model("deepseek-v4","DeepSeek · DeepSeek V4","DeepSeek V4","DeepSeek",vec![])
        ]},
        {"providerInstanceId":"antigravity","driverKind":"antigravity","displayName":"Antigravity","models":[
            model("gemini-3-8-flash","Google · Gemini 3.8 Flash","Gemini 3.8 Flash","Google",vec![])
        ]}
    ]})).expect("typed reference model catalog")
}

#[cfg(feature = "project-ui-harness")]
fn picker_reference_preferences(state: &std::path::Path) {
    use sha2::{Digest as _, Sha256};
    let origin = "https://t3.example.test";
    let directory = state.join("manager-model-preferences");
    std::fs::create_dir_all(&directory).expect("isolated model preference directory");
    let path = directory.join(format!("{:x}.json", Sha256::digest(origin.as_bytes())));
    if path.exists() {
        return;
    } // Explicit fixture edits survive its restart.
    std::fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "version":1,"origin":origin,"instances":{
                "codex":{"gpt-6-astra":{"favorite":true},"gpt-6.1-sol":{"favorite":true}},
                "claude-primary":{"claude-opus-5-5":{"favorite":true}},
                "grok":{"grok-4-7":{"favorite":true}}
            }
        }))
        .unwrap(),
    )
    .expect("isolated model preferences");
}

/// Repeatable native screenshots of the production Reader-mounted Manager.
/// Only the explicit harness feature/mode enables this synthetic local vault.
#[cfg(feature = "project-ui-harness")]
fn open_mounted_header(state: PathBuf, gateway: Arc<Fake>, cx: &mut App) {
    let vault = state.join("mounted-header-vault");
    std::fs::create_dir_all(vault.join("ops")).expect("fixture vault");
    std::fs::write(vault.join("ops/project.toml"), "kind = 'knowledge'\n")
        .expect("fixture project");
    std::fs::write(
        vault.join("home.md"),
        "# Project notes\n\nThe Manager stays beside your notes.\n",
    )
    .expect("fixture note");
    #[cfg(target_os = "linux")]
    let performance =
        std::env::var("OKILUM_PROJECT_UI_FIXTURE").is_ok_and(|mode| mode == "performance");
    let width = match std::env::var("OKILUM_HEADER_QA_WIDTH").as_deref() {
        Ok("320") => 320.,
        _ => 560.,
    };
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1200.), px(800.)),
                cx,
            ))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title("Okilum — Mounted Manager fixture");
            let reader = cx.new(|cx| {
                let mut reader = Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("home.md".into()),
                        index_dir: Some(state.join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                let project = cx.new(|cx| {
                    let mut project = view(state, gateway, window, cx);
                    project.identity = Some(ServerIdentity {
                        environment_id: "environment:fixture".into(),
                        label: "Fixture".into(),
                    });
                    project
                });
                reader.project_navigation.active =
                    Some(okilum_t3::projects::Project::read(&vault).expect("fixture project"));
                #[cfg(target_os = "linux")]
                if performance {
                    super::performance::populate(&project, cx);
                    let board = cx.new(|cx| {
                        super::board::Board::performance_fixture(vault.clone(), window, cx)
                    });
                    reader
                        .project_navigation
                        .boards
                        .insert(vault.clone(), board);
                    reader.project_navigation.board_visible = true;
                }
                reader.project = Some(project);
                reader.project_open = true;
                reader.panels.close(reader_layout::Panel::Notes);
                reader.panels.open(reader_layout::Panel::Backlinks);
                reader.panel_widths.backlinks = width;
                reader
            });
            cx.new(|cx| Root::new(reader, window, cx))
        },
    )
    .expect("mounted Manager fixture window");
}

#[cfg(feature = "project-ui-harness")]
pub(super) fn open(cx: &mut App) -> bool {
    if board::waiting::fixture::open(cx) {
        return true;
    }
    if board::fixture::open(cx) {
        return true;
    }
    #[cfg(native_manager)]
    if threads::native_fixture::open(cx) {
        return true;
    }
    let Ok(mode) = std::env::var("OKILUM_PROJECT_UI_FIXTURE") else {
        return false;
    };
    if mode.ends_with("light") {
        gpui_component::Theme::change(gpui_component::ThemeMode::Light, None, cx);
    } else {
        gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
    }
    brand::apply_theme(cx);
    let picker_reference = matches!(
        mode.as_str(),
        "picker-reference-light" | "picker-reference-dark"
    );
    let mounted_header = matches!(
        mode.as_str(),
        "mounted-header-light" | "mounted-header-dark"
    );
    let performance = cfg!(target_os = "linux") && mode == "performance";
    if picker_reference || mounted_header || performance {
        reader_features::set_enabled(reader_features::Feature::Agents, true, cx);
    }
    let state = reader_history::state_directory().expect("isolated fixture state");
    if matches!(
        mode.as_str(),
        "board-list-peek-dark"
            | "board-kanban-peek-dark"
            | "board-list-dark"
            | "board-list-light"
            | "board-kanban-dark"
            | "board-kanban-light"
            | "board-kanban-filtered-dark"
            | "board-kanban-filtered-light"
    ) {
        board::Board::open_list_fixture(state, mode.starts_with("board-kanban"), cx);
        return true;
    }
    if picker_reference {
        picker_reference_preferences(&state);
    }
    let gateway = Arc::new(Fake {
        revoked: mode.starts_with("revoked"),
        picker_reference,
        many_models: true,
        model_catalog: Mutex::new(picker_reference.then(picker_reference_catalog)),
        model_current: Mutex::new(picker_reference.then(|| serde_json::from_value(serde_json::json!({
            "instanceId":"codex","model":"gpt-6-astra","options":[{"id":"reasoningEffort","value":"high"}]
        })).expect("reference current model"))),
        grown: Mutex::new(true),
        ..Default::default()
    });
    if mounted_header || performance {
        #[cfg(target_os = "linux")]
        super::performance::collect(cx);
        open_mounted_header(state, gateway, cx);
        return true;
    }
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
            let view = cx.new(|cx| {
                let project = view(state, gateway, window, cx);
                if picker_reference {
                    project.compose.update(cx, |input, cx| {
                        input.set_value("Keep this draft while I compare models.", window, cx)
                    });
                }
                project
            });
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("Project fixture window");
    true
}

#[cfg(test)]
mod picker_reference_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn reference_catalog_has_distinct_accounts_supported_options_and_a_prompt_only_claude_value() {
        let catalog = picker_reference_catalog();
        assert_eq!(catalog.providers.len(), 6);
        let snapshot = okilum_t3::models::Snapshot {
            target: okilum_t3::models::Target {
                thread_id: "qa".into(),
                project_id: "qa".into(),
            },
            catalog,
            configuration: ThreadConfiguration {
                thread_id: "qa".into(),
                model_selection: ModelSelection {
                    instance_id: "codex".into(),
                    model: "gpt-6-astra".into(),
                    options: vec![],
                },
                runtime_mode: "full-access".into(),
                interaction_mode: "default".into(),
            },
        };
        let current = &snapshot.configuration.model_selection;
        let controls = snapshot.option_controls(current);
        assert_eq!(controls[0].label, "Reasoning");
        assert!(controls[0]
            .choices
            .iter()
            .any(|choice| choice.is_default && choice.label == "Medium"));
        assert!(controls[0]
            .choices
            .iter()
            .all(|choice| !choice.prompt_injected));
        assert_eq!(controls[1].label, "Fast");
        let claude = ModelSelection {
            instance_id: "claude-primary".into(),
            model: "claude-opus-5-5".into(),
            options: vec![],
        };
        assert!(snapshot.option_controls(&claude)[0]
            .choices
            .iter()
            .any(|choice| choice.prompt_injected && choice.label == "Ultracode"));
        assert_eq!(
            snapshot.model_info(current).unwrap().display_name(),
            "GPT-6 Astra"
        );
        assert_ne!(
            snapshot.catalog.providers[1].provider_instance_id,
            snapshot.catalog.providers[2].provider_instance_id
        );
    }
}
