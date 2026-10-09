//! Per-turn request context. Expansion reads a retained receipt, never live sources.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct TurnKey {
    goal: String,
    conversation: String,
    turn: String,
    message_index: usize,
}

#[derive(Default)]
pub(super) struct DiscussionContextUi {
    expanded: Option<TurnKey>,
    sequence: u64,
    loading: bool,
    snapshot: Option<Value>,
    error: Option<String>,
}

fn summary_for(conversation: &Value, index: usize) -> Option<&Value> {
    let mut matches = conversation["turn_contexts"]
        .as_array()?
        .iter()
        .filter(|entry| entry["user_message_index"].as_u64() == Some(index as u64));
    let entry = matches.next()?;
    matches.next().is_none().then_some(entry)
}

fn available_key(conversation: &Value, index: usize) -> Option<TurnKey> {
    if conversation["messages"].get(index)?["role"] != "user" {
        return None;
    }
    let summary = summary_for(conversation, index)?;
    let turn = summary["turn_id"].as_str()?;
    if summary["availability"] != "available" || Uuid::parse_str(turn).is_err() {
        return None;
    }
    for count in ["saved_input_count", "manual_input_count", "omission_count"] {
        summary[count].as_u64()?;
    }
    Some(TurnKey {
        goal: conversation["goal_id"].as_str()?.into(),
        conversation: conversation["id"].as_str()?.into(),
        turn: turn.into(),
        message_index: index,
    })
}

fn unavailable_reason(reason: &Value) -> &'static str {
    match reason.as_str() {
        Some("historical_context_unavailable") => "Context was not recorded for this message.",
        None => "Context is unavailable for this message.",
        Some("context_version_unsupported") => "This context requires a newer version of Okilum.",
        Some("context_source_unavailable") => "The recorded context could not be read.",
        Some("context_source_oversized") => "The recorded context exceeds the supported size.",
        _ => "The recorded context could not be validated for this message.",
    }
}

fn row_unavailable_reason(conversation: &Value, index: usize, supported: bool) -> &'static str {
    if !supported {
        return unavailable_reason(&Value::Null);
    }
    if let Some(summary) = summary_for(conversation, index) {
        return unavailable_reason(&summary["reason"]);
    }
    // A malformed/duplicate summary is not an absent historical receipt.
    let Some(summaries) = conversation["turn_contexts"].as_array() else {
        return unavailable_reason(&Value::Null);
    };
    if summaries
        .iter()
        .any(|summary| summary["user_message_index"].as_u64() == Some(index as u64))
    {
        return unavailable_reason(&json!("context_invalid"));
    }
    let history = &conversation["context_history"];
    if conversation["messages"]
        .get(index)
        .is_some_and(|message| message["role"] == "user")
        && history["unrecorded_turn_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
        && history["unrecorded_reason"].is_string()
    {
        unavailable_reason(&history["unrecorded_reason"])
    } else {
        unavailable_reason(&Value::Null)
    }
}

fn matching_snapshot(response: &Value, key: &TurnKey, brain: &Value) -> Result<Value, String> {
    if response["id"] != key.conversation || response["goal_id"] != key.goal {
        return Err("Context does not belong to this conversation.".into());
    }
    let wrapper = &response["context_snapshot"];
    if wrapper["availability"] == "unavailable" {
        return Err(unavailable_reason(&wrapper["reason"]).into());
    }
    let snapshot = &wrapper["snapshot"];
    if wrapper["availability"] != "available"
        || snapshot["schema"] != "okilum-discussion-turn/v1"
        || snapshot["conversation_id"] != key.conversation
        || snapshot["goal"]["id"] != key.goal
        || snapshot["turn_id"] != key.turn
        || snapshot["user_message_index"].as_u64() != Some(key.message_index as u64)
        || (!brain.is_null() && snapshot["brain_id"] != *brain)
        || !snapshot["inputs"].is_array()
        || !snapshot["remaining_criteria"].is_array()
        || !snapshot["goal"]["criteria"].is_array()
        || !snapshot["constraints"].is_array()
        || !snapshot["omissions"]["items"].is_array()
        || !snapshot["omissions"]["counts"].is_object()
        || snapshot["omissions"]["total"].as_u64().is_none()
    {
        return Err("The recorded context could not be validated for this message.".into());
    }
    Ok(snapshot.clone())
}

fn omission_reason(code: &str) -> &'static str {
    match code {
        "source_unavailable_or_oversized" | "source_unavailable_oversized_or_invalid" => {
            "Source is missing, too large, unreadable, or invalid"
        }
        "invalid_record" => "Source metadata could not be read",
        "unknown_owner" => "Source ownership could not be established",
        "latest_stage_unavailable_or_invalid" => "The latest stage could not be read",
        "latest_result_unavailable_oversized_or_invalid" => {
            "The latest outcome could not be included or validated"
        }
        "criterion_evidence_unavailable" => "Completion evidence is unavailable",
        "brief_input_budget" => "Source exceeds the saved-context size limit",
        "brief_inventory_limit" => "Saved-input inventory exceeds the supported limit",
        _ => "Context could not be included",
    }
}

fn escaped(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        if "\\`*_{}[]<>()#+-.!|".contains(character) {
            result.push('\\');
        }
        result.push(character);
    }
    result
}

fn known(value: &Value) -> String {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(escaped)
        .unwrap_or_else(|| "Unknown".into())
}

// Keep canonical source text literal, including frontmatter and embedded fences.
fn literal(value: &str) -> String {
    let longest = value.split(|c| c != '~').map(str::len).max().unwrap_or(0);
    let fence = "~".repeat(longest.max(2) + 1);
    format!("{fence}text\n{value}\n{fence}\n")
}

fn snapshot_markdown(snapshot: &Value) -> String {
    let mut result = format!(
        "### {}\n\nGoal revision: {}\n\nRequested by {} · {} · Model: {}\n\n",
        known(&snapshot["goal"]["title"]),
        known(&snapshot["goal"]["revision"]),
        known(&snapshot["actor_id"]),
        known(&snapshot["created_at"]),
        known(&snapshot["model"]),
    );
    result.push_str("#### Goal criteria at this request\n\n");
    for criterion in array(&snapshot["goal"]["criteria"]) {
        result.push_str(&format!(
            "- {}{}\n",
            known(&criterion["description"]),
            if criterion["requires_human"] == true {
                " · Requires human evidence"
            } else {
                ""
            }
        ));
    }
    result.push_str("\n#### Remaining criteria\n\n");
    let remaining = array(&snapshot["remaining_criteria"]);
    if remaining.is_empty() {
        result.push_str("None listed in this request. This does not change goal completion.\n");
    }
    for criterion in remaining {
        result.push_str(&format!("- {}\n", known(&criterion["description"])));
    }
    result.push_str("\n#### Constraints and next step\n\n");
    for constraint in array(&snapshot["constraints"]) {
        result.push_str(&format!("- {}\n", known(&constraint)));
    }
    result.push_str(&format!("\n{}\n", known(&snapshot["next_step"])));
    if snapshot["brief"]["availability"] != "available" {
        result.push_str(
            "\nSaved goal inputs were unavailable. Remaining criteria are conservative.\n",
        );
    }
    for input in array(&snapshot["inputs"]) {
        let reasons = array(&input["reasons"])
            .iter()
            .map(|reason| match reason.as_str() {
                Some("manual") => "Selected source",
                Some("saved_decision") => "Saved decision",
                Some("latest_result") => "Latest outcome",
                Some("original_inbox") => "Original Inbox input",
                _ => "Unknown inclusion reason",
            })
            .collect::<Vec<_>>()
            .join(" · ");
        result.push_str(&format!(
            "\n#### {}\n\n{} · Verification: {}\n\nSource: {} · Revision: {} · {}\n\nActor: {} · Received: {}\n\n",
            known(&input["title"]), escaped(&reasons), known(&input["verification"]),
            known(&input["path"]), known(&input["revision"]), known(&input["locator"]),
            known(&input["actor_id"]), known(&input["received_at"]),
        ));
        if input["owner_goal_id"].is_string() && input["owner_goal_id"] != snapshot["goal"]["id"] {
            result.push_str(&format!(
                "Explicit reference from another goal: {}\n\n",
                known(&input["owner_goal_id"])
            ));
        }
        if input["source_identity"].is_object() {
            result.push_str("Source identity:\n\n");
            result.push_str(&literal(
                &serde_json::to_string_pretty(&input["source_identity"]).unwrap_or_default(),
            ));
        }
        result.push_str(&literal(
            input["text"].as_str().unwrap_or("Source text unavailable"),
        ));
    }
    let omissions = &snapshot["omissions"];
    if omissions["total"].as_u64().unwrap_or(0) > 0 {
        result.push_str(&format!(
            "\n#### Omitted context · {}\n\n",
            omissions["total"]
        ));
        for omission in array(&omissions["items"]) {
            result.push_str(&format!(
                "- {}: {}\n",
                known(&omission["path"]),
                omission_reason(omission["code"].as_str().unwrap_or(""))
            ));
        }
        if omissions["details_complete"] != true {
            result.push_str("\nOnly some paths are listed. Full counts:\n\n");
            if let Some(counts) = omissions["counts"].as_object() {
                for (code, count) in counts {
                    result.push_str(&format!("- {}: {}\n", omission_reason(code), count));
                }
            }
        }
    }
    result
}

impl BrainView {
    pub(super) fn reset_discussion_context(&mut self) {
        self.discussion_context = DiscussionContextUi {
            sequence: self.discussion_context.sequence + 1,
            ..DiscussionContextUi::default()
        };
    }

    pub(super) fn reconcile_discussion_context(&mut self) {
        let Some(key) = &self.discussion_context.expanded else {
            return;
        };
        if available_key(&self.conversation, key.message_index).as_ref() != Some(key) {
            self.reset_discussion_context();
        }
    }

    fn finish_discussion_context(
        &mut self,
        key: &TurnKey,
        sequence: u64,
        workspace: &Option<Value>,
        response: Result<Value, String>,
    ) -> bool {
        if self.expected_workspace != *workspace
            || self.goal_id() != key.goal
            || self.conversation_id.as_deref() != Some(key.conversation.as_str())
            || self.capabilities["discussion_context"] != true
            || self.discussion_context.sequence != sequence
            || self.discussion_context.expanded.as_ref() != Some(key)
            || available_key(&self.conversation, key.message_index).as_ref() != Some(key)
        {
            return false;
        }
        self.discussion_context.loading = false;
        let brain = workspace
            .as_ref()
            .map(|value| &value["brain_id"])
            .unwrap_or(&Value::Null);
        match response.and_then(|response| matching_snapshot(&response, key, brain)) {
            Ok(snapshot) => self.discussion_context.snapshot = Some(snapshot),
            Err(error) => self.discussion_context.error = Some(error),
        }
        true
    }

    fn toggle_discussion_context(
        &mut self,
        key: TurnKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.discussion_context.sequence += 1;
        if self.discussion_context.expanded.as_ref() == Some(&key) {
            self.discussion_context.expanded = None;
            self.discussion_context.snapshot = None;
            self.discussion_context.loading = false;
            cx.notify();
            return;
        }
        self.discussion_context.expanded = Some(key.clone());
        self.discussion_context.snapshot = None;
        self.discussion_context.error = None;
        self.discussion_context.loading = true;
        let sequence = self.discussion_context.sequence;
        let workspace = self.expected_workspace.clone();
        let endpoint = self.endpoint;
        let request =
            json!({"op":"chat_get","conversation_id":key.conversation,"context_turn_id":key.turn});
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let request_workspace = workspace.clone();
            let response = cx
                .background_executor()
                .spawn(async move { rpc_guarded(endpoint, request, request_workspace.as_ref()) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.finish_discussion_context(&key, sequence, &workspace, response) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn discussion_context_row(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = super::super::brand::palette(cx);
        let mut row = v_flex().gap_2().min_w_0();
        let key = (self.capabilities["discussion_context"] == true)
            .then(|| available_key(&self.conversation, index))
            .flatten();
        let Some(key) = key else {
            return row
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.text_muted)
                        .child(row_unavailable_reason(
                            &self.conversation,
                            index,
                            self.capabilities["discussion_context"] == true,
                        )),
                )
                .into_any_element();
        };
        let summary = summary_for(&self.conversation, index).unwrap();
        let expanded = self.discussion_context.expanded.as_ref() == Some(&key);
        let counts = format!(
            "{} saved inputs · {} selected sources{}",
            summary["saved_input_count"],
            summary["manual_input_count"],
            if summary["omission_count"].as_u64().unwrap_or(0) > 0 {
                format!(" · {} omissions", summary["omission_count"])
            } else {
                String::new()
            }
        );
        row = row.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    super::super::brand::control(("discussion-context-toggle", index), cx)
                        .label(if expanded {
                            "Hide context"
                        } else {
                            "Context for this request"
                        })
                        .small()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_discussion_context(key.clone(), window, cx)
                        })),
                )
                .child(div().text_xs().text_color(colors.text_muted).child(counts)),
        );
        if expanded {
            if self.discussion_context.loading {
                row = row.child(div().text_sm().child("Loading recorded context…"));
            } else if let Some(error) = &self.discussion_context.error {
                row = row.child(div().text_sm().child(error.clone()));
            } else if let Some(snapshot) = &self.discussion_context.snapshot {
                row = row.child(
                    v_flex()
                        .id(("discussion-context-detail", index))
                        .debug_selector(move || format!("discussion-context-detail-{index}"))
                        .min_w_0()
                        .p_3()
                        .gap_2()
                        .border_1()
                        .border_color(colors.border)
                        .rounded(px(8.))
                        .child(
                            TextView::markdown(
                                ("discussion-context-text", index),
                                snapshot_markdown(snapshot),
                            )
                            .selectable(true)
                            .style(super::super::reader_text_style(cx.theme())),
                        ),
                );
            }
        }
        row.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn fixture() -> (Value, Value) {
        let turn = Uuid::new_v4().to_string();
        let conversation = json!({"id":"conversation-a","goal_id":"goal-a","status":"complete",
            "messages":[{"role":"user","text":"What comes next?"},{"role":"assistant","text":"Review the saved outcome."}],
            "turn_contexts":[{"user_message_index":0,"turn_id":turn,"availability":"available","reason":null,
                "saved_input_count":1,"manual_input_count":0,"omission_count":1}]});
        let mut response = conversation.clone();
        response["context_snapshot"] = json!({"availability":"available","reason":null,"snapshot":{
            "schema":"okilum-discussion-turn/v1","brain_id":"brain-a","conversation_id":"conversation-a",
            "turn_id":turn,"user_message_index":0,"actor_id":"Operator","created_at":"2026-09-08T00:00:00Z","model":"fixture-model",
            "goal":{"id":"goal-a","title":"Goal A","revision":"sha256:saved-goal","criteria":[{"id":"c1","description":"Observe the result","requires_human":true}]},
            "constraints":["Keep operator review"],"next_step":"Clarify the next step",
            "brief":{"availability":"available","generation":"saved-generation"},
            "inputs":[{"id":"decision-a","title":"Saved Attention reply","kind":"decision","reasons":["saved_decision"],
                "path":"records/decision-a.md","revision":"sha256:saved-input","locator":"L1-L4",
                "text":"---\nactor: Operator\n---\nRetained 雪 decision\n~~~\n","actor_id":"Operator","received_at":"2026-09-08T00:00:00Z",
                "source_identity":{"channel":"native","actor_id":"Operator"},"owner_goal_id":"goal-a","verification":"unverified"}],
            "remaining_criteria":[{"id":"c1","description":"Observe the result","requires_human":true}],
            "omissions":{"items":[{"path":"records/missing.md","code":"source_unavailable_or_oversized"}],"counts":{"source_unavailable_or_oversized":1},"total":1,"details_complete":true}
        }});
        (conversation, response)
    }

    #[test]
    fn unavailable_or_ambiguous_history_is_never_an_empty_available_context() {
        let (mut conversation, _) = fixture();
        assert!(available_key(&conversation, 0).is_some());
        assert!(available_key(&conversation, 1).is_none());
        conversation["turn_contexts"][0]["availability"] = json!("unavailable");
        conversation["turn_contexts"][0]["saved_input_count"] = Value::Null;
        assert!(available_key(&conversation, 0).is_none());
        assert_eq!(
            unavailable_reason(&json!("historical_context_unavailable")),
            "Context was not recorded for this message."
        );
        assert_ne!(
            unavailable_reason(&json!("context_invalid")),
            unavailable_reason(&Value::Null)
        );
        let (mut conversation, _) = fixture();
        let duplicate = conversation["turn_contexts"][0].clone();
        conversation["turn_contexts"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(available_key(&conversation, 0).is_none());
    }

    #[test]
    fn snapshot_binding_and_exact_provenance_survive_presentation() {
        let (conversation, mut response) = fixture();
        let key = available_key(&conversation, 0).unwrap();
        let snapshot = matching_snapshot(&response, &key, &json!("brain-a")).unwrap();
        let display = snapshot_markdown(&snapshot);
        assert!(display.contains("Verification: unverified"));
        assert!(display.contains("sha256:saved\\-input"));
        assert!(display.contains("Retained 雪 decision\n~~~\n"));
        assert!(display.contains("~~~~text\n---\nactor: Operator"));
        assert!(display.contains("native"));
        assert!(display.contains("Source is missing"));
        assert!(display.contains("Keep operator review"));
        response["context_snapshot"]["snapshot"]["brain_id"] = json!("brain-b");
        assert!(matching_snapshot(&response, &key, &json!("brain-a")).is_err());
        response["context_snapshot"] = json!({"availability":"unavailable","reason":"context_binding_mismatch","snapshot":null});
        assert!(matching_snapshot(&response, &key, &json!("brain-a")).is_err());
        response["context_snapshot"] = json!({"availability":"available","snapshot":{}});
        assert!(matching_snapshot(&response, &key, &json!("brain-a")).is_err());
    }

    #[test]
    fn bounded_history_fallback_requires_support_and_valid_unrecorded_metadata() {
        let (mut conversation, _) = fixture();
        conversation["turn_contexts"] = json!([]);
        conversation["context_history"] =
            json!({"unrecorded_turn_count":1,"unrecorded_reason":"historical_context_unavailable"});
        assert_eq!(
            row_unavailable_reason(&conversation, 0, true),
            "Context was not recorded for this message."
        );
        assert_eq!(
            row_unavailable_reason(&conversation, 0, false),
            "Context is unavailable for this message."
        );
        conversation["context_history"]["unrecorded_reason"] = json!("context_version_unsupported");
        assert_eq!(
            row_unavailable_reason(&conversation, 0, true),
            "This context requires a newer version of Okilum."
        );
        conversation["context_history"]["unrecorded_turn_count"] = json!(0);
        assert_eq!(
            row_unavailable_reason(&conversation, 0, true),
            "Context is unavailable for this message."
        );
        conversation["context_history"]["unrecorded_turn_count"] = json!(1);
        conversation["context_history"]["unrecorded_reason"] =
            json!("historical_context_unavailable");
        conversation["turn_contexts"] = json!([{"user_message_index":0},{"user_message_index":0}]);
        assert_eq!(
            row_unavailable_reason(&conversation, 0, true),
            "The recorded context could not be validated for this message."
        );
        conversation["turn_contexts"] = Value::Null;
        assert_eq!(
            row_unavailable_reason(&conversation, 0, true),
            "Context is unavailable for this message."
        );
    }

    #[gpui::test]
    fn late_context_reply_cannot_cross_workspace_goal_conversation_or_turn(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            let (conversation, response) = fixture();
            let key = available_key(&conversation, 0).unwrap();
            let workspace = Some(json!({"brain_id":"brain-a","root":"/fixture"}));
            view.expected_workspace = workspace.clone();
            view.snapshot = json!({"goal":{"id":"goal-a","title":"Goal A"}});
            view.selected_goal_id = Some("goal-a".into());
            view.conversation_id = Some("conversation-a".into());
            view.conversation = conversation;
            view.capabilities = json!({"discussion_context":true});
            view.discussion_context.expanded = Some(key.clone());
            view.discussion_context.loading = true;
            assert!(view.finish_discussion_context(&key, 0, &workspace, Ok(response.clone())));
            assert_eq!(
                view.discussion_context.snapshot.as_ref(),
                Some(&response["context_snapshot"]["snapshot"])
            );
            view.discussion_context.snapshot = None;
            assert!(!view.finish_discussion_context(&key, 1, &workspace, Ok(response.clone())));
            let other_workspace = Some(json!({"brain_id":"brain-a","root":"/other-fixture"}));
            assert!(!view.finish_discussion_context(
                &key,
                0,
                &other_workspace,
                Ok(response.clone())
            ));
            view.snapshot["goal"]["id"] = json!("goal-b");
            assert!(!view.finish_discussion_context(&key, 0, &workspace, Ok(response.clone())));
            view.snapshot["goal"]["id"] = json!("goal-a");
            view.conversation_id = Some("conversation-b".into());
            assert!(!view.finish_discussion_context(&key, 0, &workspace, Ok(response.clone())));
            view.conversation_id = Some("conversation-a".into());
            view.conversation["turn_contexts"][0]["turn_id"] = json!(Uuid::new_v4().to_string());
            assert!(!view.finish_discussion_context(&key, 0, &workspace, Ok(response.clone())));
            assert!(view.discussion_context.snapshot.is_none());
            view.reconcile_discussion_context();
            assert!(view.discussion_context.expanded.is_none());
            assert!(!view.discussion_context.loading);
            view
        });
    }

    #[gpui::test]
    fn expanded_context_keeps_retained_composer_below_transcript(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            let (conversation, _) = fixture();
            view.snapshot = json!({"goal":{"id":"goal-a","title":"Goal A","status":"active"},"goals":[{"id":"goal-a","title":"Goal A","status":"active"}]});
            view.selected_goal_id = Some("goal-a".into());
            view.conversation_id = Some("conversation-a".into());
            view.conversation = conversation;
            view.capabilities = json!({"discussion_context":true});
            view.compose.update(cx, |input, cx| input.set_value("Unsent personal question", window, cx));
            view
        });
        cx.run_until_parked();
        let composer = cx.debug_bounds("brain-composer").unwrap();
        view.update(cx, |view, cx| {
            let (_, response) = fixture();
            let mut snapshot = response["context_snapshot"]["snapshot"].clone();
            snapshot["inputs"][0]["text"] = json!("Long retained source 雪\n".repeat(100));
            view.discussion_context.expanded = available_key(&view.conversation, 0);
            view.discussion_context.snapshot = Some(snapshot);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("discussion-context-detail-0").is_some());
        assert_eq!(cx.debug_bounds("brain-composer"), Some(composer));
        assert!(cx.debug_bounds("brain-transcript").unwrap().bottom() <= composer.top());
        view.update(cx, |view, cx| {
            assert_eq!(
                view.compose.read(cx).value().as_ref(),
                "Unsent personal question"
            )
        });
    }
}
