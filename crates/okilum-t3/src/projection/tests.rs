use super::*;
use serde_json::json;
fn message(id: &str, ordinal: u64, kind: &str) -> Value {
    json!({"id":id,"threadId":"thread","ordinal":ordinal,"type":kind,"text":format!("body {id}"),"runId":"run","nodeId":"node","status":"completed","inputIntent":"turn_start","messageId":format!("message-{id}"),"futureField":true})
}
fn snapshot(items: Vec<Value>) -> ThreadUpdate {
    ThreadUpdate::Snapshot {
        sequence: 10,
        projection: json!({"thread":{"id":"thread","projectId":"project","title":"Manager"},"runs":[{"id":"run","status":"completed"}],"attempts":[],"turnItems":items,"visibleTurnItems":items.iter().enumerate().map(|(n,item)|json!({"position":n,"visibility":"local","sourceThreadId":"thread","sourceItemId":item["id"],"item":item})).collect::<Vec<_>>()}),
        history_cursor: None,
        has_more_history: false,
        payload_budget_exceeded: false,
        latest_local_turn_ordinal: None,
    }
}
fn update(kind: &str, payload: Value) -> ThreadUpdate {
    ThreadUpdate::Event {
        sequence: 11,
        event: json!({"type":kind,"threadId":"thread","payload":payload}),
    }
}
fn model(items: Vec<Value>) -> Projection {
    let mut p = Projection::new("thread", "project");
    p.apply(&[snapshot(items)]).unwrap();
    p.take_changes();
    p
}
fn row_key(id: &str) -> RowKey {
    RowKey {
        thread: "thread".into(),
        item: id.into(),
    }
}

#[test]
fn header_model_updates_do_not_erase_the_thread_title() {
    let mut initial = snapshot(vec![]);
    if let ThreadUpdate::Snapshot { projection, .. } = &mut initial {
        projection["thread"]["modelSelection"] = json!({"model":"original"});
    }
    let mut p = Projection::new("thread", "project");
    p.apply(&[initial]).unwrap();
    assert_eq!(p.metadata.title, "Manager");
    assert_eq!(p.take_changes().metadata.unwrap().model, "original");
    p.apply(&[update(
        "thread.metadata-updated",
        json!({"id":"thread","projectId":"project","modelSelection":{"model":"changed"}}),
    )])
    .unwrap();
    let metadata = p.take_changes().metadata.unwrap();
    assert_eq!(metadata.model, "changed");
    assert_eq!(metadata.title, "Manager");
    p.apply(&[update(
        "thread.metadata-updated",
        json!({"id":"thread","projectId":"project","title":"Renamed"}),
    )])
    .unwrap();
    assert_eq!(p.metadata.title, "Renamed");
    assert_eq!(p.take_changes().metadata.unwrap().model, "changed");
}

#[test]
fn streaming_changes_one_row_and_preserves_completed_history_allocations() {
    let mut p = model(
        (0..5000)
            .map(|i| message(&i.to_string(), i, "assistant_message"))
            .collect(),
    );
    let first = p.row(&row_key("0")).unwrap().clone();
    let mut tail = message("4999", 4999, "assistant_message");
    tail["text"] = json!("new streamed text 🧠");
    p.apply(&[update("turn-item.updated", tail.clone())])
        .unwrap();
    let c = p.take_changes();
    assert_eq!(c.rows.len(), 1);
    assert!(c.order.is_none());
    assert!(Arc::ptr_eq(&first, p.row(&row_key("0")).unwrap()));
    p.apply(&[
        update("turn-item.updated", tail),
        update("future.event", Value::Null),
    ])
    .unwrap();
    assert!(!p.take_changes().changed());
}
#[test]
fn malformed_batch_and_changed_project_leave_the_entire_projection_unchanged() {
    let mut p = model(vec![message("a", 1, "assistant_message")]);
    let old = p.row(&row_key("a")).unwrap().clone();
    let mut valid = message("a", 1, "assistant_message");
    valid["text"] = json!("changed");
    let mut bad = message("b", 2, "assistant_message");
    bad["text"] = json!(5);
    assert!(p
        .apply(&[
            update("turn-item.updated", valid),
            update("turn-item.updated", bad)
        ])
        .is_err());
    assert!(Arc::ptr_eq(&old, p.row(&row_key("a")).unwrap()));
    assert!(!p.take_changes().changed());
    let mut other = snapshot(vec![]);
    if let ThreadUpdate::Snapshot { projection, .. } = &mut other {
        projection["thread"]["projectId"] = json!("wrong");
    }
    assert!(p.apply(&[other]).is_err());
    assert_eq!(p.keys().len(), 1);
}
#[test]
fn unknown_items_and_events_are_skipped_without_hiding_known_activity() {
    let mut future = message("future", 2, "future_widget");
    future.as_object_mut().unwrap().remove("ordinal");
    let mut p = model(vec![
        message("known", 1, "assistant_message"),
        future.clone(),
    ]);
    assert_eq!(p.keys().len(), 1);
    p.apply(&[
        update("future.event", Value::Null),
        update("turn-item.updated", future),
    ])
    .unwrap();
    assert!(!p.take_changes().changed());
    p.apply(&[update(
        "turn-item.updated",
        message("new", 3, "command_execution"),
    )])
    .unwrap();
    assert!(p.take_changes().changed());
}
#[test]
fn run_visibility_preserves_ancestors_and_hides_cancelled_queued_or_rolled_back_rows() {
    let mut queued = message("queued", 2, "user_message");
    queued["inputIntent"] = json!("queued_turn");
    let mut p = model(vec![message("body", 1, "assistant_message"), queued]);
    p.apply(&[update(
        "run.updated",
        json!({"id":"run","status":"cancelled"}),
    )])
    .unwrap();
    assert!(p.row(&row_key("queued")).is_none());
    assert!(p.row(&row_key("body")).is_some());
    p.apply(&[update(
        "run.updated",
        json!({"id":"run","status":"rolled_back"}),
    )])
    .unwrap();
    assert!(p.keys().is_empty());
}
#[test]
fn superseded_plain_steer_interrupts_are_hidden_but_paired_stop_requests_survive() {
    let result = message("stop-result", 1, "run_interrupt_result");
    let mut p = model(vec![result.clone()]);
    p.apply(&[update(
        "run-attempt.updated",
        json!({"id":"attempt","runId":"run","rootNodeId":"node","status":"superseded"}),
    )])
    .unwrap();
    assert!(p.keys().is_empty());
    p.apply(&[update(
        "turn-item.updated",
        message("stop-request", 2, "run_interrupt_request"),
    )])
    .unwrap();
    p.apply(&[update("turn-item.updated", result)]).unwrap();
    assert!(p.row(&row_key("stop-result")).is_some());
}
#[test]
fn bounded_window_ignores_old_unloaded_rows_but_keeps_unfinished_queued_runs() {
    let mut s = snapshot(vec![message("tail", 100, "assistant_message")]);
    if let ThreadUpdate::Snapshot {
        has_more_history,
        history_cursor,
        latest_local_turn_ordinal,
        ..
    } = &mut s
    {
        *has_more_history = true;
        *history_cursor = Some("opaque-cursor".into());
        *latest_local_turn_ordinal = Some(100);
    }
    let mut p = Projection::new("thread", "project");
    p.apply(&[s]).unwrap();
    p.take_changes();
    p.apply(&[update(
        "turn-item.updated",
        message("old", 5, "assistant_message"),
    )])
    .unwrap();
    assert!(p.row(&row_key("old")).is_none());
    p.apply(&[update(
        "run.updated",
        json!({"id":"queued-run","status":"queued"}),
    )])
    .unwrap();
    let mut queued = message("queued", 4, "user_message");
    queued["runId"] = json!("queued-run");
    p.apply(&[update("turn-item.updated", queued)]).unwrap();
    assert!(p.row(&row_key("queued")).is_some());
}
#[test]
fn history_merge_preserves_live_text_deduplicates_and_rejects_a_stale_cursor() {
    let mut p = model(vec![message("tail", 10, "assistant_message")]);
    p.metadata.has_more_history = true;
    p.metadata.history_cursor = Some("cursor1".into());
    let request = p.history_request().unwrap();
    let mut latest = message("tail", 10, "assistant_message");
    latest["text"] = json!("streamed while loading");
    p.apply(&[update("turn-item.updated", latest)]).unwrap();
    p.take_changes();
    let row = |item: Value| json!({"visibility":"local","sourceThreadId":"thread","sourceItemId":item["id"],"item":item});
    p.apply_history(
        &request,
        HistoryPage {
            items: vec![
                row(message("older", 1, "assistant_message")),
                row(message("tail", 10, "assistant_message")),
            ],
            next_cursor: None,
            has_more_history: false,
        },
    )
    .unwrap();
    assert_eq!(p.keys(), &[row_key("older"), row_key("tail")]);
    assert_eq!(
        p.row(&row_key("tail")).unwrap().item["text"],
        "streamed while loading"
    );
    p.take_changes();
    p.apply_history(
        &request,
        HistoryPage {
            items: vec![row(message("stale", 0, "assistant_message"))],
            next_cursor: None,
            has_more_history: false,
        },
    )
    .unwrap();
    assert!(!p.take_changes().changed());
}
#[test]
fn reasoning_and_tools_retain_full_detail_separately_from_the_preview() {
    assert!(matches!(
        presentation::body(&message("a", 1, "reasoning")),
        presentation::Body::Reasoning(_)
    ));
    let mut command = message("cmd", 2, "command_execution");
    command["input"] = json!(format!("cargo test {}\nsecret output", "x".repeat(500)));
    let presentation::Body::Compact { label, detail } = presentation::body(&command) else {
        panic!("not compact")
    };
    assert!(label.starts_with("Command"));
    assert!(detail.contains("secret output"));
    assert!(detail.chars().count() > 500);
    assert!(presentation::preview(&command).starts_with("cargo test"));
    assert!(!presentation::preview(&command).starts_with("## Input"));
}

#[test]
fn replacement_snapshot_invalidates_an_outstanding_page_even_if_its_cursor_matches() {
    let mut p = model(vec![message("tail", 10, "assistant_message")]);
    p.metadata.has_more_history = true;
    p.metadata.history_cursor = Some("same".into());
    let request = p.history_request().unwrap();
    let mut next = snapshot(vec![]);
    if let ThreadUpdate::Snapshot {
        history_cursor,
        has_more_history,
        ..
    } = &mut next
    {
        *history_cursor = Some("same".into());
        *has_more_history = true;
    }
    p.apply(&[next]).unwrap();
    p.take_changes();
    p.apply_history(
        &request,
        HistoryPage {
            items: vec![],
            next_cursor: None,
            has_more_history: false,
        },
    )
    .unwrap();
    assert!(p.metadata.has_more_history);
    assert!(!p.take_changes().changed());
}

#[test]
fn deleted_thread_clears_history_and_later_events_cannot_resurrect_it() {
    let mut p = model(vec![message("body", 1, "assistant_message")]);
    p.apply(&[update(
        "thread.deleted",
        json!({"id":"thread","projectId":"project","title":"Manager"}),
    )])
    .unwrap();
    assert!(p.metadata.deleted && p.keys().is_empty());
    assert!(p.take_changes().changed());
    p.apply(&[update(
        "turn-item.updated",
        message("body", 1, "assistant_message"),
    )])
    .unwrap();
    assert!(!p.take_changes().changed());
    assert!(p.keys().is_empty());
}

#[test]
fn action_controls_follow_live_requests_and_capabilities_and_never_use_inherited_cards() {
    use crate::actions::{DispatchMode, Intent};
    let mut approval = message("approval", 1, "approval_request");
    approval["requestId"] = json!("request");
    approval["options"] = json!([{"decision":"accept","label":"Allow once"},{"decision":"decline","label":"Decline"}]);
    let mut initial = snapshot(vec![approval]);
    if let ThreadUpdate::Snapshot { projection, .. } = &mut initial {
        projection["runtimeRequests"] =
            json!([{"id":"request","status":"pending","responseCapability":{"type":"live"}}]);
        projection["runs"] =
            json!([{"id":"run","ordinal":1,"status":"running","providerThreadId":"pt"}]);
        projection["providerThreads"] = json!([{"id":"pt","providerSessionId":"session"}]);
        projection["providerSessions"] =
            json!([{"id":"session","capabilities":{"turns":{"supportsActiveSteering":true}}}]);
    }
    let mut p = Projection::new("thread", "project");
    p.apply(&[initial]).unwrap();
    let state = p.take_changes().actions.unwrap();
    assert!(state.ready);
    assert!(!state.allows(&Intent::message(
        "hello".into(),
        DispatchMode::StartImmediately
    )));
    assert!(state.allows(&Intent::message(
        "hello".into(),
        DispatchMode::QueueAfterActive
    )));
    assert!(state.allows(&Intent::message(
        "hello".into(),
        DispatchMode::SteerActive {
            target_run_id: "run".into()
        }
    )));
    assert!(!state.allows(&Intent::message(
        "hello".into(),
        DispatchMode::SteerActive {
            target_run_id: "old-run".into()
        }
    )));
    assert!(state.allows(&Intent::Respond {
        request_id: "request".into(),
        decision: Some("accept".into()),
        answers: None
    }));
    assert!(!state.allows(&Intent::Respond {
        request_id: "request".into(),
        decision: Some("acceptAlways".into()),
        answers: None
    }));
    p.apply(&[
        update(
            "runtime-request.updated",
            json!({"id":"request","status":"resolved","responseCapability":{"type":"live"}}),
        ),
        update(
            "run.updated",
            json!({"id":"run","ordinal":1,"status":"completed"}),
        ),
    ])
    .unwrap();
    let state = p.take_changes().actions.unwrap();
    assert!(state.requests.is_empty());
    assert!(state.active_run.is_none());
    assert!(state.allows(&Intent::message(
        "hello".into(),
        DispatchMode::StartImmediately
    )));
    p.apply(&[update(
        "thread.deleted",
        json!({"id":"thread","projectId":"project"}),
    )])
    .unwrap();
    assert!(!p.take_changes().actions.unwrap().ready);
}
#[test]
fn question_answers_preserve_option_values_multi_select_and_message_dismissal() {
    use crate::actions::{DispatchMode, Intent};
    let mut question = message("q", 1, "user_input_request");
    question["requestId"] = json!("request");
    question["questions"] = json!([{"id":"one","question":"Choose","allowCustomAnswer":false,"options":[{"label":"Label","value":" value "}]},{"id":"many","question":"Choose several","multiSelect":true,"allowCustomAnswer":false,"options":[{"label":"A"},{"label":"B"}]}]);
    let mut initial = snapshot(vec![question]);
    if let ThreadUpdate::Snapshot { projection, .. } = &mut initial {
        projection["runtimeRequests"] =
            json!([{"id":"request","status":"pending","responseCapability":{"type":"message"}}]);
        projection["providerThreads"] = json!([]);
        projection["providerSessions"] = json!([]);
    }
    let mut p = Projection::new("thread", "project");
    p.apply(&[initial]).unwrap();
    let state = p.take_changes().actions.unwrap();
    let answers = std::collections::BTreeMap::from([
        ("one".into(), json!(" value ")),
        ("many".into(), json!(["A", "B"])),
    ]);
    assert!(state.allows(&Intent::Respond {
        request_id: "request".into(),
        decision: None,
        answers: Some(answers.clone())
    }));
    let mut invalid = answers;
    invalid.insert("one".into(), json!("value"));
    assert!(!state.allows(&Intent::Respond {
        request_id: "request".into(),
        decision: None,
        answers: Some(invalid)
    }));
    assert!(state.allows(&Intent::Dismiss {
        request_id: "request".into()
    }));
    assert!(state.allows(&Intent::message(
        "hello".into(),
        DispatchMode::StartImmediately
    )));
}
