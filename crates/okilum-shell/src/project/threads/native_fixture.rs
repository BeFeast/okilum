//! Explicit, non-publishing Linux QA surface over a separately provisioned T3
//! project. No login/keychain writes or provider turn is started by setup.
use super::*;
use okilum_t3::stream::{NativeClient, NativeToken};

pub(in crate::project) fn open(cx: &mut App) -> bool {
    if std::env::var("OKILUM_PROJECT_UI_FIXTURE").as_deref() != Ok("native-drafts") {
        return false;
    }
    let root = PathBuf::from(std::env::var("OKILUM_T3_QA_ROOT").expect("isolated QA root"));
    let project = std::env::var("OKILUM_T3_QA_PROJECT").expect("isolated T3 project");
    let thread = std::env::var("OKILUM_T3_QA_THREAD").expect("isolated T3 reference thread");
    let environment = Environment::new(
        "draft-qa",
        &std::env::var("OKILUM_T3_QA_ORIGIN").expect("QA origin"),
    )
    .expect("QA environment");
    let credential =
        std::env::var("OKILUM_T3_QA_CREDENTIAL_FILE").expect("private QA credential file");
    let token = NativeToken::from_secret(
        &environment,
        &std::fs::read(credential).expect("QA credential"),
    )
    .expect("valid QA credential");
    let expiry = token.expires_at();
    let state = reader_history::state_directory().expect("isolated QA state");
    // The controller explicitly provisions this marker alongside the copied
    // vault. A regular user launch cannot activate the fixture accidentally.
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("draft-qa.json")).expect("QA marker"))
            .expect("QA marker JSON");
    assert_eq!(marker["projectId"], project);
    assert_eq!(marker["threadId"], thread);
    reader_features::set_enabled(reader_features::Feature::Agents, true, cx);
    let access = reader_features::access(reader_features::Feature::Agents, cx);
    let client = Arc::new(
        NativeClient::new(environment.clone(), token, Duration::from_secs(10))
            .expect("QA native client")
            .with_access(access),
    );
    let binding = AreaBinding::parse(&format!("t3:\n  schema_version: 1\n  environment: draft-qa\n  manager: {{project_id: '{project}', thread_id: '{thread}', workspace_mode: local}}\n  executor: {{project_id: '{project}'}}\n  default_models:\n    manager: {{instanceId: codex, model: gpt-6.1-sol}}\n    executor: {{instanceId: codex, model: gpt-6.1-sol}}\n")).expect("QA binding");
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1000.), px(780.)),
                cx,
            ))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title("Okilum — Native draft QA");
            let view = cx.new(|cx| {
                let mut view = ProjectView::empty(Some(state), window, cx);
                view.area = Some(root);
                view.binding = binding;
                view.environment = Some(environment);
                view.chat = Some(Conversation::new(thread));
                view.reason = None;
                view.threads.enabled = true;
                view.install_native(client, expiry, window, cx);
                view
            });
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("native draft QA window");
    true
}
