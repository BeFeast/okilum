//! Command availability derives only from the bound live projection. Inherited
//! history, dismissed/resolved requests and unknown capabilities never grant controls.
use super::*;
use crate::projection::Projection;

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
                        None if r.item.get("options").is_none() => {
                            matches!(d.as_str(), "accept" | "decline")
                        }
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
