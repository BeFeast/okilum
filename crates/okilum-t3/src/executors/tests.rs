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

#[test]
fn optional_current_task_is_stable_and_old_registry_columns_remain_supported() {
    let source="## Current threads\n| Role | Thread | Current task |\n|---|---|---|\n| Manager | `mcp:manager` | |\n| Editing | `mcp:launch:current` | #1860 |\n";
    assert_eq!(
        Registry::parse(source).unwrap().executors[0].current_task,
        Some(1860)
    );
    assert!(Registry::parse(REGISTRY)
        .unwrap()
        .executors
        .iter()
        .all(|entry| entry.current_task.is_none()));
    for invalid in ["#0", "#1860 #1859", "unknown"] {
        assert!(Registry::parse(&source.replace("#1860", invalid)).is_err());
    }
    assert!(Registry::parse(&source.replace(
        "| Editing | `mcp:launch:current` | #1860 |",
        "| Editing | `mcp:launch:current` | #1860 |\n| Editing | `mcp:other` | #1859 |"
    ))
    .is_err());
}

#[test]
fn malformed_current_task_reports_file_line_role_and_exact_value_without_guessing() {
    let source="## Current threads\n\n| Role | Thread | Current task |\n|---|---|---|\n| Manager | `mcp:manager` | |\n| UI | `mcp:ui` | #2020 PR #2026; #2001 main ownership |\n";
    let error = format!("{:#}", Registry::parse(source).unwrap_err());
    assert!(
        error.contains("ops/thread-ids.md:6")
            && error.contains("UI")
            && error.contains("#2020 PR #2026; #2001 main ownership"),
        "{error}"
    );
    assert!(error.contains("another column"));
    for (value, expected) in [
        ("#2020", Some(2020)),
        ("`#2020`", Some(2020)),
        ("2020", Some(2020)),
        ("—", None),
        ("", None),
    ] {
        let valid = source.replace("#2020 PR #2026; #2001 main ownership", value);
        assert_eq!(
            Registry::parse(&valid).unwrap().executors[0].current_task,
            expected
        );
    }
}

#[test]
fn link_only_or_ambiguous_thread_ids_name_file_physical_line_role_and_repair() {
    for (role, original, line) in [
        ("Manager", "`mcp:manager`", 5),
        ("UI", "`mcp:ui` description", 6),
    ] {
        for cell in [
            "[Thread](t3-thread://env/mcp:thread)",
            "[Thread](`mcp:thread`)",
            "`mcp:one` `mcp:two`",
            "`mcp:`",
            "`mcp:unclosed",
            "`mcp:two words`",
        ] {
            // CRLF and non-ASCII prose must not change the physical line number.
            let source = format!("# Registry 🧠\n{}", REGISTRY.replace(original, cell))
                .replace('\n', "\r\n");
            let error = Registry::parse_at(&source, "/fixture/area/ops/thread-ids.md")
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!("/fixture/area/ops/thread-ids.md:{}", line + 1)),
                "{error}"
            );
            assert!(
                error.contains(role) && error.contains("`mcp:<id>`") && error.contains("backticks"),
                "{error}"
            );
        }
    }
    let valid = REGISTRY.replace(
        "`mcp:manager`",
        "`mcp:manager` [Manager](t3-thread://env/mcp:manager)",
    );
    assert_eq!(Registry::parse(&valid).unwrap().manager, "mcp:manager");
}

#[test]
fn decisions_and_explicit_references_never_establish_executor_ownership() {
    let legacy = REGISTRY.replace(
        "| Missing | `mcp:missing` | Code |",
        "| decision-thread | `mcp:unregistered` | Decisions |",
    );
    let typed = "## Current threads\n| Role | Thread | Kind | Current task |\n|---|---|---|---|\n| Manager | `mcp:manager` | manager | |\n| UI | `mcp:ui` | executor | #1464 |\n| Architecture | `mcp:unregistered` | reference | not an issue |\n";
    for source in [&legacy, typed] {
        let registry = Registry::parse(source).unwrap();
        assert_eq!(registry.executors.len(), 1);
        let fake = Fake {
            calls: Mutex::new(vec![]),
            fail_questions: false,
        };
        let snapshot = Snapshot::refresh(&fake, registry, "executors").unwrap();
        assert_eq!(snapshot.registered.len(), 1);
        assert_eq!(snapshot.registered[0].entry.role, "UI");
        assert!(
            snapshot.unregistered.is_empty(),
            "references must not reappear in the unregistered group"
        );
        assert_eq!(
            *fake.calls.lock().unwrap(),
            ["mcp:ui"],
            "only executors get question queries"
        );
    }
    for invalid in ["", "worker", "manager"] {
        let error = Registry::parse(&typed.replace("| executor |", &format!("| {invalid} |")))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("ops/thread-ids.md:5")
                && error.contains("Kind")
                && error.contains("executor")
                && error.contains("reference"),
            "{error}"
        );
    }
    // Existing free-form executor roles remain valid without a Kind column.
    assert_eq!(Registry::parse(REGISTRY).unwrap().executors.len(), 2);
}
