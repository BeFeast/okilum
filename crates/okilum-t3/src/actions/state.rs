//! Command availability derives only from the bound live projection. Inherited
//! history, dismissed/resolved requests and unknown capabilities never grant controls.
use super::*;
use crate::projection::Projection;

/// T3's ordered approval choices when the provider omits `options` entirely.
/// Explicit provider options remain authoritative, including an empty list.
pub const APPROVAL_FALLBACK_OPTIONS: [(&str, &str); 4] = [
    ("cancel", "Cancel"),
    ("decline", "Decline"),
    ("acceptForSession", "Always allow this session"),
    ("accept", "Approve"),
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub ready: bool,
    pub active_run: Option<String>,
    pub can_steer: bool,
    pub requests: Vec<Request>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: String,
    pub item: Value,
    pub answerable: bool,
    pub dismissible: bool,
    /// Runtime response capability, retained independently of card presentation.
    pub response_mode: Option<String>,
    pub reason: Option<String>,
}
impl Projection {
    pub fn action_state(&self) -> State {
        let active = self
            .runs
            .values()
            .filter(|r| {
                matches!(
                    r["status"].as_str(),
                    Some("preparing" | "starting" | "running" | "waiting")
                )
            })
            .max_by_key(|r| r["ordinal"].as_u64().unwrap_or(0));
        let session = active
            .and_then(|run| run["providerThreadId"].as_str())
            .and_then(|id| self.provider_threads.get(id))
            .and_then(|thread| thread["providerSessionId"].as_str())
            .and_then(|id| self.provider_sessions.get(id));
        let mut requests: Vec<_> =
            self.requests
                .values()
                .filter(|r| r["status"] == "pending")
                .filter_map(|r| {
                    let id = r["id"].as_str().filter(|id| !id.trim().is_empty())?;
                    let kind = r["kind"].as_str()?;
                    let expected = match kind {
                        "user_input" => "user_input_request",
                        "command" | "file-read" | "file-change" | "mcp-elicitation"
                        | "permission" => "approval_request",
                        _ => return None,
                    };
                    // More than one card with this request identity is ambiguous,
                    // including cards of conflicting types. Never pick a winner.
                    let mut candidates = self.items.values().filter(|item| item["requestId"] == id);
                    let item = candidates.next()?;
                    if candidates.next().is_some()
                        || item["type"] != expected
                        || (expected == "approval_request"
                            && item
                                .get("requestKind")
                                .is_some_and(|request_kind| request_kind != kind))
                        || ["threadId", "runId", "nodeId"].iter().any(|field| {
                            r.get(*field)
                                .filter(|value| !value.is_null())
                                .is_some_and(|value| item.get(*field) != Some(value))
                        })
                    {
                        return None;
                    }
                    let mode = r["responseCapability"]["type"].as_str();
                    let message = mode == Some("message");
                    let mismatch = item["responseMode"] == "message" && !message;
                    let answerable = !mismatch
                        && match expected {
                            "approval_request" => mode == Some("live"),
                            _ => matches!(mode, Some("live" | "message")),
                        };
                    let reason = r["responseCapability"]["reason"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| {
                            (!answerable).then(|| {
                                if mismatch {
                                    "Question response mode does not match its runtime capability."
                                } else {
                                    "This request has no supported response capability."
                                }
                                .to_owned()
                            })
                        });
                    Some(Request {
                        id: id.into(),
                        item: item.as_ref().clone(),
                        answerable,
                        dismissible: answerable && expected == "user_input_request" && message,
                        response_mode: mode.map(str::to_owned),
                        reason,
                    })
                })
                .collect();
        requests.sort_by(|a, b| {
            (a.item["ordinal"].as_u64(), &a.id).cmp(&(b.item["ordinal"].as_u64(), &b.id))
        });
        State {
            ready: self.actions_ready && !self.metadata.deleted,
            active_run: active.and_then(|r| r["id"].as_str()).map(str::to_owned),
            can_steer: session
                .is_some_and(|s| s["capabilities"]["turns"]["supportsActiveSteering"] == true),
            requests,
        }
    }
}
impl State {
    pub fn allows(&self, intent: &Intent) -> bool {
        if !self.ready || !intent.validate() {
            return false;
        }
        match intent {
            Intent::Message { mode, .. } => match mode {
                DispatchMode::StartImmediately => self.active_run.is_none(),
                DispatchMode::QueueAfterActive => true,
                DispatchMode::SteerActive { target_run_id } => {
                    self.can_steer && self.active_run.as_ref() == Some(target_run_id)
                }
            },
            Intent::Interrupt { run_id } => self.active_run.as_ref() == Some(run_id),
            Intent::Dismiss { request_id } => self
                .requests
                .iter()
                .any(|r| &r.id == request_id && r.dismissible),
            Intent::Respond {
                request_id,
                decision,
                answers,
            } => self.requests.iter().any(|r| {
                if &r.id != request_id || !r.answerable {
                    return false;
                }
                if r.item["type"] == "approval_request" {
                    let Some(d) = decision else {
                        return false;
                    };
                    if answers.is_some() || r.response_mode.as_deref() != Some("live") {
                        return false;
                    }
                    match r.item["options"].as_array() {
                        Some(options) => options.iter().any(|o| o["decision"] == *d),
                        None if r.item.get("options").is_none() => APPROVAL_FALLBACK_OPTIONS
                            .iter()
                            .any(|(decision, _)| *decision == d.as_str()),
                        None => false,
                    }
                } else {
                    r.item["type"] == "user_input_request"
                        && matches!(r.response_mode.as_deref(), Some("live" | "message"))
                        && decision.is_none()
                        && answers.as_ref().is_some_and(|answers| {
                            valid_answers_for_mode(
                                &r.item,
                                answers,
                                r.response_mode.as_deref() == Some("message"),
                            )
                        })
                }
            }),
        }
    }
}
pub fn valid_answers(item: &Value, answers: &BTreeMap<String, Value>) -> bool {
    valid_answers_for_mode(item, answers, item["responseMode"] == "message")
}

fn valid_answers_for_mode(item: &Value, answers: &BTreeMap<String, Value>, message: bool) -> bool {
    let Some(questions) = item["questions"].as_array().filter(|q| !q.is_empty()) else {
        return false;
    };
    let mut ids = std::collections::HashSet::new();
    if !questions.iter().all(|question| {
        question["id"]
            .as_str()
            .is_some_and(|id| !id.trim().is_empty() && ids.insert(id))
    }) {
        return false;
    }
    if answers
        .keys()
        .any(|id| !questions.iter().any(|q| q["id"] == *id))
    {
        return false;
    }
    questions.iter().all(|q| {
        let Some(id) = q["id"].as_str() else {
            return false;
        };
        let Some(answer) = answers.get(id) else {
            return q["required"] == false;
        };
        let option = |value: &str| {
            q["options"].as_array().is_some_and(|options| {
                options
                    .iter()
                    .any(|o| o["value"].as_str().or(o["label"].as_str()) == Some(value))
            })
        };
        let selections = |values: &[Value]| {
            let mut selected = std::collections::HashSet::new();
            !values.is_empty()
                && values.iter().all(|value| {
                    value
                        .as_str()
                        .is_some_and(|value| option(value) && selected.insert(value))
                })
        };
        match answer {
            Value::String(text) if message && q["multiSelect"] == true => {
                // Message requests forward plain text to the agent. Encode a
                // multi-choice selection as JSON-array TEXT, preserving exact
                // provider option values, rather than an array on the wire.
                if let Ok(Value::Array(values)) = serde_json::from_str::<Value>(text) {
                    selections(&values)
                } else {
                    q["allowCustomAnswer"] != false && !text.trim().is_empty()
                }
            }
            Value::String(text) => {
                !text.trim().is_empty() && (q["allowCustomAnswer"] != false || option(text))
            }
            Value::Array(values) if !message && q["multiSelect"] == true => selections(values),
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(options: Option<Value>) -> State {
        let mut item = json!({
            "type": "approval_request", "requestId": "bound-request",
            "threadId": "bound-thread", "runId": "bound-run",
        });
        if let Some(options) = options {
            item["options"] = options;
        }
        State {
            ready: true,
            requests: vec![Request {
                id: "bound-request".into(),
                item,
                answerable: true,
                dismissible: false,
                response_mode: Some("live".into()),
                reason: None,
            }],
            ..State::default()
        }
    }

    fn respond(decision: &str) -> Intent {
        Intent::Respond {
            request_id: "bound-request".into(),
            decision: Some(decision.into()),
            answers: None,
        }
    }

    #[test]
    fn approval_fallback_order_and_labels_match_t3() {
        assert_eq!(
            APPROVAL_FALLBACK_OPTIONS,
            [
                ("cancel", "Cancel"),
                ("decline", "Decline"),
                ("acceptForSession", "Always allow this session"),
                ("accept", "Approve"),
            ]
        );
    }

    #[test]
    fn absent_options_accept_each_fallback_with_exact_retained_command_wire() {
        let state = approval(None);
        for (decision, _) in APPROVAL_FALLBACK_OPTIONS {
            let command = Command::new("bound-thread", respond(decision));
            assert!(state.allows(&command.intent), "{decision}");
            let wire = command.wire().unwrap();
            assert_eq!(
                wire,
                json!({
                    "type": "runtime-request.respond",
                    "commandId": command.command_id,
                    "threadId": "bound-thread",
                    "requestId": "bound-request",
                    "decision": decision,
                }),
                "UI labels must never replace protocol decisions"
            );
            let retained: Command =
                serde_json::from_slice(&serde_json::to_vec(&command).unwrap()).unwrap();
            assert_eq!(retained.wire().unwrap(), wire);
            assert_eq!(retained.command_id, command.command_id);
            assert!(state.allows(&retained.intent));
        }
        assert!(!state.allows(&respond("acceptAlways")));
    }

    #[test]
    fn explicit_provider_options_replace_fallback_without_losing_provider_metadata() {
        let options = json!([
            {"decision": "acceptAlways", "label": "Always allow this app", "warning": "Persists approval"},
            {"decision": "decline", "label": "Deny this request"},
        ]);
        let state = approval(Some(options.clone()));
        for decision in ["acceptAlways", "decline"] {
            let intent = respond(decision);
            assert!(state.allows(&intent));
            assert_eq!(
                Command::new("bound-thread", intent).wire().unwrap()["decision"],
                decision
            );
        }
        for decision in ["cancel", "acceptForSession", "accept"] {
            assert!(
                !state.allows(&respond(decision)),
                "{decision} was not offered"
            );
        }
        assert_eq!(state.requests[0].item["options"], options);

        let decline_only = approval(Some(json!([
            {"decision": "decline", "label": "Decline"},
        ])));
        assert!(decline_only.allows(&respond("decline")));
        assert!(!decline_only.allows(&respond("acceptAlways")));
    }

    #[test]
    fn null_empty_and_malformed_options_never_restore_fallback_authority() {
        for options in [
            Value::Null,
            json!([]),
            json!(false),
            json!(1),
            json!("accept"),
            json!({"decision": "accept", "label": "Approve"}),
            json!([null, false, "accept", {}, {"decision": null}, {"decision": 1}]),
        ] {
            let state = approval(Some(options.clone()));
            for decision in [
                "cancel",
                "decline",
                "acceptForSession",
                "accept",
                "acceptAlways",
            ] {
                assert!(!state.allows(&respond(decision)), "{options}: {decision}");
            }
        }
        // Invalid entries cannot make an otherwise explicit list behave like
        // an absent list; only the decision actually offered remains usable.
        let state = approval(Some(
            json!([null, {"decision": "decline", "label": "Decline"}]),
        ));
        assert!(state.allows(&respond("decline")));
        assert!(!state.allows(&respond("accept")));
        assert!(!state.allows(&respond("acceptForSession")));
    }

    #[test]
    fn explicit_options_cannot_authorize_unknown_or_inexact_wire_decisions() {
        for decision in [
            "",
            "Approve",
            "acceptforsession",
            "acceptForSession ",
            "acceptEverything",
        ] {
            let state = approval(Some(
                json!([{"decision": decision, "label": "Provider choice"}]),
            ));
            let intent = respond(decision);
            assert!(!state.allows(&intent), "{decision}");
            assert!(Command::new("bound-thread", intent).wire().is_err());
        }
    }

    #[test]
    fn fallback_still_requires_live_ready_answerable_and_exact_request_identity() {
        for (decision, _) in APPROVAL_FALLBACK_OPTIONS {
            let intent = respond(decision);
            let positive = approval(None);
            assert!(positive.allows(&intent));
            let mut not_ready = positive.clone();
            not_ready.ready = false;
            assert!(!not_ready.allows(&intent));
            let mut not_answerable = positive.clone();
            not_answerable.requests[0].answerable = false;
            assert!(!not_answerable.allows(&intent));
            for mode in [None, Some("message"), Some("unknown")] {
                let mut wrong_mode = positive.clone();
                wrong_mode.requests[0].response_mode = mode.map(str::to_owned);
                assert!(!wrong_mode.allows(&intent));
            }
            assert!(!positive.allows(&Intent::Respond {
                request_id: "foreign-request".into(),
                decision: Some(decision.into()),
                answers: None,
            }));
            assert!(!positive.allows(&Intent::Respond {
                request_id: "bound-request".into(),
                decision: Some(decision.into()),
                answers: Some(BTreeMap::from([("q".into(), json!("answer"))])),
            }));
            assert!(!State {
                ready: true,
                ..State::default()
            }
            .allows(&intent));
        }
    }
}
