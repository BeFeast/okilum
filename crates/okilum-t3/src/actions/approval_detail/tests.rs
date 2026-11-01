use super::*;
use crate::{
    actions::{Intent, State},
    projection::{HistoryPage, Projection},
    stream::ThreadUpdate,
};
use serde_json::json;
use std::sync::Arc;

struct Fixture {
    card: Value,
    command: Value,
    runtime: Value,
    input: String,
}

impl Fixture {
    fn new() -> Self {
        // Same native shape as the QA2199 approval: the card's prompt is only
        // justification, and the execution owns the complete multiline input.
        // IDs are deliberately opaque; neither node nor ordinal is a join key.
        let reference = json!({"driver":"codex","nativeId":"exec-66a59fe5-92ef-4c6f-a295-f82d04377510","strength":"strong"});
        let lines = (1..=20)
            .map(|line| format!("'QA2199_APPROVAL_LINE_{line:02}' \\\n"))
            .collect::<String>();
        let input = format!("  /bin/bash -lc \"printf '%s\\n' {lines}'QA2199_LONG_LINE_{}_HORIZONTAL_END'\n# QA2199_VERTICAL_END\"\n", "X".repeat(1000));
        Self {
            card: json!({
                "id":"opaque-approval-row","threadId":"thread","runId":"run",
                "nodeId":"opaque-request-node","providerThreadId":"provider-thread","providerTurnId":"provider-turn",
                "nativeItemRef":reference,"ordinal":13000005,"type":"approval_request","status":"waiting",
                "requestId":"opaque-runtime-request","requestKind":"command",
                "prompt":"QA2199 harmless stdout-only command detail control; await approval or decline",
                "futureProviderField":{"preserve":"verbatim"}
            }),
            command: json!({
                "id":"opaque-execution-row","threadId":"thread","runId":"run",
                "nodeId":"different-command-node","providerThreadId":"provider-thread","providerTurnId":"provider-turn",
                "nativeItemRef":reference,"ordinal":13000004,"type":"command_execution","status":"running","input":input
            }),
            runtime: json!({
                "id":"opaque-runtime-request","nodeId":"opaque-request-node","providerTurnId":"provider-turn",
                "nativeRequestRef":reference,"kind":"command","status":"pending",
                "responseCapability":{"type":"live","providerSessionId":"session"}
            }),
            input,
        }
    }

    fn snapshot(&self, commands: Vec<Value>) -> ThreadUpdate {
        let mut canonical = commands;
        canonical.push(self.card.clone());
        ThreadUpdate::Snapshot {
            sequence: 1,
            projection: json!({
                "thread":{"id":"thread","projectId":"project"},
                "runs":[{"id":"run","ordinal":13,"status":"waiting"}],"attempts":[],
                "runtimeRequests":[self.runtime],"providerThreads":[],"providerSessions":[],
                "turnItems":canonical,
                // Canonical proof need not be part of the visible history window.
                "visibleTurnItems":[{"visibility":"local","sourceThreadId":"thread","sourceItemId":self.card["id"],"item":self.card}]
            }),
            history_cursor: Some("older".into()),
            has_more_history: true,
            latest_local_turn_ordinal: None,
        }
    }

    fn model(&self, commands: Vec<Value>) -> Projection {
        let mut projection = Projection::new("thread", "project");
        projection.apply(&[self.snapshot(commands)]).unwrap();
        projection.take_changes();
        projection
    }
}

fn approve() -> Intent {
    Intent::Respond {
        request_id: "opaque-runtime-request".into(),
        decision: Some("accept".into()),
        answers: None,
        attachments_by_question_id: Default::default(),
    }
}

fn no_detail_with_unchanged_authority(projection: &Projection, card: &Value) {
    let state = projection.action_state();
    assert_eq!(state.requests.len(), 1);
    assert_eq!(state.requests[0].item, *card);
    assert_eq!(state.requests[0].approval_detail, None);
    assert!(
        state.allows(&approve()),
        "missing display context cannot change approval authority"
    );
}

fn event(item: Value) -> ThreadUpdate {
    ThreadUpdate::Event {
        sequence: 2,
        event: json!({"type":"turn-item.updated","threadId":"thread","payload":item}),
    }
}

#[test]
fn native_command_approval_uses_the_full_unique_canonical_input_and_preserves_the_source_card() {
    let fixture = Fixture::new();
    assert_eq!(fixture.card["prompt"].as_str().unwrap().len(), 77);
    let projection = fixture.model(vec![fixture.command.clone()]);
    let state = projection.action_state();
    assert_eq!(
        state.requests[0].approval_detail.as_deref(),
        Some(fixture.input.as_str())
    );
    assert_eq!(state.requests[0].item, fixture.card);
    assert_ne!(state.requests[0].item["prompt"], fixture.input);
    assert!(state.allows(&approve()));
    assert_eq!(state.requests[0].item["options"], Value::Null);
}

#[test]
fn foreign_command_scopes_never_supply_approval_detail() {
    let fixture = Fixture::new();
    for field in ["threadId", "runId", "providerThreadId", "providerTurnId"] {
        let mut projection = fixture.model(vec![]);
        let mut command = fixture.command.clone();
        command[field] = json!("foreign");
        // The projection parser independently rejects foreign threads. Exercise
        // the display join's defense as well, without weakening that parser.
        projection
            .items
            .insert("opaque-execution-row".into(), Arc::new(command));
        no_detail_with_unchanged_authority(&projection, &fixture.card);
    }
    for field in ["runId", "providerThreadId", "providerTurnId"] {
        let mut changed = Fixture::new();
        changed.card.as_object_mut().unwrap().remove(field);
        no_detail_with_unchanged_authority(
            &changed.model(vec![changed.command.clone()]),
            &changed.card,
        );
    }
}

#[test]
fn weak_missing_or_foreign_native_references_never_join_by_node_ordinal_or_encoded_ids() {
    for role in ["card", "execution"] {
        for invalid in [
            Value::Null,
            json!({}),
            json!({"driver":"codex","nativeId":"exec-66a59fe5-92ef-4c6f-a295-f82d04377510","strength":"weak"}),
            json!({"driver":"","nativeId":"issued","strength":"strong"}),
            json!({"driver":"codex","nativeId":" \n","strength":"strong"}),
        ] {
            let mut fixture = Fixture::new();
            if role == "card" {
                fixture.card["nativeItemRef"] = invalid;
            } else {
                fixture.command["nativeItemRef"] = invalid;
            }
            no_detail_with_unchanged_authority(
                &fixture.model(vec![fixture.command.clone()]),
                &fixture.card,
            );
        }
    }
    for field in ["driver", "nativeId"] {
        let mut fixture = Fixture::new();
        fixture.command["nativeItemRef"][field] = json!("foreign");
        no_detail_with_unchanged_authority(
            &fixture.model(vec![fixture.command.clone()]),
            &fixture.card,
        );
    }
    let mut fixture = Fixture::new();
    fixture.command["ordinal"] = json!(1);
    fixture.card["ordinal"] = json!(99_999_999);
    let projection = fixture.model(vec![fixture.command.clone()]);
    assert_eq!(
        projection.action_state().requests[0]
            .approval_detail
            .as_deref(),
        Some(fixture.input.as_str()),
        "exact identity does not depend on ordinal proximity or matching node IDs"
    );
}

#[test]
fn runtime_reference_agreement_is_checked_when_available_and_absence_is_not_guessed() {
    for invalid in [
        json!({"driver":"other","nativeId":"exec-66a59fe5-92ef-4c6f-a295-f82d04377510","strength":"strong"}),
        json!({"driver":"codex","nativeId":"other","strength":"strong"}),
        json!({"driver":"codex","nativeId":"exec-66a59fe5-92ef-4c6f-a295-f82d04377510","strength":"weak"}),
        json!(false),
    ] {
        let mut fixture = Fixture::new();
        fixture.runtime["nativeRequestRef"] = invalid;
        no_detail_with_unchanged_authority(
            &fixture.model(vec![fixture.command.clone()]),
            &fixture.card,
        );
    }
    for field in ["providerTurnId", "providerThreadId"] {
        let mut fixture = Fixture::new();
        fixture.runtime[field] = json!("foreign");
        no_detail_with_unchanged_authority(
            &fixture.model(vec![fixture.command.clone()]),
            &fixture.card,
        );
    }
    for absent in [true, false] {
        let mut fixture = Fixture::new();
        if absent {
            fixture
                .runtime
                .as_object_mut()
                .unwrap()
                .remove("nativeRequestRef");
        } else {
            fixture.runtime["nativeRequestRef"] = Value::Null;
        }
        assert_eq!(
            fixture
                .model(vec![fixture.command.clone()])
                .action_state()
                .requests[0]
                .approval_detail
                .as_deref(),
            Some(fixture.input.as_str())
        );
    }
}

#[test]
fn missing_or_ambiguous_canonical_commands_never_choose_the_better_text_or_nearer_ordinal() {
    let fixture = Fixture::new();
    no_detail_with_unchanged_authority(&fixture.model(vec![]), &fixture.card);
    for duplicate_input in [fixture.input.as_str(), "a misleading nearer command"] {
        let mut duplicate = fixture.command.clone();
        duplicate["id"] = json!("second-execution-row");
        duplicate["ordinal"] = json!(13000005);
        duplicate["input"] = json!(duplicate_input);
        no_detail_with_unchanged_authority(
            &fixture.model(vec![fixture.command.clone(), duplicate]),
            &fixture.card,
        );
    }
    for input in [
        Value::Null,
        json!(""),
        json!(" \n\t"),
        json!(["not a string"]),
    ] {
        let mut command = fixture.command.clone();
        command["input"] = input;
        no_detail_with_unchanged_authority(&fixture.model(vec![command]), &fixture.card);
    }
    let mut missing = fixture.command.clone();
    missing.as_object_mut().unwrap().remove("input");
    no_detail_with_unchanged_authority(&fixture.model(vec![missing]), &fixture.card);
}

#[test]
fn reused_native_ids_in_foreign_scopes_do_not_mask_the_unique_exact_current_command() {
    let fixture = Fixture::new();
    for field in ["threadId", "runId", "providerThreadId", "providerTurnId"] {
        let mut projection = fixture.model(vec![fixture.command.clone()]);
        let mut foreign = fixture.command.clone();
        foreign["id"] = json!("another-scoped-execution");
        foreign[field] = json!("foreign");
        foreign["input"] = json!("this command belongs to another scope");
        // Snapshot parsing also rejects foreign canonical threads; keeping one
        // here directly verifies that only a full key match participates.
        projection
            .items
            .insert("another-scoped-execution".into(), Arc::new(foreign));
        let state = projection.action_state();
        assert_eq!(
            state.requests[0].approval_detail.as_deref(),
            Some(fixture.input.as_str()),
            "a reused ID in another {field} cannot mask the current exact context"
        );
        assert_eq!(state.requests[0].item, fixture.card);
        assert!(state.allows(&approve()));
    }
}

#[test]
fn command_events_publish_derived_detail_changes_without_invalidating_actions_for_stdout_only() {
    let fixture = Fixture::new();
    let mut projection = fixture.model(vec![]);
    projection.apply(&[event(fixture.command.clone())]).unwrap();
    let initial: State = projection
        .take_changes()
        .actions
        .expect("command arrival publishes current approval context");
    assert_eq!(
        initial.requests[0].approval_detail.as_deref(),
        Some(fixture.input.as_str())
    );
    let mut command = fixture.command.clone();
    command["output"] = json!("ordinary stdout update");
    projection.apply(&[event(command.clone())]).unwrap();
    assert!(
        projection.take_changes().actions.is_none(),
        "stdout alone does not change approval display context"
    );
    command["input"] = json!(" exact changed command \n");
    projection.apply(&[event(command.clone())]).unwrap();
    assert_eq!(
        projection.take_changes().actions.unwrap().requests[0]
            .approval_detail
            .as_deref(),
        Some(" exact changed command \n")
    );
    let mut duplicate = command.clone();
    duplicate["id"] = json!("duplicate-row");
    projection.apply(&[event(duplicate.clone())]).unwrap();
    assert_eq!(
        projection.take_changes().actions.unwrap().requests[0].approval_detail,
        None
    );
    duplicate["type"] = json!("system_notice");
    projection.apply(&[event(duplicate)]).unwrap();
    let restored = projection.take_changes().actions.unwrap();
    assert_eq!(
        restored.requests[0].approval_detail.as_deref(),
        Some(" exact changed command \n")
    );
    let mut snapshot_model = Fixture::new();
    snapshot_model.command = command;
    assert_eq!(
        restored,
        snapshot_model
            .model(vec![snapshot_model.command.clone()])
            .action_state()
    );
    projection.apply(&[fixture.snapshot(vec![])]).unwrap();
    assert_eq!(
        projection.take_changes().actions.unwrap().requests[0].approval_detail,
        None
    );
}

#[test]
fn late_local_history_context_publishes_detail_but_inherited_history_does_not_grant_it() {
    for local in [true, false] {
        let fixture = Fixture::new();
        let mut projection = fixture.model(vec![]);
        let request = projection.history_request().unwrap();
        let mut command = fixture.command.clone();
        if !local {
            command["threadId"] = json!("ancestor-thread");
        }
        let source_thread = command["threadId"].clone();
        let source_item = command["id"].clone();
        projection.apply_history(&request, HistoryPage {items:vec![json!({"visibility":if local {"local"} else {"inherited"},"sourceThreadId":source_thread,"sourceItemId":source_item,"item":command})],next_cursor:None,has_more_history:false}).unwrap();
        let changed = projection.take_changes();
        if local {
            assert_eq!(
                changed.actions.unwrap().requests[0]
                    .approval_detail
                    .as_deref(),
                Some(fixture.input.as_str())
            );
        } else {
            assert!(changed.actions.is_none());
            no_detail_with_unchanged_authority(&projection, &fixture.card);
        }
    }
}
