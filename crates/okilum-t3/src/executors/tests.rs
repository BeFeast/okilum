use super::*;
use crate::gateway::GatewayResult;
use crate::{
    Capabilities, GatewayStatus, ModelSelection, OutgoingMessage, SendReceipt, ServerIdentity,
    ThreadConfiguration, ThreadPage, ThreadQuery,
};
use serde_json::json;
use std::sync::Mutex;

const REGISTRY: &str = "## Текущие threads (21.10)\n\n| Роль | Thread | Project |\n|---|---|---|\n| Manager | `mcp:manager` | Managers |\n| UI | `mcp:ui` description | Code |\n| Missing | `mcp:missing` | Code |\n\n## History\n| Old | `mcp:old` | Code |\n";
fn summary(id: &str) -> ThreadSummary {
    serde_json::from_value(json!({"threadId":id,"title":id,"status":"running","latestRunId":"r","providerInstanceId":"codex","model":"gpt-6.1-sol","runtimeMode":"full-access","parentThreadId":null,"linkedPullRequest":null,"pullRequests":[{"url":"https://git.example/pulls/7"}],"itemCount":42,"updatedAt":"now"})).unwrap()
}
struct Fake {
    calls: Mutex<Vec<String>>,
    fail_questions: bool,
}
impl ManagerGateway for Fake {
    fn status(&self) -> GatewayStatus {
        unimplemented!()
    }
    fn server_identity(&self) -> GatewayResult<ServerIdentity> {
        unimplemented!()
    }
    fn list_projects(&self) -> GatewayResult<Vec<crate::Project>> {
        unimplemented!()
    }
    fn list_threads(&self, project: &str) -> GatewayResult<Vec<ThreadSummary>> {
        assert_eq!(project, "executors");
        Ok(vec![
            summary("mcp:ui"),
            summary("mcp:unregistered"),
            summary("mcp:manager"),
        ])
    }
    fn pending_requests(&self, id: &str) -> GatewayResult<Vec<String>> {
        self.calls.lock().unwrap().push(id.into());
        if self.fail_questions {
            Err(crate::GatewayError::Unreachable("offline".into()))
        } else {
            Ok(vec!["q".into()])
        }
    }
    fn read_thread(&self, _: &ThreadQuery) -> GatewayResult<ThreadPage> {
        unimplemented!()
    }
    fn send_message(&self, _: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        unimplemented!()
    }
    fn capabilities(&self) -> GatewayResult<Capabilities> {
        unimplemented!()
    }
    fn thread_configuration(&self, _: &str) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
    fn configure_model(&self, _: &str, _: &ModelSelection) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
}
#[test]
fn current_registry_not_history_or_parent_ids_defines_ownership() {
    let registry = Registry::parse(REGISTRY).unwrap();
    assert_eq!(registry.manager, "mcp:manager");
    assert_eq!(registry.executors.len(), 2);
    let fake = Fake {
        calls: Mutex::new(vec![]),
        fail_questions: false,
    };
    let snapshot = Snapshot::refresh(&fake, registry, "executors").unwrap();
    assert_eq!(snapshot.registered[0].entry.role, "UI");
    assert_eq!(
        snapshot.registered[0].thread.as_ref().unwrap().item_count,
        42
    );
    assert_eq!(snapshot.registered[0].pending_questions, Some(1));
    assert!(snapshot.registered[1].thread.is_none());
    assert_eq!(
        snapshot
            .unregistered
            .iter()
            .map(|t| t.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["mcp:unregistered"]
    );
    assert_eq!(*fake.calls.lock().unwrap(), ["mcp:ui"]);
}
#[test]
fn malformed_ambiguous_duplicate_or_historical_only_registries_fail_explicitly() {
    for bad in [
        REGISTRY.replace("`mcp:missing`", "`mcp:ui`"),
        REGISTRY.replace("`mcp:ui` description", "`mcp:ui` `mcp:second`"),
        REGISTRY.replace("| Manager |", "| Other |"),
        REGISTRY.replace("## Текущие threads (21.10)", "## History"),
        format!("{REGISTRY}\n## Current threads\n{REGISTRY}"),
    ] {
        assert!(Registry::parse(&bad).is_err(), "{bad}");
    }
}
#[test]
fn a_question_error_is_unknown_not_no_questions_and_keeps_the_thread() {
    let fake = Fake {
        calls: Mutex::new(vec![]),
        fail_questions: true,
    };
    let snapshot =
        Snapshot::refresh(&fake, Registry::parse(REGISTRY).unwrap(), "executors").unwrap();
    assert!(snapshot.registered[0].thread.is_some());
    assert_eq!(snapshot.registered[0].pending_questions, None);
    assert!(snapshot.registered[0]
        .question_error
        .as_deref()
        .unwrap()
        .contains("offline"));
}
#[test]
fn modern_pr_links_work_when_the_legacy_link_is_null_and_nonweb_actions_refuse() {
    let mut thread = summary("t");
    assert_eq!(thread.pull_request_urls(), ["https://git.example/pulls/7"]);
    thread.linked_pull_request = Some(json!({"url":"https://git.example/pulls/7"}));
    assert_eq!(thread.pull_request_urls().len(), 1);
    assert!(crate::is_http_link(thread.pull_request_urls()[0]));
    for bad in [
        "file:///private",
        "javascript:alert(1)",
        "https://key:secret@git.example/pr/1",
        "t3code://threads/env/thread",
    ] {
        assert!(!crate::is_http_link(bad));
    }
}

const LAUNCH_ID: &str = "okilum-launch-12a8d3a5-8f24-47ec-8aab-7c0cc81be91a";
fn correlated_registry() -> String {
    format!("## Current threads\n\n| Role | Thread | Launch request |\n|---|---|---|\n| Manager | `mcp:manager` | |\n| UI | `mcp:ui` | `{LAUNCH_ID}` |\n| Missing | `mcp:missing` | |\n")
}
fn launch_request() -> crate::launch::Request {
    crate::launch::Request {
        request_id: LAUNCH_ID.into(),
        role: "UI".into(),
        brief_path: "brief.md".into(),
        brief_source: "brief".into(),
        executor_project_id: "executors".into(),
        executor_project_root: "/server/repo".into(),
        worktree_path: "/server/worktree".into(),
        branch: "feature/ui".into(),
        model_selection: ModelSelection {
            instance_id: "codex".into(),
            model: "gpt-6.1-sol".into(),
            options: vec![],
        },
        manager_instructions: "test fixture".into(),
    }
}

#[test]
fn explicit_launch_column_is_optional_but_malformed_or_duplicate_request_id_is_rejected() {
    assert!(Registry::parse(REGISTRY)
        .unwrap()
        .executors
        .iter()
        .all(|e| e.launch_request.is_none()));
    let registry = correlated_registry();
    assert_eq!(
        Registry::parse(&registry).unwrap().executors[0]
            .launch_request
            .as_deref(),
        Some(LAUNCH_ID)
    );
    for bad in [
        registry.replace(
            "| Missing | `mcp:missing` | |",
            &format!("| Missing | `mcp:missing` | `{LAUNCH_ID}` |"),
        ),
        registry.replace(LAUNCH_ID, "not-an-operation"),
        registry.replace(
            "| Manager | `mcp:manager` | |",
            &format!("| Manager | `mcp:manager` | `{LAUNCH_ID}` |"),
        ),
        registry.replace("| Launch request |", "| Launch request | Launch request |"),
    ] {
        assert!(Registry::parse(&bad).is_err(), "{bad}");
    }
}

#[test]
fn launch_progress_requires_explicit_correlation_then_uses_exact_live_thread_state() {
    use crate::launch::{progress, Phase};
    let fake = Fake {
        calls: Mutex::new(vec![]),
        fail_questions: false,
    };
    let request = launch_request();
    let mut legacy =
        Snapshot::refresh(&fake, Registry::parse(REGISTRY).unwrap(), "executors").unwrap();
    legacy.unregistered[0].title = format!("UI {LAUNCH_ID}");
    legacy.unregistered[0].parent_thread_id = Some("mcp:manager".into());
    assert_eq!(
        progress(&request, "mcp:manager", Some(&legacy))
            .unwrap()
            .phase,
        Phase::Requested,
        "matching role/title/parent and a live thread are not explicit request correlation"
    );
    let mut snapshot = Snapshot::refresh(
        &fake,
        Registry::parse(&correlated_registry()).unwrap(),
        "executors",
    )
    .unwrap();
    for (status, expected) in [
        ("preparing", Phase::Preparing),
        ("running", Phase::Running),
        ("waiting", Phase::Waiting),
        ("completed", Phase::Completed),
        ("failed", Phase::Failed),
        ("cancelled", Phase::Cancelled),
        ("idle", Phase::Idle),
        ("future-status", Phase::Unknown),
    ] {
        snapshot.registered[0].thread.as_mut().unwrap().status = status.into();
        let projected = progress(&request, "mcp:manager", Some(&snapshot)).unwrap();
        assert_eq!(projected.phase, expected);
        assert_eq!(projected.thread_id.as_deref(), Some("mcp:ui"));
        assert!(projected.detail.contains(status));
    }
    snapshot.registered[0].thread = None;
    let missing = progress(&request, "mcp:manager", Some(&snapshot)).unwrap();
    assert_eq!(missing.phase, Phase::Accepted);
    assert!(missing.detail.contains("missing"));
    assert!(progress(&request, "mcp:other-manager", Some(&snapshot)).is_err());
    snapshot.project_id = "other-project".into();
    assert!(progress(&request, "mcp:manager", Some(&snapshot)).is_err());
    snapshot.project_id = "executors".into();
    snapshot.registered[1].entry.launch_request = Some(LAUNCH_ID.into());
    assert!(
        progress(&request, "mcp:manager", Some(&snapshot)).is_err(),
        "ambiguous relation never picks a thread"
    );
}

#[test]
fn manager_status_projection_does_not_issue_executor_question_requests() {
    let fake = Fake {
        calls: Mutex::new(vec![]),
        fail_questions: false,
    };
    let snapshot = Snapshot::refresh_with_questions(
        &fake,
        Registry::parse(&correlated_registry()).unwrap(),
        "executors",
        false,
    )
    .unwrap();
    assert!(snapshot.registered[0].thread.is_some());
    assert_eq!(snapshot.registered[0].pending_questions, None);
    assert!(fake.calls.lock().unwrap().is_empty());
}
